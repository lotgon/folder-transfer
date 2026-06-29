//! folder-transfer (`ft`): a wire-compatible Rust port of the PowerShell
//! ft-server / ft-client. See RUST-PORT-SPEC.md.

mod client;
mod compress;
mod config;
mod firewall;
mod ignore;
mod mtime;
mod paths;
mod server;
mod tls;
mod token;
mod wire;

use std::fs;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

/// Shared error type for fallible operations.
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Default TCP port (matches `ft-server.ps1`).
const DEFAULT_PORT: u16 = 8722;

#[derive(Parser)]
#[command(name = "ft", version, about = "folder-transfer: TLS folder sync (Rust port)")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Serve folder(s) read-only over TLS (== ft-server.ps1).
    Serve(ServeArgs),
    /// Download from a server into a destination (== ft-client.ps1).
    #[command(visible_alias = "download", visible_alias = "fetch")]
    Get(GetArgs),
}

#[derive(Parser)]
struct ServeArgs {
    /// Folder to share (or a .json config file).
    folder: Option<String>,
    /// JSONC config file.
    #[arg(long)]
    config: Option<String>,
    /// Ignore patterns (`;`/`,`-separated).
    #[arg(long)]
    ignore: Option<String>,
    /// Disable adaptive compression.
    #[arg(long)]
    no_compress: bool,
    /// Parallel handler streams (1 = classic SYNC; >1 = parallel QSYNC). Default 4.
    #[arg(long)]
    streams: Option<i32>,
    /// TCP port to listen on (default 8722).
    #[arg(long)]
    port: Option<u16>,
    /// Restrict to a single source IP.
    #[arg(long)]
    allow_ip: Option<String>,
    /// Auto-shutdown after this many idle seconds with no client (default 600).
    #[arg(long)]
    idle_seconds: Option<u64>,
    /// Abort a connected-but-silent client after this many seconds (default 300).
    #[arg(long)]
    stall_timeout: Option<u64>,
    /// Exit after one clean session.
    #[arg(long)]
    once: bool,
    /// Address baked into the generated client (default: auto-detected IPv4).
    #[arg(long)]
    server_host: Option<String>,
    /// Where to write the generated client connection file.
    #[arg(long)]
    client_out: Option<String>,
    /// Do not touch the Windows firewall (no-op on Linux).
    #[arg(long)]
    no_firewall: bool,
    /// Two-phase cutover (implies --once, forces --streams 1).
    #[arg(long)]
    cutover: bool,
}

#[derive(Parser)]
struct GetArgs {
    /// Source server address.
    #[arg(long)]
    server: Option<String>,
    /// TCP port (default 8722).
    #[arg(long)]
    port: Option<u16>,
    /// Auth token (may be empty).
    #[arg(long)]
    token: Option<String>,
    /// Destination folder (ToFolder).
    #[arg(long)]
    to: Option<String>,
    /// Pinned server certificate SHA-256 fingerprint (hex).
    #[arg(long)]
    fingerprint: Option<String>,
    /// Ignore patterns (`;`/`,`-separated).
    #[arg(long)]
    ignore: Option<String>,
    /// Parallel connection streams (default 1).
    #[arg(long)]
    streams: Option<i32>,
    /// JSONC connection/config file (as written by the server).
    #[arg(long)]
    config: Option<String>,
}

fn main() {
    // rustls 0.23 wants a process-default crypto provider for some paths.
    let _ = rustls::crypto::ring::default_provider().install_default();

    // Allow `ft <folder-or-json> [dest] [flags]` with no subcommand, PowerShell-style:
    // the first positional decides serve vs get; `serve`/`get` stay available explicitly.
    let argv = rewrite_argv(std::env::args().collect());
    let cli = Cli::parse_from(argv);
    let result = match cli.cmd {
        Cmd::Serve(a) => cmd_serve(a),
        Cmd::Get(a) => cmd_get(a),
    };
    if let Err(e) = result {
        eprintln!("[ft] error: {e}");
        std::process::exit(1);
    }
}

/// Rewrite argv so a bare first positional works without a subcommand:
/// - `ft <dir>` or `ft <server-config.json>`  -> `ft serve <arg> [flags]`
/// - `ft <connection.json> [dest] [flags]`     -> `ft get --config <arg> [--to dest] [flags]`
/// Explicit `serve`/`get`/`download`/`fetch`/`help` and `-h`/`-V`/no-args pass through unchanged.
fn rewrite_argv(argv: Vec<String>) -> Vec<String> {
    let first = argv.get(1).map(|s| s.as_str());
    let is_subcommand = matches!(first, Some("serve" | "get" | "download" | "fetch" | "help"));
    let is_help_or_version =
        first.is_none() || matches!(first, Some("-h" | "--help" | "-V" | "--version"));
    // A leading flag (e.g. `ft --port 9000 sync.json`) is uncommon; fall back to clap.
    let leading_flag = first.map(|s| s.starts_with('-')).unwrap_or(false);
    if is_subcommand || is_help_or_version || leading_flag {
        return argv;
    }

    let pos1 = argv[1].clone();
    let mut out = vec![argv[0].clone()];
    if detect_is_client(&pos1) {
        out.push("get".into());
        out.push("--config".into());
        out.push(pos1);
        // an optional second positional is the destination folder
        let mut rest = 2;
        if let Some(a2) = argv.get(2) {
            if !a2.starts_with('-') {
                out.push("--to".into());
                out.push(a2.clone());
                rest = 3;
            }
        }
        out.extend(argv[rest..].iter().cloned());
    } else {
        out.push("serve".into());
        out.push(pos1); // serve's positional auto-detects a .json as its config
        out.extend(argv[2..].iter().cloned());
    }
    out
}

/// Is this argument a client connection file (has fingerprint/token/server and no
/// folders)? A directory or a server config (folders/folder) is NOT a client file.
fn detect_is_client(arg: &str) -> bool {
    if Path::new(arg).is_dir() {
        return false;
    }
    let raw = match fs::read_to_string(arg) {
        Ok(s) => s,
        Err(_) => return false, // not a readable file -> treat as a (maybe missing) folder/config
    };
    let stripped = config::strip_jsonc(&raw);
    let v: serde_json::Value = match serde_json::from_str(&stripped) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let has_server = v.get("folders").is_some() || v.get("folder").is_some();
    if has_server {
        return false;
    }
    v.get("fingerprint").is_some() || v.get("token").is_some() || v.get("server").is_some()
}

fn cmd_serve(a: ServeArgs) -> Result<(), BoxError> {
    // A positional .json (or any existing file) is taken as the config.
    let mut config_path = a.config.clone();
    let mut folder_pos = a.folder.clone();
    if config_path.is_none() {
        if let Some(f) = &folder_pos {
            if f.ends_with(".json") || Path::new(f).is_file() {
                config_path = Some(f.clone());
                folder_pos = None;
            }
        }
    }
    let cfg = match &config_path {
        Some(p) => config::ServeFileConfig::load(p)?,
        None => config::ServeFileConfig::default(),
    };

    // Assemble folders (config folders[] + folder + positional).
    let mut folders: Vec<String> = cfg.folders.clone();
    if let Some(f) = &cfg.folder {
        folders.push(f.clone());
    }
    if let Some(f) = &folder_pos {
        folders.push(f.clone());
    }

    // Ignore: config ignore[] + CLI list.
    let mut ignore_parts: Vec<String> = cfg.ignore.clone();
    if let Some(ig) = &a.ignore {
        ignore_parts.extend(ig.split([';', ',']).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()));
    }
    let ignore_parts: Vec<String> = ignore_parts.into_iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    let ignore_spec = ignore_parts.join(";");

    // Merge scalars: CLI wins over config, then a built-in default.
    let port = a.port.or(cfg.port).unwrap_or(DEFAULT_PORT);
    let allow_ip = a.allow_ip.clone().or(cfg.allow_ip.clone());
    let server_host = a.server_host.clone().or(cfg.server_host.clone());
    let idle_seconds = a.idle_seconds.or(cfg.idle_seconds).unwrap_or(600);
    let stall_timeout = a.stall_timeout.or(cfg.stall_timeout).unwrap_or(300);
    let client_out = a.client_out.clone().or(cfg.client_out.clone());
    let cutover = a.cutover || cfg.cutover.unwrap_or(false);
    let mut once = a.once || cfg.once.unwrap_or(false);
    let no_firewall = a.no_firewall || cfg.no_firewall.unwrap_or(false);
    let mut streams = a.streams.or(cfg.streams).unwrap_or(4);
    let use_compress = !a.no_compress && cfg.compress.unwrap_or(true);

    if cutover {
        once = true;
        streams = 1;
    }
    if streams < 1 {
        streams = 1;
    }
    if folders.is_empty() {
        return Err("a folder to share (or a config with folders) is required".into());
    }

    let identity = tls::make_server_identity()?;
    let tok = token::generate();
    println!("FINGERPRINT={}", identity.fingerprint);

    // Generated client: write the connection JSON and print a ready-to-run command.
    let host = server_host.unwrap_or_else(detect_server_host);
    generate_client(&host, port, &tok, &identity.fingerprint, &ignore_spec, streams, &folders, &config_path, &client_out)?;

    // Open the firewall (Windows best-effort; no-op elsewhere). Held until return.
    let _fw = if no_firewall {
        None
    } else {
        Some(firewall::open(port, allow_ip.as_deref()))
    };

    let serve_cfg = server::ServeConfig {
        folders,
        port,
        idle_seconds,
        stall_timeout,
        once,
        cutover,
        use_compress,
        ignore_spec,
        allow_ip: allow_ip.clone(),
    };
    if streams > 1 {
        server::run_serve_parallel(serve_cfg, &identity, &tok, streams)
    } else {
        server::run_serve_single(serve_cfg, &identity, &tok)
    }
}

fn cmd_get(a: GetArgs) -> Result<(), BoxError> {
    let cfg = match &a.config {
        Some(p) => config::ClientConn::load(p)?,
        None => config::ClientConn::default(),
    };
    let server = a.server.clone().or(cfg.server.clone()).ok_or("--server is required (or via --config)")?;
    let port = a.port.or(cfg.port).unwrap_or(DEFAULT_PORT);
    let token = a.token.clone().or(cfg.token.clone()).unwrap_or_default();
    let to = a.to.clone().ok_or("--to <path> is required")?;
    let fingerprint = a
        .fingerprint
        .clone()
        .or(cfg.fingerprint.clone())
        .ok_or("--fingerprint is required (or via --config)")?;
    let ignore_spec = a.ignore.clone().or(cfg.ignore.clone()).unwrap_or_default();
    let mut streams = a.streams.or(cfg.streams).unwrap_or(1);
    if streams < 1 {
        streams = 1;
    }

    if streams > 1 {
        client::run_parallel(&server, port, &token, &to, &fingerprint, &ignore_spec, streams)?;
    } else {
        client::run_single(&server, port, &token, &to, &fingerprint, &ignore_spec)?;
    }
    Ok(())
}

/// Best-effort local IPv4 detection (no packets sent; just reads the chosen
/// source address for a route to a public IP).
fn detect_server_host() -> String {
    use std::net::UdpSocket;
    let detect = || -> Option<String> {
        let s = UdpSocket::bind("0.0.0.0:0").ok()?;
        s.connect("8.8.8.8:80").ok()?;
        Some(s.local_addr().ok()?.ip().to_string())
    };
    detect().unwrap_or_else(|| "THIS-SERVER-IP".to_string())
}

/// Write the generated-client connection JSON and print a ready-to-run command.
#[allow(clippy::too_many_arguments)]
fn generate_client(
    host: &str,
    port: u16,
    token: &str,
    fingerprint: &str,
    ignore_spec: &str,
    streams: i32,
    folders: &[String],
    config_path: &Option<String>,
    client_out: &Option<String>,
) -> Result<(), BoxError> {
    let name = derive_client_name(folders, config_path);
    let out_path = match client_out {
        Some(p) => PathBuf::from(p),
        None => default_client_out(&name),
    };
    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let conn = config::ClientConn {
        server: Some(host.to_string()),
        port: Some(port),
        token: Some(token.to_string()),
        fingerprint: Some(fingerprint.to_string()),
        ignore: if ignore_spec.is_empty() { None } else { Some(ignore_spec.to_string()) },
        streams: Some(streams),
    };
    std::fs::write(&out_path, conn.to_json())?;

    eprintln!("[ft] client connection file -> {}", out_path.display());
    let mut cmd = format!(
        "ft get --server {host} --port {port} --token {token} --to <DEST> --fingerprint {fingerprint}"
    );
    if !ignore_spec.is_empty() {
        cmd.push_str(&format!(" --ignore \"{ignore_spec}\""));
    }
    if streams != 1 {
        cmd.push_str(&format!(" --streams {streams}"));
    }
    eprintln!("[ft] run on the receiver:\n    {cmd}");
    eprintln!("[ft] or: ft get --config {} --to <DEST>", out_path.display());
    Ok(())
}

/// Derive a short name for the generated client file.
fn derive_client_name(folders: &[String], config_path: &Option<String>) -> String {
    if folders.len() == 1 {
        if let Some(leaf) = Path::new(folders[0].trim().trim_matches('"')).file_name() {
            return leaf.to_string_lossy().into_owned();
        }
    } else if let Some(c) = config_path {
        if let Some(stem) = Path::new(c).file_stem() {
            return stem.to_string_lossy().into_owned();
        }
    }
    if folders.len() > 1 {
        "multi".to_string()
    } else {
        "Share".to_string()
    }
}

/// Default connection-file path: `download-scripts/ft-download-<name>.json` next
/// to the binary.
fn default_client_out(name: &str) -> PathBuf {
    let base = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("download-scripts").join(format!("ft-download-{name}.json"))
}
