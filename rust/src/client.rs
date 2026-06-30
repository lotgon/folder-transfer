//! The download client (`ft get`). Single-stream `SYNC` (M2) and parallel
//! `QSYNC` (M4). The item decoders are shared; the parallel client runs N worker
//! threads over a shared `seen`/mirror-roots set and does ONE mirror pass at the
//! end (clean finish only).

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// If no data arrives for this long, treat the connection as lost (the server
/// sends keepalives while idle, so this only fires on a real drop/stall).
const CLIENT_READ_TIMEOUT_SECS: u64 = 90;

use rustls::{ClientConfig, ClientConnection, StreamOwned};

use crate::compress::zstd_decompress;
use crate::mtime;
use crate::paths::{norm_key, safe_join, top_segment};
use crate::progress::Progress;
use crate::wire::Conn;
use crate::{ignore::IgnoreSet, tls, BoxError};

/// Running totals for the final summary line.
#[derive(Default)]
pub struct Stats {
    pub got: u64,
    pub skipped: u64,
    pub bytes: u64,
    pub deleted: u64,
}

fn eof() -> io::Error {
    io::Error::new(io::ErrorKind::UnexpectedEof, "connection closed mid-transfer")
}

/// Does this local path differ from the offered (size, mtime-ticks)?
pub fn need_fetch(path: &Path, size: u64, mt: i64) -> bool {
    match fs::metadata(path) {
        Ok(m) if m.is_file() => {
            if m.len() != size {
                return true;
            }
            match mtime::read_ticks(&m) {
                Ok(t) => t != mt,
                Err(_) => true,
            }
        }
        _ => true,
    }
}

/// Parse `<size> <mtime> <rel>` (rel may contain spaces). Mirrors `^(\d+) (\d+) (.+)$`.
fn parse_three(s: &str) -> Option<(u64, i64, String)> {
    let first = s.find(' ')?;
    let size: u64 = s[..first].parse().ok()?;
    let rest = &s[first + 1..];
    let second = rest.find(' ')?;
    let mt: i64 = rest[..second].parse().ok()?;
    let rel = &rest[second + 1..];
    if rel.is_empty() {
        return None;
    }
    Some((size, mt, rel.to_string()))
}

/// Shared `seen` set + mirror roots. Interior mutability so the single-stream
/// path and the parallel workers use the same handlers; in parallel each worker
/// holds a clone (the inner `Arc`s are shared).
#[derive(Clone)]
struct Mirror {
    seen: Arc<Mutex<HashSet<String>>>,
    roots: Arc<Mutex<HashSet<PathBuf>>>,
}

impl Mirror {
    fn new() -> Self {
        Mirror {
            seen: Arc::new(Mutex::new(HashSet::new())),
            roots: Arc::new(Mutex::new(HashSet::new())),
        }
    }
    fn insert_seen(&self, key: String) {
        self.seen.lock().unwrap().insert(key);
    }
    fn clear_seen(&self) {
        self.seen.lock().unwrap().clear();
    }
    fn contains_seen(&self, key: &str) -> bool {
        self.seen.lock().unwrap().contains(key)
    }
    fn insert_root(&self, p: PathBuf) {
        self.roots.lock().unwrap().insert(p);
    }
    fn roots_snapshot(&self) -> Vec<PathBuf> {
        self.roots.lock().unwrap().iter().cloned().collect()
    }
}

/// Record `<dest>\<top>` as a mirror root for the rel's first segment.
fn record_root(to: &Path, prefix: &str, rel: &str, mir: &Mirror) {
    if let Some(top) = top_segment(rel) {
        if let Some(r) = safe_join(to, prefix, top) {
            mir.insert_root(r);
        }
    }
}

/// Dispatch one item-stream line (`D`/`B`/`F`); unknown lines are ignored.
#[allow(clippy::too_many_arguments)]
fn dispatch_item<S: Read + Write>(
    conn: &mut Conn<S>,
    to: &Path,
    prefix: &str,
    mir: &Mirror,
    stats: &mut Stats,
    prog: &Progress,
    h: &str,
) -> io::Result<()> {
    if let Some(rest) = h.strip_prefix("D ") {
        handle_dir(to, prefix, mir, rest)?;
    } else if let Some(rest) = h.strip_prefix("B ") {
        let count: usize = rest.trim().parse().unwrap_or(0);
        handle_bundle(conn, to, prefix, mir, stats, prog, count)?;
    } else if let Some(rest) = h.strip_prefix("F ") {
        if let Some((size, mt, rel)) = parse_three(rest) {
            handle_large(conn, to, prefix, mir, stats, prog, size, mt, &rel)?;
        }
    }
    Ok(())
}

/// Handle `D <rel>`: recreate an empty directory and record its mirror root.
fn handle_dir(to: &Path, prefix: &str, mir: &Mirror, drel: &str) -> io::Result<()> {
    if let Some(dt) = safe_join(to, prefix, drel) {
        fs::create_dir_all(&dt)?;
        record_root(to, prefix, drel, mir);
    }
    Ok(())
}

/// Handle `B <count>`: read manifest, reply with a want-mask, receive wanted files.
#[allow(clippy::too_many_arguments)]
fn handle_bundle<S: Read + Write>(
    conn: &mut Conn<S>,
    to: &Path,
    prefix: &str,
    mir: &Mirror,
    stats: &mut Stats,
    prog: &Progress,
    count: usize,
) -> io::Result<()> {
    let mut items: Vec<Option<(u64, i64, String)>> = Vec::with_capacity(count);
    for _ in 0..count {
        let ml = conn.read_line()?.ok_or_else(eof)?;
        items.push(parse_three(&ml));
    }

    let mut mask = String::with_capacity(count);
    let mut targets: Vec<Option<PathBuf>> = Vec::with_capacity(count);
    for it in &items {
        match it {
            None => {
                mask.push('0');
                targets.push(None);
            }
            Some((size, mt, rel)) => match safe_join(to, prefix, rel) {
                None => {
                    mask.push('0');
                    targets.push(None);
                }
                Some(bt) => {
                    record_root(to, prefix, rel, mir);
                    mir.insert_seen(norm_key(&bt));
                    if need_fetch(&bt, *size, *mt) {
                        mask.push('1');
                        targets.push(Some(bt));
                    } else {
                        mask.push('0');
                        stats.skipped += 1;
                        targets.push(None);
                    }
                }
            },
        }
    }
    conn.send_line(&mask)?;

    for (k, target) in targets.iter().enumerate() {
        let Some(bt) = target else { continue };
        // Per file: "Z <clen> <rlen>" (zstd) / "R <rlen>" (raw) / "-1" (locked).
        let hdr = conn.read_line()?.ok_or_else(eof)?;
        if hdr == "-1" {
            continue; // locked on the source -> keep our copy
        }
        if let Some(parent) = bt.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut f = File::create(bt)?;
        let mut parts = hdr.split(' ');
        let tag = parts.next().unwrap_or("");
        let nbytes: u64 = if tag == "Z" {
            let clen: usize = parts.next().unwrap_or("0").parse().unwrap_or(0);
            let rlen: usize = parts.next().unwrap_or("0").parse().unwrap_or(0);
            let cbuf = conn.read_exact_vec(clen)?;
            let obuf = zstd_decompress(&cbuf, rlen)?;
            f.write_all(&obuf)?;
            obuf.len() as u64
        } else {
            // "R <rlen>" (raw)
            let rlen: u64 = parts.next().unwrap_or("0").parse().unwrap_or(0);
            conn.copy_exact_to_writer(rlen, &mut f)?;
            rlen
        };
        drop(f);
        let mt = items[k].as_ref().unwrap().1;
        let _ = mtime::set_ticks(bt, mt);
        stats.got += 1;
        stats.bytes += nbytes;
        prog.add(nbytes, 1);
    }
    Ok(())
}

/// Handle `F <size> <mtime> <rel>`: a large file, raw or adaptive-compressed.
#[allow(clippy::too_many_arguments)]
fn handle_large<S: Read + Write>(
    conn: &mut Conn<S>,
    to: &Path,
    prefix: &str,
    mir: &Mirror,
    stats: &mut Stats,
    prog: &Progress,
    size: u64,
    mt: i64,
    rel: &str,
) -> io::Result<()> {
    let target = match safe_join(to, prefix, rel) {
        Some(t) => t,
        None => {
            eprintln!("[ft]   skip unsafe path from server: {rel}");
            conn.send_line("-1")?;
            return Ok(());
        }
    };
    record_root(to, prefix, rel, mir);
    mir.insert_seen(norm_key(&target));

    if !need_fetch(&target, size, mt) {
        conn.send_line("-1")?;
        stats.skipped += 1;
        return Ok(());
    }
    conn.send_line("0")?; // full fetch (overwrite); resume offsets unused by the client
    let hdr = conn.read_line()?.ok_or_else(eof)?;
    if hdr == "-1" {
        return Ok(()); // locked on the source
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut f = File::create(&target)?;
    if hdr == "Z" {
        // adaptive: "Z <clen> <rlen>" (zstd) or "R <rlen>" (raw), ended by "E"
        loop {
            let ch = conn.read_line()?.ok_or_else(eof)?;
            if ch == "E" {
                break;
            }
            let mut parts = ch.split(' ');
            let tag = parts.next().unwrap_or("");
            if tag == "R" {
                let rlen: u64 = parts.next().unwrap_or("0").parse().unwrap_or(0);
                conn.copy_exact_to_writer(rlen, &mut f)?;
                stats.bytes += rlen;
                prog.add(rlen, 0);
            } else {
                let clen: usize = parts.next().unwrap_or("0").parse().unwrap_or(0);
                let rlen: usize = parts.next().unwrap_or("0").parse().unwrap_or(0);
                let cbuf = conn.read_exact_vec(clen)?;
                let obuf = zstd_decompress(&cbuf, rlen)?;
                f.write_all(&obuf)?;
                stats.bytes += obuf.len() as u64;
                prog.add(obuf.len() as u64, 0);
            }
        }
    } else {
        // raw: "R <remain>" - stream in 1 MiB chunks with mid-file progress
        let remain: u64 = hdr.split(' ').nth(1).unwrap_or("0").parse().unwrap_or(0);
        let mut left = remain;
        let mut buf = vec![0u8; 1 << 20];
        while left > 0 {
            let want = std::cmp::min(left, buf.len() as u64) as usize;
            conn.read_exact(&mut buf[..want])?;
            f.write_all(&buf[..want])?;
            left -= want as u64;
            stats.bytes += want as u64;
            prog.add(want as u64, 0);
        }
    }
    drop(f);
    let _ = mtime::set_ticks(&target, mt);
    stats.got += 1;
    prog.add(0, 1);
    Ok(())
}

/// Delete local files no longer offered (clean finish only; ignored content kept).
fn mirror_delete(prefix: &str, ignore: &IgnoreSet, mir: &Mirror, stats: &mut Stats) {
    for root in mir.roots_snapshot() {
        if !root.is_dir() {
            continue;
        }
        delete_unseen(&root, prefix, ignore, mir, stats);
    }
}

fn delete_unseen(dir: &Path, prefix: &str, ignore: &IgnoreSet, mir: &Mirror, stats: &mut Stats) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let ft = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if ft.is_dir() {
            delete_unseen(&path, prefix, ignore, mir, stats);
        } else if ft.is_file() {
            let full = path.to_string_lossy();
            let rel2 = full.get(prefix.len()..).unwrap_or("");
            if ignore.is_ignored(rel2, false) {
                continue; // never delete ignored content
            }
            if !mir.contains_seen(&norm_key(&path)) {
                let _ = fs::remove_file(&path);
                stats.deleted += 1;
            }
        }
    }
}

/// Config the server pushes right after AUTH, so the client need not carry it.
struct ServerCfg {
    streams: i32,
    ignore: String,
}

/// Parse a `CFG <streams> <ignore-spec>` line (the ignore spec may contain spaces).
fn parse_cfg(line: &str) -> ServerCfg {
    let mut it = line.splitn(3, ' ');
    if it.next() != Some("CFG") {
        return ServerCfg { streams: 1, ignore: String::new() };
    }
    let streams = it.next().unwrap_or("1").parse().unwrap_or(1);
    let ignore = it.next().unwrap_or("").to_string();
    ServerCfg { streams, ignore }
}

type ClientStream = Conn<StreamOwned<ClientConnection, TcpStream>>;

/// TLS-connect + AUTH, then read the server's pushed CFG line.
fn connect_auth(
    config: Arc<ClientConfig>,
    server: &str,
    port: u16,
    token: &str,
) -> Result<(ClientStream, ServerCfg), BoxError> {
    let tcp = TcpStream::connect((server, port))?;
    tcp.set_nodelay(true)?;
    // Read timeout so a dropped/stalled link fails with an error instead of hanging forever.
    let _ = tcp.set_read_timeout(Some(Duration::from_secs(CLIENT_READ_TIMEOUT_SECS)));
    let conn = ClientConnection::new(config, tls::sni_server_name())?;
    let mut c = Conn::new(StreamOwned::new(conn, tcp));
    c.send_line(&format!("AUTH {token}"))?;
    match c.read_line()?.as_deref() {
        Some("OK") => {}
        Some("ERR auth") => return Err("auth failed: bad token".into()),
        other => return Err(format!("unexpected auth reply: {other:?}").into()),
    }
    let scfg = match c.read_line()? {
        Some(line) => parse_cfg(&line),
        None => return Err("connection closed before server config".into()),
    };
    Ok((c, scfg))
}

/// Single-stream `SYNC`: run the pass(es), then mirror-delete on a clean finish.
#[allow(clippy::too_many_arguments)]
fn run_passes<S: Read + Write>(
    conn: &mut Conn<S>,
    to: &Path,
    prefix: &str,
    ignore: &IgnoreSet,
    mir: &Mirror,
    stats: &mut Stats,
    prog: &Progress,
) -> io::Result<bool> {
    conn.send_line("SYNC")?;
    let mut more = true;
    let mut sync_ok = false;
    while more {
        mir.clear_seen(); // mirror reflects ONLY the latest pass
        loop {
            let h = match conn.read_line()? {
                Some(l) => l,
                None => {
                    more = false;
                    break;
                }
            };
            if h == "PASS-END" {
                break;
            }
            if h == "PING" || h.strip_prefix("T ").is_some() {
                continue; // keepalive / file-count line
            }
            dispatch_item(conn, to, prefix, mir, stats, prog, &h)?;
        }
        if !more {
            break;
        }
        // Next control line, skipping PING keepalives.
        let mut next = String::new();
        loop {
            match conn.read_line()? {
                None => {
                    more = false;
                    break;
                }
                Some(d) if d == "PING" => continue,
                Some(d) => {
                    next = d;
                    break;
                }
            }
        }
        if !more {
            break;
        }
        if next == "GO" {
            eprintln!("[ft] server signalled phase 2 (final sync)");
            continue;
        }
        if next == "DONE" {
            sync_ok = true;
        }
        break;
    }

    if sync_ok {
        mirror_delete(prefix, ignore, mir, stats);
    } else {
        eprintln!("[ft] sync did not finish cleanly - nothing deleted");
    }
    conn.send_line("BYE")?;
    Ok(sync_ok)
}

/// Prepare the destination: create it, canonicalise, and build the root prefix.
fn prepare_dest(to: &str) -> Result<(PathBuf, String), BoxError> {
    let to_path = Path::new(to);
    if !to_path.exists() {
        fs::create_dir_all(to_path)?;
    }
    let to_canon = crate::paths::canonicalize(to_path)?;
    let prefix = crate::paths::root_prefix(&to_canon);
    Ok((to_canon, prefix))
}

/// Entry point for `ft get`. The server tells us how many streams to use and the
/// ignore patterns after connecting; CLI overrides win if provided.
pub fn run(
    server: &str,
    port: u16,
    token: &str,
    to: &str,
    fingerprint: &str,
    ignore_override: Option<String>,
    streams_override: Option<i32>,
) -> Result<Stats, BoxError> {
    let (to_canon, prefix) = prepare_dest(to)?;
    let config = tls::make_client_config(fingerprint)?;
    let (mut probe, scfg) = connect_auth(config.clone(), server, port, token)?;
    let streams = streams_override.unwrap_or(scfg.streams).max(1);
    let ignore_spec = ignore_override.unwrap_or(scfg.ignore);
    let ignore = IgnoreSet::parse(&ignore_spec);

    if streams <= 1 {
        // reuse the probe connection for the single-stream pass
        eprintln!("[ft] sync -> {}", to_canon.display());
        let start = Instant::now();
        let mir = Mirror::new();
        let mut stats = Stats::default();
        let prog = Progress::new("[ft]");
        if let Err(e) = run_passes(&mut probe, &to_canon, &prefix, &ignore, &mir, &mut stats, &prog) {
            let elapsed = crate::progress::fmt_elapsed(start.elapsed().as_secs_f64());
            eprintln!("[ft] !! connection to the server was lost before the sync finished ({e}).");
            println!(
                "[ft] sync INCOMPLETE. fetched={} unchanged={} bytes={} in {elapsed} -- nothing deleted; re-run to resume.",
                stats.got, stats.skipped, stats.bytes
            );
            return Err("sync incomplete (connection lost)".into());
        }
        let secs = start.elapsed().as_secs_f64();
        let mbps = if secs > 0.0 { (stats.bytes as f64 / 1_048_576.0) / secs } else { 0.0 };
        println!(
            "[ft] sync DONE. fetched={} unchanged={} deleted={} bytes={} in {} @ {mbps:.1} MB/s",
            stats.got, stats.skipped, stats.deleted, stats.bytes,
            crate::progress::fmt_elapsed(secs)
        );
        Ok(stats)
    } else {
        drop(probe); // the probe learned the stream count; workers open fresh connections
        run_parallel_impl(config, server, port, token, &to_canon, &prefix, ignore, streams)
    }
}

/// Per-worker outcome for the parallel run.
struct WorkerStat {
    ok: bool,
    stats: Stats,
}

/// One parallel worker: connect, `QSYNC`, pull units until `NOUNIT`.
#[allow(clippy::too_many_arguments)]
fn worker(
    config: Arc<ClientConfig>,
    server: &str,
    port: u16,
    token: &str,
    to: &Path,
    prefix: &str,
    mir: &Mirror,
    prog: &Progress,
) -> Option<WorkerStat> {
    // A connect/auth failure is benign: the server likely already drained the queue
    // before this stream connected (common for small/fast jobs). No stat -> the
    // aggregator only errors if NO stream connected at all.
    let (mut conn, _scfg) = match connect_auth(config, server, port, token) {
        Ok(x) => x,
        Err(_) => return None,
    };

    let mut stats = Stats::default();
    let mut units: u64 = 0;
    let mut ok = false;
    let result: io::Result<()> = (|| {
        conn.send_line("QSYNC")?;
        loop {
            let h = conn.read_line()?.ok_or_else(eof)?;
            if h == "NOUNIT" {
                ok = true;
                break;
            }
            if h == "PING" {
                continue; // server keepalive while it waits for the producer
            }
            units += 1;
            dispatch_item(&mut conn, to, prefix, mir, &mut stats, prog, &h)?;
        }
        conn.send_line("BYE")?;
        Ok(())
    })();

    // Only a stream that dropped AFTER receiving units makes the run unclean.
    // A failure with zero units lost the connect race (or never handshook because
    // the server already finished) -> benign, exactly like the PowerShell client.
    // (Consequence inherited from PS: if EVERY stream fails auth with zero units,
    // the run looks "clean" with an empty seen set. The token is baked in, so this
    // only happens on misconfiguration.)
    if result.is_err() && units == 0 {
        ok = true;
    }
    Some(WorkerStat { ok, stats })
}

/// Parallel `QSYNC` run with the stream count and ignore set already resolved.
#[allow(clippy::too_many_arguments)]
fn run_parallel_impl(
    config: Arc<ClientConfig>,
    server: &str,
    port: u16,
    token: &str,
    to_canon: &Path,
    prefix: &str,
    ignore: IgnoreSet,
    streams: i32,
) -> Result<Stats, BoxError> {
    let mir = Mirror::new();
    eprintln!("[ft] sync -> {} ({streams} parallel streams)", to_canon.display());
    let start = Instant::now();
    // One shared progress across all workers -> a single aggregate live line.
    let prog = Progress::new("[ft]");

    let mut results: Vec<Option<WorkerStat>> = Vec::new();
    std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for _ in 0..streams {
            let cfg = config.clone();
            let mir = mir.clone();
            let prog = prog.clone();
            handles.push(scope.spawn(move || worker(cfg, server, port, token, to_canon, prefix, &mir, &prog)));
        }
        for h in handles {
            results.push(h.join().unwrap_or(None));
        }
    });

    let stats_list: Vec<WorkerStat> = results.into_iter().flatten().collect();
    if stats_list.is_empty() {
        return Err(format!("could not connect to server {server}:{port} (no stream reached it)").into());
    }

    let mut total = Stats::default();
    let mut clean = true;
    for w in &stats_list {
        total.got += w.stats.got;
        total.skipped += w.stats.skipped;
        total.bytes += w.stats.bytes;
        if !w.ok {
            clean = false;
        }
    }

    if clean {
        mirror_delete(prefix, &ignore, &mir, &mut total);
    }

    let secs = start.elapsed().as_secs_f64();
    let mbps = if secs > 0.0 { (total.bytes as f64 / 1_048_576.0) / secs } else { 0.0 };
    let elapsed = crate::progress::fmt_elapsed(secs);
    if clean {
        println!(
            "[ft] sync DONE. streams={} fetched={} unchanged={} deleted={} bytes={} in {elapsed} @ {mbps:.1} MB/s",
            stats_list.len(), total.got, total.skipped, total.deleted, total.bytes
        );
    } else {
        // A stream dropped after receiving data -> connection lost or server stopped.
        eprintln!("[ft] !! connection to the server was lost before the sync finished.");
        println!(
            "[ft] sync INCOMPLETE. fetched={} unchanged={} bytes={} in {elapsed} @ {mbps:.1} MB/s -- nothing deleted; re-run the same command to resume.",
            total.got, total.skipped, total.bytes
        );
        return Err("sync incomplete (connection lost)".into());
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_three_handles_spaces_in_rel() {
        let (s, m, r) = parse_three("123 637450560000000000 Share/a b/c.txt").unwrap();
        assert_eq!(s, 123);
        assert_eq!(m, 637450560000000000);
        assert_eq!(r, "Share/a b/c.txt");
    }

    #[test]
    fn parse_three_rejects_garbage() {
        assert!(parse_three("nope").is_none());
        assert!(parse_three("12 notanum rel").is_none());
    }
}
