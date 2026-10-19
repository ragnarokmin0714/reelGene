//! URL helpers with a security job each.
//!
//! * [`redact`] — what a URL looks like in a log or an error message. Signed media
//!   URLs carry their credential in the query string; logs never see it.
//! * [`is_private`] — whether a URL points at this machine or the local network.
//!   Auto-detected links of that kind are listed, never fetched on their own: a
//!   copied router-admin link must not turn into a request.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use url::{Host, Url};

/// `scheme://host[:port]/path`, with `?…` standing in for any query and the
/// fragment and userinfo dropped.
pub fn redact(url: &Url) -> String {
    let mut out = format!("{}://", url.scheme());
    if let Some(host) = url.host_str() {
        out.push_str(host);
    }
    if let Some(port) = url.port() {
        out.push_str(&format!(":{port}"));
    }
    out.push_str(url.path());
    if url.query().is_some() {
        out.push_str("?…");
    }
    out
}

/// True for anything that is not a public http(s) host: other schemes, hostnames
/// that only resolve locally, and IP literals in private, loopback, link-local or
/// shared address space. Decided from the URL alone — no DNS lookup, which would
/// itself be a request the user did not ask for.
pub fn is_private(url: &Url) -> bool {
    if !matches!(url.scheme(), "http" | "https") {
        return true;
    }
    match url.host() {
        None => true,
        Some(Host::Domain(d)) => {
            let d = d.trim_end_matches('.').to_ascii_lowercase();
            d == "localhost"
                || d.ends_with(".localhost")
                || d.ends_with(".local")
                || d.ends_with(".internal")
                || d.ends_with(".lan")
                || d.ends_with(".home.arpa")
                || !d.contains('.')
        }
        Some(Host::Ipv4(ip)) => private_v4(ip),
        Some(Host::Ipv6(ip)) => private_v6(ip),
    }
}

fn private_v4(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || a == 0
        || (a == 100 && (64..128).contains(&b)) // shared address space (CGNAT)
}

fn private_v6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return private_v4(v4);
    }
    let first = ip.segments()[0];
    ip.is_loopback()
        || ip.is_unspecified()
        || (first & 0xfe00) == 0xfc00 // unique local fc00::/7
        || (first & 0xffc0) == 0xfe80 // link-local fe80::/10
}

/// Whether a request that started at `from` may go on to `to` — a redirect, or a
/// segment or key URL a playlist names. A public page may not steer reelgene
/// into the local network; a link the user gave to their own NAS may go
/// anywhere.
pub fn may_follow(from: &Url, to: &Url) -> bool {
    matches!(to.scheme(), "http" | "https") && (!is_private(to) || is_private(from))
}

/// The host a URL talks to, for per-host limits and backoff. IP literals and
/// names alike; `None` only for URLs without a host.
pub fn host_key(url: &Url) -> Option<String> {
    url.host_str().map(|h| h.to_ascii_lowercase())
}

/// Whether an `IpAddr` is private — for callers that hold an address, not a URL.
pub fn ip_is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => private_v4(v4),
        IpAddr::V6(v6) => private_v6(v6),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn redaction_keeps_where_and_drops_the_credential() {
        assert_eq!(
            redact(&u(
                "https://cdn.example.com/a/ep01.m3u8?sign=abc&t=123#frag"
            )),
            "https://cdn.example.com/a/ep01.m3u8?…"
        );
        assert_eq!(
            redact(&u("https://user:pass@cdn.example.com:8443/x.ts")),
            "https://cdn.example.com:8443/x.ts",
            "userinfo never reaches a log"
        );
        assert_eq!(
            redact(&u("https://cdn.example.com/x.ts")),
            "https://cdn.example.com/x.ts"
        );
    }

    #[test]
    fn local_network_and_non_http_urls_are_private() {
        for s in [
            "http://192.168.1.1/admin",
            "http://10.0.0.5/",
            "http://172.20.1.1/",
            "http://127.0.0.1:8080/",
            "http://169.254.169.254/latest/meta-data",
            "http://100.64.0.1/",
            "http://0.0.0.0/",
            "http://[::1]/",
            "http://[fd00::1]/",
            "http://[fe80::1]/",
            "http://[::ffff:192.168.0.1]/",
            "http://localhost:3000/",
            "http://printer.local/",
            "http://nas.lan/",
            "http://intranet/",
            "file:///etc/passwd",
            "ftp://example.com/x",
        ] {
            assert!(is_private(&u(s)), "{s} should be private");
        }
    }

    #[test]
    fn public_hosts_are_not_private() {
        for s in [
            "https://cdn.example.com/a.m3u8",
            "https://8.8.8.8/",
            "https://[2001:4860:4860::8888]/",
            "http://100.128.0.1/",
        ] {
            assert!(!is_private(&u(s)), "{s} should be public");
        }
    }

    #[test]
    fn public_sources_cannot_lead_into_the_local_network() {
        let public = u("https://cdn.example.com/a.m3u8");
        let nas = u("http://192.168.1.20/a.m3u8");
        assert!(may_follow(&public, &u("https://other.example.net/seg1.ts")));
        assert!(!may_follow(&public, &u("http://192.168.1.1/admin")));
        assert!(!may_follow(&public, &u("http://localhost:8080/")));
        assert!(
            may_follow(&nas, &u("http://192.168.1.20/seg1.ts")),
            "the user's own NAS"
        );
        assert!(may_follow(&nas, &public));
        assert!(
            !may_follow(&nas, &u("file:///etc/passwd")),
            "never a non-http scheme"
        );
    }

    #[test]
    fn host_key_is_case_insensitive() {
        assert_eq!(
            host_key(&u("https://CDN.Example.com/x")).as_deref(),
            Some("cdn.example.com")
        );
    }
}
