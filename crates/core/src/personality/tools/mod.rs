//! Substrate-shipped tool pack auto-prepended to every personality's
//! effective palette.
//!
//! Read tools append to `PersonalityToolContext.read_log` on success;
//! write tools (`emit_*`, `create_edge`) snapshot the union of
//! `{triggering_event} ∪ read_log` for auto-wired Provenance and
//! `wake_chain_depth` per spec §Substrate-shipped tool pack.

use std::sync::{Arc, OnceLock};

use crate::personality::PersonalityTool;

mod create_edge;
mod emit_abstraction;
mod emit_goal;
mod emit_perspective;
mod fetch_memory;
mod list_active_goals;
mod list_self_perspectives;
mod search_by_embedding;
mod shared;
mod walk_lineage;

pub use create_edge::CreateEdgeTool;
pub use emit_abstraction::EmitAbstractionTool;
pub use emit_goal::EmitGoalTool;
pub use emit_perspective::EmitPerspectiveTool;
pub use fetch_memory::FetchMemoryTool;
pub use list_active_goals::{ActiveGoalSummary, ListActiveGoalsTool};
pub use list_self_perspectives::ListSelfPerspectivesTool;
pub use search_by_embedding::SearchByEmbeddingTool;
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
            Arc::new(SearchByEmbeddingTool),
            Arc::new(ListActiveGoalsTool),
            Arc::new(EmitAbstractionTool),
            Arc::new(EmitPerspectiveTool),
            Arc::new(EmitGoalTool),
            Arc::new(CreateEdgeTool),
        ]
    })
    .as_slice()
}
