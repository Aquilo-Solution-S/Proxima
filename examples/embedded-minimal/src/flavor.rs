//! Example out-of-tree flavor: one Fact schema for "a document was filed".

use proxima::{AppInfo, FlavorApp, FlavorBundle, NamedMigrator};
use proxima_core::{
    FactPayload, FlavorRegistry, SearchProjection, SearchProjectionColumnKind,
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

    fn render(&self) -> String {
        format!("Document filed: {} ({})", self.title, self.source_path)
    }

    fn sidecar_table() -> &'static str {
        "embedded_minimal.document_filed_v1"
    }

    fn search_projection() -> Option<SearchProjection> {
        Some(SearchProjection {
            fields: &[SearchProjectionField {
                column: "title",
                kind: SearchProjectionColumnKind::Text,
            }],
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
