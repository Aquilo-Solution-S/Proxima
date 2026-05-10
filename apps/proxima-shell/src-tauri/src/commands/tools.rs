use std::sync::Arc;

use proxima_core::Engine;
use proxima_core::error::ProtocolError;
use proxima_core::personality::{
    substrate_pack, writeable_relations_for_palette, writeable_schemas_for_palette,
};
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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct RelationTs {
    pub relation_id: String,
    pub flavor_id: String,
    pub class: String,
    pub typed: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct ProducesTs {
    pub schema_ids: Vec<String>,
    pub relation_ids: Vec<String>,
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

#[tauri::command]
#[specta::specta]
pub async fn list_relations(
    engine: State<'_, Arc<Engine>>,
) -> Result<Vec<RelationTs>, ProtocolError> {
    crate::perf::ipc::record("list_relations", 0, async move {
        Ok(engine
            .registry()
            .list_relations()
            .iter()
            .map(|d| RelationTs {
                relation_id: d.relation.clone(),
                flavor_id: d
                    .relation
                    .split_once('/')
                    .map(|(f, _)| f.to_string())
                    .unwrap_or_default(),
                class: d.class.as_str().to_string(),
                typed: d.payload_schema.is_some(),
            })
            .collect())
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn wake_entry_produces(
    engine: State<'_, Arc<Engine>>,
    substrate_palette: Vec<String>,
) -> Result<ProducesTs, ProtocolError> {
    crate::perf::ipc::record("wake_entry_produces", 0, async move {
        let schema_ids = writeable_schemas_for_palette(engine.as_ref(), &substrate_palette);
        let relation_ids = writeable_relations_for_palette(engine.as_ref(), &substrate_palette);
        Ok(ProducesTs {
            schema_ids,
            relation_ids,
        })
    })
    .await
}
