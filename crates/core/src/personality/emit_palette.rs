//! Helpers for broad and schema-scoped substrate emit palette ids.

use crate::verbs::schema::PayloadKind;

pub const EMIT_ABSTRACTION_TOOL_ID: &str = "core/emit_abstraction";
pub const EMIT_PERSPECTIVE_TOOL_ID: &str = "core/emit_perspective";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedEmitToolId {
    pub base_tool_id: &'static str,
    pub schema_id: String,
    pub schema_version: u32,
    pub kind: PayloadKind,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{reason}")]
pub struct ScopedEmitToolIdError {
    pub tool_id: String,
    pub reason: String,
}

/// Parse a schema-scoped emit palette id; `Ok(None)` means the id is
/// not an emit-scoped id at all.
///
/// # Errors
///
/// Returns `ScopedEmitToolIdError` when an emit-prefixed id is malformed:
/// missing the `::<schema_id>::v<version>` shape, empty schema id, or a
/// non-integer version.
pub fn parse_scoped_emit_tool_id(
    tool_id: &str,
) -> Result<Option<ScopedEmitToolId>, ScopedEmitToolIdError> {
    for (base_tool_id, kind) in [
        (EMIT_ABSTRACTION_TOOL_ID, PayloadKind::Abstraction),
        (EMIT_PERSPECTIVE_TOOL_ID, PayloadKind::Perspective),
    ] {
        let prefix = format!("{base_tool_id}::");
        if !tool_id.starts_with(&prefix) {
            continue;
        }
        let rest = &tool_id[prefix.len()..];
        let Some((schema_id, version)) = rest.rsplit_once("::v") else {
            return Err(invalid_scoped_emit_tool_id(
                tool_id,
                "expected <base>::<schema_id>::v<schema_version>",
            ));
        };
        if schema_id.is_empty() {
            return Err(invalid_scoped_emit_tool_id(
                tool_id,
                "schema_id must be non-empty",
            ));
        }
        let schema_version = version.parse::<u32>().map_err(|_| {
            invalid_scoped_emit_tool_id(tool_id, "schema_version must be an unsigned integer")
        })?;
        return Ok(Some(ScopedEmitToolId {
            base_tool_id,
            schema_id: schema_id.to_string(),
            schema_version,
            kind,
        }));
    }
    Ok(None)
}

#[must_use]
pub fn scoped_emit_tool_id(base_tool_id: &str, schema_id: &str, schema_version: u32) -> String {
    format!("{base_tool_id}::{schema_id}::v{schema_version}")
}

#[must_use]
pub fn broad_emit_kind(tool_id: &str) -> Option<PayloadKind> {
    match tool_id {
        EMIT_ABSTRACTION_TOOL_ID => Some(PayloadKind::Abstraction),
        EMIT_PERSPECTIVE_TOOL_ID => Some(PayloadKind::Perspective),
        _ => None,
    }
}

#[must_use]
pub fn palette_authorizes_internal_tool(palette: &[String], internal_tool_id: &str) -> bool {
    palette.iter().any(|tool_id| {
        tool_id == internal_tool_id
            || parse_scoped_emit_tool_id(tool_id)
                .ok()
                .flatten()
                .is_some_and(|scoped| scoped.base_tool_id == internal_tool_id)
    })
}

fn invalid_scoped_emit_tool_id(tool_id: &str, reason: &str) -> ScopedEmitToolIdError {
    ScopedEmitToolIdError {
        tool_id: tool_id.to_string(),
        reason: reason.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_schema_scoped_emit_tool_id() {
        let parsed =
            parse_scoped_emit_tool_id("core/emit_abstraction::core/agent-derivation-v1::v1")
                .expect("valid")
                .expect("scoped");

        assert_eq!(parsed.base_tool_id, EMIT_ABSTRACTION_TOOL_ID);
        assert_eq!(parsed.schema_id, "core/agent-derivation-v1");
        assert_eq!(parsed.schema_version, 1);
        assert_eq!(parsed.kind, PayloadKind::Abstraction);
    }

    #[test]
    fn non_emit_tool_is_not_scoped_emit() {
        assert!(
            parse_scoped_emit_tool_id("core/fetch_memory")
                .expect("valid non emit")
                .is_none()
        );
    }

    #[test]
    fn malformed_scoped_emit_is_rejected() {
        let err =
            parse_scoped_emit_tool_id("core/emit_abstraction::test/schema").expect_err("malformed");

        assert!(err.reason.contains("expected"));
    }
}
