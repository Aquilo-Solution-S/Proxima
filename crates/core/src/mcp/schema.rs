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
/// object schema whose discriminator is the enum's own `#[serde(tag = "...")]`
/// key (e.g. `action`, `kind`), exposed as a string enum.
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

    // Detect the discriminator KEY: the single property name present across
    // every variant carrying a string `const`, with a distinct value per
    // variant. For an internally-tagged enum this is the `#[serde(tag = ...)]`
    // field. Bail (leaving the schema unflattened) if there is not exactly one.
    let Some(discriminator) = detect_discriminator_key(variants) else {
        return;
    };

    let mut action_values = Vec::with_capacity(variants.len());
    let mut merged_properties = serde_json::Map::new();
    let mut action_metadata = serde_json::Map::new();
    let mut field_occurrences = std::collections::BTreeMap::<String, usize>::new();

    for variant in variants {
        // A missing-properties / missing-const variant means this is not the
        // internally-tagged shape we can flatten; bail and leave it unflattened.
        if merge_variant(
            variant,
            &discriminator,
            &mut action_values,
            &mut merged_properties,
            &mut action_metadata,
            &mut field_occurrences,
        )
        .is_none()
        {
            return;
        }
    }

    if action_values.is_empty() {
        return;
    }

    for (field, count) in field_occurrences {
        if count > 1
            && let Some(property_schema) = merged_properties.get_mut(&field)
        {
            neutralize_shared_property_description(property_schema, &field, &discriminator);
        }
    }

    let discriminator_description = {
        let base = format!(
            "Dispatcher {discriminator} to execute. Additional fields depend on the selected {discriminator}."
        );
        let signatures = action_signature_block(&action_values, &action_metadata);
        if signatures.is_empty() {
            base
        } else {
            // Standard MCP clients render a property's description but not our
            // `x-proxima-actions` extension, so the per-action field contract is
            // inlined here where every client can see it.
            format!("{base}\nAction signatures (required, then +optional):\n{signatures}")
        }
    };
    merged_properties.insert(
        discriminator.clone(),
        serde_json::json!({
            "type": "string",
            "enum": action_values,
            "description": discriminator_description,
        }),
    );
    map.remove("oneOf");
    map.insert(
        "properties".to_string(),
        serde_json::Value::Object(merged_properties),
    );
    map.insert(
        "required".to_string(),
        serde_json::json!([discriminator.clone()]),
    );
    map.insert(
        "additionalProperties".to_string(),
        serde_json::Value::Bool(false),
    );
    map.insert(
        "x-proxima-actions".to_string(),
        serde_json::Value::Object(action_metadata),
    );
}

/// Fold one tagged-enum variant into the flattener's accumulators.
///
/// Returns `None` when the variant is not the expected internally-tagged object
/// shape (no `properties`, or no string `const` under `discriminator`), which
/// signals the caller to abort flattening and leave the schema as a root union.
fn merge_variant(
    variant: &serde_json::Value,
    discriminator: &str,
    action_values: &mut Vec<serde_json::Value>,
    merged_properties: &mut serde_json::Map<String, serde_json::Value>,
    action_metadata: &mut serde_json::Map<String, serde_json::Value>,
    field_occurrences: &mut std::collections::BTreeMap<String, usize>,
) -> Option<()> {
    let properties = variant
        .get("properties")
        .and_then(serde_json::Value::as_object)?;
    let action = properties
        .get(discriminator)
        .and_then(|schema| schema.get("const"))
        .and_then(serde_json::Value::as_str)?;
    action_values.push(serde_json::Value::String(action.to_string()));
    let required = variant
        .get("required")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(serde_json::Value::as_str)
                .filter(|field| *field != discriminator)
                .map(|field| serde_json::Value::String(field.to_string()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut allowed_fields = Vec::new();
    let mut field_descriptions = serde_json::Map::new();
    for (name, property_schema) in properties {
        if name != discriminator {
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
            merge_property_schema(
                merged_properties,
                name,
                property_schema,
                action,
                discriminator,
            );
        }
    }
    action_metadata.insert(
        action.to_string(),
        serde_json::json!({
            "allowed_fields": allowed_fields,
            "required_fields": required,
            "field_descriptions": field_descriptions,
        }),
    );
    Some(())
}

/// Build a compact one-line-per-action signature block from the accumulated
/// `x-proxima-actions` metadata: `- <action>: <required> (+ <optional>)`.
/// Optional fields are `allowed_fields` minus `required_fields`. Empty when no
/// action carries any field.
fn action_signature_block(
    action_values: &[serde_json::Value],
    action_metadata: &serde_json::Map<String, serde_json::Value>,
) -> String {
    use std::fmt::Write as _;
    let mut lines = Vec::new();
    for action_value in action_values {
        let Some(action) = action_value.as_str() else {
            continue;
        };
        let Some(meta) = action_metadata
            .get(action)
            .and_then(serde_json::Value::as_object)
        else {
            continue;
        };
        let field_list = |key: &str| -> Vec<String> {
            meta.get(key)
                .and_then(serde_json::Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default()
        };
        let required = field_list("required_fields");
        let optional: Vec<String> = field_list("allowed_fields")
            .into_iter()
            .filter(|field| !required.contains(field))
            .collect();
        let mut line = format!("- {action}: ");
        match (required.is_empty(), optional.is_empty()) {
            (true, true) => line.push_str("(no args)"),
            (true, false) => {
                write!(line, "(+ {})", optional.join(", ")).expect("write to String is infallible");
            }
            (false, true) => line.push_str(&required.join(", ")),
            (false, false) => {
                line.push_str(&required.join(", "));
                write!(line, " (+ {})", optional.join(", "))
                    .expect("write to String is infallible");
            }
        }
        lines.push(line);
    }
    lines.join("\n")
}

/// Top-level property names in `args_schema` that carry no non-empty
/// `description`. Used by tool registration to warn (never fail) on
/// under-documented MCP tool fields.
pub(crate) fn undescribed_property_names(args_schema: &serde_json::Value) -> Vec<String> {
    let Some(properties) = args_schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
    else {
        return Vec::new();
    };
    let mut out = properties
        .iter()
        .filter(|(_, schema)| {
            schema
                .get("description")
                .and_then(serde_json::Value::as_str)
                .is_none_or(|description| description.trim().is_empty())
        })
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    out.sort();
    out
}

/// Detect the internally-tagged discriminator KEY across a root `oneOf`'s
/// variants.
///
/// An internally-tagged enum (`#[serde(tag = "k")]`) emits one object variant
/// per case, each carrying the tag field as a string `const` whose value is the
/// (renamed) variant name. The discriminator is the property name that, across
/// *all* variants, is present with a string `const` and takes a *distinct*
/// value per variant. Returns `Some(key)` iff exactly one such key exists;
/// otherwise `None`, signalling the caller to leave the schema unflattened.
fn detect_discriminator_key(variants: &[serde_json::Value]) -> Option<String> {
    use std::collections::{BTreeMap, BTreeSet};

    // For each candidate property key, collect the string `const` value it
    // carries in each variant it appears in.
    let mut const_values: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for variant in variants {
        let properties = variant
            .get("properties")
            .and_then(serde_json::Value::as_object)?;
        for (name, property_schema) in properties {
            if let Some(value) = property_schema
                .get("const")
                .and_then(serde_json::Value::as_str)
            {
                const_values
                    .entry(name.clone())
                    .or_default()
                    .push(value.to_string());
            }
        }
    }

    let mut candidates = const_values.into_iter().filter(|(_, values)| {
        // Present in every variant, with a distinct value in each.
        values.len() == variants.len()
            && values.iter().collect::<BTreeSet<_>>().len() == values.len()
    });
    let key = candidates.next()?.0;
    if candidates.next().is_some() {
        // Ambiguous: more than one property qualifies as a discriminator.
        return None;
    }
    Some(key)
}

fn merge_property_schema(
    merged_properties: &mut serde_json::Map<String, serde_json::Value>,
    name: &str,
    property_schema: &serde_json::Value,
    action: &str,
    discriminator: &str,
) {
    let Some(existing) = merged_properties.get_mut(name) else {
        merged_properties.insert(name.to_string(), property_schema.clone());
        return;
    };
    if validation_shape(existing) == validation_shape(property_schema) {
        if existing != property_schema {
            neutralize_shared_property_description(existing, name, discriminator);
        }
        return;
    }
    panic!(
        "conflicting property `{name}` while flattening action `{action}`: {existing:#} vs {property_schema:#}"
    );
}

fn neutralize_shared_property_description(
    value: &mut serde_json::Value,
    name: &str,
    discriminator: &str,
) {
    if let serde_json::Value::Object(map) = value {
        for key in ["default", "title"] {
            map.remove(key);
        }
        map.insert(
            "description".to_string(),
            serde_json::Value::String(format!(
                "Shared dispatcher field `{name}`. Semantics and requiredness depend on `{discriminator}`; see `x-proxima-actions` or `proxima://tools` for {discriminator}-specific guidance."
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
            "MCP tool type `{}` leaves root schema combinator `{keyword}` after normalization. A dispatcher Args type must be an internally tagged enum (`#[serde(tag = \"...\")]`) whose variants each carry the tag as a distinct string `const`; adjacently/externally tagged enums, untagged enums, or otherwise heterogeneous variants cannot be made client-safe. Use such an enum or a plain struct Args type.",
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
    use serde::Deserialize;

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

    /// A dispatcher whose discriminator tag is `kind`, not `action` — the
    /// downstream-flavor shape (e.g. working-hero's query tool). The flattener
    /// must honor the actual serde tag rather than assuming `action`.
    #[derive(Deserialize, JsonSchema)]
    #[allow(dead_code)]
    #[serde(tag = "kind")]
    enum Demo {
        #[serde(rename = "a")]
        A { x: Option<String> },
        #[serde(rename = "b")]
        B {},
    }

    /// A `kind`-tagged dispatcher with a field (`shared`) present in more than
    /// one variant, so the flattener neutralizes its description. That guidance
    /// text must name the actual discriminator (`kind`), not a hardcoded
    /// `action`.
    #[derive(Deserialize, JsonSchema)]
    #[allow(dead_code)]
    #[serde(tag = "kind")]
    enum DemoShared {
        #[serde(rename = "left")]
        Left { shared: Option<String> },
        #[serde(rename = "right")]
        Right { shared: Option<String> },
    }

    #[derive(JsonSchema)]
    #[allow(dead_code)]
    struct PartiallyDescribed {
        #[schemars(description = "A described field.")]
        described: String,
        bare: String,
    }

    #[test]
    fn dispatcher_description_carries_per_action_signatures() {
        let schema = mcp_tool_schema::<Demo>();
        let description = schema
            .pointer("/properties/kind/description")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_else(|| panic!("kind discriminator description present: {schema:#}"));
        assert!(
            description.contains("Action signatures"),
            "dispatcher description must inline the signature block: {description}",
        );
        // Variant `a` carries one optional field `x`; variant `b` carries none.
        assert!(
            description.contains("- a: (+ x)"),
            "per-action signature must list optional fields: {description}",
        );
        assert!(
            description.contains("- b: (no args)"),
            "a variant with no fields must render `(no args)`: {description}",
        );
    }

    #[test]
    fn undescribed_property_names_flags_only_bare_fields() {
        let schema = mcp_tool_schema::<PartiallyDescribed>();
        assert_eq!(
            undescribed_property_names(&schema),
            vec!["bare".to_string()]
        );
    }

    #[test]
    fn shared_field_description_names_the_actual_discriminator() {
        let schema = mcp_tool_schema::<DemoShared>();
        let description = schema
            .pointer("/properties/shared/description")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_else(|| panic!("shared field description present: {schema:#}"));
        assert!(
            description.contains("depend on `kind`"),
            "shared-field guidance must name the actual discriminator `kind`: {description}",
        );
        assert!(
            !description.contains("`action`"),
            "shared-field guidance must not leak the hardcoded `action` discriminator: {description}",
        );
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

    #[test]
    fn non_action_discriminator_flattens_under_its_own_tag() {
        let schema = mcp_tool_schema::<Demo>();

        assert_eq!(
            schema.get("type").and_then(serde_json::Value::as_str),
            Some("object"),
            "kind-tagged dispatcher must have an object root: {schema:#}",
        );
        assert!(
            schema
                .get("properties")
                .is_some_and(serde_json::Value::is_object),
            "kind-tagged dispatcher must expose a top-level properties object: {schema:#}",
        );
        for combinator in ["oneOf", "anyOf", "allOf"] {
            assert!(
                schema.get(combinator).is_none(),
                "kind-tagged dispatcher must not leave a root {combinator}: {schema:#}",
            );
        }

        // The discriminator lives at `properties.kind`, NOT `properties.action`.
        assert!(
            schema.pointer("/properties/action").is_none(),
            "kind-tagged dispatcher must not invent an `action` discriminator: {schema:#}",
        );
        let mut kind_values = schema
            .pointer("/properties/kind/enum")
            .and_then(serde_json::Value::as_array)
            .unwrap_or_else(|| {
                panic!("discriminator must live at properties.kind.enum: {schema:#}")
            })
            .iter()
            .map(|value| value.as_str().expect("kind enum values are strings"))
            .collect::<Vec<_>>();
        kind_values.sort_unstable();
        assert_eq!(
            kind_values,
            ["a", "b"],
            "kind enum must carry the renamed variant values: {schema:#}",
        );

        // `required` names the detected discriminator, not `action`.
        let required = schema
            .pointer("/required")
            .and_then(serde_json::Value::as_array)
            .unwrap_or_else(|| panic!("flattened schema must declare required: {schema:#}"));
        assert!(
            required.iter().any(|item| item == "kind"),
            "flattened schema must require the `kind` discriminator: {schema:#}",
        );
        assert!(
            !required.iter().any(|item| item == "action"),
            "flattened schema must not require a phantom `action` field: {schema:#}",
        );

        // `x-proxima-actions` is keyed by the variant values, not by `action`.
        let actions = schema
            .get("x-proxima-actions")
            .and_then(serde_json::Value::as_object)
            .unwrap_or_else(|| {
                panic!("flattened schema must expose x-proxima-actions: {schema:#}")
            });
        assert!(
            actions.contains_key("a") && actions.contains_key("b"),
            "x-proxima-actions must be keyed by the kind values a/b: {schema:#}",
        );
        assert_eq!(
            actions.len(),
            2,
            "x-proxima-actions must hold one entry per variant: {schema:#}",
        );

        // The optional `x` field of variant `a` survives as a top-level property.
        assert!(
            schema.pointer("/properties/x").is_some(),
            "variant fields must be merged into top-level properties: {schema:#}",
        );
    }
}
