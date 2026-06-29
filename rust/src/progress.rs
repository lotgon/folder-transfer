//! Aggregated, throttled progress (~ every 1.5 s). One shared counter across all
//! parallel streams, so the live line shows the WHOLE transfer (total files, MB,
//! elapsed time, current MB/s) instead of each stream printing its own numbers.
//! Cheap to call per chunk; only one thread prints per interval (try-lock).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

const MB: f64 = 1_048_576.0;
const INTERVAL_SECS: f64 = 1.5;

#[derive(Clone)]
pub struct Progress {
    label: Arc<str>,
    start: Instant,
    files: Arc<AtomicU64>,
    bytes: Arc<AtomicU64>,
    /// (time of last printed line, total bytes at that time) — guards printing.
    last: Arc<Mutex<(Instant, u64)>>,
}

impl Progress {
    pub fn new(label: impl Into<String>) -> Self {
        let start = Instant::now();
        Progress {
            label: Arc::from(label.into()),
            start,
            files: Arc::new(AtomicU64::new(0)),
            bytes: Arc::new(AtomicU64::new(0)),
            last: Arc::new(Mutex::new((start, 0))),
        }
    }

    /// Record `bytes` more payload and `files` more completed files, then print a
    /// throttled aggregate line if it is time.
    pub fn add(&self, bytes: u64, files: u64) {
        if bytes > 0 {
            self.bytes.fetch_add(bytes, Ordering::Relaxed);
        }
        if files > 0 {
            self.files.fetch_add(files, Ordering::Relaxed);
        }
        self.maybe_print();
    }

    fn maybe_print(&self) {
        let now = Instant::now();
        // try_lock: if another thread is printing, skip — never block the transfer.
        let mut g = match self.last.try_lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        let dt = now.duration_since(g.0).as_secs_f64();
        if dt < INTERVAL_SECS {
            return;
        }
        let bytes = self.bytes.load(Ordering::Relaxed);
        let files = self.files.load(Ordering::Relaxed);
        let spd = (bytes.saturating_sub(g.1)) as f64 / MB / dt;
        let elapsed = fmt_elapsed(now.duration_since(self.start).as_secs_f64());
        eprintln!(
            "{} {files} files, {:.1} MB in {elapsed} @ {:.1} MB/s",
            self.label,
            bytes as f64 / MB,
            spd
        );
        g.0 = now;
        g.1 = bytes;
    }

    /// Seconds since this progress started (for the final summary).
    pub fn elapsed_secs(&self) -> f64 {
        Instant::now().duration_since(self.start).as_secs_f64()
    }
}

/// `h:mm:ss` (or `mm:ss` under an hour).
pub fn fmt_elapsed(secs: f64) -> String {
    let s = secs as u64;
    let (h, m, sec) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{sec:02}")
    } else {
        format!("{m:02}:{sec:02}")
    }
}
