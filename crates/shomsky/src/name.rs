use anyhow::{bail, Result};

/// Validate a learned command name: `[a-z][a-z0-9_-]*`.
pub fn validate_name(name: &str) -> Result<()> {
    let b = name.as_bytes();
    if b.is_empty() {
        bail!("empty command name");
    }
    if !(b[0].is_ascii_lowercase()) {
        bail!("name must start with a lowercase letter");
    }
    if !b
        .iter()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_' || *c == b'-')
    {
        bail!("name may only contain [a-z0-9_-]");
    }
    if name.len() > 64 {
        bail!("name too long (max 64)");
    }
    Ok(())
}

/// Suggest a slug from intent / command text.
pub fn suggest_slug(intent: &str, command: &str) -> String {
    let source = if !intent.trim().is_empty() {
        intent
    } else {
        command
    };
    let mut out = String::new();
    for ch in source.chars() {
        if out.len() >= 24 {
            break;
        }
        let c = ch.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if (c == ' ' || c == '-' || c == '_') && !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        return "learned-cmd".into();
    }
    if out.as_bytes()[0].is_ascii_digit() {
        return format!("cmd-{out}");
    }
    // Prefer a short memorable slug from first 2–3 tokens.
    let parts: Vec<&str> = out.split('-').filter(|p| !p.is_empty()).take(3).collect();
    let joined = parts.join("-");
    if validate_name(&joined).is_ok() {
        joined
    } else {
        "learned-cmd".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_names() {
        assert!(validate_name("recent-fat").is_ok());
        assert!(validate_name("a").is_ok());
        assert!(validate_name("Recent").is_err());
        assert!(validate_name("1abc").is_err());
        assert!(validate_name("has space").is_err());
    }

    #[test]
    fn suggests_from_intent() {
        let s = suggest_slug("find the biggest files this week", "find . -mtime -7");
        assert!(validate_name(&s).is_ok(), "{s}");
        assert!(!s.is_empty());
    }
}
