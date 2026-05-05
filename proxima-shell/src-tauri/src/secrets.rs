//! Keychain-backed `SecretResolver` for the desktop shell.
//!
//! Implements the `keychain:<service>:<account>` scheme via the
//! `keyring` crate. Each platform's native secret store is used:
//! macOS Keychain, Windows Credential Manager, Linux Secret Service.
//!
//! Lives in the Tauri shell rather than `core` so headless binaries
//! (gRPC engine, tests, CI) don't pull in OS-keychain dependencies.

use proxima_core::secrets::{SecretBytes, SecretError, SecretResolver};

/// Resolves `keychain:<service>:<account>` against the OS-native
/// secret store. The body is split on the first `:` — service comes
/// first, account second. Both must be non-empty.
#[derive(Debug, Default, Clone, Copy)]
pub struct KeychainResolver;

impl KeychainResolver {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl SecretResolver for KeychainResolver {
    fn scheme(&self) -> &'static str {
        "keychain"
    }

    fn resolve(&self, body: &str) -> Result<SecretBytes, SecretError> {
        let (service, account) = body.split_once(':').ok_or_else(|| {
            SecretError::InvalidFormat(format!(
                "keychain body must be 'service:account', got {body:?}"
            ))
        })?;
        if service.is_empty() || account.is_empty() {
            return Err(SecretError::InvalidFormat(format!(
                "keychain service/account must both be non-empty, got {body:?}"
            )));
        }

        let entry = keyring::Entry::new(service, account).map_err(|e| {
            SecretError::ResolverFailed(format!("keyring::Entry::new: {e}"))
        })?;

        match entry.get_password() {
            Ok(secret) => Ok(SecretBytes::new(secret.into_bytes())),
            Err(keyring::Error::NoEntry) => Err(SecretError::NotFound(format!(
                "keychain entry {service}/{account}"
            ))),
            Err(e) => Err(SecretError::ResolverFailed(format!(
                "keyring::Entry::get_password: {e}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_body_without_colon() {
        let err = KeychainResolver::new().resolve("only-service").unwrap_err();
        assert!(matches!(err, SecretError::InvalidFormat(_)));
    }

    #[test]
    fn rejects_empty_service() {
        let err = KeychainResolver::new().resolve(":account").unwrap_err();
        assert!(matches!(err, SecretError::InvalidFormat(_)));
    }

    #[test]
    fn rejects_empty_account() {
        let err = KeychainResolver::new().resolve("service:").unwrap_err();
        assert!(matches!(err, SecretError::InvalidFormat(_)));
    }

    #[test]
    fn scheme_is_keychain() {
        assert_eq!(KeychainResolver::new().scheme(), "keychain");
    }

    #[test]
    #[ignore = "writes to real OS keychain; run with --ignored on a workstation"]
    fn roundtrip_against_real_keychain() {
        let service = "proxima-test-s1c";
        let account = format!("test-account-{}", std::process::id());
        let secret_value = "round-trip-secret-value";

        // Write directly via the keyring crate (the resolver is read-only).
        let entry = keyring::Entry::new(service, &account)
            .expect("Entry::new");
        entry.set_password(secret_value)
            .expect("set_password — does the test host have a keychain?");

        // Read via the resolver.
        let body = format!("{service}:{account}");
        let result = KeychainResolver::new().resolve(&body);

        // Always clean up before asserting.
        let _ = entry.delete_credential();

        let bytes = result.expect("resolve");
        assert_eq!(bytes.as_str(), Some(secret_value));
    }

    #[test]
    #[ignore = "talks to real OS keychain"]
    fn missing_entry_is_not_found() {
        let body = format!("proxima-test-s1c:absent-{}", uuid::Uuid::now_v7());
        let err = KeychainResolver::new().resolve(&body).unwrap_err();
        assert!(matches!(err, SecretError::NotFound(_)),
            "expected NotFound for absent entry, got {err:?}");
    }
}
