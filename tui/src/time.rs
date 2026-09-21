//! The one place the TUI turns Unix seconds into text: a short absolute form
//! (`2026-09-21 07:00`) in the terminal's local zone. The zone is whatever the
//! environment says (`TZ`, else the system's), read when a time is drawn, and
//! UTC when the environment names none or the offset cannot be found. Nothing
//! else in the TUI formats a time.

/// `2026-09-21 07:00` for `secs` in the local zone.
pub fn fmt_time(secs: i64) -> String {
    fmt_in(local_offset(secs).unwrap_or(0), secs)
}

/// `secs` shifted by a UTC offset in seconds, as `YYYY-MM-DD HH:MM`.
pub fn fmt_in(offset: i32, secs: i64) -> String {
    let local = secs.saturating_add(i64::from(offset));
    let (days, rem) = (local.div_euclid(86_400), local.rem_euclid(86_400));
    let (y, m, d) = civil(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}",
        rem / 3600,
        rem % 3600 / 60
    )
}

// `localtime_r` need not re-read `TZ` after its first call; `tzset` does.
unsafe extern "C" {
    fn tzset();
}

/// The local zone's offset from UTC, in seconds, at the instant `secs`;
/// `None` when the C library cannot say.
pub fn local_offset(secs: i64) -> Option<i32> {
    let t = libc::time_t::try_from(secs).ok()?;
    // SAFETY: `tm` is plain data that `localtime_r` fills; `tzset` re-reads
    // `TZ`, so a zone set in the environment is the one applied.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        tzset();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return None;
        }
        i32::try_from(tm.tm_gmtoff).ok()
    }
}

/// The proleptic Gregorian date of a day count since 1970-01-01.
fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_offset_moves_the_instant_across_the_day_and_the_year() {
        // 2026-09-21 14:13:20 UTC
        assert_eq!(fmt_in(0, 1_790_000_000), "2026-09-21 14:13");
        assert_eq!(fmt_in(-4 * 3600, 1_790_000_000), "2026-09-21 10:13");
        assert_eq!(fmt_in(5 * 3600 + 1800, 1_790_000_000), "2026-09-21 19:43");
        assert_eq!(fmt_in(12 * 3600, 1_790_000_000), "2026-09-22 02:13");
        assert_eq!(fmt_in(0, 0), "1970-01-01 00:00");
        assert_eq!(fmt_in(-3600, 0), "1969-12-31 23:00");
        assert_eq!(fmt_in(0, 951_782_400), "2000-02-29 00:00");
    }
}
