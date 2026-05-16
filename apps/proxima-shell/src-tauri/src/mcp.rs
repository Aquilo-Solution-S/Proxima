//! Tauri-managed MCP listener lifetime.

use std::net::SocketAddr;
use std::sync::Mutex;

use proxima_core::Owner;
#[cfg(not(debug_assertions))]
use proxima_core::Principal;
use proxima_core::engine::EngineHandle;
use uuid::Uuid;

#[cfg(not(debug_assertions))]
const MCP_MASTER_TOKEN_SERVICE: &str = "proxima";

pub(crate) struct McpListenerHandle {
    url: String,
    engine_handle: Mutex<Option<EngineHandle>>,
}

impl McpListenerHandle {
    pub(crate) fn new(handle: EngineHandle, addr: SocketAddr) -> Self {
        Self {
            url: format!("http://{addr}/mcp"),
            engine_handle: Mutex::new(Some(handle)),
        }
    }

    #[must_use]
    pub(crate) fn url(&self) -> String {
        self.url.clone()
    }
}

impl Drop for McpListenerHandle {
    fn drop(&mut self) {
        if let Ok(mut handle) = self.engine_handle.lock()
            && let Some(handle) = handle.take()
        {
            let _ = handle.stop_tx.send(true);
            handle.dispatch_join.abort();
            if let Some(mcp_join) = handle.mcp_join {
                mcp_join.abort();
            }
        }
    }
}

impl std::fmt::Debug for McpListenerHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpListenerHandle")
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

/// Loads the per-owner MCP master token, rotating into a fresh one
/// if none exists or the stored value is malformed.
///
/// Release builds use the OS keychain; debug builds (`cargo tauri
/// dev`) fall back to a file under `~/.proxima-dev/` because dev
/// rebuilds re-link the binary and invalidate Keychain ACLs, which
/// surfaces as a password prompt every launch.
pub(crate) fn load_or_create_master_token(owner: &Owner) -> Result<Uuid, String> {
    #[cfg(debug_assertions)]
    {
        crate::dev_secrets::load_or_create_master_token(owner)
    }
    #[cfg(not(debug_assertions))]
    {
        let entry = master_token_entry(owner)?;
        match entry.get_password() {
            Ok(raw) => match Uuid::parse_str(raw.trim()) {
                Ok(token) => Ok(token),
                Err(_) => rotate_master_token(owner),
            },
            Err(keyring_core::Error::NoEntry) => rotate_master_token(owner),
            Err(err) => Err(format!("keychain get_password: {err}")),
        }
    }
}

pub(crate) fn rotate_master_token(owner: &Owner) -> Result<Uuid, String> {
    #[cfg(debug_assertions)]
    {
        crate::dev_secrets::rotate_master_token(owner)
    }
    #[cfg(not(debug_assertions))]
    {
        let token = Uuid::new_v4();
        master_token_entry(owner)?
            .set_password(&token.to_string())
            .map_err(|err| format!("keychain set_password: {err}"))?;
        Ok(token)
    }
}

#[cfg(not(debug_assertions))]
fn master_token_entry(owner: &Owner) -> Result<keyring_core::Entry, String> {
    keyring_core::Entry::new(MCP_MASTER_TOKEN_SERVICE, &master_token_account(owner))
        .map_err(|err| format!("keyring entry: {err}"))
}

#[cfg(not(debug_assertions))]
fn master_token_account(owner: &Owner) -> String {
    let principal = match &owner.principal {
        Principal::User(user) => format!("user:{}", (*user).into_inner()),
        Principal::Group(group) => format!("group:{}", (*group).into_inner()),
    };
    format!(
        "mcp-master-token:{principal}:org:{}",
        owner.org_id.into_inner()
    )
}
