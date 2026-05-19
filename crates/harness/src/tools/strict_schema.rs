//! Strict JSON-schema projection for provider-facing function tools.

use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrictToolSchema {
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StrictSchemaError {
    #[error("unbounded JSON schema at {pointer}")]
    UnboundedJson { pointer: String },
    #[error("unsupported schema at {pointer}: {reason}")]
    UnsupportedSchema { pointer: String, reason: String },
}

impl StrictToolSchema {
    /// Normalize a JSON Schema into the strict function-tool subset.
    ///
    /// The root must be an object. Nested object schemas are closed and
    /// every declared property is required.
    ///
    /// # Errors
    ///
    /// Returns [`StrictSchemaError`] when the schema contains an unbounded
    /// JSON hole or a shape the strict provider projection cannot express.
    pub fn from_schema(schema: &Value) -> Result<Self, StrictSchemaError> {
        let value = normalize_node(schema, "")?;
        require_root_object(&value)?;
        Ok(Self { value })
    }
}

fn normalize_node(schema: &Value, pointer: &str) -> Result<Value, StrictSchemaError> {
    match schema {
        Value::Bool(_) | Value::Null => Err(StrictSchemaError::UnboundedJson {
            pointer: pointer.to_string(),
        }),
        Value::Array(items) => items
            .iter()
            .enumerate()
            .map(|(index, item)| normalize_node(item, &join_index(pointer, index)))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        Value::Object(object) => normalize_object(object, pointer),
        Value::String(_) | Value::Number(_) => Ok(schema.clone()),
    }
}

fn normalize_object(
    object: &Map<String, Value>,
    pointer: &str,
) -> Result<Value, StrictSchemaError> {
    if object.is_empty() {
        return Err(StrictSchemaError::UnboundedJson {
            pointer: pointer.to_string(),
        });
    }
    if matches!(object.get("additionalProperties"), Some(Value::Bool(true))) {
        return Err(StrictSchemaError::UnboundedJson {
            pointer: join_key(pointer, "additionalProperties"),
        });
    }

    let mut normalized = Map::new();
    for (key, value) in object {
        if key == "$schema" || key == "default" {
            continue;
        }
        if key == "additionalProperties" && matches!(value, Value::Bool(false)) {
            normalized.insert(key.clone(), Value::Bool(false));
            continue;
        }
        normalized.insert(key.clone(), normalize_node(value, &join_key(pointer, key))?);
    }

    if is_object_schema(&normalized) {
        normalized
            .entry("type".to_string())
            .or_insert_with(|| Value::String("object".to_string()));
        normalized.insert("additionalProperties".to_string(), Value::Bool(false));
        if let Some(Value::Object(properties)) = normalized.get("properties") {
            let required = properties.keys().cloned().map(Value::String).collect();
            normalized.insert("required".to_string(), Value::Array(required));
        }
    }

    Ok(Value::Object(normalized))
}

fn require_root_object(schema: &Value) -> Result<(), StrictSchemaError> {
    match schema.get("type") {
        Some(Value::String(value)) if value == "object" => Ok(()),
        Some(Value::Array(values))
            if values.iter().any(|value| value.as_str() == Some("object")) =>
        {
            Ok(())
        }
        Some(value) => Err(StrictSchemaError::UnsupportedSchema {
            pointer: String::new(),
            reason: format!("root tool schema must be an object, got type {value}"),
        }),
        None => Err(StrictSchemaError::UnsupportedSchema {
            pointer: String::new(),
            reason: "root tool schema must declare type object".to_string(),
        }),
    }
}

fn is_object_schema(object: &Map<String, Value>) -> bool {
    if object.contains_key("properties") {
        return true;
    }
    match object.get("type") {
        Some(Value::String(value)) => value == "object",
        Some(Value::Array(values)) => values.iter().any(|value| value.as_str() == Some("object")),
        _ => false,
    }
}

fn join_key(pointer: &str, key: &str) -> String {
    let escaped = key.replace('~', "~0").replace('/', "~1");
    if pointer.is_empty() {
        format!("/{escaped}")
    } else {
        format!("{pointer}/{escaped}")
    }
}

fn join_index(pointer: &str, index: usize) -> String {
    if pointer.is_empty() {
        format!("/{index}")
    } else {
        format!("{pointer}/{index}")
    }
}
