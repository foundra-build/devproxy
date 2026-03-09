use crate::proxy;
use anyhow::Result;

pub async fn run(port: u16) -> Result<()> {
    eprintln!("devproxy daemon starting...");
    proxy::run_daemon(port).await
}
