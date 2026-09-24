//! Request-scoped values on the served tool path.
//!
//! Two per-request inputs reach a tool through its service set, beside the
//! boot-built one:
//!
//! - [`RequestHeaders`]: the inbound headers a host allowlists
//!   ([`RequestHeaderAllowlist`], `PROXIMA_REQUEST_HEADERS`), as opaque values.
//! - A [`FlavorServices`] a host middleware placed in the request's
//!   extensions, merged onto the boot set with
//!   [`FlavorServices::try_extend`] — the served-path twin of
//!   `BuiltProxima::core_mcp_tools_with_request_services`.

use http::{HeaderMap, HeaderName};
use proxima_core::{FlavorServices, McpToolError, RequestHeaders};

use crate::McpServerError;

/// Headers that carry the caller's own credentials. Forwarding one to flavor
/// code would hand a tool the bearer the edge just consumed, so no allowlist
/// may name one, directly or through a prefix.
const CREDENTIAL_HEADERS: [&str; 3] = ["authorization", "proxy-authorization", "cookie"];

/// Which inbound request headers the served path copies into
/// [`RequestHeaders`].
///
/// Each entry is a header name (`x-pack-ticket`) or a prefix ending in `*`
/// (`x-piy-env-*`). Matching is case-insensitive. Empty — the default —
/// copies nothing and publishes no [`RequestHeaders`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RequestHeaderAllowlist {
    exact: Vec<HeaderName>,
    prefixes: Vec<String>,
}

impl RequestHeaderAllowlist {
    /// Parse allowlist entries. Blank entries are skipped.
    ///
    /// # Errors
    ///
    /// [`McpServerError::InvalidRequestHeader`] for an entry that is not a
    /// valid header name, a bare `*`, or an entry that names or prefixes a
    /// credential header (`authorization`, `proxy-authorization`, `cookie`).
    pub fn parse<I, S>(entries: I) -> Result<Self, McpServerError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut allowlist = Self::default();
        for raw in entries {
            let entry = raw.as_ref().trim().to_ascii_lowercase();
            if entry.is_empty() {
                continue;
            }
            if let Some(prefix) = entry.strip_suffix('*') {
                if prefix.is_empty() || HeaderName::from_bytes(prefix.as_bytes()).is_err() {
                    return Err(invalid(&entry, "a prefix must be a non-empty header name"));
                }
                if CREDENTIAL_HEADERS
                    .iter()
                    .any(|credential| credential.starts_with(prefix))
                {
                    return Err(invalid(&entry, "the prefix matches a credential header"));
                }
                if !allowlist.prefixes.contains(&prefix.to_owned()) {
                    allowlist.prefixes.push(prefix.to_owned());
                }
                continue;
            }
            let name = HeaderName::from_bytes(entry.as_bytes())
                .map_err(|_| invalid(&entry, "not a header name"))?;
            if CREDENTIAL_HEADERS.contains(&name.as_str()) {
                return Err(invalid(&entry, "credential headers never reach tools"));
            }
            if !allowlist.exact.contains(&name) {
                allowlist.exact.push(name);
            }
        }
        Ok(allowlist)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.exact.is_empty() && self.prefixes.is_empty()
    }

    /// Whether `name` is copied.
    #[must_use]
    pub fn allows(&self, name: &HeaderName) -> bool {
        self.exact.contains(name)
            || self
                .prefixes
                .iter()
                .any(|prefix| name.as_str().starts_with(prefix.as_str()))
    }

    /// Copy the allowlisted headers of one request. `None` when the
    /// allowlist is empty.
    ///
    /// # Errors
    ///
    /// [`McpToolError::InvalidInput`] when an allowlisted header appears more
    /// than once (which copy a proxy appended is ambiguous) or its value is
    /// not visible ASCII.
    pub fn extract(&self, headers: &HeaderMap) -> Result<Option<RequestHeaders>, McpToolError> {
        if self.is_empty() {
            return Ok(None);
        }
        let mut pairs = Vec::new();
        for name in headers.keys().filter(|name| self.allows(name)) {
            let mut values = headers.get_all(name).iter();
            let Some(value) = values.next() else {
                continue;
            };
            if values.next().is_some() {
                return Err(McpToolError::InvalidInput(format!(
                    "request header `{name}` must appear at most once"
                )));
            }
            let value = value.to_str().map_err(|_| {
                McpToolError::InvalidInput(format!("request header `{name}` must be visible ASCII"))
            })?;
            pairs.push((name.as_str(), value.to_owned()));
        }
        Ok(Some(RequestHeaders::from_pairs(pairs)))
    }
}

fn invalid(entry: &str, reason: &str) -> McpServerError {
    McpServerError::InvalidRequestHeader(format!("{entry:?}: {reason}"))
}

/// The per-request service set: the host's request-extension bag plus the
/// allowlisted headers.
///
/// # Errors
///
/// As [`RequestHeaderAllowlist::extract`], and [`McpToolError::Other`] when
/// the extension bag already carries a [`RequestHeaders`] — only the
/// allowlist may produce one.
pub(crate) fn request_services(
    allowlist: &RequestHeaderAllowlist,
    headers: &HeaderMap,
    extensions: &http::Extensions,
) -> Result<FlavorServices, McpToolError> {
    let mut services = extensions
        .get::<FlavorServices>()
        .cloned()
        .unwrap_or_default();
    let forged = || {
        McpToolError::Other(
            "request extensions carry RequestHeaders; only the header allowlist may".into(),
        )
    };
    if services.get::<RequestHeaders>().is_some() {
        return Err(forged());
    }
    if let Some(values) = allowlist.extract(headers)? {
        services.try_insert(values).map_err(|_| forged())?;
    }
    Ok(services)
}

#[cfg(test)]
mod tests {
    use http::HeaderValue;

    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        map
    }

    #[test]
    fn copies_exact_and_prefixed_names_only() {
        let allowlist = RequestHeaderAllowlist::parse(["X-Pack-Ticket", "x-piy-env-*", " "])
            .expect("valid entries");
        let copied = allowlist
            .extract(&headers(&[
                ("x-pack-ticket", "t1"),
                ("x-piy-env-forgejo-base-url", "https://git.test"),
                ("x-other", "no"),
                ("authorization", "Bearer secret"),
            ]))
            .expect("well-formed")
            .expect("allowlist is non-empty");

        assert_eq!(copied.get("X-PACK-TICKET"), Some("t1"));
        assert_eq!(
            copied.get("x-piy-env-forgejo-base-url"),
            Some("https://git.test")
        );
        assert_eq!(copied.len(), 2);
        assert_eq!(copied.get("authorization"), None);
    }

    #[test]
    fn an_empty_allowlist_publishes_nothing() {
        assert!(
            RequestHeaderAllowlist::default()
                .extract(&headers(&[("x-pack-ticket", "t1")]))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn credential_headers_cannot_be_allowlisted() {
        for entry in [
            "authorization",
            "Cookie",
            "proxy-authorization",
            "auth*",
            "*",
        ] {
            assert!(
                RequestHeaderAllowlist::parse([entry]).is_err(),
                "{entry} must be refused"
            );
        }
        assert!(RequestHeaderAllowlist::parse(["not a header"]).is_err());
    }

    #[test]
    fn a_repeated_or_non_ascii_header_is_refused() {
        let allowlist = RequestHeaderAllowlist::parse(["x-pack-ticket"]).unwrap();
        let err = allowlist
            .extract(&headers(&[("x-pack-ticket", "a"), ("x-pack-ticket", "b")]))
            .unwrap_err();
        assert!(err.to_string().contains("at most once"), "{err}");

        let mut map = HeaderMap::new();
        map.insert(
            "x-pack-ticket",
            HeaderValue::from_bytes(b"caf\xc3\xa9").unwrap(),
        );
        assert!(allowlist.extract(&map).is_err());
    }

    #[test]
    fn request_services_merges_the_extension_bag_and_the_headers() {
        #[derive(Debug)]
        struct Budget;
        let allowlist = RequestHeaderAllowlist::parse(["x-pack-ticket"]).unwrap();
        let mut extensions = http::Extensions::new();
        extensions.insert(FlavorServices::with(Budget));

        let services = request_services(
            &allowlist,
            &headers(&[("x-pack-ticket", "t1")]),
            &extensions,
        )
        .expect("merge");
        assert!(services.get::<Budget>().is_some());
        assert_eq!(
            services
                .get::<RequestHeaders>()
                .unwrap()
                .get("x-pack-ticket"),
            Some("t1")
        );

        let mut forged = http::Extensions::new();
        forged.insert(FlavorServices::with(RequestHeaders::from_pairs([(
            "x-pack-ticket",
            "forged",
        )])));
        assert!(
            request_services(&allowlist, &headers(&[("x-pack-ticket", "t1")]), &forged).is_err()
        );
        assert!(
            request_services(
                &RequestHeaderAllowlist::default(),
                &HeaderMap::new(),
                &forged
            )
            .is_err(),
            "an empty allowlist does not make a forged value legitimate"
        );
    }
}
