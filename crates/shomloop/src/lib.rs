//! Schedule phrases, the planned-run ledger, and crontab install.
//!
//! The CSV under `~/.loop/planned_runs.csv` is the source of truth. Crontab
//! is a derived view rewritten on each change and again at container start.

mod cron;
mod run;
mod schedule;
mod store;

pub use cron::{
    ensure_crond, job_lines, log_dir, restore_notice, splice_block, sync_crontab, BEGIN_MARK,
    END_MARK,
};
pub use run::{format_logs, run_job};
pub use schedule::{parse_frequency, FREQUENCY_HELP};
pub use store::{default_ledger_path, list_runs, load, next_id, now_stamp, save, PlannedRun};
