//! Emit the `core/personality_config_changed_v1` Fact memory after a
//! successful MCP-CRUD mutation.
//!
//! Provenance:
//! - Wake-token caller: the MCP wake-context populates
//!   `ctx.caller_self_perspective` from the firing personality's Root
//!   Perspective Memory id, and `ctx.master_token_id` is `None`. The
//!   audit Fact carries the wake personality's instance id.
//! - Master-token caller: the MCP server's `call_tool` ensure step
//!   populates `ctx.caller_self_perspective` from the per-token
//!   shell-author personality minted by
//!   `Storage::ensure_master_token_personality`, and
//!   `ctx.master_token_id` is `Some`. The audit Fact carries the
//!   per-token shell-author instance id.
//!
//! Emit failures are non-fatal: the verb already succeeded.

use time::OffsetDateTime;

use crate::mcp::McpToolCtx;
use crate::mcp::core_tools::payload::{
    PersonalityConfigChangeSnapshot, PersonalityConfigChangedCaller,
    PersonalityConfigChangedSubject, PersonalityConfigChangedV1, PersonalityConfigChangedVerb,
};
use crate::verbs::event_ingest::{Citation, CitationMappingHint, CitedObjectHint, EventDraft};
use crate::{FactPayload, SchemaId, SchemaVersion, SourceBatchId, SourceId, canonical_json_bytes};

/// Outcome of an audit-emit attempt. Tools surface `Failed` as a
/// non-fatal warning attached to their successful response (the verb
/// already landed; we don't retry).
#[derive(Debug, Clone)]
pub enum AuditEmit {
    Ok,
    Failed { reason: String },
}

pub async fn emit_personality_config_changed(
    ctx: &McpToolCtx,
    verb: PersonalityConfigChangedVerb,
    subject: PersonalityConfigChangedSubject,
    before: Option<PersonalityConfigChangeSnapshot>,
    after: Option<PersonalityConfigChangeSnapshot>,
) -> AuditEmit {
    let caller = match resolve_caller(ctx).await {
        Ok(c) => c,
        Err(reason) => return AuditEmit::Failed { reason },
    };
    let payload = PersonalityConfigChangedV1 {
        verb,
        before,
        after,
        subject,
        caller,
    };
    match write_fact(ctx, &payload).await {
        Ok(()) => AuditEmit::Ok,
        Err(reason) => AuditEmit::Failed { reason },
    }
}

async fn resolve_caller(ctx: &McpToolCtx) -> Result<PersonalityConfigChangedCaller, String> {
    let storage = ctx
        .storage()
        .ok_or_else(|| "engine storage unavailable".to_string())?;

    let self_id = ctx
        .caller_self_perspective
        .ok_or_else(|| "caller_self_perspective missing for audit emit".to_string())?;

    // Both wake-token and master-token calls now carry a Self-Perspective;
    // the MCP server's ensure-on-call step populates it for master-token
    // requests. Find the personality whose current root perspective
    // matches.
    let instances = storage
        .list_personality_instances(&ctx.owner, false)
        .await
        .map_err(|e| e.to_string())?;
    let instance_id = instances
        .into_iter()
        .find(|row| row.current_root_perspective_memory_id == self_id)
        .map(|row| row.personality_instance_id.into_inner())
        .ok_or_else(|| format!("no personality matches caller_self_perspective {self_id:?}"))?;

    Ok(if ctx.master_token_id.is_some() {
        PersonalityConfigChangedCaller::MasterToken {
            personality_instance_id: instance_id,
        }
    } else {
        PersonalityConfigChangedCaller::WakePersonality {
            personality_instance_id: instance_id,
        }
    })
}

async fn write_fact(ctx: &McpToolCtx, payload: &PersonalityConfigChangedV1) -> Result<(), String> {
    let storage = ctx
        .storage()
        .ok_or_else(|| "engine storage unavailable".to_string())?;

    let payload_value = serde_json::to_value(payload).map_err(|e| e.to_string())?;
    let payload_bytes = canonical_json_bytes(&payload_value);
    let body_hash = blake3::hash(&payload_bytes);
    let observed_at = OffsetDateTime::now_utc();

    let draft = EventDraft {
        source_id: SourceId::new("core/mcp-crud"),
        source_batch_id: SourceBatchId::new(uuid::Uuid::now_v7()),
        principal: ctx.owner.principal.clone(),
        org_id: Some(ctx.owner.org_id),
        author_personality_instance_id: None,
        schema_id: SchemaId::new(PersonalityConfigChangedV1::SCHEMA_ID.into()),
        schema_version: SchemaVersion::new(PersonalityConfigChangedV1::SCHEMA_VERSION),
        payload: payload_bytes,
        rendered_text: None,
        observed_at,
        occurred_at: observed_at,
        citation: Some(Citation {
            object: CitedObjectHint {
                schema_id: SchemaId::new("core/personality_config_changed_object_v1".into()),
                schema_version: SchemaVersion::new(1),
                content_hash: *body_hash.as_bytes(),
            },
            mapping: CitationMappingHint {
                schema_id: SchemaId::new("core/personality_config_changed_whole_v1".into()),
                schema_version: SchemaVersion::new(1),
            },
        }),
    };
    storage
        .ingest_event_atomic(&draft)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::HandleTable;
    use crate::mcp::OutputMode;
    use crate::{
        AuthPath, AuthzContext, FlavorRegistry, McpAuthorContext, OrgId, Owner, Principal, UserId,
    };
    use std::sync::Arc;

    fn fake_owner() -> Owner {
        Owner {
            principal: Principal::User(UserId::new(uuid::Uuid::now_v7())),
            org_id: OrgId::new(uuid::Uuid::now_v7()),
        }
    }

    #[tokio::test]
    async fn resolve_caller_returns_failed_when_no_storage() {
        let owner = fake_owner();
        let ctx = McpToolCtx {
            pool: sqlx::PgPool::connect_lazy("postgres://placeholder/db").expect("lazy connect"),
            owner: owner.clone(),
            authz: AuthzContext::single_owner(&owner, AuthPath::System),
            handles: Some(Arc::new(HandleTable::new())),
            mode: OutputMode::Handles,
            registry: Arc::new(FlavorRegistry::new().freeze()),
            author: McpAuthorContext {
                model_id: "test".into(),
                client_name: "test".into(),
                client_version: "0".into(),
                personality_instance_id: None,
                caller_self_perspective: None,
            },
            caller_self_perspective: None,
            master_token_id: None,
            engine: None,
        };
        let outcome = emit_personality_config_changed(
            &ctx,
            PersonalityConfigChangedVerb::Instantiate,
            PersonalityConfigChangedSubject::Personality(uuid::Uuid::now_v7()),
            None,
            Some(PersonalityConfigChangeSnapshot::Personality {
                personality_instance_id: None,
                display_name: None,
                status: None,
                wake_entry_count: None,
            }),
        )
        .await;
        match outcome {
            AuditEmit::Failed { reason } => {
                assert!(reason.contains("storage unavailable"), "got {reason:?}");
            }
            AuditEmit::Ok => panic!("expected Failed without storage"),
        }
    }

    #[tokio::test]
    async fn resolve_caller_returns_failed_when_self_perspective_missing() {
        // Build a ctx with engine wired but no caller_self_perspective —
        // the new resolver fails fast since the MCP server is contract-bound
        // to populate this field.
        use crate::Engine;
        use crate::verbs::query::MemoryStore;

        let owner = fake_owner();
        let engine = std::sync::Arc::new(Engine::new(
            FlavorRegistry::new().freeze(),
            MemoryStore::new(),
        ));
        let ctx = McpToolCtx {
            pool: sqlx::PgPool::connect_lazy("postgres://placeholder/db").expect("lazy connect"),
            owner: owner.clone(),
            authz: AuthzContext::single_owner(&owner, AuthPath::System),
            handles: Some(Arc::new(HandleTable::new())),
            mode: OutputMode::Handles,
            registry: Arc::new(FlavorRegistry::new().freeze()),
            author: McpAuthorContext {
                model_id: "t".into(),
                client_name: "t".into(),
                client_version: "0".into(),
                personality_instance_id: None,
                caller_self_perspective: None,
            },
            caller_self_perspective: None,
            master_token_id: None,
            engine: Some(engine),
        };
        let outcome = emit_personality_config_changed(
            &ctx,
            PersonalityConfigChangedVerb::Instantiate,
            PersonalityConfigChangedSubject::Personality(uuid::Uuid::now_v7()),
            None,
            Some(PersonalityConfigChangeSnapshot::Personality {
                personality_instance_id: None,
                display_name: None,
                status: None,
                wake_entry_count: None,
            }),
        )
        .await;
        match outcome {
            AuditEmit::Failed { reason } => assert!(
                reason.contains("caller_self_perspective missing"),
                "got {reason:?}"
            ),
            AuditEmit::Ok => panic!("expected Failed without caller_self_perspective"),
        }
    }
}
