use crate::config::Config;
use crate::ipc::{self, Request, Response};
use anyhow::{Result, bail};
use colored::Colorize;

pub async fn run() -> Result<()> {
    let socket_path = Config::socket_path()?;

    match ipc::send_request(&socket_path, &Request::Ping).await {
        Ok(Response::Pong) => {
            eprintln!("{} daemon is running", "ok:".green());

            // Also get route count
            if let Ok(Response::Routes { routes }) =
                ipc::send_request(&socket_path, &Request::List).await
            {
                eprintln!("  active routes: {}", routes.len());
            }
        }
        Ok(Response::Error { message }) => bail!("daemon error: {message}"),
        Ok(_) => bail!("unexpected response from daemon"),
        Err(e) => {
            eprintln!("{} daemon is not running: {e}", "error:".red());
            eprintln!("  run `devproxy init` to start it");
        }
    }

    Ok(())
}
