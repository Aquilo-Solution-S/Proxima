//! Payload traits per docs/03 — typing layer required for
//! every Memory kind.
//!
//! `SCHEMA_ID` is `&'static str` (not `SchemaId`) so it can be a
//! `const`. The runtime `SchemaId` (a String wrapper) is built
//! by the `schema_id()` helper at registration time. This is a
//! deliberate divergence from the doc-illustrative
//! `const SCHEMA_ID: SchemaId = ...` shape: that requires
//! const-construction of `String`, which Rust does not allow.

use crate::SchemaId;

pub trait FactPayload: serde::Serialize + serde::de::DeserializeOwned + 'static {
    const SCHEMA_ID: &'static str;
    const SCHEMA_VERSION: u32;
    fn render(&self) -> String;
    fn sidecar_table() -> &'static str;
    /// Natural-key columns on the sidecar table for stateful Fact
    /// schemas. Default empty = stateless (every observation is a
    /// distinct head). When non-empty, the schema participates in
    /// head-by-natural-key queries (docs/03 §Stateful Fact schemas).
    fn natural_key_columns() -> &'static [&'static str] {
        &[]
    }
    fn schema_id() -> SchemaId {
        SchemaId::new(Self::SCHEMA_ID.to_string())
    }
}

pub trait AbstractionPayload: serde::Serialize + serde::de::DeserializeOwned + 'static {
    const SCHEMA_ID: &'static str;
    const SCHEMA_VERSION: u32;
    fn sidecar_table() -> &'static str;
    fn schema_id() -> SchemaId {
        SchemaId::new(Self::SCHEMA_ID.to_string())
    }
}

pub trait PerspectivePayload: serde::Serialize + serde::de::DeserializeOwned + 'static {
    const SCHEMA_ID: &'static str;
    const SCHEMA_VERSION: u32;
    fn sidecar_table() -> &'static str;
    fn schema_id() -> SchemaId {
        SchemaId::new(Self::SCHEMA_ID.to_string())
    }
}
