//! Web terminal (`:emerge`).
//!
//! The container TTY pauses. A browser at `http://localhost:1337` gets a PTY
//! whose child is this same `engram-shell` binary, resumed from a snapshot of
//! mode, working directory, conversation, and (in hallucination mode) the
//! virtual filesystem.

use anyhow::{anyhow, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, watch};
use tokio_tungstenite::tungstenite::Message;

use crate::llm::ChatMessage;

pub const DEFAULT_PORT: u16 = 1337;
const RING_MAX: usize = 256 * 1024;

static SIG_WRITE_FD: AtomicI32 = AtomicI32::new(-1);

extern "C" fn handle_sigint(_: libc::c_int) {
    let fd = SIG_WRITE_FD.load(Ordering::Relaxed);
    if fd >= 0 {
        let byte = [1u8];
        unsafe {
            libc::write(fd, byte.as_ptr() as *const libc::c_void, 1);
        }
    }
}

/// Conversation handoff written by the parent shell and read by the child.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub version: u32,
    pub mode: String,
    pub tool_style: String,
    pub cwd: PathBuf,
    pub history: Vec<ChatMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub halluc_files: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub halluc_session_path: Option<PathBuf>,
}

pub fn port_from_env() -> u16 {
    std::env::var("ENGRAM_EMERGE_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|p| *p != 0)
        .unwrap_or(DEFAULT_PORT)
}

pub fn snapshot_path() -> PathBuf {
    std::env::temp_dir().join(format!("engram-emerge-{}.json", std::process::id()))
}

pub fn write_snapshot(path: &Path, snap: &Snapshot) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string(snap).context("serializing emerge snapshot")?;
    std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

pub fn load_snapshot(path: &Path) -> Result<Snapshot> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading emerge snapshot {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

pub fn already_listening(port: u16) -> bool {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok()
}

pub fn resumed_shell_argv(snapshot: &Path) -> Result<Vec<String>> {
    let exe = std::env::current_exe().context("locating engram-shell")?;
    Ok(vec![
        exe.to_string_lossy().into_owned(),
        "--emerge-resume".into(),
        snapshot.display().to_string(),
    ])
}

pub struct ServeOpts {
    pub bind: std::net::SocketAddr,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
}

pub struct Running {
    /// Bound address. Unit tests read this; the interactive handoff prints the URL itself.
    #[allow(dead_code)]
    pub addr: std::net::SocketAddr,
    shutdown: watch::Sender<bool>,
    task: tokio::task::JoinHandle<Result<()>>,
}

impl Running {
    pub async fn shutdown(self) -> Result<()> {
        let _ = self.shutdown.send(true);
        match self.task.await {
            Ok(result) => result,
            Err(e) => Err(anyhow!("emerge server task: {e}")),
        }
    }
}

pub async fn start(opts: ServeOpts) -> Result<Running> {
    let listener = TcpListener::bind(opts.bind)
        .await
        .with_context(|| format!("binding {}", opts.bind))?;
    let addr = listener.local_addr().context("local addr")?;
    let (shutdown, rx) = watch::channel(false);
    let app = Arc::new(App {
        argv: opts.argv,
        cwd: opts.cwd,
        session: tokio::sync::Mutex::new(None),
    });
    let task = tokio::spawn(accept_loop(listener, app, rx));
    Ok(Running {
        addr,
        shutdown,
        task,
    })
}

/// Bind, print the URL, and block until Ctrl-C. Then stop the child.
pub async fn handoff(snapshot: &Path, cwd: &Path) -> Result<()> {
    let port = port_from_env();
    if already_listening(port) {
        println!("already emerged — open http://localhost:{port}");
        let _ = io::stdout().flush();
        return Ok(());
    }
    let opts = ServeOpts {
        bind: std::net::SocketAddr::from(([0, 0, 0, 0], port)),
        argv: resumed_shell_argv(snapshot)?,
        cwd: cwd.to_path_buf(),
    };
    let running = start(opts).await?;
    println!("emerged");
    println!("  open http://localhost:{port}");
    println!("  this prompt is paused — Ctrl-C returns here and stops the page");
    let _ = io::stdout().flush();
    wait_sigint().await;
    let result = running.shutdown().await;
    println!("emerge stopped");
    let _ = io::stdout().flush();
    result
}

fn page_html() -> &'static str {
    static PAGE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    PAGE.get_or_init(|| {
        const CSS: &str = include_str!("../assets/emerge/xterm.css");
        const JS: &str = include_str!("../assets/emerge/xterm.js");
        const FIT: &str = include_str!("../assets/emerge/xterm-addon-fit.js");
        const APP: &str = include_str!("../assets/emerge/app.js");
        format!(
            r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>emerge</title>
<style>
html, body, #term {{ height: 100%; margin: 0; background: #0d1117; }}
{css}
</style>
</head>
<body>
<div id="term"></div>
<script>{js}</script>
<script>{fit}</script>
<script>{app}</script>
</body>
</html>
"#,
            css = CSS,
            js = JS,
            fit = FIT,
            app = APP
        )
    })
    .as_str()
}

struct App {
    argv: Vec<String>,
    cwd: PathBuf,
    session: tokio::sync::Mutex<Option<Session>>,
}

#[derive(Clone)]
struct Share {
    master_fd: i32,
    alive: Arc<AtomicBool>,
    ring: Arc<Mutex<VecDeque<u8>>>,
    tx: broadcast::Sender<Vec<u8>>,
}

struct Session {
    master: Option<OwnedFd>,
    reader_fd: Option<OwnedFd>,
    reader_thread: Option<std::thread::JoinHandle<()>>,
    child: Option<Child>,
    share: Share,
}

impl Session {
    fn stop(&mut self) {
        self.share.alive.store(false, Ordering::SeqCst);
        self.reader_fd.take();
        if let Some(handle) = self.reader_thread.take() {
            let _ = handle.join();
        }
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.master.take();
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop();
    }
}

async fn accept_loop(
    listener: TcpListener,
    app: Arc<App>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            accepted = listener.accept() => {
                let (stream, _) = match accepted {
                    Ok(pair) => pair,
                    Err(_) => continue,
                };
                let app = Arc::clone(&app);
                tokio::spawn(async move {
                    let _ = handle_conn(stream, app).await;
                });
            }
        }
    }
    if let Some(mut session) = app.session.lock().await.take() {
        session.stop();
    }
    Ok(())
}

async fn handle_conn(mut stream: TcpStream, app: Arc<App>) -> Result<()> {
    let head = peek_request_line(&mut stream).await?;
    let path = request_path(&head);
    if path == "/ws" || path.starts_with("/ws?") {
        let ws = tokio_tungstenite::accept_async(stream)
            .await
            .context("websocket upgrade")?;
        handle_ws(ws, app).await?;
        return Ok(());
    }
    let _ = read_headers(&mut stream).await;
    if path == "/" {
        let body = page_html();
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n",
            body.len()
        );
        stream.write_all(header.as_bytes()).await?;
        stream.write_all(body.as_bytes()).await?;
    } else {
        let body = "not found\n";
        let header = format!(
            "HTTP/1.1 404 Not Found\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(header.as_bytes()).await?;
        stream.write_all(body.as_bytes()).await?;
    }
    Ok(())
}

async fn peek_request_line(stream: &TcpStream) -> Result<String> {
    let mut buf = [0u8; 2048];
    for _ in 0..50 {
        let n = stream.peek(&mut buf).await.context("peek request")?;
        if n == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            continue;
        }
        let head = String::from_utf8_lossy(&buf[..n]);
        if head.contains("\r\n") || head.contains('\n') {
            return Ok(head.into_owned());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Err(anyhow!("incomplete HTTP request"))
}

fn request_path(head: &str) -> &str {
    let line = head.lines().next().unwrap_or("");
    line.split_whitespace().nth(1).unwrap_or("/")
}

async fn read_headers(stream: &mut TcpStream) -> Result<()> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 1024];
    loop {
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 8192 {
            break;
        }
    }
    Ok(())
}

enum WsEvent {
    In(Option<std::result::Result<Message, tokio_tungstenite::tungstenite::Error>>),
    Out(std::result::Result<Vec<u8>, broadcast::error::RecvError>),
}

async fn handle_ws<S>(mut ws: tokio_tungstenite::WebSocketStream<S>, app: Arc<App>) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let share = ensure_session(&app).await?;
    let (replay, mut rx) = subscribe(&share);
    if !replay.is_empty() {
        ws.send(Message::Binary(replay)).await.ok();
    }
    let master_fd = share.master_fd;
    // One owner of the socket. Poll read and pty-output, then send only after
    // the read future is dropped so the two sides are not polled together.
    loop {
        let event = {
            let incoming = ws.next();
            tokio::pin!(incoming);
            tokio::select! {
                msg = incoming => WsEvent::In(msg),
                out = rx.recv() => WsEvent::Out(out),
            }
        };
        match event {
            WsEvent::In(Some(Ok(Message::Binary(data)))) => {
                if pty_write(master_fd, &data).is_err() {
                    break;
                }
            }
            WsEvent::In(Some(Ok(Message::Text(text)))) => apply_resize(master_fd, &text),
            WsEvent::In(Some(Ok(Message::Ping(data)))) => {
                if ws.send(Message::Pong(data)).await.is_err() {
                    break;
                }
            }
            WsEvent::In(Some(Ok(Message::Close(_))))
            | WsEvent::In(None)
            | WsEvent::In(Some(Err(_))) => break,
            WsEvent::In(Some(Ok(_))) => {}
            WsEvent::Out(Ok(bytes)) => {
                if ws.send(Message::Binary(bytes)).await.is_err() {
                    break;
                }
            }
            WsEvent::Out(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            WsEvent::Out(Err(_)) => break,
        }
    }
    Ok(())
}

fn subscribe(share: &Share) -> (Vec<u8>, broadcast::Receiver<Vec<u8>>) {
    let guard = share.ring.lock().unwrap_or_else(|e| e.into_inner());
    let replay: Vec<u8> = guard.iter().copied().collect();
    let rx = share.tx.subscribe();
    (replay, rx)
}

async fn ensure_session(app: &App) -> Result<Share> {
    let mut guard = app.session.lock().await;
    if let Some(session) = guard.as_mut() {
        if let Some(child) = session.child.as_mut() {
            if matches!(child.try_wait(), Ok(Some(_))) {
                session.share.alive.store(false, Ordering::SeqCst);
            }
        }
        if session.share.alive.load(Ordering::SeqCst) {
            return Ok(session.share.clone());
        }
    }
    if let Some(mut old) = guard.take() {
        old.stop();
    }
    let session = spawn_session(&app.argv, &app.cwd)?;
    let share = session.share.clone();
    *guard = Some(session);
    Ok(share)
}

fn spawn_session(argv: &[String], cwd: &Path) -> Result<Session> {
    if argv.is_empty() {
        return Err(anyhow!("emerge command is empty"));
    }
    let winsize = nix::pty::Winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let termios: Option<&nix::sys::termios::Termios> = None;
    let pair = nix::pty::openpty(Some(&winsize), termios).context("openpty")?;
    let slave_fd = pair.slave.as_raw_fd();
    let master_fd = pair.master.as_raw_fd();

    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    cmd.current_dir(cwd);
    cmd.env("TERM", "xterm-256color");
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::null());
    unsafe {
        cmd.pre_exec(move || {
            libc::close(master_fd);
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::ioctl(slave_fd, libc::TIOCSCTTY, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::dup2(slave_fd, 0) < 0
                || libc::dup2(slave_fd, 1) < 0
                || libc::dup2(slave_fd, 2) < 0
            {
                return Err(io::Error::last_os_error());
            }
            if slave_fd > 2 {
                libc::close(slave_fd);
            }
            Ok(())
        });
    }
    let child = cmd
        .spawn()
        .with_context(|| format!("spawning {}", argv[0]))?;
    drop(pair.slave);

    let reader_raw = nix::unistd::dup(pair.master.as_raw_fd()).context("dup pty master")?;
    let reader_fd = unsafe { OwnedFd::from_raw_fd(reader_raw) };
    let (tx, _rx) = broadcast::channel(256);
    let share = Share {
        master_fd: pair.master.as_raw_fd(),
        alive: Arc::new(AtomicBool::new(true)),
        ring: Arc::new(Mutex::new(VecDeque::new())),
        tx: tx.clone(),
    };
    let read_fd = reader_fd.as_raw_fd();
    let ring = Arc::clone(&share.ring);
    let alive = Arc::clone(&share.alive);
    let reader_thread = std::thread::Builder::new()
        .name("emerge-pty".into())
        .spawn(move || reader_loop(read_fd, ring, tx, alive))
        .context("spawn pty reader")?;

    Ok(Session {
        master: Some(pair.master),
        reader_fd: Some(reader_fd),
        reader_thread: Some(reader_thread),
        child: Some(child),
        share,
    })
}

fn reader_loop(
    fd: i32,
    ring: Arc<Mutex<VecDeque<u8>>>,
    tx: broadcast::Sender<Vec<u8>>,
    alive: Arc<AtomicBool>,
) {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags >= 0 {
        unsafe {
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }
    let mut buf = [0u8; 8192];
    while alive.load(Ordering::SeqCst) {
        let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            if err.kind() == io::ErrorKind::WouldBlock {
                let mut pfd = libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                };
                unsafe {
                    libc::poll(&mut pfd, 1, 200);
                }
                continue;
            }
            break;
        }
        if n == 0 {
            break;
        }
        let bytes = buf[..n as usize].to_vec();
        {
            let mut guard = ring.lock().unwrap_or_else(|e| e.into_inner());
            guard.extend(bytes.iter().copied());
            while guard.len() > RING_MAX {
                guard.pop_front();
            }
            let _ = tx.send(bytes);
        }
    }
    alive.store(false, Ordering::SeqCst);
}

fn pty_write(fd: i32, mut data: &[u8]) -> io::Result<()> {
    while !data.is_empty() {
        let n = unsafe { libc::write(fd, data.as_ptr() as *const libc::c_void, data.len()) };
        if n < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            if err.kind() == io::ErrorKind::WouldBlock {
                let mut pfd = libc::pollfd {
                    fd,
                    events: libc::POLLOUT,
                    revents: 0,
                };
                unsafe {
                    libc::poll(&mut pfd, 1, 200);
                }
                continue;
            }
            return Err(err);
        }
        data = &data[n as usize..];
    }
    Ok(())
}

fn apply_resize(fd: i32, text: &str) {
    #[derive(Deserialize)]
    struct Size {
        cols: u16,
        rows: u16,
    }
    let Ok(size) = serde_json::from_str::<Size>(text) else {
        return;
    };
    if size.cols == 0 || size.rows == 0 {
        return;
    }
    let ws = libc::winsize {
        ws_row: size.rows,
        ws_col: size.cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    unsafe {
        libc::ioctl(fd, libc::TIOCSWINSZ, &ws);
    }
}

async fn wait_sigint() {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let spawned = std::thread::Builder::new()
        .name("emerge-sigint".into())
        .spawn(move || {
            let _ = block_on_sigint();
            let _ = tx.send(());
        });
    if spawned.is_err() {
        return;
    }
    let _ = rx.await;
}

fn block_on_sigint() -> Result<()> {
    let mut fds = [0i32; 2];
    let rc = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
    if rc < 0 {
        return Err(anyhow!("pipe: {}", io::Error::last_os_error()));
    }
    let flags = unsafe { libc::fcntl(fds[1], libc::F_GETFL) };
    if flags >= 0 {
        unsafe {
            libc::fcntl(fds[1], libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }
    SIG_WRITE_FD.store(fds[1], Ordering::SeqCst);
    let previous = unsafe {
        nix::sys::signal::signal(
            nix::sys::signal::Signal::SIGINT,
            nix::sys::signal::SigHandler::Handler(handle_sigint),
        )
    };
    let previous = match previous {
        Ok(handler) => handler,
        Err(e) => {
            SIG_WRITE_FD.store(-1, Ordering::SeqCst);
            unsafe {
                libc::close(fds[0]);
                libc::close(fds[1]);
            }
            return Err(anyhow!("signal: {e}"));
        }
    };
    let mut byte = [0u8; 1];
    loop {
        let n = unsafe { libc::read(fds[0], byte.as_mut_ptr() as *mut libc::c_void, 1) };
        if n > 0 {
            break;
        }
        if n < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            break;
        }
    }
    SIG_WRITE_FD.store(-1, Ordering::SeqCst);
    unsafe {
        let _ = nix::sys::signal::signal(nix::sys::signal::Signal::SIGINT, previous);
        libc::close(fds[0]);
        libc::close(fds[1]);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;

    #[test]
    fn index_page_mounts_terminal() {
        let html = page_html();
        assert!(html.contains("id=\"term\""));
        assert!(html.contains("/ws"));
        assert!(html.contains("xterm"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pty_echo_round_trip() {
        let cat = if Path::new("/bin/cat").exists() {
            "/bin/cat"
        } else {
            "cat"
        };
        let running = start(ServeOpts {
            bind: "127.0.0.1:0".parse().unwrap(),
            argv: vec![cat.to_string()],
            cwd: std::env::temp_dir(),
        })
        .await
        .unwrap();
        let addr = running.addr;

        let body = tokio::time::timeout(
            Duration::from_secs(5),
            reqwest::get(format!("http://{addr}/")),
        )
        .await
        .expect("page timeout")
        .unwrap()
        .text()
        .await
        .unwrap();
        assert!(body.contains("id=\"term\""), "page missing terminal mount");

        let (ws, _) = tokio::time::timeout(
            Duration::from_secs(5),
            tokio_tungstenite::connect_async(format!("ws://{addr}/ws")),
        )
        .await
        .expect("ws timeout")
        .unwrap();
        let (mut write, mut read) = ws.split();
        // Read while sending. A single-task send waits on the echo and never
        // reaches the read, so the round-trip stalls.
        let recv = tokio::spawn(async move {
            let mut got = Vec::new();
            let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
            while tokio::time::Instant::now() < deadline {
                let left = deadline.saturating_duration_since(tokio::time::Instant::now());
                if left.is_zero() {
                    break;
                }
                match tokio::time::timeout(left, read.next()).await {
                    Ok(Some(Ok(Message::Binary(bytes)))) => got.extend(bytes),
                    Ok(Some(Ok(Message::Text(text)))) => got.extend(text.into_bytes()),
                    Ok(Some(Ok(_))) => {}
                    _ => break,
                }
                if String::from_utf8_lossy(&got).contains("hello-emerge") {
                    break;
                }
            }
            got
        });
        write
            .send(Message::Text(r#"{"cols":40,"rows":12}"#.into()))
            .await
            .unwrap();
        write
            .send(Message::Binary(b"hello-emerge\n".to_vec()))
            .await
            .unwrap();
        let got = recv.await.unwrap();
        let text = String::from_utf8_lossy(&got).into_owned();
        running.shutdown().await.unwrap();
        assert!(
            text.contains("hello-emerge"),
            "pty output missing echo: {text:?}"
        );
    }
}
