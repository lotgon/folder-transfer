//! Block compression for the transfer: zstd (vendored libzstd, statically linked)
//! plus the adaptive zstd-level controller (shared across a connection's streams).

/// Block size for adaptive compression (1 MiB).
pub const BLOCK_SIZE: usize = 1 << 20;

/// zstd level bounds + the adaptive-level control band.
pub const LEVEL_START: i32 = 3;
pub const LEVEL_MIN: i32 = -5; // ultra-fast
pub const LEVEL_MAX: i32 = 19;
/// Default coefficient: compression must stay at least this many times faster than
/// the link. Overridable via --compress-margin. The raise threshold is derived
/// (margin + 0.4), so the level settles at ~margin..(margin+0.4) x the link —
/// at the coefficient, with margin, never "at the edge".
pub const SPEED_MARGIN: f64 = 1.6;
/// In raw mode (incompressible data, or a link faster than even the floor level),
/// re-probe compression every N chunks.
pub const RAW_REPROBE: i32 = 64;
/// Blocks below this size never benefit from a zstd frame -> always sent raw.
pub const MIN_COMPRESS: usize = 256;
/// Consecutive blocks that don't shrink before we stop compressing (switch to raw).
pub const INCOMPRESSIBLE_STREAK: i32 = 4;
/// Re-evaluate the level once per this many WIRE bytes. Must be well above the
/// socket/proxy buffer so the aggregate write time reflects the real link rate
/// (per-block write time is meaningless — writes return into the buffer instantly).
pub const WINDOW_BYTES: i64 = 4 << 20;

/// Compress with zstd at the given level (negative = ultra-fast .. ~19 high).
/// One-shot; the zstd frame is self-describing, so the decoder needs only the bytes.
pub fn zstd_compress(data: &[u8], level: i32) -> Vec<u8> {
    zstd::bulk::compress(data, level).expect("zstd compress to Vec cannot fail")
}

/// Decompress a zstd frame into at most `expected_len` bytes.
pub fn zstd_decompress(data: &[u8], expected_len: usize) -> std::io::Result<Vec<u8>> {
    zstd::bulk::decompress(data, expected_len)
}

/// zstd treats level 0 as "use the default" (currently 3), so the controller must
/// never sit on 0 or it would silently run level 3 while believing it's near the floor.
fn nonzero_level(l: i32) -> i32 {
    if l == 0 {
        -1
    } else {
        l
    }
}

/// Adaptive zstd-level controller, SHARED across all streams of one transfer
/// (one `Arc<Mutex<AdaptiveState>>` per connection). Two independent concerns:
///   * compress-vs-raw: a short run of blocks that don't shrink flips to raw mode
///     (with periodic re-probe), so incompressible data isn't recompressed forever;
///   * how-hard: among compressed blocks, pick the highest zstd level whose
///     aggregate speed stays >= `margin` x the link (measured over a WINDOW of wire
///     bytes — per-block write time is meaningless, it's absorbed by the socket
///     buffer). Slow link -> climbs (more ratio); fast link -> ultra-fast floor.
#[derive(Default)]
pub struct AdaptiveState {
    /// Current zstd level (kept != 0, see `nonzero_level`).
    pub level: i32,
    /// Raw mode: send raw and re-probe compression every RAW_REPROBE chunks.
    pub prefer_raw: bool,
    /// Chunks sent raw since the last compression probe.
    raw_since: i32,
    /// Consecutive compressed-attempts that did not shrink.
    incompressible_run: i32,
    /// Coefficient: keep compression >= `margin` x the link; climb while >= `raise`.
    margin: f64,
    raise: f64,
    // Window accumulators for the level decision (compressed blocks only).
    win_tc: f64,
    win_tw: f64,
    win_orig: i64,
    win_wire: i64,
}

impl AdaptiveState {
    /// `margin` is the coefficient (compression must stay >= margin x the link).
    /// Clamped to a sane range so a typo can't silently disable compression.
    pub fn new(margin: f64) -> Self {
        let margin = if margin.is_finite() { margin.clamp(1.0, 16.0) } else { SPEED_MARGIN };
        Self { level: LEVEL_START, margin, raise: margin + 0.4, ..Default::default() }
    }

    /// The zstd level to compress the next block at (never 0).
    pub fn level(&self) -> i32 {
        nonzero_level(self.level)
    }

    /// Call at the start of each chunk. Returns true if we're in raw mode (send raw);
    /// advances the re-probe counter and exits raw mode for one probe every RAW_REPROBE.
    pub fn want_raw(&mut self) -> bool {
        if self.prefer_raw {
            self.raw_since += 1;
            if self.raw_since >= RAW_REPROBE {
                self.prefer_raw = false; // probe compression on the next chunk
                self.raw_since = 0;
                self.reset_window();
            }
            true
        } else {
            false
        }
    }

    /// A block that actually compressed: feed the level window. Once a window of
    /// WINDOW_BYTES (wire) is full, re-pick the level from headroom = Σtw/Σtc.
    pub fn note_compressed(&mut self, tc: f64, tw: f64, orig: usize, wire: usize) {
        self.incompressible_run = 0;
        self.win_tc += tc;
        self.win_tw += tw;
        self.win_orig += orig as i64;
        self.win_wire += wire as i64;
        if self.win_wire < WINDOW_BYTES {
            return;
        }
        let headroom = if self.win_tc > 1e-9 { self.win_tw / self.win_tc } else { f64::INFINITY };
        if headroom >= self.raise && self.level < LEVEL_MAX {
            let step = if headroom >= 12.0 { 4 } else if headroom >= 6.0 { 3 } else { 2 };
            self.level = nonzero_level((self.level + step).min(LEVEL_MAX));
        } else if headroom < self.margin && self.level > LEVEL_MIN {
            let step = if headroom < 0.5 { 3 } else if headroom < 1.0 { 2 } else { 1 };
            self.level = nonzero_level((self.level - step).max(LEVEL_MIN));
        }
        // Genuinely fast link: at the floor and compression still can't keep up -> raw.
        if self.level <= LEVEL_MIN && headroom < 1.0 {
            self.prefer_raw = true;
            self.raw_since = 0;
        }
        self.reset_window();
    }

    /// A block that did not shrink (`clen >= 0.95 * n`). After a short run we stop
    /// compressing (raw mode); the level is left alone (incompressibility is not a
    /// link-speed signal, so it must not drag the shared level).
    pub fn note_incompressible(&mut self) {
        self.incompressible_run += 1;
        if self.incompressible_run >= INCOMPRESSIBLE_STREAK {
            self.prefer_raw = true;
            self.raw_since = 0;
            self.incompressible_run = 0;
            self.reset_window();
        }
    }

    fn reset_window(&mut self) {
        self.win_tc = 0.0;
        self.win_tw = 0.0;
        self.win_orig = 0;
        self.win_wire = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zstd_round_trip() {
        let data = b"the quick brown fox jumps over the lazy dog ".repeat(500);
        for lvl in [-3, 1, 3, 9, 19] {
            let c = zstd_compress(&data, lvl);
            assert!(c.len() < data.len(), "zstd level {lvl} should compress");
            let d = zstd_decompress(&c, data.len()).unwrap();
            assert_eq!(d, data, "round-trip at level {lvl}");
        }
    }

    const W: usize = WINDOW_BYTES as usize;

    #[test]
    fn level_climbs_on_slow_link() {
        let mut st = AdaptiveState::new(SPEED_MARGIN);
        let start = st.level;
        st.note_compressed(0.001, 0.010, W, W); // headroom 10 -> climb
        assert!(st.level > start, "slow link should raise the level");
    }

    #[test]
    fn level_falls_on_fast_link() {
        let mut st = AdaptiveState::new(SPEED_MARGIN);
        let start = st.level;
        st.note_compressed(0.010, 0.001, W, W); // headroom 0.1 -> drop
        assert!(st.level < start, "fast link should lower the level");
    }

    #[test]
    fn level_holds_in_band() {
        let mut st = AdaptiveState::new(SPEED_MARGIN);
        let start = st.level;
        st.note_compressed(0.010, 0.018, W, W); // headroom 1.8 in [1.6, 2.0) -> hold
        assert_eq!(st.level, start, "headroom in the band should hold");
    }

    #[test]
    fn level_clamped_and_never_zero() {
        let mut st = AdaptiveState::new(SPEED_MARGIN);
        for _ in 0..50 {
            st.note_compressed(0.001, 1.0, W, W); // huge headroom -> climb
            assert_ne!(st.level, 0, "level must never settle on 0 (zstd default)");
        }
        assert_eq!(st.level, LEVEL_MAX);
        let mut st = AdaptiveState::new(SPEED_MARGIN);
        for _ in 0..50 {
            st.note_compressed(1.0, 0.001, W, W); // no headroom -> drop (crosses 0)
            assert_ne!(st.level, 0, "level must never settle on 0 (zstd default)");
        }
        assert_eq!(st.level, LEVEL_MIN);
    }

    #[test]
    fn incompressible_run_enters_raw() {
        let mut st = AdaptiveState::new(SPEED_MARGIN);
        for _ in 0..(INCOMPRESSIBLE_STREAK - 1) {
            st.note_incompressible();
            assert!(!st.prefer_raw, "should not flip before the streak completes");
        }
        st.note_incompressible();
        assert!(st.prefer_raw, "a full incompressible streak should switch to raw");
    }

    #[test]
    fn raw_mode_reprobes() {
        let mut st = AdaptiveState::new(SPEED_MARGIN);
        st.prefer_raw = true;
        for _ in 0..(RAW_REPROBE - 1) {
            assert!(st.want_raw(), "still in raw mode before the re-probe");
        }
        // the RAW_REPROBE-th call exits raw mode to probe compression next chunk
        assert!(st.want_raw());
        assert!(!st.prefer_raw, "re-probe should exit raw mode");
        assert!(!st.want_raw(), "next chunk compresses (probe)");
    }

    #[test]
    fn margin_is_clamped() {
        assert_eq!(AdaptiveState::new(0.5).margin, 1.0, "below-1 margin clamps to 1.0");
        assert_eq!(AdaptiveState::new(1000.0).margin, 16.0, "huge margin clamps so it can't disable compression");
        assert_eq!(AdaptiveState::new(f64::NAN).margin, SPEED_MARGIN, "NaN falls back to default");
    }
}
