//! Generic read-side consolidation helpers.

#![allow(clippy::missing_errors_doc, clippy::type_complexity)]

mod dependencies;
mod events;
mod memories;

pub use dependencies::list_memory_dependencies;
pub(crate) use events::edge_event_visibility_predicate;
pub use events::{list_change_events_after, list_change_events_for_replay};
pub use memories::{
    load_abstraction_heads, load_memories_by_ids, load_memory_batch_facts, load_memory_by_id,
};
