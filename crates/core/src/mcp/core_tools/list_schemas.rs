//! `core/list_schemas` — project `SpiiceRegistryFrozen` schemas.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::mcp::{McpToolCtx, McpToolError};
use crate::verbs::schema::{PayloadKind, SchemaInfo, SpiiceRegistryFrozen};
use crate::{SchemaId, SchemaVersion};

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ListSchemasArgs {
    /// Optional filter. One of "Fact", "Abstraction", "Perspective",
    /// "Goal", "`CitedObject`" (case-insensitive). Omit to return all kinds.
    #[serde(default)]
    pub kind: Option<PayloadKind>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SchemaItem {
    pub schema_id: String,
    pub schema_version: u32,
    pub kind: PayloadKind,
    /// Whether an admitted Fact of this schema also captures a
    /// `CloudEvents` publication record in the same transaction (issue
    /// #305). Always `false` for the non-Fact kinds.
    pub listenable: bool,
    /// Consumer-facing meaning of this target after an explicit evolution.
    pub change: Option<String>,
    /// Required action for consumers of this target schema.
    pub consumer_action: Option<String>,
}

pub(crate) fn schema_item(registry: &SpiiceRegistryFrozen, info: &SchemaInfo) -> SchemaItem {
    let metadata = registry.evolution_metadata(&info.schema_id, info.schema_version, info.kind);
    SchemaItem {
        schema_id: info.schema_id.as_str().to_string(),
        schema_version: info.schema_version.into_inner(),
        kind: info.kind,
        listenable: info.listenable,
        change: metadata.map(|(change, _)| change.to_owned()),
        consumer_action: metadata.map(|(_, action)| action.to_owned()),
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ListSchemasOutput {
    pub schemas: Vec<SchemaItem>,
}

#[allow(clippy::unused_async)]
/// # Errors
///
/// Returns invalid kind filters.
pub async fn list_schemas(
    ctx: McpToolCtx,
    args: ListSchemasArgs,
) -> Result<ListSchemasOutput, McpToolError> {
    let schemas = ctx
        .registry
        .list()
        .into_iter()
        .filter(|info| args.kind.is_none_or(|k| info.kind == k))
        .map(|info| schema_item(&ctx.registry, &info))
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
    pub kind: PayloadKind,
    pub listenable: bool,
    pub change: Option<String>,
    pub consumer_action: Option<String>,
    /// The registered `json_schema()` of the payload type.
    pub json_schema: serde_json::Value,
}

/// Read one registered schema, backing `proxiima://schema/{id}/{version}`.
///
/// A registered schema with no `json_schema()` — the opaque
/// cited-object lane — is reported as missing rather
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
    let metadata = ctx
        .registry
        .evolution_metadata(&info.schema_id, info.schema_version, info.kind);
    Ok(GetSchemaOutput {
        schema_id: info.schema_id.as_str().to_string(),
        schema_version: info.schema_version.into_inner(),
        kind: info.kind,
        listenable: info.listenable,
        change: metadata.map(|(change, _)| change.to_owned()),
        consumer_action: metadata.map(|(_, action)| action.to_owned()),
        json_schema,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::get_schema;
    use crate::mcp::{McpAuthorContext, McpToolCtx, McpToolError};

    use crate::{AuthPath, AuthzContext, OwnerId, SpiiceServices, UserId};

    fn ctx_with_probes() -> McpToolCtx {
        let owner = OwnerId::from(UserId::new(uuid::Uuid::now_v7()));
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
            services: SpiiceServices::default(),
            engine: None,
            judged: proxiima_core::mcp::JudgedKey::default(),
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
        assert!(
            "Facts"
                .parse::<crate::verbs::schema::PayloadKind>()
                .is_err()
        );
        assert!("".parse::<crate::verbs::schema::PayloadKind>().is_err());
        assert_eq!(
            "citedobject"
                .parse::<crate::verbs::schema::PayloadKind>()
                .ok(),
            Some(crate::verbs::schema::PayloadKind::CitedObject)
        );
    }
}
