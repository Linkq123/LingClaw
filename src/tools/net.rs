use std::net::{IpAddr, ToSocketAddrs};
use std::sync::OnceLock;
use std::time::Duration;

use futures::StreamExt;
use reqwest::Client;
use scraper::{Html, Selector};

use crate::{Config, truncate};

const SAFE_FETCH_TIMEOUT: Duration = Duration::from_secs(15);
const HTTP_FETCH_DEFAULT_MAX_BYTES: usize = 102_400;
const HTTP_FETCH_HTML_READ_BUDGET_MULTIPLIER: usize = 4;
const HTTP_FETCH_HTML_READ_BUDGET_CAP: usize = 256 * 1024;
const HTML_BLOCK_SEPARATOR: char = '\u{001e}';
const HTML_PREFORMATTED_START: char = '\u{001f}';
const HTML_PREFORMATTED_END: char = '\u{001d}';

static HTTP_FETCH_CLIENT: OnceLock<Result<Client, String>> = OnceLock::new();

pub(crate) fn build_safe_fetch_client() -> Result<Client, String> {
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(SAFE_FETCH_TIMEOUT)
        .build()
        .map_err(|e| format!("http_fetch error: failed to create safe HTTP client: {e}"))
}

fn shared_http_fetch_client() -> Result<&'static Client, String> {
    HTTP_FETCH_CLIENT
        .get_or_init(build_safe_fetch_client)
        .as_ref()
        .map_err(Clone::clone)
}

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
pub(crate) async fn check_ssrf(url: &str) -> Option<String> {
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

/// Allowed image URL extensions (lowercase) — restricted to the formats LingClaw accepts.
const IMAGE_EXTENSIONS: &[&str] = &[".jpg", ".jpeg", ".png"];

/// Common image extensions that LingClaw intentionally rejects.
const UNSUPPORTED_IMAGE_EXTENSIONS: &[&str] = &[
    ".gif", ".webp", ".svg", ".bmp", ".ico", ".tif", ".tiff", ".avif",
];

fn decode_url_path_segment(segment: &str) -> String {
    let bytes = segment.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hi = (bytes[index + 1] as char).to_digit(16);
            let lo = (bytes[index + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                decoded.push(((hi << 4) | lo) as u8);
                index += 3;
                continue;
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn last_path_segment(url: &reqwest::Url) -> Option<String> {
    let segment = url
        .path_segments()
        .and_then(|mut segments| segments.rfind(|segment| !segment.is_empty()))?;
    Some(decode_url_path_segment(segment).to_ascii_lowercase())
}

fn explicit_path_extension(path: &str) -> Option<&str> {
    let segment = path.trim().trim_end_matches('.');
    let dot_index = segment.rfind('.')?;
    if dot_index + 1 >= segment.len() {
        return None;
    }
    Some(&segment[dot_index..])
}

fn truncate_bytes(bytes: &[u8], max: usize) -> String {
    truncate_decoded_text(String::from_utf8_lossy(bytes).into_owned(), bytes.len() > max, max)
}

fn truncate_decoded_text(text: String, was_truncated: bool, max: usize) -> String {
    if !was_truncated {
        return text;
    }
    let cut = text.len().min(max);
    let end = (0..=cut)
        .rev()
        .find(|&i| text.is_char_boundary(i))
        .unwrap_or(0);
    format!(
        "{}...\n[truncated at {} bytes, reached fetch limit of {} bytes]",
        &text[..end],
        end,
        max
    )
}

fn response_content_type(resp: &reqwest::Response) -> Option<String> {
    resp.headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_ascii_lowercase())
}

fn is_html_response_content_type(content_type: &str) -> bool {
    content_type.starts_with("text/html") || content_type.starts_with("application/xhtml+xml")
}

fn http_fetch_final_limit(max_bytes: usize, config: &Config) -> usize {
    max_bytes.min(config.max_output_bytes)
}

fn html_fetch_read_limit(final_limit: usize) -> usize {
    final_limit
        .saturating_mul(HTTP_FETCH_HTML_READ_BUDGET_MULTIPLIER)
        .min(HTTP_FETCH_HTML_READ_BUDGET_CAP)
        .max(final_limit)
}

struct LimitedBodyRead {
    text: String,
    was_truncated: bool,
}

async fn read_response_body_limited(
    resp: reqwest::Response,
    max_bytes: usize,
) -> Result<LimitedBodyRead, String> {
    let mut stream = resp.bytes_stream();
    let mut body = Vec::new();
    let read_limit = max_bytes.saturating_add(1);

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("http_fetch error reading body: {e}"))?;
        let remaining = read_limit.saturating_sub(body.len());
        if remaining == 0 {
            break;
        }
        let to_take = chunk.len().min(remaining);
        body.extend_from_slice(&chunk[..to_take]);
        if body.len() >= read_limit {
            break;
        }
    }

    let was_truncated = body.len() > max_bytes;
    if was_truncated {
        body.truncate(max_bytes);
    }

    Ok(LimitedBodyRead {
        text: String::from_utf8_lossy(&body).into_owned(),
        was_truncated,
    })
}

fn normalize_html_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn finalize_visible_html_text(text: &str) -> String {
    let mut out = String::new();
    let mut whitespace = String::new();
    let mut in_preformatted = false;

    for ch in text.chars() {
        match ch {
            HTML_PREFORMATTED_START => {
                whitespace.clear();
                in_preformatted = true;
            }
            HTML_PREFORMATTED_END => {
                while out.ends_with([' ', '\n']) {
                    out.pop();
                }
                in_preformatted = false;
            }
            HTML_BLOCK_SEPARATOR if in_preformatted => {
                if !out.ends_with('\n') {
                    out.push('\n');
                }
            }
            HTML_BLOCK_SEPARATOR => {
                whitespace.clear();
                if !out.is_empty() && !out.ends_with(' ') {
                    out.push(' ');
                }
            }
            '\r' if in_preformatted => {}
            '\n' if in_preformatted => {
                while out.ends_with(' ') {
                    out.pop();
                }
                if !out.ends_with('\n') {
                    out.push('\n');
                }
            }
            ch if ch.is_whitespace() && in_preformatted => {
                out.push(ch);
            }
            ch if ch.is_whitespace() => {
                whitespace.push(ch);
            }
            ch => {
                if !whitespace.is_empty() && !out.is_empty() && !out.ends_with(' ') {
                    out.push(' ');
                }
                whitespace.clear();
                out.push(ch);
            }
        }
    }

    if out.starts_with(HTML_BLOCK_SEPARATOR) {
        out.remove(0);
    }
    while out.ends_with([' ', '\n', HTML_BLOCK_SEPARATOR]) {
        out.pop();
    }
    out
}

fn normalize_html_text_preserving_preformatted(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn collect_preformatted_text(node: scraper::ElementRef<'_>, out: &mut String) {
    for child in node.children() {
        if let Some(text) = child.value().as_text() {
            out.push_str(text);
            continue;
        }
        if let Some(element) = scraper::ElementRef::wrap(child) {
            let name = element.value().name();
            if name == "br" {
                out.push('\n');
                continue;
            }
            if matches!(name, "script" | "style") {
                continue;
            }
            collect_preformatted_text(element, out);
        }
    }
}

fn strip_html_block_case_insensitive(html: &str, tag_name: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let open = format!("<{tag_name}");
    let close = format!("</{tag_name}>");
    let mut out = String::with_capacity(html.len());
    let mut cursor = 0;

    while let Some(start_rel) = lower[cursor..].find(&open) {
        let start = cursor + start_rel;
        out.push_str(&html[cursor..start]);
        let search_from = start + open.len();
        if let Some(close_rel) = lower[search_from..].find(&close) {
            let after_close = search_from + close_rel + close.len();
            cursor = after_close;
        } else {
            cursor = html.len();
            break;
        }
    }

    out.push_str(&html[cursor..]);
    out
}

fn strip_html_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;

    for ch in html.chars() {
        match ch {
            '<' => {
                in_tag = true;
                if !out.ends_with([' ', '\n']) {
                    out.push(' ');
                }
            }
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }

    normalize_html_whitespace(&out)
}

fn is_block_boundary_element(name: &str) -> bool {
    matches!(
        name,
        "address"
            | "article"
            | "aside"
            | "blockquote"
            | "body"
            | "br"
            | "caption"
            | "dd"
            | "details"
            | "div"
            | "dl"
            | "dt"
            | "figcaption"
            | "figure"
            | "footer"
            | "form"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "header"
            | "hr"
            | "li"
            | "main"
            | "nav"
            | "ol"
            | "p"
            | "pre"
            | "section"
            | "table"
            | "tbody"
            | "td"
            | "tfoot"
            | "th"
            | "thead"
            | "tr"
            | "ul"
    )
}

fn ensure_text_separator(out: &mut String) {
    if !out.is_empty() && !out.ends_with(HTML_BLOCK_SEPARATOR) {
        out.push(HTML_BLOCK_SEPARATOR);
    }
}

fn collect_inline_text(node: scraper::ElementRef<'_>, out: &mut String) {
    for child in node.children() {
        if let Some(text) = child.value().as_text() {
            out.push_str(text);
            continue;
        }
        if let Some(element) = scraper::ElementRef::wrap(child) {
            let name = element.value().name();
            if name == "br" {
                out.push(' ');
                continue;
            }
            if matches!(name, "script" | "style") {
                continue;
            }
            collect_inline_text(element, out);
        }
    }
}

fn collect_visible_text(node: scraper::ElementRef<'_>, out: &mut String) {
    let name = node.value().name();
    if name == "pre" {
        ensure_text_separator(out);
        out.push(HTML_PREFORMATTED_START);
        let mut preformatted = String::new();
        collect_preformatted_text(node, &mut preformatted);
        out.push_str(&normalize_html_text_preserving_preformatted(&preformatted));
        out.push(HTML_PREFORMATTED_END);
        ensure_text_separator(out);
        return;
    }
    if name == "code" {
        let mut inline = String::new();
        collect_inline_text(node, &mut inline);
        out.push_str(&inline);
        return;
    }

    let is_block = is_block_boundary_element(name);
    if is_block {
        ensure_text_separator(out);
    }

    for child in node.children() {
        if let Some(text) = child.value().as_text() {
            out.push_str(text);
            continue;
        }
        if let Some(element) = scraper::ElementRef::wrap(child) {
            let child_name = element.value().name();
            if matches!(child_name, "script" | "style") {
                continue;
            }
            collect_visible_text(element, out);
        }
    }

    if is_block {
        ensure_text_separator(out);
    }
}

fn visible_text_from_document(document: &Html) -> String {
    let body_selector = Selector::parse("body").expect("body selector should be valid");
    let mut out = String::new();
    if let Some(body) = document.select(&body_selector).next() {
        collect_visible_text(body, &mut out);
    } else {
        collect_visible_text(document.root_element(), &mut out);
    }
    finalize_visible_html_text(&out)
}

fn simplify_html_for_fetch(html: &str) -> String {
    let without_scripts = strip_html_block_case_insensitive(html, "script");
    let without_styles = strip_html_block_case_insensitive(&without_scripts, "style");
    let document = Html::parse_document(&without_styles);
    let visible = visible_text_from_document(&document);
    if visible.is_empty() {
        strip_html_tags(&without_styles)
    } else {
        visible
    }
}

/// Validate that a URL is a safe, reachable image URL.
/// Performs SSRF check, allows extensionless dynamic image URLs, and rejects
/// explicit non-PNG/JPEG suffixes early so obvious bad inputs fail before model calls.
pub(crate) async fn validate_image_url(url: &str) -> Result<(), String> {
    if let Some(msg) = check_ssrf(url).await {
        return Err(msg);
    }
    let parsed = reqwest::Url::parse(url).map_err(|e| format!("Invalid URL: {e}"))?;
    let Some(last_segment) = last_path_segment(&parsed) else {
        return Ok(());
    };
    let Some(extension) = explicit_path_extension(&last_segment) else {
        return Ok(());
    };
    if IMAGE_EXTENSIONS.contains(&extension) {
        return Ok(());
    }
    if UNSUPPORTED_IMAGE_EXTENSIONS.contains(&extension) {
        return Err(format!("Only PNG and JPEG image URLs are supported: {url}"));
    }
    Err(format!("URL does not appear to be an image: {url}"))
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
    let max_bytes = args["max_bytes"]
        .as_u64()
        .unwrap_or(HTTP_FETCH_DEFAULT_MAX_BYTES as u64) as usize;
    if max_bytes == 0 {
        return "http_fetch error: max_bytes must be >= 1".into();
    }

    let fetch_client = match shared_http_fetch_client() {
        Ok(client) => client,
        Err(err) => return err,
    };
    let final_limit = http_fetch_final_limit(max_bytes, config);

    let result = tokio::time::timeout(SAFE_FETCH_TIMEOUT, fetch_client.get(url).send()).await;

    match result {
        Ok(Ok(resp)) => {
            let status = resp.status();
            let content_type = response_content_type(&resp).unwrap_or_else(|| "unknown".into());
            let body_limit = if is_html_response_content_type(&content_type) {
                html_fetch_read_limit(final_limit)
            } else {
                final_limit
            };
            match read_response_body_limited(resp, body_limit).await {
                Ok(read) => {
                    let body = if is_html_response_content_type(&content_type) {
                        simplify_html_for_fetch(&read.text)
                    } else {
                        read.text
                    };
                    let body = truncate_decoded_text(body, read.was_truncated, body_limit);
                    let header = format!("HTTP {status} | {content_type}\n---\n");
                    truncate(&format!("{header}{body}"), final_limit)
                }
                Err(e) => e,
            }
        }
        Ok(Err(e)) => format!("http_fetch error: {e}"),
        Err(_) => format!(
            "http_fetch error: request timed out ({}s)",
            SAFE_FETCH_TIMEOUT.as_secs()
        ),
    }
}

#[cfg(test)]
#[path = "../tests/net_tests.rs"]
mod tests;
