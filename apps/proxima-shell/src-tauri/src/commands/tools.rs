use std::sync::Arc;

use proxima_core::Engine;
use proxima_core::error::ProtocolError;
use proxima_core::personality::substrate_pack;
use tauri::State;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct McpToolTs {
    pub name: String,
    pub description: String,
    pub flavor_id: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct WorkspaceToolTs {
    pub id: String,
    pub description: String,
}

#[tauri::command]
#[specta::specta]
pub async fn list_mcp_tools(
    engine: State<'_, Arc<Engine>>,
) -> Result<Vec<McpToolTs>, ProtocolError> {
    crate::perf::ipc::record("list_mcp_tools", 0, async move {
        let mut tools: Vec<McpToolTs> = substrate_pack()
            .iter()
            .map(|tool| McpToolTs {
                name: tool.tool_id().to_string(),
                description: tool.description().to_string(),
                flavor_id: "core".to_string(),
            })
            .collect();
        tools.extend(engine.registry().list_mcp_tools().iter().map(|d| {
            McpToolTs {
                name: d.name.to_string(),
                description: d.description.to_string(),
                flavor_id: d
                    .name
                    .split_once('/')
                    .map(|(f, _)| f.to_string())
                    .unwrap_or_default(),
            }
        }));
        Ok(tools)
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn list_workspace_tools() -> Result<Vec<WorkspaceToolTs>, ProtocolError> {
    crate::perf::ipc::record("list_workspace_tools", 0, async move {
        Ok(proxima_core::personality::WORKSPACE_TOOL_CATALOG
            .iter()
            .map(|(id, desc)| WorkspaceToolTs {
                id: (*id).to_string(),
                description: (*desc).to_string(),
            })
            .collect())
    })
    .await
}
