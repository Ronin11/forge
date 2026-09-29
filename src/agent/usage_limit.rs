//! Reading when a spent usage window reopens from the provider's own
//! error message: the time (and date) it names, in this machine's zone.

/// When a codex error message says the usage window is spent, the unix
/// time it can be tried again: the "try again at H:MM AM" it names, read
/// in this machine's local zone (the next such time after `now`), or an
/// hour from now when the message names no time it can read. Once the
/// message says the limit is hit it is a refusal whatever follows: this
/// never returns `None` for one (on 2026-09-23 "try again at Sep 24th,
/// 2026 3:24 AM" — a date ahead of the time — read as no refusal at all,
/// and task 570 was failed twice into a closed window). `None` only for
/// an error that is not a limit.
pub fn usage_limit_reset(msg: &str, now: i64) -> Option<i64> {
    let lower = msg.to_ascii_lowercase();
    if !(lower.contains("usage limit") || lower.contains("rate limit")) {
        return None;
    }
    let Some(i) = lower.find("try again at ") else {
        return Some(now + 3600);
    };
    let rest = &lower[i + "try again at ".len()..];
    let tokens: Vec<&str> = rest
        .split_whitespace()
        .map(|t| t.trim_end_matches(['.', ',']))
        .collect();
    // The time is the first "H:MM" token, its am/pm the next one; whatever
    // precedes it may be a date.
    let Some(ti) = tokens.iter().position(|t| t.contains(':')) else {
        return Some(now + 3600);
    };
    let Some((h, m)) = tokens[ti].split_once(':') else {
        return Some(now + 3600);
    };
    let (Ok(h), Ok(m)) = (h.parse::<i64>(), m.parse::<i64>()) else {
        return Some(now + 3600);
    };
    let h = match tokens.get(ti + 1).copied() {
        Some("am") => h % 12,
        Some("pm") => h % 12 + 12,
        _ => return Some(now + 3600),
    };
    if let Some((mon, mday, year)) = date_before(&tokens[..ti]) {
        // A dated reset is read as the instant it names. Past already:
        // retry in a minute. Beyond this window: the weekly limit, which
        // the five-hour sample cannot carry, so hold this window fully and
        // let the next launch be refused again if it is still closed.
        let at = local_time_on(now, mon, mday, year, h, m);
        return Some(if at <= now {
            now + 60
        } else {
            at.min(now + FIVE_HOURS)
        });
    }
    // The named time is when the five-hour window resets, so it can never
    // be more than five hours off. A refusal seen at the named minute
    // itself ("try again at 3:00 PM" at 3:00 PM) names a reset that has
    // just happened, not tomorrow's: retry in a minute. Anything past five
    // hours is the local-time reading having wrapped to the next day.
    let at = next_local_time(now, h, m);
    if at - now > FIVE_HOURS {
        return Some(now + 60);
    }
    Some(at)
}

/// A "<month> <day>[, <year>]" a codex message puts before the time
/// ("sep 24th, 2026", lower-cased and stripped of trailing punctuation
/// by the caller): month 0-11, day of month, and the year when named.
fn date_before(tokens: &[&str]) -> Option<(i32, i32, Option<i32>)> {
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let mut rest = tokens;
    let mut year = None;
    if let Some(last) = rest.last()
        && last.len() == 4
        && let Ok(y) = last.parse::<i32>()
    {
        year = Some(y);
        rest = &rest[..rest.len() - 1];
    }
    let [.., mon_tok, day_tok] = rest else {
        return None;
    };
    let mon = MONTHS.iter().position(|m| mon_tok.starts_with(m))? as i32;
    let digits: String = day_tok.chars().take_while(char::is_ascii_digit).collect();
    let mday: i32 = digits.parse().ok().filter(|d| (1..=31).contains(d))?;
    Some((mon, mday, year))
}

/// The unix time of local `hour:minute` on the given month and day (this
/// year unless `year` is named), from the zone `now` is read in.
fn local_time_on(now: i64, mon: i32, mday: i32, year: Option<i32>, hour: i64, minute: i64) -> i64 {
    // SAFETY: libc::localtime_r and mktime write only into the tm we own.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        let t = now as libc::time_t;
        libc::localtime_r(&t, &mut tm);
        tm.tm_mon = mon as libc::c_int;
        tm.tm_mday = mday as libc::c_int;
        if let Some(y) = year {
            tm.tm_year = (y - 1900) as libc::c_int;
        }
        tm.tm_hour = hour as libc::c_int;
        tm.tm_min = minute as libc::c_int;
        tm.tm_sec = 0;
        tm.tm_isdst = -1;
        libc::mktime(&mut tm) as i64
    }
}

/// The longest a five-hour window can be from resetting.
const FIVE_HOURS: i64 = 5 * 3600;

/// The next unix time at local `hour:minute` strictly after `now`.
fn next_local_time(now: i64, hour: i64, minute: i64) -> i64 {
    // SAFETY: libc::localtime_r and mktime write only into the tm we own.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        let t = now as libc::time_t;
        libc::localtime_r(&t, &mut tm);
        tm.tm_hour = hour as libc::c_int;
        tm.tm_min = minute as libc::c_int;
        tm.tm_sec = 0;
        let mut at = libc::mktime(&mut tm) as i64;
        if at <= now {
            tm.tm_mday += 1;
            at = libc::mktime(&mut tm) as i64;
        }
        at
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_codex_usage_limit_message_is_a_refusal_with_a_reset() {
        let now = crate::unix_now();
        // A time two hours ahead, in the local zone, as codex would name it.
        let (h, m) = unsafe {
            let mut tm: libc::tm = std::mem::zeroed();
            let t = (now + 2 * 3600) as libc::time_t;
            libc::localtime_r(&t, &mut tm);
            (tm.tm_hour as i64, tm.tm_min as i64)
        };
        let ampm = if h >= 12 { "PM" } else { "AM" };
        let h12 = if h % 12 == 0 { 12 } else { h % 12 };
        let msg = format!(
            "You've hit your usage limit. Upgrade to Pro, or try again at {h12}:{m:02} {ampm}."
        );
        let reset = usage_limit_reset(&msg, now).unwrap();
        assert!(
            reset > now && reset <= now + 2 * 3600,
            "the named time, within its window: {reset} vs {now}"
        );
        // The named time, read in the local zone.
        let secs_of_day = {
            // SAFETY: as in next_local_time.
            unsafe {
                let mut tm: libc::tm = std::mem::zeroed();
                let t = reset as libc::time_t;
                libc::localtime_r(&t, &mut tm);
                (tm.tm_hour as i64, tm.tm_min as i64)
            }
        };
        assert_eq!(secs_of_day, (h, m));
        // No time named: an hour's hold. Not a limit at all: nothing.
        assert_eq!(
            usage_limit_reset("usage limit reached", now),
            Some(now + 3600)
        );
        assert_eq!(usage_limit_reset("something else broke", now), None);
        assert_eq!(
            usage_limit_reset("rate limit exceeded, try again at 11:05 PM", 0).map(|r| r > 0),
            Some(true)
        );
    }

    /// The message task 570 was failed on (2026-09-23): a date ahead of the
    /// time. It is a refusal, read as the instant it names; a date beyond
    /// this window (the weekly limit) holds the window fully; a reading
    /// that cannot be parsed after "try again at" still holds an hour and
    /// is never `None`.
    #[test]
    fn a_dated_usage_limit_message_is_a_refusal_read_as_the_instant_it_names() {
        const MONTHS: [&str; 12] = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        let now = crate::unix_now();
        let named = |t: i64| -> String {
            // SAFETY: as in next_local_time.
            let tm = unsafe {
                let mut tm: libc::tm = std::mem::zeroed();
                let t = t as libc::time_t;
                libc::localtime_r(&t, &mut tm);
                tm
            };
            let (h, m) = (tm.tm_hour as i64, tm.tm_min as i64);
            let ampm = if h >= 12 { "PM" } else { "AM" };
            let h12 = if h % 12 == 0 { 12 } else { h % 12 };
            format!(
                "You’ve hit your usage limit. Upgrade to Pro (https://chatgpt.com/explore/pro), \
                 visit https://chatgpt.com/codex/settings/usage to purchase more credits or try \
                 again at {} {}th, {} {h12}:{m:02} {ampm}.",
                MONTHS[tm.tm_mon as usize],
                tm.tm_mday,
                tm.tm_year + 1900
            )
        };
        // Four hours ahead, dated: the named instant, to the minute.
        let target = now + 4 * 3600;
        let reset = usage_limit_reset(&named(target), now).unwrap();
        assert!((reset - target).abs() < 60, "{reset} vs {target}");
        // Three days ahead (the weekly window): this window's full hold.
        let reset = usage_limit_reset(&named(now + 3 * 86_400), now).unwrap();
        assert_eq!(reset, now + FIVE_HOURS);
        // An hour ago, dated: the reset has happened; retry in a minute.
        let reset = usage_limit_reset(&named(now - 3600), now).unwrap();
        assert_eq!(reset, now + 60);
        // Unreadable after the phrase: an hour, never nothing.
        assert_eq!(
            usage_limit_reset("usage limit hit, try again at some point", now),
            Some(now + 3600)
        );
        assert_eq!(
            usage_limit_reset("usage limit hit, try again at x:y zz", now),
            Some(now + 3600)
        );
    }

    /// The named time is the five-hour window's reset, so a refusal seen at
    /// that very minute (the reset just happened) means "retry now", and a
    /// reading that wraps to tomorrow is capped: on 2026-09-22 a refusal at
    /// 3:00 PM saying "try again at 3:00 PM" held every openai task for a
    /// day.
    #[test]
    fn a_reset_time_already_reached_or_a_day_away_never_holds_past_five_hours() {
        let now = crate::unix_now();
        let (h, m) = unsafe {
            let mut tm: libc::tm = std::mem::zeroed();
            let t = now as libc::time_t;
            libc::localtime_r(&t, &mut tm);
            (tm.tm_hour as i64, tm.tm_min as i64)
        };
        let ampm = if h >= 12 { "PM" } else { "AM" };
        let h12 = if h % 12 == 0 { 12 } else { h % 12 };
        let msg = format!("You've hit your usage limit. Try again at {h12}:{m:02} {ampm}.");
        let reset = usage_limit_reset(&msg, now).unwrap();
        assert!(reset > now && reset <= now + 60, "{reset} vs {now}");
        // One minute earlier than now: the same reading, wrapped to tomorrow.
        let m_prev = (m + 59) % 60;
        let h_prev = if m == 0 { (h + 23) % 24 } else { h };
        let ampm = if h_prev >= 12 { "PM" } else { "AM" };
        let h12 = if h_prev % 12 == 0 { 12 } else { h_prev % 12 };
        let msg = format!("usage limit; try again at {h12}:{m_prev:02} {ampm}");
        let reset = usage_limit_reset(&msg, now).unwrap();
        assert!(reset > now && reset <= now + 60, "{reset} vs {now}");
        assert!(
            usage_limit_reset("usage limit; try again at 3:37 AM", now).unwrap()
                <= now + FIVE_HOURS
        );
    }
}
