//! Example out-of-tree flavor: one Fact schema for "a document was filed".

use proxima::{
    AppInfo, FlavorApp, FlavorBundle, MemoryId, NamedMigrator, PgMemoryPayload,
    PgMemoryPayloadFuture, PgMemorySidecar, PgSidecarFuture, SidecarPayload, StorageError,
};
use proxima_core::{
    FactPayload, FlavorRegistry, PayloadKeyBuilder, SearchProjection, SearchProjectionColumnKind,
    SearchProjectionField,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentFiledV1 {
    pub source_path: String,
    pub title: String,
}

impl FactPayload for DocumentFiledV1 {
    const SCHEMA_ID: &'static str = "embedded-minimal/document-filed-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn event_key(&self) -> Vec<u8> {
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
        })
    }
}

impl PgMemorySidecar for DocumentFiledV1 {
    fn insert_memory_sidecar<'t>(
        &'t self,
        tx: &'t mut sqlx::Transaction<'_, sqlx::Postgres>,
        memory_id: MemoryId,
    ) -> PgSidecarFuture<'t> {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO embedded_minimal.document_filed_v1
                    (memory_id, source_path, title)
                 VALUES ($1, $2, $3)",
            )
            .bind(memory_id.into_inner())
            .bind(&self.source_path)
            .bind(&self.title)
            .execute(tx.as_mut())
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            Ok(())
        })
    }
}

impl PgMemoryPayload for DocumentFiledV1 {
    fn load_memory_payload(pool: &sqlx::PgPool, memory_id: MemoryId) -> PgMemoryPayloadFuture<'_> {
        Box::pin(async move {
            let row: Option<(String, String)> = sqlx::query_as(
                "SELECT source_path, title
                   FROM embedded_minimal.document_filed_v1
                  WHERE memory_id = $1",
            )
            .bind(memory_id.into_inner())
            .fetch_optional(pool)
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            Ok(row.map(|(source_path, title)| {
                SidecarPayload::fact(DocumentFiledV1 { source_path, title })
            }))
        })
    }
}

proxima_core::proxima_flavor! {
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
    fn register(registry: &mut FlavorRegistry) {
        self::register(registry);
    }

    fn register_pg_sidecars(registry: &mut proxima::PgSidecarRegistry) {
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
