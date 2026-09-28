//! The listenable-Fact catalog as an `AsyncAPI` 3.0.0 document (issue #388,
//! docs/18 §`AsyncAPI` catalog).
//!
//! Offline: [`asyncapi_document`] reads a frozen registry and nothing else —
//! no database, broker or boot. A host calls it from a build step or a test
//! on the same bundles it boots, commits the output, and has CI check that
//! the file is current.
//!
//! | Part | Content |
//! |---|---|
//! | `channels` | one per listenable schema id, keyed by its [`type_token`] |
//! | `operations` | one `send` per channel, same key |
//! | `components.messages` | one per registered (schema id, version), keyed `<type_token>.v<version>` |
//! | message `payload` | the structured-mode `CloudEvents` envelope capture seals, with the registered `json_schema()` as `data` |
//!
//! `servers` is omitted: broker topology belongs to the deployment
//! (docs/18 §Deployment topology).

use proxima_core::publication::{
    CLOUDEVENTS_DATA_CONTENT_TYPE, CLOUDEVENTS_SPEC_VERSION, MAX_EXTENSION_NAME_CHARS,
    MAX_EXTENSION_VALUE_BYTES, data_schema_uri,
};
use proxima_core::verbs::schema::SchemaInfo;
use proxima_core::{FlavorRegistryFrozen, OwnerRefKind};
use serde_json::{Map, Value, json};

use crate::config::{subject_from_parts, type_token, validated_subject_prefix};
use crate::publisher::CONTENT_TYPE_CLOUDEVENTS;

/// The `AsyncAPI` version [`asyncapi_document`] emits.
pub const ASYNCAPI_VERSION: &str = "3.0.0";

/// Channel address parameter carrying the owner kind.
pub const OWNER_KIND_PARAMETER: &str = "ownerKind";

/// Channel address parameter carrying the owner id.
pub const OWNER_ID_PARAMETER: &str = "ownerId";

/// The host-supplied `info` object: what the document describes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsyncApiInfo {
    pub title: String,
    pub version: String,
}

impl AsyncApiInfo {
    #[must_use]
    pub fn new(title: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            version: version.into(),
        }
    }
}

/// Why [`asyncapi_document`] refused a registry.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AsyncApiError {
    #[error(
        "subject prefix {value:?} must be dot-separated tokens of [A-Za-z0-9_-] \
         with no `*` or `>` wildcards"
    )]
    InvalidSubjectPrefix { value: String },
    /// Unreachable through [`proxima_core::FlavorRegistry::try_freeze`],
    /// which refuses `ListenableWithoutSchema`; refused here rather than
    /// described as "anything".
    #[error("listenable schema {schema_id}/{schema_version} has no registered JSON Schema")]
    MissingJsonSchema {
        schema_id: String,
        schema_version: u32,
    },
    /// The schema moves into the document, so its local `$ref`s are rebased
    /// onto its new location. A `$id` makes JSON Schema resolve them against
    /// that id instead, so no one rebasing is right for both readers.
    #[error(
        "listenable schema {schema_id}/{schema_version} declares `$id` and a local `$ref`; \
         its references cannot be placed in the document"
    )]
    IdentifiedLocalReference {
        schema_id: String,
        schema_version: u32,
    },
}

/// Describe every listenable schema in `registry` as an `AsyncAPI` 3.0.0
/// document.
///
/// Channel addresses are the subjects the publisher sends on,
/// `<subject_prefix>.{ownerKind}.{ownerId}.<type_token>`, built by the same
/// formatter as [`crate::subject_for`]. Pass the prefix the publisher runs
/// under ([`crate::DEFAULT_SUBJECT_PREFIX`] unless the deployment sets
/// `PROXIMA_NATS_SUBJECT_PREFIX`).
///
/// Deterministic: channels, operations and messages are ordered by schema
/// id, then version, and the same registry yields byte-identical output.
/// Key order is insertion order because `proxima-core` builds `serde_json`
/// with `preserve_order`.
///
/// A registry with no listenable schema yields a valid document with no
/// channels. Non-listenable schemas never appear.
///
/// # Errors
///
/// [`AsyncApiError`]: an invalid subject prefix, a listenable schema with no
/// JSON Schema, or one whose local `$ref`s cannot be rebased.
///
/// # Example
///
/// ```
/// use proxima_outbox_nats::{AsyncApiInfo, DEFAULT_SUBJECT_PREFIX, asyncapi_document};
///
/// // A host registers the bundles it boots before freezing.
/// let registry = proxima_core::FlavorRegistry::new().try_freeze()?;
/// let info = AsyncApiInfo::new("acme-host", "1.0.0");
/// let document = asyncapi_document(&registry, &info, DEFAULT_SUBJECT_PREFIX)?;
/// assert_eq!(document["asyncapi"], "3.0.0");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn asyncapi_document(
    registry: &FlavorRegistryFrozen,
    info: &AsyncApiInfo,
    subject_prefix: &str,
) -> Result<Value, AsyncApiError> {
    let prefix = validated_subject_prefix(subject_prefix).map_err(|_| {
        AsyncApiError::InvalidSubjectPrefix {
            value: subject_prefix.to_owned(),
        }
    })?;
    let mut listenable: Vec<&SchemaInfo> = registry
        .schemas()
        .iter()
        .filter(|schema| schema.listenable)
        .collect();
    listenable.sort_by(|a, b| {
        (a.schema_id.as_str(), a.schema_version.into_inner())
            .cmp(&(b.schema_id.as_str(), b.schema_version.into_inner()))
    });

    let mut channels = Map::new();
    let mut operations = Map::new();
    let mut messages = Map::new();
    for versions in listenable.chunk_by(|a, b| a.schema_id == b.schema_id) {
        let Some(first) = versions.first() else {
            continue;
        };
        let schema_id = first.schema_id.as_str();
        let channel_key = type_token(schema_id);
        let mut channel_messages = Map::new();
        let mut operation_messages = Vec::new();
        for schema in versions {
            let version = schema.schema_version.into_inner();
            let message_key = format!("{channel_key}.v{version}");
            let registered = registry
                .payload_json_schema(&schema.schema_id, schema.schema_version, schema.kind)
                .ok_or_else(|| AsyncApiError::MissingJsonSchema {
                    schema_id: schema_id.to_owned(),
                    schema_version: version,
                })?;
            let data = placed_data_schema(registered, &message_key).map_err(|()| {
                AsyncApiError::IdentifiedLocalReference {
                    schema_id: schema_id.to_owned(),
                    schema_version: version,
                }
            })?;
            messages.insert(
                message_key.clone(),
                json!({
                    "title": format!("{schema_id} v{version}"),
                    "contentType": CONTENT_TYPE_CLOUDEVENTS,
                    "payload": envelope_schema(schema, &data),
                }),
            );
            channel_messages.insert(
                message_key.clone(),
                json!({ "$ref": format!("#/components/messages/{message_key}") }),
            );
            operation_messages.push(json!({
                "$ref": format!("#/channels/{channel_key}/messages/{message_key}"),
            }));
        }
        channels.insert(
            channel_key.clone(),
            channel(&prefix, schema_id, &channel_messages),
        );
        operations.insert(
            channel_key.clone(),
            json!({
                "action": "send",
                "channel": { "$ref": format!("#/channels/{channel_key}") },
                "messages": operation_messages,
            }),
        );
    }

    Ok(json!({
        "asyncapi": ASYNCAPI_VERSION,
        "info": { "title": info.title, "version": info.version },
        "channels": channels,
        "operations": operations,
        "components": { "messages": messages },
    }))
}

fn channel(prefix: &str, schema_id: &str, messages: &Map<String, Value>) -> Value {
    let address = subject_from_parts(
        prefix,
        &format!("{{{OWNER_KIND_PARAMETER}}}"),
        &format!("{{{OWNER_ID_PARAMETER}}}"),
        schema_id,
    );
    json!({
        "address": address,
        "title": schema_id,
        "messages": messages,
        "parameters": {
            OWNER_KIND_PARAMETER: {
                "enum": owner_kinds(),
                "description": "Kind of the Fact's owner.",
            },
            OWNER_ID_PARAMETER: {
                "description": "The owner's UUID, lowercase and hyphenated.",
            },
        },
    })
}

/// Every owner kind a subject can carry. The match is exhaustive, so a new
/// kind fails to compile here instead of going undescribed.
fn owner_kinds() -> Vec<&'static str> {
    [OwnerRefKind::Personal, OwnerRefKind::Group]
        .into_iter()
        .map(|kind| match kind {
            OwnerRefKind::Personal | OwnerRefKind::Group => kind.as_str(),
        })
        .collect()
}

/// The structured-mode `CloudEvent` capture seals for `schema`
/// (docs/18 §Publication Contract), with `data` as its payload schema.
fn envelope_schema(schema: &SchemaInfo, data: &Value) -> Value {
    json!({
        "type": "object",
        "required": [
            "specversion", "id", "source", "type", "datacontenttype",
            "dataschema", "time", "proximaowner", "data",
        ],
        "properties": {
            "specversion": { "const": CLOUDEVENTS_SPEC_VERSION },
            "id": { "type": "string", "pattern": "^F:" },
            "source": { "type": "string", "format": "uri-reference" },
            "type": { "const": schema.schema_id.as_str() },
            "datacontenttype": { "const": CLOUDEVENTS_DATA_CONTENT_TYPE },
            "dataschema": { "const": data_schema_uri(&schema.schema_id, schema.schema_version) },
            "time": { "type": "string", "format": "date-time" },
            "proximaowner": { "type": "string" },
            "proximamodel": { "type": "string" },
            "data": data,
        },
        // Host-bound extensions (docs/18 §Host-bound extension attributes):
        // the name rule and value types `PublicationExtensions` enforces at
        // binding. `subject` is the one core attribute not listed above;
        // `data_base64` already fails the pattern.
        "propertyNames": {
            "pattern": format!("^[a-z0-9]{{1,{MAX_EXTENSION_NAME_CHARS}}}$"),
            "not": { "enum": ["subject"] },
            "anyOf": [
                { "enum": ["proximaowner", "proximamodel"] },
                { "not": { "pattern": "^proxima" } },
            ],
        },
        "additionalProperties": {
            "anyOf": [
                {
                    "type": "string",
                    "minLength": 1,
                    // A byte limit, which bounds the character count.
                    "maxLength": MAX_EXTENSION_VALUE_BYTES,
                    "pattern": "^[^\\u0000-\\u001f\\u007f-\\u009f]*$",
                },
                { "type": "integer", "minimum": i32::MIN, "maximum": i32::MAX },
                { "type": "boolean" },
            ],
        },
    })
}

/// The registered schema, as it sits at
/// `#/components/messages/<message_key>/payload/properties/data`.
///
/// Its content is kept verbatim. The one change is to local references: a
/// `#…` `$ref` pointed at the schema's own root, which is now a subschema
/// of the document, so it is rebased onto that location. `AsyncAPI` tools
/// resolve references against the document; so does a JSON Schema
/// validator, as long as no `$id` opens a resource of its own, which is why
/// that combination is refused (`Err(())`).
fn placed_data_schema(registered: &Value, message_key: &str) -> Result<Value, ()> {
    let base = format!("#/components/messages/{message_key}/payload/properties/data");
    let mut placed = registered.clone();
    let mut declares_id = false;
    let mut rebased = false;
    for_each_schema(&mut placed, &mut |schema| {
        declares_id |= schema.contains_key("$id");
        if let Some(Value::String(reference)) = schema.get_mut("$ref")
            && let Some(pointer) = reference.strip_prefix('#')
            && (pointer.is_empty() || pointer.starts_with('/'))
        {
            *reference = format!("{base}{pointer}");
            rebased = true;
        }
    });
    if declares_id && rebased {
        return Err(());
    }
    Ok(placed)
}

/// Visit `schema` and every subschema in it, by keyword.
///
/// Walks schema positions only, so a `$ref` inside `const`, `enum`,
/// `default`, `examples` or an unknown keyword is data and stays as it is.
fn for_each_schema(schema: &mut Value, visit: &mut impl FnMut(&mut Map<String, Value>)) {
    let Value::Object(map) = schema else {
        return;
    };
    visit(map);
    for (keyword, value) in map.iter_mut() {
        match keyword.as_str() {
            "properties" | "patternProperties" | "$defs" | "definitions" | "dependentSchemas"
            | "dependencies" => {
                if let Value::Object(named) = value {
                    for subschema in named.values_mut() {
                        for_each_schema(subschema, visit);
                    }
                }
            }
            "items" | "prefixItems" | "allOf" | "anyOf" | "oneOf" => match value {
                Value::Array(subschemas) => {
                    for subschema in subschemas {
                        for_each_schema(subschema, visit);
                    }
                }
                subschema => for_each_schema(subschema, visit),
            },
            "additionalProperties"
            | "additionalItems"
            | "unevaluatedProperties"
            | "unevaluatedItems"
            | "contains"
            | "propertyNames"
            | "if"
            | "then"
            | "else"
            | "not"
            | "contentSchema" => for_each_schema(value, visit),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_local_reference_is_rebased_onto_the_schemas_place_in_the_document() {
        let registered = json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "properties": {
                "step": { "$ref": "#/$defs/Step" },
                "self": { "$ref": "#" },
                "remote": { "$ref": "https://example.test/other.json#/x" },
                "anchor": { "$ref": "#step" },
                "const": { "const": { "$ref": "#/not/a/reference" } },
            },
            "$defs": { "Step": { "items": [{ "$ref": "#/$defs/Leaf" }] }, "Leaf": {} },
        });
        let placed = placed_data_schema(&registered, "k.v1").expect("no $id");
        let base = "#/components/messages/k.v1/payload/properties/data";
        let properties = &placed["properties"];
        assert_eq!(properties["step"]["$ref"], format!("{base}/$defs/Step"));
        assert_eq!(properties["self"]["$ref"], base);
        assert_eq!(
            properties["remote"]["$ref"],
            "https://example.test/other.json#/x"
        );
        assert_eq!(properties["anchor"]["$ref"], "#step");
        assert_eq!(
            properties["const"]["const"]["$ref"], "#/not/a/reference",
            "a `const` value is data, not a schema"
        );
        assert_eq!(
            placed["$defs"]["Step"]["items"][0]["$ref"],
            format!("{base}/$defs/Leaf")
        );
        assert_eq!(placed["$schema"], registered["$schema"]);
    }

    #[test]
    fn a_schema_without_local_references_is_placed_verbatim() {
        let registered = json!({
            "$id": "https://example.test/build.json",
            "type": "object",
            "properties": { "remote": { "$ref": "https://example.test/other.json" } },
        });
        assert_eq!(
            placed_data_schema(&registered, "k.v1").expect("nothing to rebase"),
            registered
        );
    }

    #[test]
    fn an_identified_schema_with_a_local_reference_is_refused() {
        let registered = json!({
            "$id": "https://example.test/build.json",
            "properties": { "step": { "$ref": "#/$defs/Step" } },
            "$defs": { "Step": {} },
        });
        assert_eq!(placed_data_schema(&registered, "k.v1"), Err(()));
    }

    #[test]
    fn owner_kinds_are_the_subject_owner_segments() {
        assert_eq!(owner_kinds(), ["personal", "group"]);
    }
}
