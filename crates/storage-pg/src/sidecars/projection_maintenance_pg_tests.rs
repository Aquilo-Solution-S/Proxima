//! Every verb that touches a searchable admission maintains its projection.
//!
//! The projection is the one memory-keyed surface that is neither stamped on
//! `memory.sidecar_tables` nor derived at read time: it is a row the WRITE path
//! has to keep, and a row every inverse has to reach. A vector living in the
//! sidecar's own GENERATED column follows the sidecar for free — delete the
//! sidecar row and the vector goes with it, transfer the Memory and the vector
//! has no owner to move. A projection row has neither property, so each verb
//! needs its own evidence.
//!
//! `search_projection_identity` carries the READ side. This is the write side:
//! write, transfer, forget-to-cold, erase.
#![allow(clippy::doc_markdown)]

use crate::PgStorage;
use crate::core_pg_sidecars;
use crate::verbs::forget::{MemoryColdStore, cold_object_key, erase_memory, forget_memory_oneshot};
use crate::verbs::memory_timeseries::ingest_fact_timeseries;
use proxima_core::owner_inverse::{
    EraseAuthorization, OwnerEraseOutcome, OwnerEraseTarget, OwnerSurfaces,
};
use proxima_core::storage_ports::{OwnerInversePort, OwnerTransferPort, OwnerWritePermit};
use proxima_core::verbs::fact_ingest::FactWriteCommand;
use proxima_core::{
    AccessKind, AgentNoteV1, EntityId, FactPayload, GroupId, MemoryId, OwnerRef, SchemaId,
    SchemaVersion, SidecarPayload, StorageError, UserId,
};
use proxima_pg_testkit::{create_db, db_url, drop_db, unique_db_name};
use uuid::Uuid;

/// The transfer's registry-resolved legs, exactly as the engine assembles
/// them. Passing a hand-built set here would test a registry production
/// never sees.
fn transfer_surfaces() -> proxima_core::owner_inverse::OwnerSurfaces {
    proxima_core::owner_inverse::OwnerSurfaces::for_registry(
        &proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests(),
    )
}

const AGENT_NOTE: &str = "proxima_core.agent_note_v1";

/// `language` is the write's own: `agent-note-v1` declares
/// `LanguagePolicy::PerRow`, so the draft has to name one. A fixture that
/// does not care which asks for the deployment configuration, exactly as an
/// omitted `language` argument resolves on the tool surfaces.
fn draft(language: Option<&str>) -> FactWriteCommand {
    FactWriteCommand {
        schema_id: SchemaId::new(AgentNoteV1::SCHEMA_ID.to_string()),
        schema_version: SchemaVersion::new(1),
        handle: None,
        source_id: None,
        ingest_key: None,
        payload: Vec::new(),
        rendered_text: None,
        lexical_language: Some(
            language
                .unwrap_or(proxima_core::lexical_language::LEXICAL_LANGUAGE_DEPLOYMENT_DEFAULT)
                .to_owned(),
        ),
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
    let write = draft(language);
    let outcome =
        ingest_fact_timeseries(&mut tx, &owner, &write, &[AGENT_NOTE.to_owned()], None).await?;
    core_pg_sidecars()
        .writing(&write)
        .insert_memory_sidecar(&mut tx, outcome.memory_id, &note())
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

/// A `PerRow` schema whose write named no language is refused, not
/// projected at the deployment default.
///
/// `PerRow` means the row's configuration IS the writer's, so a write that
/// named none made no choice, and stamping one silently would decide how
/// its own words are tokenised — the row can end up unmatchable by them —
/// with nobody having chosen. The refusal is here rather than one layer up
/// because this is the only place that holds both the write and the
/// schema's declared policy.
#[tokio::test]
async fn a_per_row_schema_refuses_a_write_that_named_no_language() {
    with_db("proxima_proj_no_language", async |pg| {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let pool = pg.pool_for_tests();
        let mut write = draft(None);
        write.lexical_language = None;

        let mut tx = pool.begin().await?;
        let outcome =
            ingest_fact_timeseries(&mut tx, &owner, &write, &[AGENT_NOTE.to_owned()], None).await?;
        let err = core_pg_sidecars()
            .writing(&write)
            .insert_memory_sidecar(&mut tx, outcome.memory_id, &note())
            .await
            .expect_err("a PerRow schema refuses a write that named no language");
        tx.rollback().await?;

        let message = err.to_string();
        assert!(
            message.contains(AgentNoteV1::SCHEMA_ID)
                && message.contains("declares LanguagePolicy::PerRow"),
            "the refusal names the schema and its policy: {message}"
        );
        assert!(
            message.contains("resolve_lexical_language")
                && message.contains("declare a pinned language policy"),
            "the refusal names the fix: {message}"
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
            pg.transfer_to_owner(&permit, EntityId::Memory(t), dest, &transfer_surfaces())
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
            &transfer_surfaces(),
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
        erase_memory(
            &mut tx,
            &core_pg_sidecars(),
            &transfer_surfaces(),
            &owner,
            erased.into_inner(),
        )
        .await?;
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

/// The owner inverse reaches the projection WITHOUT being taught about it.
///
/// `projection.memory_id` is `REFERENCES proxima_core.memory (t) ON DELETE
/// CASCADE`, and an owner erase deletes `proxima_core.memory` rows, so
/// the projection goes with them. Nothing in `verbs::owner_erase`
/// names a projection table and nothing should: the erase is the inverse
/// of the write at the scope the write happened, and the constraint is
/// what makes that true rather than a list somebody has to maintain.
///
/// The pin matters because the failure is silent and bad: a projection row
/// surviving its erased owner is a searchable row for a subject whose data
/// was destroyed.
#[tokio::test]
async fn an_owner_erase_takes_the_owners_projection_rows_by_cascade() {
    with_db("proxima_proj_owner_erase", async |pg| {
        let user = UserId::new(Uuid::now_v7());
        let owner = OwnerRef::Personal(user);
        let bystander = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let pool = pg.pool_for_tests();
        let erased = write_note(pool, owner, None).await?;
        let kept = write_note(pool, bystander, None).await?;

        let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalOwner {
            user_id: user,
            drop_event_id: "projection-cascade".into(),
        });
        let sidecar_tables = OwnerSurfaces::for_registry(
            &proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests(),
        );
        let outcome = pg
            .erase_personal_owner(&auth, user, false, &sidecar_tables)
            .await?;
        assert!(
            matches!(outcome, OwnerEraseOutcome::Completed { .. }),
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

/// A projection row may only be stamped with the schema its memory IS.
///
/// `projection_insert_sql` binds `$3` as the schema id it writes onto the
/// row AND as the id the memory must already carry. Without the second use
/// the value was a caller's assertion about a row it was not reading: pass
/// the wrong id and a projection row lands claiming a schema the memory is
/// not. Search then narrows on `p.schema_id` in the ranked arm and on
/// `m.schema_id` at admit, so that row is a candidate the window pays for
/// and admission discards — the starvation defect, arriving from the write
/// side.
///
/// The mismatched call uses the NOTE's generated statement, because that is
/// the reachable shape: the statement is chosen by sidecar table and the id
/// is a bind, so nothing about the statement itself says which schema the
/// row belongs to. Deleting `AND m.schema_id = $3` makes the first half of
/// this test file a row and fail.
#[tokio::test]
async fn a_projection_row_cannot_claim_a_schema_its_memory_is_not() {
    with_db("proxima_proj_schema_guard", async |pg| {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let pool = pg.pool_for_tests();
        let t = write_note(pool, owner, None).await?;

        // The write path already filed the row; clear it so the two runs
        // below are the only writers and the PK cannot mask the result.
        sqlx::query("DELETE FROM proxima_core.projection WHERE memory_id = $1")
            .bind(t.into_inner())
            .execute(pool)
            .await?;
        assert_eq!(
            projection_of(pool, t).await?,
            None,
            "the fixture starts empty"
        );

        let note_schema = proxima_core::FLAVOR_0
            .schemas
            .iter()
            .find(|schema| schema.schema_id().as_str() == AgentNoteV1::SCHEMA_ID)
            .expect("the note schema is declared");
        let sql = crate::projection::projection_insert_sql(&proxima_core::FLAVOR_0, note_schema)
            .expect("the generator emits a valid statement");

        // A real, declared schema id — and not this memory's.
        // SQL-POLICY: generated
        let wrong = sqlx::query(sqlx::AssertSqlSafe(sql.clone()))
            .bind(t.into_inner())
            .bind(None::<&str>)
            .bind("core/interpretation-v1")
            .execute(pool)
            .await?;
        assert_eq!(
            wrong.rows_affected(),
            0,
            "a projection row stamped with a schema the memory is not must \
             not be written"
        );
        assert_eq!(
            projection_of(pool, t).await?,
            None,
            "…and nothing may be left behind"
        );

        // The control: the same statement with the memory's own id writes.
        // Without it, a guard that rejected EVERYTHING would pass the half
        // above.
        // SQL-POLICY: generated
        let right = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(t.into_inner())
            .bind(None::<&str>)
            .bind(AgentNoteV1::SCHEMA_ID)
            .execute(pool)
            .await?;
        assert_eq!(right.rows_affected(), 1, "the matching write still lands");
        assert_eq!(
            projection_of(pool, t).await?.map(|row| row.schema_id),
            Some(AgentNoteV1::SCHEMA_ID.to_string()),
            "…carrying the memory's own schema id"
        );
        Ok(())
    })
    .await;
}

/// Flavor #0's note declaration, re-pointed at a sidecar that keys its
/// memory on a column of its own naming.
///
/// Everything else is the shipped declaration, so the statement under test
/// differs from the production one in the table and the key column and
/// nothing else. The contract is narrowed to the one schema so the surface
/// lookup answers from this declaration rather than from the real note's.
fn a_note_contract_keyed_on(
    table: &'static str,
    key_column: &'static str,
) -> (
    proxima_core::flavor::FlavorContract,
    &'static proxima_core::flavor::SchemaContract,
) {
    let mut surface = proxima_core::FLAVOR_0
        .surface_for(AGENT_NOTE)
        .expect("the note sidecar is declared");
    surface.table = table;
    surface.key = proxima_core::flavor::KeyShape::MemoryT { column: key_column };
    let mut schema = *proxima_core::FLAVOR_0
        .schemas
        .iter()
        .find(|schema| schema.schema_id().as_str() == AgentNoteV1::SCHEMA_ID)
        .expect("the note schema is declared");
    schema.sidecar_table = Some(table);
    schema.surfaces = Box::leak(Box::new([surface]));
    let schema: &'static proxima_core::flavor::SchemaContract = Box::leak(Box::new(schema));
    let mut contract = proxima_core::FLAVOR_0;
    contract.schemas = std::slice::from_ref(schema);
    (contract, schema)
}

/// A sidecar may key its memory on a column of its own naming, and the
/// projection row still lands.
///
/// `KeyShape::MemoryT { column }` exists because the erase and export lanes
/// have no other way to learn which column carries the id; a downstream
/// flavor that keyed its sidecar on such a name got no projection rows,
/// because the generator spelled the sidecar side of its join `t` whatever
/// the declaration said. The generated text is asserted in the module's own
/// unit tests; what this adds is the half a string comparison cannot reach
/// — the statement has to be valid `PostgreSQL` against a table with that
/// column, and it has to file the row.
///
/// Deliberately NOT routed through `insert_memory_sidecar`: reaching that
/// path with a renamed key needs a payload type, a `pg_sidecar!`
/// registration and a second flavor contract in the frozen registry, which
/// would exercise the registry rather than the generator. The statement
/// that path runs is the one executed here — `attach_projections` stores
/// exactly this function's output.
#[tokio::test]
async fn a_sidecar_keyed_on_its_own_column_name_still_files_its_projection_row() {
    const RENAMED_TABLE: &str = "proxima_core.renamed_note_v1";
    const RENAMED_KEY: &str = "note_memory_id";

    with_db("proxima_proj_renamed_key", async |pg| {
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let pool = pg.pool_for_tests();
        let t = write_note(pool, owner, None).await?;

        // The real write path already filed this (memory, schema) pair;
        // clear it so the renamed statement is the only writer and the
        // primary key cannot mask the result.
        sqlx::query("DELETE FROM proxima_core.projection WHERE memory_id = $1")
            .bind(t.into_inner())
            .execute(pool)
            .await?;

        sqlx::query(
            "CREATE TABLE proxima_core.renamed_note_v1 (
                 note_memory_id uuid PRIMARY KEY
                                REFERENCES proxima_core.memory (t) ON DELETE CASCADE,
                 title          text   NOT NULL,
                 body           text   NOT NULL,
                 tags           text[] NOT NULL DEFAULT '{}'
             )",
        )
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.renamed_note_v1 (note_memory_id, title, body, tags)
             VALUES ($1, 'harbour survey', 'the pilings under the north quay are sound',
                     ARRAY['survey'])",
        )
        .bind(t.into_inner())
        .execute(pool)
        .await?;

        let (contract, schema) = a_note_contract_keyed_on(RENAMED_TABLE, RENAMED_KEY);
        let sql = crate::projection::projection_insert_sql(&contract, schema)
            .expect("the generator emits a valid statement");
        assert!(
            sql.contains(&format!("c.{RENAMED_KEY}")),
            "the generator reads the declared key column: {sql}"
        );

        // SQL-POLICY: generated
        let filed = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(t.into_inner())
            .bind(Some("german"))
            .bind(AgentNoteV1::SCHEMA_ID)
            .execute(pool)
            .await?;
        assert_eq!(
            filed.rows_affected(),
            1,
            "a sidecar keyed on its own column name files exactly one projection row"
        );
        assert_eq!(
            projection_of(pool, t).await?,
            Some(ProjectionRow {
                schema_id: AgentNoteV1::SCHEMA_ID.to_string(),
                owner_id: owner.stored_owner_id(),
                lexical_language: "german".to_string(),
                tag: vec!["survey".to_string()],
                has_vector: true,
            }),
            "and it carries the joined memory's owner, the copied tag and the vector"
        );
        Ok(())
    })
    .await;
}
