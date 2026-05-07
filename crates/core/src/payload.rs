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
    /// and docs/15 §Compliance vocabulary.
    const SPECIAL_CATEGORY: bool = false;
    fn render(&self) -> String;
    fn sidecar_table() -> &'static str;
    /// Natural-key columns on the sidecar table for stateful Fact
    /// schemas. Default empty = stateless (every observation is a
    /// distinct head). When non-empty, the schema participates in
    /// head-by-natural-key queries (docs/03 §Stateful Fact schemas).
    fn natural_key_columns() -> &'static [&'static str] {
        &[]
    }
    /// Optional discriminator for stateful Fact deletion observations.
    /// Storage uses this build-time metadata for `PresentOnly` queries.
    fn tombstone() -> Option<FactTombstone> {
        None
    }
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
/// See docs/03 §EdgePayload and docs/02 §"Typed edge payloads".
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
    fn schema_id() -> SchemaId {
        SchemaId::new(Self::SCHEMA_ID.to_string())
    }
}
