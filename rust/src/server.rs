//! The serve side (`ft serve`). Milestone 3: single-stream `SYNC` — a lazy walk
//! that bundles small files, offers large files individually with raw or adaptive
//! per-block compression, honours ignore patterns, and supports two-phase cutover.
//! The classic accept loop enforces idle and stall timeouts and `--once`.

use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rustls::{ServerConfig, ServerConnection, StreamOwned};

use crate::compress::{zstd_compress, AdaptiveState, BLOCK_SIZE, MIN_COMPRESS};
use crate::ignore::IgnoreSet;
use crate::mtime;
use crate::paths::is_incompressible;
use crate::progress::Progress;
use crate::tls::ServerIdentity;
use crate::wire::Conn;
use crate::BoxError;

/// Files <= 1 MiB are batched into bundles.
const SMALL_FILE: u64 = 1_048_576;
/// Flush a bundle once it reaches ~10 MiB of files...
const BUNDLE_BYTES: u64 = 10_485_760;
/// ...or this many files.
const BUNDLE_MAX_COUNT: usize = 4096;
/// Coalesce a bundle's file bodies: flush once this many buffered bytes accumulate
/// (instead of one flush per file), so thousands of small files become a handful of
/// full-size TLS records/TCP segments rather than runt segments that stall the cwnd.
const BUNDLE_FLUSH: usize = 512 * 1024;

#[derive(Default)]
struct SendStats {
    offered: u64,
    sent: u64,
    skipped: u64,
    bytes: u64,
    wire: u64,
}

struct BundleItem {
    full: PathBuf,
    rel: String,
    size: u64,
    mtime: i64,
}

/// `full` relative to `base`, with the leading separator trimmed (matches the
/// PowerShell `full.Substring(base.Length).TrimStart('\','/')`).
fn rel_to_base(base: &Path, full: &Path) -> String {
    let b = base.to_string_lossy();
    let f = full.to_string_lossy();
    let r = f.get(b.len()..).unwrap_or("");
    r.trim_start_matches(['\\', '/']).to_string()
}

/// Read up to `buf.len()` bytes, looping over short reads; returns bytes read.
fn read_block(f: &mut File, buf: &mut [u8]) -> io::Result<usize> {
    let mut off = 0;
    while off < buf.len() {
        let n = f.read(&mut buf[off..])?;
        if n == 0 {
            break;
        }
        off += n;
    }
    Ok(off)
}

/// Send raw helper: `R <n>` + bytes.
fn send_raw_chunk<S: Read + Write>(conn: &mut Conn<S>, data: &[u8], stats: &mut SendStats) -> io::Result<()> {
    conn.put_line(&format!("R {}", data.len()));
    conn.put_bytes(data);
    conn.flush()?;
    stats.wire += data.len() as u64;
    Ok(())
}

/// Buffer one bundled small file WITHOUT flushing (the bundle flushes in batches).
/// `Some(l)` tries zstd at level `l` and keeps it only if it shrank below 95%;
/// `None` (or no shrink) sends raw. Deliberately does NOT touch the adaptive
/// controller: the bodies are coalesced, so a per-file write time is meaningless —
/// the large-file path (real per-block flushes) drives the level.
fn put_chunk_bundled<S: Read + Write>(conn: &mut Conn<S>, data: &[u8], level: Option<i32>, stats: &mut SendStats) {
    let n = data.len();
    if let Some(l) = level {
        if n >= MIN_COMPRESS {
            let cbuf = zstd_compress(data, l);
            if (cbuf.len() as f64) < (n as f64 * 0.95) {
                conn.put_line(&format!("Z {} {}", cbuf.len(), n));
                conn.put_bytes(&cbuf);
                stats.wire += cbuf.len() as u64;
                return;
            }
        }
    }
    conn.put_line(&format!("R {n}"));
    conn.put_bytes(data);
    stats.wire += n as u64;
}

/// Send one chunk of `data`, framed as `Z <clen> <rlen>` (zstd at the adaptive
/// level) or `R <rlen>` (raw) followed by the bytes. The level controller keeps
/// compression >= SPEED_MARGIN x the link; if even the floor level loses to raw
/// (a very fast link), it switches to raw and re-probes periodically. Shared by
/// the bundle path (one chunk per small file) and the large-file path (per block).
/// `state` is shared across all parallel streams (one controller per connection),
/// so the lock is held only for the tiny read/update — never during compress or write.
fn send_chunk_adaptive<S: Read + Write>(
    conn: &mut Conn<S>,
    data: &[u8],
    state: &Mutex<AdaptiveState>,
    stats: &mut SendStats,
) -> io::Result<()> {
    let n = data.len();

    // Tiny blocks (e.g. a large file's final tail) never beat a zstd frame -> raw,
    // and must not disturb the shared controller.
    if n < MIN_COMPRESS {
        return send_raw_chunk(conn, data, stats);
    }

    // Decide under a short lock: raw mode (incompressible run / link faster than the
    // floor) or compress at the shared level? The lock never spans compress or write.
    let level = {
        let mut s = state.lock().unwrap();
        if s.want_raw() {
            None
        } else {
            Some(s.level())
        }
    };
    let level = match level {
        None => return send_raw_chunk(conn, data, stats),
        Some(l) => l,
    };

    let t0 = Instant::now();
    let cbuf = zstd_compress(data, level);
    let tc = t0.elapsed().as_secs_f64();

    // Didn't shrink -> raw; a short run of these flips the controller to raw mode
    // (the level is NOT touched here — incompressibility is not a link-speed signal).
    if (cbuf.len() as f64) >= (n as f64 * 0.95) {
        send_raw_chunk(conn, data, stats)?;
        state.lock().unwrap().note_incompressible();
        return Ok(());
    }

    let t1 = Instant::now();
    conn.put_line(&format!("Z {} {}", cbuf.len(), n));
    conn.put_bytes(&cbuf);
    conn.flush()?;
    let tw = t1.elapsed().as_secs_f64();
    stats.wire += cbuf.len() as u64;
    // Re-pick the level from the windowed aggregate (per-block tw is buffer-distorted).
    state.lock().unwrap().note_compressed(tc, tw, n, cbuf.len());
    Ok(())
}

/// Send a bundle of small files. Returns `false` if the client dropped.
/// Each wanted file is framed like a large-file block: `Z <clen> <rlen>` / `R <rlen>`
/// (adaptive) or `-1` (locked on the source).
#[allow(clippy::too_many_arguments)]
fn send_bundle<S: Read + Write>(
    conn: &mut Conn<S>,
    bundle: &[BundleItem],
    stats: &mut SendStats,
    use_compress: bool,
    state: &Mutex<AdaptiveState>,
    prog: &Progress,
    stream_mode: bool,
) -> io::Result<bool> {
    if bundle.is_empty() {
        return Ok(true);
    }
    // Header + manifest in one write (one TLS record).
    let mut header = format!("B {}\n", bundle.len());
    for b in bundle {
        header.push_str(&format!("{} {} {}\n", b.size, b.mtime, b.rel));
    }
    conn.put_bytes(header.as_bytes());
    conn.flush()?;

    // Want-mask. On a fresh fetch (stream_mode) the client wants everything, so we
    // skip the per-bundle round-trip — that mask RTT was pure latency on a fresh
    // pull. Otherwise read the client's mask (resume / mirror / incremental).
    let mask: Vec<u8> = if stream_mode {
        vec![b'1'; bundle.len()]
    } else {
        match conn.read_line()? {
            Some(m) => m.into_bytes(),
            None => return Ok(false),
        }
    };

    // Compress bundled files at the controller's CURRENT level (read once); the
    // controller itself is driven by the large-file path (see put_chunk_bundled).
    let level: Option<i32> = if use_compress { Some(state.lock().unwrap().level()) } else { None };

    for (i, b) in bundle.iter().enumerate() {
        stats.offered += 1;
        let wanted = i < mask.len() && mask[i] == b'1';
        if !wanted {
            stats.skipped += 1;
            continue;
        }
        let mut f = match File::open(&b.full) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("[ft] cannot read (in use?), skipping: {} -- {e}", b.rel);
                conn.put_line("-1"); // buffered; flushed with the batch
                stats.skipped += 1;
                continue;
            }
        };
        let mut data = Vec::new();
        f.read_to_end(&mut data)?;
        let rlen = data.len();
        let ext = b
            .full
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();
        let lvl = if use_compress && rlen >= MIN_COMPRESS && !is_incompressible(&ext) { level } else { None };
        put_chunk_bundled(conn, &data, lvl, stats);
        // Coalesce: flush in BUNDLE_FLUSH-sized batches (not per file) so memory is
        // bounded and small files ride full-size records instead of runt segments.
        if conn.buffered_len() >= BUNDLE_FLUSH {
            conn.flush()?;
        }
        stats.sent += 1;
        stats.bytes += rlen as u64;
        prog.add(rlen as u64, 1);
    }
    conn.flush()?; // flush the bundle's tail
    Ok(true)
}

/// Send one already-opened large file: raw (`R <bytes>`) or adaptive (`Z` ... `E`).
#[allow(clippy::too_many_arguments)]
fn send_large_file<S: Read + Write>(
    conn: &mut Conn<S>,
    file: &mut File,
    offset: i64,
    ext: &str,
    stats: &mut SendStats,
    use_compress: bool,
    state: &Mutex<AdaptiveState>,
    prog: &Progress,
) -> io::Result<()> {
    let file_len = file.metadata()?.len() as i64;
    let mut remain = file_len - offset;
    if remain < 0 {
        remain = 0;
    }
    if offset > 0 {
        file.seek(SeekFrom::Start(offset as u64))?;
    }
    let zip = use_compress && remain >= MIN_COMPRESS as i64 && !is_incompressible(ext);
    if !zip {
        conn.put_line(&format!("R {remain}"));
        // stream in 1 MiB chunks with mid-file progress (so one big file isn't silent)
        let mut buf = vec![0u8; BLOCK_SIZE];
        let mut left = remain as u64;
        while left > 0 {
            let want = std::cmp::min(left, buf.len() as u64) as usize;
            let n = read_block(file, &mut buf[..want])?;
            if n == 0 {
                break;
            }
            conn.put_bytes(&buf[..n]);
            conn.flush()?;
            left -= n as u64;
            stats.bytes += n as u64;
            stats.wire += n as u64;
            prog.add(n as u64, 0);
        }
    } else {
        conn.send_line("Z")?;
        let mut buf = vec![0u8; BLOCK_SIZE];
        loop {
            let n = read_block(file, &mut buf)?;
            if n == 0 {
                break;
            }
            send_chunk_adaptive(conn, &buf[..n], state, stats)?;
            stats.bytes += n as u64;
            prog.add(n as u64, 0);
        }
        conn.send_line("E")?;
    }
    stats.sent += 1;
    prog.add(0, 1);
    Ok(())
}

/// One lazy walk of each shared folder. Returns `false` if the client dropped.
fn send_pass<S: Read + Write>(
    conn: &mut Conn<S>,
    roots: &[PathBuf],
    ignore: &IgnoreSet,
    use_compress: bool,
    margin: f64,
) -> io::Result<(bool, SendStats)> {
    let mut stats = SendStats::default();
    conn.send_line("T 0")?; // no up-front count
    let state = Mutex::new(AdaptiveState::new(margin));
    let prog = Progress::new("[ft serve]");
    let mut bundle: Vec<BundleItem> = Vec::new();
    let mut bundle_bytes: u64 = 0;

    for root in roots {
        let base: PathBuf = root.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| root.clone());
        let mut stack: Vec<(PathBuf, bool)> = vec![(root.clone(), false)];
        while let Some((dir, dirs_only)) = stack.pop() {
            if dirs_only {
                // ignored subtree: recreate the (empty) dir, recurse, send no files
                let rel_dir = rel_to_base(&base, &dir);
                if !rel_dir.is_empty() {
                    conn.send_line(&format!("D {rel_dir}"))?;
                }
                if let Ok(rd) = fs::read_dir(&dir) {
                    for e in rd.flatten() {
                        if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                            stack.push((e.path(), true));
                        }
                    }
                }
                continue;
            }
            // Classify entries: push subdirs (ignored => DirsOnly), collect files.
            let mut files: Vec<PathBuf> = Vec::new();
            if let Ok(rd) = fs::read_dir(&dir) {
                for e in rd.flatten() {
                    let ft = match e.file_type() {
                        Ok(t) => t,
                        Err(_) => continue,
                    };
                    let p = e.path();
                    if ft.is_dir() {
                        let ig = ignore.is_ignored(&rel_to_base(&base, &p), true);
                        stack.push((p, ig));
                    } else if ft.is_file() {
                        files.push(p);
                    }
                }
            }
            for full in files {
                let rel = rel_to_base(&base, &full);
                if ignore.is_ignored(&rel, false) {
                    continue;
                }
                let meta = match fs::metadata(&full) {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                let size = meta.len();
                let mt = mtime::read_ticks(&meta).unwrap_or(0);
                if size <= SMALL_FILE {
                    bundle.push(BundleItem { full: full.clone(), rel, size, mtime: mt });
                    bundle_bytes += size;
                    if bundle_bytes >= BUNDLE_BYTES || bundle.len() >= BUNDLE_MAX_COUNT {
                        if !send_bundle(conn, &bundle, &mut stats, use_compress, &state, &prog, false)? {
                            return Ok((false, stats));
                        }
                        bundle.clear();
                        bundle_bytes = 0;
                    }
                    continue;
                }
                // large file: flush any pending bundle, then offer it on its own
                if !bundle.is_empty() {
                    if !send_bundle(conn, &bundle, &mut stats, use_compress, &state, &prog, false)? {
                        return Ok((false, stats));
                    }
                    bundle.clear();
                    bundle_bytes = 0;
                }
                conn.send_line(&format!("F {size} {mt} {rel}"))?;
                stats.offered += 1;
                let resp = match conn.read_line()? {
                    Some(r) => r,
                    None => return Ok((false, stats)),
                };
                let offset: i64 = resp.trim().parse().unwrap_or(-1);
                if offset < 0 {
                    stats.skipped += 1;
                    continue;
                }
                let mut f = match File::open(&full) {
                    Ok(f) => f,
                    Err(e) => {
                        eprintln!("[ft] cannot read (in use?), skipping: {rel} -- {e}");
                        conn.send_line("-1")?;
                        stats.skipped += 1;
                        continue;
                    }
                };
                let ext = full
                    .extension()
                    .map(|e| format!(".{}", e.to_string_lossy()))
                    .unwrap_or_default();
                send_large_file(conn, &mut f, offset, &ext, &mut stats, use_compress, &state, &prog)?;
            }
        }
    }
    if !bundle.is_empty() && !send_bundle(conn, &bundle, &mut stats, use_compress, &state, &prog, false)? {
        return Ok((false, stats));
    }
    Ok((true, stats))
}

/// Cutover pause: wait for the operator to signal via an `ft-cutover.go` flag file
/// next to the binary, sending `PING` keepalives ~every 15 s.
fn wait_cutover<S: Read + Write>(conn: &mut Conn<S>) -> io::Result<()> {
    let go = cutover_flag_path();
    let _ = fs::remove_file(&go);
    eprintln!("========================================================================");
    eprintln!(" PHASE 1 complete. Now STOP THE DATABASE so its files are consistent.");
    eprintln!(" Then create the file to signal phase 2:");
    eprintln!("   {}", go.display());
    eprintln!("========================================================================");
    let mut ticks = 0u64;
    loop {
        if go.exists() {
            let _ = fs::remove_file(&go);
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
        ticks += 1;
        if ticks % 60 == 0 {
            conn.send_line("PING")?;
        }
    }
    eprintln!("[ft] cutover signal received - running final sync pass");
    Ok(())
}

fn cutover_flag_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ft-cutover.go")
}

/// Configuration for one serve run.
pub struct ServeConfig {
    pub folders: Vec<String>,
    pub port: u16,
    pub idle_seconds: u64,
    pub stall_timeout: u64,
    pub once: bool,
    pub cutover: bool,
    pub use_compress: bool,
    /// Adaptive-level coefficient: keep compression >= this many times the link speed.
    pub compress_margin: f64,
    pub ignore_spec: String,
    /// Serve only this source IP (application-layer gate, independent of the firewall).
    pub allow_ip: Option<String>,
}

/// Reject a peer whose IP is not the allowed one (mirrors the PowerShell
/// `if ($AllowIp -and $ip -ne $AllowIp)` check). Returns true if allowed.
fn ip_allowed(tcp: &TcpStream, allow_ip: &Option<String>) -> bool {
    match allow_ip {
        None => true,
        Some(want) => match tcp.peer_addr() {
            Ok(addr) => addr.ip().to_string() == *want,
            Err(_) => false,
        },
    }
}

/// Validate and canonicalise each shared folder (drops a trailing slash so the
/// folder's own name resolves correctly, like the PowerShell server).
fn resolve_folders(folders: &[String]) -> Result<Vec<PathBuf>, BoxError> {
    let mut out = Vec::new();
    for f in folders {
        let f = f.trim().trim_matches('"');
        if f.is_empty() {
            continue;
        }
        let p = Path::new(f);
        if !p.exists() {
            return Err(format!("folder not found: {f}").into());
        }
        out.push(crate::paths::canonicalize(p).map_err(|e| format!("cannot resolve folder {f}: {e}"))?);
    }
    if out.is_empty() {
        return Err("at least one folder is required (the folder to share)".into());
    }
    Ok(out)
}

/// Entry point for `ft serve` (single-stream / classic).
pub fn run_serve_single(cfg: ServeConfig, identity: &ServerIdentity, token: &str) -> Result<(), BoxError> {
    let roots = resolve_folders(&cfg.folders)?;
    let ignore = IgnoreSet::parse(&cfg.ignore_spec);

    let listener = TcpListener::bind(("0.0.0.0", cfg.port))?;
    listener.set_nonblocking(true)?;
    eprintln!(
        "[ft] listening 0.0.0.0:{}  folders={}  idle={}s  once={}",
        cfg.port,
        roots.len(),
        cfg.idle_seconds,
        cfg.once
    );

    loop {
        // Wait for a client, enforcing the idle timeout.
        let mut waited = 0u64;
        let tcp = loop {
            match listener.accept() {
                Ok((s, _)) => break s,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(200));
                    waited += 200;
                    if waited >= cfg.idle_seconds * 1000 {
                        eprintln!("[ft] idle timeout ({}s, no client) - shutting down", cfg.idle_seconds);
                        return Ok(());
                    }
                }
                Err(e) => return Err(e.into()),
            }
        };

        match handle_session(tcp, identity, token, &roots, &ignore, &cfg) {
            Ok(true) => {
                if cfg.once {
                    eprintln!("[ft] one-time job done - shutting down");
                    return Ok(());
                }
            }
            Ok(false) => {}
            Err(e) => eprintln!("[ft] session aborted: {e}"),
        }
    }
}

/// Handle one connection; returns whether the client completed cleanly (`BYE`).
fn handle_session(
    tcp: TcpStream,
    identity: &ServerIdentity,
    token: &str,
    roots: &[PathBuf],
    ignore: &IgnoreSet,
    cfg: &ServeConfig,
) -> Result<bool, BoxError> {
    if !ip_allowed(&tcp, &cfg.allow_ip) {
        let who = tcp.peer_addr().map(|a| a.to_string()).unwrap_or_default();
        eprintln!("[ft] session REJECTED from {who} (only {} is allowed)", cfg.allow_ip.as_deref().unwrap_or(""));
        return Ok(false);
    }
    tcp.set_nonblocking(false)?;
    tcp.set_nodelay(true)?;
    tcp.set_read_timeout(Some(Duration::from_secs(cfg.stall_timeout)))?;
    let server_conn = ServerConnection::new(identity.config.clone())?;
    let tls = StreamOwned::new(server_conn, tcp);
    let mut c = Conn::new(tls);

    let line = c.read_line()?;
    let authed = match line.as_deref().and_then(|l| l.strip_prefix("AUTH ")) {
        Some(t) => token.is_empty() || t == token,
        None => false,
    };
    if !authed {
        c.send_line("ERR auth")?;
        eprintln!("[ft] bad auth (rejected)");
        return Ok(false);
    }
    c.send_line("OK")?;
    // Push our config so the client doesn't have to carry ignore/streams: classic = 1 stream.
    c.send_line(&format!("CFG 1 {}", cfg.ignore_spec))?;
    eprintln!("[ft] client authenticated");

    let mut completed = false;
    loop {
        match c.read_line()? {
            None => break,
            Some(cmd) if cmd == "SYNC" => {
                eprintln!("[ft] sync pass 1");
                let t1 = Instant::now();
                let (ok1, s1) = send_pass(&mut c, roots, ignore, cfg.use_compress, cfg.compress_margin)?;
                if !ok1 {
                    eprintln!("[ft] client dropped during pass 1");
                    break;
                }
                c.send_line("PASS-END")?;
                eprintln!(
                    "[ft] pass 1 done: sent={} unchanged={} bytes={} wire={} in {}",
                    s1.sent, s1.skipped, s1.bytes, s1.wire,
                    crate::progress::fmt_elapsed(t1.elapsed().as_secs_f64())
                );
                if cfg.cutover {
                    wait_cutover(&mut c)?;
                    c.send_line("GO")?;
                    eprintln!("[ft] sync pass 2 (final)");
                    let t2 = Instant::now();
                    let (ok2, s2) = send_pass(&mut c, roots, ignore, cfg.use_compress, cfg.compress_margin)?;
                    if !ok2 {
                        eprintln!("[ft] client dropped during pass 2");
                        break;
                    }
                    c.send_line("PASS-END")?;
                    eprintln!(
                        "[ft] pass 2 done: sent={} unchanged={} bytes={} wire={} in {}",
                        s2.sent, s2.skipped, s2.bytes, s2.wire,
                        crate::progress::fmt_elapsed(t2.elapsed().as_secs_f64())
                    );
                }
                c.send_line("DONE")?;
            }
            Some(cmd) if cmd == "BYE" => {
                completed = true;
                break;
            }
            Some(_) => c.send_line("ERR cmd")?,
        }
    }
    Ok(completed)
}

// ===========================================================================
// Parallel mode (QSYNC): a bounded-queue producer + N handler threads. Units
// are fine-grained (a bundle, one large file, or an empty-dir marker), so even
// one giant folder spreads across all streams. Mirrors `Invoke-ParallelServe`.
// ===========================================================================

/// Backpressure cap on the work queue (matches the PowerShell `$cap`).
const QUEUE_CAP: usize = 256;

/// A unit of work pulled by a handler.
enum Unit {
    Dir(String),
    Bundle(Vec<BundleItem>),
    Large { full: PathBuf, rel: String, size: u64, mtime: i64 },
}

/// A simple shared FIFO; handlers poll `try_pop` (like the PowerShell `TryDequeue`).
struct WorkQueue {
    q: Mutex<VecDeque<Unit>>,
}

impl WorkQueue {
    fn new() -> Self {
        WorkQueue { q: Mutex::new(VecDeque::new()) }
    }
    fn len(&self) -> usize {
        self.q.lock().unwrap().len()
    }
    fn try_pop(&self) -> Option<Unit> {
        self.q.lock().unwrap().pop_front()
    }
    /// Enqueue with backpressure: wait while the queue is at capacity.
    fn enqueue_blocking(&self, u: Unit) {
        while self.len() >= QUEUE_CAP {
            std::thread::sleep(Duration::from_millis(20));
        }
        self.q.lock().unwrap().push_back(u);
    }
}

/// The producer: lazily walk the roots and assemble units into the queue.
fn producer_walk(roots: &[PathBuf], ignore: &IgnoreSet, queue: &WorkQueue) {
    let mut bundle: Vec<BundleItem> = Vec::new();
    let mut bbytes: u64 = 0;
    for root in roots {
        let base: PathBuf = root.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| root.clone());
        let mut stack: Vec<(PathBuf, bool)> = vec![(root.clone(), false)];
        while let Some((dir, dirs_only)) = stack.pop() {
            if dirs_only {
                let rel_dir = rel_to_base(&base, &dir);
                if !rel_dir.is_empty() {
                    queue.enqueue_blocking(Unit::Dir(rel_dir));
                }
                if let Ok(rd) = fs::read_dir(&dir) {
                    for e in rd.flatten() {
                        if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                            stack.push((e.path(), true));
                        }
                    }
                }
                continue;
            }
            let mut files: Vec<PathBuf> = Vec::new();
            if let Ok(rd) = fs::read_dir(&dir) {
                for e in rd.flatten() {
                    let ft = match e.file_type() {
                        Ok(t) => t,
                        Err(_) => continue,
                    };
                    let p = e.path();
                    if ft.is_dir() {
                        let ig = ignore.is_ignored(&rel_to_base(&base, &p), true);
                        stack.push((p, ig));
                    } else if ft.is_file() {
                        files.push(p);
                    }
                }
            }
            for full in files {
                let rel = rel_to_base(&base, &full);
                if ignore.is_ignored(&rel, false) {
                    continue;
                }
                let meta = match fs::metadata(&full) {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                let size = meta.len();
                let mt = mtime::read_ticks(&meta).unwrap_or(0);
                if size <= SMALL_FILE {
                    bundle.push(BundleItem { full: full.clone(), rel, size, mtime: mt });
                    bbytes += size;
                    if bbytes >= BUNDLE_BYTES || bundle.len() >= BUNDLE_MAX_COUNT {
                        queue.enqueue_blocking(Unit::Bundle(std::mem::take(&mut bundle)));
                        bbytes = 0;
                    }
                    continue;
                }
                // large file: flush any pending bundle, then offer it on its own
                if !bundle.is_empty() {
                    queue.enqueue_blocking(Unit::Bundle(std::mem::take(&mut bundle)));
                    bbytes = 0;
                }
                queue.enqueue_blocking(Unit::Large { full, rel, size, mtime: mt });
            }
        }
    }
    if !bundle.is_empty() {
        queue.enqueue_blocking(Unit::Bundle(bundle));
    }
}

/// One handler thread: AUTH, expect `QSYNC`, then serve units until `NOUNIT`.
#[allow(clippy::too_many_arguments)]
fn parallel_handler(
    tcp: TcpStream,
    config: Arc<ServerConfig>,
    token: String,
    queue: Arc<WorkQueue>,
    done: Arc<AtomicBool>,
    use_compress: bool,
    stall: u64,
    allow_ip: Option<String>,
    streams: i32,
    ignore_spec: String,
    prog: Progress,
    state: Arc<Mutex<AdaptiveState>>,
) {
    if !ip_allowed(&tcp, &allow_ip) {
        let who = tcp.peer_addr().map(|a| a.to_string()).unwrap_or_default();
        eprintln!("[ft] parallel conn REJECTED from {who} (only {} is allowed)", allow_ip.as_deref().unwrap_or(""));
        return;
    }
    if let Err(e) = (|| -> io::Result<()> {
        tcp.set_nonblocking(false)?;
        tcp.set_nodelay(true)?;
        tcp.set_read_timeout(Some(Duration::from_secs(stall)))?;
        let server_conn = ServerConnection::new(config).map_err(io::Error::other)?;
        let mut c = Conn::new(StreamOwned::new(server_conn, tcp));

        let line = c.read_line()?;
        let authed = match line.as_deref().and_then(|l| l.strip_prefix("AUTH ")) {
            Some(t) => token.is_empty() || t == token,
            None => false,
        };
        if !authed {
            c.send_line("ERR auth")?;
            return Ok(());
        }
        c.send_line("OK")?;
        // Push our config so the client doesn't have to carry ignore/streams.
        c.send_line(&format!("CFG {streams} {ignore_spec}"))?;
        // `QSYNC STREAM` = the client's destination is empty (a fresh pull), so it
        // wants every file at offset 0 — we can stream without the per-unit want-mask
        // / offset round-trips (pure latency on a fresh fetch). Plain `QSYNC` keeps
        // the round-trips (resume / mirror / incremental).
        let stream_mode = match c.read_line()?.as_deref() {
            Some("QSYNC") => false,
            Some("QSYNC STREAM") => true,
            _ => {
                c.send_line("ERR cmd")?;
                return Ok(());
            }
        };

        let mut stats = SendStats::default();
        let mut last_send = Instant::now();
        loop {
            let unit = match queue.try_pop() {
                Some(u) => u,
                None => {
                    if done.load(Ordering::Acquire) {
                        c.send_line("NOUNIT")?;
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(15));
                    // keepalive while waiting for the producer, so the client's read
                    // timeout never fires on a slow walk (the client ignores PING).
                    if last_send.elapsed() >= Duration::from_secs(10) {
                        c.send_line("PING")?;
                        last_send = Instant::now();
                    }
                    continue;
                }
            };
            last_send = Instant::now();
            match unit {
                Unit::Dir(rel) => c.send_line(&format!("D {rel}"))?,
                Unit::Bundle(items) => {
                    if !send_bundle(&mut c, &items, &mut stats, use_compress, &state, &prog, stream_mode)? {
                        return Ok(()); // client dropped
                    }
                }
                Unit::Large { full, rel, size, mtime } => {
                    c.send_line(&format!("F {size} {mtime} {rel}"))?;
                    stats.offered += 1;
                    // Fresh fetch: skip the offset round-trip and send from the start.
                    let offset: i64 = if stream_mode {
                        0
                    } else {
                        match c.read_line()? {
                            Some(r) => r.trim().parse().unwrap_or(-1),
                            None => return Ok(()), // client dropped
                        }
                    };
                    if offset < 0 {
                        stats.skipped += 1;
                        continue;
                    }
                    let mut f = match File::open(&full) {
                        Ok(f) => f,
                        Err(e) => {
                            eprintln!("[ft] cannot read (in use?), skipping: {rel} -- {e}");
                            c.send_line("-1")?;
                            stats.skipped += 1;
                            continue;
                        }
                    };
                    let ext = full
                        .extension()
                        .map(|e| format!(".{}", e.to_string_lossy()))
                        .unwrap_or_default();
                    send_large_file(&mut c, &mut f, offset, &ext, &mut stats, use_compress, &state, &prog)?;
                }
            }
        }
        Ok(())
    })() {
        eprintln!("[ft] parallel conn aborted: {e}");
    }
}

/// Entry point for `ft serve` with `--streams > 1` (parallel `QSYNC`).
pub fn run_serve_parallel(cfg: ServeConfig, identity: &ServerIdentity, token: &str, streams: i32) -> Result<(), BoxError> {
    let roots = resolve_folders(&cfg.folders)?;
    let ignore = Arc::new(IgnoreSet::parse(&cfg.ignore_spec));
    let queue = Arc::new(WorkQueue::new());
    let done = Arc::new(AtomicBool::new(false));

    let listener = TcpListener::bind(("0.0.0.0", cfg.port))?;
    listener.set_nonblocking(true)?;
    eprintln!(
        "[ft] PARALLEL listening 0.0.0.0:{}  folders={}  streams={}  idle={}s",
        cfg.port,
        roots.len(),
        streams,
        cfg.idle_seconds
    );

    // Producer thread.
    let producer = {
        let queue = queue.clone();
        let done = done.clone();
        let ignore = ignore.clone();
        std::thread::spawn(move || {
            producer_walk(&roots, &ignore, &queue);
            done.store(true, Ordering::Release);
        })
    };

    // One shared progress across all handler threads, so the live line is the
    // aggregate of the whole transfer (not per-connection numbers that jump).
    let prog = Progress::new("[ft serve]");

    // One shared adaptive-level controller across ALL streams: pooling every
    // stream's measurements converges the level fast (a single stream sees too
    // little data in parallel mode) and keeps all streams at one consistent level.
    let state = Arc::new(Mutex::new(AdaptiveState::new(cfg.compress_margin)));

    // Accept loop with the parallel shutdown rules.
    let mut handles: Vec<std::thread::JoinHandle<()>> = Vec::new();
    let mut idle: u64 = 0;
    let mut connected_any = false;
    loop {
        match listener.accept() {
            Ok((tcp, _)) => {
                connected_any = true;
                idle = 0;
                let config = identity.config.clone();
                let token = token.to_string();
                let queue = queue.clone();
                let done = done.clone();
                let use_compress = cfg.use_compress;
                let stall = cfg.stall_timeout;
                let allow_ip = cfg.allow_ip.clone();
                let ignore_spec = cfg.ignore_spec.clone();
                let prog = prog.clone();
                let state = state.clone();
                handles.push(std::thread::spawn(move || {
                    parallel_handler(tcp, config, token, queue, done, use_compress, stall, allow_ip, streams, ignore_spec, prog, state);
                }));
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(150));
                idle += 150;
            }
            Err(e) => return Err(e.into()),
        }
        handles.retain(|h| !h.is_finished());

        if connected_any && handles.is_empty() {
            if done.load(Ordering::Acquire) && queue.len() == 0 {
                break; // clean finish
            }
            if idle >= 5000 {
                eprintln!("[ft] all clients gone - stopping");
                break;
            }
        }
        if !connected_any && idle >= cfg.idle_seconds * 1000 {
            eprintln!("[ft] idle timeout ({}s, no client) - shutting down", cfg.idle_seconds);
            break;
        }
    }

    let _ = producer.join();
    for h in handles {
        let _ = h.join();
    }
    eprintln!("[ft] parallel job done in {}", crate::progress::fmt_elapsed(prog.elapsed_secs()));
    Ok(())
}
