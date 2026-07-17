//! Loopback hardening: Host + Origin allowlist and bearer-token checks.
//!
//! The core binds to 127.0.0.1, but loopback is not a security boundary: other local processes and
//! browser pages can reach it. A per-session bearer token gates every request; the Host check blocks
//! DNS-rebinding and the Origin check blocks cross-site (including WebSocket) calls from other pages.

/// Loopback hostnames the Host / Origin checks accept.
const LOOPBACK_HOSTS: [&str; 3] = ["127.0.0.1", "localhost", "::1"];

fn is_loopback(host: &str) -> bool {
    LOOPBACK_HOSTS.contains(&host)
}

/// Extract the hostname from a `host[:port]` authority (`[::1]:8000` -> `::1`).
fn hostname(authority: &str) -> Option<&str> {
    if authority.is_empty() {
        return None;
    }
    if let Some(rest) = authority.strip_prefix('[') {
        // Bracketed IPv6 literal: everything up to the closing bracket.
        return rest.split(']').next().filter(|h| !h.is_empty());
    }
    // `host` or `host:port` — the host is everything before the first colon.
    authority.split(':').next().filter(|h| !h.is_empty())
}

/// Extract the hostname from an Origin header value (`http://127.0.0.1:5173` -> `127.0.0.1`).
///
/// Returns `None` for a value without a scheme (e.g. the `null` origin of a sandboxed page), which
/// the caller treats as not allowed.
fn origin_hostname(origin: &str) -> Option<&str> {
    let (_scheme, authority) = origin.split_once("://")?;
    // Strip any path/query that follows the authority.
    let authority = authority.split(['/', '?', '#']).next().unwrap_or(authority);
    hostname(authority)
}

/// Whether the request's `Host` header names a loopback host. A missing Host is rejected: the core
/// requires an explicit, loopback Host.
pub fn host_allowed(host_header: Option<&str>) -> bool {
    match host_header {
        Some(host) => hostname(host).is_some_and(is_loopback),
        None => false,
    }
}

/// Whether the request's `Origin` header is loopback. A missing Origin is allowed (a non-browser
/// client; the token is the gate); a present but non-loopback Origin is rejected.
pub fn origin_allowed(origin_header: Option<&str>) -> bool {
    match origin_header {
        Some(origin) => origin_hostname(origin).is_some_and(is_loopback),
        None => true,
    }
}

/// Constant-time bytewise equality (length is not secret here — the token is fixed-length).
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Whether `provided` matches the session token (constant-time; empty never matches).
pub fn token_matches(provided: Option<&str>, expected: &str) -> bool {
    match provided {
        Some(token) if !token.is_empty() => constant_time_eq(token.as_bytes(), expected.as_bytes()),
        _ => false,
    }
}

/// Extract the `token` value from a raw query string (`a=b&token=xyz`), without percent-decoding:
/// the session token is hex (`[0-9a-f]`), which never contains a percent-encoded byte, so the raw
/// substring is the token verbatim. Used by the `<audio>` element and the WebSocket (which cannot
/// set an Authorization header) and by the index gate.
pub fn query_token(query: Option<&str>) -> Option<&str> {
    query?
        .split('&')
        .find_map(|pair| pair.strip_prefix("token="))
}

/// Extract the token from an `Authorization: Bearer <token>` header, or `None`.
pub fn bearer_token(authorization_header: Option<&str>) -> Option<&str> {
    let header = authorization_header?;
    let (scheme, token) = header.split_once(' ')?;
    if scheme.eq_ignore_ascii_case("bearer") && !token.is_empty() {
        Some(token)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_allows_loopback_forms_only() {
        assert!(host_allowed(Some("127.0.0.1:8000")));
        assert!(host_allowed(Some("localhost")));
        assert!(host_allowed(Some("[::1]:8000")));
        assert!(!host_allowed(Some("example.com")));
        assert!(!host_allowed(Some("10.0.0.1:8000")));
        assert!(!host_allowed(Some("")));
        assert!(!host_allowed(None));
    }

    #[test]
    fn origin_missing_is_allowed_but_non_loopback_is_not() {
        assert!(origin_allowed(None));
        assert!(origin_allowed(Some("http://127.0.0.1:5173")));
        assert!(origin_allowed(Some("http://localhost")));
        assert!(origin_allowed(Some("https://[::1]:443/path")));
        assert!(!origin_allowed(Some("https://evil.example")));
        assert!(!origin_allowed(Some("null")));
    }

    #[test]
    fn token_matches_is_exact_and_rejects_empty() {
        assert!(token_matches(Some("secret"), "secret"));
        assert!(!token_matches(Some("secre"), "secret"));
        assert!(!token_matches(Some("Secret"), "secret"));
        assert!(!token_matches(Some(""), "secret"));
        assert!(!token_matches(None, "secret"));
    }

    #[test]
    fn query_token_extracts_raw_value_without_decoding() {
        assert_eq!(query_token(Some("token=abc123")), Some("abc123"));
        assert_eq!(query_token(Some("a=b&token=abc123")), Some("abc123"));
        assert_eq!(query_token(Some("a=b&c=d")), None);
        assert_eq!(query_token(Some("")), None);
        assert_eq!(query_token(None), None);
    }

    #[test]
    fn bearer_token_parses_scheme_case_insensitively() {
        assert_eq!(bearer_token(Some("Bearer abc123")), Some("abc123"));
        assert_eq!(bearer_token(Some("bearer abc123")), Some("abc123"));
        assert_eq!(bearer_token(Some("Basic abc123")), None);
        assert_eq!(bearer_token(Some("Bearer ")), None);
        assert_eq!(bearer_token(Some("Bearer")), None);
        assert_eq!(bearer_token(None), None);
    }
}
