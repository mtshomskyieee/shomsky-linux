//! Minimal OpenAI-compatible chat client for quickyn.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::stub_reply_text;

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
            .or_else(|_| std::env::var("QUICKYN_LLM_URL"))
            .unwrap_or_else(|_| "http://127.0.0.1:1234".into());
        let model = std::env::var("ENGRAM_LLM_MODEL")
            .or_else(|_| std::env::var("QUICKYN_LLM_MODEL"))
            .unwrap_or_else(|_| "default".into());
        let stub = matches!(
            std::env::var("QUICKYN_STUB_LLM").as_deref(),
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
            let question = extract_question(messages);
            let file_body = extract_file_body(messages);
            return Ok(stub_reply_text(&question, file_body.as_deref()));
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
            .with_context(|| {
                format!("quickyn chat request to {url} (model={})", self.model)
            })?;
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

fn extract_question(messages: &[ChatMessage]) -> String {
    let user = messages
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .map(|m| m.content.as_str())
        .unwrap_or("");
    user.rsplit("Question:")
        .next()
        .unwrap_or(user)
        .trim()
        .to_string()
}

fn extract_file_body(messages: &[ChatMessage]) -> Option<String> {
    let user = messages
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .map(|m| m.content.as_str())?;
    if let Some(idx) = user.rfind("\n\nQuestion:") {
        let body = user[..idx].trim();
        if body.is_empty() {
            None
        } else {
            Some(body.to_string())
        }
    } else {
        None
    }
}
