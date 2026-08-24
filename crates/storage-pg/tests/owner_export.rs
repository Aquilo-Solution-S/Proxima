//! Export projects pins from memory rows; no reconstructed Edge table.
#![allow(clippy::doc_markdown, clippy::too_many_lines)]

use proxima_core::owner_inverse::{ExportAuthorization, OwnerExportTarget, OwnerSurfaces};
use proxima_core::storage_ports::FactIngestPort;
use proxima_core::storage_ports::{OwnerInversePort, OwnerWritePermit};
use proxima_core::verbs::fact_ingest::FactWriteCommand;
use proxima_core::verbs::query::EntityKind;
use proxima_core::{AccessKind, EdgeEndpoint, MemoryId, OwnerRef, SchemaId, SchemaVersion, UserId};
use proxima_pg_testkit::{create_db, db_url, drop_db};
use proxima_storage_pg::PgStorage;
use proxima_storage_pg::core_pg_sidecars;
use proxima_storage_pg::verbs::forget::{MemoryColdStore, cold_object_key, forget_memory};
use uuid::Uuid;

/// The five sidecar legs exactly as the engine assembles them: from the
/// frozen flavor registry. Passing empty slices here would silently skip
/// the owner-pinned leg, which is the difference these tests exist to
/// measure.
fn contract_sidecar_tables() -> OwnerSurfaces {
    OwnerSurfaces::for_registry(&proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests())
}

/// The two synthetic citation surfaces, declared exactly as a flavor would:
/// keyed on a blob under a column of their own naming, carrying no `owner_id`
/// of their own, so the generated statement reaches the owner through
/// `proxima_core.blob`.
fn citation_surfaces() -> OwnerSurfaces {
    use proxima_core::flavor::{
        CounterRule, EraseRule, ExportRule, ForgetRule, KeyShape, Surface, TransferRule,
    };
    const fn citation(table: &'static str, column: &'static str) -> Surface {
        Surface {
            table,
            key: KeyShape::BlobId { column },
            owner_column: None,
            transfer: TransferRule::StaysOnKey,
            erase: EraseRule::ByKey,
            export: ExportRule::Rows,
            forget: ForgetRule::Keep {
                why: "a citation outlives the Fact that made it",
            },
            lexical_language_column: None,
            counter: CounterRule::Counted("sidecar_rows"),
            completeness: None,
        }
    }
    OwnerSurfaces::from_surfaces(vec![
        citation("proxima_core.test_cited_object_v1", "cited_object_id"),
        citation(
            "proxima_core.test_citation_mapping_v1",
            "citation_mapping_id",
        ),
    ])
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

        let leaf = pg
            .ingest_fact_atomic(&permit, &draft("fact", vec![], vec![]), None)
            .await?;
        let derived = pg
            .ingest_fact_atomic(
                &permit,
                &draft("abstraction", vec![], vec![leaf.memory_id.into_inner()]),
                None,
            )
            .await?;

        let auth =
            ExportAuthorization::new_for_tests(OwnerExportTarget::PersonalOwner { user_id: user });
        let bundle = pg
            .export_owner_bundle(&auth, &contract_sidecar_tables())
            .await?;
        assert_eq!(bundle.count("proxima_core.memory"), 2);
        assert_eq!(bundle.count("edges"), 1);
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

        let hot = pg
            .ingest_fact_atomic(&permit, &sketched("fact", "Still hot"), None)
            .await?;
        let cooled = pg
            .ingest_fact_atomic(&permit, &sketched("fact", "Went cold\nbody"), None)
            .await?;
        let cooled_t = cooled.memory_id.into_inner();
        let key = cold_object_key(cooled_t);
        let mut tx = pool.begin().await?;
        forget_memory(
            &mut tx,
            &core_pg_sidecars(),
            &contract_sidecar_tables(),
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
        let neighbour = pg
            .ingest_fact_atomic(&other_permit, &sketched("fact", "Not yours"), None)
            .await?;
        let neighbour_t = neighbour.memory_id.into_inner();
        let neighbour_key = cold_object_key(neighbour_t);
        let mut tx = pool.begin().await?;
        forget_memory(
            &mut tx,
            &core_pg_sidecars(),
            &contract_sidecar_tables(),
            &cold,
            &neighbour_key,
            neighbour_t,
            other.stored_owner_id(),
        )
        .await?;
        tx.commit().await?;

        let auth =
            ExportAuthorization::new_for_tests(OwnerExportTarget::PersonalOwner { user_id: user });
        let bundle = pg
            .export_owner_bundle(&auth, &contract_sidecar_tables())
            .await?;

        assert_eq!(
            bundle.count("proxima_core.memory"),
            1,
            "the cooled admission left memory"
        );
        let cooled_rows = bundle.table("proxima_core.cooled");
        assert_eq!(bundle.count("proxima_core.cooled"), 1);
        assert_eq!(cooled_rows.len(), 1);
        assert_eq!(
            cooled_rows[0]["t"].as_str(),
            Some(cooled_t.to_string()).as_deref()
        );
        assert_eq!(
            cooled_rows[0]["object_key"].as_str(),
            Some(key.as_str()),
            "the bundle carries the locator, not the cold bytes"
        );

        let sketches = bundle.table("proxima_core.sketch");
        assert_eq!(
            bundle.count("proxima_core.sketch"),
            1,
            "forget deletes the cooled row's sketch; the hot one remains"
        );
        assert_eq!(sketches.len(), 1);
        assert_eq!(
            sketches[0]["t"].as_str(),
            Some(hot.memory_id.into_inner().to_string()).as_deref()
        );
        assert_eq!(sketches[0]["text"].as_str(), Some("Still hot"));
        assert!(
            sketches[0].get("search_tsv").is_none(),
            "the sketch allowlist names four columns; the generated lexical index \
             is not one of them: {:?}",
            sketches[0]
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("cooled/sketch export test failed");
}

/// A citation is the `proxima_core.blob` row a Fact names, so both citation
/// sidecar families key on `blob_id` and are owner-filtered through
/// `blob.owner_id`. Export must act on the registered table lists it receives:
/// discarding them drops a flavor's typed cited-object and citation-mapping
/// payloads from the bundle.
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
            pg.ingest_fact_atomic(&permit, &draft("fact", vec![], vec![]), None)
                .await?;
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

        let auth =
            ExportAuthorization::new_for_tests(OwnerExportTarget::PersonalOwner { user_id: user });
        let bundle = pg.export_owner_bundle(&auth, &citation_surfaces()).await?;

        let tables: Vec<&str> = bundle.tables.keys().map(String::as_str).collect();
        assert_eq!(
            tables,
            vec![
                "proxima_core.test_citation_mapping_v1",
                "proxima_core.test_cited_object_v1"
            ],
            "the bundle carries exactly the declared surfaces"
        );
        let mapping = bundle.table("proxima_core.test_citation_mapping_v1");
        let cited = bundle.table("proxima_core.test_cited_object_v1");
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
            pg.ingest_fact_atomic(&permit, &draft("fact", vec![], vec![]), None)
                .await?;
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

        let auth =
            ExportAuthorization::new_for_tests(OwnerExportTarget::PersonalOwner { user_id: user });
        let bundle = pg
            .export_owner_bundle(&auth, &contract_sidecar_tables())
            .await?;

        assert_eq!(bundle.count("proxima_core.blob"), 1);
        let blobs = bundle.table("proxima_core.blob");
        assert_eq!(blobs.len(), 1);
        let blob = &blobs[0];
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
        assert!(
            bundle.table("proxima_core.agent_note_v1").is_empty(),
            "an opaque schema registers no sidecar, so its declared families stay empty"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("opaque blob export test failed");
}

/// The bundle carries EXACTLY the surfaces the contracts declare
/// exportable — including the ones that came back empty.
///
/// `OwnerExportBundle`'s own doc states that invariant, and the differential
/// harness cannot check it: it strips empty sections before comparing,
/// deliberately, because its goldens were captured from a corpus and a corpus
/// writes what it writes. So a surface dropped from the generator's loop simply
/// stops being exported, with nothing in the workspace failing — the exact
/// failure the "declaration generates the statement" argument is supposed to
/// make impossible.
///
/// A FRESH owner is the right subject. Every table is empty, so the only
/// thing the assertion can be measuring is which surfaces the generator
/// visited; a seeded owner would let a present-but-unwritten table hide
/// behind a present-and-empty one.
///
/// This is the flavor-#0 half. The full-registry half, where the code
/// flavor's surfaces are visible too, is in `crates/proxima`.
#[tokio::test]
async fn the_bundle_carries_every_exportable_surface_even_when_empty() {
    use proxima_core::flavor::ExportRule;

    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;

        let surfaces = contract_sidecar_tables();
        let expected: std::collections::BTreeSet<&str> = surfaces
            .surfaces()
            .iter()
            .filter(|surface| !matches!(surface.export, ExportRule::Excluded { .. }))
            .map(|surface| surface.table)
            .collect();
        assert!(
            expected.len() > 10,
            "flavor 0 declares more than ten exportable surfaces, got {}",
            expected.len()
        );

        let auth = ExportAuthorization::new_for_tests(OwnerExportTarget::PersonalOwner {
            user_id: UserId::new(Uuid::now_v7()),
        });
        let bundle = pg.export_owner_bundle(&auth, &surfaces).await?;
        let actual: std::collections::BTreeSet<&str> =
            bundle.tables.keys().map(String::as_str).collect();

        assert_eq!(
            actual,
            expected,
            "the bundle's tables must be exactly the declared exportable surfaces; \
             missing {:?}, unexpected {:?}",
            expected.difference(&actual).collect::<Vec<_>>(),
            actual.difference(&expected).collect::<Vec<_>>()
        );
        for table in &expected {
            assert!(
                bundle.table(table).is_empty(),
                "{table} should be empty for an owner that wrote nothing"
            );
            assert_eq!(
                bundle.count(table),
                0,
                "{table}'s count must be derived from its rows"
            );
        }
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("exportable surface completeness failed");
}
