//! Detect-only wake-entry validation.

use std::collections::HashSet;

use crate::error::ProtocolError;
use crate::{WakeEntryDraft, WakeEntryTriggerKind};

/// Validate wake-entry detect config only.
///
/// # Errors
///
/// Returns `DuplicateTriggerInRequest` for repeated trigger pairs and
/// `InvalidArgument` for malformed detect/config fields.
pub fn validate_wake_entries_detect_config(
    entries: &[WakeEntryDraft],
) -> Result<(), ProtocolError> {
    validate_unique_triggers(entries)?;
    for entry in entries {
        validate_entry_shape(entry)?;
    }
    Ok(())
}

fn validate_unique_triggers(entries: &[WakeEntryDraft]) -> Result<(), ProtocolError> {
    let mut seen: HashSet<(WakeEntryTriggerKind, &str)> = HashSet::new();
    for entry in entries {
        if !seen.insert((entry.trigger_kind, entry.trigger_id.as_str())) {
            return Err(ProtocolError::duplicate_trigger_in_request(
                entry.trigger_kind.as_str(),
                &entry.trigger_id,
            ));
        }
    }
    Ok(())
}

fn validate_entry_shape(entry: &WakeEntryDraft) -> Result<(), ProtocolError> {
    if entry.trigger_id.trim().is_empty() {
        return Err(ProtocolError::invalid_argument(
            "trigger_id",
            "must be non-empty",
        ));
    }
    if entry.label.trim().is_empty() {
        return Err(ProtocolError::invalid_argument(
            "label",
            "must be non-empty",
        ));
    }
    if entry.probability_promille > 1000 {
        return Err(ProtocolError::invalid_argument(
            "probability_promille",
            "must be between 0 and 1000",
        ));
    }
    Ok(())
}
