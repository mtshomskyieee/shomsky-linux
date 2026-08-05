//! Runtime paths for shomagentd state.

use std::path::{Path, PathBuf};

pub const DEFAULT_RUN_DIR: &str = "/run/shomagent";
pub const DEFAULT_SYSTEM_CREW: &str = "/etc/shomsky/shomagent_crew.d";
pub const LOCAL_CREW_DIR: &str = "shomagent_crew.d";
pub const CTL_SOCK_NAME: &str = "ctl.sock";
pub const NOTIFY_NAME: &str = "notify";
pub const MEMBERS_DIR: &str = "members";

pub fn run_dir() -> PathBuf {
    std::env::var("SHOMAGENT_RUN_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_RUN_DIR))
}

pub fn ctl_sock() -> PathBuf {
    run_dir().join(CTL_SOCK_NAME)
}

pub fn notify_path() -> PathBuf {
    run_dir().join(NOTIFY_NAME)
}

pub fn members_dir() -> PathBuf {
    run_dir().join(MEMBERS_DIR)
}

pub fn system_crew_dir() -> PathBuf {
    std::env::var("SHOMAGENT_SYSTEM_CREW")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_SYSTEM_CREW))
}

pub fn ensure_run_dirs() -> std::io::Result<()> {
    let root = run_dir();
    std::fs::create_dir_all(&root)?;
    std::fs::create_dir_all(root.join(MEMBERS_DIR))?;
    Ok(())
}

pub fn local_crew_dir(cwd: &Path) -> PathBuf {
    cwd.join(LOCAL_CREW_DIR)
}
