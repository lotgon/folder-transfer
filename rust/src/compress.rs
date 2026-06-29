//! RAW DEFLATE (RFC 1951) to interoperate with .NET `DeflateStream`, plus the
//! per-connection adaptive A/B compression state. See spec sections 6.4 and 6.6.
//!
//! NOTE: raw deflate, NOT zlib/gzip. flate2's `Deflate*` types are raw deflate;
//! `Zlib*`/`Gz*` would add a header and break interop.

use std::io::{Read, Write};

use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;
use flate2::Compression;

/// Block size for adaptive compression (1 MiB), matching the PowerShell server.
pub const BLOCK_SIZE: usize = 1 << 20;
/// Re-probe the other mode every this many blocks.
pub const REPROBE: i32 = 64;

/// Compress with raw deflate at a "fastest"-equivalent level (.NET CompressionLevel.Fastest).
pub fn deflate_raw(data: &[u8]) -> Vec<u8> {
    let mut enc = DeflateEncoder::new(Vec::with_capacity(data.len() / 2 + 16), Compression::fast());
    enc.write_all(data).expect("write to Vec cannot fail");
    enc.finish().expect("finish to Vec cannot fail")
}

/// Inflate raw deflate into exactly `expected_len` bytes (best effort: returns
/// whatever decoded, like the PowerShell client which stops on the first 0-read).
pub fn inflate_raw(data: &[u8], expected_len: usize) -> std::io::Result<Vec<u8>> {
    let mut dec = DeflateDecoder::new(data);
    let mut out = vec![0u8; expected_len];
    let mut off = 0;
    while off < expected_len {
        let n = dec.read(&mut out[off..])?;
        if n == 0 {
            break;
        }
        off += n;
    }
    out.truncate(off);
    Ok(out)
}

/// Per-connection adaptive-compression measurements. Persist across files in a
/// single connection (`Send-LargeFile` in ft-server.ps1). See spec section 6.6.
#[derive(Default)]
pub struct AdaptiveState {
    /// Raw mode: original bytes sent and seconds taken (write time only).
    pub cz_raw_bytes: i64,
    pub cz_raw_sec: f64,
    /// Compressed mode: original bytes, with the two stage times kept SEPARATELY.
    /// `send_large_file` pipelines deflate (CPU, worker thread) concurrently with the
    /// socket write (main thread), so the effective compressed time is the slower
    /// stage -- `max(deflate, write)` -- not their sum; hence two accumulators.
    pub cz_cmp_bytes: i64,
    pub cz_cmp_deflate_sec: f64,
    pub cz_cmp_write_sec: f64,
    /// Last block's ratio `rlen/clen` (`n / clen`).
    pub cz_ratio: f64,
    /// Blocks since the last re-probe.
    pub cz_since: i32,
}

impl AdaptiveState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Decide whether to compress a block of `n` original bytes. Returns
    /// `(do_compress, reprobe_now)`. Mirrors the decision in `Send-LargeFile`.
    pub fn decide(&self, n: usize) -> (bool, bool) {
        let have_r = self.cz_raw_sec > 0.0;
        let have_c = self.cz_cmp_write_sec > 0.0;
        let tr = if have_r { self.cz_raw_bytes as f64 / self.cz_raw_sec } else { 0.0 };
        // Pipeline bottleneck: deflate overlaps the socket write, so the effective
        // compressed time is the slower stage, not deflate + write.
        let cmp_sec = self.cz_cmp_deflate_sec.max(self.cz_cmp_write_sec);
        let tc = if cmp_sec > 0.0 { self.cz_cmp_bytes as f64 / cmp_sec } else { 0.0 };
        let incomp = self.cz_ratio > 0.0 && self.cz_ratio < 1.05;
        if n < 256 {
            return (false, false);
        }
        if !have_c {
            (true, false) // seed a compressed sample
        } else if !have_r {
            (false, false) // seed a raw sample
        } else {
            let decided = !incomp && tc >= 1.25 * tr;
            if self.cz_since >= REPROBE {
                (!decided, true) // refresh the other mode
            } else {
                (decided, false)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deflate_inflate_round_trip() {
        let data = b"the quick brown fox jumps over the lazy dog".repeat(100);
        let c = deflate_raw(&data);
        assert!(c.len() < data.len(), "should compress repetitive data");
        let d = inflate_raw(&c, data.len()).unwrap();
        assert_eq!(d, data);
    }

    #[test]
    fn decide_seeds_then_compares() {
        let mut st = AdaptiveState::new();
        // No samples yet -> seed compressed.
        assert_eq!(st.decide(BLOCK_SIZE), (true, false));
        // Have a compressed sample, no raw yet -> seed raw.
        st.cz_cmp_bytes = 1000;
        st.cz_cmp_deflate_sec = 0.0005;
        st.cz_cmp_write_sec = 0.001;
        assert_eq!(st.decide(BLOCK_SIZE), (false, false));
        // Both samples; compressed much faster -> compress.
        st.cz_raw_bytes = 1000;
        st.cz_raw_sec = 1.0; // raw is slow
        assert_eq!(st.decide(BLOCK_SIZE), (true, false));
        // Tiny block -> never compress.
        assert_eq!(st.decide(100), (false, false));
    }

    #[test]
    fn decide_reprobe_flips() {
        let mut st = AdaptiveState::new();
        st.cz_cmp_bytes = 1000;
        st.cz_cmp_write_sec = 0.001; // compressed fast
        st.cz_raw_bytes = 1000;
        st.cz_raw_sec = 1.0; // raw slow -> decided = compress
        st.cz_since = REPROBE; // time to re-probe
        let (do_comp, reprobe) = st.decide(BLOCK_SIZE);
        assert!(reprobe);
        assert!(!do_comp, "re-probe flips to the other (raw) mode");
    }

    #[test]
    fn decide_uses_pipeline_bottleneck_not_sum() {
        // deflate and write overlap, so effective compressed time is max(.,.),
        // not the sum. With these numbers the OLD sum model would NOT compress,
        // but the pipeline (max) model should.
        let mut st = AdaptiveState::new();
        st.cz_cmp_bytes = 1000;
        st.cz_cmp_deflate_sec = 0.001; // CPU
        st.cz_cmp_write_sec = 0.001; // network (overlaps the CPU)
        st.cz_raw_bytes = 1000;
        st.cz_raw_sec = 0.0015; // raw write throughput
        // sum model:  Tc = 1000 / 0.002  = 500_000  < 1.25 * (1000/0.0015 = 666_666)
        // max model:  Tc = 1000 / 0.001  = 1_000_000 >= 1.25 * 666_666 = 833_333
        let (do_comp, _) = st.decide(BLOCK_SIZE);
        assert!(do_comp, "pipeline (max) model should choose to compress here");
    }
}
