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

// ── validate_image_url ──────────────────────────────────────────────────────

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
