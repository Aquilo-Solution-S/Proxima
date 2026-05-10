//! Emit the `core/personality_config_changed_v1` Fact memory after a
//! successful MCP-CRUD mutation.
//!
//! Provenance:
//! - Wake-token caller: `ctx.caller_self_perspective` points at the
//!   calling personality's Root Perspective Memory; we look up the
//!   personality whose `current_root_perspective_memory_id == that id`.
//! - Master-token caller: a substrate-shipped `proxima/shell-author`
//!   personality, materialised lazily via
//!   `Storage::ensure_shell_author_personality(owner)`.
//!
//! Emit failures are non-fatal: the verb already succeeded.

use time::OffsetDateTime;

use crate::mcp::McpToolCtx;
use crate::mcp::core_tools::payload::{
    PersonalityConfigChangedCaller, PersonalityConfigChangedSubject,
    PersonalityConfigChangedV1, PersonalityConfigChangedVerb,
};
use crate::verbs::event_ingest::{CitationMappingHint, CitedObjectHint, EventDraft};
use crate::{FactPayload, SchemaId, SchemaVersion, SourceBatchId, SourceId};

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
    before: Option<serde_json::Value>,
    after: Option<serde_json::Value>,
) -> AuditEmit {
    let caller = match resolve_caller(ctx).await {
        Ok(c) => c,
        Err(reason) => return AuditEmit::Failed { reason },
    };
    let payload = PersonalityConfigChangedV1 {
        verb,
        subject,
        before,
        after,
        caller,
    };
    match write_fact(ctx, &payload).await {
        Ok(()) => AuditEmit::Ok,
        Err(reason) => AuditEmit::Failed { reason },
    }
}

async fn resolve_caller(
    ctx: &McpToolCtx,
) -> Result<PersonalityConfigChangedCaller, String> {
    let storage = ctx
        .storage()
        .ok_or_else(|| "engine storage unavailable".to_string())?;
    if let Some(self_id) = ctx.caller_self_perspective {
        // Wake-token: caller_self_perspective is the calling
        // personality's Root Perspective Memory id. Find the personality
        // whose current_root_perspective_memory_id matches.
        let instances = storage
            .list_personality_instances(&ctx.owner, false)
            .await
            .map_err(|e| e.to_string())?;
        let id = instances
            .into_iter()
            .find(|row| row.current_root_perspective_memory_id == self_id)
            .map(|row| row.personality_instance_id.into_inner())
            .ok_or_else(|| {
                format!("no personality matches caller_self_perspective {self_id:?}")
            })?;
        Ok(PersonalityConfigChangedCaller::WakePersonality {
            personality_instance_id: id,
        })
    } else {
        // Master-token caller branch is rewritten in Task 7 to read
        // from ctx.master_token_id; the placeholder here keeps the
        // workspace compiling between Task 2 and Task 7.
        Err("master-token audit caller resolution moved to Task 7".to_string())
    }
}

async fn write_fact(
    ctx: &McpToolCtx,
    payload: &PersonalityConfigChangedV1,
) -> Result<(), String> {
    let storage = ctx
        .storage()
        .ok_or_else(|| "engine storage unavailable".to_string())?;

    let mut payload_bytes = Vec::new();
    ciborium::ser::into_writer(&payload, &mut payload_bytes).map_err(|e| e.to_string())?;
    let body_hash = blake3::hash(&payload_bytes);
    let observed_at = OffsetDateTime::now_utc();

    let draft = EventDraft {
        source_id: SourceId::new("core/mcp-crud"),
        source_batch_id: SourceBatchId::new(uuid::Uuid::now_v7()),
        owner: ctx.owner.clone(),
        schema_id: SchemaId::new(PersonalityConfigChangedV1::SCHEMA_ID.into()),
        schema_version: SchemaVersion::new(PersonalityConfigChangedV1::SCHEMA_VERSION),
        payload: payload_bytes,
        observed_at,
        occurred_at: observed_at,
        cited_object: CitedObjectHint {
            schema_id: SchemaId::new("core/personality_config_changed_object_v1".into()),
            schema_version: SchemaVersion::new(1),
            content_hash: *body_hash.as_bytes(),
        },
        citation_mapping: CitationMappingHint {
            schema_id: SchemaId::new("core/personality_config_changed_whole_v1".into()),
            schema_version: SchemaVersion::new(1),
        },
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
    use crate::{
        FlavorRegistry, McpAuthorContext, OrgId, Owner, Principal, UserId,
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
        let ctx = McpToolCtx {
            pool: sqlx::PgPool::connect_lazy("postgres://placeholder/db")
                .expect("lazy connect"),
            owner: fake_owner(),
            handles: Arc::new(HandleTable::new()),
            registry: Arc::new(FlavorRegistry::new().freeze()),
            author: McpAuthorContext {
                model_id: "test".into(),
                client_name: "test".into(),
                client_version: "0".into(),
                caller_self_perspective: None,
            },
            caller_self_perspective: None,
            engine: None,
        };
        let outcome = emit_personality_config_changed(
            &ctx,
            PersonalityConfigChangedVerb::Instantiate,
            PersonalityConfigChangedSubject::Personality(uuid::Uuid::now_v7()),
            None,
            Some(serde_json::json!({})),
        )
        .await;
        match outcome {
            AuditEmit::Failed { reason } => assert!(
                reason.contains("storage unavailable"),
                "got {reason:?}"
            ),
            AuditEmit::Ok => panic!("expected Failed without storage"),
        }
    }
}
