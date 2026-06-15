//! Payload traits per docs/03 — typing layer required for
//! every Memory kind.
//!
//! `SCHEMA_ID` is `&'static str` (not `SchemaId`) so it can be a
//! `const`. The runtime `SchemaId` (a String wrapper) is built
//! by the `schema_id()` helper at registration time. This is a
//! deliberate divergence from the doc-illustrative
//! `const SCHEMA_ID: SchemaId = ...` shape: that requires
//! const-construction of `String`, which Rust does not allow.

use crate::{RelationClass, SchemaId, StorageError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SearchProjectionColumnKind {
    Text,
    TextArray,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SearchProjectionField {
    pub column: &'static str,
    pub kind: SearchProjectionColumnKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchProjection {
    pub fields: &'static [SearchProjectionField],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FactTombstone {
    pub column: &'static str,
    pub value: &'static str,
}

pub trait FactPayload: serde::Serialize + serde::de::DeserializeOwned + 'static {
    const SCHEMA_ID: &'static str;
    const SCHEMA_VERSION: u32;
    /// GDPR Art. 9 (and analogous regimes') special-category flag.
    /// Defaults to `false`; controllers handling health, biometric,
    /// political, or other heightened-protection categories must
    /// override to `true`. See docs/03 §Special-category declaration
    /// and docs/13 §Compliance vocabulary.
    const SPECIAL_CATEGORY: bool = false;
    fn render(&self) -> String;
    /// Per-schema typed Fact sidecar table, or `None` when the Fact
    /// carries no sidecar of its own (its typed payload lives elsewhere,
    /// e.g. in a citation cited-object). Defaults to `None`; mirrors the
    /// optional-sidecar treatment of `CitationMappingPayload`.
    #[must_use]
    fn sidecar_table() -> Option<&'static str> {
        None
    }
    /// Natural-key columns on the sidecar table for stateful Fact
    /// schemas. Default empty = stateless (every observation is a
    /// distinct head). When non-empty, the schema participates in
    /// head-by-natural-key queries (docs/03 §Stateful Fact schemas).
    #[must_use]
    fn natural_key_columns() -> &'static [&'static str] {
        &[]
    }
    /// Optional discriminator for stateful Fact deletion observations.
    /// Storage uses this build-time metadata for `PresentOnly` queries.
    #[must_use]
    fn tombstone() -> Option<FactTombstone> {
        None
    }
    /// Build-time lexical search projection. Only human-meaningful
    /// text columns belong here; raw JSON, code bodies, logs, and
    /// opaque ids stay out of `core/search_memories`.
    #[must_use]
    fn search_projection() -> Option<SearchProjection> {
        None
    }
    #[must_use]
    fn json_schema() -> Option<serde_json::Value> {
        None
    }
    #[must_use]
    fn schema_id() -> SchemaId {
        SchemaId::new(Self::SCHEMA_ID.to_string())
    }
}

pub trait AbstractionPayload: serde::Serialize + serde::de::DeserializeOwned + 'static {
    const SCHEMA_ID: &'static str;
    const SCHEMA_VERSION: u32;
    /// See `FactPayload::SPECIAL_CATEGORY`.
    const SPECIAL_CATEGORY: bool = false;
    fn sidecar_table() -> &'static str;
    #[must_use]
    fn search_projection() -> Option<SearchProjection> {
        None
    }
    #[must_use]
    fn json_schema() -> Option<serde_json::Value> {
        None
    }
    #[must_use]
    fn schema_id() -> SchemaId {
        SchemaId::new(Self::SCHEMA_ID.to_string())
    }
}

pub trait PerspectivePayload: serde::Serialize + serde::de::DeserializeOwned + 'static {
    const SCHEMA_ID: &'static str;
    const SCHEMA_VERSION: u32;
    /// See `FactPayload::SPECIAL_CATEGORY`.
    const SPECIAL_CATEGORY: bool = false;
    fn sidecar_table() -> &'static str;
    #[must_use]
    fn search_projection() -> Option<SearchProjection> {
        None
    }
    #[must_use]
    fn json_schema() -> Option<serde_json::Value> {
        None
    }
    #[must_use]
    fn schema_id() -> SchemaId {
        SchemaId::new(Self::SCHEMA_ID.to_string())
    }
}

/// Typed payload for a Goal row in `proxima_core.goals`.
/// Mirrors `FactPayload` / `AbstractionPayload` for the Goal layer.
///
/// See docs/06 §Goal entity and docs/03 §Sidecar tables.
pub trait GoalPayload: serde::Serialize + serde::de::DeserializeOwned + 'static {
    const SCHEMA_ID: &'static str;
    const SCHEMA_VERSION: u32;
    /// See `FactPayload::SPECIAL_CATEGORY`.
    const SPECIAL_CATEGORY: bool = false;
    fn sidecar_table() -> &'static str;
    #[must_use]
    fn json_schema() -> Option<serde_json::Value> {
        None
    }
    #[must_use]
    fn schema_id() -> SchemaId {
        SchemaId::new(Self::SCHEMA_ID.to_string())
    }
}

/// Typed payload for an edge row in `proxima_core.edges`. Mirrors
/// `FactPayload` / `AbstractionPayload` for the edge layer; opt-in
/// per relation via `RelationDescriptor::payload_schema`.
///
/// `RELATION_CLASS` pins the substrate class that edges carrying this
/// payload must declare. The atomic edge-write verb cross-checks the
/// descriptor's class against this constant at registration time so
/// a payload cannot be misfiled across classes.
///
/// See docs/03 §`EdgePayload` and docs/02 §"Typed edge payloads".
pub trait EdgePayload: serde::Serialize + serde::de::DeserializeOwned + 'static {
    const SCHEMA_ID: &'static str;
    const SCHEMA_VERSION: u32;
    const RELATION_CLASS: RelationClass;
    /// See `FactPayload::SPECIAL_CATEGORY`.
    const SPECIAL_CATEGORY: bool = false;
    /// Sidecar table identifier (qualified, e.g.
    /// `"proxima_code.code_calls_v1"`). The table's primary key is
    /// `edge_id uuid` referencing `proxima_core.edges(edge_id)`.
    fn sidecar_table() -> &'static str;
    #[must_use]
    fn json_schema() -> Option<serde_json::Value> {
        None
    }
    #[must_use]
    fn schema_id() -> SchemaId {
        SchemaId::new(Self::SCHEMA_ID.to_string())
    }
}

/// Typed payload for a `cited_objects` row, keyed on
/// `cited_object_id`. Cited objects do not participate in F/A/P
/// queries; the sidecar stores the artifact body, while the core row
/// stores ownership and a content-addressed hash. See docs/11
/// §"Trait families".
pub trait CitedObjectPayload:
    serde::Serialize + serde::de::DeserializeOwned + Send + Sync + 'static
{
    const SCHEMA_ID: &'static str;
    const SCHEMA_VERSION: u32;
    /// See `FactPayload::SPECIAL_CATEGORY`.
    const SPECIAL_CATEGORY: bool = false;
    fn sidecar_table() -> &'static str;
    #[must_use]
    fn json_schema() -> Option<serde_json::Value> {
        None
    }
    #[must_use]
    fn schema_id() -> SchemaId {
        SchemaId::new(Self::SCHEMA_ID.to_string())
    }

    /// Stable BLAKE3-32 hash of the artifact content. Re-ingesting
    /// the same artifact for the same Owner deduplicates the
    /// `cited_objects` row via `(owner, schema_id, content_hash)`.
    fn idempotency_key(&self) -> [u8; 32];

    fn sidecar_insert<'t>(
        &'t self,
        _tx: &'t mut sqlx::Transaction<'_, sqlx::Postgres>,
        _sidecar_row_id: uuid::Uuid,
    ) -> futures::future::BoxFuture<'t, Result<(), StorageError>> {
        Box::pin(async move { Err(Self::missing_inline_sidecar_inserter_error()) })
    }

    #[must_use]
    fn missing_inline_sidecar_inserter_error() -> StorageError {
        StorageError::Internal(format!(
            "schema {} has no inline sidecar inserter",
            Self::SCHEMA_ID,
        ))
    }
}

/// Typed payload for a `citation_mappings` row, keyed on
/// `citation_mapping_id`. Citation mappings pin exactly one Memory
/// to exactly one `CitedObject`. The link itself — memory, cited
/// object, owner, schema — lives in the generic `citation_mappings`
/// table; a sidecar is **optional**, needed only when the mapping
/// carries schema-specific metadata such as byte ranges. A fieldless
/// mapping (a pure link, the common case) returns `None` and gets no
/// sidecar table. See docs/11 §"Trait families".
pub trait CitationMappingPayload:
    serde::Serialize + serde::de::DeserializeOwned + Send + Sync + 'static
{
    const SCHEMA_ID: &'static str;
    const SCHEMA_VERSION: u32;
    /// See `FactPayload::SPECIAL_CATEGORY`.
    const SPECIAL_CATEGORY: bool = false;
    /// Sidecar table for this mapping's schema-specific metadata, or
    /// `None` when the mapping is a pure link with no extra columns.
    /// Returning `None` means no sidecar row is written and no table is
    /// required — don't mint an empty table just to satisfy the trait.
    #[must_use]
    fn sidecar_table() -> Option<&'static str> {
        None
    }
    #[must_use]
    fn json_schema() -> Option<serde_json::Value> {
        None
    }
    #[must_use]
    fn schema_id() -> SchemaId {
        SchemaId::new(Self::SCHEMA_ID.to_string())
    }

    /// Schema id of the `CitedObjectPayload` this mapping is allowed
    /// to annotate.
    fn cited_object_schema() -> SchemaId;

    fn sidecar_insert<'t>(
        &'t self,
        _tx: &'t mut sqlx::Transaction<'_, sqlx::Postgres>,
        _sidecar_row_id: uuid::Uuid,
    ) -> futures::future::BoxFuture<'t, Result<(), StorageError>> {
        Box::pin(async move { Err(Self::missing_inline_sidecar_inserter_error()) })
    }

    #[must_use]
    fn missing_inline_sidecar_inserter_error() -> StorageError {
        StorageError::Internal(format!(
            "schema {} has no inline sidecar inserter",
            Self::SCHEMA_ID,
        ))
    }
}
