//! Per-row lexical language (migration 0014).
//!
//! The language a row's stored vector tokenises with is data on the row:
//! stamped at write time, mirrored onto sidecars, immutable afterwards.
//! The query side ORs one tsquery per active language for MATCHING and
//! ranks each candidate with its own row's configuration — the shape
//! measured to make a mixed-language corpus cost nothing over a
//! single-language one.

use crate::common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::engine::Engine;
use proxima_core::storage_ports::*;
use proxima_core::verbs::fact_ingest::{FactReceiptDraft, FactWriteCommand};
use proxima_core::verbs::query::EntityKind;
use proxima_core::verbs::schema::{FlavorRegistryFrozen, PayloadKind, SchemaInfo};
use proxima_core::{Owner, OwnerRef, SchemaId, SchemaVersion, SourceBatchId, SourceId, UserId};
use std::sync::Arc;
use uuid::Uuid;

use super::lexical_request;

async fn insert_text_memory_in_language(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    text: &str,
    language: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let memory_id = Uuid::now_v7();
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(owner);
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_kind, owner_id, schema_id, schema_version, kind, text,
             operator_kind, operator_id, input_contract_id, source_batch_id,
             model_id, prompt_version, lexical_language)
         VALUES ($1, $2, $3, 'test/search-language-v1', 1,
                 'Abstraction', $4, 'AtoA',
                 '00000000-0000-0000-0000-000000000341'::uuid,
                 '00000000-0000-0000-0000-000000000342'::uuid, NULL,
                 'test-model', 'test-v1', $5::regconfig)",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(text)
    .bind(language)
    .execute(pg.pool_for_tests())
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.lexical_languages (config)
         VALUES ($1::regconfig) ON CONFLICT (config) DO NOTHING",
    )
    .bind(language)
    .execute(pg.pool_for_tests())
    .await?;
    Ok(memory_id)
}

/// One owner, two languages, one query side. Each row is reachable
/// through the inflection only its own stemmer can conflate — the
/// cross-language OR match plus per-row rank, end to end.
#[tokio::test]
async fn a_mixed_language_corpus_is_searchable_per_row() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;

    let owner = owner_fixture();
    let german_row = insert_text_memory_in_language(
        &pg,
        &owner,
        "Die Bauleitung überwacht die Ausführung",
        "german",
    )
    .await?;
    let english_row = insert_text_memory_in_language(
        &pg,
        &owner,
        "The supervisor adopted the inspection procedure",
        "english",
    )
    .await?;

    // `Bauleitungen` → `bauleit` only under the German stemmer; no literal
    // token or substring of the German row matches it otherwise.
    let mut german_query = lexical_request(&owner, "Bauleitungen");
    german_query.kind = Some(EntityKind::Abstraction);
    let hits = pg.search_memories(&german_query, &[]).await?.results;
    assert_eq!(
        hits.first().map(|row| row.memory_id.into_inner()),
        Some(german_row),
        "the German row is unreachable through its own inflection: {hits:#?}"
    );

    // `adopting` → `adopt` only under the English stemmer.
    let mut english_query = lexical_request(&owner, "adopting inspections");
    english_query.kind = Some(EntityKind::Abstraction);
    let hits = pg.search_memories(&english_query, &[]).await?.results;
    assert_eq!(
        hits.first().map(|row| row.memory_id.into_inner()),
        Some(english_row),
        "the English row is unreachable through its own inflection: {hits:#?}"
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

/// The sidecar's language column is stamped from the owning memories row
/// by the BEFORE INSERT trigger, and its stored vector tokenises with it
/// — a german note must not carry an english-stemmed sidecar vector.
#[tokio::test]
async fn sidecar_rows_mirror_the_owning_memory_language() -> Result<(), Box<dyn std::error::Error>>
{
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;

    let owner = owner_fixture();
    let memory_id =
        insert_text_memory_in_language(&pg, &owner, "Die Bauleitungen wurden beauftragt", "german")
            .await?;
    sqlx::query(
        "INSERT INTO proxima_core.agent_derivation_v1
            (memory_id, title, body, tags, source_memory_ids, model_id,
             client_name, client_version)
         VALUES ($1, 'Bauleitung', 'Die Bauleitungen wurden beauftragt',
                 ARRAY[]::text[], ARRAY[]::uuid[], 'test-model', 'test', '1')",
    )
    .bind(memory_id)
    .execute(pg.pool_for_tests())
    .await?;

    let (language, tsv): (String, String) = sqlx::query_as(
        "SELECT lexical_language::text, search_tsv::text
           FROM proxima_core.agent_derivation_v1 WHERE memory_id = $1",
    )
    .bind(memory_id)
    .fetch_one(pg.pool_for_tests())
    .await?;
    assert_eq!(
        language, "german",
        "the sidecar did not mirror the owning row's language"
    );
    assert!(
        tsv.contains("'bauleit'") && !tsv.contains("'die'"),
        "the sidecar vector is not German-tokenised: {tsv}"
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

fn language_schemas_for_test() -> Vec<SchemaInfo> {
    vec![SchemaInfo {
        schema_id: SchemaId::new("test/fact_blob".into()),
        schema_version: SchemaVersion::new(1),
        kind: PayloadKind::Fact,
        filter_keys: vec![],
        sidecar_table: None,
        natural_key_columns: vec![],
        tombstone: None,
        has_typed_ingress: false,
        cited_object_schema: None,
        embeddable: true,
    }]
}

fn draft_in_language(language: Option<&str>) -> FactWriteCommand {
    let now = time::OffsetDateTime::now_utc();
    FactWriteCommand {
        schema_id: SchemaId::new("test/fact_blob".into()),
        schema_version: SchemaVersion::new(1),
        payload: format!("payload {}", Uuid::now_v7()).into_bytes(),
        rendered_text: Some("Die Bauleitung überwacht die Ausführung".into()),
        lexical_language: language.map(str::to_string),
        receipt: Some(FactReceiptDraft {
            source_id: SourceId::new("test/source"),
            source_batch_id: SourceBatchId::new(Uuid::now_v7()),
            observed_at: now,
            occurred_at: now,
        }),
        citation: None,
        derived_from: None,
    }
}

/// The full ingest path: an explicit language lands on the row, its vector
/// tokenises with it, and the language joins the active set the query
/// builder ORs over — all in the write transaction.
#[tokio::test]
async fn an_explicit_language_stamps_the_row_and_registers_the_language()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;
    let storage = Arc::new(pg.clone()).storage_ports();
    let registry = FlavorRegistryFrozen::with_schemas(language_schemas_for_test());
    let engine = Engine::new(registry).with_storage_ports(storage);

    let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let authz =
        proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::HostBearer);

    let outcome = engine
        .fact_ingest(&authz, draft_in_language(Some("german")))
        .await?;

    let (language, tsv): (String, String) = sqlx::query_as(
        "SELECT lexical_language::text, search_tsv::text
           FROM proxima_core.memories WHERE memory_id = $1",
    )
    .bind(outcome.memory_id.into_inner())
    .fetch_one(pg.pool_for_tests())
    .await?;
    assert_eq!(language, "german");
    assert!(
        tsv.contains("'bauleit'") && !tsv.contains("'die'"),
        "explicit german did not drive the stored vector: {tsv}"
    );

    let registered: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM proxima_core.lexical_languages
          WHERE config = 'german'::regconfig)",
    )
    .fetch_one(pg.pool_for_tests())
    .await?;
    assert!(
        registered,
        "the stamped language did not join the active set"
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

/// A language that names no catalog configuration is rejected before any
/// row lands — loudly, not by silently falling back to the default.
#[tokio::test]
async fn an_unknown_language_is_rejected_before_the_row_lands()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;
    let storage = Arc::new(pg.clone()).storage_ports();
    let registry = FlavorRegistryFrozen::with_schemas(language_schemas_for_test());
    let engine = Engine::new(registry).with_storage_ports(storage);

    let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let authz =
        proxima_core::AuthzContext::single_owner(&owner, proxima_core::AuthPath::HostBearer);

    let err = engine
        .fact_ingest(&authz, draft_in_language(Some("klingon")))
        .await
        .expect_err("an unknown text-search configuration must be rejected");
    assert!(
        err.to_string()
            .contains("unknown text-search configuration"),
        "unexpected error for an unknown configuration: {err}"
    );

    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM proxima_core.memories")
        .fetch_one(pg.pool_for_tests())
        .await?;
    assert_eq!(rows, 0, "a row landed despite the rejected language");

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

/// Forgetting a language is guarded: while any row still references the
/// configuration, removal is refused — dropping a configuration rows
/// still hold leaves dangling OIDs that make those rows un-updatable.
#[tokio::test]
async fn lexical_language_forget_refuses_while_referenced() -> Result<(), Box<dyn std::error::Error>>
{
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;

    let owner = owner_fixture();
    let memory_id =
        insert_text_memory_in_language(&pg, &owner, "Die Bauleitung prüft", "german").await?;

    let refused = sqlx::query("SELECT proxima_core.lexical_language_forget('german')")
        .execute(pg.pool_for_tests())
        .await;
    let err = refused.expect_err("forget must refuse while a row references the language");
    assert!(
        err.to_string().contains("still reference"),
        "unexpected refusal message: {err}"
    );

    // The default is never forgettable, referenced or not.
    let default_refused = sqlx::query("SELECT proxima_core.lexical_language_forget('english')")
        .execute(pg.pool_for_tests())
        .await;
    assert!(
        default_refused
            .expect_err("the default must not be forgettable")
            .to_string()
            .contains("default"),
    );

    // A materialized view stores regconfig values as durably as a table
    // and dangles the same way after a config drop — it holds the guard.
    sqlx::query(
        "CREATE MATERIALIZED VIEW held_language AS
         SELECT lexical_language FROM proxima_core.memories
          WHERE lexical_language = 'german'::regconfig",
    )
    .execute(pg.pool_for_tests())
    .await?;

    sqlx::query("DELETE FROM proxima_core.memories WHERE memory_id = $1")
        .bind(memory_id)
        .execute(pg.pool_for_tests())
        .await?;
    let matview_refused = sqlx::query("SELECT proxima_core.lexical_language_forget('german')")
        .execute(pg.pool_for_tests())
        .await;
    assert!(
        matview_refused
            .expect_err("forget must refuse: a materialized view still holds the language")
            .to_string()
            .contains("held_language"),
    );
    sqlx::query("DROP MATERIALIZED VIEW held_language")
        .execute(pg.pool_for_tests())
        .await?;

    sqlx::query("SELECT proxima_core.lexical_language_forget('german')")
        .execute(pg.pool_for_tests())
        .await?;
    let gone: bool = sqlx::query_scalar(
        "SELECT NOT EXISTS (SELECT 1 FROM proxima_core.lexical_languages
          WHERE config = 'german'::regconfig)",
    )
    .fetch_one(pg.pool_for_tests())
    .await?;
    assert!(gone, "an unreferenced language was not forgotten");

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

/// One CJK note must not turn every query's function words into match
/// terms. `simple` keeps stopwords in its vectors; without the
/// query-side stop filter (`proxima_core.lexical_query_text`), activating
/// it once would let any row carrying an incidental English function word
/// rescue-match unrelated questions above the substring band.
#[tokio::test]
async fn an_active_simple_language_does_not_make_stopwords_match_terms()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;

    let owner = owner_fixture();
    // The realistic polluter: a mostly-CJK note quoting an English error
    // line, reliably detected as Chinese and stamped `simple`.
    let cjk_row = insert_text_memory_in_language(
        &pg,
        &owner,
        "这个部署报错 the connection is refused 请检查网络配置",
        "simple",
    )
    .await?;
    let english_row = insert_text_memory_in_language(
        &pg,
        &owner,
        "The migration plan covers the staged rollout",
        "english",
    )
    .await?;

    // An English question sharing ONLY function words with the CJK note
    // must not surface it.
    let mut unrelated = lexical_request(&owner, "what is the plan for the migration");
    unrelated.kind = Some(EntityKind::Abstraction);
    let hits = pg.search_memories(&unrelated, &[]).await?.results;
    assert!(
        !hits.iter().any(|row| row.memory_id.into_inner() == cjk_row),
        "a simple-stamped row matched an unrelated question through stopwords: {hits:#?}"
    );
    assert!(
        hits.iter()
            .any(|row| row.memory_id.into_inner() == english_row),
        "the actually relevant row went missing: {hits:#?}"
    );

    // Content words still reach the simple row — the filter must not cost
    // its legitimate reachability.
    let mut by_content = lexical_request(&owner, "connection refused");
    by_content.kind = Some(EntityKind::Abstraction);
    let hits = pg.search_memories(&by_content, &[]).await?.results;
    assert!(
        hits.iter().any(|row| row.memory_id.into_inner() == cjk_row),
        "the simple-stamped row lost its content-word reachability: {hits:#?}"
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

/// `lexical_language_forget` must not certify a language unreferenced
/// while a write stamping it is still in flight: the writer holds FOR KEY
/// SHARE on the registration row until commit, and forget's FOR UPDATE
/// blocks on it.
#[tokio::test]
async fn forget_blocks_on_an_in_flight_write_in_that_language()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;

    let owner = owner_fixture();
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(&owner);

    // 'german' is already active (committed) — the hazard is a NEW write
    // in an active language racing a forget, not a first-time registration
    // (which stays invisible to forget until the writer commits).
    sqlx::query("INSERT INTO proxima_core.lexical_languages (config) VALUES ('german'::regconfig)")
        .execute(pg.pool_for_tests())
        .await?;

    // Session A: the write path's exact statement sequence, held open.
    let mut writer = pg.pool_for_tests().begin().await?;
    sqlx::query(
        "INSERT INTO proxima_core.lexical_languages (config)
         VALUES ('german'::regconfig) ON CONFLICT (config) DO NOTHING",
    )
    .execute(writer.as_mut())
    .await?;
    sqlx::query(
        "SELECT 1 FROM proxima_core.lexical_languages
          WHERE config = 'german'::regconfig FOR KEY SHARE",
    )
    .execute(writer.as_mut())
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_kind, owner_id, schema_id, schema_version, kind, text,
             operator_kind, operator_id, input_contract_id, source_batch_id,
             model_id, prompt_version, lexical_language)
         VALUES ($1, $2, $3, 'test/search-language-v1', 1, 'Abstraction',
                 'Die Bauleitung prüft', 'AtoA',
                 '00000000-0000-0000-0000-000000000343'::uuid,
                 '00000000-0000-0000-0000-000000000344'::uuid, NULL,
                 'test-model', 'test-v1', 'german'::regconfig)",
    )
    .bind(Uuid::now_v7())
    .bind(owner_kind)
    .bind(owner_id)
    .execute(writer.as_mut())
    .await?;

    // Session B: forget must block on the writer's row lock, not succeed.
    let mut forgetter = pg.pool_for_tests().begin().await?;
    sqlx::query("SET LOCAL lock_timeout = '500ms'")
        .execute(forgetter.as_mut())
        .await?;
    let raced = sqlx::query("SELECT proxima_core.lexical_language_forget('german')")
        .execute(forgetter.as_mut())
        .await;
    let err = raced.expect_err("forget must block on the in-flight write, then time out");
    assert!(
        err.to_string().contains("lock timeout"),
        "expected a lock timeout, got: {err}"
    );
    forgetter.rollback().await?;

    // After the writer commits, forget sees the committed row and refuses
    // on the referencing-rows scan instead.
    writer.commit().await?;
    let refused = sqlx::query("SELECT proxima_core.lexical_language_forget('german')")
        .execute(pg.pool_for_tests())
        .await;
    assert!(
        refused
            .expect_err("forget must refuse: a committed row references the language")
            .to_string()
            .contains("still reference"),
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}
