use crate::config::{self, Config};
use crate::ipc::{self, Request, Response, RouteInfo};
use anyhow::{Result, bail};
use colored::Colorize;

/// Format a single route line, with optional `*` marker for current project.
fn format_route_line(route: &RouteInfo, current_slug: Option<&str>) -> String {
    let marker = match current_slug {
        Some(s) if s == route.slug => "* ",
        _ => "  ",
    };
    format!(
        "{}{:<40} {:<10}",
        marker,
        format!("https://{}", route.slug),
        route.port
    )
}

pub async fn run() -> Result<()> {
    let socket_path = Config::socket_path()?;
    let response = ipc::send_request(&socket_path, &Request::List).await?;

    // Try to read current project slug from cwd (silently ignore failures)
    let current_slug = std::env::current_dir()
        .ok()
        .and_then(|cwd| config::read_project_file(&cwd).ok())
        .and_then(|slug| {
            let config = Config::load().ok()?;
            Some(format!("{slug}.{}", config.domain))
        });

    match response {
        Response::Routes { routes } => {
            if routes.is_empty() {
                println!("no active projects");
            } else {
                println!("  {:<40} {:<10}", "URL".bold(), "PORT".bold());
                for route in &routes {
                    println!("{}", format_route_line(route, current_slug.as_deref()));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::RouteInfo;

    #[test]
    fn format_route_with_current_marker() {
        let route = RouteInfo {
            slug: "swift-penguin-devproxy.mysite.dev".to_string(),
            port: 51234,
        };
        let line = format_route_line(&route, Some("swift-penguin-devproxy.mysite.dev"));
        assert!(line.contains("*"), "current project should have * marker: {line}");
    }

    #[test]
    fn format_route_without_current_marker() {
        let route = RouteInfo {
            slug: "bold-fox-devproxy.mysite.dev".to_string(),
            port: 51235,
        };
        let line = format_route_line(&route, Some("swift-penguin-devproxy.mysite.dev"));
        assert!(!line.contains("*"), "non-current project should not have * marker: {line}");
    }

    #[test]
    fn format_route_no_current_project() {
        let route = RouteInfo {
            slug: "swift-penguin-devproxy.mysite.dev".to_string(),
            port: 51234,
        };
        let line = format_route_line(&route, None);
        assert!(!line.contains("*"), "no current project means no marker: {line}");
    }
}
