//! Shared outbound-endpoint transport validation.

use std::net::IpAddr;

/// Plaintext policy for an outbound HTTP endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointUrlPolicy {
    /// Require HTTPS for every endpoint.
    HttpsOnly,
    /// Permit plaintext HTTP only when the endpoint host is loopback.
    AllowLoopbackHttp,
}

/// Failure to parse an endpoint URL or satisfy its transport policy.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EndpointUrlError {
    /// The value is not an absolute endpoint URL with a host.
    #[error("invalid endpoint URL: {0}")]
    InvalidUrl(String),
    /// The URL transport is forbidden by the selected policy.
    #[error("endpoint URL must use https")]
    InsecureTransport,
}

/// Whether `host` denotes the local machine.
///
/// Hostname matching is ASCII-case-insensitive. Brackets around IPv6 literals
/// are accepted.
#[must_use]
pub fn is_loopback_host(host: &str) -> bool {
    let host = host.trim();
    let host = host
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(host);

    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

/// Validate an absolute outbound endpoint URL against `policy`.
///
/// HTTPS is always accepted. HTTP is accepted only for loopback hosts when
/// [`EndpointUrlPolicy::AllowLoopbackHttp`] is selected. Scheme and hostname
/// comparisons are ASCII-case-insensitive.
///
/// # Errors
///
/// Returns [`EndpointUrlError::InvalidUrl`] for a malformed, relative, or
/// hostless URL and [`EndpointUrlError::InsecureTransport`] for a forbidden
/// scheme/host combination.
pub fn validate_endpoint_url(raw: &str, policy: EndpointUrlPolicy) -> Result<(), EndpointUrlError> {
    let uri = raw
        .parse::<http::Uri>()
        .map_err(|error| EndpointUrlError::InvalidUrl(error.to_string()))?;
    let scheme = uri
        .scheme_str()
        .ok_or_else(|| EndpointUrlError::InvalidUrl("missing scheme".into()))?;
    let host = uri
        .host()
        .ok_or_else(|| EndpointUrlError::InvalidUrl("missing host".into()))?;

    if scheme.eq_ignore_ascii_case("https") {
        return Ok(());
    }
    if scheme.eq_ignore_ascii_case("http")
        && policy == EndpointUrlPolicy::AllowLoopbackHttp
        && is_loopback_host(host)
    {
        return Ok(());
    }

    Err(EndpointUrlError::InsecureTransport)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_host_recognizes_hostname_and_ip_forms() {
        assert!(is_loopback_host("LOCALHOST"));
        assert!(is_loopback_host("127.0.0.1"));
        assert!(is_loopback_host("[::1]"));
        assert!(!is_loopback_host("api.example.com"));
        assert!(!is_loopback_host("10.0.0.1"));
    }

    #[test]
    fn https_is_accepted_under_both_policies() {
        for policy in [
            EndpointUrlPolicy::HttpsOnly,
            EndpointUrlPolicy::AllowLoopbackHttp,
        ] {
            validate_endpoint_url("HTTPS://Api.Example.com/v1", policy)
                .expect("https endpoint accepted");
        }
    }

    #[test]
    fn loopback_http_is_accepted_when_policy_allows_it() {
        for endpoint in [
            "http://LOCALHOST:11434",
            "http://127.0.0.1:9000",
            "http://[::1]:9000",
        ] {
            validate_endpoint_url(endpoint, EndpointUrlPolicy::AllowLoopbackHttp)
                .unwrap_or_else(|error| panic!("loopback endpoint {endpoint} rejected: {error}"));
        }
    }

    #[test]
    fn uppercase_plaintext_scheme_cannot_bypass_remote_rejection() {
        assert_eq!(
            validate_endpoint_url(
                "HTTP://api.example.com/v1",
                EndpointUrlPolicy::AllowLoopbackHttp,
            ),
            Err(EndpointUrlError::InsecureTransport)
        );
    }

    #[test]
    fn https_only_policy_rejects_loopback_http() {
        assert_eq!(
            validate_endpoint_url("http://localhost:8080", EndpointUrlPolicy::HttpsOnly),
            Err(EndpointUrlError::InsecureTransport)
        );
    }

    #[test]
    fn relative_or_hostless_urls_are_rejected() {
        assert!(matches!(
            validate_endpoint_url("api.example.com", EndpointUrlPolicy::AllowLoopbackHttp),
            Err(EndpointUrlError::InvalidUrl(_))
        ));
        assert!(matches!(
            validate_endpoint_url("https:///v1", EndpointUrlPolicy::AllowLoopbackHttp),
            Err(EndpointUrlError::InvalidUrl(_))
        ));
    }
}
