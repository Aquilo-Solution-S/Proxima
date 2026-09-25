//! `core/list_schemas` — project `FlavorRegistryFrozen` schemas.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mcp::{McpToolCtx, McpToolError};
use crate::verbs::schema::PayloadKind;
use crate::{SchemaId, SchemaVersion};

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
    /// Whether an admitted Fact of this schema also captures a
    /// `CloudEvents` publication record in the same transaction (issue
    /// #305). Always `false` for the non-Fact kinds.
    pub listenable: bool,
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
            listenable: info.listenable,
        })
        .collect();
    Ok(ListSchemasOutput { schemas })
}

/// One registered payload type's JSON Schema, addressed by the same
/// `{schema_id}/{schema_version}` pair a published event carries in its
/// `CloudEvents` `dataschema`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct GetSchemaOutput {
    pub schema_id: String,
    pub schema_version: u32,
    pub kind: String,
    pub listenable: bool,
    /// The registered `json_schema()` of the payload type.
    pub json_schema: serde_json::Value,
}

/// Read one registered schema, backing `proxima://schema/{id}/{version}`.
///
/// A registered schema with no `json_schema()` — the opaque
/// cited-object/citation-mapping lane — is reported as missing rather
/// than as an empty document: there is nothing to validate against, and a
/// consumer resolving a `dataschema` must not mistake silence for a
/// permissive schema. A listenable schema can never land here, because
/// freeze refuses `LISTENABLE = true` without a schema.
///
/// # Errors
///
/// Returns [`McpToolError::NotFound`] when the schema is not registered at
/// that version or carries no JSON Schema.
#[allow(clippy::unused_async)]
pub async fn get_schema(
    ctx: McpToolCtx,
    schema_id: &str,
    schema_version: u32,
) -> Result<GetSchemaOutput, McpToolError> {
    let id = SchemaId::new(schema_id.to_string());
    let version = SchemaVersion::new(schema_version);
    let missing =
        || McpToolError::NotFound(format!("schema {schema_id}/{schema_version} not found"));
    let info = ctx.registry.lookup(&id, version).ok_or_else(missing)?;
    let json_schema = ctx
        .registry
        .payload_json_schema(&id, version, info.kind)
        .ok_or_else(missing)?
        .clone();
    Ok(GetSchemaOutput {
        schema_id: info.schema_id.as_str().to_string(),
        schema_version: info.schema_version.into_inner(),
        kind: kind_str(info.kind).to_string(),
        listenable: info.listenable,
        json_schema,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{get_schema, parse_kind};
    use crate::mcp::{McpAuthorContext, McpToolCtx, McpToolError};

    use crate::{AuthPath, AuthzContext, FlavorServices, OwnerRef, UserId};

    fn ctx_with_probes() -> McpToolCtx {
        let owner = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
        McpToolCtx {
            owner,
            authz: AuthzContext::single_owner(&owner, AuthPath::HostBearer),
            registry: Arc::new(crate::test_fixtures::probe_registry()),
            author: McpAuthorContext {
                model_id: "t".into(),
                trusted_model_id: None,
                client_name: "t".into(),
                client_version: "0".into(),
                caller_self_perspective: None,
            },
            caller_self_perspective: None,
            services: FlavorServices::default(),
            engine: None,
        }
    }

    #[tokio::test]
    async fn an_unregistered_schema_version_is_not_found() {
        let err = get_schema(ctx_with_probes(), "probe/listenable-v1", 7)
            .await
            .expect_err("version 7 is not registered");
        assert!(matches!(err, McpToolError::NotFound(_)), "{err}");
    }

    #[test]
    fn unknown_kind_still_fails_closed() {
        // Case-insensitivity must not soften the typo guard.
        assert_eq!(parse_kind("Facts"), None);
        assert_eq!(parse_kind(""), None);
    }
}
