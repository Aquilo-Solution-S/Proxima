//! Example out-of-tree flavor: one Fact schema for "a document was filed".

use proxima::flavor::{
    FactPayload, FlavorBundle, FlavorRegistry, FlavorRegistryError, PayloadKeyBuilder,
    SearchProjection, SearchProjectionColumnKind, SearchProjectionField,
};
use proxima::{AppInfo, FlavorApp, NamedMigrator};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentFiledV1 {
    pub source_path: String,
    pub title: String,
}

impl FactPayload for DocumentFiledV1 {
    const SCHEMA_ID: &'static str = "embedded-minimal/document-filed-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn receipt_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_str("source_path", &self.source_path);
        key.finish()
    }

    fn render(&self) -> String {
        format!("Document filed: {} ({})", self.title, self.source_path)
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("embedded_minimal.document_filed_v1")
    }

    fn search_projection() -> Option<SearchProjection> {
        Some(SearchProjection {
            fields: &[SearchProjectionField {
                column: "title",
                kind: SearchProjectionColumnKind::Text,
            }],
            tag_column: None,
            tsv_column: None,
        })
    }
}

proxima::flavor::pg_sidecar! {
    payload: DocumentFiledV1,
    row: DocumentFiledRow,
    kinds: [Fact],
    table: "embedded_minimal.document_filed_v1",
    key: memory_id,
    fields: {
        source_path => source_path: (text),
        title => title: (text),
    },
}

proxima::flavor::proxima_flavor! {
    name = "embedded-minimal",
    display_name = "Embedded Minimal Example",
    fact_schemas = [DocumentFiledV1],
    abstraction_schemas = [],
    perspective_schemas = [],
    goal_schemas = [],
    edge_schemas = [],
    relations = [],
    mcp_tools = [],
}

#[must_use]
pub fn migrator() -> sqlx::migrate::Migrator {
    let mut migrator = sqlx::migrate!("./migrations");
    migrator.set_ignore_missing(true);
    migrator
}

pub struct EmbeddedMinimalFlavor;

impl FlavorBundle for EmbeddedMinimalFlavor {
    fn register(registry: &mut FlavorRegistry) -> Result<(), FlavorRegistryError> {
        self::register(registry)
    }

    fn register_pg_sidecars(registry: &mut proxima::flavor::PgSidecarRegistry) {
        registry.add_fact::<DocumentFiledV1>();
    }

    fn migrators() -> Vec<NamedMigrator> {
        vec![NamedMigrator::new("embedded-minimal", migrator())]
    }
}

impl FlavorApp for EmbeddedMinimalFlavor {
    fn app_info() -> AppInfo {
        AppInfo {
            id: "embedded-minimal",
            title: "Embedded Minimal Example",
            version: "0.1.0",
        }
    }
}

#[cfg(test)]
mod tests {
    use proxima::flavor::{PgMemoryPayload, PgSidecarReadCtx, SidecarPayload};
    use proxima::{
        FactWriteCommand, OwnerRef, PayloadKind, Proxima, Relation, SourceBatchId, ToolScope,
        UserId,
    };
    use proxima_pg_testkit::{create_db, db_url, drop_db, unique_db_name};

    use super::{DocumentFiledV1, EmbeddedMinimalFlavor};

    #[tokio::test]
    async fn document_filed_sidecar_roundtrips_through_migrations() {
        let db_name = unique_db_name("embedded_minimal");
        create_db(&db_name).await.expect("PG required for tests");
        let url = db_url(&db_name);

        let result: Result<(), Box<dyn std::error::Error>> = async {
            let booted = Proxima::<EmbeddedMinimalFlavor>::app()
                .database_url(url)
                .owner(OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7())))
                .allow_insecure_single_owner()
                .tool_scope(ToolScope::All)
                .build()
                .await?;
            let authz = booted
                .single_owner_authz()
                .expect("insecure single-owner mode is enabled");
            let payload = DocumentFiledV1 {
                source_path: "/example/intake/r-2026-0001.pdf".into(),
                title: "Example invoice".into(),
            };
            let draft = FactWriteCommand::from_payload(
                "embedded-minimal/test",
                SourceBatchId::new(uuid::Uuid::now_v7()),
                &payload,
                time::OffsetDateTime::now_utc(),
            );
            let authorized = booted
                .engine
                .authorize_fact_ingest(&authz, Relation::Ingest, draft)
                .await?;
            let outcome = booted
                .engine
                .ingest_fact_with_typed_sidecar(
                    &authorized,
                    &SidecarPayload::fact(payload.clone()),
                    None,
                )
                .await?;

            let loaded = DocumentFiledV1::load_batch(
                PgSidecarReadCtx::from(booted.pool_for_tests()),
                PayloadKind::Fact,
                &[outcome.memory_id],
            )
            .await?;
            assert_eq!(loaded.len(), 1, "document_filed_v1 sidecar row must exist");
            let (loaded_memory_id, loaded) = loaded.into_iter().next().expect("checked len");
            assert_eq!(loaded_memory_id, outcome.memory_id);
            assert_eq!(loaded.kind, PayloadKind::Fact);
            assert_eq!(
                loaded.downcast_ref::<DocumentFiledV1>(),
                Some(&DocumentFiledV1 {
                    source_path: "/example/intake/r-2026-0001.pdf".into(),
                    title: "Example invoice".into(),
                })
            );

            booted.shutdown();
            Ok(())
        }
        .await;

        let _ = drop_db(&db_name).await;
        result.expect("document_filed_sidecar_roundtrips_through_migrations failed");
    }
}
