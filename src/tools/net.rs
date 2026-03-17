use std::net::{IpAddr, ToSocketAddrs};
use std::time::Duration;

use reqwest::Client;

use crate::{truncate, Config};

/// Return true if an IP address is private/loopback/link-local/unspecified.
fn is_private_ip(ip: &IpAddr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() {
        return true;
    }
    match ip {
        IpAddr::V4(v4) => {
            v4.is_private()
                || v4.is_link_local()
                || v4.octets()[0] == 169 // 169.254.x.x link-local
                || v4.octets()[0] == 0 // 0.0.0.0/8
        }
        IpAddr::V6(v6) => {
            let segs = v6.segments();
            // unique-local (fc00::/7)
            (segs[0] & 0xfe00) == 0xfc00
                // link-local (fe80::/10)
                || (segs[0] & 0xffc0) == 0xfe80
        }
    }
}

/// Check if a URL targets a private/loopback/link-local address or a disallowed scheme.
/// Returns an error message if blocked, None if the URL is allowed.
fn check_ssrf(url: &str) -> Option<String> {
    // Only allow http and https schemes
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Some(format!(
            "BLOCKED: unsupported URL scheme in '{url}'. Only http:// and https:// are allowed."
        ));
    }
    // Use reqwest::Url for robust parsing (handles IPv6 brackets, userinfo, etc.)
    let parsed = match reqwest::Url::parse(url) {
        Ok(u) => u,
        Err(e) => return Some(format!("BLOCKED: invalid URL: {e}")),
    };
    let host = match parsed.host_str() {
        Some(h) => h.to_string(),
        None => return Some("BLOCKED: URL has no host.".into()),
    };
    // Strip IPv6 brackets if present for resolution
    let bare_host = host.trim_start_matches('[').trim_end_matches(']');
    // Try parsing as IP literal first
    if let Ok(ip) = bare_host.parse::<IpAddr>() {
        if is_private_ip(&ip) {
            return Some(format!(
                "BLOCKED: URL targets private/reserved address ({ip}). Refusing to fetch."
            ));
        }
    } else {
        // DNS resolution
        let port = parsed.port().unwrap_or(80);
        let to_resolve = format!("{bare_host}:{port}");
        if let Ok(addrs) = to_resolve.to_socket_addrs() {
            for addr in addrs {
                if is_private_ip(&addr.ip()) {
                    return Some(format!("BLOCKED: URL resolves to private/reserved address ({}). Refusing to fetch.", addr.ip()));
                }
            }
        }
    }
    None
}

// ── http_fetch ───────────────────────────────────────────────────────────────

pub(crate) async fn tool_http_fetch(
    args: &serde_json::Value,
    _http: &Client,
    config: &Config,
) -> String {
    let url = match args["url"].as_str() {
        Some(u) => u,
        None => return "Error: 'url' parameter is required".into(),
    };
    if let Some(msg) = check_ssrf(url) {
        return msg;
    }
    let max_bytes = args["max_bytes"].as_u64().unwrap_or(102_400) as usize;

    // Build a one-off client with redirects disabled to prevent redirect-based SSRF.
    let no_redirect = match Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .build()
    {
        Ok(c) => c,
        Err(e) => return format!("http_fetch error: failed to create safe HTTP client: {e}"),
    };

    let result = tokio::time::timeout(Duration::from_secs(15), no_redirect.get(url).send()).await;

    match result {
        Ok(Ok(resp)) => {
            let status = resp.status();
            let content_type = resp
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("unknown")
                .to_string();
            match resp.text().await {
                Ok(text) => {
                    let header = format!("HTTP {status} | {content_type}\n---\n");
                    truncate(
                        &format!("{header}{text}"),
                        max_bytes.min(config.max_output_bytes),
                    )
                }
                Err(e) => format!("http_fetch error reading body: {e}"),
            }
        }
        Ok(Err(e)) => format!("http_fetch error: {e}"),
        Err(_) => "http_fetch error: request timed out (15s)".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn is_private_ip_loopback() {
        assert!(is_private_ip(&IpAddr::V4(Ipv4Addr::LOCALHOST)));
        assert!(is_private_ip(&IpAddr::V6(Ipv6Addr::LOCALHOST)));
    }

    #[test]
    fn is_private_ip_unspecified() {
        assert!(is_private_ip(&IpAddr::V4(Ipv4Addr::UNSPECIFIED)));
        assert!(is_private_ip(&IpAddr::V6(Ipv6Addr::UNSPECIFIED)));
    }

    #[test]
    fn is_private_ip_private_ranges_v4() {
        assert!(is_private_ip(&IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))));
        assert!(is_private_ip(&IpAddr::V4(Ipv4Addr::new(172, 16, 0, 1))));
        assert!(is_private_ip(&IpAddr::V4(Ipv4Addr::new(172, 31, 255, 255))));
        assert!(is_private_ip(&IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))));
    }

    #[test]
    fn is_private_ip_link_local_v4() {
        assert!(is_private_ip(&IpAddr::V4(Ipv4Addr::new(169, 254, 1, 1))));
    }

    #[test]
    fn is_private_ip_v6_unique_local_and_link_local() {
        // fc00::/7 unique-local
        let ula: Ipv6Addr = "fd00::1".parse().unwrap();
        assert!(is_private_ip(&IpAddr::V6(ula)));
        // fe80::/10 link-local
        let ll: Ipv6Addr = "fe80::1".parse().unwrap();
        assert!(is_private_ip(&IpAddr::V6(ll)));
    }

    #[test]
    fn is_private_ip_public_addresses() {
        assert!(!is_private_ip(&IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
        assert!(!is_private_ip(&IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1))));
        let public_v6: Ipv6Addr = "2001:db8::1".parse().unwrap();
        assert!(!is_private_ip(&IpAddr::V6(public_v6)));
    }

    #[test]
    fn check_ssrf_blocks_unsupported_schemes() {
        assert!(check_ssrf("ftp://example.com").is_some());
        assert!(check_ssrf("file:///etc/passwd").is_some());
        assert!(check_ssrf("gopher://evil.com").is_some());
    }

    #[test]
    fn check_ssrf_blocks_private_ip_literals() {
        assert!(check_ssrf("http://127.0.0.1/admin").is_some());
        assert!(check_ssrf("http://10.0.0.1/internal").is_some());
        assert!(check_ssrf("http://192.168.1.1/").is_some());
        assert!(check_ssrf("http://[::1]/").is_some());
    }

    #[test]
    fn check_ssrf_allows_public_ip() {
        assert!(check_ssrf("http://8.8.8.8/dns").is_none());
        assert!(check_ssrf("https://1.1.1.1/").is_none());
    }

    #[test]
    fn check_ssrf_blocks_invalid_url() {
        assert!(check_ssrf("http://").is_some());
        assert!(check_ssrf("not-a-url").is_some());
    }

    #[test]
    fn check_ssrf_allows_https_public_domain() {
        // Public domains should pass (DNS resolves to public IPs)
        assert!(check_ssrf("https://example.com").is_none());
    }
}
