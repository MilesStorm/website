//! Rate limits on password login and registration: per visitor (Cloudflare's `CF-Connecting-IP`,
//! as the Gateway's own limits use) and per username, so neither one address nor many addresses
//! can try passwords for an account without end. Over a limit, the API answers 429
//! `rate_limited` before auth is asked.
//!
//! The counts are kept in each replica's memory, like the Gateway's per-pod limits: a restart
//! forgets them, and with N replicas the real ceiling is up to N times the number here.
//!
//! The per-username limit means anyone can lock an account out of password login for its window
//! (15 minutes) by failing ten logins for it; GitHub and Google logins still work then
//! (TRACING.md, "Rate limits").

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use api_types::{ApiError, ErrorCode};
use axum::http::HeaderMap;

/// Fixed windows: at most `max` slots per key in each `window`. A slot is taken before the work
/// it limits starts ([`Limit::try_take`]), so concurrent requests can't all pass the check before
/// any of them is counted, and given back when the work turns out not to count.
pub(crate) struct Limit {
    max: u32,
    window: Duration,
    state: Mutex<State>,
}

struct State {
    hits: HashMap<String, (Instant, u32)>,
    last_sweep: Instant,
}

/// The most keys one limit tracks. Past it a new key is refused (429) until a sweep frees room:
/// a flood of addresses or usernames can't grow the map, and the limited work stays limited.
const MAX_KEYS: usize = 100_000;

impl Limit {
    pub(crate) fn new(max: u32, window: Duration) -> Self {
        Self { max, window, state: Mutex::new(State { hits: HashMap::new(), last_sweep: Instant::now() }) }
    }

    /// Takes one of `key`'s slots in the current window; false when none is left.
    pub(crate) fn try_take(&self, key: &str, now: Instant) -> bool {
        let mut state = self.state.lock().expect("limit lock");
        // Expired windows go at most once a window: the sweep is O(keys), under the lock.
        if now.duration_since(state.last_sweep) >= self.window {
            let window = self.window;
            state.hits.retain(|_, &mut (start, _)| now.duration_since(start) < window);
            state.last_sweep = now;
        }
        if !state.hits.contains_key(key) && state.hits.len() >= MAX_KEYS {
            return false;
        }
        let entry = state.hits.entry(key.to_owned()).or_insert((now, 0));
        if now.duration_since(entry.0) >= self.window {
            *entry = (now, 0);
        }
        if entry.1 >= self.max {
            return false;
        }
        entry.1 += 1;
        true
    }

    /// Gives back a slot taken in the current window (the work didn't count).
    pub(crate) fn release(&self, key: &str, now: Instant) {
        let mut state = self.state.lock().expect("limit lock");
        if let Some(entry) = state.hits.get_mut(key) {
            if now.duration_since(entry.0) < self.window {
                entry.1 = entry.1.saturating_sub(1);
            }
        }
    }

    /// Forgets `key` (a login that worked clears its username's failures).
    pub(crate) fn clear(&self, key: &str) {
        self.state.lock().expect("limit lock").hits.remove(key);
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

/// Takes a slot of each of `limits` (limit, key) or of none: false, with nothing held, when one
/// is used up.
pub(crate) fn take_all(limits: &[(&Limit, &str)], now: Instant) -> bool {
    for (i, (limit, key)) in limits.iter().enumerate() {
        if !limit.try_take(key, now) {
            for (taken, key) in &limits[..i] {
                taken.release(key, now);
            }
            return false;
        }
    }
    true
}

/// Requests without a usable `CF-Connecting-IP` (that didn't come through Cloudflare) share one
/// visitor: they are limited together, never skipped.
pub(crate) const UNKNOWN_VISITOR: &str = "unknown";

/// The visitor, as Cloudflare saw it: an IPv4 address, or an IPv6 address's /64 (one household or
/// host usually has a whole /64, so its addresses are one visitor).
pub(crate) fn visitor(headers: &HeaderMap) -> String {
    let ip = headers
        .get("cf-connecting-ip")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<IpAddr>().ok());
    match ip {
        Some(IpAddr::V4(v4)) => v4.to_string(),
        Some(IpAddr::V6(v6)) => {
            let s = v6.segments();
            format!("{:x}:{:x}:{:x}:{:x}::/64", s[0], s[1], s[2], s[3])
        }
        None => UNKNOWN_VISITOR.to_owned(),
    }
}

/// The longest username an account can have (auth's `users.username varchar(255)`).
pub(crate) const MAX_USERNAME: usize = 255;

/// A username as a limit's key, counted without case (`Ann` and `ann` share one count); None
/// when it is longer than any account's, which is refused before any limit is touched.
pub(crate) fn username_key(username: &str) -> Option<String> {
    let name = username.trim();
    (name.chars().count() <= MAX_USERNAME).then(|| name.to_lowercase())
}

/// The answer to a username no account can have.
pub(crate) fn username_too_long() -> ApiError {
    ApiError::new(ErrorCode::Invalid, "That username is too long.")
}

pub(crate) fn rate_limited() -> ApiError {
    ApiError::new(ErrorCode::RateLimited, "Too many tries. Wait a few minutes and try again.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_has_max_slots_then_refuses_until_it_ends() {
        let limit = Limit::new(2, Duration::from_secs(60));
        let t0 = Instant::now();
        assert!(limit.try_take("a", t0));
        assert!(limit.try_take("a", t0));
        assert!(!limit.try_take("a", t0 + Duration::from_secs(59)));
        assert!(limit.try_take("b", t0), "keys are counted apart");
        limit.release("a", t0);
        assert!(limit.try_take("a", t0), "a slot given back can be taken again");
        assert!(limit.try_take("a", t0 + Duration::from_secs(60)), "a new window");
        limit.clear("a");
        assert!(limit.try_take("a", t0));
    }

    #[test]
    fn taking_from_several_limits_holds_all_or_none() {
        let (a, b) = (Limit::new(1, Duration::from_secs(60)), Limit::new(1, Duration::from_secs(60)));
        let t0 = Instant::now();
        assert!(b.try_take("k", t0));
        assert!(!take_all(&[(&a, "k"), (&b, "k")], t0));
        assert!(a.try_take("k", t0), "a's slot was given back");
    }

    #[test]
    fn a_full_limit_refuses_new_keys_until_a_sweep_frees_room() {
        let limit = Limit::new(1, Duration::from_secs(60));
        let t0 = Instant::now();
        for i in 0..MAX_KEYS {
            assert!(limit.try_take(&i.to_string(), t0));
        }
        assert!(!limit.try_take("new", t0));
        assert!(limit.try_take("new", t0 + Duration::from_secs(60)), "the expired windows were swept");
    }

    #[test]
    fn visitors_are_cloudflares_address_or_ipv6_slash_64_and_usernames_ignore_case() {
        let mut headers = HeaderMap::new();
        assert_eq!(visitor(&headers), UNKNOWN_VISITOR);
        headers.insert("cf-connecting-ip", " 203.0.113.7 ".parse().unwrap());
        assert_eq!(visitor(&headers), "203.0.113.7");
        headers.insert("cf-connecting-ip", "2001:db8:1:2:aaaa::1".parse().unwrap());
        assert_eq!(visitor(&headers), "2001:db8:1:2::/64");
        headers.insert("cf-connecting-ip", "not an address".parse().unwrap());
        assert_eq!(visitor(&headers), UNKNOWN_VISITOR);
        assert_eq!(username_key(" Ann "), username_key("ann"));
        assert!(username_key(&"a".repeat(MAX_USERNAME)).is_some());
        assert_eq!(username_key(&"a".repeat(MAX_USERNAME + 1)), None);
    }
}
