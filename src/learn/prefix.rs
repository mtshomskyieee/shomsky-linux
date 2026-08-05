/// Parse `, intent` or `?? intent`. Returns the intent string when matched.

pub fn parse_intent_prefix(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    if let Some(rest) = trimmed.strip_prefix("??") {
        let intent = rest.trim();
        if intent.is_empty() {
            return None;
        }
        return Some(intent);
    }
    if let Some(rest) = trimmed.strip_prefix(',') {
        // Allow `,intent` or `, intent` but not bare `,` alone.
        let intent = rest.trim();
        if intent.is_empty() {
            return None;
        }
        return Some(intent);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comma_prefix() {
        assert_eq!(
            parse_intent_prefix(", find big files"),
            Some("find big files")
        );
        assert_eq!(parse_intent_prefix(",find"), Some("find"));
        assert_eq!(parse_intent_prefix(","), None);
        assert_eq!(parse_intent_prefix("  , hello"), Some("hello"));
    }

    #[test]
    fn qq_prefix() {
        assert_eq!(parse_intent_prefix("?? list files"), Some("list files"));
        assert_eq!(parse_intent_prefix("??"), None);
        assert_eq!(parse_intent_prefix("? list"), None);
    }

    #[test]
    fn non_intent() {
        assert_eq!(parse_intent_prefix("ls -la"), None);
        assert_eq!(parse_intent_prefix(":help"), None);
    }
}
