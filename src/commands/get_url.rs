use crate::config::{self, Config};
use anyhow::Result;

/// Print the current project's proxy URL to stdout, or exit 1 if not running.
/// Designed for scripting: bare URL on stdout, no decoration.
pub fn run() -> Result<()> {
    let cwd = std::env::current_dir()?;

    let slug = match config::read_project_file(&cwd) {
        Ok(s) => s,
        Err(_) => std::process::exit(1),
    };

    let config = match Config::load() {
        Ok(c) => c,
        Err(_) => std::process::exit(1),
    };

    println!("https://{slug}.{}", config.domain);
    Ok(())
}
