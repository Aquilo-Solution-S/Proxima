//! Personality wake/decide/write storage helpers.

#![allow(clippy::missing_errors_doc, clippy::type_complexity)]

mod dependencies;
mod events;
mod instances;
mod memories;
mod read_scope;
mod wake_entries;

pub use dependencies::list_memory_dependencies;
pub use events::{list_change_events_after, list_change_events_for_replay};
pub(crate) use instances::instantiate_personality_on_conn;
pub use instances::{instantiate_personality, list_personality_instances};
pub use memories::{
    append_personality_memories, load_abstraction_heads, load_memory_batch_facts,
    load_memory_by_id, load_perspective_heads, lookup_prior_personality_head,
};
pub use read_scope::{list_read_scope, set_read_scope};
pub use wake_entries::{set_wake_entries, set_wake_entries_within, tombstone_personality};
