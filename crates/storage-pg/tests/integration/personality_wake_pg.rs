//! PG coverage for Phase 1a personality wake-entry storage.

#![allow(clippy::too_many_lines)]

use crate::common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::personality::{
    InstantiatePersonalityRequest, PersonalityInstanceId, PersonalityMemoryDraft,
    PersonalityMemoryKind, PersonalityRef, PersonalityWriteRequest,
    ROOT_PERSONALITY_PERSPECTIVE_SCHEMA_ID, SetWakeEntriesRequest, SidecarSpec,
    TombstonePersonalityRequest, WakeChainDepth, WakeEntryAuthoredBy, WakeEntryDraft,
    WakeEntryTriggerKind,
};
use proxima_core::relation::{
    CORE_AUTHORED_RELATION, CORE_DERIVED_FROM_RELATION, CORE_SUPERSEDES_RELATION,
    core_relation_descriptors,
};
use proxima_core::storage::Storage;
use proxima_core::verbs::event_ingest::{
    Citation, CitationMappingHint, CitedObjectHint, EventDraft,
};
use proxima_core::{
    EntityKind, MemoryId, Owner, Principal, RegisteredRelation, RelationClass, SchemaId,
    SchemaVersion, SourceBatchId, SourceId, WakeEntryGoalScope,
};
use sqlx::Executor;
use uuid::Uuid;

use proxima_core::EdgeAuthorshipKind;

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
            principal: owner.principal.clone(),
            org_id: Some(owner.org_id),
            display_name: "Engineer A".into(),
            purpose: "exercise wake storage".into(),
        })
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
                "owner_principal_kind",
                "owner_principal_id",
                "owner_org_id",
                "personality_instance_id",
                "wake_entry_id",
                "trigger_kind",
                "trigger_id",
                "label",
                "enabled",
                "authored_by",
                "probability_promille",
                "disabled_reason",
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
                   'source_batch_f2a',
                   'personality',
                   'personality_wake_entries',
                   'personality_wake_cursor'
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
async fn personality_wake_schema_enforces_root_sidecar_and_promille() {
    let Some((pg, db)) = fresh_pg().await else {
        return;
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
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
        assert_eq!(columns, vec!["memory_id", "display_name", "purpose"]);

        let mut entry = sample_entry(response.instance_id, "proxima-test/fact-v1");
        entry.instructions = "Use the committed fact to decide whether to write a summary.".into();
        let err = sqlx::query(
            "INSERT INTO proxima_core.personality_wake_entries
                (owner_principal_kind, owner_principal_id, owner_org_id,
                 personality_instance_id, wake_entry_id, trigger_kind, trigger_id,
                 label, authored_by, probability_promille)
             VALUES ('User', $1, $2, $3, $4, 'on_memory', 'proxima-test/fact-v1',
                     'bad', 'any', 1001)",
        )
        .bind(principal_id(&owner))
        .bind(owner.org_id.into_inner())
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
            principal: owner.principal.clone(),
            org_id: Some(owner.org_id),
            personality_instance_id: instance,
            entries: vec![first],
        })
        .await?;
        let mut replacement = sample_entry(instance, "proxima-goal/goal-activated-v1");
        replacement.goal_scope = WakeEntryGoalScope::TriggerGoalAssigned;
        pg.set_wake_entries(&SetWakeEntriesRequest {
            principal: owner.principal.clone(),
            org_id: Some(owner.org_id),
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
                principal: owner.principal,
                org_id: Some(owner.org_id),
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
    let Some((pg, db)) = fresh_pg().await else {
        return;
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        let owner = owner_fixture();
        let response = seed_test_personality(&pg, &owner).await?;
        let entry = sample_entry(response.instance_id, "proxima-test/fact-v1");

        pg.set_wake_entries(&SetWakeEntriesRequest {
            principal: owner.principal.clone(),
            org_id: Some(owner.org_id),
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

fn fact_draft(owner: Owner) -> EventDraft {
    let now = time::OffsetDateTime::now_utc();
    EventDraft {
        source_id: SourceId::new("proxima-test/source"),
        source_batch_id: SourceBatchId::new(Uuid::now_v7()),
        principal: owner.principal,
        org_id: Some(owner.org_id),
        author_personality_instance_id: None,
        schema_id: SchemaId::new("proxima-test/fact-v1".into()),
        schema_version: SchemaVersion::new(1),
        payload: b"fact".to_vec(),
        rendered_text: None,
        observed_at: now,
        occurred_at: now,
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
    PersonalityMemoryDraft {
        kind,
        schema_id: SchemaId::new(schema_id.into()),
        schema_version: SchemaVersion::new(1),
        text: label.into(),
        typed_payload: serde_json::json!({ "label": label }),
        provenance,
        embedding: vec![0.1, 0.2, 0.3],
        embedding_model_id: "test-embed".into(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn load_perspective_heads_returns_current_same_personality_learned_heads() {
    let Some((pg, db)) = fresh_pg().await else {
        return;
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        apply_personality_output_sidecar(pg.pool()).await?;

        let owner = owner_fixture();
        let seed = seed_test_personality(&pg, &owner).await?;
        let sibling = seed_test_personality(&pg, &owner).await?;
        let root_id = current_root_perspective_memory_id(&pg, seed.instance_id).await?;
        let descriptors = core_relation_descriptors();
        let resolve = |id: &str| {
            let descriptor = descriptors
                .iter()
                .find(|descriptor| descriptor.relation == id)
                .expect("relation registered");
            RegisteredRelation {
                descriptor,
                payload_sidecar_table: None,
            }
        };
        let provenance_relation = resolve(CORE_DERIVED_FROM_RELATION);
        let supersedes_relation = resolve(CORE_SUPERSEDES_RELATION);
        let authored_relation = resolve(CORE_AUTHORED_RELATION);

        let instance = PersonalityRef::new(seed.instance_id);
        let first = pg
            .append_personality_memories(&PersonalityWriteRequest {
                owner: owner.clone(),
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
                sidecar_table: "proxima_test.personality_output_v1",
            })
            .await?;
        let second = pg
            .append_personality_memories(&PersonalityWriteRequest {
                owner: owner.clone(),
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
                sidecar_table: "proxima_test.personality_output_v1",
            })
            .await?;
        pg.append_personality_memories(&PersonalityWriteRequest {
            owner: owner.clone(),
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
            sidecar_table: "proxima_test.personality_output_v1",
        })
        .await?;

        let sibling_root_id = current_root_perspective_memory_id(&pg, sibling.instance_id).await?;
        pg.append_personality_memories(&PersonalityWriteRequest {
            owner: owner.clone(),
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
            sidecar_table: "proxima_test.personality_output_v1",
        })
        .await?;

        let sidecars = vec![
            SidecarSpec {
                schema_id: SchemaId::new("proxima-test/output-v1".into()),
                sidecar_table: "proxima_test.personality_output_v1".into(),
            },
            SidecarSpec {
                schema_id: SchemaId::new("proxima-test/engineer-self-v1".into()),
                sidecar_table: "proxima_test.personality_output_v1".into(),
            },
            SidecarSpec {
                schema_id: SchemaId::new(ROOT_PERSONALITY_PERSPECTIVE_SCHEMA_ID.into()),
                sidecar_table: "proxima_core.root_personality_perspective_v1".into(),
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
    result.expect("perspective heads should filter superseded, root, self, and sibling rows");
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
        let seed = seed_test_personality(&pg, &owner).await?;
        let root_id = current_root_perspective_memory_id(&pg, seed.instance_id).await?;
        let fact = pg.ingest_event_atomic(&fact_draft(owner.clone())).await?;
        let descriptors = core_relation_descriptors();
        let resolve = |id: &str| {
            let descriptor = descriptors
                .iter()
                .find(|descriptor| descriptor.relation == id)
                .expect("relation registered");
            RegisteredRelation {
                descriptor,
                payload_sidecar_table: None,
            }
        };
        let provenance_relation = resolve(CORE_DERIVED_FROM_RELATION);
        let supersedes_relation = resolve(CORE_SUPERSEDES_RELATION);
        let authored_relation = resolve(CORE_AUTHORED_RELATION);
        let instance = PersonalityRef::new(seed.instance_id);

        let abstraction = pg
            .append_personality_memories(&PersonalityWriteRequest {
                owner: owner.clone(),
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
                authored_relation,
                current_root_perspective_memory_id: root_id,
                wake_chain_depth: WakeChainDepth::new(1),
                memories: &[memory_draft(
                    PersonalityMemoryKind::Perspective,
                    "perspective",
                    vec![abstraction_id],
                )],
                sidecar_table: "proxima_test.personality_output_v1",
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
    let Some((pg, db)) = fresh_pg().await else {
        return;
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        apply_personality_output_sidecar(pg.pool()).await?;

        let owner = owner_fixture();
        let seed = seed_test_personality(&pg, &owner).await?;
        let root_id = current_root_perspective_memory_id(&pg, seed.instance_id).await?;
        let fact = pg.ingest_event_atomic(&fact_draft(owner.clone())).await?;
        let descriptors = core_relation_descriptors();
        let resolve = |id: &str| {
            let descriptor = descriptors
                .iter()
                .find(|descriptor| descriptor.relation == id)
                .expect("relation registered");
            RegisteredRelation {
                descriptor,
                payload_sidecar_table: None,
            }
        };
        let provenance_relation = resolve(CORE_DERIVED_FROM_RELATION);
        let supersedes_relation = resolve(CORE_SUPERSEDES_RELATION);
        let authored_relation = resolve(CORE_AUTHORED_RELATION);

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
                sidecar_table: "proxima_test.personality_output_v1",
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
    let Some((pg, db)) = fresh_pg().await else {
        return;
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        apply_personality_output_sidecar(pg.pool()).await?;

        let owner = owner_fixture();
        let seed = seed_test_personality(&pg, &owner).await?;
        let root_id = current_root_perspective_memory_id(&pg, seed.instance_id).await?;
        let fact = pg.ingest_event_atomic(&fact_draft(owner.clone())).await?;
        let descriptors = core_relation_descriptors();
        let resolve = |id: &str| {
            let descriptor = descriptors
                .iter()
                .find(|descriptor| descriptor.relation == id)
                .expect("relation registered");
            RegisteredRelation {
                descriptor,
                payload_sidecar_table: None,
            }
        };
        let provenance_relation = resolve(CORE_DERIVED_FROM_RELATION);
        let supersedes_relation = resolve(CORE_SUPERSEDES_RELATION);
        let authored_relation = resolve(CORE_AUTHORED_RELATION);
        let instance = PersonalityRef::new(seed.instance_id);

        let abstraction = pg
            .append_personality_memories(&PersonalityWriteRequest {
                owner: owner.clone(),
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
                authored_relation,
                current_root_perspective_memory_id: root_id,
                wake_chain_depth: WakeChainDepth::new(1),
                memories: &[memory_draft(
                    PersonalityMemoryKind::Perspective,
                    "perspective",
                    vec![abstraction_id],
                )],
                sidecar_table: "proxima_test.personality_output_v1",
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
