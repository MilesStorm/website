//! Rate limits on password login and registration: per visitor (Cloudflare's `CF-Connecting-IP`,
//! as the Gateway's own limits use) and per username, so neither one address nor many addresses
//! can try passwords for an account without end. Over a limit, the API answers 429
//! `rate_limited` before auth is asked.
//!
//! The counts are kept in each replica's memory, like the Gateway's per-pod limits: a restart
//! forgets them, and with N replicas the real ceiling is up to N times the number here.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use api_types::{ApiError, ErrorCode};
use axum::http::HeaderMap;

/// Fixed windows: at most `max` hits per key in each `window`.
pub(crate) struct Limit {
    max: u32,
    window: Duration,
    hits: Mutex<HashMap<String, (Instant, u32)>>,
}

/// Windows tracked before expired ones are swept out.
const SWEEP_AT: usize = 10_000;

impl Limit {
    pub(crate) fn new(max: u32, window: Duration) -> Self {
        Self { max, window, hits: Mutex::new(HashMap::new()) }
    }

    /// Whether `key` has used up its window.
    pub(crate) fn exceeded(&self, key: &str, now: Instant) -> bool {
        let hits = self.hits.lock().expect("limit lock");
        hits.get(key).is_some_and(|&(start, n)| now.duration_since(start) < self.window && n >= self.max)
    }

    /// Counts one hit for `key`.
    pub(crate) fn hit(&self, key: &str, now: Instant) {
        let mut hits = self.hits.lock().expect("limit lock");
        if hits.len() >= SWEEP_AT {
            hits.retain(|_, &mut (start, _)| now.duration_since(start) < self.window);
        }
        let entry = hits.entry(key.to_owned()).or_insert((now, 0));
        if now.duration_since(entry.0) >= self.window {
            *entry = (now, 0);
        }
        entry.1 += 1;
    }

    /// Forgets `key` (a login that worked clears its username's failures).
    pub(crate) fn clear(&self, key: &str) {
        self.hits.lock().expect("limit lock").remove(key);
    }
}

/// The limits of the login and registration endpoints.
pub(crate) struct Limits {
    /// Failed password logins per visitor.
    pub(crate) login_ip: Limit,
    /// Failed password logins per username.
    pub(crate) login_user: Limit,
    /// Registrations per visitor.
    pub(crate) register_ip: Limit,
    /// Registrations per username asked for.
    pub(crate) register_user: Limit,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            login_ip: Limit::new(30, Duration::from_secs(10 * 60)),
            login_user: Limit::new(10, Duration::from_secs(15 * 60)),
            register_ip: Limit::new(10, Duration::from_secs(60 * 60)),
            register_user: Limit::new(5, Duration::from_secs(60 * 60)),
        }
    }
}

/// The visitor's address, as Cloudflare saw it. None without the header (a request that didn't
/// come through Cloudflare: the visitor limit doesn't apply, the username limit still does).
pub(crate) fn visitor(headers: &HeaderMap) -> Option<String> {
    headers
        .get("cf-connecting-ip")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
}

/// Usernames are counted without case: `Ann` and `ann` share one count.
pub(crate) fn username_key(username: &str) -> String {
    username.trim().to_lowercase()
}

pub(crate) fn rate_limited() -> ApiError {
    ApiError::new(ErrorCode::RateLimited, "Too many tries. Wait a few minutes and try again.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_allows_max_hits_then_refuses_until_it_ends() {
        let limit = Limit::new(2, Duration::from_secs(60));
        let t0 = Instant::now();
        assert!(!limit.exceeded("a", t0));
        limit.hit("a", t0);
        limit.hit("a", t0);
        assert!(limit.exceeded("a", t0 + Duration::from_secs(59)));
        assert!(!limit.exceeded("b", t0), "keys are counted apart");
        assert!(!limit.exceeded("a", t0 + Duration::from_secs(60)), "a new window");
        limit.hit("a", t0 + Duration::from_secs(60));
        assert!(!limit.exceeded("a", t0 + Duration::from_secs(60)));
        limit.clear("a");
        assert!(!limit.exceeded("a", t0));
    }

    #[test]
    fn the_visitor_is_cloudflares_address_and_usernames_ignore_case() {
        let mut headers = HeaderMap::new();
        assert_eq!(visitor(&headers), None);
        headers.insert("cf-connecting-ip", " 203.0.113.7 ".parse().unwrap());
        assert_eq!(visitor(&headers).as_deref(), Some("203.0.113.7"));
        assert_eq!(username_key(" Ann "), username_key("ann"));
    }
}
