//! Derived replay requires matching head metadata and pins; content stays immutable.
//!
//! Crate-internal for the same reason as the origin-proof tests beside it.

use super::{DerivedDraft, DerivedOutcome};
use crate::PgStorage;
use crate::test_fixtures::create_core_db;
use proxiima_core::storage_ports::{FactIngestPort, MemoryAuthoringPort, OwnerWritePermit};
use proxiima_core::verbs::fact_ingest::FactWriteCommand;
use proxiima_core::{
    AccessKind, ContentFingerprint, EdgeEndpoint, EntityKind, GoalId, IdempotencyKey, KeyScope,
    MemoryOperatorKind, OwnerId, RequestKey, SchemaId, SchemaVersion, StorageError, UserId,
};
use proxiima_pg_testkit::{db_url, drop_db};
use uuid::Uuid;

fn fact_draft() -> FactWriteCommand {
    FactWriteCommand {
        schema_id: SchemaId::new("p6/fact".into()),
        schema_version: SchemaVersion::new(1),
        handle: None,
        source_id: None,
        ingest_key: None,
        payload: Vec::new(),
        rendered_text: None,
        lexical_language: None,
        receipt: None,
        occurred_at: None,
        citation: None,
        additional_references: Vec::new(),
        refs: Vec::new(),
        blob_id: None,
        kind: proxiima_core::MemoryKind::Fact,
        request: None,
    }
}

fn derived_draft(owner: OwnerId, handle: Uuid) -> DerivedDraft<'static> {
    DerivedDraft {
        store_number: 1,
        memory_id: handle,
        owner,
        kind: EntityKind::Abstraction,
        schema_id: SchemaId::new("p6/derived".into()),
        schema_version: SchemaVersion::new(1),
        text: "derived".into(),
        operator_kind: MemoryOperatorKind::FtoA,
        supersedes: None,
        lexical_language: None,
        queued_spaces: &[],
        guided_by: &[],
        request: None,
    }
}

fn perspective_draft(owner: OwnerId, handle: Uuid, guided_by: &[GoalId]) -> DerivedDraft<'_> {
    let mut draft = derived_draft(owner, handle);
    draft.kind = EntityKind::Perspective;
    draft.operator_kind = MemoryOperatorKind::AtoP;
    draft.guided_by = guided_by;
    draft
}

fn keyed_perspective(owner: OwnerId, handle: Uuid, guided_by: &[GoalId]) -> DerivedDraft<'_> {
    let mut draft = perspective_draft(owner, handle, guided_by);
    draft.request = Some(RequestKey::new(
        KeyScope::Perspective,
        IdempotencyKey::new("guided-replay").expect("valid key"),
        ContentFingerprint::builder(KeyScope::Perspective)
            .text("key", "guided-replay")
            .build(),
    ));
    draft
}

async fn goal_target(pool: &sqlx::PgPool, owner: OwnerId) -> Result<GoalId, sqlx::Error> {
    let handle = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxiima_core.goal_head (handle, schema_id, owner_id, t)
         VALUES ($1, 'core/task-goal-v1', $2, $1)",
    )
    .bind(handle)
    .bind(owner.into_inner())
    .execute(pool)
    .await?;
    let t: Uuid = sqlx::query_scalar(
        "INSERT INTO proxiima_core.goal
             (handle, t, owner_id, title, state, request_id, motivated_by, \"by\")
         VALUES ($1, $1, $2, 'replay target', 'Active', $3, '{}',
                 '00000000-0000-0000-0000-000000000509'::uuid)
         RETURNING t",
    )
    .bind(handle)
    .bind(owner.into_inner())
    .bind(format!("replay-target-{handle}"))
    .fetch_one(pool)
    .await?;
    Ok(GoalId::new(t))
}

/// The four steps a derived write is: the origin proof, the reference-kind
/// proof, the append, and the declared-index assertion.
///
/// [`crate::ports::memory`] and [`crate::ports::write_session`] run exactly
/// these (with a sketch write between the last two). These tests are about
/// the four proofs, none of which reads the sidecar payload, so they compose
/// them directly rather than through a payload registration that would only
/// stand between the assertion and what it asserts.
async fn append_with_edges(
    pool: &sqlx::PgPool,
    permit: &OwnerWritePermit,
    draft: &DerivedDraft<'_>,
    origins: &[EdgeEndpoint],
    references: &[EdgeEndpoint],
) -> Result<DerivedOutcome, StorageError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|err| StorageError::Internal(err.to_string()))?;
    super::validate_derived_origins_in_tx(&mut tx, draft, origins, references).await?;
    super::validate_derived_reference_kinds_in_tx(&mut tx, draft, origins, references).await?;
    let outcome = super::append_derived_with_content_payloads_in_tx(
        &mut tx,
        permit,
        draft,
        super::DerivedAdmissionInput {
            origins,
            references,
            sidecar_tables: &[],
            scopes: &[],
            content: super::ContentResolution {
                content_id: None,
                payloads: None,
            },
        },
        |_, _| Box::pin(async { Ok(()) }),
    )
    .await?;
    super::assert_derived_index_rows(&mut tx, draft, &outcome, origins, references).await?;
    tx.commit()
        .await
        .map_err(|err| StorageError::Internal(err.to_string()))?;
    Ok(outcome)
}

async fn fresh_pg() -> (String, PgStorage) {
    let db_name = format!("proxiima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let pg = PgStorage::connect_for_tests(db_url(&db_name))
        .await
        .expect("connect");
    (db_name, pg)
}

#[tokio::test]
async fn same_first_originsnd_refs_replay() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Abstraction);
        let pool = pg.pool_for_tests();
        let origin = pg.ingest_fact_atomic(&permit, &fact_draft(), None).await?;
        let callee = pg.ingest_fact_atomic(&permit, &fact_draft(), None).await?;
        let handle = Uuid::now_v7();
        let draft = derived_draft(owner, handle);
        let origins = [EdgeEndpoint::memory(EntityKind::Fact, origin.memory_id)];
        let references = [EdgeEndpoint::memory(EntityKind::Fact, callee.memory_id)];
        let first = append_with_edges(pool, &permit, &draft, &origins, &references).await?;
        assert!(!first.idempotent_replay);
        let replay = append_with_edges(pool, &permit, &draft, &origins, &references).await?;
        assert!(replay.idempotent_replay);
        assert_eq!(replay.memory_id, first.memory_id);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("same pins must replay");
}

#[tokio::test]
async fn changed_perspective_guidance_appends_with_same_origins() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Perspective);
        let pool = pg.pool_for_tests();
        let fact = pg.ingest_fact_atomic(&permit, &fact_draft(), None).await?;
        let mut abstraction = fact_draft();
        abstraction.kind = proxiima_core::MemoryKind::Abstraction;
        abstraction.additional_references =
            vec![EdgeEndpoint::memory(EntityKind::Fact, fact.memory_id)];
        let abstraction = pg.ingest_fact_atomic(&permit, &abstraction, None).await?;
        let goal_one = goal_target(pool, owner).await?;
        let goal_two = goal_target(pool, owner).await?;
        let handle = Uuid::now_v7();
        let origins = [EdgeEndpoint::memory(
            EntityKind::Abstraction,
            abstraction.memory_id,
        )];
        let first_guidance = [goal_one];
        let first_draft = perspective_draft(owner, handle, &first_guidance);
        let first = append_with_edges(pool, &permit, &first_draft, &origins, &[]).await?;
        let changed_guidance = [goal_two];
        let changed_draft = perspective_draft(owner, handle, &changed_guidance);
        let second = append_with_edges(pool, &permit, &changed_draft, &origins, &[]).await?;
        assert!(!first.idempotent_replay);
        assert!(!second.idempotent_replay);
        assert_ne!(first.memory_id, second.memory_id);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("changed guidance must append a new Perspective version");
}

async fn head_snapshot(
    pool: &sqlx::PgPool,
    handle: Uuid,
) -> Result<(Uuid, String, String, Uuid, i64), sqlx::Error> {
    let row = sqlx::query_as::<_, (Uuid, String, String, Uuid)>(
        "SELECT t, kind::text, schema_id, owner_id
           FROM proxiima_core.memory_head WHERE handle = $1",
    )
    .bind(handle)
    .fetch_one(pool)
    .await?;
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM proxiima_core.memory WHERE handle = $1")
            .bind(handle)
            .fetch_one(pool)
            .await?;
    Ok((row.0, row.1, row.2, row.3, count))
}

#[tokio::test]
async fn replay_metadata_mismatch_never_replays_or_mutates() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let other_owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Abstraction);
        let other_permit = OwnerWritePermit::new_for_tests(other_owner, AccessKind::Abstraction);
        let pool = pg.pool_for_tests();
        let fact = pg.ingest_fact_atomic(&permit, &fact_draft(), None).await?;
        let handle = Uuid::now_v7();
        let origins = [EdgeEndpoint::memory(EntityKind::Fact, fact.memory_id)];
        let first =
            append_with_edges(pool, &permit, &derived_draft(owner, handle), &origins, &[]).await?;
        let before = head_snapshot(pool, handle).await?;

        let wrong_owner = derived_draft(other_owner, handle);
        let owner_err = append_with_edges(pool, &other_permit, &wrong_owner, &origins, &[])
            .await
            .expect_err("wrong owner must not replay");
        assert!(matches!(owner_err, StorageError::ConstraintViolation(_)));
        assert_eq!(head_snapshot(pool, handle).await?, before);

        let premise = first.memory_id;
        let premise_origin = [EdgeEndpoint::memory(EntityKind::Abstraction, premise)];
        let second_handle = Uuid::now_v7();
        let mut second_draft = derived_draft(owner, second_handle);
        second_draft.operator_kind = MemoryOperatorKind::AtoA;
        let second = append_with_edges(pool, &permit, &second_draft, &premise_origin, &[]).await?;
        let before_kind = head_snapshot(pool, second_handle).await?;
        let mut wrong_kind = derived_draft(owner, second_handle);
        wrong_kind.kind = EntityKind::Perspective;
        wrong_kind.operator_kind = MemoryOperatorKind::AtoP;
        let kind_err = append_with_edges(pool, &permit, &wrong_kind, &premise_origin, &[])
            .await
            .expect_err("wrong output kind must not replay");
        assert!(matches!(kind_err, StorageError::ConstraintViolation(_)));
        assert_eq!(head_snapshot(pool, second_handle).await?, before_kind);

        let schema_handle = Uuid::now_v7();
        let schema_first = append_with_edges(
            pool,
            &permit,
            &derived_draft(owner, schema_handle),
            &origins,
            &[],
        )
        .await?;
        let before_schema = head_snapshot(pool, schema_handle).await?;
        let mut wrong_schema = derived_draft(owner, schema_handle);
        wrong_schema.schema_id = SchemaId::new("p6/other-derived".into());
        let schema_err = append_with_edges(pool, &permit, &wrong_schema, &origins, &[])
            .await
            .expect_err("wrong schema must not replay");
        assert!(matches!(schema_err, StorageError::ConstraintViolation(_)));
        assert_eq!(head_snapshot(pool, schema_handle).await?, before_schema);
        assert_eq!(schema_first.memory_id.into_inner(), before_schema.0);
        assert_eq!(second.memory_id.into_inner(), before_kind.0);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("metadata mismatch replay gate");
}

#[tokio::test]
async fn same_origins_different_refs_is_conflict() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Abstraction);
        let pool = pg.pool_for_tests();
        let origin = pg.ingest_fact_atomic(&permit, &fact_draft(), None).await?;
        let callee_a = pg.ingest_fact_atomic(&permit, &fact_draft(), None).await?;
        let callee_b = pg.ingest_fact_atomic(&permit, &fact_draft(), None).await?;
        let handle = Uuid::now_v7();
        let draft = derived_draft(owner, handle);
        let origins = [EdgeEndpoint::memory(EntityKind::Fact, origin.memory_id)];
        let first = append_with_edges(
            pool,
            &permit,
            &draft,
            &origins,
            &[EdgeEndpoint::memory(EntityKind::Fact, callee_a.memory_id)],
        )
        .await?;
        assert!(!first.idempotent_replay);
        let err = append_with_edges(
            pool,
            &permit,
            &draft,
            &origins,
            &[EdgeEndpoint::memory(EntityKind::Fact, callee_b.memory_id)],
        )
        .await
        .expect_err("changed refs must not replay");
        match err {
            StorageError::ReplayChanged(changed) => {
                assert_eq!(changed, proxiima_core::ReplayDrift::DerivedRefs);
            }
            other => panic!("expected ReplayChanged, got {other:?}"),
        }
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("ref mismatch must Conflict");
}

#[tokio::test]
async fn changed_guidance_appends_and_exact_guidance_replays() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Perspective);
        let pool = pg.pool_for_tests();
        let fact = pg.ingest_fact_atomic(&permit, &fact_draft(), None).await?;
        let mut abstraction = fact_draft();
        abstraction.kind = proxiima_core::MemoryKind::Abstraction;
        abstraction.additional_references =
            vec![EdgeEndpoint::memory(EntityKind::Fact, fact.memory_id)];
        let origin = pg.ingest_fact_atomic(&permit, &abstraction, None).await?;
        let goal_a = goal_target(pool, owner).await?;
        let goal_b = goal_target(pool, owner).await?;
        let handle = Uuid::now_v7();
        let origins = [EdgeEndpoint::memory(
            EntityKind::Abstraction,
            origin.memory_id,
        )];
        let guidance_a = [goal_a];
        let guidance_b = [goal_b];

        let first = append_with_edges(
            pool,
            &permit,
            &perspective_draft(owner, handle, &guidance_a),
            &origins,
            &[],
        )
        .await?;
        let replay = append_with_edges(
            pool,
            &permit,
            &perspective_draft(owner, handle, &guidance_a),
            &origins,
            &[],
        )
        .await?;
        assert!(replay.idempotent_replay);
        assert_eq!(replay.memory_id, first.memory_id);

        let changed = append_with_edges(
            pool,
            &permit,
            &perspective_draft(owner, handle, &guidance_b),
            &origins,
            &[],
        )
        .await?;
        assert!(!changed.idempotent_replay);
        assert_ne!(changed.memory_id, first.memory_id);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("changed guidance must append while exact guidance replays");
}

#[tokio::test]
async fn keyed_reuse_with_changed_guidance_conflicts() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Perspective);
        let pool = pg.pool_for_tests();
        let fact = pg.ingest_fact_atomic(&permit, &fact_draft(), None).await?;
        let mut abstraction = fact_draft();
        abstraction.kind = proxiima_core::MemoryKind::Abstraction;
        abstraction.additional_references =
            vec![EdgeEndpoint::memory(EntityKind::Fact, fact.memory_id)];
        let origin = pg.ingest_fact_atomic(&permit, &abstraction, None).await?;
        let goal_a = goal_target(pool, owner).await?;
        let goal_b = goal_target(pool, owner).await?;
        let handle = Uuid::now_v7();
        let origins = [EdgeEndpoint::memory(
            EntityKind::Abstraction,
            origin.memory_id,
        )];
        let guided_a = [goal_a];
        let guided_b = [goal_b];
        let first = append_with_edges(
            pool,
            &permit,
            &keyed_perspective(owner, handle, &guided_a),
            &origins,
            &[],
        )
        .await?;
        assert!(!first.idempotent_replay);
        let err = append_with_edges(
            pool,
            &permit,
            &keyed_perspective(owner, handle, &guided_b),
            &origins,
            &[],
        )
        .await
        .expect_err("changed guidance must not reuse an idempotency key");
        assert!(matches!(err, StorageError::ReplayChanged(_)), "got {err:?}");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("keyed guidance mismatch must conflict");
}

/// A new origin set appends a new version under the same handle. Each
/// version's request key then replays that version even after its input was
/// forgotten and the head has moved past it: the ledger, not the head's
/// pins, says it is a replay (#488 review r3).
#[tokio::test]
async fn different_first_originsppend_new_t_and_each_key_replays_its_version() {
    let (db_name, pg) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Abstraction);
        let fact_permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let pool = pg.pool_for_tests();
        let origin_a = pg
            .ingest_fact_atomic(&fact_permit, &fact_draft(), None)
            .await?;
        let origin_b = pg
            .ingest_fact_atomic(&fact_permit, &fact_draft(), None)
            .await?;
        let handle = Uuid::now_v7();
        let keyed = |key: &str| -> Result<DerivedDraft<'static>, String> {
            let mut draft = derived_draft(owner, handle);
            draft.request = Some(RequestKey::new(
                KeyScope::Abstraction,
                IdempotencyKey::new(key)?,
                ContentFingerprint::builder(KeyScope::Abstraction)
                    .text("key", key)
                    .build(),
            ));
            Ok(draft)
        };
        let first_origins = [EdgeEndpoint::memory(EntityKind::Fact, origin_a.memory_id)];
        let first = append_with_edges(pool, &permit, &keyed("k1")?, &first_origins, &[]).await?;
        let second = append_with_edges(
            pool,
            &permit,
            &keyed("k2")?,
            &[EdgeEndpoint::memory(EntityKind::Fact, origin_b.memory_id)],
            &[],
        )
        .await?;
        assert!(!first.idempotent_replay);
        assert!(!second.idempotent_replay);
        assert_ne!(first.memory_id, second.memory_id);
        let head: Uuid =
            sqlx::query_scalar("SELECT t FROM proxiima_core.memory_head WHERE handle = $1")
                .bind(handle)
                .fetch_one(pool)
                .await?;
        assert_eq!(head, second.memory_id.into_inner());

        MemoryAuthoringPort::forget_memory(&pg, &fact_permit, origin_a.memory_id).await?;
        let replay = append_with_edges(pool, &permit, &keyed("k1")?, &first_origins, &[]).await?;
        assert!(replay.idempotent_replay);
        assert_eq!(replay.memory_id, first.memory_id);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("origin change must append a new t");
}
