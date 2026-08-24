//! The series-head lookup against a sidecar keyed on its own column name.

use proxima_core::storage_ports::OwnerWritePermit;
use proxima_core::test_fixtures::owner_fixture;
use proxima_core::verbs::fact_ingest::FactWriteCommand;
use proxima_core::verbs::query::SidecarAtom;
use proxima_core::{
    AccessKind, AuthPath, AuthzContext, Engine, FactIngestPort, FlavorRegistry, Owner, SchemaId,
    SchemaVersion, StorageError,
};
use proxima_pg_testkit::drop_db;

use super::owned_head_handle;
use crate::test_fixtures::fresh_pg;

const SCHEMA: &str = "proxima-test/series-head-v1";
const RENAMED_TABLE: &str = "proxima_core.renamed_series_note_v1";
const RENAMED_KEY: &str = "note_memory_id";

fn draft() -> FactWriteCommand {
    FactWriteCommand {
        schema_id: SchemaId::new(SCHEMA.into()),
        schema_version: SchemaVersion::new(1),
        handle: None,
        source_id: None,
        ingest_key: None,
        payload: Vec::new(),
        rendered_text: Some("north quay survey".into()),
        lexical_language: None,
        receipt: None,
        citation: None,
        derived_from: Vec::new(),
        refs: Vec::new(),
        blob_id: None,
        kind: "fact".into(),
    }
}

async fn write_permit(owner: &Owner) -> Result<OwnerWritePermit, StorageError> {
    let Owner::Personal(user_id) = owner else {
        return Err(StorageError::Internal(
            "series-head fixture expects a personal owner".into(),
        ));
    };
    let engine = Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests());
    engine
        .authorize_owner_write(
            &AuthzContext::for_subject(*user_id, AuthPath::HostBearer),
            owner,
            AccessKind::Fact,
        )
        .await
        .map_err(|err| StorageError::Internal(err.to_string()))
}

/// A sidecar keyed on a column of its own naming is reachable through the
/// series-head lookup.
///
/// The lookup joins the sidecar back to `proxima_core.memory` and used to
/// spell that join `m.t = s.t` whatever `pg_sidecar!(key: …)` said, so a
/// flavor keyed on any other column could not use it at all: an ingest
/// looking for its own current head found none and appended a second
/// series instead of a later `t` on the first. `m.t` and `h.t` in the
/// statement are the kernel tables' own keys and stay fixed; only the
/// sidecar side moves.
///
/// The control matters as much as the positive: `t` is not a column of this
/// table, so a statement that still spelled it could not pass by accident.
#[tokio::test]
async fn a_sidecar_keyed_on_its_own_column_name_is_found_by_the_series_head_lookup()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg("proxima_spg_series_key").await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner_fixture();
        let permit = write_permit(&owner).await?;
        let outcome = pg.ingest_fact_atomic(&permit, &draft(), None).await?;
        let pool = pg.pool_for_tests();

        sqlx::query(
            "CREATE TABLE proxima_core.renamed_series_note_v1 (
                 note_memory_id uuid PRIMARY KEY
                                REFERENCES proxima_core.memory (t) ON DELETE CASCADE,
                 slug           text NOT NULL
             )",
        )
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.renamed_series_note_v1 (note_memory_id, slug)
             VALUES ($1, 'north-quay')",
        )
        .bind(outcome.memory_id.into_inner())
        .execute(pool)
        .await?;

        let schema_id = SchemaId::new(SCHEMA.into());
        let columns = [("slug", SidecarAtom::Text("north-quay".into()))];

        let found = owned_head_handle(
            pool,
            owner,
            &schema_id,
            RENAMED_TABLE,
            RENAMED_KEY,
            &columns,
        )
        .await?;
        assert_eq!(
            found,
            Some(outcome.handle),
            "the lookup joins the sidecar on the column its registration declares"
        );

        let miss = owned_head_handle(
            pool,
            owner,
            &schema_id,
            RENAMED_TABLE,
            RENAMED_KEY,
            &[("slug", SidecarAtom::Text("south-quay".into()))],
        )
        .await?;
        assert_eq!(miss, None, "and still matches on the caller's own columns");

        let wrong_key =
            owned_head_handle(pool, owner, &schema_id, RENAMED_TABLE, "t", &columns).await;
        assert!(
            wrong_key.is_err(),
            "spelling the key `t` reaches no column of a sidecar keyed otherwise"
        );
        Ok(())
    }
    .await;
    drop(pg);
    drop_db(&db_name).await?;
    result
}
