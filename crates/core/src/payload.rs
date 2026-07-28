//! Payload traits per docs/03 — typing layer required for
//! every Memory kind.
//!
//! `SCHEMA_ID` is `&'static str` (not `SchemaId`) so it can be a
//! `const`. The runtime `SchemaId` (a String wrapper) is built
//! by the `schema_id()` helper at registration time. This is a
//! deliberate divergence from the doc-illustrative
//! `const SCHEMA_ID: SchemaId = ...` shape: that requires
//! const-construction of `String`, which Rust does not allow.

use crate::{RelationClass, SchemaId};

#[derive(Debug, Clone)]
pub struct PayloadKeyBuilder {
    bytes: Vec<u8>,
}

impl PayloadKeyBuilder {
    #[must_use]
    pub fn new(schema_id: &str, schema_version: u32) -> Self {
        let mut this = Self {
            bytes: b"PKEY1\0".to_vec(),
        };
        this.raw_str(schema_id);
        this.bytes.extend_from_slice(&schema_version.to_be_bytes());
        this
    }

    #[must_use]
    pub fn finish(self) -> Vec<u8> {
        self.bytes
    }

    pub fn field_str(&mut self, name: &str, value: &str) {
        self.field(name, b's');
        self.raw_str(value);
    }

    pub fn field_bool(&mut self, name: &str, value: bool) {
        self.field(name, b'b');
        self.bytes.push(u8::from(value));
    }

    pub fn field_u8(&mut self, name: &str, value: u8) {
        self.field(name, b'1');
        self.bytes.push(value);
    }

    pub fn field_u32(&mut self, name: &str, value: u32) {
        self.field(name, b'4');
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    pub fn field_i32(&mut self, name: &str, value: i32) {
        self.field(name, b'I');
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    pub fn field_i64(&mut self, name: &str, value: i64) {
        self.field(name, b'L');
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    pub fn field_u64(&mut self, name: &str, value: u64) {
        self.field(name, b'8');
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    pub fn field_usize(&mut self, name: &str, value: usize) {
        self.field_u64(name, u64::try_from(value).unwrap_or(u64::MAX));
    }

    pub fn field_uuid(&mut self, name: &str, value: uuid::Uuid) {
        self.field(name, b'u');
        self.bytes.extend_from_slice(value.as_bytes());
    }

    pub fn field_bytes(&mut self, name: &str, value: &[u8]) {
        self.field(name, b'B');
        self.raw_bytes(value);
    }

    pub fn field_time(&mut self, name: &str, value: time::OffsetDateTime) {
        self.field(name, b't');
        self.bytes
            .extend_from_slice(&value.unix_timestamp_nanos().to_be_bytes());
    }

    pub fn field_option_str(&mut self, name: &str, value: Option<&str>) {
        self.field(name, b'S');
        match value {
            Some(value) => {
                self.bytes.push(1);
                self.raw_str(value);
            }
            None => self.bytes.push(0),
        }
    }

    pub fn field_option_uuid(&mut self, name: &str, value: Option<uuid::Uuid>) {
        self.field(name, b'U');
        match value {
            Some(value) => {
                self.bytes.push(1);
                self.bytes.extend_from_slice(value.as_bytes());
            }
            None => self.bytes.push(0),
        }
    }

    pub fn field_option_bool(&mut self, name: &str, value: Option<bool>) {
        self.field(name, b'O');
        match value {
            Some(value) => {
                self.bytes.push(1);
                self.bytes.push(u8::from(value));
            }
            None => self.bytes.push(0),
        }
    }

    pub fn field_option_time(&mut self, name: &str, value: Option<time::OffsetDateTime>) {
        self.field(name, b'T');
        match value {
            Some(value) => {
                self.bytes.push(1);
                self.bytes
                    .extend_from_slice(&value.unix_timestamp_nanos().to_be_bytes());
            }
            None => self.bytes.push(0),
        }
    }

    pub fn field_str_list(&mut self, name: &str, values: &[String]) {
        self.field(name, b'[');
        self.raw_len(values.len());
        for value in values {
            self.raw_str(value);
        }
    }

    pub fn field_uuid_list(&mut self, name: &str, values: &[uuid::Uuid]) {
        self.field(name, b'[');
        self.raw_len(values.len());
        for value in values {
            self.bytes.extend_from_slice(value.as_bytes());
        }
    }

    pub fn list(&mut self, name: &str, len: usize) {
        self.field(name, b'[');
        self.raw_len(len);
    }

    fn field(&mut self, name: &str, tag: u8) {
        self.raw_str(name);
        self.bytes.push(tag);
    }

    fn raw_str(&mut self, value: &str) {
        self.raw_bytes(value.as_bytes());
    }

    fn raw_bytes(&mut self, value: &[u8]) {
        self.raw_len(value.len());
        self.bytes.extend_from_slice(value);
    }

    fn raw_len(&mut self, len: usize) {
        self.bytes
            .extend_from_slice(&u64::try_from(len).unwrap_or(u64::MAX).to_be_bytes());
    }
}

#[must_use]
pub fn schema_only_key(schema_id: &str, schema_version: u32) -> Vec<u8> {
    PayloadKeyBuilder::new(schema_id, schema_version).finish()
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchProjection {
    pub fields: &'static [SearchProjectionField],
    pub tag_column: Option<String>,
    /// Column holding the row's pre-computed lexical vector, for sidecar
    /// tables whose migration adds one (see `proxima_core.lexical_tsv`).
    /// Declaring it lets search read the stored vector instead of
    /// tokenising the projected text on every candidate row.
    pub tsv_column: Option<&'static str>,
    /// Column holding the row's lexical language (`regconfig`), for
    /// sidecar tables whose migration adds one. Search ranks each
    /// candidate with its own language's tsquery; declared, it reads the
    /// sidecar's column (which may be pinned, as the code flavor pins
    /// `english`), absent it falls back to the owning memory row's
    /// `lexical_language`. A sidecar declaring `tsv_column` over a
    /// per-row-language vector should declare this too, or ranking uses
    /// the wrong configuration for pinned rows.
    pub language_column: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FactTombstone {
    pub column: &'static str,
    pub value: &'static str,
}

pub trait FactPayload:
    serde::Serialize + serde::de::DeserializeOwned + Send + Sync + 'static
{
    const SCHEMA_ID: &'static str;
    const SCHEMA_VERSION: u32;
    /// GDPR Art. 9 (and analogous regimes') special-category flag.
    /// Defaults to `false`; controllers handling health, biometric,
    /// political, or other heightened-protection categories must
    /// override to `true`. See docs/03 §Special-category declaration
    /// and docs/13 §Compliance vocabulary.
    const SPECIAL_CATEGORY: bool = false;
    /// Schema-owned receipt replay key material. This is not a payload
    /// serialization format; the typed sidecar remains the payload.
    #[must_use]
    fn receipt_key(&self) -> Vec<u8>;
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

pub trait AbstractionPayload:
    serde::Serialize + serde::de::DeserializeOwned + Send + Sync + 'static
{
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

pub trait PerspectivePayload:
    serde::Serialize + serde::de::DeserializeOwned + Send + Sync + 'static
{
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
pub trait GoalPayload:
    serde::Serialize + serde::de::DeserializeOwned + Send + Sync + 'static
{
    const SCHEMA_ID: &'static str;
    const SCHEMA_VERSION: u32;
    /// See `FactPayload::SPECIAL_CATEGORY`.
    const SPECIAL_CATEGORY: bool = false;
    /// Schema-owned body key used for `GoalWrite` idempotency conflict
    /// checks. Title/text and authorship are compared separately.
    #[must_use]
    fn goal_key(&self) -> Vec<u8>;
    /// Per-schema typed Goal sidecar table, or `None` when the Goal's
    /// typed payload has no schema-specific storage beyond
    /// `proxima_core.goals.payload`.
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
pub trait EdgePayload:
    serde::Serialize + serde::de::DeserializeOwned + Send + Sync + 'static
{
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
}
