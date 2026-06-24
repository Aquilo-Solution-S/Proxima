use crate::common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::storage::{Storage, StorageError};
use proxima_core::{
    AgentDerivationV1, EntityKind, MemoryOperatorKind, Owner, PersonalityInstanceId, SchemaId,
    SchemaVersion, SidecarPayload,
};
use proxima_storage_pg::verbs::derive_append::{DerivedDraft, append_derived_in_tx};
use proxima_storage_pg::{PgSidecarRegistryFrozen, verbs::derive_append::DerivedOutcome};
use sqlx::{Postgres, Transaction};

fn agent_draft(
    memory_id: uuid::Uuid,
    owner: Owner,
    kind: EntityKind,
    _title: &'static str,
    body: &'static str,
    author: Option<PersonalityInstanceId>,
) -> DerivedDraft<'static> {
    DerivedDraft {
        memory_id,
        owner,
        kind,
        author_personality_instance_id: author,
        schema_id: SchemaId::new("core/agent-derivation-v1".into()),
        schema_version: SchemaVersion::new(1),
        text: body.into(),
        operator_kind: MemoryOperatorKind::ExternalAgent,
        model_id: "claude-opus-4.7",
        prompt_version: "mcp-agent-v1",
        supersedes: None,
        embedding: None,
        embedding_model_id: None,
    }
}

fn agent_sidecar(kind: EntityKind, title: &'static str, body: &'static str) -> SidecarPayload {
    let payload = AgentDerivationV1 {
        title: title.into(),
        body: body.into(),
        tags: Vec::new(),
        idempotency_key: None,
        source_memory_ids: Vec::new(),
        model_id: "claude-opus-4.7".into(),
        client_name: "codex".into(),
        client_version: "1".into(),
    };
    match kind {
        EntityKind::Abstraction => SidecarPayload::abstraction(payload),
        EntityKind::Perspective => SidecarPayload::perspective(payload),
        other => panic!("unexpected derived kind in test: {other:?}"),
    }
}

async fn append_with_sidecar(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    draft: &DerivedDraft<'_>,
    sidecar: &SidecarPayload,
) -> Result<DerivedOutcome, StorageError> {
    let sidecars = sidecars.clone();
    let sidecar = sidecar.clone();
    append_derived_in_tx(tx, draft, move |tx, outcome| {
        Box::pin(async move {
            sidecars
                .insert_memory_sidecar(tx, outcome.memory_id, &sidecar)
                .await
        })
    })
    .await
}

#[tokio::test]
async fn external_agent_abstraction_persists_with_replay() -> Result<(), Box<dyn std::error::Error>>
{
    let (pg, db_name) = fresh_pg().await;

    let result = async {
        pg.run_migrations().await?;
        let owner = owner_fixture();
        let memory_id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, b"derive-test-1");
        let draft = agent_draft(
            memory_id,
            owner,
            EntityKind::Abstraction,
            "x",
            "the agent view",
            None,
        );
        let sidecar = agent_sidecar(EntityKind::Abstraction, "x", "the agent view");

        let mut tx = pg.pool().begin().await?;
        let outcome = append_with_sidecar(&mut tx, pg.sidecars(), &draft, &sidecar).await?;
        tx.commit().await?;
        assert_eq!(outcome.memory_id.into_inner(), memory_id);
        assert!(!outcome.idempotent_replay);

        let mut tx = pg.pool().begin().await?;
        let replay = append_with_sidecar(&mut tx, pg.sidecars(), &draft, &sidecar).await?;
        tx.commit().await?;
        assert!(replay.idempotent_replay);

        let row_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM proxima_core.agent_derivation_v1 WHERE memory_id = $1",
        )
        .bind(memory_id)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(row_count, 1);
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn external_agent_perspective_persists() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let result = async {
        pg.run_migrations().await?;
        let memory_id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, b"derive-test-2");
        let draft = agent_draft(
            memory_id,
            owner_fixture(),
            EntityKind::Perspective,
            "p",
            "perspective body",
            None,
        );
        let sidecar = agent_sidecar(EntityKind::Perspective, "p", "perspective body");
        let mut tx = pg.pool().begin().await?;
        append_with_sidecar(&mut tx, pg.sidecars(), &draft, &sidecar).await?;
        tx.commit().await?;
        let kind: EntityKind =
            sqlx::query_scalar("SELECT kind FROM proxima_core.memories WHERE memory_id = $1")
                .bind(memory_id)
                .fetch_one(pg.pool())
                .await?;
        assert_eq!(kind, EntityKind::Perspective);
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn external_agent_abstraction_stamps_author_without_change_event_author()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let result = async {
        pg.run_migrations().await?;

        let owner = owner_fixture();
        let subject = owner.clone();
        let personality = pg.ensure_subject_personality(&owner, &subject).await?;

        let authored_id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, b"derive-author-test-1");
        let authored = agent_draft(
            authored_id,
            owner.clone(),
            EntityKind::Abstraction,
            "authored",
            "authored abstraction",
            Some(personality.instance_id),
        );
        let authored_sidecar =
            agent_sidecar(EntityKind::Abstraction, "authored", "authored abstraction");

        let mut tx = pg.pool().begin().await?;
        append_with_sidecar(&mut tx, pg.sidecars(), &authored, &authored_sidecar).await?;
        tx.commit().await?;

        let stamped: uuid::Uuid = sqlx::query_scalar(
            "SELECT personality_instance_id
             FROM proxima_core.memories
             WHERE memory_id = $1",
        )
        .bind(authored_id)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(stamped, personality.instance_id.into_inner());
        assert_ne!(stamped, uuid::Uuid::nil());

        let authored_change_author: Option<uuid::Uuid> = sqlx::query_scalar(
            "SELECT entity_personality_instance_id
             FROM proxima_core.change_event
             WHERE entity_memory_id = $1",
        )
        .bind(authored_id)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(authored_change_author, None);

        let system_id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, b"derive-author-test-2");
        let system = agent_draft(
            system_id,
            owner,
            EntityKind::Abstraction,
            "system",
            "system abstraction",
            None,
        );
        let system_sidecar = agent_sidecar(EntityKind::Abstraction, "system", "system abstraction");

        let mut tx = pg.pool().begin().await?;
        append_with_sidecar(&mut tx, pg.sidecars(), &system, &system_sidecar).await?;
        tx.commit().await?;

        let system_stamped: uuid::Uuid = sqlx::query_scalar(
            "SELECT personality_instance_id
             FROM proxima_core.memories
             WHERE memory_id = $1",
        )
        .bind(system_id)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(system_stamped, uuid::Uuid::nil());

        let system_change_author: Option<uuid::Uuid> = sqlx::query_scalar(
            "SELECT entity_personality_instance_id
             FROM proxima_core.change_event
             WHERE entity_memory_id = $1",
        )
        .bind(system_id)
        .fetch_one(pg.pool())
        .await?;
        assert_eq!(system_change_author, None);

        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}
