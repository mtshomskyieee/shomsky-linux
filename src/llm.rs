use anyhow::{anyhow, Context, Result};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{self, Write};

use crate::config::Config;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    pub fn assistant_text(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    pub fn assistant_tools(content: Option<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: "assistant".into(),
            content,
            tool_calls: Some(tool_calls),
            tool_call_id: None,
            name: None,
        }
    }

    pub fn tool_result(tool_call_id: impl Into<String>, name: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: "tool".into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
            name: Some(name.into()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone)]
pub struct ChatResponse {
    pub content: Option<String>,
    pub tool_calls: Vec<ToolCall>,
}

pub struct LlmClient {
    http: reqwest::Client,
    base_url: String,
    model: String,
}

impl LlmClient {
    pub fn new(cfg: &Config) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(900))
            .build()?;
        Ok(Self {
            http,
            base_url: cfg.llm_url.trim_end_matches('/').to_string(),
            model: cfg.llm_model.clone(),
        })
    }

    pub async fn health(&self) -> Result<()> {
        let url = format!("{}/v1/models", self.base_url);
        let resp = self.http.get(&url).send().await.context("health check")?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(anyhow!("LLM health check failed: {}", resp.status()))
        }
    }

    /// Chat completion. Prefer non-streaming for reliable tool-call parsing.
    pub async fn chat(
        &self,
        messages: &[ChatMessage],
        tools: &[Value],
        stream_text: bool,
    ) -> Result<ChatResponse> {
        if stream_text && tools.is_empty() {
            self.chat_stream_text(messages).await
        } else {
            self.chat_json(messages, tools).await
        }
    }

    async fn chat_json(&self, messages: &[ChatMessage], tools: &[Value]) -> Result<ChatResponse> {
        let url = format!("{}/v1/chat/completions", self.base_url);
        let mut body = json!({
            "model": self.model,
            "messages": messages,
            "stream": false,
            "temperature": 0.0,
            "max_tokens": 64,
        });
        if !tools.is_empty() {
            body["tools"] = Value::Array(tools.to_vec());
            body["tool_choice"] = json!("auto");
        }

        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .context("chat completions request")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("chat completions failed ({status}): {text}"));
        }

        let value: Value = resp.json().await.context("decoding chat response")?;
        let choice = value
            .pointer("/choices/0/message")
            .cloned()
            .ok_or_else(|| anyhow!("missing choices[0].message in response: {value}"))?;

        let content = choice
            .get("content")
            .and_then(|c| c.as_str())
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty());

        let tool_calls = choice
            .get("tool_calls")
            .and_then(|t| serde_json::from_value::<Vec<ToolCall>>(t.clone()).ok())
            .unwrap_or_default();

        Ok(ChatResponse {
            content,
            tool_calls,
        })
    }

    async fn chat_stream_text(&self, messages: &[ChatMessage]) -> Result<ChatResponse> {
        let url = format!("{}/v1/chat/completions", self.base_url);
        let body = json!({
            "model": self.model,
            "messages": messages,
            "stream": true,
            "temperature": 0.0,
            "max_tokens": 64,
        });

        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .context("streaming chat request")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("streaming chat failed ({status}): {text}"));
        }

        let mut stream = resp.bytes_stream();
        let mut buffer = String::new();
        let mut content = String::new();
        let stdout = io::stdout();
        let mut out = stdout.lock();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("reading stream chunk")?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));

            while let Some(pos) = buffer.find('\n') {
                let line = buffer[..pos].trim_end_matches('\r').to_string();
                buffer.drain(..=pos);
                if !line.starts_with("data:") {
                    continue;
                }
                let data = line[5..].trim();
                if data.is_empty() || data == "[DONE]" {
                    continue;
                }
                if let Ok(value) = serde_json::from_str::<Value>(data) {
                    if let Some(delta) = value
                        .pointer("/choices/0/delta/content")
                        .and_then(|c| c.as_str())
                    {
                        write!(out, "{delta}")?;
                        out.flush()?;
                        content.push_str(delta);
                    }
                }
            }
        }
        if !content.is_empty() {
            writeln!(out)?;
            out.flush()?;
        }

        Ok(ChatResponse {
            content: if content.is_empty() {
                None
            } else {
                Some(content)
            },
            tool_calls: vec![],
        })
    }
}
