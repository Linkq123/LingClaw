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

#[tokio::test]
async fn check_ssrf_blocks_unsupported_schemes() {
    assert!(check_ssrf("ftp://example.com").await.is_some());
    assert!(check_ssrf("file:///etc/passwd").await.is_some());
    assert!(check_ssrf("gopher://evil.com").await.is_some());
}

#[tokio::test]
async fn check_ssrf_blocks_private_ip_literals() {
    assert!(check_ssrf("http://127.0.0.1/admin").await.is_some());
    assert!(check_ssrf("http://10.0.0.1/internal").await.is_some());
    assert!(check_ssrf("http://192.168.1.1/").await.is_some());
    assert!(check_ssrf("http://[::1]/").await.is_some());
}

#[tokio::test]
async fn check_ssrf_allows_public_ip() {
    assert!(check_ssrf("http://8.8.8.8/dns").await.is_none());
    assert!(check_ssrf("https://1.1.1.1/").await.is_none());
}

#[tokio::test]
async fn check_ssrf_blocks_invalid_url() {
    assert!(check_ssrf("http://").await.is_some());
    assert!(check_ssrf("not-a-url").await.is_some());
}

#[tokio::test]
async fn check_ssrf_allows_https_public_domain() {
    // Public domains should pass (DNS resolves to public IPs)
    assert!(check_ssrf("https://example.com").await.is_none());
}

#[test]
fn shared_http_fetch_client_reuses_single_client_instance() {
    let first = shared_http_fetch_client().expect("shared fetch client should initialize");
    let second = shared_http_fetch_client().expect("shared fetch client should be reused");
    assert!(std::ptr::eq(first, second));
}

#[test]
fn html_fetch_read_limit_expands_and_caps_html_budget() {
    assert_eq!(html_fetch_read_limit(1_000), 4_000);
    assert_eq!(html_fetch_read_limit(100_000), 256 * 1024);
    assert_eq!(html_fetch_read_limit(300_000), 300_000);
}

#[test]
fn is_html_response_content_type_matches_html_and_xhtml() {
    assert!(is_html_response_content_type("text/html; charset=utf-8"));
    assert!(is_html_response_content_type("application/xhtml+xml"));
    assert!(!is_html_response_content_type("text/plain"));
}

#[test]
fn simplify_html_for_fetch_uses_visible_text_order() {
    let html = r#"
        <html>
          <body>
            <header><a href="/">Home</a></header>
            <main>
              <article>
                <h1>Title</h1>
                <p>First <strong>paragraph</strong>.</p>
              </article>
            </main>
          </body>
        </html>
    "#;

    let simplified = simplify_html_for_fetch(html);

    assert!(simplified.contains("Home Title First paragraph."));
}

#[test]
fn simplify_html_for_fetch_preserves_code_like_span_boundaries() {
    let html = r#"
        <html>
          <body>
            <code><span>foo</span><span>(</span><span>bar</span><span>)</span></code>
            <pre><span>{</span><span>"a"</span><span>:</span><span>1</span><span>}</span></pre>
          </body>
        </html>
    "#;

    let simplified = simplify_html_for_fetch(html);

    assert!(simplified.contains("foo(bar)"));
    assert!(simplified.contains("{\"a\":1}"));
    assert!(!simplified.contains("foo ( bar )"));
}

#[test]
fn simplify_html_for_fetch_keeps_inline_code_adjacent_to_punctuation() {
    let html = r#"
        <html>
          <body>
            <p>Use <code>foo(bar)</code>.</p>
          </body>
        </html>
    "#;

    let simplified = simplify_html_for_fetch(html);

    assert!(simplified.contains("Use foo(bar)."));
    assert!(!simplified.contains("Use foo(bar) ."));
}

#[test]
fn simplify_html_for_fetch_preserves_leading_indent_in_first_pre_line() {
    let html = r#"
        <html>
          <body><pre>  first
    second</pre></body>
        </html>
    "#;

    let simplified = simplify_html_for_fetch(html);

    assert!(simplified.starts_with("  first\n    second"));
}

#[test]
fn simplify_html_for_fetch_separates_adjacent_block_elements_in_compressed_html() {
    let html = "<html><body><h1>Title</h1><p>Intro</p><ul><li>Step1</li><li>Step2</li></ul></body></html>";

    let simplified = simplify_html_for_fetch(html);

    assert!(simplified.contains("Title Intro Step1 Step2"));
    assert!(!simplified.contains("TitleIntro"));
    assert!(!simplified.contains("Step1Step2"));
}

#[test]
fn simplify_html_for_fetch_keeps_noscript_fallback_text() {
    let html = r#"
        <html>
          <body>
            <div id="app"></div>
            <noscript>Enable JavaScript or read the fallback docs here.</noscript>
          </body>
        </html>
    "#;

    let simplified = simplify_html_for_fetch(html);

    assert!(simplified.contains("Enable JavaScript or read the fallback docs here."));
}

#[test]
fn simplify_html_for_fetch_preserves_preformatted_indentation_and_br() {
    let html = r#"
        <html>
          <body>
            <pre>if x:
  print("a")<br>  print("b")</pre>
          </body>
        </html>
    "#;

    let simplified = simplify_html_for_fetch(html);

    assert!(simplified.contains("if x:\n  print(\"a\")\n  print(\"b\")"));
}

#[test]
fn simplify_html_for_fetch_preserves_preformatted_newlines() {
    let html = r#"
        <html>
          <body>
            <pre>cargo test
  -- --nocapture
{"a": 1}</pre>
          </body>
        </html>
    "#;

    let simplified = simplify_html_for_fetch(html);

    assert!(simplified.contains("cargo test\n  -- --nocapture\n{\"a\": 1}"));
}

#[test]
fn truncate_decoded_text_adds_marker_after_html_simplification() {
    let body = truncate_decoded_text("Hello world".to_string(), true, 5);
    assert!(body.contains("[truncated at 5 bytes, reached fetch limit of 5 bytes]"));
}

#[test]
fn simplify_html_for_fetch_removes_tags_and_script_like_blocks() {
    let html = r#"
        <html>
          <head>
            <title>Example</title>
            <style>.hidden { display:none; }</style>
            <script>console.log('x')</script>
          </head>
          <body>
            <h1>Hello</h1>
            <p>World <strong>again</strong></p>
            <noscript>fallback only</noscript>
          </body>
        </html>
    "#;

    let simplified = simplify_html_for_fetch(html);

    assert!(!simplified.contains("<html"));
    assert!(simplified.contains("Hello"));
    assert!(simplified.contains("World"));
    assert!(simplified.contains("again"));
    assert!(simplified.contains("fallback only"));
    assert!(!simplified.contains("console.log"));
    assert!(!simplified.contains("display:none"));
}

#[test]
fn simplify_html_for_fetch_collapses_whitespace() {
    let simplified = simplify_html_for_fetch("<div>Hello</div>\n\n   <div>world</div>");
    assert_eq!(simplified, "Hello world");
}

#[test]
fn read_response_body_limited_truncates_at_max_bytes() {
    let text = truncate_bytes(b"hello world", 5);
    assert_eq!(text, "hello...\n[truncated at 5 bytes, reached fetch limit of 5 bytes]");
}

#[test]
fn read_response_body_limited_preserves_utf8_boundaries() {
    let text = truncate_bytes("你好世界".as_bytes(), 5);
    assert_eq!(text, "你...\n[truncated at 3 bytes, reached fetch limit of 5 bytes]");
}

#[tokio::test]
async fn validate_image_url_accepts_common_image_extensions() {
    assert!(
        validate_image_url("https://example.com/photo.jpg")
            .await
            .is_ok()
    );
    assert!(
        validate_image_url("https://example.com/photo.jpeg")
            .await
            .is_ok()
    );
    assert!(
        validate_image_url("https://example.com/photo.png")
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn validate_image_url_blocks_non_image_extensions() {
    assert!(
        validate_image_url("https://example.com/script.js")
            .await
            .is_err()
    );
    assert!(
        validate_image_url("https://example.com/page.html")
            .await
            .is_err()
    );
    assert!(
        validate_image_url("https://example.com/data.json")
            .await
            .is_err()
    );
    assert!(
        validate_image_url("https://example.com/file.pdf")
            .await
            .is_err()
    );
    assert!(
        validate_image_url("https://example.com/malware.exe")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn validate_image_url_blocks_unsupported_image_extensions() {
    assert!(
        validate_image_url("https://example.com/photo.gif")
            .await
            .is_err()
    );
    assert!(
        validate_image_url("https://example.com/photo.webp")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn validate_image_url_blocks_other_explicit_non_image_extensions() {
    assert!(
        validate_image_url("https://example.com/video.mp4")
            .await
            .is_err()
    );
    assert!(
        validate_image_url("https://example.com/report.csv")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn validate_image_url_blocks_encoded_non_image_extensions() {
    assert!(
        validate_image_url("https://example.com/video%2Emp4")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn validate_image_url_blocks_dotfile_non_image_extensions() {
    assert!(
        validate_image_url("https://example.com/.gif")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn validate_image_url_blocks_trailing_dot_bypass() {
    assert!(
        validate_image_url("https://example.com/video.mp4.")
            .await
            .is_err()
    );
    assert!(
        validate_image_url("https://example.com/script.js..")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn validate_image_url_allows_dynamic_urls_without_extensions() {
    assert!(
        validate_image_url("https://images.unsplash.com/photo-123456")
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn validate_image_url_blocks_private_ips() {
    assert!(
        validate_image_url("http://127.0.0.1/image.png")
            .await
            .is_err()
    );
    assert!(
        validate_image_url("http://10.0.0.1/image.jpg")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn validate_image_url_blocks_non_http_schemes() {
    assert!(
        validate_image_url("ftp://example.com/image.png")
            .await
            .is_err()
    );
    assert!(validate_image_url("file:///etc/passwd").await.is_err());
}
