//! Planned-run ledger at `$HOME/.loop/planned_runs.csv`.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedRun {
    pub id: u32,
    pub enabled: bool,
    pub frequency: String,
    pub cron: String,
    pub command: String,
    pub created_at: String,
}

/// `$SHOMLOOP_HOME` or `$HOME` or `/root`, then `.loop/planned_runs.csv`.
pub fn default_ledger_path() -> PathBuf {
    let home = std::env::var_os("SHOMLOOP_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        })
        .unwrap_or_else(|| PathBuf::from("/root"));
    home.join(".loop").join("planned_runs.csv")
}

pub fn list_runs() -> Result<Vec<PlannedRun>> {
    let mut runs = load(&default_ledger_path())?;
    runs.sort_by_key(|r| r.id);
    Ok(runs)
}

pub fn load(path: &Path) -> Result<Vec<PlannedRun>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let mut rdr =
        csv::Reader::from_path(path).with_context(|| format!("read {}", path.display()))?;
    let mut out = Vec::new();
    for rec in rdr.records() {
        let rec = rec.with_context(|| format!("parse {}", path.display()))?;
        let id = rec
            .get(0)
            .unwrap_or("")
            .trim()
            .parse::<u32>()
            .with_context(|| format!("bad id in {}", path.display()))?;
        let enabled = matches!(rec.get(1).unwrap_or("").trim(), "1" | "true" | "yes" | "on");
        out.push(PlannedRun {
            id,
            enabled,
            frequency: rec.get(2).unwrap_or("").to_string(),
            cron: rec.get(3).unwrap_or("").to_string(),
            command: rec.get(4).unwrap_or("").to_string(),
            created_at: rec.get(5).unwrap_or("").to_string(),
        });
    }
    Ok(out)
}

pub fn save(path: &Path, runs: &[PlannedRun]) -> Result<()> {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            fs::create_dir_all(dir).with_context(|| format!("mkdir {}", dir.display()))?;
        }
    }
    let tmp = path.with_extension("csv.tmp");
    {
        let mut wtr =
            csv::Writer::from_path(&tmp).with_context(|| format!("write {}", tmp.display()))?;
        wtr.write_record([
            "id",
            "enabled",
            "frequency",
            "cron",
            "command",
            "created_at",
        ])?;
        for run in runs {
            wtr.write_record([
                run.id.to_string(),
                if run.enabled { "1" } else { "0" }.to_string(),
                run.frequency.clone(),
                run.cron.clone(),
                run.command.clone(),
                run.created_at.clone(),
            ])?;
        }
        wtr.flush()?;
    }
    fs::rename(&tmp, path).with_context(|| format!("rename {}", path.display()))?;
    Ok(())
}

pub fn next_id(runs: &[PlannedRun]) -> u32 {
    runs.iter()
        .map(|r| r.id)
        .max()
        .unwrap_or(0)
        .saturating_add(1)
}

pub fn now_stamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format_utc(secs)
}

fn format_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let tod = secs % 86_400;
    let hour = tod / 3_600;
    let min = (tod % 3_600) / 60;
    let sec = tod % 60;
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}T{hour:02}:{min:02}:{sec:02}Z")
}

/// Days since 1970-01-01 → (year, month, day). Howard Hinnant.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    if m <= 2 {
        y += 1;
    }
    (y, m as u32, d as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_stamp() {
        assert_eq!(format_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_utc(86_400 + 3_600 + 60 + 1), "1970-01-02T01:01:01Z");
    }

    #[test]
    fn round_trip_quotes_and_commas() {
        let dir = std::env::temp_dir().join(format!("shomloop-csv-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("planned_runs.csv");
        let runs = vec![PlannedRun {
            id: 1,
            enabled: true,
            frequency: "daily at 5pm".into(),
            cron: "0 17 * * *".into(),
            command: "echo \"hi, there\"".into(),
            created_at: "1970-01-01T00:00:00Z".into(),
        }];
        save(&path, &runs).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded, runs);
        assert_eq!(next_id(&loaded), 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_is_empty() {
        let path = std::env::temp_dir().join(format!(
            "shomloop-missing-{}-{}.csv",
            std::process::id(),
            now_stamp()
        ));
        let _ = fs::remove_file(&path);
        assert!(load(&path).unwrap().is_empty());
    }
}
