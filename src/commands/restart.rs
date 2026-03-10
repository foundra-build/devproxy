use anyhow::Result;
use colored::Colorize;

pub fn run() -> Result<()> {
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
