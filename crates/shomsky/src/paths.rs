use std::path::{Path, PathBuf};

/// Layout under agent `$HOME` for learned commands.
#[derive(Debug, Clone)]
pub struct LearnPaths {
    pub home: PathBuf,
}

impl LearnPaths {
    pub fn new(home: impl Into<PathBuf>) -> Self {
        Self { home: home.into() }
    }

    /// Agent home: `ENGRAM_USER_DIR` or `/engram/user`.
    pub fn from_env() -> Self {
        let home = std::env::var_os("ENGRAM_USER_DIR")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("/engram/user"));
        Self::new(home)
    }

    pub fn bin_dir(&self) -> PathBuf {
        self.home.join("bin")
    }

    pub fn command_path(&self, name: &str) -> PathBuf {
        self.bin_dir().join(name)
    }

    pub fn man_dir(&self) -> PathBuf {
        self.home.join("share/man/man1")
    }

    pub fn man_path(&self, name: &str) -> PathBuf {
        self.man_dir().join(format!("{name}.1"))
    }

    pub fn tests_dir(&self) -> PathBuf {
        self.bin_dir().join("tests")
    }

    pub fn bats_path(&self, name: &str) -> PathBuf {
        self.tests_dir().join(format!("{name}.bats"))
    }

    pub fn completions_dir(&self) -> PathBuf {
        self.bin_dir().join("completions")
    }

    pub fn completion_path(&self, name: &str) -> PathBuf {
        self.completions_dir().join(name)
    }

    pub fn registry_dir(&self) -> PathBuf {
        self.bin_dir().join(".learn")
    }

    pub fn registry_path(&self, name: &str) -> PathBuf {
        self.registry_dir().join(format!("{name}.toml"))
    }

    pub fn packages_dir(&self) -> PathBuf {
        self.bin_dir().join("packages")
    }

    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        for d in [
            self.bin_dir(),
            self.man_dir(),
            self.tests_dir(),
            self.completions_dir(),
            self.registry_dir(),
            self.packages_dir(),
        ] {
            std::fs::create_dir_all(d)?;
        }
        Ok(())
    }

    /// Executables in `~/bin` (non-hidden, regular files).
    pub fn list_commands(&self) -> Vec<String> {
        let dir = self.bin_dir();
        let Ok(rd) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut names = Vec::new();
        for ent in rd.flatten() {
            let name = ent.file_name();
            let Some(s) = name.to_str() else { continue };
            if s.starts_with('.') {
                continue;
            }
            let path = ent.path();
            if path.is_file() {
                names.push(s.to_string());
            }
        }
        names.sort();
        names
    }

    pub fn home_path(&self) -> &Path {
        &self.home
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_under_home() {
        let p = LearnPaths::new("/tmp/learn-home");
        assert_eq!(p.command_path("foo"), PathBuf::from("/tmp/learn-home/bin/foo"));
        assert_eq!(
            p.man_path("foo"),
            PathBuf::from("/tmp/learn-home/share/man/man1/foo.1")
        );
        assert_eq!(
            p.registry_path("foo"),
            PathBuf::from("/tmp/learn-home/bin/.learn/foo.toml")
        );
    }
}
