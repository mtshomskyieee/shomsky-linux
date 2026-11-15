//! Headless browser sessions for `shomagent web`.
//!
//! `SHOMAGENT_STUB_WEB=1` uses an in-memory page so CI does not need Chromium.
//! Otherwise Chromium is launched with CDP (`--remote-debugging-port`).

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::TcpListener;
use tokio::process::{Child, Command};
use tokio::time::sleep;
use tokio_tungstenite::{connect_async, tungstenite::Message};

/// Live page behind a web member.
pub enum WebSession {
    Stub(StubPage),
    Chromium(ChromiumSession),
}

pub struct StubPage {
    url: String,
    title: String,
    body: String,
    links: Vec<String>,
    /// (label, value)
    fields: Vec<(String, String)>,
}

pub struct ChromiumSession {
    /// DevTools HTTP port (`--remote-debugging-port`).
    port: u16,
    /// CDP page WebSocket for the active tab.
    ws_url: String,
    /// Prefer DevTools tabs whose URL matches this (host or full URL).
    want_url: Option<String>,
    user_data_dir: PathBuf,
    next_id: u64,
}

/// Formatted page text stored in the member ring and sent to ask/talk.
#[derive(Debug, Clone)]
pub struct PageSnapshot {
    pub url: String,
    pub title: String,
    pub body: String,
    pub links: Vec<String>,
    pub fields: Vec<(String, String)>,
}

impl PageSnapshot {
    pub fn format(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("URL: {}\n", self.url));
        out.push_str(&format!("Title: {}\n", self.title));
        out.push('\n');
        out.push_str(self.body.trim());
        out.push_str("\n\n");
        out.push_str("Links and buttons (chat> click N  or  click <text>):\n");
        if self.links.is_empty() {
            out.push_str("(none)\n");
        } else {
            for (i, l) in self.links.iter().enumerate() {
                out.push_str(&format!("{}. {l}\n", i + 1));
            }
        }
        out.push('\n');
        out.push_str("Fields (chat> type <label> -- <text>):\n");
        if self.fields.is_empty() {
            out.push_str("(none)\n");
        } else {
            for (i, (label, value)) in self.fields.iter().enumerate() {
                if value.is_empty() {
                    out.push_str(&format!("{}. {label} (empty)\n", i + 1));
                } else {
                    out.push_str(&format!("{}. {label} ({value})\n", i + 1));
                }
            }
        }
        out
    }
}

impl WebSession {
    /// Open `url`. Returns the session, first snapshot, and optional Chromium child
    /// (store the child on the member for pause/stop).
    pub async fn open(url: &str) -> Result<(Self, PageSnapshot, Option<Child>)> {
        validate_http_url(url)?;
        if stub_web_enabled() {
            let page = StubPage::example(url);
            let snap = page.snapshot();
            return Ok((Self::Stub(page), snap, None));
        }
        let (mut session, child) = ChromiumSession::launch().await?;
        session.navigate(url).await?;
        let snap = session.snapshot().await?;
        Ok((Self::Chromium(session), snap, Some(child)))
    }

    pub async fn refresh(&mut self) -> Result<PageSnapshot> {
        match self {
            Self::Stub(p) => Ok(p.snapshot()),
            Self::Chromium(c) => c.snapshot().await,
        }
    }

    pub async fn click(&mut self, text: &str) -> Result<PageSnapshot> {
        let text = text.trim();
        if text.is_empty() {
            bail!("click text is empty");
        }
        match self {
            Self::Stub(p) => {
                p.click(text)?;
                Ok(p.snapshot())
            }
            Self::Chromium(c) => {
                c.click(text).await?;
                c.wait_for_load().await?;
                let snap = c.snapshot().await?;
                c.want_url = Some(snap.url.clone());
                let _ = c.resync_ws().await;
                Ok(snap)
            }
        }
    }

    pub async fn goto(&mut self, url: &str) -> Result<PageSnapshot> {
        validate_http_url(url)?;
        match self {
            Self::Stub(p) => {
                *p = StubPage::example(url);
                Ok(p.snapshot())
            }
            Self::Chromium(c) => {
                c.navigate(url).await?;
                c.snapshot().await
            }
        }
    }

    pub async fn type_into(&mut self, label: &str, text: &str) -> Result<PageSnapshot> {
        let label = label.trim();
        if label.is_empty() {
            bail!("field label is empty");
        }
        match self {
            Self::Stub(p) => {
                p.type_into(label, text)?;
                Ok(p.snapshot())
            }
            Self::Chromium(c) => {
                c.type_into(label, text).await?;
                sleep(Duration::from_millis(100)).await;
                c.snapshot().await
            }
        }
    }

    pub fn cleanup_files(self) {
        if let Self::Chromium(c) = self {
            let _ = std::fs::remove_dir_all(&c.user_data_dir);
        }
    }
}

fn stub_web_enabled() -> bool {
    matches!(
        std::env::var("SHOMAGENT_STUB_WEB").as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

pub fn validate_http_url(url: &str) -> Result<()> {
    let url = url.trim();
    if url.is_empty() {
        bail!("url is empty");
    }
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        bail!("only http and https URLs are supported");
    }
    Ok(())
}

impl StubPage {
    fn example(url: &str) -> Self {
        Self {
            url: url.to_string(),
            title: "Example Domain".into(),
            body: "Example Domain\n\nThis domain is for use in illustrative examples in documents.\nYou may use this domain in literature without prior coordination or asking for permission."
                .into(),
            links: vec!["Learn more".into()],
            fields: vec![("Search".into(), String::new())],
        }
    }

    fn snapshot(&self) -> PageSnapshot {
        PageSnapshot {
            url: self.url.clone(),
            title: self.title.clone(),
            body: self.body.clone(),
            links: self.links.clone(),
            fields: self.fields.clone(),
        }
    }

    fn click(&mut self, text: &str) -> Result<()> {
        let idx = match_label(text, &self.links)?;
        let chosen = self.links[idx].clone();
        if chosen.eq_ignore_ascii_case("Learn more") {
            self.url = format!(
                "{}{}",
                self.url.trim_end_matches('/'),
                "/more"
            );
            self.title = "Learn More".into();
            self.body =
                "More information about Example Domain.\n\nStub page after clicking Learn more."
                    .into();
            self.links = vec!["Back to example".into()];
            self.fields = vec![("Search".into(), String::new())];
        } else if chosen.eq_ignore_ascii_case("Back to example") {
            let base = self
                .url
                .trim_end_matches('/')
                .trim_end_matches("/more")
                .to_string();
            *self = Self::example(&base);
        } else {
            self.body = format!("Clicked: {chosen}\n\n{}", self.body);
        }
        Ok(())
    }

    fn type_into(&mut self, label: &str, text: &str) -> Result<()> {
        let labels: Vec<String> = self.fields.iter().map(|(l, _)| l.clone()).collect();
        let idx = match_label(label, &labels)?;
        self.fields[idx].1 = text.to_string();
        Ok(())
    }
}

/// Exact match wins; otherwise a single case-insensitive substring match.
/// `N` or `#N` selects the 1-based index from the numbered Links/Fields list.
pub fn match_label(needle: &str, haystack: &[String]) -> Result<usize> {
    let needle = needle.trim();
    if needle.is_empty() {
        bail!("empty match text");
    }
    let index_str = needle.strip_prefix('#').unwrap_or(needle);
    if let Ok(n) = index_str.parse::<usize>() {
        if n >= 1 && n <= haystack.len() {
            return Ok(n - 1);
        }
        bail!(
            "no link/field #{n}; available 1..{}",
            haystack.len().max(0)
        );
    }
    let exact: Vec<usize> = haystack
        .iter()
        .enumerate()
        .filter(|(_, s)| s.eq_ignore_ascii_case(needle))
        .map(|(i, _)| i)
        .collect();
    if exact.len() == 1 {
        return Ok(exact[0]);
    }
    if exact.len() > 1 {
        bail!(
            "ambiguous match for {needle:?}; matches: {}",
            exact
                .iter()
                .map(|&i| format!("{:?}", haystack[i]))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let lower = needle.to_ascii_lowercase();
    let partial: Vec<usize> = haystack
        .iter()
        .enumerate()
        .filter(|(_, s)| s.to_ascii_lowercase().contains(&lower))
        .map(|(i, _)| i)
        .collect();
    match partial.len() {
        0 => bail!(
            "no match for {needle:?}; available: {}",
            if haystack.is_empty() {
                "(none)".into()
            } else {
                haystack
                    .iter()
                    .enumerate()
                    .map(|(i, s)| format!("{}. {s:?}", i + 1))
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        ),
        1 => Ok(partial[0]),
        _ => bail!(
            "ambiguous match for {needle:?}; matches: {}",
            partial
                .iter()
                .map(|&i| format!("{}. {:?}", i + 1, haystack[i]))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn find_chromium() -> Result<String> {
    for name in ["chromium-browser", "chromium", "google-chrome", "chrome"] {
        if which(name) {
            return Ok(name.into());
        }
    }
    bail!("chromium not found on PATH — install with: apk add chromium");
}

fn which(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|dir| {
                let p = dir.join(name);
                p.is_file()
            })
        })
        .unwrap_or(false)
}

async fn free_port() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .context("binding ephemeral port for Chromium CDP")?;
    Ok(listener.local_addr()?.port())
}

impl ChromiumSession {
    async fn launch() -> Result<(Self, Child)> {
        let bin = find_chromium()?;
        let port = free_port().await?;
        let user_data_dir = std::env::temp_dir().join(format!(
            "shomagent-web-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&user_data_dir)
            .with_context(|| format!("creating {}", user_data_dir.display()))?;

        let mut cmd = Command::new(&bin);
        cmd.arg("--headless=new")
            .arg("--disable-gpu")
            .arg("--disable-dev-shm-usage")
            .arg("--disable-extensions")
            .arg("--disable-component-extensions-with-background-pages")
            .arg("--disable-default-apps")
            .arg("--disable-blink-features=AutomationControlled")
            .arg("--no-first-run")
            .arg("--no-default-browser-check")
            // Realistic UA — some news sites serve an empty/error shell to bare headless.
            .arg("--user-agent=Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
            .arg(format!("--remote-debugging-port={port}"))
            .arg(format!("--user-data-dir={}", user_data_dir.display()))
            .arg("about:blank")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if unsafe { libc::geteuid() } == 0 {
            cmd.arg("--no-sandbox");
        }

        let mut child = cmd
            .spawn()
            .with_context(|| format!("spawning {bin} (apk add chromium)"))?;

        // Drain stderr so the pipe cannot fill; wait for DevTools.
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(_)) = lines.next_line().await {}
            });
        }
        if let Some(stdout) = child.stdout.take() {
            tokio::spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(_)) = lines.next_line().await {}
            });
        }

        let ws_url = wait_for_page_ws(port, PagePick::BlankOrHttp, None)
            .await
            .map_err(|e| {
                let _ = child.start_kill();
                e
            })?;

        Ok((
            Self {
                port,
                ws_url,
                want_url: None,
                user_data_dir,
                next_id: 1,
            },
            child,
        ))
    }

    /// Prefer a real http(s) tab matching `want_url`. Never demote to about:blank.
    async fn resync_ws(&mut self) -> Result<()> {
        self.ws_url =
            wait_for_page_ws(self.port, PagePick::PreferHttp, self.want_url.clone()).await?;
        Ok(())
    }

    async fn cdp(&mut self, method: &str, params: Value) -> Result<Value> {
        match self.cdp_once(method, params.clone()).await {
            Ok(v) => Ok(v),
            Err(e) => {
                // Navigations and tab swaps invalidate the old page websocket.
                self.resync_ws().await?;
                self.cdp_once(method, params)
                    .await
                    .with_context(|| format!("CDP {method} after resync (earlier: {e:#})"))
            }
        }
    }

    async fn cdp_once(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let (mut ws, _) = connect_async(&self.ws_url)
            .await
            .with_context(|| format!("connecting to CDP {}", self.ws_url))?;
        let msg = json!({
            "id": id,
            "method": method,
            "params": params,
        });
        ws.send(Message::Text(msg.to_string()))
            .await
            .context("sending CDP command")?;

        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                bail!("CDP timeout waiting for {method}");
            }
            let next = tokio::time::timeout(remaining, ws.next())
                .await
                .map_err(|_| anyhow!("CDP timeout waiting for {method}"))?
                .ok_or_else(|| anyhow!("CDP websocket closed"))?
                .context("reading CDP message")?;
            let text = match next {
                Message::Text(t) => t,
                Message::Ping(p) => {
                    let _ = ws.send(Message::Pong(p)).await;
                    continue;
                }
                Message::Close(_) => bail!("CDP websocket closed"),
                _ => continue,
            };
            let v: Value = serde_json::from_str(&text).context("decode CDP JSON")?;
            if v.get("id").and_then(|x| x.as_u64()) == Some(id) {
                if let Some(err) = v.get("error") {
                    bail!("CDP {method} error: {err}");
                }
                return Ok(v.get("result").cloned().unwrap_or(Value::Null));
            }
        }
    }

    async fn navigate(&mut self, url: &str) -> Result<()> {
        self.want_url = Some(url.to_string());
        // Prefer navigating the existing about:blank tab in place so the page
        // websocket stays valid. /json/new is a fallback when that fails.
        let navigated = async {
            self.cdp("Page.enable", json!({})).await?;
            self.cdp("Runtime.enable", json!({})).await?;
            self.cdp("Page.navigate", json!({ "url": url }))
                .await
                .context("Page.navigate")?;
            Ok::<(), anyhow::Error>(())
        }
        .await;

        if navigated.is_err() {
            let ws = open_url_tab(self.port, url).await?;
            self.ws_url = ws;
        }

        self.wait_for_load().await?;

        // Confirm we left about:blank; if not, force a new tab.
        let href = self.current_href().await.unwrap_or_default();
        if href.starts_with("about:") || href.is_empty() {
            let ws = open_url_tab(self.port, url)
                .await
                .context("open tab after blank navigate")?;
            self.ws_url = ws;
            self.wait_for_load().await?;
        }

        // Re-pin to the http(s) tab for `want_url` (never about:blank).
        self.resync_ws().await?;
        Ok(())
    }

    async fn current_href(&mut self) -> Result<String> {
        let v = self
            .cdp(
                "Runtime.evaluate",
                json!({
                    "expression": "location.href",
                    "returnByValue": true,
                }),
            )
            .await?;
        Ok(v.pointer("/result/value")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string())
    }

    async fn wait_for_load(&mut self) -> Result<()> {
        let _ = self.cdp_once("Page.enable", json!({})).await;
        let _ = self.cdp_once("Runtime.enable", json!({})).await;
        // Poll readyState; news sites need more than a fixed 500ms.
        // Use cdp_once so a blip does not PreferHttp-resync onto about:blank.
        for _ in 0..50 {
            let result = self
                .cdp_once(
                    "Runtime.evaluate",
                    json!({
                        "expression": "document.readyState",
                        "returnByValue": true,
                    }),
                )
                .await;
            if let Ok(v) = result {
                if v.pointer("/result/value")
                    .and_then(|x| x.as_str())
                    .is_some_and(|s| s == "complete" || s == "interactive")
                {
                    // Give JS-rendered shells (CNN, etc.) time to fill the body.
                    sleep(Duration::from_secs(2)).await;
                    return Ok(());
                }
            }
            sleep(Duration::from_millis(150)).await;
        }
        sleep(Duration::from_secs(2)).await;
        Ok(())
    }

    async fn snapshot(&mut self) -> Result<PageSnapshot> {
        // Stay on the current tab when possible; only resync if CDP is dead.
        if self.cdp_once("Runtime.enable", json!({})).await.is_err() {
            self.resync_ws().await?;
            self.cdp("Runtime.enable", json!({})).await?;
        }
        const SCRIPT: &str = r#"
(() => {
  const url = location.href;
  const title = document.title || '';
  const body = document.body ? (document.body.innerText || '') : '';
  const links = [];
  const seen = new Set();
  for (const el of document.querySelectorAll(
    'a, button, [role="button"], input[type="submit"], input[type="button"]'
  )) {
    const t = (el.innerText || el.value || el.getAttribute('aria-label') || '')
      .trim().replace(/\s+/g, ' ');
    if (!t || seen.has(t)) continue;
    seen.add(t);
    links.push(t);
  }
  const fields = [];
  for (const el of document.querySelectorAll(
    'input:not([type="submit"]):not([type="button"]):not([type="hidden"]):not([type="checkbox"]):not([type="radio"]):not([type="file"]), textarea, select'
  )) {
    let label = (el.getAttribute('aria-label') || el.placeholder || el.name || el.id || '').trim();
    if (el.id) {
      const lab = document.querySelector('label[for="' + CSS.escape(el.id) + '"]');
      if (lab) {
        const lt = (lab.innerText || '').trim().replace(/\s+/g, ' ');
        if (lt) label = lt;
      }
    }
    if (!label) label = 'field';
    const value = (el.value != null ? String(el.value) : '');
    fields.push({ label, value });
  }
  return { url, title, body, links, fields };
})()
"#;
        let result = self
            .cdp(
                "Runtime.evaluate",
                json!({
                    "expression": SCRIPT,
                    "returnByValue": true,
                    "awaitPromise": false,
                }),
            )
            .await?;
        let value = result
            .pointer("/result/value")
            .cloned()
            .ok_or_else(|| anyhow!("snapshot evaluate returned no value: {result}"))?;
        parse_snapshot_json(&value)
    }

    async fn click(&mut self, text: &str) -> Result<()> {
        let snap = self.snapshot().await?;
        let idx = match_label(text, &snap.links)?;
        // Always click by resolved label so `click 3` / `click #3` work.
        let label = &snap.links[idx];
        let script = format!(
            r#"
(() => {{
  const needle = {needle};
  const els = Array.from(document.querySelectorAll(
    'a, button, [role="button"], input[type="submit"], input[type="button"]'
  ));
  const labelOf = (el) => (el.innerText || el.value || el.getAttribute('aria-label') || '')
    .trim().replace(/\s+/g, ' ');
  let exact = els.filter(el => labelOf(el).toLowerCase() === needle.toLowerCase());
  let pick = exact;
  if (pick.length === 0) {{
    pick = els.filter(el => labelOf(el).toLowerCase().includes(needle.toLowerCase()));
  }}
  if (pick.length === 0) return {{ ok: false, error: 'no match' }};
  if (pick.length > 1) {{
    // Prefer exact when substring also hit several.
    if (exact.length === 1) pick = exact;
    else return {{ ok: false, error: 'ambiguous', labels: pick.map(labelOf) }};
  }}
  pick[0].click();
  return {{ ok: true }};
}})()
"#,
            needle = serde_json::to_string(label).unwrap()
        );
        let result = self
            .cdp(
                "Runtime.evaluate",
                json!({
                    "expression": script,
                    "returnByValue": true,
                }),
            )
            .await?;
        let value = result
            .pointer("/result/value")
            .cloned()
            .unwrap_or(Value::Null);
        if value.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            if let Some(labels) = value.get("labels").and_then(|v| v.as_array()) {
                let list: Vec<String> = labels
                    .iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect();
                bail!(
                    "ambiguous match for {text:?}; matches: {}",
                    list.iter()
                        .map(|s| format!("{s:?}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            bail!("click failed for {text:?}: {value}");
        }
        Ok(())
    }

    async fn type_into(&mut self, label: &str, text: &str) -> Result<()> {
        let snap = self.snapshot().await?;
        let labels: Vec<String> = snap.fields.iter().map(|(l, _)| l.clone()).collect();
        let _idx = match_label(label, &labels)?;
        let script = format!(
            r#"
(() => {{
  const needle = {needle};
  const value = {value};
  const els = Array.from(document.querySelectorAll(
    'input:not([type="submit"]):not([type="button"]):not([type="hidden"]):not([type="checkbox"]):not([type="radio"]):not([type="file"]), textarea, select'
  ));
  const labelOf = (el) => {{
    let label = (el.getAttribute('aria-label') || el.placeholder || el.name || el.id || '').trim();
    if (el.id) {{
      const lab = document.querySelector('label[for="' + CSS.escape(el.id) + '"]');
      if (lab) {{
        const lt = (lab.innerText || '').trim().replace(/\s+/g, ' ');
        if (lt) label = lt;
      }}
    }}
    return label || 'field';
  }};
  let exact = els.filter(el => labelOf(el).toLowerCase() === needle.toLowerCase());
  let pick = exact;
  if (pick.length === 0) {{
    pick = els.filter(el => labelOf(el).toLowerCase().includes(needle.toLowerCase()));
  }}
  if (pick.length === 0) return {{ ok: false, error: 'no match' }};
  if (pick.length > 1) {{
    if (exact.length === 1) pick = exact;
    else return {{ ok: false, error: 'ambiguous', labels: pick.map(labelOf) }};
  }}
  const el = pick[0];
  el.focus();
  el.value = value;
  el.dispatchEvent(new Event('input', {{ bubbles: true }}));
  el.dispatchEvent(new Event('change', {{ bubbles: true }}));
  return {{ ok: true }};
}})()
"#,
            needle = serde_json::to_string(label).unwrap(),
            value = serde_json::to_string(text).unwrap()
        );
        let result = self
            .cdp(
                "Runtime.evaluate",
                json!({
                    "expression": script,
                    "returnByValue": true,
                }),
            )
            .await?;
        let value = result
            .pointer("/result/value")
            .cloned()
            .unwrap_or(Value::Null);
        if value.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            if let Some(labels) = value.get("labels").and_then(|v| v.as_array()) {
                let list: Vec<String> = labels
                    .iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect();
                bail!(
                    "ambiguous field for {label:?}; matches: {}",
                    list.iter()
                        .map(|s| format!("{s:?}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            bail!("type failed for field {label:?}: {value}");
        }
        Ok(())
    }
}

fn parse_snapshot_json(value: &Value) -> Result<PageSnapshot> {
    let url = value
        .get("url")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let title = value
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let body = value
        .get("body")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let links = value
        .get("links")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let fields = value
        .get("fields")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| {
                    let label = v.get("label")?.as_str()?.to_string();
                    let value = v
                        .get("value")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    Some((label, value))
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(PageSnapshot {
        url,
        title,
        body,
        links,
        fields,
    })
}

#[derive(Clone, Copy)]
enum PagePick {
    /// Startup: about:blank or already-http tab.
    BlankOrHttp,
    /// After navigate: http(s) only — never fall back to about:blank.
    PreferHttp,
}

fn page_url(page: &Value) -> &str {
    page.get("url").and_then(|v| v.as_str()).unwrap_or("")
}

fn page_type(page: &Value) -> &str {
    page.get("type").and_then(|v| v.as_str()).unwrap_or("page")
}

fn page_ws(page: &Value) -> Option<&str> {
    page.get("webSocketDebuggerUrl").and_then(|v| v.as_str())
}

/// Reject extension / DevTools / worker targets — those were the HN bug.
fn is_usable_tab(page: &Value) -> bool {
    if page_ws(page).is_none() {
        return false;
    }
    let typ = page_type(page);
    if matches!(
        typ,
        "background_page" | "service_worker" | "worker" | "shared_worker" | "browser" | "iframe"
    ) {
        return false;
    }
    let url = page_url(page);
    if url.starts_with("chrome-extension://")
        || url.starts_with("chrome://")
        || url.starts_with("devtools://")
        || url.starts_with("chrome-error://")
    {
        return false;
    }
    typ == "page" || typ.is_empty()
}

fn is_http_tab(page: &Value) -> bool {
    let url = page_url(page);
    url.starts_with("http://") || url.starts_with("https://")
}

fn url_host(u: &str) -> String {
    u.trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or("")
        .to_string()
}

fn url_matches_want(page_url: &str, want: &str) -> bool {
    if page_url.starts_with(want) || want.starts_with(page_url.trim_end_matches('/')) {
        return true;
    }
    // Match by host so https://www.cnn.com/ matches want https://www.cnn.com
    let a = url_host(page_url);
    let b = url_host(want);
    !a.is_empty() && a.eq_ignore_ascii_case(&b)
}

fn pick_ws_from_list(pages: &[Value], prefer: PagePick, want: Option<&str>) -> Option<String> {
    let usable: Vec<&Value> = pages.iter().filter(|p| is_usable_tab(p)).collect();
    if usable.is_empty() {
        return None;
    }
    match prefer {
        PagePick::BlankOrHttp => usable
            .iter()
            .find(|p| {
                let u = page_url(p);
                u == "about:blank" || u.starts_with("http://") || u.starts_with("https://")
            })
            .or(usable.first())
            .and_then(|p| page_ws(p).map(|s| s.to_string())),
        PagePick::PreferHttp => {
            let http: Vec<&Value> = usable.iter().copied().filter(|p| is_http_tab(p)).collect();
            if http.is_empty() {
                return None;
            }
            if let Some(want) = want {
                if let Some(p) = http.iter().find(|p| url_matches_want(page_url(p), want)) {
                    return page_ws(p).map(|s| s.to_string());
                }
            }
            page_ws(http[0]).map(|s| s.to_string())
        }
    }
}

async fn list_targets(port: u16) -> Result<Vec<Value>> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()?;
    let list_url = format!("http://127.0.0.1:{port}/json/list");
    let resp = client
        .get(&list_url)
        .send()
        .await
        .with_context(|| format!("DevTools list {list_url}"))?;
    if !resp.status().is_success() {
        bail!("DevTools list status {}", resp.status());
    }
    let pages: Value = resp.json().await.context("decode DevTools list")?;
    Ok(pages.as_array().cloned().unwrap_or_default())
}

async fn wait_for_page_ws(port: u16, prefer: PagePick, want: Option<String>) -> Result<String> {
    let mut last_err = None;
    for _ in 0..50 {
        match list_targets(port).await {
            Ok(pages) => {
                if let Some(ws) = pick_ws_from_list(&pages, prefer, want.as_deref()) {
                    return Ok(ws);
                }
                last_err = Some(anyhow!(
                    "no usable page tab yet (filtered extension/devtools targets)"
                ));
            }
            Err(e) => {
                last_err = Some(e);
            }
        }
        sleep(Duration::from_millis(100)).await;
    }
    Err(last_err.unwrap_or_else(|| anyhow!("Chromium DevTools did not become ready")))
}

/// Create a tab pointed at `url` via DevTools `/json/new`. Returns its page WS.
async fn open_url_tab(port: u16, url: &str) -> Result<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?;
    // Chromium accepts PUT (and often GET) /json/new?<url>
    let new_url = format!(
        "http://127.0.0.1:{port}/json/new?{}",
        urlencoding_minimal(url)
    );
    let resp = match client.put(&new_url).send().await {
        Ok(r) if r.status().is_success() => r,
        _ => client
            .get(&new_url)
            .send()
            .await
            .context("DevTools /json/new")?,
    };
    if !resp.status().is_success() {
        bail!("DevTools /json/new status {}", resp.status());
    }
    let page: Value = resp.json().await.context("decode /json/new")?;
    if let Some(ws) = page_ws(&page) {
        if is_usable_tab(&page) || is_http_tab(&page) {
            return Ok(ws.to_string());
        }
    }
    // Some builds return the target id only; pick from the list.
    wait_for_page_ws(port, PagePick::PreferHttp, Some(url.to_string())).await
}

/// Encode a URL for a query string without pulling in the `urlencoding` crate.
fn urlencoding_minimal(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            b => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_exact_and_substring() {
        let labels = vec!["Learn more".into(), "Search".into()];
        assert_eq!(match_label("Learn more", &labels).unwrap(), 0);
        assert_eq!(match_label("learn", &labels).unwrap(), 0);
        assert_eq!(match_label("1", &labels).unwrap(), 0);
        assert_eq!(match_label("#2", &labels).unwrap(), 1);
        assert!(match_label("3", &labels).is_err());
        assert!(match_label("missing", &labels).is_err());
    }

    #[test]
    fn snapshot_format_numbers_links() {
        let p = StubPage::example("https://example.com");
        let text = p.snapshot().format();
        assert!(text.contains("1. Learn more"), "{text}");
        assert!(text.contains("click N"), "{text}");
    }

    #[test]
    fn stub_click_and_type() {
        let mut p = StubPage::example("https://example.com");
        p.type_into("Search", "hello").unwrap();
        assert_eq!(p.fields[0].1, "hello");
        p.click("Learn more").unwrap();
        assert!(p.body.contains("Stub page after clicking"));
        assert!(p.title.contains("Learn More"));
    }

    #[test]
    fn snapshot_format_lists_fields() {
        let p = StubPage::example("https://example.com");
        let text = p.snapshot().format();
        assert!(text.contains("URL: https://example.com"));
        assert!(text.contains("Learn more"));
        assert!(text.contains("Search (empty)"));
    }

    #[test]
    fn pick_ws_skips_extension_targets() {
        let pages = vec![
            json!({
                "type": "background_page",
                "url": "chrome-extension://nkeimhogjdpnpccoofpliimaahmaaome/background.html",
                "webSocketDebuggerUrl": "ws://127.0.0.1:1/ext"
            }),
            json!({
                "type": "page",
                "url": "https://news.ycombinator.com/",
                "webSocketDebuggerUrl": "ws://127.0.0.1:1/hn"
            }),
        ];
        assert_eq!(
            pick_ws_from_list(&pages, PagePick::PreferHttp, None).unwrap(),
            "ws://127.0.0.1:1/hn"
        );
        assert!(!is_usable_tab(&pages[0]));
        assert!(is_usable_tab(&pages[1]));
    }

    #[test]
    fn prefer_http_never_returns_blank() {
        let pages = vec![json!({
            "type": "page",
            "url": "about:blank",
            "webSocketDebuggerUrl": "ws://127.0.0.1:1/blank"
        })];
        assert!(pick_ws_from_list(&pages, PagePick::PreferHttp, None).is_none());
    }
}
