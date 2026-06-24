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
    force_object_root(&mut value);
    assert!(
        !schema_contains_ref(&value),
        "MCP tool type `{}` is recursive: schemars emitted a $ref that \
         cannot be inlined. MCP tool argument types must be non-recursive.",
        std::any::type_name::<T>(),
    );
    value
}

/// Ensure the generated schema is acceptable as an MCP `inputSchema` root.
///
/// Some enum-shaped schemas are emitted as top-level `oneOf` without an
/// explicit object root. MCP clients such as Pi require every tool input schema
/// to declare `type: "object"` and a root `properties` object.
fn force_object_root(value: &mut serde_json::Value) {
    if let serde_json::Value::Object(map) = value {
        map.entry("type".to_string())
            .or_insert_with(|| serde_json::Value::String("object".to_string()));
        map.entry("properties".to_string())
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    }
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
