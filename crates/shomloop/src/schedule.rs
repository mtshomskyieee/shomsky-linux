//! Closed schedule phrases. Unknown wording is an error; nothing calls a model.

use anyhow::{bail, Result};

pub const FREQUENCY_HELP: &str = "\
accepted frequencies:
  hourly
  daily [at 5pm|5:30pm|17:00]
  weekly [on monday] [at 5pm]
  monthly [at 5pm]
  every N minutes
  every N hours
  a five-field cron expression (digits, *, /, -, comma)";

/// Compile a frequency phrase into a five-field cron expression.
pub fn parse_frequency(input: &str) -> Result<String> {
    let lower = input.trim().to_ascii_lowercase();
    let parts: Vec<&str> = lower.split_whitespace().collect();
    if parts.is_empty() {
        bail!("missing frequency\n{FREQUENCY_HELP}");
    }
    if let Some(cron) = raw_cron(&parts) {
        return Ok(cron);
    }
    let cron = match parts.as_slice() {
        ["hourly"] => "0 * * * *".to_string(),
        ["daily"] => "0 0 * * *".to_string(),
        ["weekly"] => "0 0 * * 0".to_string(),
        ["monthly"] => "0 0 1 * *".to_string(),
        ["daily", "at", time] => {
            let (min, hour) = parse_time(time)?;
            format!("{min} {hour} * * *")
        }
        ["weekly", "at", time] => {
            let (min, hour) = parse_time(time)?;
            format!("{min} {hour} * * 0")
        }
        ["weekly", "on", day] => {
            let d = parse_day(day)?;
            format!("0 0 * * {d}")
        }
        ["weekly", "on", day, "at", time] => {
            let d = parse_day(day)?;
            let (min, hour) = parse_time(time)?;
            format!("{min} {hour} * * {d}")
        }
        ["monthly", "at", time] => {
            let (min, hour) = parse_time(time)?;
            format!("{min} {hour} 1 * *")
        }
        ["every", n, unit] if unit == &"minute" || unit == &"minutes" => {
            let n = parse_step(n, 1, 59, "minutes")?;
            format!("*/{n} * * * *")
        }
        ["every", n, unit] if unit == &"hour" || unit == &"hours" => {
            let n = parse_step(n, 1, 23, "hours")?;
            format!("0 */{n} * * *")
        }
        _ => bail!("unknown frequency '{input}'\n{FREQUENCY_HELP}"),
    };
    Ok(cron)
}

fn raw_cron(parts: &[&str]) -> Option<String> {
    if parts.len() != 5 {
        return None;
    }
    let ok = parts.iter().all(|p| {
        !p.is_empty()
            && p.chars()
                .all(|c| c.is_ascii_digit() || matches!(c, '*' | '/' | '-' | ','))
    });
    if ok {
        Some(parts.join(" "))
    } else {
        None
    }
}

fn parse_step(raw: &str, min: u32, max: u32, unit: &str) -> Result<u32> {
    let n: u32 = raw
        .parse()
        .map_err(|_| anyhow::anyhow!("'{raw}' is not a number of {unit}"))?;
    if n < min || n > max {
        bail!("{unit} step must be {min}..={max}");
    }
    Ok(n)
}

/// Minute and hour. Accepts `5pm`, `5:30pm`, `17:00`.
fn parse_time(raw: &str) -> Result<(u32, u32)> {
    let (clock, ampm) = if let Some(rest) = raw.strip_suffix("am") {
        (rest, Some(false))
    } else if let Some(rest) = raw.strip_suffix("pm") {
        (rest, Some(true))
    } else {
        (raw, None)
    };
    let (hour_s, min_s) = if let Some((h, m)) = clock.split_once(':') {
        (h, m)
    } else if ampm.is_some() {
        (clock, "00")
    } else {
        bail!("time '{raw}' — use 5pm, 5:30pm, or 17:00");
    };
    if min_s.len() != 2 || hour_s.is_empty() {
        bail!("time '{raw}' — use 5pm, 5:30pm, or 17:00");
    }
    let mut hour: u32 = hour_s
        .parse()
        .map_err(|_| anyhow::anyhow!("time '{raw}' — use 5pm, 5:30pm, or 17:00"))?;
    let min: u32 = min_s
        .parse()
        .map_err(|_| anyhow::anyhow!("time '{raw}' — use 5pm, 5:30pm, or 17:00"))?;
    if min > 59 {
        bail!("minutes must be 00..=59");
    }
    match ampm {
        Some(pm) => {
            if !(1..=12).contains(&hour) {
                bail!("hour must be 1..=12 with am/pm");
            }
            if hour == 12 {
                hour = 0;
            }
            if pm {
                hour += 12;
            }
        }
        None => {
            if hour > 23 {
                bail!("hour must be 0..=23");
            }
        }
    }
    Ok((min, hour))
}

fn parse_day(raw: &str) -> Result<u32> {
    let day = match raw {
        "sunday" | "sun" => 0,
        "monday" | "mon" => 1,
        "tuesday" | "tue" | "tues" => 2,
        "wednesday" | "wed" => 3,
        "thursday" | "thu" | "thur" | "thurs" => 4,
        "friday" | "fri" => 5,
        "saturday" | "sat" => 6,
        _ => bail!("unknown day '{raw}' (sunday..saturday)"),
    };
    Ok(day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phrases() {
        assert_eq!(parse_frequency("hourly").unwrap(), "0 * * * *");
        assert_eq!(parse_frequency("daily").unwrap(), "0 0 * * *");
        assert_eq!(parse_frequency("Daily At 5PM").unwrap(), "0 17 * * *");
        assert_eq!(parse_frequency("daily at 5:30pm").unwrap(), "30 17 * * *");
        assert_eq!(parse_frequency("daily at 17:00").unwrap(), "0 17 * * *");
        assert_eq!(parse_frequency("daily at 12am").unwrap(), "0 0 * * *");
        assert_eq!(parse_frequency("daily at 12pm").unwrap(), "0 12 * * *");
        assert_eq!(parse_frequency("weekly").unwrap(), "0 0 * * 0");
        assert_eq!(parse_frequency("weekly on monday").unwrap(), "0 0 * * 1");
        assert_eq!(
            parse_frequency("weekly on monday at 5pm").unwrap(),
            "0 17 * * 1"
        );
        assert_eq!(parse_frequency("monthly").unwrap(), "0 0 1 * *");
        assert_eq!(parse_frequency("monthly at 5pm").unwrap(), "0 17 1 * *");
        assert_eq!(parse_frequency("every 15 minutes").unwrap(), "*/15 * * * *");
        assert_eq!(parse_frequency("every 2 hours").unwrap(), "0 */2 * * *");
        assert_eq!(parse_frequency("*/5 * * * *").unwrap(), "*/5 * * * *");
        assert_eq!(parse_frequency("0 17 * * 1-5").unwrap(), "0 17 * * 1-5");
    }

    #[test]
    fn rejects_unknown() {
        assert!(parse_frequency("yearly").is_err());
        assert!(parse_frequency("daily at noon").is_err());
        assert!(parse_frequency("every 0 minutes").is_err());
        assert!(parse_frequency("every 60 minutes").is_err());
        assert!(parse_frequency("every 24 hours").is_err());
        assert!(parse_frequency("").is_err());
    }
}
