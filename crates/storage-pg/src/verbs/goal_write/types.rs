use super::{GoalId, GoalState, MemoryId, SchemaId, SchemaVersion};

#[derive(Debug)]
pub(super) struct InsertedGoal {
    pub(super) goal_id: GoalId,
    pub(super) idempotent_replay: bool,
}

#[derive(Debug, Clone)]
pub(super) struct StoredGoal {
    pub(super) schema_id: SchemaId,
    pub(super) schema_version: SchemaVersion,
    pub(super) title: String,
    /// `goal.text` as stored: `None` is unknown.
    pub(super) text: Option<String>,
    pub(super) payload: Vec<u8>,
    pub(super) state: GoalState,
    pub(super) assignment: Option<MemoryId>,
    pub(super) dependencies: Vec<GoalId>,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct EvidenceTarget {
    pub(super) memory_id: MemoryId,
}

/// The columns of a Goal row that its [`GoalDraft`] does not decide.
///
/// `text` is what `goal.text` holds, and it is given, never derived from the
/// draft: a write that declares a text (create, modify, a decomposed child)
/// passes `Some(draft.text)`, and a write that only moves the lifecycle
/// (transition, achieve) passes its prior version's `StoredGoal::text` exactly
/// as loaded, `None` (unknown) included. Nothing on the way judges it, so a
/// stored value comes out byte for byte; the one judge is the database CHECK.
///
/// [`GoalDraft`]: proxima_core::verbs::goal_write::GoalDraft
#[derive(Clone, Copy)]
pub(super) struct RowColumns<'a> {
    /// The configured number encoded into this row's minted identity.
    pub(super) store_number: u16,
    pub(super) writer: proxima_core::WriterAttribution,
    pub(super) text: Option<&'a str>,
    pub(super) write_act_t: Option<uuid::Uuid>,
}
