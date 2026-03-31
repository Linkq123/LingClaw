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
