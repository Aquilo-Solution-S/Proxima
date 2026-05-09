//! Tauri-managed MCP listener lifetime.

use std::net::SocketAddr;
use std::sync::Mutex;

use proxima_core::engine::EngineHandle;
use proxima_core::{Owner, Principal};
use uuid::Uuid;

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

pub(crate) fn load_or_create_master_token(owner: &Owner) -> Result<Uuid, String> {
    let entry = master_token_entry(owner)?;
    match entry.get_password() {
        Ok(raw) => match Uuid::parse_str(raw.trim()) {
            Ok(token) => Ok(token),
            Err(_) => rotate_master_token(owner),
        },
        Err(keyring::Error::NoEntry) => rotate_master_token(owner),
        Err(err) => Err(format!("keychain get_password: {err}")),
    }
}

pub(crate) fn rotate_master_token(owner: &Owner) -> Result<Uuid, String> {
    let token = Uuid::new_v4();
    master_token_entry(owner)?
        .set_password(&token.to_string())
        .map_err(|err| format!("keychain set_password: {err}"))?;
    Ok(token)
}

fn master_token_entry(owner: &Owner) -> Result<keyring::Entry, String> {
    keyring::Entry::new(MCP_MASTER_TOKEN_SERVICE, &master_token_account(owner))
        .map_err(|err| format!("keyring entry: {err}"))
}

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
