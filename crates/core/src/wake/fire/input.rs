//! Input types for wake fire operations.

use std::time::Duration;

use uuid::Uuid;

use crate::Owner;
use crate::personality::{PersonalityInstanceId, WakeEntryRow};

#[derive(Debug, Clone)]
pub struct FireWakeContinuation {
    pub intervention_decision_memory_id: crate::MemoryId,
    pub intervention_request_memory_id: crate::MemoryId,
    pub original_invocation_id: Uuid,
    pub wake_trace_memory_id: crate::MemoryId,
    pub triggering_memory_id: crate::MemoryId,
    pub grant_rounds: u16,
    pub rationale: String,
}

/// Inputs to one wake fire — assembled by the dispatcher tick from the
/// `WakeDispatchEntryRow` it just matched.
#[derive(Debug, Clone)]
pub struct FireWakeEntryInput {
    pub owner: Owner,
    pub personality_instance_id: PersonalityInstanceId,
    pub wake_entry: WakeEntryRow,
    pub change_event_seq: Uuid,
    pub triggering_memory_id: Uuid,
    pub continuation: Option<FireWakeContinuation>,
}

/// Per-invocation timeout calculation.
/// Conservative: 60s per round + 30s startup. Adapter-side timeouts
/// are the floor; the dispatcher's outer cancel signal is the ceiling.
/// Phase 1e tunes this once Code-flavor wake entries have a measured p95.
pub fn per_invocation_timeout(max_rounds: u32) -> Duration {
    if max_rounds == 0 {
        return Duration::from_secs(24 * 60 * 60);
    }
    Duration::from_secs(30 + u64::from(max_rounds) * 60)
}
