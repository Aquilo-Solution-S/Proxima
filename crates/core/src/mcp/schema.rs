//! MCP tool argument-schema generation.
//!
//! The single source of truth for a tool's argument schema is its Rust
//! `Args` type. `mcp_tool_schema` produces a `$ref`-free / `$defs`-free
//! JSON Schema draft 2020-12 document so that MCP clients which do not
//! resolve `$ref` still render every field (see commit 37f209b).

use schemars::JsonSchema;
use schemars::generate::SchemaSettings;

/// Generate a `$ref`-free draft-2020-12 argument schema for `T`.
///
/// Panics at registration (startup) if `T` is recursive: `schemars`
/// cannot inline a recursive subschema, so it emits a `$ref` that no
/// inlining pass can eliminate. A recursive MCP tool argument type is a
/// registration error.
pub(crate) fn mcp_tool_schema<T: JsonSchema>() -> serde_json::Value {
    let mut settings = SchemaSettings::draft2020_12();
    settings.inline_subschemas = true;
    let schema = settings.into_generator().into_root_schema_for::<T>();
    let mut value = serde_json::to_value(schema).expect("JsonSchema must serialize");
    flatten_root_tagged_enum(&mut value);
    ensure_client_safe_root::<T>(&mut value);
    assert!(
        !schema_contains_ref(&value),
        "MCP tool type `{}` is recursive: schemars emitted a $ref that \
         cannot be inlined. MCP tool argument types must be non-recursive.",
        std::any::type_name::<T>(),
    );
    value
}

/// Flatten a schemars root `oneOf` for an internally tagged enum into a plain
/// object schema with an `action` enum discriminator.
///
/// Anthropic/OpenAI-compatible tool schemas cannot rely on a root-level union.
/// Runtime serde validation remains authoritative for per-action required
/// fields; the flattened schema is the MCP/client-facing discovery surface.
fn flatten_root_tagged_enum(value: &mut serde_json::Value) {
    let Some(map) = value.as_object_mut() else {
        return;
    };
    let Some(variants) = map.get("oneOf").and_then(serde_json::Value::as_array) else {
        return;
    };

    let mut action_values = Vec::with_capacity(variants.len());
    let mut merged_properties = serde_json::Map::new();
    let mut action_metadata = serde_json::Map::new();
    let mut field_occurrences = std::collections::BTreeMap::<String, usize>::new();

    for variant in variants {
        let Some(properties) = variant
            .get("properties")
            .and_then(serde_json::Value::as_object)
        else {
            return;
        };
        let Some(action) = properties
            .get("action")
            .and_then(|schema| schema.get("const"))
            .and_then(serde_json::Value::as_str)
        else {
            return;
        };
        action_values.push(serde_json::Value::String(action.to_string()));
        let required = variant
            .get("required")
            .and_then(serde_json::Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .filter(|field| *field != "action")
                    .map(|field| serde_json::Value::String(field.to_string()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut allowed_fields = Vec::new();
        let mut field_descriptions = serde_json::Map::new();
        for (name, property_schema) in properties {
            if name != "action" {
                *field_occurrences.entry(name.clone()).or_default() += 1;
                allowed_fields.push(serde_json::Value::String(name.clone()));
                if let Some(description) = property_schema
                    .get("description")
                    .and_then(serde_json::Value::as_str)
                {
                    field_descriptions.insert(
                        name.clone(),
                        serde_json::Value::String(description.to_string()),
                    );
                }
                merge_property_schema(&mut merged_properties, name, property_schema, action);
            }
        }
        action_metadata.insert(
            action.to_string(),
            serde_json::json!({
                "allowedFields": allowed_fields,
                "required": required,
                "fieldDescriptions": field_descriptions,
            }),
        );
    }

    if action_values.is_empty() {
        return;
    }

    for (field, count) in field_occurrences {
        if count > 1
            && let Some(property_schema) = merged_properties.get_mut(&field)
        {
            neutralize_shared_property_description(property_schema, &field);
        }
    }

    merged_properties.insert(
        "action".to_string(),
        serde_json::json!({
            "type": "string",
            "enum": action_values,
            "description": "Dispatcher action to execute. Additional fields depend on the selected action."
        }),
    );
    map.remove("oneOf");
    map.insert(
        "properties".to_string(),
        serde_json::Value::Object(merged_properties),
    );
    map.insert("required".to_string(), serde_json::json!(["action"]));
    map.insert(
        "additionalProperties".to_string(),
        serde_json::Value::Bool(false),
    );
    map.insert(
        "x-proxima-actions".to_string(),
        serde_json::Value::Object(action_metadata),
    );
}

fn merge_property_schema(
    merged_properties: &mut serde_json::Map<String, serde_json::Value>,
    name: &str,
    property_schema: &serde_json::Value,
    action: &str,
) {
    let Some(existing) = merged_properties.get_mut(name) else {
        merged_properties.insert(name.to_string(), property_schema.clone());
        return;
    };
    if validation_shape(existing) == validation_shape(property_schema) {
        if existing != property_schema {
            neutralize_shared_property_description(existing, name);
        }
        return;
    }
    panic!(
        "conflicting property `{name}` while flattening action `{action}`: {existing:#} vs {property_schema:#}"
    );
}

fn neutralize_shared_property_description(value: &mut serde_json::Value, name: &str) {
    if let serde_json::Value::Object(map) = value {
        for key in ["default", "title"] {
            map.remove(key);
        }
        map.insert(
            "description".to_string(),
            serde_json::Value::String(format!(
                "Shared dispatcher field `{name}`. Semantics and requiredness depend on `action`; see `x-proxima-actions` or `proxima://tools` for action-specific guidance."
            )),
        );
    }
}

fn validation_shape(value: &serde_json::Value) -> serde_json::Value {
    let mut value = value.clone();
    strip_non_validation_fields(&mut value);
    normalize_nullable_types(&mut value);
    value
}

fn strip_non_validation_fields(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for key in ["description", "title", "default"] {
                map.remove(key);
            }
            for child in map.values_mut() {
                strip_non_validation_fields(child);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                strip_non_validation_fields(item);
            }
        }
        _ => {}
    }
}

fn normalize_nullable_types(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(type_value) = map.get_mut("type")
                && let serde_json::Value::Array(types) = type_value
            {
                types.retain(|item| item != "null");
                if types.len() == 1 {
                    *type_value = types[0].clone();
                }
            }
            for child in map.values_mut() {
                normalize_nullable_types(child);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                normalize_nullable_types(item);
            }
        }
        _ => {}
    }
}

/// Ensure the generated schema is acceptable as an MCP `inputSchema` root.
///
/// MCP clients such as Pi require every tool input schema to declare
/// `type: "object"` and a root `properties` object. Provider-compatible
/// tool schemas must also avoid root combinators.
fn ensure_client_safe_root<T: JsonSchema>(value: &mut serde_json::Value) {
    let serde_json::Value::Object(map) = value else {
        panic!(
            "MCP tool type `{}` root schema must be an object schema document",
            std::any::type_name::<T>(),
        );
    };
    for keyword in ["oneOf", "anyOf", "allOf"] {
        assert!(
            !map.contains_key(keyword),
            "MCP tool type `{}` leaves root schema combinator `{keyword}` after normalization; use an internally tagged dispatcher enum or a struct Args type",
            std::any::type_name::<T>(),
        );
    }
    if let Some(root_type) = map.get("type") {
        assert_eq!(
            root_type,
            "object",
            "MCP tool type `{}` root schema type must be `object`, got {root_type:#}",
            std::any::type_name::<T>(),
        );
    } else {
        map.insert(
            "type".to_string(),
            serde_json::Value::String("object".to_string()),
        );
    }
    map.entry("properties".to_string())
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
}

/// True if `value` contains a `$ref` key anywhere in its tree.
///
/// With `inline_subschemas = true`, a `$ref` survives generation only
/// for a recursive type, so this doubles as the recursion detector. It
/// only *detects* — it never transforms the schema.
fn schema_contains_ref(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(map) => {
            map.contains_key("$ref") || map.values().any(schema_contains_ref)
        }
        serde_json::Value::Array(items) => items.iter().any(schema_contains_ref),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use schemars::JsonSchema;

    #[derive(JsonSchema)]
    #[allow(dead_code)]
    struct Inner {
        label: String,
    }

    #[derive(JsonSchema)]
    #[allow(dead_code)]
    struct Nested {
        inner: Inner,
        count: u32,
    }

    #[derive(JsonSchema)]
    #[allow(dead_code)]
    struct Recursive {
        /// A self-referential field makes this type unrepresentable as a
        /// finite `$ref`-free schema.
        next: Option<Box<Recursive>>,
    }

    #[derive(JsonSchema)]
    #[allow(dead_code)]
    struct Described {
        /// Description authored as a doc-comment.
        documented: String,
        #[schemars(description = "Description authored as a schemars attribute.")]
        attributed: String,
    }

    #[derive(JsonSchema)]
    #[allow(dead_code)]
    #[serde(untagged)]
    enum UntaggedRootUnion {
        Text { text: String },
        Count { count: u32 },
    }

    #[derive(JsonSchema)]
    #[allow(dead_code)]
    #[serde(tag = "action", rename_all = "snake_case")]
    enum CollidingDispatcher {
        Text { value: String },
        Count { value: u32 },
    }

    #[test]
    fn nested_struct_schema_is_inlined() {
        let schema = mcp_tool_schema::<Nested>();
        assert!(
            !schema_contains_ref(&schema),
            "nested struct schema must be $ref-free: {schema:#}",
        );
        assert!(
            schema.get("$defs").is_none(),
            "nested struct schema must be $defs-free: {schema:#}",
        );
        assert!(
            schema
                .pointer("/properties/inner/properties/label")
                .is_some(),
            "the inlined Inner subschema must expose its fields: {schema:#}",
        );
    }

    #[test]
    #[should_panic(expected = "is recursive")]
    fn recursive_type_panics() {
        let _ = mcp_tool_schema::<Recursive>();
    }

    #[test]
    #[should_panic(expected = "root schema combinator")]
    fn unflattenable_root_union_panics() {
        let _ = mcp_tool_schema::<UntaggedRootUnion>();
    }

    #[test]
    #[should_panic(expected = "root schema type")]
    fn non_object_root_panics() {
        let _ = mcp_tool_schema::<String>();
    }

    #[test]
    #[should_panic(expected = "conflicting property")]
    fn tagged_enum_duplicate_incompatible_properties_panic() {
        let _ = mcp_tool_schema::<CollidingDispatcher>();
    }

    #[test]
    fn field_descriptions_survive() {
        let schema = mcp_tool_schema::<Described>();
        assert_eq!(
            schema
                .pointer("/properties/documented/description")
                .and_then(serde_json::Value::as_str),
            Some("Description authored as a doc-comment."),
            "doc-comment description must survive into the schema: {schema:#}",
        );
        assert_eq!(
            schema
                .pointer("/properties/attributed/description")
                .and_then(serde_json::Value::as_str),
            Some("Description authored as a schemars attribute."),
            "schemars-attribute description must survive into the schema: {schema:#}",
        );
    }
}
