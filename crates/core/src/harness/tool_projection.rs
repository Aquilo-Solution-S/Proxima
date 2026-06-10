//! Provider-facing wake tool projection.
//!
//! Wake entries store raw capability ids. This module derives the concrete
//! provider tool list for one wake without adding runtime registration.

use std::collections::HashMap;

use serde_json::{Map, Value};

use crate::mcp::provider_safe_tool_name;
use crate::personality::{
    broad_emit_kind, parse_scoped_emit_tool_id, scoped_emit_tool_id, substrate_pack,
};
use crate::verbs::schema::{FlavorRegistryFrozen, PayloadKind};

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct HarnessToolProjection {
    pub palette_id: String,
    pub canonical_name: String,
    pub provider_name: String,
    pub description: String,
    pub produces_schema_ids: Vec<String>,
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
    #[error("invalid scoped emit tool {tool_id}: {reason}")]
    InvalidScopedEmitToolId { tool_id: String, reason: String },
    #[error("scoped emit schema {schema_id} v{schema_version} is not registered as {kind:?}")]
    ScopedEmitSchemaNotRegistered {
        schema_id: String,
        schema_version: u32,
        kind: PayloadKind,
    },
}

/// Project a wake entry's tool palette into harness tool definitions.
///
/// # Errors
///
/// Returns a `ToolProjectionError` when a palette id is unknown or
/// malformed, an emit schema is unregistered or unprojectable, or two
/// tools collide on the same provider-facing name.
pub fn build_wake_tool_projection(
    registry: &FlavorRegistryFrozen,
    palette: &[String],
) -> Result<Vec<HarnessToolProjection>, ToolProjectionError> {
    let mut projected = Vec::new();
    let mut provider_names = HashMap::new();

    for palette_id in palette {
        if let Some(scoped) = parse_scoped_emit_tool_id(palette_id).map_err(|err| {
            ToolProjectionError::InvalidScopedEmitToolId {
                tool_id: err.tool_id,
                reason: err.reason,
            }
        })? {
            project_one_emit_tool(
                registry,
                palette_id,
                scoped.base_tool_id,
                &scoped.schema_id,
                scoped.schema_version,
                scoped.kind,
                &mut projected,
            )?;
            continue;
        }
        if let Some(kind) = broad_emit_kind(palette_id) {
            project_broad_emit_tool(registry, palette_id, kind, &mut projected)?;
        } else {
            projected.push(project_direct_tool(registry, palette_id)?);
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
            produces_schema_ids: Vec::new(),
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
            produces_schema_ids: tool
                .produces_schema_ids
                .iter()
                .map(|schema_id| (*schema_id).to_string())
                .collect(),
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

fn project_broad_emit_tool(
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
        project_one_emit_tool(
            registry,
            tool_id,
            tool_id,
            schema.schema_id.as_str(),
            schema.schema_version.into_inner(),
            kind,
            projected,
        )?;
    }

    Ok(())
}

fn project_one_emit_tool(
    registry: &FlavorRegistryFrozen,
    palette_id: &str,
    internal_tool_id: &str,
    schema_id: &str,
    schema_version: u32,
    kind: PayloadKind,
    projected: &mut Vec<HarnessToolProjection>,
) -> Result<(), ToolProjectionError> {
    let schema_id_typed = crate::SchemaId::new(schema_id.to_string());
    let schema_version_typed = crate::SchemaVersion::new(schema_version);
    if registry
        .lookup_payload(&schema_id_typed, schema_version_typed, kind)
        .is_none()
    {
        return Err(ToolProjectionError::ScopedEmitSchemaNotRegistered {
            schema_id: schema_id.to_string(),
            schema_version,
            kind,
        });
    }
    let payload_schema = registry
        .payload_json_schema(&schema_id_typed, schema_version_typed, kind)
        .ok_or_else(|| ToolProjectionError::MissingPayloadJsonSchema {
            schema_id: schema_id.to_string(),
            schema_version,
            kind,
        })?;
    let input_schema = typed_emit_input_schema(payload_schema).map_err(|reason| {
        ToolProjectionError::InvalidPayloadWrapperSchema {
            schema_id: schema_id.to_string(),
            schema_version,
            reason,
        }
    })?;
    let canonical_name = scoped_emit_tool_id(internal_tool_id, schema_id, schema_version);
    projected.push(HarnessToolProjection {
        palette_id: palette_id.to_string(),
        provider_name: provider_safe_tool_name(&canonical_name),
        canonical_name,
        description: typed_emit_description(kind, schema_id),
        produces_schema_ids: vec![schema_id.to_string()],
        input_schema,
        dispatch: HarnessToolDispatch::TypedEmit {
            internal_canonical_name: internal_tool_id.to_string(),
            schema_id: schema_id.to_string(),
            schema_version,
            payload_kind: kind,
        },
    });
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
    ensure_property_descriptions(&mut lifted);
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

fn ensure_property_descriptions(properties: &mut Map<String, Value>) {
    for (key, schema) in properties.iter_mut() {
        if schema
            .get("description")
            .and_then(Value::as_str)
            .is_none_or(|description| description.trim().is_empty())
            && let Some(object) = schema.as_object_mut()
        {
            object.insert(
                "description".to_string(),
                Value::String(format!(
                    "Typed payload field `{key}` for this emitted memory."
                )),
            );
        }
        if let Some(nested) = schema.get_mut("properties").and_then(Value::as_object_mut) {
            ensure_property_descriptions(nested);
        }
    }
}

fn normalize_reference_properties(properties: &mut Map<String, Value>) {
    for (key, schema) in properties.iter_mut() {
        if is_reference_key(key) {
            *schema = reference_property_schema(key);
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

fn reference_property_schema(key: &str) -> Value {
    let description = reference_property_description(key);
    if is_plural_reference_key(key) {
        serde_json::json!({
            "type": "array",
            "items": { "type": "string" },
            "description": description,
        })
    } else {
        serde_json::json!({
            "type": "string",
            "description": description,
        })
    }
}

fn reference_property_description(key: &str) -> String {
    let examples = if key == "goal_id"
        || key == "goal_ids"
        || key.ends_with("_goal_id")
        || key.ends_with("_goal_ids")
    {
        "G1"
    } else if key == "memory_id"
        || key == "memory_ids"
        || key.ends_with("_memory_id")
        || key.ends_with("_memory_ids")
    {
        "F1, A1, or P1"
    } else if key == "personality_instance_id"
        || key == "personality_instance_ids"
        || key.ends_with("_personality_instance_id")
        || key.ends_with("_personality_instance_ids")
    {
        "I1"
    } else if key == "wake_entry_id"
        || key == "wake_entry_ids"
        || key.ends_with("_wake_entry_id")
        || key.ends_with("_wake_entry_ids")
    {
        "W1"
    } else if key == "edge_id"
        || key == "edge_ids"
        || key.ends_with("_edge_id")
        || key.ends_with("_edge_ids")
    {
        "E1"
    } else {
        "F1, A1, P1, G1, I1, E1, or W1"
    };
    format!("Use wake handles for `{key}` (for example {examples}), not raw UUIDs.")
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

fn is_plural_reference_key(key: &str) -> bool {
    key == "goal_ids"
        || key.ends_with("_goal_ids")
        || key == "memory_ids"
        || key.ends_with("_memory_ids")
        || key == "personality_instance_ids"
        || key.ends_with("_personality_instance_ids")
        || key == "wake_entry_ids"
        || key.ends_with("_wake_entry_ids")
        || key == "edge_ids"
        || key.ends_with("_edge_ids")
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::typed_emit_input_schema;

    #[test]
    fn typed_emit_schema_keeps_plural_reference_fields_as_arrays() {
        let payload_schema = json!({
            "type": "object",
            "properties": {
                "source_memory_ids": {
                    "type": "array",
                    "items": { "type": "string", "format": "uuid" }
                },
                "request_memory_id": {
                    "type": "string",
                    "format": "uuid"
                }
            },
            "required": ["source_memory_ids", "request_memory_id"]
        });

        let projected = typed_emit_input_schema(&payload_schema).expect("schema projects");
        let source = &projected["properties"]["source_memory_ids"];
        let request = &projected["properties"]["request_memory_id"];

        assert_eq!(source["type"].as_str(), Some("array"));
        assert_eq!(source["items"]["type"].as_str(), Some("string"));
        let source_description = source["description"].as_str().expect("description");
        assert!(source_description.contains("F1, A1, or P1"));
        assert!(!source_description.contains("W1"));
        assert_eq!(request["type"].as_str(), Some("string"));
    }
}
