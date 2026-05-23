//! Tool surfaces the harness exposes to the model.
//!
//! Two sources:
//! - **Substrate**: wake-visible substrate tools resolved by the
//!   `HarnessSubstrateBridge`; includes registered MCP descriptors
//!   and personality substrate-pack tools.
//! - **Flavor**: same shape as substrate; the harness doesn't
//!   distinguish them at the dispatch layer.

use proxima_core::harness::SubstrateToolBinding;
use proxima_core::verbs::schema::PayloadKind;

pub mod strict_inventory;
pub mod strict_schema;
pub mod substrate_dispatch;

/// Resolved binding per tool in the active palette.
#[derive(Clone)]
pub enum ToolBinding {
    Substrate(SubstrateToolBinding),
    TypedEmit {
        internal: SubstrateToolBinding,
        schema_id: String,
        schema_version: u32,
        kind: PayloadKind,
    },
}

impl std::fmt::Debug for ToolBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Substrate(s) => f.debug_tuple("Substrate").field(&s.canonical_name).finish(),
            Self::TypedEmit {
                internal,
                schema_id,
                schema_version,
                kind,
            } => f
                .debug_struct("TypedEmit")
                .field("internal", &internal.canonical_name)
                .field("schema_id", schema_id)
                .field("schema_version", schema_version)
                .field("kind", kind)
                .finish(),
        }
    }
}
