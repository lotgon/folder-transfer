//! Block compression for the transfer: zstd (vendored libzstd, statically linked)
//! plus the per-connection adaptive zstd-level controller.

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
/// In raw mode (link faster than even the floor level), re-probe every N chunks.
pub const RAW_REPROBE: i32 = 64;
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

/// Per-connection adaptive zstd-level controller. Picks the highest level whose
/// compression speed stays >= SPEED_MARGIN x the link speed, so compression never
/// becomes the bottleneck and never sits "at the edge": a slow link climbs toward
/// higher levels (more ratio), a fast link falls toward the ultra-fast floor (and,
/// if even the floor can't keep up, switches to raw). State persists across files
/// in one connection.
#[derive(Default)]
pub struct AdaptiveState {
    /// Current zstd level.
    pub level: i32,
    /// Last block's ratio `n / clen` (for reporting).
    pub cz_ratio: f64,
    /// Link so fast that even the floor level loses to raw: send raw, re-probe later.
    pub prefer_raw: bool,
    /// Chunks sent raw since the last compression probe.
    pub raw_since: i32,
    /// Coefficient: keep compression >= `margin` x the link; climb while >= `raise`.
    margin: f64,
    raise: f64,
    // Window accumulators (reset every WINDOW_BYTES of wire output).
    win_tc: f64,
    win_tw: f64,
    win_orig: i64,
    win_wire: i64,
}

impl AdaptiveState {
    /// `margin` is the coefficient (compression must stay >= margin x the link).
    pub fn new(margin: f64) -> Self {
        let margin = if margin.is_finite() && margin >= 1.0 { margin } else { SPEED_MARGIN };
        Self { level: LEVEL_START, margin, raise: margin + 0.4, ..Default::default() }
    }

    /// Record one compressed block (compress time `tc`, socket write time `tw`,
    /// `orig`/`wire` bytes). Once a window of WINDOW_BYTES has been written, re-pick
    /// the level from the AGGREGATE headroom = Σtw/Σtc (how many times faster
    /// compression is than the link): comfortable headroom -> climb (more ratio),
    /// margin threatened -> drop. Steps proportionally so it converges quickly.
    pub fn record(&mut self, tc: f64, tw: f64, orig: usize, wire: usize) {
        self.win_tc += tc;
        self.win_tw += tw;
        self.win_orig += orig as i64;
        self.win_wire += wire as i64;
        if self.win_wire < WINDOW_BYTES {
            return;
        }
        let headroom = if self.win_tc > 1e-9 { self.win_tw / self.win_tc } else { f64::INFINITY };
        if headroom >= self.raise && self.level < LEVEL_MAX {
            // climb fast (few windows per transfer); it self-corrects down by 1 if it overshoots.
            let step = if headroom >= 12.0 { 4 } else if headroom >= 6.0 { 3 } else { 2 };
            self.level = (self.level + step).min(LEVEL_MAX);
        } else if headroom < self.margin && self.level > LEVEL_MIN {
            let step = if headroom < 0.5 { 3 } else if headroom < 1.0 { 2 } else { 1 };
            self.level = (self.level - step).max(LEVEL_MIN);
        }
        // Very fast link: even the floor level loses to raw end-to-end -> raw mode.
        let avg_ratio = self.win_orig as f64 / self.win_wire.max(1) as f64;
        if self.level <= LEVEL_MIN && (self.win_tc + self.win_tw) > self.win_tw * avg_ratio {
            self.prefer_raw = true;
            self.raw_since = 0;
        }
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

    #[test]
    fn level_climbs_on_slow_link() {
        let mut st = AdaptiveState::new(SPEED_MARGIN);
        let start = st.level;
        // one full window written 10x slower than compress -> headroom 10 -> climb
        st.record(0.001, 0.010, WINDOW_BYTES as usize, WINDOW_BYTES as usize);
        assert!(st.level > start, "slow link should raise the level");
    }

    #[test]
    fn level_falls_on_fast_link() {
        let mut st = AdaptiveState::new(SPEED_MARGIN);
        let start = st.level;
        st.record(0.010, 0.001, WINDOW_BYTES as usize, WINDOW_BYTES as usize); // headroom 0.1
        assert!(st.level < start, "fast link should lower the level");
    }

    #[test]
    fn level_holds_in_band() {
        let mut st = AdaptiveState::new(SPEED_MARGIN);
        let start = st.level;
        st.record(0.010, 0.018, WINDOW_BYTES as usize, WINDOW_BYTES as usize); // headroom 1.8 in [1.6,2.0)
        assert_eq!(st.level, start, "headroom in the band should hold");
    }

    #[test]
    fn level_clamped_to_range() {
        let mut st = AdaptiveState::new(SPEED_MARGIN);
        for _ in 0..50 {
            st.record(0.001, 1.0, WINDOW_BYTES as usize, WINDOW_BYTES as usize); // huge headroom
        }
        assert_eq!(st.level, LEVEL_MAX);
        let mut st = AdaptiveState::new(SPEED_MARGIN);
        for _ in 0..50 {
            st.record(1.0, 0.001, WINDOW_BYTES as usize, WINDOW_BYTES as usize); // no headroom
        }
        assert_eq!(st.level, LEVEL_MIN);
    }
}
