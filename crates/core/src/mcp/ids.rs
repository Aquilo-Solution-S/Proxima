use super::error::McpToolError;
use super::handles::{PrefixedUuidClass, parse_prefixed_uuid};

pub(super) fn parse_any_prefixed_memory_uuid(raw: &str) -> Result<uuid::Uuid, McpToolError> {
    let class = match raw.split_once(':').map(|(prefix, _)| prefix) {
        Some("F") => PrefixedUuidClass::Fact,
        Some("A") => PrefixedUuidClass::Abstraction,
        Some("P") => PrefixedUuidClass::Perspective,
        Some(prefix) => {
            return Err(McpToolError::InvalidInput(format!(
                "expected memory id prefix F, A, or P; got '{prefix}' in '{raw}'"
            )));
        }
        None => {
            return Err(McpToolError::InvalidInput(format!(
                "malformed memory id '{raw}': expected F:<uuid>, A:<uuid>, or P:<uuid>"
            )));
        }
    };
    parse_prefixed_uuid(raw, class).map_err(|e| McpToolError::InvalidInput(e.to_string()))
}

pub(super) fn parse_flavor_prefixed_uuid(raw: &str) -> Result<uuid::Uuid, McpToolError> {
    let Some((prefix, uuid_part)) = raw.split_once(':') else {
        return Err(McpToolError::InvalidInput(format!(
            "malformed flavor object id '{raw}': expected <prefix>:<uuid>"
        )));
    };
    let mut chars = prefix.chars();
    if !matches!(chars.next(), Some(c) if c.is_ascii_uppercase()) || chars.next().is_some() {
        return Err(McpToolError::InvalidInput(format!(
            "malformed flavor object id '{raw}': prefix must be one ASCII uppercase letter"
        )));
    }
    uuid_part
        .parse::<uuid::Uuid>()
        .map_err(|e| McpToolError::InvalidInput(format!("not a uuid: {e}")))
}
