//! engram-cmdfreq — stdio-only MCP server (no listen sockets).
//!
//! Spawned as a child of engram-shell inside the Shomsky container.
//! Store path: ENGRAM_CMDFREQ_PATH or /engram/user/.command_freq.json

use anyhow::Result;
use engram_cmdfreq::default_store_path;
use engram_cmdfreq::mcp_stdio::run_stdio_server;

fn main() -> Result<()> {
    // Defense in depth: even if still in the shell's process group, do not die
    // on TTY interrupt — exit only when stdin closes (parent gone / shutdown).
    #[cfg(unix)]
    ignore_tty_signals();

    let path = default_store_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    run_stdio_server(path)
}

#[cfg(unix)]
fn ignore_tty_signals() {
    // libc is avoided as a dep; POSIX signal(2) is enough here.
    extern "C" {
        fn signal(signum: i32, handler: usize) -> usize;
    }
    const SIG_IGN: usize = 1;
    const SIGINT: i32 = 2;
    const SIGQUIT: i32 = 3;
    unsafe {
        signal(SIGINT, SIG_IGN);
        signal(SIGQUIT, SIG_IGN);
    }
}
