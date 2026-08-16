//! Export projects pins from memory rows; no reconstructed Edge table.
#![allow(clippy::doc_markdown, clippy::too_many_lines)]

use proxima_core::compliance::{ComplianceExportTarget, ExportAuthorization};
use proxima_core::storage_ports::{ComplianceErasePort, OwnerWritePermit};
use proxima_core::verbs::fact_ingest::FactWriteCommand;
use proxima_core::verbs::query::EntityKind;
use proxima_core::{AccessKind, EdgeEndpoint, MemoryId, OwnerRef, SchemaId, SchemaVersion, UserId};
use proxima_pg_testkit::{create_db, db_url, drop_db};
use proxima_storage_pg::PgStorage;
use proxima_storage_pg::verbs::fact_ingest::ingest_fact_atomic;
use uuid::Uuid;

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
        let bundle = pg.export_owner_bundle(&auth, &[], &[], &[], &[]).await?;
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
