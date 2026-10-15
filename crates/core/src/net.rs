//! Shared outbound-endpoint transport validation.

use std::collections::BTreeSet;
use std::net::IpAddr;

/// Environment variable naming the hosts that may use plaintext HTTP in
/// addition to loopback. Comma-separated exact host names; empty by default.
pub const PROXIMA_PLAINTEXT_HOSTS: &str = "PROXIMA_PLAINTEXT_HOSTS";

/// Plaintext policy for an outbound HTTP endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointUrlPolicy {
    /// Require HTTPS for every endpoint.
    HttpsOnly,
    /// Permit plaintext HTTP only when the endpoint host is loopback, or is
    /// named in the [`PlaintextHosts`] passed to
    /// [`validate_endpoint_url_for_hosts`].
    AllowLoopbackHttp,
}

/// Failure to parse a [`PlaintextHosts`] setting.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{PROXIMA_PLAINTEXT_HOSTS} entry {entry:?} is not an exact host name: {reason}")]
pub struct PlaintextHostsError {
    entry: String,
    reason: &'static str,
}

/// Exact host names that may use plaintext HTTP in addition to loopback.
///
/// Empty by default: with no host named, every validation answers as it did
/// before this type existed. Entries are exact, ASCII-case-insensitive host
/// names. Wildcards, address ranges and address literals are refused when
/// the setting is parsed, so a bad value fails boot instead of widening the
/// policy. Ports and schemes are not part of an entry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlaintextHosts(BTreeSet<String>);

impl PlaintextHosts {
    /// No host named: loopback is the only plaintext exception.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// Parse a comma-separated list of exact host names. Blank input names
    /// no host.
    ///
    /// # Errors
    ///
    /// Returns [`PlaintextHostsError`] for an empty entry, a wildcard, an
    /// address range, an IP address literal, or any other value that is not
    /// a plain host name.
    pub fn parse(raw: &str) -> Result<Self, PlaintextHostsError> {
        if raw.trim().is_empty() {
            return Ok(Self::none());
        }
        raw.split(',')
            .map(parse_plaintext_host)
            .collect::<Result<BTreeSet<_>, _>>()
            .map(Self)
    }

    /// Read [`PROXIMA_PLAINTEXT_HOSTS`] through `lookup`; unset or blank
    /// names no host.
    ///
    /// # Errors
    ///
    /// Returns [`PlaintextHostsError`] as [`Self::parse`] does.
    pub fn from_lookup(
        lookup: &impl Fn(&str) -> Option<String>,
    ) -> Result<Self, PlaintextHostsError> {
        match crate::env_value(lookup, PROXIMA_PLAINTEXT_HOSTS) {
            Some(raw) => Self::parse(&raw),
            None => Ok(Self::none()),
        }
    }

    /// Whether no host is named.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Whether `host` is exactly one of the named hosts.
    #[must_use]
    pub fn contains(&self, host: &str) -> bool {
        !self.0.is_empty() && self.0.contains(&host.trim().to_ascii_lowercase())
    }
}

fn parse_plaintext_host(entry: &str) -> Result<String, PlaintextHostsError> {
    let entry = entry.trim();
    let refuse = |reason| PlaintextHostsError {
        entry: entry.to_string(),
        reason,
    };
    if entry.is_empty() {
        return Err(refuse("empty entry"));
    }
    if entry.contains('*') {
        return Err(refuse("wildcards are not accepted"));
    }
    if entry.contains('/') {
        return Err(refuse("address ranges are not accepted"));
    }
    let unbracketed = entry
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(entry);
    // A name with an all-digit label, or a last label that starts `0x`, is
    // an address or a range spelled like a name (`10.0.0.1-9`, `0x7f.1`):
    // URL parsers read a numeric last label as an IPv4 address.
    let numeric = |label: &str| !label.is_empty() && label.chars().all(|c| c.is_ascii_digit());
    let last_label = unbracketed.rsplit('.').next().unwrap_or_default();
    if unbracketed.parse::<IpAddr>().is_ok()
        || unbracketed.split('.').any(numeric)
        || last_label.to_ascii_lowercase().starts_with("0x")
    {
        return Err(refuse("address literals and ranges are not accepted"));
    }
    let label_ok = |label: &str| {
        !label.is_empty()
            && label
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    };
    if !entry.split('.').all(label_ok) {
        return Err(refuse(
            "only ASCII letters, digits, '-', '_' and '.' between non-empty labels",
        ));
    }
    Ok(entry.to_ascii_lowercase())
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
/// comparisons are ASCII-case-insensitive. Equivalent to
/// [`validate_endpoint_url_for_hosts`] with no host named.
///
/// # Errors
///
/// Returns [`EndpointUrlError::InvalidUrl`] for a malformed, relative, or
/// hostless URL and [`EndpointUrlError::InsecureTransport`] for a forbidden
/// scheme/host combination.
pub fn validate_endpoint_url(raw: &str, policy: EndpointUrlPolicy) -> Result<(), EndpointUrlError> {
    validate_endpoint_url_for_hosts(raw, policy, &PlaintextHosts::none())
}

/// Validate an absolute outbound endpoint URL against `policy`, admitting
/// plaintext HTTP to the hosts in `plaintext_hosts` as well as to loopback.
///
/// The named hosts widen only [`EndpointUrlPolicy::AllowLoopbackHttp`];
/// [`EndpointUrlPolicy::HttpsOnly`] ignores them. The match is on the URL's
/// parsed host, exact and ASCII-case-insensitive, so userinfo or a longer
/// name cannot borrow an entry.
///
/// # Errors
///
/// As [`validate_endpoint_url`].
pub fn validate_endpoint_url_for_hosts(
    raw: &str,
    policy: EndpointUrlPolicy,
    plaintext_hosts: &PlaintextHosts,
) -> Result<(), EndpointUrlError> {
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
        && (is_loopback_host(host) || plaintext_hosts.contains(host))
    {
        return Ok(());
    }

    Err(EndpointUrlError::InsecureTransport)
}

/// Whether `raw` is an endpoint URL whose host denotes the local machine.
///
/// Callers use this to decide whether a *second* endpoint learned from the
/// first may relax to plaintext: a loopback-hosted dependency may point at
/// its own loopback sub-endpoints, but a remote one may not drag key or
/// token resolution back onto the host. A URL that does not parse, or has no
/// host, is not loopback.
#[must_use]
pub fn is_loopback_endpoint(raw: &str) -> bool {
    raw.parse::<http::Uri>()
        .ok()
        .and_then(|uri| uri.host().map(is_loopback_host))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn hosts(raw: &str) -> PlaintextHosts {
        PlaintextHosts::parse(raw).expect("valid plaintext hosts")
    }

    const GATEWAY: &str = "http://gateway:9090";

    #[test]
    fn a_named_host_may_use_plaintext() {
        validate_endpoint_url_for_hosts(
            GATEWAY,
            EndpointUrlPolicy::AllowLoopbackHttp,
            &hosts("gateway"),
        )
        .expect("named host accepted");
        validate_endpoint_url_for_hosts(
            "HTTP://GATEWAY/v1",
            EndpointUrlPolicy::AllowLoopbackHttp,
            &hosts("Gateway, other"),
        )
        .expect("match is ASCII-case-insensitive and the list is comma-separated");
    }

    #[test]
    fn an_unnamed_host_is_refused() {
        for named in [PlaintextHosts::none(), hosts("gateway")] {
            for endpoint in [
                "http://other:9090",
                "http://gateway.evil.example:9090",
                "http://sub.gateway:9090",
                "http://gateway@evil.example:9090",
                "http://evil.example@gateway.evil.example",
            ] {
                assert_eq!(
                    validate_endpoint_url_for_hosts(
                        endpoint,
                        EndpointUrlPolicy::AllowLoopbackHttp,
                        &named,
                    ),
                    Err(EndpointUrlError::InsecureTransport),
                    "{endpoint} with {named:?}",
                );
            }
        }
        assert_eq!(
            validate_endpoint_url_for_hosts(
                GATEWAY,
                EndpointUrlPolicy::AllowLoopbackHttp,
                &PlaintextHosts::none(),
            ),
            Err(EndpointUrlError::InsecureTransport),
            "nothing named: gateway refused",
        );
        assert_eq!(
            validate_endpoint_url_for_hosts(
                "http://other:9090",
                EndpointUrlPolicy::AllowLoopbackHttp,
                &hosts("gateway"),
            ),
            Err(EndpointUrlError::InsecureTransport),
            "only gateway named: other refused",
        );
    }

    #[test]
    fn userinfo_cannot_borrow_a_named_host() {
        // The parsed host is `evil.example`; `gateway` is only userinfo.
        assert_eq!(
            validate_endpoint_url_for_hosts(
                "http://gateway@evil.example/v1",
                EndpointUrlPolicy::AllowLoopbackHttp,
                &hosts("gateway"),
            ),
            Err(EndpointUrlError::InsecureTransport),
        );
    }

    #[test]
    fn https_only_ignores_named_hosts() {
        assert_eq!(
            validate_endpoint_url_for_hosts(
                GATEWAY,
                EndpointUrlPolicy::HttpsOnly,
                &hosts("gateway"),
            ),
            Err(EndpointUrlError::InsecureTransport),
        );
    }

    #[test]
    fn wildcards_ranges_and_addresses_are_refused_at_parse() {
        for raw in [
            "*",
            "*.internal",
            "gate*",
            "gateway,*",
            "10.0.0.0/8",
            "10.0.0.1-10.0.0.9",
            "10.0.0.1-9",
            "0x7f.1",
            "0x7f000001",
            "10.0.0.5",
            "10.0",
            "::1",
            "[::1]",
            "fd00::/8",
            "gateway:9090",
            "http://gateway",
            "gateway/",
            "user@gateway",
            "gateway,,other",
            "gateway,",
            ",gateway",
            "gateway.",
            ".gateway",
            "gate way",
            "gate\u{e9}way",
        ] {
            assert!(
                PlaintextHosts::parse(raw).is_err(),
                "{raw:?} must be refused at boot",
            );
        }
    }

    #[test]
    fn lookup_reads_the_setting_and_blank_names_nothing() {
        let env = |value: Option<&'static str>| {
            move |key: &str| {
                (key == PROXIMA_PLAINTEXT_HOSTS)
                    .then_some(value)
                    .flatten()
                    .map(str::to_string)
            }
        };
        assert!(PlaintextHosts::from_lookup(&env(None)).unwrap().is_empty());
        assert!(
            PlaintextHosts::from_lookup(&env(Some("  ")))
                .unwrap()
                .is_empty()
        );
        assert!(
            PlaintextHosts::from_lookup(&env(Some("gateway")))
                .unwrap()
                .contains("gateway")
        );
        assert!(PlaintextHosts::from_lookup(&env(Some("*"))).is_err());
    }

    #[test]
    fn unset_answers_as_before() {
        for (endpoint, accepted_with_loopback_policy) in [
            ("https://api.example.com/v1", true),
            ("HTTPS://Api.Example.com/v1", true),
            ("http://localhost:11434", true),
            ("http://127.0.0.1:9000", true),
            ("http://[::1]:9000", true),
            ("http://gateway:9090", false),
            ("http://localhost.evil.example/v1", false),
            ("http://localhost@evil.example/v1", false),
            ("HTTP://api.example.com/v1", false),
        ] {
            for policy in [
                EndpointUrlPolicy::HttpsOnly,
                EndpointUrlPolicy::AllowLoopbackHttp,
            ] {
                let plain = validate_endpoint_url(endpoint, policy);
                assert_eq!(
                    plain,
                    validate_endpoint_url_for_hosts(endpoint, policy, &PlaintextHosts::none()),
                    "{endpoint}",
                );
                let expected = endpoint.to_ascii_lowercase().starts_with("https")
                    || (accepted_with_loopback_policy
                        && policy == EndpointUrlPolicy::AllowLoopbackHttp);
                assert_eq!(plain.is_ok(), expected, "{endpoint} under {policy:?}");
            }
        }
        for relative in ["gateway:9090", "gateway"] {
            assert!(matches!(
                validate_endpoint_url_for_hosts(
                    relative,
                    EndpointUrlPolicy::AllowLoopbackHttp,
                    &hosts("gateway"),
                ),
                Err(EndpointUrlError::InvalidUrl(_))
            ));
        }
    }
}
