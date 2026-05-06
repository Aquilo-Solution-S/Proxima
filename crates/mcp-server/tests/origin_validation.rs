use http::header::{HeaderMap, HeaderValue, ORIGIN};
use proxima_mcp_server::security::{assert_loopback, default_allowlist};

fn header_with(origin: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(ORIGIN, HeaderValue::from_str(origin).expect("origin"));
    headers
}

#[test]
fn loopback_origin_with_random_port_accepted() {
    let allow = default_allowlist();
    assert!(allow.allows(&header_with("http://127.0.0.1:54231")));
    assert!(allow.allows(&header_with("http://localhost:54231")));
    assert!(allow.allows(&header_with("http://localhost")));
}

#[test]
fn tauri_origin_accepted() {
    let allow = default_allowlist();
    assert!(allow.allows(&header_with("tauri://localhost")));
    assert!(allow.allows(&header_with("https://tauri.localhost")));
}

#[test]
fn missing_origin_rejected() {
    let allow = default_allowlist();
    assert!(!allow.allows(&HeaderMap::new()));
}

#[test]
fn off_host_origin_rejected() {
    let allow = default_allowlist();
    assert!(!allow.allows(&header_with("http://evil.example.com")));
    assert!(!allow.allows(&header_with("http://127.0.0.2:31415")));
}

#[test]
fn loopback_addrs() {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

    assert_loopback(&SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 31415)).unwrap();
    assert_loopback(&SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 31415)).unwrap();
    assert!(assert_loopback(&SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 31415)).is_err());
    assert!(
        assert_loopback(&SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(192, 168, 0, 1)),
            31415
        ))
        .is_err()
    );
}
