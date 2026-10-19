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
    owner_only(&root, 0o700)?;
    owner_only(&root.join(MEMBERS_DIR), 0o700)?;
    Ok(())
}

/// Restrict `path` to the daemon uid. Mode is the low 9 bits (`0o700`, `0o600`).
pub fn owner_only(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

pub fn local_crew_dir(cwd: &Path) -> PathBuf {
    cwd.join(LOCAL_CREW_DIR)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn owner_only_clears_group_and_other() {
        let dir = std::env::temp_dir().join(format!("shomagent-mode-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).unwrap();
        owner_only(&dir, 0o700).unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
