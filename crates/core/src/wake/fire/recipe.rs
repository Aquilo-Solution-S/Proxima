//! Recipe writing and YAML utilities for wake fire.

use std::path::PathBuf;

use uuid::Uuid;

use crate::error::ProtocolError;
use crate::mcp::provider_safe_tool_name;

/// Write the effective recipe with MCP extension injected.
pub async fn write_effective_recipe(
    source_bytes: &[u8],
    mcp_url: &str,
    wake_token: Uuid,
    substrate_tools: &[String],
    workspace_tools: &[String],
) -> Result<PathBuf, ProtocolError> {
    let source = std::str::from_utf8(source_bytes)
        .map_err(|e| ProtocolError::internal(format!("recipe is not utf8: {e}")))?;
    let mut rendered = strip_top_level_extensions(source);
    if !rendered.ends_with('\n') {
        rendered.push('\n');
    }
    rendered.push_str("extensions:\n");
    rendered.push_str("  - type: streamable_http\n");
    rendered.push_str("    name: proxima-engine-mcp\n");
    rendered.push_str(&format!("    uri: \"{}\"\n", yaml_quote(mcp_url)));
    rendered.push_str("    headers:\n");
    rendered.push_str(&format!(
        "      authorization: \"Bearer {}\"\n",
        yaml_quote(&wake_token.to_string())
    ));
    if substrate_tools.is_empty() {
        rendered.push_str("    available_tools: []\n");
    } else {
        rendered.push_str("    available_tools:\n");
        for tool in substrate_tools {
            rendered.push_str(&format!(
                "      - \"{}\"\n",
                yaml_quote(&provider_safe_tool_name(tool))
            ));
        }
    }
    for tool in workspace_tools {
        if !workspace_tool_supported(tool) {
            return Err(ProtocolError::tool_not_registered(format!(
                "workspace tool mapping missing: {tool}"
            )));
        }
    }

    let path =
        std::env::temp_dir().join(format!("proxima-wake-{}-{wake_token}.yaml", Uuid::new_v4()));
    tokio::fs::write(&path, rendered)
        .await
        .map_err(|e| ProtocolError::internal(format!("write effective recipe: {e}")))?;
    Ok(path)
}

/// Check if a workspace tool is supported.
pub fn workspace_tool_supported(tool_id: &str) -> bool {
    matches!(
        tool_id,
        "proxima-workspace/text_editor"
            | "proxima-workspace/shell"
            | "proxima-workspace/list_files"
    )
}

/// Strip top-level extensions from a recipe source.
pub fn strip_top_level_extensions(source: &str) -> String {
    let mut out = String::new();
    let mut skipping = false;
    for line in source.lines() {
        let is_top_level = !line.starts_with(' ') && !line.starts_with('\t');
        if is_top_level && line.trim_start().starts_with("extensions:") {
            skipping = true;
            continue;
        }
        if skipping
            && is_top_level
            && !line.trim().is_empty()
            && !line.trim_start().starts_with('#')
        {
            skipping = false;
        }
        if !skipping {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Quote a string for YAML.
pub fn yaml_quote(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_existing_top_level_extensions() {
        let source =
            "version: 1.0.0\nextensions:\n  - type: builtin\n    name: developer\nprompt: hi\n";

        let stripped = strip_top_level_extensions(source);

        assert!(!stripped.contains("type: builtin"));
        assert!(stripped.contains("prompt: hi"));
    }

    #[tokio::test]
    async fn effective_recipe_injects_wake_mcp_extension() {
        let token = Uuid::new_v4();
        let path = write_effective_recipe(
            b"version: 1.0.0\ntitle: smoke\nprompt: hi\n",
            "http://127.0.0.1:31415/mcp",
            token,
            &[
                "core/fetch_memory".to_string(),
                "core/emit_abstraction".to_string(),
            ],
            &[],
        )
        .await
        .expect("write effective recipe");

        let rendered = tokio::fs::read_to_string(&path).await.expect("read recipe");
        let _ = tokio::fs::remove_file(&path).await;

        assert!(rendered.contains("type: streamable_http"));
        assert!(rendered.contains("uri: \"http://127.0.0.1:31415/mcp\""));
        assert!(rendered.contains(&format!("authorization: \"Bearer {token}\"")));
        assert!(rendered.contains("- \"core_fetch_memory\""));
        assert!(rendered.contains("- \"core_emit_abstraction\""));
    }

    #[tokio::test]
    async fn effective_recipe_validates_workspace_tools_without_recipe_developer_extension() {
        let token = Uuid::new_v4();
        let path = write_effective_recipe(
            b"version: 1.0.0\ntitle: smoke\nprompt: hi\n",
            "http://127.0.0.1:31415/mcp",
            token,
            &[],
            &[
                "proxima-workspace/text_editor".to_string(),
                "proxima-workspace/shell".to_string(),
                "proxima-workspace/list_files".to_string(),
            ],
        )
        .await
        .expect("write effective recipe");

        let rendered = tokio::fs::read_to_string(&path).await.expect("read recipe");
        let _ = tokio::fs::remove_file(&path).await;

        assert!(!rendered.contains("name: developer"));
        assert!(!rendered.contains("developer__text_editor"));
    }
}
