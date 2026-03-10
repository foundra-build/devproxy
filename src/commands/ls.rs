use crate::config::{self, Config};
use crate::ipc::{self, Request, Response};
use anyhow::{Result, bail};
use colored::Colorize;

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
                // Compute column width dynamically based on longest URL
                let url_width = routes
                    .iter()
                    .map(|r| format!("https://{}", r.slug).len())
                    .max()
                    .unwrap_or(3)
                    .max(3); // at least "URL".len()

                println!(
                    "  {:<width$} {}",
                    "URL".bold(),
                    "PORT".bold(),
                    width = url_width + 2
                );
                for route in &routes {
                    let is_current = current_slug.as_deref() == Some(&route.slug);
                    let marker = if is_current { "* " } else { "  " };
                    let url = format!("https://{}", route.slug);
                    println!(
                        "{}{:<width$} {}",
                        marker,
                        url.cyan(),
                        route.port,
                        width = url_width + 2
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

#[cfg(test)]
mod tests {
    use crate::ipc::RouteInfo;

    /// Format a single route line, with optional `*` marker for current project.
    /// Used only in tests to verify marker logic independently of colored output.
    fn format_route_line(route: &RouteInfo, current_slug: Option<&str>) -> String {
        let marker = match current_slug {
            Some(s) if s == route.slug => "* ",
            _ => "  ",
        };
        let url = format!("https://{}", route.slug);
        format!("{}{} {}", marker, url, route.port)
    }

    #[test]
    fn format_route_with_current_marker() {
        let route = RouteInfo {
            slug: "swift-penguin-devproxy.mysite.dev".to_string(),
            port: 51234,
        };
        let line = format_route_line(&route, Some("swift-penguin-devproxy.mysite.dev"));
        assert!(
            line.starts_with("* "),
            "current project should have * marker: {line}"
        );
    }

    #[test]
    fn format_route_without_current_marker() {
        let route = RouteInfo {
            slug: "bold-fox-devproxy.mysite.dev".to_string(),
            port: 51235,
        };
        let line = format_route_line(&route, Some("swift-penguin-devproxy.mysite.dev"));
        assert!(
            line.starts_with("  "),
            "non-current project should not have * marker: {line}"
        );
    }

    #[test]
    fn format_route_no_current_project() {
        let route = RouteInfo {
            slug: "swift-penguin-devproxy.mysite.dev".to_string(),
            port: 51234,
        };
        let line = format_route_line(&route, None);
        assert!(
            line.starts_with("  "),
            "no current project means no marker: {line}"
        );
    }
}
