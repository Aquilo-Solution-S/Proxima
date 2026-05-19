//! Provider-facing wake tool projection.
//!
//! Wake entries store raw capability ids. This module derives the concrete
//! provider tool list for one wake without adding runtime registration.

use std::collections::HashMap;

use serde_json::{Map, Value};

use crate::mcp::provider_safe_tool_name;
use crate::personality::substrate_pack;
use crate::verbs::schema::{FlavorRegistryFrozen, PayloadKind};

const EMIT_ABSTRACTION: &str = "core/emit_abstraction";
const EMIT_PERSPECTIVE: &str = "core/emit_perspective";

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct HarnessToolProjection {
    pub palette_id: String,
    pub canonical_name: String,
    pub provider_name: String,
    pub description: String,
    pub input_schema: Value,
    pub dispatch: HarnessToolDispatch,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HarnessToolDispatch {
    DirectSubstrate {
        internal_canonical_name: String,
    },
    TypedEmit {
        internal_canonical_name: String,
        schema_id: String,
        schema_version: u32,
        payload_kind: PayloadKind,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum ToolProjectionError {
    #[error("emit tool {tool_id} has no registered {kind:?} schemas")]
    NoConcreteSchemas { tool_id: String, kind: PayloadKind },
    #[error("schema {schema_id} v{schema_version} is missing json_schema for {kind:?}")]
    MissingPayloadJsonSchema {
        schema_id: String,
        schema_version: u32,
        kind: PayloadKind,
    },
    #[error("tool {tool_id} is not registered in substrate pack or MCP registry")]
    UnknownTool { tool_id: String },
    #[error("provider-safe tool name collision for {provider_name}: {left} and {right}")]
    ProviderNameCollision {
        provider_name: String,
        left: String,
        right: String,
    },
    #[error("schema {schema_id} v{schema_version} cannot be projected: {reason}")]
    InvalidPayloadWrapperSchema {
        schema_id: String,
        schema_version: u32,
        reason: String,
    },
}

pub fn build_wake_tool_projection(
    registry: &FlavorRegistryFrozen,
    palette: &[String],
) -> Result<Vec<HarnessToolProjection>, ToolProjectionError> {
    let mut projected = Vec::new();
    let mut provider_names = HashMap::new();

    for palette_id in palette {
        match palette_id.as_str() {
            EMIT_ABSTRACTION => project_emit_tool(
                registry,
                palette_id,
                PayloadKind::Abstraction,
                &mut projected,
            )?,
            EMIT_PERSPECTIVE => project_emit_tool(
                registry,
                palette_id,
                PayloadKind::Perspective,
                &mut projected,
            )?,
            _ => projected.push(project_direct_tool(registry, palette_id)?),
        }
    }

    for tool in &projected {
        if let Some(left) =
            provider_names.insert(tool.provider_name.clone(), tool.canonical_name.clone())
        {
            return Err(ToolProjectionError::ProviderNameCollision {
                provider_name: tool.provider_name.clone(),
                left,
                right: tool.canonical_name.clone(),
            });
        }
    }

    Ok(projected)
}

fn project_direct_tool(
    registry: &FlavorRegistryFrozen,
    palette_id: &str,
) -> Result<HarnessToolProjection, ToolProjectionError> {
    if let Some(tool) = substrate_pack()
        .iter()
        .find(|tool| tool.tool_id() == palette_id)
    {
        return Ok(HarnessToolProjection {
            palette_id: palette_id.to_string(),
            canonical_name: palette_id.to_string(),
            provider_name: provider_safe_tool_name(palette_id),
            description: tool.description().to_string(),
            input_schema: tool.args_schema(),
            dispatch: HarnessToolDispatch::DirectSubstrate {
                internal_canonical_name: palette_id.to_string(),
            },
        });
    }

    if let Some(tool) = registry
        .list_mcp_tools()
        .iter()
        .find(|tool| tool.name == palette_id)
    {
        return Ok(HarnessToolProjection {
            palette_id: palette_id.to_string(),
            canonical_name: palette_id.to_string(),
            provider_name: provider_safe_tool_name(palette_id),
            description: tool.description.to_string(),
            input_schema: tool.args_schema.clone(),
            dispatch: HarnessToolDispatch::DirectSubstrate {
                internal_canonical_name: palette_id.to_string(),
            },
        });
    }

    Err(ToolProjectionError::UnknownTool {
        tool_id: palette_id.to_string(),
    })
}

fn project_emit_tool(
    registry: &FlavorRegistryFrozen,
    tool_id: &str,
    kind: PayloadKind,
    projected: &mut Vec<HarnessToolProjection>,
) -> Result<(), ToolProjectionError> {
    let schemas = registry
        .list()
        .into_iter()
        .filter(|schema| schema.kind == kind)
        .collect::<Vec<_>>();
    if schemas.is_empty() {
        return Err(ToolProjectionError::NoConcreteSchemas {
            tool_id: tool_id.to_string(),
            kind,
        });
    }

    for schema in schemas {
        let schema_id = schema.schema_id.as_str().to_string();
        let schema_version = schema.schema_version.into_inner();
        let payload_schema = registry
            .payload_json_schema(&schema.schema_id, schema.schema_version, kind)
            .ok_or_else(|| ToolProjectionError::MissingPayloadJsonSchema {
                schema_id: schema_id.clone(),
                schema_version,
                kind,
            })?;
        let input_schema = typed_emit_input_schema(payload_schema).map_err(|reason| {
            ToolProjectionError::InvalidPayloadWrapperSchema {
                schema_id: schema_id.clone(),
                schema_version,
                reason,
            }
        })?;
        let canonical_name = format!("{tool_id}::{schema_id}::v{schema_version}");
        projected.push(HarnessToolProjection {
            palette_id: tool_id.to_string(),
            provider_name: provider_safe_tool_name(&canonical_name),
            canonical_name,
            description: typed_emit_description(kind, &schema_id),
            input_schema,
            dispatch: HarnessToolDispatch::TypedEmit {
                internal_canonical_name: tool_id.to_string(),
                schema_id,
                schema_version,
                payload_kind: kind,
            },
        });
    }

    Ok(())
}

fn typed_emit_input_schema(payload_schema: &Value) -> Result<Value, String> {
    let object = payload_schema
        .as_object()
        .ok_or_else(|| "payload schema root must be an object".to_string())?;

    if let Some(schema_type) = object.get("type") {
        let is_object = match schema_type {
            Value::String(value) => value == "object",
            Value::Array(values) => values.iter().any(|value| value.as_str() == Some("object")),
            _ => false,
        };
        if !is_object {
            return Err(format!(
                "payload schema root must be type object, got {schema_type}"
            ));
        }
    }

    let properties = object
        .get("properties")
        .and_then(Value::as_object)
        .ok_or_else(|| "payload schema root must expose object properties".to_string())?;
    for reserved in ["text", "schema_id", "schema_version", "payload"] {
        if properties.contains_key(reserved) {
            return Err(format!(
                "payload schema uses reserved wrapper field {reserved}"
            ));
        }
    }

    let required = match object.get("required") {
        Some(Value::Array(values)) => {
            if values.iter().all(Value::is_string) {
                Some(Value::Array(values.clone()))
            } else {
                return Err("payload required must be an array of strings".to_string());
            }
        }
        Some(_) => return Err("payload required must be an array of strings".to_string()),
        None => None,
    };

    let mut root = Map::new();
    root.insert("type".to_string(), Value::String("object".to_string()));
    root.insert("additionalProperties".to_string(), Value::Bool(false));
    if let Some(defs) = object.get("$defs").cloned() {
        root.insert("$defs".to_string(), defs);
    }
    if let Some(definitions) = object.get("definitions").cloned() {
        root.insert("definitions".to_string(), definitions);
    }

    let mut lifted = properties.clone();
    normalize_reference_properties(&mut lifted);
    lifted.insert(
        "text".to_string(),
        serde_json::json!({
            "type": ["string", "null"],
            "description": "Optional authored text. Omit or null to derive text from payload."
        }),
    );
    root.insert("properties".to_string(), Value::Object(lifted));
    if let Some(required) = required {
        root.insert("required".to_string(), required);
    }

    Ok(Value::Object(root))
}

fn normalize_reference_properties(properties: &mut Map<String, Value>) {
    for (key, schema) in properties.iter_mut() {
        if is_reference_key(key) {
            *schema = serde_json::json!({
                "type": "string",
                "description": format!("Use the wake handle for `{key}` (for example N1, G1, P1, E1, or W1), not a raw UUID.")
            });
            continue;
        }
        if let Some(nested) = schema
            .get_mut("properties")
            .and_then(serde_json::Value::as_object_mut)
        {
            normalize_reference_properties(nested);
        }
    }
}

fn is_reference_key(key: &str) -> bool {
    let normalized = key.strip_suffix('s').unwrap_or(key);
    normalized == "goal_id"
        || normalized.ends_with("_goal_id")
        || normalized == "memory_id"
        || normalized.ends_with("_memory_id")
        || normalized == "personality_instance_id"
        || normalized.ends_with("_personality_instance_id")
        || normalized == "wake_entry_id"
        || normalized.ends_with("_wake_entry_id")
        || normalized == "edge_id"
        || normalized.ends_with("_edge_id")
}

fn typed_emit_description(kind: PayloadKind, schema_id: &str) -> String {
    let kind = match kind {
        PayloadKind::Abstraction => "Abstraction",
        PayloadKind::Perspective => "Perspective",
        _ => "typed memory",
    };
    format!(
        "Emit one {kind} memory with schema {schema_id}. Provide payload fields directly; schema_id and schema_version are hidden dispatch metadata."
    )
}
