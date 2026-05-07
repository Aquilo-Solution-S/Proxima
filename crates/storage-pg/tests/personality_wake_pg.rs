//! PG coverage for the personality wake tables and storage helpers.

mod common;

use common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::personality::{
    InstantiatePersonalityRequest, PersonalityMemoryDraft, PersonalityMemoryKind, PersonalityRef,
    PersonalitySelfDraft, PersonalityWriteRequest, SetWakeConfigRequest, WakeChainDepth,
    WakeFilter,
};
use proxima_core::relation::{
    CORE_DERIVED_FROM_RELATION, CORE_SUPERSEDES_RELATION, core_relation_descriptors,
};
use proxima_core::storage::Storage;
use proxima_core::verbs::event_ingest::{CitationMappingHint, CitedObjectHint, EventDraft};
use proxima_core::{
    MemoryId, RegisteredRelation, SchemaId, SchemaVersion, SourceBatchId, SourceId,
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
        }),
    }
}

fn fact_draft(owner: proxima_core::Owner) -> EventDraft {
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
async fn personality_instance_config_cursor_and_invocation_round_trip() {
    let Some((pg, db)) = fresh_pg().await else {
        return;
    };

    let result: Result<(), Box<dyn std::error::Error>> = async {
        pg.run_migrations().await?;
        apply_self_sidecar(pg.pool()).await?;

        let owner = owner_fixture();
        let req = InstantiatePersonalityRequest {
            owner: owner.clone(),
            personality_type_id: "proxima-test/personality-v1".into(),
            payload_overrides: None,
        };
        let initial_filters = vec![WakeFilter::on_memory(SchemaId::new(
            "proxima-test/fact-v1".into(),
        ))];

        let response = pg
            .instantiate_personality(
                &req,
                &self_draft("Engineer A"),
                "proxima_test.personality_self_v1",
                &initial_filters,
            )
            .await?;
        let instance = PersonalityRef::new(req.personality_type_id.clone(), response.instance_id);

        let listed = pg
            .list_personality_instances(&owner, Some("proxima-test/personality-v1"))
            .await?;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].display_name, "Engineer A");
        assert_eq!(listed[0].status, "active");
        assert_eq!(listed[0].wake_filters, initial_filters);

        let configs = pg.list_active_wake_configs().await?;
        assert_eq!(configs.len(), 1);
        assert_eq!(
            configs[0].personality_type_id,
            "proxima-test/personality-v1"
        );
        assert_ne!(configs[0].last_considered_seq, Uuid::nil());

        let updated_filters = vec![WakeFilter::OnMemory {
            version: 1,
            schema_id: SchemaId::new("proxima-test/other-fact-v1".into()),
            authored_by: proxima_core::AuthorFilter::External,
            probability: 0.25,
        }];
        let set = pg
            .set_wake_config(&SetWakeConfigRequest {
                owner: owner.clone(),
                personality_type_id: req.personality_type_id.clone(),
                personality_instance_id: response.instance_id,
                wake_filters: updated_filters.clone(),
            })
            .await?;
        assert_eq!(set.status, "active");
        let listed = pg
            .list_personality_instances(&owner, Some("proxima-test/personality-v1"))
            .await?;
        assert_eq!(listed[0].wake_filters, updated_filters);

        pg.mark_wake_config_needs_repair(&owner, &instance).await?;
        let listed = pg
            .list_personality_instances(&owner, Some("proxima-test/personality-v1"))
            .await?;
        assert_eq!(listed[0].status, "needs_repair");

        let seq = configs[0].last_considered_seq;
        assert!(pg.try_begin_wake_invocation(&owner, &instance, seq).await?);
        assert!(
            !pg.try_begin_wake_invocation(&owner, &instance, seq).await?,
            "wake invocation idempotency must reject the same tuple twice"
        );
        pg.finish_wake_invocation(
            &owner,
            &instance,
            seq,
            proxima_core::WakeInvocationStatus::Succeeded,
            1,
            0.0,
        )
        .await?;

        Ok(())
    }
    .await;

    drop(pg);
    let _ = drop_db(&db).await;
    result.expect("personality wake storage round trip failed");
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

        // Sorted ASC by authorship_kind, so "OperatorAtoP" precedes
        // "OperatorFtoA" alphabetically.
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
