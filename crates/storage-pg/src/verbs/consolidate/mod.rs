//! Generic read-side consolidation helpers.

// The sites are `sqlx::query_as` row tuples read straight into a destructuring
// `let`. Naming them would put a type alias between the SELECT list and the
// binding that mirrors it, which is where the drift would start.
#![allow(clippy::type_complexity)]

mod events;
mod memories;

pub use events::{list_change_events_after, list_change_events_for_replay};
pub use memories::{
    load_abstraction_heads, load_memories_by_ids, load_memory_batch_facts, load_memory_by_id,
    load_memory_graph_payloads,
};
