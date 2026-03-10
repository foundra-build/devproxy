use crate::proxy;
use anyhow::Result;

pub async fn run(port: u16) -> Result<()> {
    eprintln!("devproxy daemon starting...");
    proxy::run_daemon(port).await
}

pub fn restart() -> Result<()> {
    use colored::Colorize;
    match crate::platform::restart_daemon() {
        Ok(true) => {
            eprintln!("{} daemon restarted", "ok:".green());
            Ok(())
        }
        Ok(false) => {
            eprintln!(
                "{} no platform-managed daemon found. Run {} to set one up",
                "error:".red(),
                "devproxy init".bold()
            );
            std::process::exit(1);
        }
        Err(e) => Err(e),
    }
}
