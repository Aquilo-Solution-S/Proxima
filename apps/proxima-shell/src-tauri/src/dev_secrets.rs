//! File-backed secret storage for `cfg(debug_assertions)` builds.
//!
//! macOS Keychain ACLs are tied to the binary's code signature, and
//! `cargo tauri dev` re-links on every build, invalidating "Always
//! Allow" and prompting for the login password on every launch. In
//! dev we sidestep the OS keychain entirely:
//!
//! - master token  → `$HOME/.proxima-dev/master-token-<acct>`
//! - `keychain:` `secret_refs` → `$HOME/.proxima-dev/secrets.json`
//!
//! Plain-text on disk; release builds keep using the real Keychain.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use proxima_core::secrets::{SecretBytes, SecretError, SecretResolver};
use proxima_core::{Owner, Principal};
use uuid::Uuid;

const DEV_DIR_NAME: &str = ".proxima-dev";
const SECRETS_FILE: &str = "secrets.json";

fn dev_data_dir() -> io::Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME not set"))?;
    let dir = PathBuf::from(home).join(DEV_DIR_NAME);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// File-backed analogue of `KeychainResolver`. Same `keychain:` scheme
/// so user configs work in dev without rewriting `secret_ref` URIs.
#[derive(Debug, Default, Clone, Copy)]
pub struct DevFileSecretResolver;

impl DevFileSecretResolver {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl SecretResolver for DevFileSecretResolver {
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

        let path = dev_data_dir()
            .map_err(|e| SecretError::ResolverFailed(format!("dev data dir: {e}")))?
            .join(SECRETS_FILE);

        let raw = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(SecretError::NotFound(format!(
                    "dev secrets file absent ({}); add an entry for {service}/{account}",
                    path.display()
                )));
            }
            Err(e) => {
                return Err(SecretError::ResolverFailed(format!(
                    "read {}: {e}",
                    path.display()
                )));
            }
        };

        let map: HashMap<String, String> = serde_json::from_str(&raw).map_err(|e| {
            SecretError::ResolverFailed(format!("parse {}: {e}", path.display()))
        })?;

        let key = format!("{service}:{account}");
        match map.get(&key) {
            Some(v) => Ok(SecretBytes::new(v.as_bytes().to_vec())),
            None => Err(SecretError::NotFound(format!(
                "dev secrets entry {service}/{account} (looked up key {key:?} in {})",
                path.display()
            ))),
        }
    }
}

/// Mirrors `mcp::load_or_create_master_token` but reads from a file
/// under `~/.proxima-dev/`. Same per-owner addressing so multi-owner
/// dev scenarios don't collide.
pub fn load_or_create_master_token(owner: &Owner) -> Result<Uuid, String> {
    let path = master_token_path(owner)?;
    match std::fs::read_to_string(&path) {
        Ok(raw) => match Uuid::parse_str(raw.trim()) {
            Ok(token) => Ok(token),
            Err(_) => rotate_master_token(owner),
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => rotate_master_token(owner),
        Err(e) => Err(format!("read {}: {e}", path.display())),
    }
}

pub fn rotate_master_token(owner: &Owner) -> Result<Uuid, String> {
    let path = master_token_path(owner)?;
    let token = Uuid::new_v4();
    write_private_file(&path, &token.to_string())
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(token)
}

fn master_token_path(owner: &Owner) -> Result<PathBuf, String> {
    let dir = dev_data_dir().map_err(|e| format!("dev data dir: {e}"))?;
    let principal = match &owner.principal {
        Principal::User(u) => format!("user-{}", (*u).into_inner()),
        Principal::Group(g) => format!("group-{}", (*g).into_inner()),
    };
    Ok(dir.join(format!(
        "master-token-{principal}-org-{}",
        owner.org_id.into_inner()
    )))
}

fn write_private_file(path: &Path, content: &str) -> io::Result<()> {
    use std::io::Write;

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(content.as_bytes())?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, content)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_body_without_colon() {
        let err = DevFileSecretResolver::new()
            .resolve("only-service")
            .unwrap_err();
        assert!(matches!(err, SecretError::InvalidFormat(_)));
    }

    #[test]
    fn rejects_empty_service() {
        let err = DevFileSecretResolver::new().resolve(":account").unwrap_err();
        assert!(matches!(err, SecretError::InvalidFormat(_)));
    }

    #[test]
    fn rejects_empty_account() {
        let err = DevFileSecretResolver::new().resolve("service:").unwrap_err();
        assert!(matches!(err, SecretError::InvalidFormat(_)));
    }

    #[test]
    fn scheme_is_keychain() {
        assert_eq!(DevFileSecretResolver::new().scheme(), "keychain");
    }
}
