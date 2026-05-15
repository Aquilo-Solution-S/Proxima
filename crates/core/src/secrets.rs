//! Secret resolution — `secret_ref` strings to opaque bytes.
//!
//! Embedding model registration (docs/10 §Embedding model: one per binary)
//! stores optional credentials as `secret_ref` URIs of shape
//! `<scheme>:<body>`. Current chat inference targets use `api_key_env`
//! names or provider-specific auth.
//! Schemes for v1:
//!
//! - `env:NAME` — process environment variable lookup
//! - `keychain:service:account` — OS keychain (impl in S1.c)
//! - `file:path` — local file read (impl deferred)
//! - `aws-sm:arn` — AWS Secrets Manager (impl deferred)
//!
//! S1.b ships the trait + `EnvResolver` + a `ResolverRegistry` keyed
//! by scheme prefix. Other resolvers register against the same trait.

use std::fmt;

/// Opaque secret payload. v1 is a thin `Vec<u8>` wrapper — no
/// zero-on-drop yet, no constant-time compare. The `Debug` impl
/// elides the contents so accidental logging cannot leak.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretBytes(Vec<u8>);

impl SecretBytes {
    #[must_use]
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }

    /// UTF-8 view of the payload, if valid. Most v1 secrets are
    /// API-key strings, so this is the common access path.
    pub fn as_str(&self) -> Option<&str> {
        std::str::from_utf8(&self.0).ok()
    }
}

impl fmt::Debug for SecretBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretBytes(<{} bytes redacted>)", self.0.len())
    }
}

/// Errors from `SecretResolver::resolve` and `ResolverRegistry::resolve`.
#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    /// `secret_ref` did not contain a `:` separator, or the scheme
    /// or body was empty.
    #[error("invalid secret_ref format: {0:?}")]
    InvalidFormat(String),

    /// No resolver registered for the requested scheme.
    #[error("no resolver registered for scheme {0:?}")]
    UnknownScheme(String),

    /// Resolver located the addressing target but the secret itself
    /// is missing (env var unset, file absent, keychain entry empty).
    #[error("secret not found: {0}")]
    NotFound(String),

    /// Underlying resolver failed for a reason other than absence
    /// (IO error, permission denied, malformed payload).
    #[error("resolver failed: {0}")]
    ResolverFailed(String),
}

/// One resolver per scheme. The trait is intentionally narrow — a
/// `body` parser per scheme keeps `secret_ref` syntax local to each
/// impl (e.g. `keychain:service:account` splits on `:` inside the
/// keychain resolver, not the registry).
pub trait SecretResolver: Send + Sync + std::fmt::Debug {
    /// Scheme prefix this resolver handles (no trailing colon).
    /// Examples: `"env"`, `"keychain"`, `"file"`, `"aws-sm"`.
    fn scheme(&self) -> &'static str;

    /// Resolve the body portion (everything after `<scheme>:`) into
    /// bytes. The body is opaque to the registry — the resolver
    /// owns its parsing.
    fn resolve(&self, body: &str) -> Result<SecretBytes, SecretError>;
}

/// Reads from `std::env::var`. Empty-string env values resolve
/// successfully (treated as a present-but-empty secret); the
/// caller decides whether to reject empty.
#[derive(Debug, Default, Clone, Copy)]
pub struct EnvResolver;

impl EnvResolver {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl SecretResolver for EnvResolver {
    fn scheme(&self) -> &'static str {
        "env"
    }

    fn resolve(&self, body: &str) -> Result<SecretBytes, SecretError> {
        if body.is_empty() {
            return Err(SecretError::InvalidFormat(format!("env:{body}")));
        }
        match std::env::var(body) {
            Ok(v) => Ok(SecretBytes::new(v.into_bytes())),
            Err(std::env::VarError::NotPresent) => {
                Err(SecretError::NotFound(format!("env var {body}")))
            }
            Err(std::env::VarError::NotUnicode(_)) => Err(SecretError::ResolverFailed(format!(
                "env var {body} contains non-UTF8 bytes"
            ))),
        }
    }
}

/// Holds one resolver per scheme. Registration is scheme-unique:
/// re-registering the same scheme replaces the prior resolver.
#[derive(Debug, Default)]
pub struct ResolverRegistry {
    resolvers: Vec<Box<dyn SecretResolver>>,
}

impl ResolverRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace any existing resolver for the same scheme.
    pub fn register(&mut self, r: Box<dyn SecretResolver>) {
        self.resolvers
            .retain(|existing| existing.scheme() != r.scheme());
        self.resolvers.push(r);
    }

    /// Resolve `secret_ref` of shape `<scheme>:<body>`. The first
    /// colon is the scheme/body separator; subsequent colons are
    /// part of `body` and parsed by the resolver.
    pub fn resolve(&self, secret_ref: &str) -> Result<SecretBytes, SecretError> {
        let (scheme, body) = secret_ref
            .split_once(':')
            .ok_or_else(|| SecretError::InvalidFormat(secret_ref.to_string()))?;
        if scheme.is_empty() {
            return Err(SecretError::InvalidFormat(secret_ref.to_string()));
        }
        let r = self
            .resolvers
            .iter()
            .find(|r| r.scheme() == scheme)
            .ok_or_else(|| SecretError::UnknownScheme(scheme.to_string()))?;
        r.resolve(body)
    }

    /// Convenience: registry pre-populated with `EnvResolver`.
    /// Composite binaries add `KeychainResolver` etc. on top.
    #[must_use]
    pub fn default_with_env() -> Self {
        let mut r = Self::new();
        r.register(Box::new(EnvResolver::new()));
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_resolver_returns_present_var() {
        // SAFETY: distinct test-only var name to avoid race with other tests.
        unsafe {
            std::env::set_var("PROXIMA_S1B_TEST_PRESENT", "hello");
        }
        let bytes = EnvResolver::new()
            .resolve("PROXIMA_S1B_TEST_PRESENT")
            .expect("present var resolves");
        assert_eq!(bytes.as_str(), Some("hello"));
        unsafe {
            std::env::remove_var("PROXIMA_S1B_TEST_PRESENT");
        }
    }

    #[test]
    fn env_resolver_not_found_when_absent() {
        unsafe {
            std::env::remove_var("PROXIMA_S1B_TEST_ABSENT");
        }
        let err = EnvResolver::new()
            .resolve("PROXIMA_S1B_TEST_ABSENT")
            .unwrap_err();
        assert!(matches!(err, SecretError::NotFound(_)));
    }

    #[test]
    fn env_resolver_rejects_empty_body() {
        let err = EnvResolver::new().resolve("").unwrap_err();
        assert!(matches!(err, SecretError::InvalidFormat(_)));
    }

    #[test]
    fn registry_routes_by_scheme() {
        unsafe {
            std::env::set_var("PROXIMA_S1B_TEST_REG", "via-registry");
        }
        let reg = ResolverRegistry::default_with_env();
        let bytes = reg.resolve("env:PROXIMA_S1B_TEST_REG").unwrap();
        assert_eq!(bytes.as_str(), Some("via-registry"));
        unsafe {
            std::env::remove_var("PROXIMA_S1B_TEST_REG");
        }
    }

    #[test]
    fn registry_unknown_scheme() {
        let reg = ResolverRegistry::default_with_env();
        let err = reg.resolve("keychain:foo:bar").unwrap_err();
        assert!(matches!(err, SecretError::UnknownScheme(s) if s == "keychain"));
    }

    #[test]
    fn registry_invalid_format() {
        let reg = ResolverRegistry::default_with_env();
        for bad in ["no-colon", ":missing-scheme"] {
            let err = reg.resolve(bad).unwrap_err();
            assert!(
                matches!(err, SecretError::InvalidFormat(_)),
                "bad ref {bad:?} should be InvalidFormat, got {err:?}"
            );
        }
    }

    #[test]
    fn registry_re_register_replaces() {
        #[derive(Debug)]
        struct A;
        impl SecretResolver for A {
            fn scheme(&self) -> &'static str {
                "x"
            }
            fn resolve(&self, _: &str) -> Result<SecretBytes, SecretError> {
                Ok(SecretBytes::new(b"first".to_vec()))
            }
        }
        #[derive(Debug)]
        struct B;
        impl SecretResolver for B {
            fn scheme(&self) -> &'static str {
                "x"
            }
            fn resolve(&self, _: &str) -> Result<SecretBytes, SecretError> {
                Ok(SecretBytes::new(b"second".to_vec()))
            }
        }
        let mut reg = ResolverRegistry::new();
        reg.register(Box::new(A));
        reg.register(Box::new(B)); // same scheme — replaces
        let out = reg.resolve("x:any").unwrap();
        assert_eq!(out.as_str(), Some("second"));
    }

    #[test]
    fn secret_bytes_debug_redacts() {
        let s = SecretBytes::new(b"super-secret".to_vec());
        let dbg = format!("{s:?}");
        assert!(
            !dbg.contains("super-secret"),
            "Debug must not leak bytes; got {dbg}"
        );
        assert!(dbg.contains("redacted"));
    }
}
