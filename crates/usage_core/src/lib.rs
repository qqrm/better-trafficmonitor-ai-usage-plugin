mod claude;
mod codex;
#[cfg(any(windows, test))]
mod codex_live;
mod history;
mod zcode;

use claude::{load_claude, load_claude_next_reset, newest_claude_cache};
use codex::load_codex;
use zcode::load_zcode;

pub const MAX_HISTORY_POINTS: usize = 128;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct UsageCoreHistoryPoint {
    pub timestamp_unix_seconds: i64,
    pub used_percentage: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct UsageCoreMetric {
    pub available: u8,
    pub _padding: [u8; 7],
    pub used_percentage: f64,
    pub reset_at_unix_seconds: i64,
    pub history_len: u32,
    pub _reserved: u32,
    pub history: [UsageCoreHistoryPoint; MAX_HISTORY_POINTS],
}

impl Default for UsageCoreMetric {
    fn default() -> Self {
        Self {
            available: 0,
            _padding: [0; 7],
            used_percentage: 0.0,
            reset_at_unix_seconds: 0,
            history_len: 0,
            _reserved: 0,
            history: [UsageCoreHistoryPoint::default(); MAX_HISTORY_POINTS],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct UsageCoreSnapshot {
    pub claude_5h: UsageCoreMetric,
    pub claude_7d: UsageCoreMetric,
    pub codex_7d: UsageCoreMetric,
    pub claude_next_reset_at_unix_seconds: i64,
    pub zcode_5h: UsageCoreMetric,
    pub zcode_7d: UsageCoreMetric,
    pub zcode_next_reset_at_unix_seconds: i64,
    pub zcode_5h_used_units: i64,
    pub zcode_5h_limit_units: i64,
    pub zcode_7d_used_units: i64,
    pub zcode_7d_limit_units: i64,
}

/// Writes a fixed-size snapshot into caller-owned memory. Claude is read from
/// local application data. On Windows, Codex reads authenticated usage over
/// HTTPS with rustls without launching a subprocess; its fresh local JSONL data is
/// used only if that request is unavailable. ZCode usage comes from the
/// latest z.ai quota response, kept locally between requests.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn usage_core_refresh(out: *mut UsageCoreSnapshot) -> i32 {
    if out.is_null() {
        return 0;
    }
    let (claude_5h, claude_7d) = load_claude();
    let claude_next_reset_at_unix_seconds = newest_claude_cache()
        .and_then(|path| path.parent().map(load_claude_next_reset))
        .unwrap_or_default();
    let codex_7d = load_codex();
    let zcode = load_zcode();
    // SAFETY: null was checked above; the ABI requires a writable snapshot.
    unsafe {
        *out = UsageCoreSnapshot {
            claude_5h,
            claude_7d,
            codex_7d,
            claude_next_reset_at_unix_seconds,
            zcode_5h: zcode.five_hour,
            zcode_7d: zcode.seven_day,
            zcode_next_reset_at_unix_seconds: zcode.next_reset_at,
            zcode_5h_used_units: zcode.five_hour_used_units,
            zcode_5h_limit_units: zcode.five_hour_limit_units,
            zcode_7d_used_units: zcode.seven_day_used_units,
            zcode_7d_limit_units: zcode.seven_day_limit_units,
        };
    }
    1
}

#[cfg(test)]
mod abi_tests {
    use super::*;
    use std::mem::{align_of, offset_of, size_of};
    #[test]
    fn c_snapshot_layout_stays_compatible() {
        assert_eq!(MAX_HISTORY_POINTS, 128);
        assert_eq!(size_of::<UsageCoreHistoryPoint>(), 16);
        assert_eq!(size_of::<UsageCoreMetric>(), 2080);
        assert_eq!(align_of::<UsageCoreMetric>(), 8);
        assert_eq!(offset_of!(UsageCoreMetric, history), 32);
        assert_eq!(
            offset_of!(UsageCoreSnapshot, claude_next_reset_at_unix_seconds),
            6240
        );
        assert_eq!(offset_of!(UsageCoreSnapshot, zcode_5h), 6248);
        assert_eq!(size_of::<UsageCoreSnapshot>(), 10448);
        assert_eq!(unsafe { usage_core_refresh(std::ptr::null_mut()) }, 0);
    }
}
