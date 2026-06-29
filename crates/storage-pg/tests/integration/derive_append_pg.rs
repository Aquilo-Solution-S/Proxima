use crate::common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::storage::StorageError;
use proxima_core::{
    AgentDerivationV1, EntityKind, MemoryId, MemoryOperatorKind, Owner, OwnerRef, SchemaId,
    SchemaVersion, SidecarPayload, UserId,
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
) -> DerivedDraft<'static> {
    DerivedDraft {
        memory_id,
        owner,
        kind,
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
#[allow(clippy::too_many_lines)]
async fn append_derived_in_tx_enforces_supersedes_owner_and_kind()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let result = async {
        pg.run_migrations().await?;
        let attacker = owner_fixture();
        let victim = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));

        let victim_prior_id = uuid::Uuid::now_v7();
        let victim_prior = agent_draft(
            victim_prior_id,
            victim,
            EntityKind::Abstraction,
            "victim",
            "victim prior",
        );
        let victim_sidecar = agent_sidecar(EntityKind::Abstraction, "victim", "victim prior");
        let mut tx = pg.pool().begin().await?;
        append_with_sidecar(&mut tx, pg.sidecars(), &victim_prior, &victim_sidecar).await?;
        tx.commit().await?;

        let mut foreign = agent_draft(
            uuid::Uuid::now_v7(),
            attacker,
            EntityKind::Abstraction,
            "foreign",
            "foreign successor",
        );
        foreign.supersedes = Some(MemoryId::new(victim_prior_id));
        let foreign_sidecar =
            agent_sidecar(EntityKind::Abstraction, "foreign", "foreign successor");
        let mut tx = pg.pool().begin().await?;
        let err = append_with_sidecar(&mut tx, pg.sidecars(), &foreign, &foreign_sidecar)
            .await
            .expect_err("foreign supersedes target must be rejected");
        tx.rollback().await?;
        assert_supersedes_constraint(err);
        assert_eq!(memory_count(&pg, foreign.memory_id).await?, 0);
        assert_eq!(supersedes_pointer_count(&pg, victim_prior_id).await?, 0);

        let attacker_prior_id = uuid::Uuid::now_v7();
        let attacker_prior = agent_draft(
            attacker_prior_id,
            attacker,
            EntityKind::Abstraction,
            "attacker",
            "attacker prior",
        );
        let attacker_sidecar = agent_sidecar(EntityKind::Abstraction, "attacker", "attacker prior");
        let mut tx = pg.pool().begin().await?;
        append_with_sidecar(&mut tx, pg.sidecars(), &attacker_prior, &attacker_sidecar).await?;
        tx.commit().await?;

        let mut same_owner = agent_draft(
            uuid::Uuid::now_v7(),
            attacker,
            EntityKind::Abstraction,
            "same-owner",
            "same-owner successor",
        );
        same_owner.supersedes = Some(MemoryId::new(attacker_prior_id));
        let same_owner_sidecar = agent_sidecar(
            EntityKind::Abstraction,
            "same-owner",
            "same-owner successor",
        );
        let mut tx = pg.pool().begin().await?;
        append_with_sidecar(&mut tx, pg.sidecars(), &same_owner, &same_owner_sidecar).await?;
        tx.commit().await?;
        assert_eq!(
            stored_supersedes(&pg, same_owner.memory_id).await?,
            Some(attacker_prior_id)
        );

        let attacker_perspective_id = uuid::Uuid::now_v7();
        let attacker_perspective = agent_draft(
            attacker_perspective_id,
            attacker,
            EntityKind::Perspective,
            "perspective",
            "attacker perspective",
        );
        let perspective_sidecar = agent_sidecar(
            EntityKind::Perspective,
            "perspective",
            "attacker perspective",
        );
        let mut tx = pg.pool().begin().await?;
        append_with_sidecar(
            &mut tx,
            pg.sidecars(),
            &attacker_perspective,
            &perspective_sidecar,
        )
        .await?;
        tx.commit().await?;

        let mut wrong_kind = agent_draft(
            uuid::Uuid::now_v7(),
            attacker,
            EntityKind::Abstraction,
            "wrong-kind",
            "wrong-kind successor",
        );
        wrong_kind.supersedes = Some(MemoryId::new(attacker_perspective_id));
        let wrong_kind_sidecar = agent_sidecar(
            EntityKind::Abstraction,
            "wrong-kind",
            "wrong-kind successor",
        );
        let mut tx = pg.pool().begin().await?;
        let err = append_with_sidecar(&mut tx, pg.sidecars(), &wrong_kind, &wrong_kind_sidecar)
            .await
            .expect_err("same-owner wrong-kind supersedes target must be rejected");
        tx.rollback().await?;
        assert_supersedes_constraint(err);
        assert_eq!(memory_count(&pg, wrong_kind.memory_id).await?, 0);
        assert_eq!(
            supersedes_pointer_count(&pg, attacker_perspective_id).await?,
            0
        );

        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

fn assert_supersedes_constraint(err: StorageError) {
    match err {
        StorageError::ConstraintViolation(message) => assert_eq!(
            message,
            "supersedes crosses Owner boundary or does not exist"
        ),
        other => panic!("expected supersedes constraint violation, got {other:?}"),
    }
}

async fn memory_count(
    pg: &proxima_storage_pg::PgStorage,
    memory_id: uuid::Uuid,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT count(*) FROM proxima_core.memories WHERE memory_id = $1")
        .bind(memory_id)
        .fetch_one(pg.pool())
        .await
}

async fn stored_supersedes(
    pg: &proxima_storage_pg::PgStorage,
    memory_id: uuid::Uuid,
) -> Result<Option<uuid::Uuid>, sqlx::Error> {
    sqlx::query_scalar("SELECT supersedes FROM proxima_core.memories WHERE memory_id = $1")
        .bind(memory_id)
        .fetch_one(pg.pool())
        .await
}

async fn supersedes_pointer_count(
    pg: &proxima_storage_pg::PgStorage,
    prior: uuid::Uuid,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT count(*) FROM proxima_core.memories WHERE supersedes = $1")
        .bind(prior)
        .fetch_one(pg.pool())
        .await
}
