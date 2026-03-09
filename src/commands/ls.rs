use crate::config::Config;
use crate::ipc::{self, Request, Response};
use anyhow::{Result, bail};
use colored::Colorize;

pub async fn run() -> Result<()> {
    let socket_path = Config::socket_path()?;
    let response = ipc::send_request(&socket_path, &Request::List).await?;

    match response {
        Response::Routes { routes } => {
            if routes.is_empty() {
                println!("no active projects");
            } else {
                println!("{:<30} {:<10}", "SLUG".bold(), "PORT".bold());
                for route in &routes {
                    println!(
                        "{:<30} {:<10}",
                        format!("https://{}", route.slug).cyan(),
                        route.port
                    );
                }
                println!();
                println!("{} active project(s)", routes.len());
            }
        }
        Response::Error { message } => bail!("daemon error: {message}"),
        _ => bail!("unexpected response from daemon"),
    }

    Ok(())
}
