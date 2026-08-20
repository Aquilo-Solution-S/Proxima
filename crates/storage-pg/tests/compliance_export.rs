//! Export projects pins from memory rows; no reconstructed Edge table.
#![allow(clippy::doc_markdown, clippy::too_many_lines)]

use proxima_core::compliance::{
    ComplianceExportTarget, ComplianceSidecarTables, ExportAuthorization,
};
use proxima_core::storage_ports::{ComplianceErasePort, OwnerWritePermit};
use proxima_core::verbs::fact_ingest::FactWriteCommand;
use proxima_core::verbs::query::EntityKind;
use proxima_core::{AccessKind, EdgeEndpoint, MemoryId, OwnerRef, SchemaId, SchemaVersion, UserId};
use proxima_pg_testkit::{create_db, db_url, drop_db};
use proxima_storage_pg::PgStorage;
use proxima_storage_pg::core_pg_sidecars;
use proxima_storage_pg::verbs::fact_ingest::ingest_fact_atomic;
use proxima_storage_pg::verbs::forget::{MemoryColdStore, cold_object_key, forget_memory};
use uuid::Uuid;

/// The five sidecar legs exactly as the engine assembles them: from the
/// frozen flavor registry. Passing empty slices here would silently skip
/// the owner-pinned leg, which is the difference these tests exist to
/// measure.
fn contract_sidecar_tables() -> ComplianceSidecarTables {
    ComplianceSidecarTables::for_registry(
        &proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests(),
    )
}

fn draft(kind: &str, refs: Vec<Uuid>, origins: Vec<Uuid>) -> FactWriteCommand {
    FactWriteCommand {
        schema_id: SchemaId::new(format!("export/{kind}")),
        schema_version: SchemaVersion::new(1),
        handle: None,
        source_id: None,
        ingest_key: None,
        payload: Vec::new(),
        rendered_text: None,
        lexical_language: None,
        receipt: None,
        citation: None,
        derived_from: origins
            .into_iter()
            .map(|t| EdgeEndpoint::memory(EntityKind::Fact, MemoryId::new(t)))
            .collect(),
        refs,
        blob_id: None,
        kind: kind.into(),
    }
}

#[tokio::test]
async fn export_edges_are_the_pins_already_on_memory() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let user = UserId::new(Uuid::now_v7());
        let owner = OwnerRef::Personal(user);
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let pool = pg.pool_for_tests();

        let leaf = ingest_fact_atomic(pool, &permit, &draft("fact", vec![], vec![]), None).await?;
        let derived = ingest_fact_atomic(
            pool,
            &permit,
            &draft("abstraction", vec![], vec![leaf.memory_id.into_inner()]),
            None,
        )
        .await?;

        let auth = ExportAuthorization::new_for_tests(ComplianceExportTarget::PersonalOwner {
            user_id: user,
        });
        let bundle = pg
            .export_owner_bundle(&auth, &contract_sidecar_tables())
            .await?;
        assert_eq!(bundle.counts.memories, 2);
        assert_eq!(bundle.counts.edges, 1);
        assert_eq!(bundle.edges.len(), 1);
        assert_eq!(
            bundle.edges[0]["kind"].as_str(),
            Some("origin"),
            "pin kind follows the origins array, not an Edge table"
        );
        assert_eq!(
            bundle.edges[0]["source_t"].as_str(),
            Some(derived.memory_id.into_inner().to_string()).as_deref()
        );
        assert_eq!(
            bundle.edges[0]["target_t"].as_str(),
            Some(leaf.memory_id.into_inner().to_string()).as_deref()
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("export pin projection failed");
}

fn sketched(kind: &str, text: &str) -> FactWriteCommand {
    let mut command = draft(kind, vec![], vec![]);
    command.rendered_text = Some(text.to_owned());
    command
}

/// A forgotten admission's content is not in `memory` any more; it is an
/// `object_key` in `cooled`. Exporting `memory` alone therefore returned a
/// bundle that silently omitted every cooled admission — and the derived
/// one-liners in `sketch` were never exported at all.
#[tokio::test]
async fn export_carries_cooled_locators_and_sketches() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let user = UserId::new(Uuid::now_v7());
        let owner = OwnerRef::Personal(user);
        let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
        let pool = pg.pool_for_tests();
        let cold = MemoryColdStore::default();

        let hot = ingest_fact_atomic(pool, &permit, &sketched("fact", "Still hot"), None).await?;
        let cooled =
            ingest_fact_atomic(pool, &permit, &sketched("fact", "Went cold\nbody"), None).await?;
        let cooled_t = cooled.memory_id.into_inner();
        let key = cold_object_key(cooled_t);
        let mut tx = pool.begin().await?;
        forget_memory(
            &mut tx,
            &core_pg_sidecars(),
            &cold,
            &key,
            cooled_t,
            owner.stored_owner_id(),
        )
        .await?;
        tx.commit().await?;

        // A neighbour owner's cooled admission and sketch must not appear.
        let other = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let other_permit = OwnerWritePermit::new_for_tests(other, AccessKind::Fact);
        let neighbour =
            ingest_fact_atomic(pool, &other_permit, &sketched("fact", "Not yours"), None).await?;
        let neighbour_t = neighbour.memory_id.into_inner();
        let neighbour_key = cold_object_key(neighbour_t);
        let mut tx = pool.begin().await?;
        forget_memory(
            &mut tx,
            &core_pg_sidecars(),
            &cold,
            &neighbour_key,
            neighbour_t,
            other.stored_owner_id(),
        )
        .await?;
        tx.commit().await?;

        let auth = ExportAuthorization::new_for_tests(ComplianceExportTarget::PersonalOwner {
            user_id: user,
        });
        let bundle = pg
            .export_owner_bundle(&auth, &contract_sidecar_tables())
            .await?;

        assert_eq!(
            bundle.counts.memories, 1,
            "the cooled admission left memory"
        );
        assert_eq!(bundle.counts.cooled, 1);
        assert_eq!(bundle.cooled.len(), 1);
        assert_eq!(
            bundle.cooled[0]["t"].as_str(),
            Some(cooled_t.to_string()).as_deref()
        );
        assert_eq!(
            bundle.cooled[0]["object_key"].as_str(),
            Some(key.as_str()),
            "the bundle carries the locator, not the cold bytes"
        );

        assert_eq!(
            bundle.counts.sketches, 1,
            "forget deletes the cooled row's sketch; the hot one remains"
        );
        assert_eq!(bundle.sketches.len(), 1);
        assert_eq!(
            bundle.sketches[0]["t"].as_str(),
            Some(hot.memory_id.into_inner().to_string()).as_deref()
        );
        assert_eq!(bundle.sketches[0]["text"].as_str(), Some("Still hot"));
        assert!(
            bundle.sketches[0].get("search_tsv").is_none(),
            "the generated lexical index is not owner data: {:?}",
            bundle.sketches[0]
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("cooled/sketch export test failed");
}

/// A v0.0.8 citation is the `proxima_core.blob` row a Fact names, so both
/// citation sidecar families key on `blob_id` and are owner-filtered through
/// `blob.owner_id`. Export used to receive the registered table lists and throw
/// them away, so a flavor's typed cited-object and citation-mapping payloads
/// were absent from the bundle.
#[tokio::test]
async fn export_carries_registered_citation_sidecar_rows() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let user = UserId::new(Uuid::now_v7());
        let owner = OwnerRef::Personal(user);
        let pool = pg.pool_for_tests();

        // No core payload registers a citation sidecar, so the coverage is a
        // synthetic registration: the export takes the table lists as
        // arguments, exactly as a flavor's frozen registry supplies them.
        sqlx::query(
            "CREATE TABLE proxima_core.test_cited_object_v1 (
                 cited_object_id uuid PRIMARY KEY
                     REFERENCES proxima_core.blob (blob_id),
                 body text NOT NULL
             )",
        )
        .execute(pool)
        .await?;
        sqlx::query(
            "CREATE TABLE proxima_core.test_citation_mapping_v1 (
                 citation_mapping_id uuid PRIMARY KEY
                     REFERENCES proxima_core.blob (blob_id),
                 page_from integer NOT NULL
             )",
        )
        .execute(pool)
        .await?;

        let other = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        for (holder, schema, seed, body, page) in [
            (owner, "core/cited-v1", 7u8, "mine", 4),
            (other, "core/cited-v1", 9u8, "not mine", 11),
        ] {
            let permit = OwnerWritePermit::new_for_tests(holder, AccessKind::Fact);
            ingest_fact_atomic(pool, &permit, &draft("fact", vec![], vec![]), None).await?;
            let blob_id: Uuid = sqlx::query_scalar(
                "INSERT INTO proxima_core.blob (owner_id, schema_id, content_hash)
                 VALUES ($1, $2, $3)
                 RETURNING blob_id",
            )
            .bind(holder.stored_owner_id())
            .bind(schema)
            .bind(vec![seed; 32])
            .fetch_one(pool)
            .await?;
            sqlx::query(
                "INSERT INTO proxima_core.test_cited_object_v1 (cited_object_id, body)
                 VALUES ($1, $2)",
            )
            .bind(blob_id)
            .bind(body)
            .execute(pool)
            .await?;
            sqlx::query(
                "INSERT INTO proxima_core.test_citation_mapping_v1
                     (citation_mapping_id, page_from)
                 VALUES ($1, $2)",
            )
            .bind(blob_id)
            .bind(page)
            .execute(pool)
            .await?;
        }

        let auth = ExportAuthorization::new_for_tests(ComplianceExportTarget::PersonalOwner {
            user_id: user,
        });
        let bundle = pg
            .export_owner_bundle(
                &auth,
                &ComplianceSidecarTables {
                    citation_mapping: vec!["proxima_core.test_citation_mapping_v1".to_owned()],
                    cited_object: vec!["proxima_core.test_cited_object_v1".to_owned()],
                    ..ComplianceSidecarTables::default()
                },
            )
            .await?;

        assert_eq!(bundle.counts.sidecar_rows, 2, "got {:?}", bundle.sidecars);
        let tables: Vec<&str> = bundle
            .sidecars
            .iter()
            .map(|sidecar| sidecar.table.as_str())
            .collect();
        assert_eq!(
            tables,
            vec![
                "proxima_core.test_citation_mapping_v1",
                "proxima_core.test_cited_object_v1"
            ]
        );
        let mapping = &bundle.sidecars[0].rows;
        let cited = &bundle.sidecars[1].rows;
        assert_eq!(mapping.len(), 1);
        assert_eq!(mapping[0]["page_from"].as_i64(), Some(4));
        assert_eq!(cited.len(), 1);
        assert_eq!(
            cited[0]["body"].as_str(),
            Some("mine"),
            "the join filters on blob.owner_id"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("citation sidecar export test failed");
}

/// Opaque CitedObject schemas deliberately have no registered sidecar. Their
/// `blob` row is therefore the only portable record of schema and content
/// identity; export must include it without leaking a neighbouring owner's row.
#[tokio::test]
async fn export_carries_owner_scoped_opaque_blob_metadata() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let user = UserId::new(Uuid::now_v7());
        let owner = OwnerRef::Personal(user);
        let other = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let pool = pg.pool_for_tests();

        for holder in [owner, other] {
            let permit = OwnerWritePermit::new_for_tests(holder, AccessKind::Fact);
            ingest_fact_atomic(pool, &permit, &draft("fact", vec![], vec![]), None).await?;
        }

        let blob_id: Uuid = sqlx::query_scalar(
            "INSERT INTO proxima_core.blob (owner_id, schema_id, content_hash)
             VALUES ($1, 'opaque/archive-v1', $2)
             RETURNING blob_id",
        )
        .bind(owner.stored_owner_id())
        .bind(vec![7u8; 32])
        .fetch_one(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.blob (owner_id, schema_id, content_hash)
             VALUES ($1, 'opaque/archive-v1', $2)",
        )
        .bind(other.stored_owner_id())
        .bind(vec![9u8; 32])
        .execute(pool)
        .await?;

        let auth = ExportAuthorization::new_for_tests(ComplianceExportTarget::PersonalOwner {
            user_id: user,
        });
        let bundle = pg
            .export_owner_bundle(&auth, &contract_sidecar_tables())
            .await?;

        assert_eq!(bundle.counts.blobs, 1);
        assert_eq!(bundle.blobs.len(), 1);
        let blob = &bundle.blobs[0];
        assert_eq!(
            blob["blob_id"].as_str(),
            Some(blob_id.to_string()).as_deref()
        );
        assert_eq!(blob["schema_id"].as_str(), Some("opaque/archive-v1"));
        assert_eq!(
            blob["content_hash"].as_str(),
            Some(format!("\\x{}", "07".repeat(32))).as_deref()
        );
        let mut fields: Vec<&str> = blob
            .as_object()
            .expect("blob export row is an object")
            .keys()
            .map(String::as_str)
            .collect();
        fields.sort_unstable();
        assert_eq!(
            fields,
            vec!["blob_id", "content_hash", "schema_id"],
            "the blob export is a stable allowlist"
        );
        assert!(bundle.sidecars.is_empty(), "opaque schemas need no sidecar");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("opaque blob export test failed");
}
