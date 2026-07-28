use crate::common::{drop_db, fresh_pg, owner_fixture, owner_write_permit, test_registry};
use proxima_core::storage::StorageError;
use proxima_core::{
    AgentDerivationV1, CORE_DERIVED_FROM_RELATION, DerivedEdgeSpec, EntityKind,
    FlavorRegistryFrozen, InputContractId, MemoryId, MemoryOperatorKind, OperatorId, Owner,
    OwnerRef, RegisteredRelation, SchemaId, SchemaVersion, SidecarPayload, UserId,
};
use proxima_storage_pg::verbs::derive_append::{DerivedDraft, append_derived_with_edges_in_tx};
use proxima_storage_pg::{PgSidecarRegistryFrozen, verbs::derive_append::DerivedOutcome};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

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
        operator_kind: match kind {
            EntityKind::Perspective => MemoryOperatorKind::AtoP,
            EntityKind::Abstraction | EntityKind::Fact | EntityKind::Goal => {
                MemoryOperatorKind::AtoA
            }
        },
        operator_id: OperatorId::new(Uuid::now_v7()),
        input_contract_id: InputContractId::new(Uuid::now_v7()),
        source_batch_id: None,
        model_id: "claude-opus-4.7",
        prompt_version: "mcp-agent-v1",
        supersedes: None,
        lexical_language: None,
        embedding: None,
        embedding_model_id: None,
    }
}

async fn insert_source_abstraction(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    label: &str,
) -> Result<MemoryId, sqlx::Error> {
    let memory_id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, label.as_bytes());
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(owner);
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_kind, owner_id, schema_id, schema_version, kind, text,
             operator_kind, operator_id, input_contract_id, source_batch_id, model_id,
             prompt_version)
         VALUES ($1, $2, $3, 'test/source-abstraction', 1, 'Abstraction', $4,
                 'AtoA', '00000000-0000-0000-0000-000000000571'::uuid,
                 '00000000-0000-0000-0000-000000000572'::uuid, NULL,
                 'test', 'test')
         ON CONFLICT (memory_id) DO NOTHING",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(label)
    .execute(pg.pool_for_tests())
    .await?;
    Ok(MemoryId::new(memory_id))
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
    registry: &FlavorRegistryFrozen,
    draft: &DerivedDraft<'_>,
    sidecar: &SidecarPayload,
    source: MemoryId,
) -> Result<DerivedOutcome, StorageError> {
    let relation = registry
        .resolve_relation(CORE_DERIVED_FROM_RELATION)
        .expect("core derived-from relation registered");
    let edges = [derived_edge(&draft.owner, draft, source, relation)];
    let sidecars = sidecars.clone();
    let sidecar = sidecar.clone();
    let access_kind = match draft.kind {
        EntityKind::Fact => proxima_core::AccessKind::Fact,
        EntityKind::Abstraction => proxima_core::AccessKind::Abstraction,
        EntityKind::Perspective => proxima_core::AccessKind::Perspective,
        EntityKind::Goal => proxima_core::AccessKind::Goal,
    };
    let permit = owner_write_permit(&draft.owner, access_kind)
        .await
        .map_err(|err| StorageError::Internal(err.to_string()))?;
    append_derived_with_edges_in_tx(tx, &permit, draft, &edges, move |tx, outcome| {
        Box::pin(async move {
            sidecars
                .insert_memory_sidecar(tx, outcome.memory_id, &sidecar)
                .await
        })
    })
    .await
}

fn derived_edge<'a>(
    owner: &'a Owner,
    draft: &DerivedDraft<'_>,
    source: MemoryId,
    relation: RegisteredRelation<'a>,
) -> DerivedEdgeSpec<'a> {
    let target_kind = draft.operator_kind.phase().input_kind();
    DerivedEdgeSpec {
        owner,
        relation,
        source_kind: draft.kind,
        source_memory_id: MemoryId::new(draft.memory_id),
        target_kind,
        target_memory_id: source,
        authorship_kind: draft.operator_kind.edge_authorship(),
        authorship_owner_memory_id: Some(source),
        sidecar_payload: None,
    }
}

#[tokio::test]
async fn external_agent_abstraction_persists_with_replay() -> Result<(), Box<dyn std::error::Error>>
{
    let (pg, db_name) = fresh_pg().await;

    let result = async {
        pg.run_migrations().await?;
        let owner = owner_fixture();
        let registry = test_registry();
        let source = insert_source_abstraction(&pg, &owner, "derive-test-1-source").await?;
        let memory_id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, b"derive-test-1");
        let draft = agent_draft(
            memory_id,
            owner,
            EntityKind::Abstraction,
            "x",
            "the agent view",
        );
        let sidecar = agent_sidecar(EntityKind::Abstraction, "x", "the agent view");

        let mut tx = pg.pool_for_tests().begin().await?;
        let outcome =
            append_with_sidecar(&mut tx, pg.sidecars(), &registry, &draft, &sidecar, source)
                .await?;
        tx.commit().await?;
        assert_eq!(outcome.memory_id.into_inner(), memory_id);
        assert!(!outcome.idempotent_replay);

        let mut tx = pg.pool_for_tests().begin().await?;
        let replay =
            append_with_sidecar(&mut tx, pg.sidecars(), &registry, &draft, &sidecar, source)
                .await?;
        tx.commit().await?;
        assert!(replay.idempotent_replay);

        let row_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM proxima_core.agent_derivation_v1 WHERE memory_id = $1",
        )
        .bind(memory_id)
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(row_count, 1);
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}

#[tokio::test]
async fn derived_replay_rejects_mismatched_input_contract() -> Result<(), Box<dyn std::error::Error>>
{
    let (pg, db_name) = fresh_pg().await;

    let result = async {
        pg.run_migrations().await?;
        let owner = owner_fixture();
        let registry = test_registry();
        let source =
            insert_source_abstraction(&pg, &owner, "derive-mismatch-contract-source").await?;
        let memory_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, b"derive-mismatch-contract");
        let draft = agent_draft(
            memory_id,
            owner,
            EntityKind::Abstraction,
            "x",
            "the agent view",
        );
        let sidecar = agent_sidecar(EntityKind::Abstraction, "x", "the agent view");

        let mut tx = pg.pool_for_tests().begin().await?;
        append_with_sidecar(&mut tx, pg.sidecars(), &registry, &draft, &sidecar, source).await?;
        tx.commit().await?;

        let mut mismatch = draft.clone();
        mismatch.input_contract_id = InputContractId::new(Uuid::now_v7());
        let mut tx = pg.pool_for_tests().begin().await?;
        let err = append_with_sidecar(
            &mut tx,
            pg.sidecars(),
            &registry,
            &mismatch,
            &sidecar,
            source,
        )
        .await
        .expect_err("mismatched proof metadata must not replay");
        tx.rollback().await?;
        assert!(
            err.to_string()
                .contains("derived memory idempotent replay proof mismatch")
        );
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
        let owner = owner_fixture();
        let registry = test_registry();
        let source = insert_source_abstraction(&pg, &owner, "derive-test-2-source").await?;
        let memory_id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, b"derive-test-2");
        let draft = agent_draft(
            memory_id,
            owner,
            EntityKind::Perspective,
            "p",
            "perspective body",
        );
        let sidecar = agent_sidecar(EntityKind::Perspective, "p", "perspective body");
        let mut tx = pg.pool_for_tests().begin().await?;
        append_with_sidecar(&mut tx, pg.sidecars(), &registry, &draft, &sidecar, source).await?;
        tx.commit().await?;
        let kind: EntityKind =
            sqlx::query_scalar("SELECT kind FROM proxima_core.memories WHERE memory_id = $1")
                .bind(memory_id)
                .fetch_one(pg.pool_for_tests())
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
        let registry = test_registry();
        let victim_source = insert_source_abstraction(&pg, &victim, "derive-victim-source").await?;
        let attacker_source =
            insert_source_abstraction(&pg, &attacker, "derive-attacker-source").await?;

        let victim_prior_id = uuid::Uuid::now_v7();
        let victim_prior = agent_draft(
            victim_prior_id,
            victim,
            EntityKind::Abstraction,
            "victim",
            "victim prior",
        );
        let victim_sidecar = agent_sidecar(EntityKind::Abstraction, "victim", "victim prior");
        let mut tx = pg.pool_for_tests().begin().await?;
        append_with_sidecar(
            &mut tx,
            pg.sidecars(),
            &registry,
            &victim_prior,
            &victim_sidecar,
            victim_source,
        )
        .await?;
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
        let mut tx = pg.pool_for_tests().begin().await?;
        let err = append_with_sidecar(
            &mut tx,
            pg.sidecars(),
            &registry,
            &foreign,
            &foreign_sidecar,
            attacker_source,
        )
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
        let mut tx = pg.pool_for_tests().begin().await?;
        append_with_sidecar(
            &mut tx,
            pg.sidecars(),
            &registry,
            &attacker_prior,
            &attacker_sidecar,
            attacker_source,
        )
        .await?;
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
        let mut tx = pg.pool_for_tests().begin().await?;
        append_with_sidecar(
            &mut tx,
            pg.sidecars(),
            &registry,
            &same_owner,
            &same_owner_sidecar,
            attacker_source,
        )
        .await?;
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
        let mut tx = pg.pool_for_tests().begin().await?;
        append_with_sidecar(
            &mut tx,
            pg.sidecars(),
            &registry,
            &attacker_perspective,
            &perspective_sidecar,
            attacker_source,
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
        let mut tx = pg.pool_for_tests().begin().await?;
        let err = append_with_sidecar(
            &mut tx,
            pg.sidecars(),
            &registry,
            &wrong_kind,
            &wrong_kind_sidecar,
            attacker_source,
        )
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
        .fetch_one(pg.pool_for_tests())
        .await
}

async fn stored_supersedes(
    pg: &proxima_storage_pg::PgStorage,
    memory_id: uuid::Uuid,
) -> Result<Option<uuid::Uuid>, sqlx::Error> {
    sqlx::query_scalar("SELECT supersedes FROM proxima_core.memories WHERE memory_id = $1")
        .bind(memory_id)
        .fetch_one(pg.pool_for_tests())
        .await
}

async fn supersedes_pointer_count(
    pg: &proxima_storage_pg::PgStorage,
    prior: uuid::Uuid,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT count(*) FROM proxima_core.memories WHERE supersedes = $1")
        .bind(prior)
        .fetch_one(pg.pool_for_tests())
        .await
}

/// A derive replay must not mutate the active-language set: the replayed
/// pipeline re-arrives with whatever language it resolves today (possibly
/// one this database has never seen, possibly one that does not exist),
/// and the whole point of `idempotent_replay` is that nothing happened.
#[tokio::test]
async fn derived_replay_does_not_register_its_language() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;

    let result = async {
        pg.run_migrations().await?;
        let owner = owner_fixture();
        let registry = test_registry();
        let source = insert_source_abstraction(&pg, &owner, "derive-lang-replay-source").await?;
        let memory_id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, b"derive-lang-replay");
        let operator_id = OperatorId::new(uuid::Uuid::new_v5(
            &uuid::Uuid::NAMESPACE_OID,
            b"derive-lang-replay-op",
        ));
        let input_contract_id = InputContractId::new(uuid::Uuid::new_v5(
            &uuid::Uuid::NAMESPACE_OID,
            b"derive-lang-replay-contract",
        ));
        let draft_in = |language: Option<&'static str>| DerivedDraft {
            memory_id,
            owner,
            kind: EntityKind::Abstraction,
            schema_id: SchemaId::new("core/agent-derivation-v1".into()),
            schema_version: SchemaVersion::new(1),
            text: "die zusammengefasste Sicht".into(),
            operator_kind: MemoryOperatorKind::AtoA,
            operator_id,
            input_contract_id,
            source_batch_id: None,
            model_id: "claude-opus-4.7",
            prompt_version: "mcp-agent-v1",
            supersedes: None,
            lexical_language: language,
            embedding: None,
            embedding_model_id: None,
        };
        let sidecar = agent_sidecar(EntityKind::Abstraction, "x", "die zusammengefasste Sicht");

        let mut tx = pg.pool_for_tests().begin().await?;
        let outcome = append_with_sidecar(
            &mut tx,
            pg.sidecars(),
            &registry,
            &draft_in(Some("german")),
            &sidecar,
            source,
        )
        .await?;
        tx.commit().await?;
        assert!(!outcome.idempotent_replay);
        let stamped: String = sqlx::query_scalar(
            "SELECT lexical_language::text FROM proxima_core.memories WHERE memory_id = $1",
        )
        .bind(memory_id)
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(
            stamped, "german",
            "the fresh insert must stamp and register"
        );

        // Replay with a DIFFERENT language: still a replay, and the
        // language set must not grow by a configuration nothing is
        // stamped with.
        let mut tx = pg.pool_for_tests().begin().await?;
        let replay = append_with_sidecar(
            &mut tx,
            pg.sidecars(),
            &registry,
            &draft_in(Some("italian")),
            &sidecar,
            source,
        )
        .await?;
        tx.commit().await?;
        assert!(replay.idempotent_replay);
        let italian_registered: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM proxima_core.lexical_languages
              WHERE config = 'italian'::regconfig)",
        )
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert!(
            !italian_registered,
            "a replay registered a language no row is stamped with"
        );

        // Replay with a configuration this database does not even have:
        // must no-op like every other replay, not fail validation.
        let mut tx = pg.pool_for_tests().begin().await?;
        let replay = append_with_sidecar(
            &mut tx,
            pg.sidecars(),
            &registry,
            &draft_in(Some("klingon")),
            &sidecar,
            source,
        )
        .await?;
        tx.commit().await?;
        assert!(replay.idempotent_replay);

        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result
}
