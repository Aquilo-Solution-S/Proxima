//! `core/list_schemas` — project `FlavorRegistryFrozen` schemas.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mcp::{McpToolCtx, McpToolError};
use crate::verbs::schema::PayloadKind;

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ListSchemasArgs {
    /// Optional filter. One of "Fact", "Abstraction", "Perspective",
    /// "Goal", "`CitedObject`", "`CitationMapping`"
    /// (case-insensitive). Omit to return all kinds.
    #[serde(default)]
    pub kind: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SchemaItem {
    pub schema_id: String,
    pub schema_version: u32,
    pub kind: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListSchemasOutput {
    pub schemas: Vec<SchemaItem>,
}

// Case-insensitive: agents reading `kind` values back from output see
// `CitedObject` but frequently send `citedobject`/`FACT`; casing is not
// signal, so it must not turn a valid filter into an error.
fn parse_kind(s: &str) -> Option<PayloadKind> {
    match s.to_ascii_lowercase().as_str() {
        "fact" => Some(PayloadKind::Fact),
        "abstraction" => Some(PayloadKind::Abstraction),
        "perspective" => Some(PayloadKind::Perspective),
        "goal" => Some(PayloadKind::Goal),
        "citedobject" => Some(PayloadKind::CitedObject),
        "citationmapping" => Some(PayloadKind::CitationMapping),
        _ => None,
    }
}

fn kind_str(k: PayloadKind) -> &'static str {
    match k {
        PayloadKind::Fact => "Fact",
        PayloadKind::Abstraction => "Abstraction",
        PayloadKind::Perspective => "Perspective",
        PayloadKind::Goal => "Goal",
        PayloadKind::CitedObject => "CitedObject",
        PayloadKind::CitationMapping => "CitationMapping",
    }
}

#[allow(clippy::unused_async)]
/// # Errors
///
/// Returns invalid kind filters.
pub async fn list_schemas(
    ctx: McpToolCtx,
    args: ListSchemasArgs,
) -> Result<ListSchemasOutput, McpToolError> {
    // Reject an unknown `kind` rather than silently returning all
    // schemas (a typo like "Facts" must not look successful).
    let filter = match args.kind.as_deref() {
        Some(raw) => Some(parse_kind(raw).ok_or_else(|| {
            McpToolError::InvalidInput(format!(
                "unknown kind '{raw}'; expected one of: Fact, Abstraction, \
                         Perspective, Goal, CitedObject, CitationMapping"
            ))
        })?),
        None => None,
    };
    let schemas = ctx
        .registry
        .list()
        .into_iter()
        .filter(|info| filter.is_none_or(|k| info.kind == k))
        .map(|info| SchemaItem {
            schema_id: info.schema_id.as_str().to_string(),
            schema_version: info.schema_version.into_inner(),
            kind: kind_str(info.kind).to_string(),
        })
        .collect();
    Ok(ListSchemasOutput { schemas })
}

#[cfg(test)]
mod tests {
    use super::parse_kind;
    use crate::verbs::schema::PayloadKind;

    #[test]
    fn kind_filter_is_case_insensitive() {
        for raw in ["Fact", "fact", "FACT"] {
            assert_eq!(parse_kind(raw), Some(PayloadKind::Fact), "{raw}");
        }
        for raw in ["CitedObject", "citedobject", "CITEDOBJECT"] {
            assert_eq!(parse_kind(raw), Some(PayloadKind::CitedObject), "{raw}");
        }
    }

    #[test]
    fn unknown_kind_still_fails_closed() {
        // Case-insensitivity must not soften the typo guard.
        assert_eq!(parse_kind("Facts"), None);
        assert_eq!(parse_kind(""), None);
    }
}
