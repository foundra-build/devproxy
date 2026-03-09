use crate::ipc::RouteInfo;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

#[derive(Debug, Clone)]
pub struct Route {
    pub host_port: u16,
}

/// Thread-safe route table mapping hostnames to upstream ports.
///
/// Lock methods use `expect("lock poisoned")` deliberately: a poisoned lock
/// means a thread panicked while holding it, leaving the route table in an
/// unknown state. In a single-binary daemon there is no meaningful recovery
/// from this, so panicking to crash the daemon (which will be restarted) is
/// the correct behavior.
#[derive(Debug, Clone)]
pub struct Router {
    routes: Arc<RwLock<HashMap<String, Route>>>,
    domain: String,
}

impl Router {
    pub fn new(domain: &str) -> Self {
        Self {
            routes: Arc::new(RwLock::new(HashMap::new())),
            domain: domain.to_string(),
        }
    }

    /// Insert a route: slug -> host_port. The full hostname is slug.domain.
    pub fn insert(&self, slug: &str, host_port: u16) {
        let hostname = format!("{slug}.{}", self.domain);
        let route = Route {
            host_port,
        };
        self.routes.write().expect("lock poisoned").insert(hostname, route);
    }

    /// Remove a route by slug
    pub fn remove(&self, slug: &str) {
        let hostname = format!("{slug}.{}", self.domain);
        self.routes.write().expect("lock poisoned").remove(&hostname);
    }

    /// Look up a host_port by full hostname (e.g., "swift-penguin.mysite.dev")
    pub fn get(&self, hostname: &str) -> Option<u16> {
        self.routes
            .read()
            .expect("lock poisoned")
            .get(hostname)
            .map(|r| r.host_port)
    }

    /// List all routes
    pub fn list(&self) -> Vec<RouteInfo> {
        self.routes
            .read()
            .expect("lock poisoned")
            .iter()
            .map(|(hostname, route)| RouteInfo {
                slug: hostname.clone(),
                port: route.host_port,
            })
            .collect()
    }

    #[allow(dead_code)]
    pub fn domain(&self) -> &str {
        &self.domain
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_get() {
        let router = Router::new("mysite.dev");
        router.insert("swift-penguin", 51234);
        assert_eq!(router.get("swift-penguin.mysite.dev"), Some(51234));
    }

    #[test]
    fn get_missing_returns_none() {
        let router = Router::new("mysite.dev");
        assert_eq!(router.get("nonexistent.mysite.dev"), None);
    }

    #[test]
    fn remove_route() {
        let router = Router::new("mysite.dev");
        router.insert("swift-penguin", 51234);
        router.remove("swift-penguin");
        assert_eq!(router.get("swift-penguin.mysite.dev"), None);
    }

    #[test]
    fn list_routes() {
        let router = Router::new("mysite.dev");
        router.insert("swift-penguin", 51234);
        router.insert("calm-otter", 51235);
        let routes = router.list();
        assert_eq!(routes.len(), 2);
    }
}
