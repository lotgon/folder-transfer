//! folder-transfer (`ft`): a wire-compatible Rust port of the PowerShell
//! ft-server / ft-client. See RUST-PORT-SPEC.md.

use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

// All the real logic lives in the library crate (also built as ft.dll / libft.so).
use ft::{client, config, firewall, server, tls, token, BoxError, DEFAULT_PORT};

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
    /// Adaptive-level coefficient: keep compression >= this x the link speed (default 1.6).
    #[arg(long)]
    compress_margin: Option<f64>,
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
    /// Log important decisions (adaptive compression level, etc.) to stderr and a
    /// debug log file, so you can watch what happens and share the log.
    #[arg(long)]
    debug: bool,
    /// Path for the debug log (default ft-debug.log in the current directory).
    #[arg(long)]
    debug_log: Option<String>,
    /// Keep the window open at the end (wait for Enter). Auto-enabled when launched
    /// into its own console (double-click); pass this to force it from a launcher.
    #[arg(long)]
    pause: bool,
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
    /// Log important decisions to stderr and a debug log file.
    #[arg(long)]
    debug: bool,
    /// Path for the debug log (default ft-debug.log in the current directory).
    #[arg(long)]
    debug_log: Option<String>,
    /// Keep the window open at the end (wait for Enter). Auto-enabled when launched
    /// into its own console (double-click); pass this to force it from a launcher.
    #[arg(long)]
    pause: bool,
}

fn main() {
    // rustls 0.23 wants a process-default crypto provider for some paths.
    let _ = rustls::crypto::ring::default_provider().install_default();

    // Allow `ft <folder-or-json> [dest] [flags]` with no subcommand, PowerShell-style:
    // the first positional decides serve vs get; `serve`/`get` stay available explicitly.
    let argv = rewrite_argv(std::env::args().collect());
    let cli = Cli::parse_from(argv);
    let force_pause = match &cli.cmd {
        Cmd::Serve(a) => a.pause,
        Cmd::Get(a) => a.pause,
    };
    let result = match cli.cmd {
        Cmd::Serve(a) => cmd_serve(a),
        Cmd::Get(a) => cmd_get(a),
    };
    let code = match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("[ft] error: {e}");
            1
        }
    };
    // Don't let the final summary vanish when ft was launched into its own console
    // (double-click / launcher) that closes the instant we exit.
    pause_at_exit(force_pause);
    std::process::exit(code);
}

/// True if this process is the ONLY one attached to its console — Windows created
/// the console for us and destroys it the instant we exit (double-click / launcher),
/// so the output would flash and vanish. False when run from an existing shell
/// (cmd/PowerShell are also attached) or when there is no console.
#[cfg(windows)]
fn owns_console() -> bool {
    extern "system" {
        fn GetConsoleProcessList(lpdwProcessList: *mut u32, dwProcessCount: u32) -> u32;
    }
    let mut buf = [0u32; 4];
    let n = unsafe { GetConsoleProcessList(buf.as_mut_ptr(), buf.len() as u32) };
    n == 1
}
#[cfg(not(windows))]
fn owns_console() -> bool {
    false
}

/// Keep the console open so the final summary stays readable: pause when forced
/// (`--pause`) or when we own the console (it would otherwise close). Never pauses
/// without an interactive stdin, so scripts and pipes are unaffected.
fn pause_at_exit(force: bool) {
    use std::io::{BufRead, Write};
    if !(force || owns_console()) || !std::io::stdin().is_terminal() {
        return;
    }
    eprint!("\n[ft] Press Enter to close . . . ");
    std::io::stderr().flush().ok();
    let mut s = String::new();
    let _ = std::io::stdin().lock().read_line(&mut s);
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
    if a.debug {
        ft::debug::init(a.debug_log.as_deref().unwrap_or("ft-debug.log"));
    }
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
    // An empty string in the config means "not set" (matches the PowerShell stance,
    // where `"clientOut": ""` / `"allowIp": ""` / `"serverHost": ""` fall back to the
    // default / no-restriction / auto-detect). Without this, Some("") would be taken
    // as a literal empty path (clientOut) or a literal "only IP '' may connect" (allowIp).
    let nonempty = |s: Option<String>| s.filter(|v| !v.trim().is_empty());
    let port = a.port.or(cfg.port).unwrap_or(DEFAULT_PORT);
    let allow_ip = nonempty(a.allow_ip.clone().or(cfg.allow_ip.clone()));
    let server_host = nonempty(a.server_host.clone().or(cfg.server_host.clone()));
    let idle_seconds = a.idle_seconds.or(cfg.idle_seconds).unwrap_or(600);
    let stall_timeout = a.stall_timeout.or(cfg.stall_timeout).unwrap_or(300);
    let client_out = nonempty(a.client_out.clone().or(cfg.client_out.clone()));
    let cutover = a.cutover || cfg.cutover.unwrap_or(false);
    let mut once = a.once || cfg.once.unwrap_or(false);
    let no_firewall = a.no_firewall || cfg.no_firewall.unwrap_or(false);
    let mut streams = a.streams.or(cfg.streams).unwrap_or(4);
    let use_compress = !a.no_compress && cfg.compress.unwrap_or(true);
    // Clamping lives in AdaptiveState::new (single source of truth, range [1.0, 16.0]).
    let compress_margin = a.compress_margin.or(cfg.compress_margin).unwrap_or(1.6);

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
    generate_client(&host, port, &tok, &identity.fingerprint, &folders, &config_path, &client_out)?;

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
        compress_margin,
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
    if a.debug {
        ft::debug::init(a.debug_log.as_deref().unwrap_or("ft-debug.log"));
    }
    let cfg = match &a.config {
        Some(p) => config::ClientConn::load(p)?,
        None => config::ClientConn::default(),
    };
    let nonempty = |s: Option<String>| s.filter(|v| !v.trim().is_empty());
    let server = nonempty(a.server.clone().or(cfg.server.clone()))
        .ok_or("--server is required (or via --config)")?;
    let port = a.port.or(cfg.port).unwrap_or(DEFAULT_PORT);
    let token = a.token.clone().or(cfg.token.clone()).unwrap_or_default();
    // If no destination was given, ask (Enter = current folder), like the PowerShell client.
    // Prompt BEFORE connecting so slow human input can't trip the server's stall timeout.
    let to = match a.to.clone() {
        Some(t) => t,
        None => prompt_destination()?,
    };
    let fingerprint = nonempty(a.fingerprint.clone().or(cfg.fingerprint.clone()))
        .ok_or("--fingerprint is required (or via --config)")?;
    // ignore + streams now come from the server after connecting; CLI/old-file values override.
    let ignore_override = nonempty(a.ignore.clone().or(cfg.ignore.clone()));
    let streams_override = a.streams.or(cfg.streams);
    client::run(&server, port, &token, &to, &fingerprint, ignore_override, streams_override)?;
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

/// Enable ANSI/VT processing on the Windows console so color codes render
/// (no-op elsewhere; modern terminals already support it).
#[cfg(windows)]
fn enable_vt() {
    use std::os::raw::c_void;
    const STD_ERROR_HANDLE: u32 = 0xFFFF_FFF4; // (DWORD)-12
    const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;
    extern "system" {
        fn GetStdHandle(n: u32) -> *mut c_void;
        fn GetConsoleMode(h: *mut c_void, mode: *mut u32) -> i32;
        fn SetConsoleMode(h: *mut c_void, mode: u32) -> i32;
    }
    unsafe {
        let h = GetStdHandle(STD_ERROR_HANDLE);
        if h.is_null() {
            return;
        }
        let mut mode: u32 = 0;
        if GetConsoleMode(h, &mut mode) != 0 {
            SetConsoleMode(h, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
        }
    }
}
#[cfg(not(windows))]
fn enable_vt() {}

/// Print the receiver command in a hard-to-miss, cream-coloured block (color only
/// when stderr is a real terminal; plain text when redirected to a file).
fn print_receiver_command(cmd: &str, file_hint: &str) {
    let tty = std::io::stderr().is_terminal();
    if tty {
        enable_vt();
    }
    // bold + cream (256-colour 230); dim for the secondary note.
    let (cream, dim, reset) = if tty {
        ("\x1b[1;38;5;230m", "\x1b[2m", "\x1b[0m")
    } else {
        ("", "", "")
    };
    let bar = "===============================================================";
    eprintln!();
    eprintln!("  {bar}");
    eprintln!("   >> COPY THIS and run it on the RECEIVING machine:");
    eprintln!("      (it will ask where to save - press Enter for the current folder)");
    eprintln!();
    eprintln!("      {cream}{cmd}{reset}");
    eprintln!();
    eprintln!("   {dim}(or append a destination folder, or use the saved file: {file_hint}){reset}");
    eprintln!("  {bar}");
    eprintln!();
}

/// Ask the user where to save (Enter = current folder), like the PowerShell client.
fn prompt_destination() -> Result<String, BoxError> {
    use std::io::{BufRead, Write};
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| ".".to_string());
    eprint!("Destination folder [{cwd}]: ");
    std::io::stderr().flush().ok();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    let picked = line.trim().trim_matches('"').trim();
    Ok(if picked.is_empty() { cwd } else { picked.to_string() })
}

/// Write the generated-client connection JSON and print a ready-to-run command.
#[allow(clippy::too_many_arguments)]
fn generate_client(
    host: &str,
    port: u16,
    token: &str,
    fingerprint: &str,
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
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create client-out folder {}: {e} (check the 'clientOut' path in your config)", parent.display()))?;
        }
    }
    let conn = config::ClientConn {
        server: Some(host.to_string()),
        port: Some(port),
        token: Some(token.to_string()),
        fingerprint: Some(fingerprint.to_string()),
        // ignore + streams are pushed by the server after connecting, not carried here.
        ignore: None,
        streams: None,
    };
    std::fs::write(&out_path, conn.to_json())
        .map_err(|e| format!("cannot write client file {}: {e}", out_path.display()))?;

    eprintln!("[ft] client connection file -> {}", out_path.display());
    let connname = out_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| out_path.display().to_string());
    let cmd = format!("ft get --server {host} --port {port} --token {token} --fingerprint {fingerprint}");
    print_receiver_command(&cmd, &format!("ft {connname} <DEST>"));
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
