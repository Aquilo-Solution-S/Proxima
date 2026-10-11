use super::{
    EntityKind, GoalId, GoalState, GoalWakeConfigWrite, MemoryId, SchemaId, SchemaVersion,
};

#[derive(Debug)]
pub(super) struct InsertedGoal {
    pub(super) goal_id: GoalId,
    pub(super) change_event_seq: uuid::Uuid,
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
    pub(super) assignment: MemoryId,
    pub(super) dependencies: Vec<GoalId>,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct EvidenceTarget {
    pub(super) kind: EntityKind,
    pub(super) memory_id: MemoryId,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum WakeWrite<'a> {
    Explicit(Option<&'a GoalWakeConfigWrite>),
    CarryFrom(GoalId),
}

/// A [`GoalDraft`](proxima_core::verbs::goal_write::GoalDraft) carries its
/// text as a `String`, and a version whose text is unknown has none to carry.
/// It travels as the empty string, and [`column_text`] is the only way back
/// into `goal.text`: a blank never reaches the column, whatever built the
/// draft.
pub(super) fn draft_text(stored: Option<&str>) -> String {
    stored.unwrap_or_default().to_owned()
}

/// The `goal.text` value of a draft's text: the text itself, or NULL when it
/// is blank.
pub(super) fn column_text(draft_text: &str) -> Option<String> {
    (!draft_text.trim().is_empty()).then(|| draft_text.to_owned())
}

#[cfg(test)]
mod tests {
    use super::{column_text, draft_text};

    #[test]
    fn unknown_text_stays_unknown_through_a_draft() {
        assert_eq!(column_text(&draft_text(None)), None);
    }

    #[test]
    fn known_text_survives_a_draft_unchanged() {
        let text = "  keep the words,\n and the edges  ";
        assert_eq!(column_text(&draft_text(Some(text))).as_deref(), Some(text));
    }

    #[test]
    fn a_blank_draft_text_never_reaches_the_column() {
        for blank in ["", " ", "\n\t ", "\u{a0}"] {
            assert_eq!(column_text(blank), None, "{blank:?}");
        }
    }
}
