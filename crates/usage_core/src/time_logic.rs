//! Pure display-time rules. Native code owns controls/GDI; parsing and weekly
//! arithmetic have one implementation here, with no clock or Windows dependency.

const MINUTES_PER_DAY: i32 = 24 * 60;
const MINUTES_PER_WEEK: i32 = 7 * MINUTES_PER_DAY;
const PEAK_RAMP_MINUTES: i32 = 60;
const MAX_MINUTE_TEXT_UNITS: usize = 31;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct UsageCorePeakSettings {
    pub enabled: i32,
    pub start_minute: i32,
    pub end_minute: i32,
    pub utc_offset_minutes: i32,
    pub weekdays_only: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UsageCorePeakPosition {
    pub inside: i32,
    pub minutes_until_start: i32,
    pub minutes_until_end: i32,
    pub reserved: i32,
    pub intensity: f64,
}

impl Default for UsageCorePeakPosition {
    fn default() -> Self {
        Self {
            inside: 0,
            minutes_until_start: -1,
            minutes_until_end: -1,
            reserved: 0,
            intensity: 0.0,
        }
    }
}

fn digits(text: &str) -> Option<i32> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

fn parse_minutes(text: &str) -> Option<i32> {
    let text = text.trim();
    let negative = text.starts_with('-');
    let text = text.strip_prefix(['-', '+']).unwrap_or(text);
    let (hours, minutes) = match text.split_once(':').or_else(|| text.split_once('.')) {
        Some((hours, minutes)) => (digits(hours)?, digits(minutes)?),
        None => {
            let value = digits(text)?;
            match text.len() {
                1 | 2 => (value, 0),
                3 | 4 => (value / 100, value % 100),
                _ => return None,
            }
        }
    };
    if !(0..24).contains(&hours) || !(0..60).contains(&minutes) {
        return None;
    }
    let magnitude = hours.checked_mul(60)?.checked_add(minutes)?;
    Some(if negative { -magnitude } else { magnitude })
}

fn format_utc_offset(minutes: i32) -> Option<String> {
    if !(-13 * 60..=14 * 60).contains(&minutes) {
        return None;
    }
    let sign = if minutes < 0 { '-' } else { '+' };
    let magnitude = minutes.unsigned_abs();
    Some(format!("{sign}{:02}:{:02}", magnitude / 60, magnitude % 60))
}

fn peak_position_at(unix_seconds: i64, settings: UsageCorePeakSettings) -> UsageCorePeakPosition {
    let mut result = UsageCorePeakPosition::default();
    if settings.enabled == 0
        || !(0..MINUTES_PER_DAY).contains(&settings.start_minute)
        || !(0..MINUTES_PER_DAY).contains(&settings.end_minute)
        || !(-13 * 60..=14 * 60).contains(&settings.utc_offset_minutes)
    {
        return result;
    }
    let length = (settings.end_minute - settings.start_minute).rem_euclid(MINUTES_PER_DAY);
    // Equal boundaries have always meant no peak window, not a 24-hour peak.
    if length == 0 {
        return result;
    }

    // The Unix epoch was Thursday; the public weekly coordinate starts Sunday.
    // Divide before adding the small offset so even i64 boundary clocks are safe.
    let now = (unix_seconds.div_euclid(60)
        + i64::from(settings.utc_offset_minutes)
        + 4 * i64::from(MINUTES_PER_DAY))
    .rem_euclid(i64::from(MINUTES_PER_WEEK)) as i32;
    let days = if settings.weekdays_only != 0 {
        1..6
    } else {
        0..7
    };
    let mut until_start = MINUTES_PER_WEEK;
    let mut until_end = MINUTES_PER_WEEK;
    for day in days {
        let start = day * MINUTES_PER_DAY + settings.start_minute;
        let end = start + length;
        if (now - start).rem_euclid(MINUTES_PER_WEEK) < length {
            result.inside = 1;
        }
        until_start = until_start.min((start - now).rem_euclid(MINUTES_PER_WEEK));
        until_end = until_end.min((end - now).rem_euclid(MINUTES_PER_WEEK));
    }
    result.minutes_until_start = until_start;
    result.minutes_until_end = until_end;
    result.intensity = if result.inside != 0 {
        1.0
    } else {
        let nearest = until_start.min(until_end);
        if nearest < PEAK_RAMP_MINUTES {
            1.0 - f64::from(nearest) / f64::from(PEAK_RAMP_MINUTES)
        } else {
            0.0
        }
    };
    result
}

/// Parses the native edit control's UTF-16 text. Invalid input preserves fallback.
/// `length` excludes the NUL terminator; at most 31 code units are accepted.
///
/// # Safety
/// A non-null `text` with a length in 1..=31 must point to that many readable u16s.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn usage_core_parse_minutes(
    text: *const u16,
    length: u32,
    fallback: i32,
) -> i32 {
    if text.is_null() || length == 0 || length as usize > MAX_MINUTE_TEXT_UNITS {
        return fallback;
    }
    // SAFETY: this is the caller-owned readable slice required by the ABI.
    let text = unsafe { std::slice::from_raw_parts(text, length as usize) };
    let mut ascii = [0_u8; MAX_MINUTE_TEXT_UNITS];
    for (out, unit) in ascii.iter_mut().zip(text) {
        let Ok(byte) = u8::try_from(*unit) else {
            return fallback;
        };
        if !byte.is_ascii() {
            return fallback;
        }
        *out = byte;
    }
    std::str::from_utf8(&ascii[..text.len()])
        .ok()
        .and_then(parse_minutes)
        .unwrap_or(fallback)
}

/// Writes a signed HH:MM UTC offset and a NUL, returning its length excluding NUL.
/// An invalid offset or short buffer returns zero and writes an empty string.
///
/// # Safety
/// A non-null `out` must point to `capacity` writable u16s.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn usage_core_format_utc_offset(
    minutes: i32,
    out: *mut u16,
    capacity: u32,
) -> u32 {
    if out.is_null() || capacity == 0 {
        return 0;
    }
    // SAFETY: capacity is positive and out is writable under the ABI contract.
    unsafe { out.write(0) };
    let Some(text) = format_utc_offset(minutes) else {
        return 0;
    };
    if text.len() >= capacity as usize {
        return 0;
    }
    for (index, byte) in text.bytes().enumerate() {
        // SAFETY: this ASCII string plus NUL fits in the caller's capacity.
        unsafe { out.add(index).write(u16::from(byte)) };
    }
    unsafe { out.add(text.len()).write(0) };
    text.len() as u32
}

/// Calculates the peak window for an explicit UTC time without reading a clock.
///
/// # Safety
/// Non-null settings/out must be valid readable/writable, aligned objects.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn usage_core_peak_position(
    unix_seconds: i64,
    settings: *const UsageCorePeakSettings,
    out: *mut UsageCorePeakPosition,
) -> i32 {
    if settings.is_null() || out.is_null() {
        return 0;
    }
    // SAFETY: valid caller-owned input and output as required by the ABI.
    unsafe { out.write(peak_position_at(unix_seconds, settings.read())) };
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUNDAY_UTC: i64 = 3 * 24 * 3600; // 1970-01-04, independent calendar anchor.

    fn settings(start: i32, end: i32, weekdays_only: bool) -> UsageCorePeakSettings {
        UsageCorePeakSettings {
            enabled: 1,
            start_minute: start,
            end_minute: end,
            utc_offset_minutes: 0,
            weekdays_only: i32::from(weekdays_only),
        }
    }

    #[test]
    fn edit_time_accepts_documented_formats_and_preserves_negative_zero_hour() {
        for (text, expected) in [
            ("9", 540),
            ("9:30", 570),
            ("09:00", 540),
            ("0930", 570),
            ("9.30", 570),
            ("-4:30", -270),
            ("-0:30", -30),
            ("-0030", -30),
            (" +00:45 ", 45),
            ("23:59", 1439),
            ("00:00", 0),
        ] {
            assert_eq!(parse_minutes(text), Some(expected), "{text}");
            let utf16: Vec<_> = text.encode_utf16().collect();
            assert_eq!(
                unsafe { usage_core_parse_minutes(utf16.as_ptr(), utf16.len() as u32, -777) },
                expected
            );
        }
    }

    #[test]
    fn edit_time_rejects_junk_invalid_minutes_and_overflow() {
        for text in [
            "",
            " ",
            "+",
            "9junk",
            "9:99",
            "09:60",
            "24:00",
            "2360",
            "9:",
            ":30",
            "--1",
            "9:-1",
            "1:2:3",
            "1.2.3",
            "9223372036854775807",
            "35791395",
            "-2147483648",
            "00000",
            "９:３０",
        ] {
            assert_eq!(parse_minutes(text), None, "{text}");
            let utf16: Vec<_> = text.encode_utf16().collect();
            assert_eq!(
                unsafe { usage_core_parse_minutes(utf16.as_ptr(), utf16.len() as u32, 123) },
                123,
                "{text}"
            );
        }
    }

    #[test]
    fn every_supported_utc_offset_round_trips_through_the_shipping_ffi() {
        for minutes in -13 * 60..=14 * 60 {
            let mut output = [0xaaaau16; 16];
            let length = unsafe { usage_core_format_utc_offset(minutes, output.as_mut_ptr(), 16) };
            assert_eq!(length, 6);
            assert_eq!(output[length as usize], 0);
            assert_eq!(output[length as usize + 1], 0xaaaa);
            assert_eq!(
                unsafe { usage_core_parse_minutes(output.as_ptr(), length, i32::MIN) },
                minutes
            );
        }
    }

    #[test]
    fn ffi_rejects_null_oversized_and_short_buffers_without_touching_canaries() {
        assert_eq!(
            unsafe { usage_core_parse_minutes(std::ptr::null(), 1, 42) },
            42
        );
        let single = [b'9' as u16];
        assert_eq!(
            unsafe { usage_core_parse_minutes(single.as_ptr(), u32::MAX, 42) },
            42
        );
        let mut guarded = [0xaaaau16; 4];
        assert_eq!(
            unsafe { usage_core_format_utc_offset(-30, guarded[1..].as_mut_ptr(), 1) },
            0
        );
        assert_eq!(guarded, [0xaaaa, 0, 0xaaaa, 0xaaaa]);
        assert_eq!(
            unsafe { usage_core_format_utc_offset(i32::MIN, guarded.as_mut_ptr(), 4) },
            0
        );
        let mut position = UsageCorePeakPosition::default();
        assert_eq!(
            unsafe { usage_core_peak_position(0, std::ptr::null(), &mut position) },
            0
        );
    }

    #[test]
    fn weekday_schedule_uses_the_real_epoch_day_and_exact_boundaries() {
        let weekday = settings(9 * 60, 13 * 60, true);
        for day in 0..7 {
            for (minute, in_window) in [
                (9 * 60 - 1, false),
                (9 * 60, true),
                (13 * 60 - 1, true),
                (13 * 60, false),
            ] {
                let result = peak_position_at(
                    SUNDAY_UTC + i64::from(day * MINUTES_PER_DAY + minute) * 60,
                    weekday,
                );
                assert_eq!(
                    result.inside != 0,
                    (1..=5).contains(&day) && in_window,
                    "day={day}, minute={minute}"
                );
            }
        }
    }

    #[test]
    fn daily_overnight_window_matches_every_minute_including_week_wrap() {
        let daily = settings(23 * 60, 60, false);
        for minute in 0..MINUTES_PER_WEEK {
            let result = peak_position_at(SUNDAY_UTC + i64::from(minute) * 60, daily);
            let minute_of_day = minute % MINUTES_PER_DAY;
            assert_eq!(
                result.inside != 0,
                !(60..23 * 60).contains(&minute_of_day),
                "minute={minute}"
            );
        }
    }

    #[test]
    fn weekday_overnight_tail_belongs_to_its_start_day() {
        let weekday = settings(23 * 60, 60, true);
        for (day, minute, expected) in
            [(6, 30, true), (0, 30, false), (1, 30, false), (2, 30, true)]
        {
            let result = peak_position_at(
                SUNDAY_UTC + i64::from(day * MINUTES_PER_DAY + minute) * 60,
                weekday,
            );
            assert_eq!(result.inside != 0, expected, "day={day}, minute={minute}");
        }
    }

    #[test]
    fn weekly_schedule_is_periodic_and_respects_offsets_without_overflow() {
        for offset in [-780, -30, 0, 45, 180, 840] {
            let mut configured = settings(9 * 60, 13 * 60, true);
            configured.utc_offset_minutes = offset;
            let utc = SUNDAY_UTC + 24 * 3600 + 10 * 3600 - i64::from(offset) * 60;
            let first = peak_position_at(utc, configured);
            assert_eq!(first.inside, 1);
            assert_eq!(first, peak_position_at(utc + 7 * 24 * 3600, configured));
            for boundary in [i64::MIN, -1, 0, i64::MAX] {
                let result = peak_position_at(boundary, configured);
                assert!((0..MINUTES_PER_WEEK).contains(&result.minutes_until_start));
                assert!((0..=1).contains(&result.inside));
            }
        }
        assert_eq!(
            peak_position_at(0, settings(60, 60, false)),
            UsageCorePeakPosition::default()
        );
        assert_eq!(
            peak_position_at(0, settings(-1, 60, false)),
            UsageCorePeakPosition::default()
        );
    }
}
