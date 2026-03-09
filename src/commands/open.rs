use crate::config::{self, Config};
use crate::ipc::{self, Request, Response};
use anyhow::{Context, Result, bail};
use colored::Colorize;

pub async fn run() -> Result<()> {
    let cwd = std::env::current_dir()?;

    // Read slug from .devproxy-project to identify THIS project
    let slug = config::read_project_file(&cwd)?;
    let config = Config::load()?;
    let full_host = format!("{slug}.{}", config.domain);

    // Verify the route is active by querying the daemon
    let socket_path = Config::socket_path()?;
    let response = ipc::send_request(&socket_path, &Request::List).await?;

    match response {
        Response::Routes { routes } => {
            let found = routes.iter().any(|r| r.slug == full_host);
            if !found {
                bail!(
                    "project '{slug}' is not currently routed by the daemon. \
                     Is the container running?"
                );
            }
            let url = format!("https://{full_host}");
            eprintln!("opening {}...", url.cyan());
            open::that(&url).context("could not open browser")?;
        }
        Response::Error { message } => bail!("daemon error: {message}"),
        _ => bail!("unexpected response from daemon"),
    }

    Ok(())
}
