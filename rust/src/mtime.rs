//! .NET `DateTime` ticks <-> `SystemTime`.
//!
//! The wire carries `LastWriteTimeUtc.Ticks`: 100-nanosecond intervals since
//! 0001-01-01T00:00:00Z. Change detection is `size && ticks`, so this conversion
//! MUST match .NET's integer truncation exactly or every re-sync re-copies
//! everything. See RUST-PORT-SPEC.md section 6.3.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Ticks per second (100 ns each).
const TICKS_PER_SEC: i64 = 10_000_000;
/// Nanoseconds per tick.
const NANOS_PER_TICK: i64 = 100;
/// Seconds between 0001-01-01 and the Unix epoch (1970-01-01).
const EPOCH_DIFF_SECS: i64 = 62_135_596_800;

/// Convert .NET ticks to a `SystemTime`.
pub fn ticks_to_systemtime(ticks: i64) -> SystemTime {
    let unix_secs = ticks / TICKS_PER_SEC - EPOCH_DIFF_SECS;
    let sub_ticks = ticks % TICKS_PER_SEC; // always >= 0 for real file times
    let nanos = (sub_ticks * NANOS_PER_TICK) as u32;
    if unix_secs >= 0 {
        UNIX_EPOCH + Duration::new(unix_secs as u64, nanos)
    } else {
        UNIX_EPOCH - Duration::new((-unix_secs) as u64, 0) + Duration::from_nanos(nanos as u64)
    }
}

/// Convert a `SystemTime` to .NET ticks (truncating sub-100 ns toward zero,
/// like .NET).
pub fn systemtime_to_ticks(t: SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => {
            (d.as_secs() as i64 + EPOCH_DIFF_SECS) * TICKS_PER_SEC
                + (d.subsec_nanos() as i64 / NANOS_PER_TICK)
        }
        Err(e) => {
            // Time before the Unix epoch (unusual for files).
            let d = e.duration();
            let total_nanos = d.as_secs() as i128 * 1_000_000_000 + d.subsec_nanos() as i128;
            let total_ticks = -(total_nanos / NANOS_PER_TICK as i128);
            (total_ticks - EPOCH_DIFF_SECS as i128 * TICKS_PER_SEC as i128) as i64
        }
    }
}

/// Read a file's `LastWriteTimeUtc` as .NET ticks.
pub fn read_ticks(meta: &std::fs::Metadata) -> std::io::Result<i64> {
    Ok(systemtime_to_ticks(meta.modified()?))
}

/// Set a file's modified time from .NET ticks (the receiver's `SetLastWriteTimeUtc`).
pub fn set_ticks(path: &Path, ticks: i64) -> std::io::Result<()> {
    let f = std::fs::File::options().write(true).open(path)?;
    f.set_modified(ticks_to_systemtime(ticks))
}

#[cfg(test)]
mod tests {
    use super::*;

    // .NET: new DateTime(2021, 1, 1, 0, 0, 0, DateTimeKind.Utc).Ticks
    const TICKS_2021: i64 = 637_450_560_000_000_000;

    #[test]
    fn known_value_matches_dotnet() {
        let st = ticks_to_systemtime(TICKS_2021);
        let unix = st.duration_since(UNIX_EPOCH).unwrap().as_secs();
        assert_eq!(unix, 1_609_459_200); // 2021-01-01T00:00:00Z
        assert_eq!(systemtime_to_ticks(st), TICKS_2021);
    }

    #[test]
    fn round_trip_with_subsecond() {
        // 2021-01-01 plus 1234500 ticks (0.12345 s).
        let ticks = TICKS_2021 + 1_234_500;
        let st = ticks_to_systemtime(ticks);
        assert_eq!(systemtime_to_ticks(st), ticks);
    }

    #[test]
    fn truncates_sub_tick_nanos() {
        // 150 ns past the epoch-difference boundary truncates to 1 tick (100 ns).
        let st = UNIX_EPOCH + Duration::new(0, 150);
        let ticks = systemtime_to_ticks(st);
        assert_eq!(ticks, EPOCH_DIFF_SECS * TICKS_PER_SEC + 1);
    }
}
