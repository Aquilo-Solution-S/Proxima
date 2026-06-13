//! End-to-end (PG-backed): when a Personality emits an Abstraction
//! through the substrate `core/emit_abstraction` tool, storage wires a
//! `core/authored` edge from the personality's snapshotted Root
//! Perspective to the new memory in the same transaction.
//!
//! Slice-3 coverage for the design at
//! `docs/superpowers/specs/2026-05-09-personality-authorship-edge.md`:
//! the storage layer is exercised by `personality_wake_pg.rs::
//! personality_authored_edge_links_root_to_emitted_memory`; this test
//! drives the same outcome through the substrate-tool path that wakes
//! actually use, proving `shared.rs::emit_personality_memory` resolves
//! the `core/authored` relation from the registry and threads
//! `ctx.current_root_perspective_memory_id` into
//! `PersonalityWriteRequest`.

#![allow(clippy::too_many_lines, clippy::unnecessary_literal_bound)]

use std::sync::Arc;

use async_trait::async_trait;
use proxima_code::{CommitSummaryV1, CommitV1, build_engine_with, ingest_commit, register_repo};
use proxima_core::llm::{EmbeddingClient, LlmError};
use proxima_core::personality::tools::EmitAbstractionTool;
use proxima_core::personality::{
    InstantiatePersonalityRequest, PersonalityTool, PersonalityToolContext,
};
use proxima_core::storage::Storage;
use proxima_core::wake::token_store::WakeTokenContext;
use proxima_core::{
    AbstractionPayload, AuthPath, AuthzContext, EntityKind, HandleTable, MemoryId, OrgId, Owner,
    Principal, RelationClass, SourceBatchId, UserId, WakeChainDepth,
};
use proxima_pg_testkit::{create_db, db_url, drop_db, unique_db_name};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

use proxima_core::EdgeAuthorshipKind;

async fn migrated_db() -> Option<(String, PgStorage)> {
    let db_name = unique_db_name("proxima_test");
    create_db(&db_name).await.expect("PG required for tests");
    let url = db_url(&db_name);
    let pg = PgStorage::connect(&url).await.expect("connect test db");
    pg.run_migrations().await.expect("core migrations");
    proxima_code::migrator()
        .run(pg.pool())
        .await
        .expect("code migrations");
    Some((db_name, pg))
}

fn test_owner() -> Owner {
    Owner {
        principal: Principal::User(UserId::new(Uuid::now_v7())),
        org_id: OrgId::new(Uuid::now_v7()),
    }
}

#[derive(Debug)]
struct FakeEmbedding;

#[async_trait]
impl EmbeddingClient for FakeEmbedding {
    async fn embed(&self, _text: &str) -> Result<Vec<f32>, LlmError> {
        Ok(vec![0.0; 8])
    }
    fn model_id(&self) -> &str {
        "fake-embed"
    }
    fn dim(&self) -> usize {
        8
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn emit_abstraction_writes_core_authored_edge_from_root_perspective() {
    let Some((db, pg)) = migrated_db().await else {
        return;
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = test_owner();
        let repo_id = Uuid::now_v7();
        register_repo(
            pg.pool(),
            &owner,
            repo_id,
            "/tmp/personality-authored-e2e",
            "e2e",
        )
        .await?;

        let engine =
            build_engine_with(pg.clone(), |_registry| {}).with_embed(Arc::new(FakeEmbedding));

        let authz = AuthzContext::single_owner(&owner, AuthPath::System);
        let inst = engine
            .instantiate_personality(
                &authz,
                InstantiatePersonalityRequest {
                    principal: owner.principal.clone(),
                    org_id: None,
                    display_name: "Slice-3 Engineer".into(),
                    purpose: "Drive the substrate-tool emit path".into(),
                },
            )
            .await?;
        let runtime = pg
            .fetch_personality_runtime(&owner, inst.instance_id)
            .await?
            .expect("personality runtime row");
        let root_id: MemoryId = runtime.current_root_perspective_memory_id;

        let now = time::OffsetDateTime::now_utc();
        let commit_payload = CommitV1 {
            repo_id,
            sha: "abc123def456".into(),
            parents: Vec::new(),
            author_name: "Slice 3".into(),
            author_email: "slice3@example.com".into(),
            author_time: now,
            committer_name: "Slice 3".into(),
            committer_email: "slice3@example.com".into(),
            committer_time: now,
            message: "feat: thread root through emit".into(),
        };
        let commit_outcome = ingest_commit(
            pg.pool(),
            &owner,
            SourceBatchId::new(Uuid::now_v7()),
            &commit_payload,
            now,
        )
        .await?;
        let commit_memory_id: MemoryId = commit_outcome.memory_id;

        let palette: Vec<Arc<dyn PersonalityTool>> = Vec::new();
        let writeable_schemas =
            vec![<CommitSummaryV1 as AbstractionPayload>::SCHEMA_ID.to_string()];
        let wake = WakeTokenContext {
            invocation_id: Uuid::now_v7(),
            personality_instance_id: inst.instance_id.into_inner(),
            wake_entry_id: Uuid::now_v7(),
            change_event_seq: Uuid::now_v7(),
            owner: owner.clone(),
            palette: vec!["core/emit_abstraction".into()],
            model_id: "test/slice-3-model".into(),
            max_rounds: 4,
            current_root_perspective_memory_id: root_id,
            current_root_perspective_memory_class: proxima_core::MemoryHandleClass::Perspective,
            triggering_event_memory_id: commit_memory_id,
            triggering_event_memory_class: proxima_core::MemoryHandleClass::Fact,
            triggering_event_depth: WakeChainDepth::new(0),
            read_log: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            handles: Arc::new(HandleTable::new()),
        };
        let ctx = PersonalityToolContext::new(
            &engine,
            &owner,
            "test/slice-3-engineer",
            inst.instance_id,
            root_id,
            commit_memory_id,
            WakeChainDepth::new(0),
            writeable_schemas,
            Vec::new(),
            &palette,
            wake.handles.clone(),
        )
        .with_wake_invocation(&wake);

        let tool = EmitAbstractionTool;
        let args = serde_json::json!({
            "schema_id": <CommitSummaryV1 as AbstractionPayload>::SCHEMA_ID,
            "schema_version": 1,
            "payload": {
                "repo_id": repo_id,
                "commit_sha": commit_payload.sha,
                "summary": "Wires Root Perspective into the emit path.",
                "key_files": ["crates/core/src/personality/tools/shared.rs"],
                "change_kind": "feature",
            },
        });
        let result = tool.invoke(&ctx, args).await?;
        assert!(
            !result.is_error,
            "emit_abstraction must succeed; got error payload: {:?}",
            result.content
        );
        let memory_handle = result
            .content
            .get("memory")
            .and_then(serde_json::Value::as_str)
            .expect("emit returns memory handle");
        let new_memory_id: Uuid = wake
            .handles
            .resolve_memory(memory_handle)
            .expect("emitted memory handle resolves")
            .into_inner();

        let edges: Vec<(
            Uuid,
            Uuid,
            EntityKind,
            EntityKind,
            RelationClass,
            EdgeAuthorshipKind,
        )> = sqlx::query_as(
            "SELECT source_memory_id, target_memory_id, source_kind, target_kind,
                    relation_class, authorship_kind
             FROM proxima_core.edges
             WHERE relation = 'core/authored'
               AND target_memory_id = $1",
        )
        .bind(new_memory_id)
        .fetch_all(pg.pool())
        .await?;

        assert_eq!(
            edges.len(),
            1,
            "exactly one core/authored edge per emitted Abstraction"
        );
        let edge = &edges[0];
        assert_eq!(
            edge.0,
            root_id.into_inner(),
            "edge originates at the snapshotted Root Perspective threaded through ctx"
        );
        assert_eq!(edge.1, new_memory_id, "edge targets the new memory");
        assert_eq!(
            edge.2,
            EntityKind::Perspective,
            "source_kind == Perspective"
        );
        assert_eq!(
            edge.3,
            EntityKind::Abstraction,
            "target_kind == Abstraction"
        );
        assert_eq!(
            edge.4,
            RelationClass::Causal,
            "core/authored class == Causal"
        );
        assert_eq!(
            edge.5,
            EdgeAuthorshipKind::Engine,
            "substrate authors the edge"
        );

        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db).await;
    result.expect("personality_authored_edge_e2e failed");
}
