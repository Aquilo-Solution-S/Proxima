//! Every verb that touches a searchable admission maintains its projection.
//!
//! The projection is the first memory-keyed surface that is neither stamped
//! on `memory.sidecar_tables` nor derived at read time: it is a row the
//! WRITE path has to keep, and a row every inverse has to reach. Before it,
//! the vector lived in the sidecar's own GENERATED column and followed the
//! sidecar around for free — delete the sidecar row and the vector went
//! with it, transfer the Memory and the vector never had an owner to move.
//! None of that is true now, so each verb needs its own evidence.
//!
//! `search_projection_identity` proves the READ side is unchanged. This is
//! the write side: write, transfer, forget-to-cold, erase.
#![allow(clippy::doc_markdown)]

use proxima_core::compliance::{
    ComplianceEraseOutcome, ComplianceEraseTarget, ComplianceSidecarTables, EraseAuthorization,
};
use proxima_core::storage_ports::{ComplianceErasePort, OwnerTransferPort, OwnerWritePermit};
use proxima_core::verbs::fact_ingest::FactWriteCommand;
use proxima_core::{
    AccessKind, AgentNoteV1, EntityId, FactPayload, GroupId, MemoryId, OwnerRef, SchemaId,
    SchemaVersion, SidecarPayload, StorageError, UserId,
};
use proxima_pg_testkit::{create_db, db_url, drop_db, unique_db_name};
use proxima_storage_pg::PgStorage;
use proxima_storage_pg::core_pg_sidecars;
use proxima_storage_pg::verbs::forget::{
    MemoryColdStore, cold_object_key, erase_memory, forget_memory_oneshot,
};
use proxima_storage_pg::verbs::memory_timeseries::ingest_fact_timeseries;
use uuid::Uuid;

const AGENT_NOTE: &str = "proxima_core.agent_note_v1";

fn draft() -> FactWriteCommand {
    FactWriteCommand {
        schema_id: SchemaId::new(AgentNoteV1::SCHEMA_ID.to_string()),
        schema_version: SchemaVersion::new(1),
        handle: None,
        source_id: None,
        ingest_key: None,
        payload: Vec::new(),
        rendered_text: None,
        lexical_language: None,
        receipt: None,
        citation: None,
        derived_from: Vec::new(),
        refs: Vec::new(),
        blob_id: None,
        kind: "fact".into(),
    }
}

fn note() -> SidecarPayload {
    SidecarPayload::fact(AgentNoteV1 {
        note_id: Uuid::now_v7(),
        title: "harbour survey".into(),
        body: "the pilings under the north quay are sound".into(),
        tags: vec!["survey".into()],
        idempotency_key: None,
    })
}

/// The production write, not a hand-rolled one: `ingest_fact_timeseries`
/// for the admission and `insert_memory_sidecar` for the payload, which is
/// where the generated projection statement runs. A test that INSERTed the
/// projection row itself would prove nothing about the write path.
async fn write_note(
    pool: &sqlx::PgPool,
    owner: OwnerRef,
    language: Option<&str>,
) -> Result<MemoryId, StorageError> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|err| StorageError::Internal(err.to_string()))?;
    let outcome =
        ingest_fact_timeseries(&mut tx, &owner, &draft(), &[AGENT_NOTE.to_owned()], None).await?;
    core_pg_sidecars()
        .insert_memory_sidecar(&mut tx, outcome.memory_id, &note(), language)
        .await?;
    tx.commit()
        .await
        .map_err(|err| StorageError::Internal(err.to_string()))?;
    Ok(outcome.memory_id)
}

#[derive(Debug, PartialEq, Eq, sqlx::FromRow)]
struct ProjectionRow {
    schema_id: String,
    owner_id: Uuid,
    lexical_language: String,
    tag: Vec<String>,
    has_vector: bool,
}

async fn projection_of(
    pool: &sqlx::PgPool,
    t: MemoryId,
) -> Result<Option<ProjectionRow>, sqlx::Error> {
    sqlx::query_as(
        "SELECT schema_id,
                owner_id,
                lexical_language::text AS lexical_language,
                tag,
                search_tsv <> ''::tsvector AS has_vector
           FROM proxima_core.projection
          WHERE memory_id = $1",
    )
    .bind(t.into_inner())
    .fetch_optional(pool)
    .await
}

async fn with_db<F>(name: &str, body: F)
where
    F: AsyncFnOnce(&PgStorage) -> Result<(), Box<dyn std::error::Error>>,
{
    let db_name = unique_db_name(name);
    create_db(&db_name).await.expect("PG required for tests");
    let result = async {
        let pg = PgStorage::connect(&db_url(&db_name)).await?;
        pg.run_migrations().await?;
        body(&pg).await
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.unwrap_or_else(|err| panic!("{name} failed: {err}"));
}

#[tokio::test]
async fn a_write_files_one_projection_row_carrying_the_owner_the_tag_and_the_language() {
    with_db("proxima_proj_write", async |pg| {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let pool = pg.pool_for_tests();
        let t = write_note(pool, owner, Some("german")).await?;

        assert_eq!(
            projection_of(pool, t).await?,
            Some(ProjectionRow {
                schema_id: AgentNoteV1::SCHEMA_ID.to_string(),
                owner_id: owner.stored_owner_id(),
                lexical_language: "german".to_string(),
                tag: vec!["survey".to_string()],
                has_vector: true,
            }),
            "the projection row is the write path's, not the reader's"
        );
        Ok(())
    })
    .await;
}

#[tokio::test]
async fn a_transfer_moves_the_projection_row_with_the_admission() {
    with_db("proxima_proj_transfer", async |pg| {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let dest = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let pool = pg.pool_for_tests();
        let t = write_note(pool, owner, None).await?;

        assert!(
            pg.transfer_to_owner(&permit, EntityId::Memory(t), dest)
                .await?
        );

        let row = projection_of(pool, t).await?.expect("projection survives");
        assert_eq!(
            row.owner_id,
            dest.stored_owner_id(),
            "a projection row left at the source is a cross-owner index leak: \
             the ranked arm reads p.owner_id and nothing else"
        );
        let memory_owner: Uuid =
            sqlx::query_scalar("SELECT owner_id FROM proxima_core.memory WHERE t = $1")
                .bind(t.into_inner())
                .fetch_one(pool)
                .await?;
        assert_eq!(
            row.owner_id, memory_owner,
            "the two owner columns move in one transaction or not at all"
        );
        Ok(())
    })
    .await;
}

#[tokio::test]
async fn forgetting_to_cold_takes_the_projection_row_with_the_sidecar_row() {
    with_db("proxima_proj_forget", async |pg| {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let pool = pg.pool_for_tests();
        let t = write_note(pool, owner, None).await?;
        assert!(projection_of(pool, t).await?.is_some());

        let cold = MemoryColdStore::default();
        forget_memory_oneshot(
            pool,
            &core_pg_sidecars(),
            &cold,
            &cold_object_key(t.into_inner()),
            t.into_inner(),
            permit.owner().stored_owner_id(),
        )
        .await?;

        assert_eq!(
            projection_of(pool, t).await?,
            None,
            "a cooled admission has no hot text, so it must have no hot vector: \
             leaving one behind puts a searchable row in front of bytes that are gone"
        );
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*)::bigint FROM proxima_core.agent_note_v1 WHERE t = $1",
            )
            .bind(t.into_inner())
            .fetch_one(pool)
            .await?,
            0,
            "the sidecar row went first; the projection followed it"
        );
        Ok(())
    })
    .await;
}

#[tokio::test]
async fn erasing_an_admission_takes_its_projection_row() {
    with_db("proxima_proj_erase", async |pg| {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let pool = pg.pool_for_tests();
        let kept = write_note(pool, owner, None).await?;
        let erased = write_note(pool, owner, None).await?;

        let mut tx = pool.begin().await?;
        erase_memory(&mut tx, &core_pg_sidecars(), &owner, erased.into_inner()).await?;
        tx.commit().await?;

        assert_eq!(projection_of(pool, erased).await?, None);
        assert!(
            projection_of(pool, kept).await?.is_some(),
            "an erase reaches one admission, not the schema"
        );
        Ok(())
    })
    .await;
}

/// R13: the compliance inverse reaches the projection WITHOUT being taught
/// about it.
///
/// `projection.memory_id` is `REFERENCES proxima_core.memory (t) ON DELETE
/// CASCADE`, and Article 17 erase deletes `proxima_core.memory` rows, so
/// the projection goes with them. Nothing in `verbs::compliance_erase`
/// names a projection table and nothing should: the erase is the inverse
/// of the write at the scope the write happened, and the constraint is
/// what makes that true rather than a list somebody has to maintain.
///
/// The pin matters because the failure is silent and bad: a projection row
/// surviving its erased owner is a searchable row for a subject whose data
/// was destroyed.
#[tokio::test]
async fn a_compliance_erase_takes_the_owners_projection_rows_by_cascade() {
    with_db("proxima_proj_compliance", async |pg| {
        let user = UserId::new(Uuid::now_v7());
        let owner = OwnerRef::Personal(user);
        let bystander = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let pool = pg.pool_for_tests();
        let erased = write_note(pool, owner, None).await?;
        let kept = write_note(pool, bystander, None).await?;

        let auth = EraseAuthorization::new_for_tests(ComplianceEraseTarget::PersonalOwner {
            user_id: user,
            drop_event_id: "projection-cascade".into(),
        });
        let sidecar_tables = ComplianceSidecarTables::for_registry(
            &proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests(),
        );
        let outcome = pg
            .erase_personal_owner_if_drop_verified(&auth, user, false, &sidecar_tables)
            .await?;
        assert!(
            matches!(outcome, ComplianceEraseOutcome::Completed { .. }),
            "expected a completed erase, got {outcome:?}"
        );

        assert_eq!(
            projection_of(pool, erased).await?,
            None,
            "an erased subject must not keep a searchable row"
        );
        assert!(
            projection_of(pool, kept).await?.is_some(),
            "one owner's erase is not another owner's"
        );
        Ok(())
    })
    .await;
}
