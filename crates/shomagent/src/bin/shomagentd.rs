//! shomagentd — background crew supervisor.

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    shomagent::daemon::run_daemon().await
}
