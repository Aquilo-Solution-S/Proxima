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

    sqlx::query("DELETE FROM proxima_core.memories WHERE memory_id = $1")
        .bind(memory_id)
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
