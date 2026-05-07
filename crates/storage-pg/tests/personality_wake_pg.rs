//! PG coverage for Phase 1a personality wake-entry storage.

mod common;

use common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::personality::{
    InstantiatePersonalityRequest, PersonalityInstanceId, PersonalityMemoryDraft,
    PersonalityMemoryKind, PersonalityRef, PersonalitySelfDraft, PersonalityWriteRequest,
    SetWakeEntriesRequest, TombstonePersonalityRequest, WakeAuthorFilter, WakeChainDepth,
    WakeEntryDraft, WakeEntryTriggerKind,
};
use proxima_core::relation::{
    CORE_DERIVED_FROM_RELATION, CORE_SUPERSEDES_RELATION, core_relation_descriptors,
};
use proxima_core::storage::Storage;
use proxima_core::verbs::event_ingest::{CitationMappingHint, CitedObjectHint, EventDraft};
use proxima_core::{
    MemoryId, ModelTier, Owner, Principal, RegisteredRelation, SchemaId, SchemaVersion,
    SourceBatchId, SourceId,
};
use sqlx::Executor;
use uuid::Uuid;

async fn apply_self_sidecar(pool: &sqlx::PgPool) -> sqlx::Result<()> {
    pool.execute(
        "CREATE SCHEMA IF NOT EXISTS proxima_test; \
         CREATE TABLE IF NOT EXISTS proxima_test.personality_self_v1 ( \
             memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id), \
             display_name text NOT NULL, \
             purpose text NOT NULL \
         );",
    )
    .await
    .map(|_| ())
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

fn self_draft(display_name: &str) -> PersonalitySelfDraft {
    PersonalitySelfDraft {
        schema_id: SchemaId::new("proxima-test/self-v1".into()),
        schema_version: SchemaVersion::new(1),
        text: display_name.into(),
        typed_payload: serde_json::json!({
            "display_name": display_name,
            "purpose": "exercise wake storage",
            "system_prompt": "test system prompt",
        }),
    }
}

async fn seed_test_personality(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
) -> Result<proxima_core::InstantiatePersonalityResponse, Box<dyn std::error::Error>> {
    apply_self_sidecar(pg.pool()).await?;
    let response = pg
        .instantiate_personality(
            &InstantiatePersonalityRequest {
                owner: owner.clone(),
                personality_type_id: "proxima-test/personality-v1".into(),
                payload_overrides: None,
            },
            &self_draft("Engineer A"),
            "proxima_test.personality_self_v1",
        )
        .await?;
    Ok(response)
}

fn principal_id(owner: &Owner) -> Uuid {
    match &owner.principal {
        Principal::User(id) => id.into_inner(),
        Principal::Group(id) => id.into_inner(),
    }
}

fn sample_entry(instance: PersonalityInstanceId, trigger_id: &str) -> WakeEntryDraft {
    WakeEntryDraft::new(
        Uuid::now_v7(),
        instance,
        WakeEntryTriggerKind::OnMemory,
        trigger_id,
        "on_test_fact",
        WakeAuthorFilter::Any,
        250,
        "recipe:proxima-test/personality-v1",
        ModelTier::Fast,
        Some("local-cli:codex-spark".to_string()),
        vec!["core/query".to_string()],
        4,
    )
    .expect("valid wake entry")
}

#[tokio::test(flavor = "multi_thread")]
async fn personality_wake_schema_replaces_legacy_tables() {
    let Some((pg, db)) = fresh_pg().await else {
        return;
    };

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

        let expected = [
            "personality",
            "root_personality_perspective_v1",
            "personality_wake_entries",
            "personality_wake_cursor",
            "personality_wake_invocations",
        ];
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

        let type_columns: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM information_schema.columns
             WHERE table_schema = 'proxima_core'
               AND table_name IN (
                   'personality',
                   'personality_wake_entries',
                   'personality_wake_cursor',
                   'personality_wake_invocations'
               )
               AND column_name = 'personality_type_id'",
        )
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
async fn personality_wake_schema_enforces_root_sidecar_and_promille() {
    let Some((pg, db)) = fresh_pg().await else {
        return;
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        apply_self_sidecar(pg.pool()).await?;
        let owner = owner_fixture();
        let response = seed_test_personality(&pg, &owner).await?;

        let columns: Vec<String> = sqlx::query_scalar(
            "SELECT column_name FROM information_schema.columns
             WHERE table_schema = 'proxima_core'
               AND table_name = 'root_personality_perspective_v1'
             ORDER BY ordinal_position",
        )
        .fetch_all(pg.pool())
        .await?;
        assert_eq!(
            columns,
            vec!["memory_id", "display_name", "purpose", "system_prompt"]
        );

        let entry = sample_entry(response.instance_id, "proxima-test/fact-v1");
        let err = sqlx::query(
            "INSERT INTO proxima_core.personality_wake_entries
                (owner_principal_kind, owner_principal_id, owner_org_id,
                 personality_instance_id, wake_entry_id, trigger_kind, trigger_id,
                 label, authored_by, probability_promille, recipe_ref, model_tier)
             VALUES ('User', $1, $2, $3, $4, 'on_memory', 'proxima-test/fact-v1',
                     'bad', 'any', 1001, 'recipe:test', 'fast')",
        )
        .bind(principal_id(&owner))
        .bind(owner.org_id.into_inner())
        .bind(response.instance_id.into_inner())
        .bind(entry.wake_entry_id)
        .execute(pg.pool())
        .await
        .expect_err("probability_promille > 1000 must fail");
        assert!(err.to_string().contains("personality_wake_entries_probability_chk"));
        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db).await;
    result.expect("personality wake constraints failed");
}

#[tokio::test(flavor = "multi_thread")]
async fn personality_wake_storage_round_trip() {
    let Some((pg, db)) = fresh_pg().await else {
        return;
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        let owner = owner_fixture();
        let response = seed_test_personality(&pg, &owner).await?;
        let instance = response.instance_id;

        let personality_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM proxima_core.personality
             WHERE owner_principal_id = $1 AND personality_instance_id = $2",
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
            owner: owner.clone(),
            personality_instance_id: instance,
            entries: vec![first],
        })
        .await?;
        let replacement = sample_entry(instance, "proxima-test/fact-v1");
        pg.set_wake_entries(&SetWakeEntriesRequest {
            owner: owner.clone(),
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

        let seq = Uuid::now_v7();
        pg.advance_wake_cursor(&owner, instance, seq).await?;
        let stored: Uuid = sqlx::query_scalar(
            "SELECT last_considered_seq FROM proxima_core.personality_wake_cursor
             WHERE personality_instance_id = $1",
        )
        .bind(instance.into_inner())
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(stored, seq);

        assert!(
            pg.try_begin_wake_invocation(&owner, instance, replacement.wake_entry_id, seq)
                .await?
        );
        assert!(
            !pg.try_begin_wake_invocation(&owner, instance, replacement.wake_entry_id, seq)
                .await?
        );
        pg.finish_wake_invocation(
            &owner,
            instance,
            replacement.wake_entry_id,
            seq,
            proxima_core::WakeInvocationStatus::Truncated,
            4,
            0.125,
        )
        .await?;

        let res = pg
            .tombstone_personality(&TombstonePersonalityRequest {
                owner,
                personality_type_id: "proxima-test/personality-v1".into(),
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

fn fact_draft(owner: Owner) -> EventDraft {
    let now = time::OffsetDateTime::now_utc();
    EventDraft {
        source_id: SourceId::new("proxima-test/source"),
        source_batch_id: SourceBatchId::new(Uuid::now_v7()),
        owner,
        schema_id: SchemaId::new("proxima-test/fact-v1".into()),
        schema_version: SchemaVersion::new(1),
        payload: b"fact".to_vec(),
        observed_at: now,
        occurred_at: now,
        cited_object: CitedObjectHint {
            schema_id: SchemaId::new("proxima-test/cited-v1".into()),
            schema_version: SchemaVersion::new(1),
            content_hash: [9u8; 32],
        },
        citation_mapping: CitationMappingHint {
            schema_id: SchemaId::new("proxima-test/citation-v1".into()),
            schema_version: SchemaVersion::new(1),
        },
    }
}

fn memory_draft(
    kind: PersonalityMemoryKind,
    label: &str,
    provenance: Vec<MemoryId>,
) -> PersonalityMemoryDraft {
    PersonalityMemoryDraft {
        kind,
        schema_id: SchemaId::new("proxima-test/output-v1".into()),
        schema_version: SchemaVersion::new(1),
        text: label.into(),
        typed_payload: serde_json::json!({ "label": label }),
        provenance,
        embedding: vec![0.1, 0.2, 0.3],
        embedding_model_id: "test-embed".into(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn personality_provenance_edges_use_operator_authorship() {
    let Some((pg, db)) = fresh_pg().await else {
        return;
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        apply_personality_output_sidecar(pg.pool()).await?;

        let owner = owner_fixture();
        let fact = pg.ingest_event_atomic(&fact_draft(owner.clone())).await?;
        let descriptors = core_relation_descriptors();
        let provenance_relation = descriptors
            .iter()
            .find(|descriptor| descriptor.relation == CORE_DERIVED_FROM_RELATION)
            .expect("core provenance relation");
        let supersedes_relation = descriptors
            .iter()
            .find(|descriptor| descriptor.relation == CORE_SUPERSEDES_RELATION)
            .expect("core supersedes relation");
        let provenance_relation = RegisteredRelation {
            descriptor: provenance_relation,
            payload_sidecar_table: None,
        };
        let supersedes_relation = RegisteredRelation {
            descriptor: supersedes_relation,
            payload_sidecar_table: None,
        };
        let instance = PersonalityRef::new(
            "proxima-test/personality-v1",
            proxima_core::PersonalityInstanceId::new(Uuid::now_v7()),
        );

        let abstraction = pg
            .append_personality_memories(&PersonalityWriteRequest {
                owner: owner.clone(),
                instance: instance.clone(),
                model_id: "test-model",
                prompt_version: "test-v1",
                provenance_relation,
                supersedes_relation,
                wake_chain_depth: WakeChainDepth::new(0),
                memories: &[memory_draft(
                    PersonalityMemoryKind::Abstraction,
                    "abstraction",
                    vec![fact.memory_id],
                )],
                sidecar_table: "proxima_test.personality_output_v1",
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
                wake_chain_depth: WakeChainDepth::new(1),
                memories: &[memory_draft(
                    PersonalityMemoryKind::Perspective,
                    "perspective",
                    vec![abstraction_id],
                )],
                sidecar_table: "proxima_test.personality_output_v1",
            })
            .await?;

        let authored: Vec<(Uuid, String)> = sqlx::query_as(
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

        assert_eq!(
            authored,
            vec![
                (
                    perspective.memory_ids[0].into_inner(),
                    "OperatorAtoP".into()
                ),
                (abstraction_id.into_inner(), "OperatorFtoA".into()),
            ]
        );

        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db).await;
    result.expect("personality provenance edge authorship must satisfy DB constraint");
}
