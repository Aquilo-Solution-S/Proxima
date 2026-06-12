use std::sync::Arc;

use proxima_mcp_server::McpEdgeAuth;
use tauri::State;
use uuid::Uuid;

use crate::boot::sentinel_owner;
use crate::command_error::CommandError;
use crate::mcp::{McpListenerHandle, load_or_create_master_token, rotate_master_token};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct McpConnectionTs {
    pub url: Option<String>,
    pub token: String,
    pub authorization_header: String,
    pub listening: bool,
}

/// # Errors
/// Returns `CommandError::SecretStore` when the OS keychain cannot be used.
#[tauri::command]
#[specta::specta]
pub async fn mcp_connection_get(
    listener: State<'_, Option<McpListenerHandle>>,
    auth_store: State<'_, Arc<McpEdgeAuth>>,
) -> Result<McpConnectionTs, CommandError> {
    let owner = sentinel_owner();
    let token = load_or_create_master_token(&owner).map_err(CommandError::secret_store)?;
    auth_store.replace_local_master_token(token, owner).await;
    Ok(connection_payload(listener.as_ref(), token))
}

/// # Errors
/// Returns `CommandError::SecretStore` when the OS keychain cannot be used.
#[tauri::command]
#[specta::specta]
pub async fn mcp_master_token_rotate(
    listener: State<'_, Option<McpListenerHandle>>,
    auth_store: State<'_, Arc<McpEdgeAuth>>,
) -> Result<McpConnectionTs, CommandError> {
    let owner = sentinel_owner();
    let token = rotate_master_token(&owner).map_err(CommandError::secret_store)?;
    auth_store.replace_local_master_token(token, owner).await;
    Ok(connection_payload(listener.as_ref(), token))
}

fn connection_payload(listener: Option<&McpListenerHandle>, token: Uuid) -> McpConnectionTs {
    let wire_token = format!("pxm_{token}");
    let authorization_header = format!("Bearer {wire_token}");
    McpConnectionTs {
        url: listener.map(McpListenerHandle::url),
        token: wire_token,
        authorization_header,
        listening: listener.is_some(),
    }
}
