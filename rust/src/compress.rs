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

/// Compress one block with LZ4 (raw block format; the caller stores the original
/// length out-of-band, like the `Z <clen> <rlen>` frame).
pub fn lz4_compress(data: &[u8]) -> Vec<u8> {
    lz4_flex::block::compress(data)
}

/// Decompress an LZ4 raw block into exactly `expected_len` bytes.
pub fn lz4_decompress(data: &[u8], expected_len: usize) -> std::io::Result<Vec<u8>> {
    lz4_flex::block::decompress(data, expected_len)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// Per-connection adaptive-compression measurements. Persist across files in a
/// single connection (`Send-LargeFile` in ft-server.ps1). See spec section 6.6.
#[derive(Default)]
pub struct AdaptiveState {
    /// Raw mode: original bytes sent and seconds taken (write time only).
    pub cz_raw_bytes: i64,
    pub cz_raw_sec: f64,
    /// Compressed mode: original bytes and seconds (deflate + write).
    pub cz_cmp_bytes: i64,
    pub cz_cmp_sec: f64,
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
        let have_c = self.cz_cmp_sec > 0.0;
        let tr = if have_r { self.cz_raw_bytes as f64 / self.cz_raw_sec } else { 0.0 };
        let tc = if have_c { self.cz_cmp_bytes as f64 / self.cz_cmp_sec } else { 0.0 };
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
    fn lz4_round_trip() {
        let data = b"the quick brown fox jumps over the lazy dog ".repeat(500);
        let c = lz4_compress(&data);
        assert!(c.len() < data.len(), "lz4 should compress repetitive data");
        let d = lz4_decompress(&c, data.len()).unwrap();
        assert_eq!(d, data);
        // empty + tiny round-trip (edge cases the bundle path will hit)
        for s in [&b""[..], &b"x"[..]] {
            let c = lz4_compress(s);
            assert_eq!(lz4_decompress(&c, s.len()).unwrap(), s);
        }
    }

    #[test]
    fn decide_seeds_then_compares() {
        let mut st = AdaptiveState::new();
        // No samples yet -> seed compressed.
        assert_eq!(st.decide(BLOCK_SIZE), (true, false));
        // Have a compressed sample, no raw yet -> seed raw.
        st.cz_cmp_bytes = 1000;
        st.cz_cmp_sec = 0.001;
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
        st.cz_cmp_sec = 0.001; // compressed fast
        st.cz_raw_bytes = 1000;
        st.cz_raw_sec = 1.0; // raw slow -> decided = compress
        st.cz_since = REPROBE; // time to re-probe
        let (do_comp, reprobe) = st.decide(BLOCK_SIZE);
        assert!(reprobe);
        assert!(!do_comp, "re-probe flips to the other (raw) mode");
    }
}
