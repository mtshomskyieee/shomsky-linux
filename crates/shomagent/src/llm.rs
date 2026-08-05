//! Minimal OpenAI-compatible chat client for summarizer + talk.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".into(),
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".into(),
            content: content.into(),
        }
    }
}

#[derive(Clone)]
pub struct LlmClient {
    http: reqwest::Client,
    base_url: String,
    model: String,
    stub: bool,
}

impl LlmClient {
    pub fn from_env() -> Result<Self> {
        let base_url = std::env::var("ENGRAM_LLM_URL")
            .or_else(|_| std::env::var("SHOMAGENT_LLM_URL"))
            .unwrap_or_else(|_| "http://127.0.0.1:1234".into());
        let model = std::env::var("SHOMAGENT_LLM_MODEL")
            .or_else(|_| std::env::var("ENGRAM_LLM_MODEL"))
            .unwrap_or_else(|_| "default".into());
        let stub = matches!(
            std::env::var("SHOMAGENT_STUB_LLM").as_deref(),
            Ok("1") | Ok("true") | Ok("yes")
        );
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()?;
        Ok(Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            model,
            stub,
        })
    }

    pub fn is_stub(&self) -> bool {
        self.stub
    }

    pub async fn chat(&self, messages: &[ChatMessage], max_tokens: u32) -> Result<String> {
        if self.stub {
            return Ok(stub_reply(messages));
        }
        let url = format!("{}/v1/chat/completions", self.base_url);
        let body = json!({
            "model": self.model,
            "messages": messages,
            "stream": false,
            "temperature": 0.0,
            "max_tokens": max_tokens,
        });
        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .context("shomagent chat request")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("chat failed ({status}): {text}"));
        }
        let value: Value = resp.json().await.context("decode chat")?;
        value
            .pointer("/choices/0/message/content")
            .and_then(|c| c.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("empty LLM content: {value}"))
    }
}

fn stub_reply(messages: &[ChatMessage]) -> String {
    let last_user = messages
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .map(|m| m.content.as_str())
        .unwrap_or("");
    let system = messages
        .iter()
        .find(|m| m.role == "system")
        .map(|m| m.content.as_str())
        .unwrap_or("");

    if system.contains("finding JSON") || system.contains("structured finding") {
        // Heuristic: if log mentions 500 / error / CVE, emit a finding JSON.
        let lower = last_user.to_lowercase();
        if lower.contains("500")
            || lower.contains("error")
            || lower.contains("cve")
            || lower.contains("panic")
            || lower.contains("fail")
        {
            let title = if lower.contains("500") {
                "HTTP 500 detected in stream"
            } else if lower.contains("cve") {
                "CVE mention in stream"
            } else {
                "Error pattern in stream"
            };
            return format!(
                r#"{{"title":"{title}","detail":"stub summarizer matched log noise","suggestion":null,"none":false}}"#
            );
        }
        return r#"{"none":true}"#.into();
    }

    // Document ask/talk: pull matching lines from the Document: section.
    if let Some(doc) = system.split("Document:\n").nth(1) {
        let doc = doc
            .split("\n\nKeep answers")
            .next()
            .unwrap_or(doc)
            .trim();
        if !doc.is_empty() && !doc.starts_with("(document") && !doc.starts_with("(could not") {
            let q = last_user.to_lowercase();
            let keywords: Vec<&str> = q
                .split(|c: char| !c.is_alphanumeric())
                .filter(|w| w.len() > 2 && !["where", "what", "when", "who", "how", "the", "and", "for", "are", "is"].contains(w))
                .collect();
            let mut hits: Vec<&str> = doc
                .lines()
                .filter(|line| {
                    let l = line.to_lowercase();
                    !l.trim().is_empty()
                        && !l.starts_with('#')
                        && !l.contains("document attached via shomagent")
                        && keywords.iter().any(|k| l.contains(k))
                })
                .collect();
            if hits.is_empty() {
                // Fall back to first non-stub body lines so the user sees the doc was read.
                hits = doc
                    .lines()
                    .filter(|line| {
                        let l = line.trim();
                        !l.is_empty()
                            && !l.starts_with('#')
                            && !l.contains("document attached via shomagent")
                    })
                    .take(3)
                    .collect();
            }
            if !hits.is_empty() {
                return format!(
                    "from the document: {}",
                    hits.join(" | ")
                );
            }
            return "the document is empty or has no matching lines".into();
        }
    }

    // Process-stream talk stub
    format!(
        "stub: based on this member's stream only — {}",
        last_user.chars().take(120).collect::<String>()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_answers_from_document_lines() {
        let messages = vec![
            ChatMessage::system(
                "You are shomagent#1 (notes).\nBrief: x\n\nDocument:\nparis is in france\n\nKeep answers short.",
            ),
            ChatMessage::user("where is paris"),
        ];
        let reply = stub_reply(&messages);
        assert!(
            reply.contains("paris is in france"),
            "unexpected stub reply: {reply}"
        );
    }

    #[test]
    fn stub_finding_json_on_500() {
        let messages = vec![
            ChatMessage::system("Emit structured finding JSON."),
            ChatMessage::user("New log lines:\nERROR 500 boom"),
        ];
        let reply = stub_reply(&messages);
        assert!(reply.contains("\"none\":false") || reply.contains("HTTP 500"));
    }
}
