//! Substrate-shipped tool pack auto-prepended to every personality's
//! effective palette.
//!
//! Read tools append to `PersonalityToolContext.read_log` on success;
//! write tools (`emit_abstraction`, `emit_perspective`) snapshot the
//! union of `{triggering_event} ∪ read_log` for auto-wired
//! Provenance and `wake_chain_depth` per spec §Substrate-shipped tool
//! pack.

use std::sync::{Arc, OnceLock};

use crate::personality::PersonalityTool;

mod emit_abstraction;
mod emit_perspective;
mod fetch_memory;
mod list_active_goals;
mod list_self_perspectives;
mod search_memories;
mod shared;
mod walk_lineage;

#[doc(hidden)]
pub use shared::model_id_from_wake_invocation as __test_only_model_id_from_wake_invocation;

pub use emit_abstraction::EmitAbstractionTool;
pub use emit_perspective::EmitPerspectiveTool;
pub use fetch_memory::FetchMemoryTool;
pub use list_active_goals::{ActiveGoalSummary, ListActiveGoalsTool};
pub use list_self_perspectives::ListSelfPerspectivesTool;
pub use search_memories::SearchMemoriesTool;
pub use walk_lineage::WalkLineageTool;

/// Lazy-built static palette of substrate-pack tools. Auto-prepended
/// to every personality's effective palette in dispatcher
/// context-building.
#[must_use]
pub fn substrate_pack() -> &'static [Arc<dyn PersonalityTool>] {
    static PACK: OnceLock<Vec<Arc<dyn PersonalityTool>>> = OnceLock::new();
    PACK.get_or_init(|| {
        vec![
            Arc::new(FetchMemoryTool) as Arc<dyn PersonalityTool>,
            Arc::new(ListSelfPerspectivesTool),
            Arc::new(WalkLineageTool),
            Arc::new(SearchMemoriesTool),
            Arc::new(ListActiveGoalsTool),
            Arc::new(EmitAbstractionTool),
            Arc::new(EmitPerspectiveTool),
        ]
    })
    .as_slice()
}
