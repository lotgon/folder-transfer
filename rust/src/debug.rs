//! Lightweight debug logging for important runtime decisions — chiefly the adaptive
//! compression controller (level changes, raw-mode toggles, per-window measurements).
//!
//! Off unless enabled via `--debug`. When on, every line goes to BOTH stderr (so it
//! shows in the console) AND a debug log file (so it can be captured and shared).
//! Disabled = a single atomic load, so call sites are cheap; still, guard expensive
//! `format!` args with `if debug::enabled()`.

use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

static ON: AtomicBool = AtomicBool::new(false);
static FILE: OnceLock<Mutex<Option<File>>> = OnceLock::new();
static START: OnceLock<Instant> = OnceLock::new();

/// Turn debug logging on and open the log file (truncating any previous run).
/// Safe to call once; later calls are ignored.
pub fn init(path: &str) {
    if START.set(Instant::now()).is_err() {
        return; // already initialised
    }
    let file = match File::create(path) {
        Ok(f) => {
            let abs = std::fs::canonicalize(path).map(|p| p.display().to_string()).unwrap_or_else(|_| path.to_string());
            eprintln!("[ft] debug logging ON -> {abs}");
            Some(f)
        }
        Err(e) => {
            eprintln!("[ft] WARN: could not open debug log {path}: {e} (logging to stderr only)");
            None
        }
    };
    let _ = FILE.set(Mutex::new(file));
    ON.store(true, Ordering::Relaxed);
}

/// Is debug logging on? (One relaxed atomic load.)
pub fn enabled() -> bool {
    ON.load(Ordering::Relaxed)
}

/// Emit one debug line (prefixed with seconds since init) to stderr and the log file.
pub fn log(msg: &str) {
    if !enabled() {
        return;
    }
    let t = START.get().map(|s| s.elapsed().as_secs_f64()).unwrap_or(0.0);
    let line = format!("[ft-dbg {t:8.3}] {msg}");
    eprintln!("{line}");
    if let Some(m) = FILE.get() {
        if let Ok(mut g) = m.lock() {
            if let Some(f) = g.as_mut() {
                let _ = writeln!(f, "{line}");
            }
        }
    }
}
