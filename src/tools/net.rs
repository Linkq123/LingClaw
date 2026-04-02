use std::net::{IpAddr, ToSocketAddrs};
use std::time::Duration;

use reqwest::Client;

use crate::{Config, truncate};

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
/// DNS resolution runs on a blocking thread to avoid stalling tokio workers.
async fn check_ssrf(url: &str) -> Option<String> {
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
        // DNS resolution on a blocking thread to avoid stalling async workers
        let port = parsed.port().unwrap_or(80);
        let to_resolve = format!("{bare_host}:{port}");
        let dns_result = tokio::task::spawn_blocking(move || {
            to_resolve
                .to_socket_addrs()
                .ok()
                .and_then(|addrs| addrs.into_iter().find(|addr| is_private_ip(&addr.ip())))
        })
        .await;
        if let Ok(Some(private_addr)) = dns_result {
            return Some(format!(
                "BLOCKED: URL resolves to private/reserved address ({}). Refusing to fetch.",
                private_addr.ip()
            ));
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
    if let Some(msg) = check_ssrf(url).await {
        return msg;
    }
    let max_bytes = args["max_bytes"].as_u64().unwrap_or(102_400) as usize;
    if max_bytes == 0 {
        return "http_fetch error: max_bytes must be >= 1".into();
    }

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
#[path = "../tests/net_tests.rs"]
mod tests;
