//! quickyn — one-shot yes/no decision from the local LLM.

mod llm;

pub use llm::{ChatMessage, LlmClient};

/// Files longer than this (Unicode scalar count) trigger a stderr warning.
pub const FILE_SIZE_WARN_CHARS: usize = 1000;

/// Exit codes: 0 = yes, 1 = no, 2 = error / unparseable.
pub const EXIT_YES: i32 = 0;
pub const EXIT_NO: i32 = 1;
pub const EXIT_ERROR: i32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Yes,
    No,
}

impl Decision {
    pub fn as_str(self) -> &'static str {
        match self {
            Decision::Yes => "yes",
            Decision::No => "no",
        }
    }

    pub fn exit_code(self) -> i32 {
        match self {
            Decision::Yes => EXIT_YES,
            Decision::No => EXIT_NO,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    pub decision: Decision,
    /// Confidence in \[0.0, 1.0\].
    pub confidence: f32,
}

impl Verdict {
    pub fn format_line(&self) -> String {
        format!("{} {:.2}", self.decision.as_str(), self.confidence.clamp(0.0, 1.0))
    }

    pub fn exit_code(&self) -> i32 {
        self.decision.exit_code()
    }
}

/// System prompt: require a strict two-line reply.
pub fn system_prompt() -> &'static str {
    "You are quickyn, a yes/no decision tool. Answer ONLY with exactly two lines and nothing else:\n\
     DECISION: yes\n\
     CONFIDENCE: 0.91\n\
     or\n\
     DECISION: no\n\
     CONFIDENCE: 0.42\n\
     DECISION must be yes or no. CONFIDENCE must be a number between 0 and 1.\n\
     Do not explain. Do not use markdown. Do not add extra lines."
}

/// Build the user message. Optional file body is pre-prompt context before the question.
pub fn build_user_message(question: &str, file_body: Option<&str>) -> String {
    match file_body {
        Some(body) if !body.is_empty() => {
            format!("{}\n\nQuestion: {}", body.trim_end(), question.trim())
        }
        _ => format!("Question: {}", question.trim()),
    }
}

/// True when the file body exceeds [`FILE_SIZE_WARN_CHARS`].
pub fn should_warn_file_size(body: &str) -> bool {
    body.chars().count() > FILE_SIZE_WARN_CHARS
}

/// Warning line for stderr when the file is large.
pub fn file_size_warning(path_display: &str, char_count: usize) -> String {
    format!(
        "quickyn: {path_display} is {char_count} characters; this may not be a quick decision"
    )
}

/// Parse a model reply into a [`Verdict`].
///
/// Accepts the strict `DECISION:` / `CONFIDENCE:` shape and a looser form
/// that finds `yes`/`no` plus a float somewhere in the text.
pub fn parse_verdict(raw: &str) -> Option<Verdict> {
    let text = raw.trim();
    if text.is_empty() {
        return None;
    }

    if let Some(v) = parse_strict(text) {
        return Some(v);
    }
    parse_loose(text)
}

fn parse_strict(text: &str) -> Option<Verdict> {
    let mut decision: Option<Decision> = None;
    let mut confidence: Option<f32> = None;

    for line in text.lines() {
        let line = line.trim();
        let lower = line.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("decision:") {
            decision = match rest.trim() {
                "yes" | "y" | "true" => Some(Decision::Yes),
                "no" | "n" | "false" => Some(Decision::No),
                _ => None,
            };
        } else if let Some(rest) = lower.strip_prefix("confidence:") {
            confidence = parse_confidence(rest.trim());
        }
    }

    match (decision, confidence) {
        (Some(d), Some(c)) => Some(Verdict {
            decision: d,
            confidence: c,
        }),
        (Some(d), None) => Some(Verdict {
            decision: d,
            confidence: 0.5,
        }),
        _ => None,
    }
}

fn parse_loose(text: &str) -> Option<Verdict> {
    let lower = text.to_ascii_lowercase();
    let decision = if word_boundary_contains(&lower, "yes") {
        Some(Decision::Yes)
    } else if word_boundary_contains(&lower, "no") {
        Some(Decision::No)
    } else {
        None
    }?;

    let confidence = extract_float(&lower).unwrap_or(0.5);
    Some(Verdict {
        decision,
        confidence: confidence.clamp(0.0, 1.0),
    })
}

fn word_boundary_contains(hay: &str, needle: &str) -> bool {
    for token in hay.split(|c: char| !c.is_ascii_alphanumeric()) {
        if token == needle {
            return true;
        }
    }
    false
}

fn extract_float(text: &str) -> Option<f32> {
    // Prefer an explicit confidence-looking number in 0..1 with a decimal.
    for token in text.split_whitespace() {
        let t = token.trim_matches(|c: char| !c.is_ascii_digit() && c != '.' && c != '-');
        if let Ok(v) = t.parse::<f32>() {
            if (0.0..=1.0).contains(&v) {
                return Some(v);
            }
        }
    }
    None
}

fn parse_confidence(s: &str) -> Option<f32> {
    let t = s
        .trim()
        .trim_matches(|c: char| !c.is_ascii_digit() && c != '.' && c != '-');
    t.parse::<f32>().ok().map(|v| v.clamp(0.0, 1.0))
}

/// Stub reply for CI (`QUICKYN_STUB_LLM=1`): yes unless question or file has `stub-no`.
pub fn stub_verdict(question: &str, file_body: Option<&str>) -> Verdict {
    let hit_no = question.contains("stub-no")
        || file_body.map(|b| b.contains("stub-no")).unwrap_or(false);
    if hit_no {
        Verdict {
            decision: Decision::No,
            confidence: 0.87,
        }
    } else {
        Verdict {
            decision: Decision::Yes,
            confidence: 0.91,
        }
    }
}

/// Format the stub as the model would (for the LLM client stub path).
pub fn stub_reply_text(question: &str, file_body: Option<&str>) -> String {
    let v = stub_verdict(question, file_body);
    format!(
        "DECISION: {}\nCONFIDENCE: {:.2}",
        v.decision.as_str(),
        v.confidence
    )
}

/// Failure from [`decide`]: LLM transport/HTTP, or an unparseable model reply.
#[derive(Debug)]
pub enum DecideError {
    Llm(anyhow::Error),
    Unparseable { raw: String },
}

impl std::fmt::Display for DecideError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecideError::Llm(e) => write!(f, "{e:#}"),
            DecideError::Unparseable { raw } => {
                write!(f, "could not parse decision from model reply:\n{raw}")
            }
        }
    }
}

impl std::error::Error for DecideError {}

/// Ask the configured local model a yes/no question (shared by CLI and shell TUI).
pub async fn decide(question: &str, file_body: Option<&str>) -> Result<Verdict, DecideError> {
    let question = question.trim();
    let messages = vec![
        ChatMessage::system(system_prompt()),
        ChatMessage::user(build_user_message(question, file_body)),
    ];
    let client = LlmClient::from_env().map_err(DecideError::Llm)?;
    let raw = client
        .chat(&messages, 64)
        .await
        .map_err(DecideError::Llm)?;
    parse_verdict(&raw).ok_or(DecideError::Unparseable { raw })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_message_question_only() {
        let m = build_user_message("is it ready?", None);
        assert_eq!(m, "Question: is it ready?");
    }

    #[test]
    fn user_message_file_before_question() {
        let m = build_user_message("ship it?", Some("status: green\n"));
        assert!(m.starts_with("status: green"));
        assert!(m.ends_with("Question: ship it?"));
        assert!(m.find("status: green").unwrap() < m.find("Question:").unwrap());
    }

    #[test]
    fn warn_at_1001_chars() {
        let small = "a".repeat(1000);
        assert!(!should_warn_file_size(&small));
        let large = "a".repeat(1001);
        assert!(should_warn_file_size(&large));
        let warning = file_size_warning("notes.md", 4821);
        assert!(warning.contains("4821"));
        assert!(warning.contains("may not be a quick decision"));
    }

    #[test]
    fn parse_strict_reply() {
        let v = parse_verdict("DECISION: yes\nCONFIDENCE: 0.91").unwrap();
        assert_eq!(v.decision, Decision::Yes);
        assert!((v.confidence - 0.91).abs() < 0.001);
        assert_eq!(v.format_line(), "yes 0.91");
        assert_eq!(v.exit_code(), EXIT_YES);

        let n = parse_verdict("DECISION: no\nCONFIDENCE: 0.42").unwrap();
        assert_eq!(n.decision, Decision::No);
        assert_eq!(n.exit_code(), EXIT_NO);
    }

    #[test]
    fn parse_messy_reply() {
        let v = parse_verdict("Sure, yes — confidence 0.75 roughly.").unwrap();
        assert_eq!(v.decision, Decision::Yes);
        assert!((v.confidence - 0.75).abs() < 0.001);
    }

    #[test]
    fn parse_empty_fails() {
        assert!(parse_verdict("").is_none());
        assert!(parse_verdict("I am not sure").is_none());
    }

    #[test]
    fn stub_yes_and_stub_no() {
        let y = stub_verdict("is paris in france?", None);
        assert_eq!(y.decision, Decision::Yes);
        assert_eq!(y.exit_code(), EXIT_YES);

        let n = stub_verdict("stub-no please", None);
        assert_eq!(n.decision, Decision::No);

        let n2 = stub_verdict("anything", Some("context stub-no here"));
        assert_eq!(n2.decision, Decision::No);
    }

    #[test]
    fn exit_status_mapping() {
        assert_eq!(Decision::Yes.exit_code(), 0);
        assert_eq!(Decision::No.exit_code(), 1);
        assert_eq!(EXIT_ERROR, 2);
    }

    #[tokio::test]
    async fn decide_stub_yes_and_no() {
        std::env::set_var("QUICKYN_STUB_LLM", "1");
        let y = decide("is paris in france?", None).await.unwrap();
        assert_eq!(y.decision, Decision::Yes);
        assert!((y.confidence - 0.91).abs() < 0.001);

        let n = decide("stub-no please", None).await.unwrap();
        assert_eq!(n.decision, Decision::No);
        assert!((n.confidence - 0.87).abs() < 0.001);

        let n2 = decide("anything", Some("context stub-no here"))
            .await
            .unwrap();
        assert_eq!(n2.decision, Decision::No);
    }
}
