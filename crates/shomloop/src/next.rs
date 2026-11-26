//! Next-fire lookup for five-field cron expressions (BusyBox-compatible).

use libc::{localtime_r, time_t, tm};

/// Seconds in a day.
const DAY: i64 = 86_400;
/// Walk at most one year of minutes looking for a match.
const MAX_MINUTES: i64 = 366 * 24 * 60;

/// Unix timestamp of the next minute that matches `cron`, strictly after
/// `after_unix`. `None` if the expression is invalid or no match within a year.
///
/// Uses the process local timezone (`localtime_r`), matching BusyBox `crond`.
/// When both day-of-month and day-of-week are restricted, a day matches if
/// either field matches (BusyBox / Vixie OR rule).
pub fn next_fire(cron: &str, after_unix: i64) -> Option<i64> {
    let fields = CronFields::parse(cron)?;
    // Start at the beginning of the next minute.
    let mut t = ((after_unix / 60) + 1) * 60;
    for _ in 0..MAX_MINUTES {
        if fields.matches(t) {
            return Some(t);
        }
        t += 60;
    }
    None
}

#[derive(Debug)]
struct CronFields {
    minute: Field,
    hour: Field,
    dom: Field,
    month: Field,
    dow: Field,
}

impl CronFields {
    fn parse(cron: &str) -> Option<Self> {
        let parts: Vec<&str> = cron.split_whitespace().collect();
        if parts.len() != 5 {
            return None;
        }
        Some(Self {
            minute: Field::parse(parts[0], 0, 59)?,
            hour: Field::parse(parts[1], 0, 23)?,
            dom: Field::parse(parts[2], 1, 31)?,
            month: Field::parse(parts[3], 1, 12)?,
            dow: Field::parse(parts[4], 0, 7)?, // 7 ≡ Sunday in some crons
        })
    }

    fn matches(&self, unix: i64) -> bool {
        let Some(c) = civil_local(unix) else {
            return false;
        };
        if !self.minute.matches(c.min) || !self.hour.matches(c.hour) || !self.month.matches(c.month)
        {
            return false;
        }
        let dom_star = self.dom.is_star;
        let dow_star = self.dow.is_star;
        // BusyBox: if both DOM and DOW are restricted, OR them.
        let day_ok = if !dom_star && !dow_star {
            self.dom.matches(c.dom) || self.dow.matches_dow(c.dow)
        } else {
            (dom_star || self.dom.matches(c.dom)) && (dow_star || self.dow.matches_dow(c.dow))
        };
        day_ok
    }
}

#[derive(Debug)]
struct Field {
    /// True when the field was a bare `*` (no step / list).
    is_star: bool,
    /// Allowed values in the field's range.
    allowed: [bool; 60],
}

impl Field {
    fn parse(raw: &str, min: u32, max: u32) -> Option<Self> {
        let mut allowed = [false; 60];
        let is_star = raw == "*";
        for piece in raw.split(',') {
            if piece.is_empty() {
                return None;
            }
            let (range, step) = match piece.split_once('/') {
                Some((r, s)) => (r, s.parse::<u32>().ok()?),
                None => (piece, 1u32),
            };
            if step == 0 {
                return None;
            }
            let (lo, hi) = if range == "*" {
                (min, max)
            } else if let Some((a, b)) = range.split_once('-') {
                let lo = a.parse::<u32>().ok()?;
                let hi = b.parse::<u32>().ok()?;
                if lo > hi || lo < min || hi > max {
                    return None;
                }
                (lo, hi)
            } else {
                let n = range.parse::<u32>().ok()?;
                if n < min || n > max {
                    // Allow dow 7 as Sunday alias when max is 7.
                    if max == 7 && n == 7 {
                        (0, 0)
                    } else {
                        return None;
                    }
                } else {
                    (n, n)
                }
            };
            let mut v = lo;
            while v <= hi {
                if (v as usize) < allowed.len() {
                    allowed[v as usize] = true;
                }
                v = v.saturating_add(step);
            }
        }
        Some(Self { is_star, allowed })
    }

    fn matches(&self, v: u32) -> bool {
        (v as usize) < self.allowed.len() && self.allowed[v as usize]
    }

    /// Day-of-week: treat 7 as Sunday (0).
    fn matches_dow(&self, v: u32) -> bool {
        let v = if v == 7 { 0 } else { v };
        self.matches(v) || (v == 0 && self.matches(7))
    }
}

#[derive(Debug, Clone, Copy)]
struct Civil {
    min: u32,
    hour: u32,
    dom: u32,
    month: u32,
    /// 0 = Sunday … 6 = Saturday (libc `tm_wday`).
    dow: u32,
}

fn civil_local(unix: i64) -> Option<Civil> {
    if unix < 0 || unix > time_t::MAX as i64 {
        return None;
    }
    let mut tt = unix as time_t;
    let mut out = unsafe { std::mem::zeroed::<tm>() };
    let ptr = unsafe { localtime_r(&mut tt, &mut out) };
    if ptr.is_null() {
        return None;
    }
    Some(Civil {
        min: out.tm_min as u32,
        hour: out.tm_hour as u32,
        dom: out.tm_mday as u32,
        month: (out.tm_mon + 1) as u32,
        dow: out.tm_wday as u32,
    })
}

/// Relative label for a fire time relative to `now` (e.g. `in 12m`, `in 2h`).
pub fn format_until(fire_unix: i64, now_unix: i64) -> String {
    let secs = (fire_unix - now_unix).max(0);
    let mins = (secs + 59) / 60; // round up to whole minutes
    if mins < 60 {
        return format!("in {mins}m");
    }
    let hours = mins / 60;
    let rem = mins % 60;
    if rem == 0 {
        format!("in {hours}h")
    } else if hours < 24 {
        format!("in {hours}h {rem}m")
    } else {
        let days = hours / 24;
        let h = hours % 24;
        if h == 0 {
            format!("in {days}d")
        } else {
            format!("in {days}d {h}h")
        }
    }
}

/// Horizon used by SINE's left-pane "soon" list.
pub const SOON_HORIZON_SECS: i64 = DAY;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: std::sync::OnceLock<Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    extern "C" {
        fn tzset();
    }

    fn with_utc<F: FnOnce()>(f: F) {
        let _g = env_lock().lock().unwrap();
        // SAFETY: tests hold the env lock; restore after.
        let prev = std::env::var_os("TZ");
        std::env::set_var("TZ", "UTC");
        unsafe {
            tzset();
        }
        f();
        match prev {
            Some(v) => std::env::set_var("TZ", v),
            None => std::env::remove_var("TZ"),
        }
        unsafe {
            tzset();
        }
    }

    #[test]
    fn daily_4am_from_3am() {
        with_utc(|| {
            // 1970-01-01 03:00:00 UTC
            let after = 3 * 3600;
            let fire = next_fire("0 4 * * *", after).unwrap();
            assert_eq!(fire, 4 * 3600);
        });
    }

    #[test]
    fn daily_4am_from_5am_rolls_to_tomorrow() {
        with_utc(|| {
            // 1970-01-01 05:00:00 UTC → next is 1970-01-02 04:00
            let after = 5 * 3600;
            let fire = next_fire("0 4 * * *", after).unwrap();
            assert_eq!(fire, DAY + 4 * 3600);
        });
    }

    #[test]
    fn every_15_minutes() {
        with_utc(|| {
            // 00:07 → 00:15
            let after = 7 * 60;
            let fire = next_fire("*/15 * * * *", after).unwrap();
            assert_eq!(fire, 15 * 60);
        });
    }

    #[test]
    fn weekly_sunday_midnight() {
        with_utc(|| {
            // 1970-01-01 was Thursday (dow=4). Next Sunday 00:00 is 1970-01-04.
            let after = 0;
            let fire = next_fire("0 0 * * 0", after).unwrap();
            assert_eq!(fire, 3 * DAY);
            let c = civil_local(fire).unwrap();
            assert_eq!(c.dow, 0);
            assert_eq!(c.hour, 0);
            assert_eq!(c.min, 0);
        });
    }

    #[test]
    fn both_dom_and_dow_or() {
        with_utc(|| {
            // 1st of month OR Sunday, at 00:00.
            // From 1970-01-01 (Thu): next Sunday Jan 4 wins before Feb 1.
            let after = 0;
            let fire = next_fire("0 0 1 * 0", after).unwrap();
            assert_eq!(fire, 3 * DAY);
        });
    }

    #[test]
    fn bad_cron_is_none() {
        assert!(next_fire("not cron", 0).is_none());
        assert!(next_fire("0 4 * *", 0).is_none());
        assert!(next_fire("60 4 * * *", 0).is_none());
    }

    #[test]
    fn format_until_coarse() {
        assert_eq!(format_until(100, 100), "in 0m");
        assert_eq!(format_until(100 + 12 * 60, 100), "in 12m");
        assert_eq!(format_until(100 + 2 * 3600, 100), "in 2h");
        assert_eq!(format_until(100 + 2 * 3600 + 10 * 60, 100), "in 2h 10m");
    }
}
