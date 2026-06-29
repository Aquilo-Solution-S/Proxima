//! PG coverage for Phase 1a personality wake-entry storage.

#![allow(clippy::too_many_lines)]

use crate::common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::llm::EMBEDDING_DIM;
use proxima_core::personality::{
    InstantiatePersonalityRequest, PersonalityInstanceId, PersonalityMemoryDraft,
    PersonalityMemoryKind, PersonalityRef, PersonalityWriteRequest, SetWakeEntriesRequest,
    SidecarSpec, TombstonePersonalityRequest, WakeChainDepth, WakeEntryAuthoredBy, WakeEntryDraft,
    WakeEntryTriggerKind,
};
use proxima_core::relation::{
    CORE_AUTHORED_RELATION, CORE_DERIVED_FROM_RELATION, CORE_SUPERSEDES_RELATION,
};
use proxima_core::storage_ports::*;
use proxima_core::verbs::fact_ingest::{
    Citation, CitationMappingHint, CitedObjectHint, FactReceiptDraft, FactWriteCommand,
};
use proxima_core::{
    AbstractionPayload, EntityKind, FlavorRegistry, FlavorRegistryFrozen, MemoryId, Owner,
    OwnerRef, PerspectivePayload, RegisteredRelation, RelationClass, SchemaId, SchemaVersion,
    SidecarPayload, SourceBatchId, SourceId, StorageError, WakeEntryGoalScope,
};
use proxima_storage_pg::sidecars::{
    PgMemoryPayload, PgMemoryPayloadFuture, PgMemorySidecar, PgSidecarFuture,
};
use proxima_storage_pg::{
    PgSidecarRegistry, PgSidecarRegistryFrozen, PgStorage, register_core_pg_sidecars,
};
use sqlx::Executor;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use proxima_core::EdgeAuthorshipKind;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct PersonalityOutputV1 {
    label: String,
}

impl AbstractionPayload for PersonalityOutputV1 {
    const SCHEMA_ID: &'static str = "proxima-test/output-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_test.personality_output_v1"
    }
}

impl PerspectivePayload for PersonalityOutputV1 {
    const SCHEMA_ID: &'static str = "proxima-test/output-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_test.personality_output_v1"
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct EngineerSelfOutputV1 {
    label: String,
}

impl PerspectivePayload for EngineerSelfOutputV1 {
    const SCHEMA_ID: &'static str = "proxima-test/engineer-self-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_test.personality_output_v1"
    }
}

impl PgMemorySidecar for PersonalityOutputV1 {
    fn insert_memory_sidecar<'t>(
        &'t self,
        tx: &'t mut Transaction<'_, Postgres>,
        memory_id: MemoryId,
    ) -> PgSidecarFuture<'t> {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO proxima_test.personality_output_v1 (memory_id, label)
                 VALUES ($1, $2)",
            )
            .bind(memory_id.into_inner())
            .bind(&self.label)
            .execute(tx.as_mut())
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            Ok(())
        })
    }
}

impl PgMemoryPayload for PersonalityOutputV1 {
    fn load_memory_payload(
        _pool: &sqlx::PgPool,
        _memory_id: MemoryId,
    ) -> PgMemoryPayloadFuture<'_> {
        Box::pin(async { Ok(None) })
    }
}

impl PgMemorySidecar for EngineerSelfOutputV1 {
    fn insert_memory_sidecar<'t>(
        &'t self,
        tx: &'t mut Transaction<'_, Postgres>,
        memory_id: MemoryId,
    ) -> PgSidecarFuture<'t> {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO proxima_test.personality_output_v1 (memory_id, label)
                 VALUES ($1, $2)",
            )
            .bind(memory_id.into_inner())
            .bind(&self.label)
            .execute(tx.as_mut())
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            Ok(())
        })
    }
}

impl PgMemoryPayload for EngineerSelfOutputV1 {
    fn load_memory_payload(
        _pool: &sqlx::PgPool,
        _memory_id: MemoryId,
    ) -> PgMemoryPayloadFuture<'_> {
        Box::pin(async { Ok(None) })
    }
}

fn personality_pg_sidecars() -> PgSidecarRegistryFrozen {
    let mut registry = FlavorRegistry::new();
    registry.add_abstraction_schema::<PersonalityOutputV1>();
    registry.add_perspective_schema::<PersonalityOutputV1>();
    registry.add_perspective_schema::<EngineerSelfOutputV1>();
    let registry = registry.freeze();

    let mut sidecars = PgSidecarRegistry::new();
    register_core_pg_sidecars(&mut sidecars);
    sidecars.add_abstraction::<PersonalityOutputV1>();
    sidecars.add_perspective::<PersonalityOutputV1>();
    sidecars.add_perspective::<EngineerSelfOutputV1>();
    sidecars
        .freeze_against(registry.schemas())
        .expect("personality test PG sidecars match schemas")
}

async fn fresh_pg_with_personality_sidecars() -> (PgStorage, String) {
    let (pg, db) = fresh_pg().await;
    (pg.with_sidecars(personality_pg_sidecars()), db)
}

async fn apply_personality_output_sidecar(pool: &sqlx::PgPool) -> sqlx::Result<()> {
    pool.execute(
        "CREATE SCHEMA IF NOT EXISTS proxima_test; \
         CREATE TABLE IF NOT EXISTS proxima_test.personality_output_v1 ( \
             memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id), \
             label text NOT NULL \
         );",
    )
    .await
    .map(|_| ())
}

async fn seed_test_personality(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
) -> Result<proxima_core::InstantiatePersonalityResponse, Box<dyn std::error::Error>> {
    let response = pg
        .instantiate_personality(&InstantiatePersonalityRequest {
            principal: *owner,
            display_name: "Engineer A".into(),
        })
        .await?;
    Ok(response)
}

fn principal_id(owner: &Owner) -> Uuid {
    match owner {
        OwnerRef::World => proxima_core::WORLD_OWNER_UUID,
        OwnerRef::Personal(id) => id.into_inner(),
        OwnerRef::Group(id) => id.into_inner(),
    }
}

fn sample_entry(instance: PersonalityInstanceId, trigger_id: &str) -> WakeEntryDraft {
    let mut draft = WakeEntryDraft::new(
        Uuid::now_v7(),
        instance,
        WakeEntryTriggerKind::OnMemory,
        trigger_id,
        "on_test_fact",
        WakeEntryAuthoredBy::Any,
        250,
    )
    .expect("valid wake entry");
    draft.instructions = "Use the committed fact to decide whether to write a summary.".into();
    draft
}

async fn current_root_perspective_memory_id(
    pg: &proxima_storage_pg::PgStorage,
    instance: PersonalityInstanceId,
) -> Result<MemoryId, Box<dyn std::error::Error>> {
    let memory_id: Uuid = sqlx::query_scalar(
        "SELECT current_root_perspective_memory_id
         FROM proxima_core.personality
         WHERE personality_instance_id = $1",
    )
    .bind(instance.into_inner())
    .fetch_one(pg.pool())
    .await?;
    Ok(MemoryId::new(memory_id))
}

#[tokio::test(flavor = "multi_thread")]
async fn personality_wake_schema_replaces_legacy_tables() {
    let (pg, db) = fresh_pg_with_personality_sidecars().await;

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        let legacy: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM information_schema.tables
             WHERE table_schema = 'proxima_core'
               AND table_name = 'personality_wake_config'",
        )
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(legacy, 0);

        let expected = ["personality", "personality_wake_entries"];
        for table in expected {
            let count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM information_schema.tables
                 WHERE table_schema = 'proxima_core'
                   AND table_name = $1",
            )
            .bind(table)
            .fetch_one(pg.pool())
            .await?;
            assert_eq!(count, 1, "{table} must exist");
        }

        let wake_columns: Vec<String> = sqlx::query_scalar(
            "SELECT column_name
             FROM information_schema.columns
             WHERE table_schema = 'proxima_core'
               AND table_name = 'personality_wake_entries'
             ORDER BY ordinal_position",
        )
        .fetch_all(pg.pool())
        .await?;
        assert_eq!(
            wake_columns,
            vec![
                "owner_kind",
                "owner_id",
                "personality_instance_id",
                "wake_entry_id",
                "trigger_kind",
                "trigger_id",
                "label",
                "enabled",
                "authored_by",
                "probability_promille",
                "created_at",
                "updated_at",
                "tombstoned_at",
                "goal_scope",
                "instructions",
            ],
        );

        let retired_column = ["personality", "type", "id"].join("_");
        let type_columns: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM information_schema.columns
             WHERE table_schema = 'proxima_core'
               AND table_name IN (
                   'memories',
                   'change_event',
                   'goals',
                   'personality',
                   'personality_wake_entries'
               )
               AND column_name = $1",
        )
        .bind(retired_column)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(type_columns, 0);
        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db).await;
    result.expect("personality wake schema replacement failed");
}

#[tokio::test(flavor = "multi_thread")]
async fn personality_wake_schema_enforces_promille() {
    let (pg, db) = fresh_pg_with_personality_sidecars().await;

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        let owner = owner_fixture();
        let response = seed_test_personality(&pg, &owner).await?;

        let mut entry = sample_entry(response.instance_id, "proxima-test/fact-v1");
        entry.instructions = "Use the committed fact to decide whether to write a summary.".into();
        let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(&owner);
        let err = sqlx::query(
            "INSERT INTO proxima_core.personality_wake_entries
                (owner_kind, owner_id,
                 personality_instance_id, wake_entry_id, trigger_kind, trigger_id,
                 label, authored_by, probability_promille)
             VALUES ($1, $2, $3, $4, 'on_memory', 'proxima-test/fact-v1',
                     'bad', 'any', 1001)",
        )
        .bind(owner_kind)
        .bind(owner_id)
        .bind(response.instance_id.into_inner())
        .bind(entry.wake_entry_id)
        .execute(pg.pool())
        .await
        .expect_err("probability_promille > 1000 must fail");
        assert!(
            err.to_string()
                .contains("personality_wake_entries_probability_chk")
        );
        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db).await;
    result.expect("personality wake constraints failed");
}

#[tokio::test(flavor = "multi_thread")]
async fn personality_wake_storage_round_trip() {
    let (pg, db) = fresh_pg_with_personality_sidecars().await;

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        let owner = owner_fixture();
        let response = seed_test_personality(&pg, &owner).await?;
        let instance = response.instance_id;

        let personality_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM proxima_core.personality
             WHERE owner_id = $1 AND personality_instance_id = $2",
        )
        .bind(principal_id(&owner))
        .bind(instance.into_inner())
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(personality_count, 1);

        let entry_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM proxima_core.personality_wake_entries
             WHERE personality_instance_id = $1",
        )
        .bind(instance.into_inner())
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(entry_count, 0, "Phase 1a creates Inert personalities");

        let first = sample_entry(instance, "proxima-test/fact-v1");
        pg.set_wake_entries(&SetWakeEntriesRequest {
            principal: owner,
            personality_instance_id: instance,
            entries: vec![first],
        })
        .await?;
        let mut replacement = sample_entry(instance, "core/goal-activated-v1");
        replacement.goal_scope = WakeEntryGoalScope::TriggerGoalAssigned;
        pg.set_wake_entries(&SetWakeEntriesRequest {
            principal: owner,
            personality_instance_id: instance,
            entries: vec![replacement.clone()],
        })
        .await?;

        let active: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM proxima_core.personality_wake_entries
             WHERE personality_instance_id = $1 AND tombstoned_at IS NULL",
        )
        .bind(instance.into_inner())
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(active, 1);
        let goal_scope: WakeEntryGoalScope = sqlx::query_scalar(
            "SELECT goal_scope
             FROM proxima_core.personality_wake_entries
             WHERE personality_instance_id = $1
               AND wake_entry_id = $2
               AND tombstoned_at IS NULL",
        )
        .bind(instance.into_inner())
        .bind(replacement.wake_entry_id)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(goal_scope, WakeEntryGoalScope::TriggerGoalAssigned);

        let res = pg
            .tombstone_personality(&TombstonePersonalityRequest {
                principal: owner,
                personality_instance_id: instance,
            })
            .await?;
        assert!(!res.idempotent_replay);
        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db).await;
    result.expect("personality wake storage round trip failed");
}

#[tokio::test(flavor = "multi_thread")]
async fn list_personality_instances_populates_wake_entries() {
    let (pg, db) = fresh_pg_with_personality_sidecars().await;

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        let owner = owner_fixture();
        let response = seed_test_personality(&pg, &owner).await?;
        let entry = sample_entry(response.instance_id, "proxima-test/fact-v1");

        pg.set_wake_entries(&SetWakeEntriesRequest {
            principal: owner,
            personality_instance_id: response.instance_id,
            entries: vec![entry],
        })
        .await?;

        let rows = pg.list_personality_instances(&owner, false).await?;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].wake_entries.len(), 1);
        assert_eq!(rows[0].wake_entries[0].label, "on_test_fact");
        assert_eq!(rows[0].wake_entries[0].goal_scope, WakeEntryGoalScope::None);
        assert_eq!(
            rows[0].wake_entries[0].instructions,
            "Use the committed fact to decide whether to write a summary."
        );
        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db).await;
    result.expect("list_personality_instances must populate wake_entries");
}

fn fact_draft(_owner: Owner) -> FactWriteCommand {
    let now = time::OffsetDateTime::now_utc();
    FactWriteCommand {
        author_personality_instance_id: None,
        schema_id: SchemaId::new("proxima-test/fact-v1".into()),
        schema_version: SchemaVersion::new(1),
        payload: b"fact".to_vec(),
        rendered_text: None,
        receipt: Some(FactReceiptDraft {
            source_id: SourceId::new("proxima-test/source"),
            source_batch_id: SourceBatchId::new(Uuid::now_v7()),
            observed_at: now,
            occurred_at: now,
        }),
        citation: Some(Citation {
            object: CitedObjectHint {
                schema_id: SchemaId::new("proxima-test/cited-v1".into()),
                schema_version: SchemaVersion::new(1),
                content_hash: [9u8; 32],
            },
            mapping: CitationMappingHint {
                schema_id: SchemaId::new("proxima-test/citation-v1".into()),
                schema_version: SchemaVersion::new(1),
            },
        }),
    }
}

fn memory_draft(
    kind: PersonalityMemoryKind,
    label: &str,
    provenance: Vec<MemoryId>,
) -> PersonalityMemoryDraft {
    memory_draft_with_schema(kind, "proxima-test/output-v1", label, provenance)
}

fn memory_draft_with_schema(
    kind: PersonalityMemoryKind,
    schema_id: &str,
    label: &str,
    provenance: Vec<MemoryId>,
) -> PersonalityMemoryDraft {
    let sidecar_payload = match (kind, schema_id) {
        (PersonalityMemoryKind::Abstraction, "proxima-test/output-v1") => {
            SidecarPayload::abstraction(PersonalityOutputV1 {
                label: label.to_string(),
            })
        }
        (PersonalityMemoryKind::Perspective, "proxima-test/output-v1") => {
            SidecarPayload::perspective(PersonalityOutputV1 {
                label: label.to_string(),
            })
        }
        (PersonalityMemoryKind::Perspective, "proxima-test/engineer-self-v1") => {
            SidecarPayload::perspective(EngineerSelfOutputV1 {
                label: label.to_string(),
            })
        }
        other => panic!("unsupported personality test payload: {other:?}"),
    };
    PersonalityMemoryDraft {
        kind,
        schema_id: SchemaId::new(schema_id.into()),
        schema_version: SchemaVersion::new(1),
        text: label.into(),
        sidecar_payload,
        provenance,
        embedding: padded_embedding([0.1, 0.2, 0.3]),
        embedding_model_id: "test-embed".into(),
    }
}

fn padded_embedding(prefix: [f32; 3]) -> Vec<f32> {
    let mut embedding = vec![0.0; EMBEDDING_DIM];
    embedding[..prefix.len()].copy_from_slice(&prefix);
    embedding
}

fn resolve_registered_relation<'a>(
    registry: &'a FlavorRegistryFrozen,
    relation: &str,
) -> RegisteredRelation<'a> {
    registry
        .resolve_relation(relation)
        .expect("relation registered")
}

#[tokio::test(flavor = "multi_thread")]
async fn load_perspective_heads_returns_current_same_personality_learned_heads() {
    let (pg, db) = fresh_pg_with_personality_sidecars().await;

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        apply_personality_output_sidecar(pg.pool()).await?;

        let owner = owner_fixture();
        let seed = seed_test_personality(&pg, &owner).await?;
        let sibling = seed_test_personality(&pg, &owner).await?;
        let root_id = current_root_perspective_memory_id(&pg, seed.instance_id).await?;
        let registry = FlavorRegistry::new().freeze();
        let provenance_relation =
            resolve_registered_relation(&registry, CORE_DERIVED_FROM_RELATION);
        let supersedes_relation = resolve_registered_relation(&registry, CORE_SUPERSEDES_RELATION);
        let authored_relation = resolve_registered_relation(&registry, CORE_AUTHORED_RELATION);

        let instance = PersonalityRef::new(seed.instance_id);
        let first = pg
            .append_personality_memories(&PersonalityWriteRequest {
                owner,
                instance: instance.clone(),
                model_id: "test-model",
                prompt_version: "test-v1",
                provenance_relation,
                supersedes_relation,
                authored_relation,
                current_root_perspective_memory_id: root_id,
                wake_chain_depth: WakeChainDepth::new(1),
                memories: &[memory_draft(
                    PersonalityMemoryKind::Perspective,
                    "old perspective",
                    Vec::new(),
                )],
            })
            .await?;
        let second = pg
            .append_personality_memories(&PersonalityWriteRequest {
                owner,
                instance: instance.clone(),
                model_id: "test-model",
                prompt_version: "test-v1",
                provenance_relation,
                supersedes_relation,
                authored_relation,
                current_root_perspective_memory_id: root_id,
                wake_chain_depth: WakeChainDepth::new(2),
                memories: &[memory_draft(
                    PersonalityMemoryKind::Perspective,
                    "current perspective",
                    Vec::new(),
                )],
            })
            .await?;
        pg.append_personality_memories(&PersonalityWriteRequest {
            owner,
            instance: instance.clone(),
            model_id: "test-model",
            prompt_version: "test-v1",
            provenance_relation,
            supersedes_relation,
            authored_relation,
            current_root_perspective_memory_id: root_id,
            wake_chain_depth: WakeChainDepth::new(3),
            memories: &[memory_draft_with_schema(
                PersonalityMemoryKind::Perspective,
                "proxima-test/engineer-self-v1",
                "legacy self perspective",
                Vec::new(),
            )],
        })
        .await?;

        let sibling_root_id = current_root_perspective_memory_id(&pg, sibling.instance_id).await?;
        pg.append_personality_memories(&PersonalityWriteRequest {
            owner,
            instance: PersonalityRef::new(sibling.instance_id),
            model_id: "test-model",
            prompt_version: "test-v1",
            provenance_relation,
            supersedes_relation,
            authored_relation,
            current_root_perspective_memory_id: sibling_root_id,
            wake_chain_depth: WakeChainDepth::new(1),
            memories: &[memory_draft(
                PersonalityMemoryKind::Perspective,
                "sibling perspective",
                Vec::new(),
            )],
        })
        .await?;

        // The root self-perspective has no registered schema/sidecar, so it
        // never appears in a spec list; it is excluded from perspective heads
        // by `memory_id <> root` + the supersession filter, not by a schema
        // clause.
        let sidecars = vec![
            SidecarSpec {
                schema_id: SchemaId::new("proxima-test/output-v1".into()),
                schema_version: SchemaVersion::new(1),
                sidecar_table: "proxima_test.personality_output_v1".into(),
            },
            SidecarSpec {
                schema_id: SchemaId::new("proxima-test/engineer-self-v1".into()),
                schema_version: SchemaVersion::new(1),
                sidecar_table: "proxima_test.personality_output_v1".into(),
            },
        ];

        let heads = pg
            .load_perspective_heads(&owner, seed.instance_id, root_id, &sidecars, 8)
            .await?;

        assert_eq!(heads.len(), 1);
        assert_eq!(heads[0].memory_id, second.memory_ids[0]);
        assert_eq!(heads[0].text.as_deref(), Some("current perspective"));
        assert_ne!(heads[0].memory_id, first.memory_ids[0]);

        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db).await;
    result.expect("perspective heads should filter superseded, self, and sibling rows");
}

/// Two Perspectives of the SAME (owner, instance, schema) in ONE write must
/// form a linear chain (second supersedes first), not fork into parallel heads.
/// This only holds if the in-batch prior-head lookup runs inside the write
/// transaction (so it sees the first, uncommitted insert); a pool-bound lookup
/// would have both supersede the root head, which `idx_memories_supersedes_uq`
/// (migration 0003) now rejects outright.
#[tokio::test(flavor = "multi_thread")]
async fn same_batch_same_schema_perspectives_form_a_linear_chain_not_a_fork() {
    let (pg, db) = fresh_pg_with_personality_sidecars().await;

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        apply_personality_output_sidecar(pg.pool()).await?;

        let owner = owner_fixture();
        let seed = seed_test_personality(&pg, &owner).await?;
        let root_id = current_root_perspective_memory_id(&pg, seed.instance_id).await?;
        let registry = FlavorRegistry::new().freeze();
        let provenance_relation =
            resolve_registered_relation(&registry, CORE_DERIVED_FROM_RELATION);
        let supersedes_relation = resolve_registered_relation(&registry, CORE_SUPERSEDES_RELATION);
        let authored_relation = resolve_registered_relation(&registry, CORE_AUTHORED_RELATION);
        let instance = PersonalityRef::new(seed.instance_id);

        let out = pg
            .append_personality_memories(&PersonalityWriteRequest {
                owner,
                instance: instance.clone(),
                model_id: "test-model",
                prompt_version: "test-v1",
                provenance_relation,
                supersedes_relation,
                authored_relation,
                current_root_perspective_memory_id: root_id,
                wake_chain_depth: WakeChainDepth::new(1),
                memories: &[
                    memory_draft(
                        PersonalityMemoryKind::Perspective,
                        "first in batch",
                        Vec::new(),
                    ),
                    memory_draft(
                        PersonalityMemoryKind::Perspective,
                        "second in batch",
                        Vec::new(),
                    ),
                ],
            })
            .await?;
        assert_eq!(out.memory_ids.len(), 2);

        let sidecars = vec![SidecarSpec {
            schema_id: SchemaId::new("proxima-test/output-v1".into()),
            schema_version: SchemaVersion::new(1),
            sidecar_table: "proxima_test.personality_output_v1".into(),
        }];
        let heads = pg
            .load_perspective_heads(&owner, seed.instance_id, root_id, &sidecars, 8)
            .await?;

        // Exactly one head, and it is the SECOND draft — proving a linear chain
        // (root <- first <- second), not a fork.
        assert_eq!(
            heads.len(),
            1,
            "same-schema batch must not fork into parallel heads"
        );
        assert_eq!(heads[0].memory_id, out.memory_ids[1]);
        assert_eq!(heads[0].text.as_deref(), Some("second in batch"));
        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db).await;
    result.expect("same-batch same-schema perspectives must form a linear chain");
}

/// Two CONCURRENT cross-request Perspective appends for the SAME
/// (owner, instance, schema) must both land as a linear supersedes chain.
/// Before the per-instance transaction advisory lock in
/// `append_personality_memories`, both transactions read the same head H0 and
/// raced the supersede insert; `idx_memories_supersedes_uq` (migration 0003)
/// failed the loser with a unique violation and that append was lost. The lock
/// now serializes them (root <- H0 <- A <- B) so both succeed.
#[tokio::test(flavor = "multi_thread")]
async fn concurrent_cross_request_perspectives_serialize_into_linear_chain() {
    let (pg, db) = fresh_pg_with_personality_sidecars().await;

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        apply_personality_output_sidecar(pg.pool()).await?;

        let owner = owner_fixture();
        let seed = seed_test_personality(&pg, &owner).await?;
        let root_id = current_root_perspective_memory_id(&pg, seed.instance_id).await?;
        let registry = FlavorRegistry::new().freeze();
        let provenance_relation =
            resolve_registered_relation(&registry, CORE_DERIVED_FROM_RELATION);
        let supersedes_relation = resolve_registered_relation(&registry, CORE_SUPERSEDES_RELATION);
        let authored_relation = resolve_registered_relation(&registry, CORE_AUTHORED_RELATION);
        let instance = PersonalityRef::new(seed.instance_id);

        // Establish a non-null head H0 so the concurrent appends contend on a
        // populated `supersedes` — two first-ever appends would both be
        // supersedes=NULL and would NOT collide on the partial unique index.
        let head_drafts = [memory_draft(
            PersonalityMemoryKind::Perspective,
            "head H0",
            Vec::new(),
        )];
        pg.append_personality_memories(&PersonalityWriteRequest {
            owner,
            instance: instance.clone(),
            model_id: "test-model",
            prompt_version: "test-v1",
            provenance_relation,
            supersedes_relation,
            authored_relation,
            current_root_perspective_memory_id: root_id,
            wake_chain_depth: WakeChainDepth::new(1),
            memories: &head_drafts,
        })
        .await?;

        // Two independent concurrent appends, each its own transaction, both
        // targeting H0's successor slot.
        let drafts_a = [memory_draft(
            PersonalityMemoryKind::Perspective,
            "concurrent A",
            Vec::new(),
        )];
        let drafts_b = [memory_draft(
            PersonalityMemoryKind::Perspective,
            "concurrent B",
            Vec::new(),
        )];
        let req_a = PersonalityWriteRequest {
            owner,
            instance: instance.clone(),
            model_id: "test-model",
            prompt_version: "test-v1",
            provenance_relation,
            supersedes_relation,
            authored_relation,
            current_root_perspective_memory_id: root_id,
            wake_chain_depth: WakeChainDepth::new(2),
            memories: &drafts_a,
        };
        let req_b = PersonalityWriteRequest {
            owner,
            instance: instance.clone(),
            model_id: "test-model",
            prompt_version: "test-v1",
            provenance_relation,
            supersedes_relation,
            authored_relation,
            current_root_perspective_memory_id: root_id,
            wake_chain_depth: WakeChainDepth::new(2),
            memories: &drafts_b,
        };
        let (landed_a, landed_b) = tokio::join!(
            pg.append_personality_memories(&req_a),
            pg.append_personality_memories(&req_b),
        );
        // Pre-fix, exactly one of these fails with a 23505 unique violation.
        landed_a?;
        landed_b?;

        // The chain is linear: exactly one head, three rows total for the schema.
        let sidecars = vec![SidecarSpec {
            schema_id: SchemaId::new("proxima-test/output-v1".into()),
            schema_version: SchemaVersion::new(1),
            sidecar_table: "proxima_test.personality_output_v1".into(),
        }];
        let heads = pg
            .load_perspective_heads(&owner, seed.instance_id, root_id, &sidecars, 8)
            .await?;
        assert_eq!(
            heads.len(),
            1,
            "concurrent cross-request appends must serialize to one linear head, not fork"
        );

        let row_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM proxima_core.memories
              WHERE personality_instance_id = $1
                AND schema_id = 'proxima-test/output-v1'
                AND kind = 'Perspective'
                AND tombstoned_at IS NULL",
        )
        .bind(seed.instance_id.into_inner())
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(row_count, 3, "H0 + two concurrent appends must all persist");

        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db).await;
    result.expect("concurrent cross-request perspectives must serialize into a linear chain");
}

#[tokio::test(flavor = "multi_thread")]
async fn personality_provenance_edges_use_operator_authorship() {
    let (pg, db) = fresh_pg_with_personality_sidecars().await;

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        apply_personality_output_sidecar(pg.pool()).await?;

        let owner = owner_fixture();
        let seed = seed_test_personality(&pg, &owner).await?;
        let root_id = current_root_perspective_memory_id(&pg, seed.instance_id).await?;
        let fact = pg
            .ingest_fact_atomic(&owner, &fact_draft(owner), None)
            .await?;
        let registry = FlavorRegistry::new().freeze();
        let provenance_relation =
            resolve_registered_relation(&registry, CORE_DERIVED_FROM_RELATION);
        let supersedes_relation = resolve_registered_relation(&registry, CORE_SUPERSEDES_RELATION);
        let authored_relation = resolve_registered_relation(&registry, CORE_AUTHORED_RELATION);
        let instance = PersonalityRef::new(seed.instance_id);

        let abstraction = pg
            .append_personality_memories(&PersonalityWriteRequest {
                owner,
                instance: instance.clone(),
                model_id: "test-model",
                prompt_version: "test-v1",
                provenance_relation,
                supersedes_relation,
                authored_relation,
                current_root_perspective_memory_id: root_id,
                wake_chain_depth: WakeChainDepth::new(0),
                memories: &[memory_draft(
                    PersonalityMemoryKind::Abstraction,
                    "abstraction",
                    vec![fact.memory_id],
                )],
            })
            .await?;
        let abstraction_id = abstraction.memory_ids[0];

        let perspective = pg
            .append_personality_memories(&PersonalityWriteRequest {
                owner,
                instance,
                model_id: "test-model",
                prompt_version: "test-v1",
                provenance_relation,
                supersedes_relation,
                authored_relation,
                current_root_perspective_memory_id: root_id,
                wake_chain_depth: WakeChainDepth::new(1),
                memories: &[memory_draft(
                    PersonalityMemoryKind::Perspective,
                    "perspective",
                    vec![abstraction_id],
                )],
            })
            .await?;

        let authored: Vec<(Uuid, EdgeAuthorshipKind)> = sqlx::query_as(
            "SELECT source_memory_id, authorship_kind
             FROM proxima_core.edges
             WHERE source_memory_id = ANY($1)
               AND relation = 'core/derived-from'
             ORDER BY authorship_kind",
        )
        .bind(
            &[
                abstraction_id.into_inner(),
                perspective.memory_ids[0].into_inner(),
            ][..],
        )
        .fetch_all(pg.pool())
        .await?;

        assert!(authored.contains(&(
            perspective.memory_ids[0].into_inner(),
            EdgeAuthorshipKind::OperatorAtoP,
        )));
        assert!(authored.contains(&(
            abstraction_id.into_inner(),
            EdgeAuthorshipKind::OperatorFtoA,
        )));

        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db).await;
    result.expect("personality provenance edge authorship must satisfy DB constraint");
}

#[tokio::test(flavor = "multi_thread")]
async fn personality_provenance_skips_perspective_context_targets() {
    let (pg, db) = fresh_pg_with_personality_sidecars().await;

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        apply_personality_output_sidecar(pg.pool()).await?;

        let owner = owner_fixture();
        let seed = seed_test_personality(&pg, &owner).await?;
        let root_id = current_root_perspective_memory_id(&pg, seed.instance_id).await?;
        let fact = pg
            .ingest_fact_atomic(&owner, &fact_draft(owner), None)
            .await?;
        let registry = FlavorRegistry::new().freeze();
        let provenance_relation =
            resolve_registered_relation(&registry, CORE_DERIVED_FROM_RELATION);
        let supersedes_relation = resolve_registered_relation(&registry, CORE_SUPERSEDES_RELATION);
        let authored_relation = resolve_registered_relation(&registry, CORE_AUTHORED_RELATION);

        let outcome = pg
            .append_personality_memories(&PersonalityWriteRequest {
                owner,
                instance: PersonalityRef::new(seed.instance_id),
                model_id: "test-model",
                prompt_version: "test-v1",
                provenance_relation,
                supersedes_relation,
                authored_relation,
                current_root_perspective_memory_id: root_id,
                wake_chain_depth: WakeChainDepth::new(1),
                memories: &[memory_draft(
                    PersonalityMemoryKind::Abstraction,
                    "abstraction with perspective context",
                    vec![fact.memory_id, root_id],
                )],
            })
            .await?;
        let abstraction_id = outcome.memory_ids[0];

        let derived_targets: Vec<(Uuid, EntityKind)> = sqlx::query_as(
            "SELECT target_memory_id, target_kind
             FROM proxima_core.edges
             WHERE source_memory_id = $1
               AND relation = 'core/derived-from'
             ORDER BY target_kind",
        )
        .bind(abstraction_id.into_inner())
        .fetch_all(pg.pool())
        .await?;

        assert_eq!(
            derived_targets,
            vec![(fact.memory_id.into_inner(), EntityKind::Fact)]
        );

        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db).await;
    result.expect("personality provenance must treat Perspectives as context, not derived-from");
}

#[tokio::test(flavor = "multi_thread")]
async fn personality_authored_edge_links_root_to_emitted_memory() {
    let (pg, db) = fresh_pg_with_personality_sidecars().await;

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        apply_personality_output_sidecar(pg.pool()).await?;

        let owner = owner_fixture();
        let seed = seed_test_personality(&pg, &owner).await?;
        let root_id = current_root_perspective_memory_id(&pg, seed.instance_id).await?;
        let fact = pg
            .ingest_fact_atomic(&owner, &fact_draft(owner), None)
            .await?;
        let registry = FlavorRegistry::new().freeze();
        let provenance_relation =
            resolve_registered_relation(&registry, CORE_DERIVED_FROM_RELATION);
        let supersedes_relation = resolve_registered_relation(&registry, CORE_SUPERSEDES_RELATION);
        let authored_relation = resolve_registered_relation(&registry, CORE_AUTHORED_RELATION);
        let instance = PersonalityRef::new(seed.instance_id);

        let abstraction = pg
            .append_personality_memories(&PersonalityWriteRequest {
                owner,
                instance: instance.clone(),
                model_id: "test-model",
                prompt_version: "test-v1",
                provenance_relation,
                supersedes_relation,
                authored_relation,
                current_root_perspective_memory_id: root_id,
                wake_chain_depth: WakeChainDepth::new(0),
                memories: &[memory_draft(
                    PersonalityMemoryKind::Abstraction,
                    "abstraction",
                    vec![fact.memory_id],
                )],
            })
            .await?;
        let abstraction_id = abstraction.memory_ids[0];

        let perspective = pg
            .append_personality_memories(&PersonalityWriteRequest {
                owner,
                instance,
                model_id: "test-model",
                prompt_version: "test-v1",
                provenance_relation,
                supersedes_relation,
                authored_relation,
                current_root_perspective_memory_id: root_id,
                wake_chain_depth: WakeChainDepth::new(1),
                memories: &[memory_draft(
                    PersonalityMemoryKind::Perspective,
                    "perspective",
                    vec![abstraction_id],
                )],
            })
            .await?;
        let perspective_id = perspective.memory_ids[0];

        let authored_rows: Vec<(
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
                   AND target_memory_id = ANY($1)
                 ORDER BY target_kind",
        )
        .bind(&[abstraction_id.into_inner(), perspective_id.into_inner()][..])
        .fetch_all(pg.pool())
        .await?;

        assert_eq!(
            authored_rows.len(),
            2,
            "exactly one core/authored edge per emitted memory"
        );
        for row in &authored_rows {
            assert_eq!(
                row.0,
                root_id.into_inner(),
                "edge originates at the wake's snapshotted Root Perspective"
            );
            assert_eq!(
                row.2,
                EntityKind::Perspective,
                "source_kind must mark the edge origin as the Root Perspective"
            );
            assert_eq!(
                row.4,
                RelationClass::Causal,
                "core/authored is registered with class Causal, mirroring core/inspires"
            );
            assert_eq!(
                row.5,
                EdgeAuthorshipKind::Engine,
                "substrate authors the edge on the personality's behalf"
            );
        }
        assert_eq!(authored_rows[0].1, abstraction_id.into_inner());
        assert_eq!(authored_rows[0].3, EntityKind::Abstraction);
        assert_eq!(authored_rows[1].1, perspective_id.into_inner());
        assert_eq!(authored_rows[1].3, EntityKind::Perspective);

        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db).await;
    result.expect("personality authored-edge wiring");
}
