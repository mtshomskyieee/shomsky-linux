use anyhow::{Context, Result};
use std::path::Path;
use tokio::fs;

pub async fn read_file(path: &str) -> Result<String> {
    let p = Path::new(path);
    let data = fs::read(p)
        .await
        .with_context(|| format!("reading {}", p.display()))?;
    // Prefer UTF-8; fall back to lossy for binary-ish files.
    Ok(String::from_utf8_lossy(&data).into_owned())
}

pub async fn write_file(path: &str, content: &str) -> Result<String> {
    let p = Path::new(path);
    if let Some(parent) = p.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .await
                .with_context(|| format!("creating {}", parent.display()))?;
        }
    }
    fs::write(p, content)
        .await
        .with_context(|| format!("writing {}", p.display()))?;
    Ok(format!("wrote {} bytes to {}", content.len(), p.display()))
}

pub async fn list_dir(path: &str) -> Result<String> {
    let p = Path::new(path);
    let mut entries = fs::read_dir(p)
        .await
        .with_context(|| format!("listing {}", p.display()))?;
    let mut names = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        let meta = entry.metadata().await.ok();
        let suffix = match meta {
            Some(m) if m.is_dir() => "/",
            Some(m) if m.is_symlink() => "@",
            _ => "",
        };
        names.push(format!("{}{suffix}", entry.file_name().to_string_lossy()));
    }
    names.sort();
    if names.is_empty() {
        Ok("(empty)".into())
    } else {
        Ok(names.join("\n"))
    }
}
