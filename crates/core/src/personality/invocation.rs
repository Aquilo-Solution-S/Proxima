//! Wake invocation types.
//!
//! This module contains types related to wake invocations:
//! - `WakeInvocationStart` - Input for starting a wake invocation
//! - `WakeInvocationFinalize` - Input for finalizing a wake invocation
//! - `WakeInvocationLogDraft` - Draft of a wake invocation log entry
//! - `WakeInvocationLogRow` - Row of a wake invocation log entry
//! - `WakeInvocationRow` - Full wake invocation row with logs

use time::OffsetDateTime;
use uuid::Uuid;

use crate::Owner;
use crate::personality::types::WakeInvocationStatus;

use super::personality::PersonalityInstanceId;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WakeInvocationStart {
    pub owner: Owner,
    pub personality_instance_id: PersonalityInstanceId,
    pub wake_entry_id: Uuid,
    pub change_event_seq: Uuid,
    pub wake_token: Uuid,
    pub recipe_sha256: String,
    pub resolved_inference_target_ref: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WakeInvocationFinalize {
    pub owner: Owner,
    pub personality_instance_id: PersonalityInstanceId,
    pub wake_entry_id: Uuid,
    pub change_event_seq: Uuid,
    pub status: WakeInvocationStatus,
    pub turn_count: Option<u16>,
    pub cost_usd: Option<f64>,
    pub failure_reason: Option<String>,
    pub exit_code: Option<i32>,
    pub duration_ms: Option<u64>,
    pub stdout_tail: Option<String>,
    pub stderr_tail: Option<String>,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WakeInvocationLogDraft {
    pub owner: Owner,
    pub personality_instance_id: PersonalityInstanceId,
    pub wake_entry_id: Uuid,
    pub change_event_seq: Uuid,
    pub phase: String,
    pub tool_id: Option<String>,
    pub status: String,
    pub duration_ms: Option<u64>,
    pub message_tail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WakeInvocationLogRow {
    pub log_seq: i64,
    pub at: OffsetDateTime,
    pub phase: String,
    pub tool_id: Option<String>,
    pub status: String,
    pub duration_ms: Option<u64>,
    pub message_tail: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WakeInvocationRow {
    pub owner: Owner,
    pub personality_instance_id: PersonalityInstanceId,
    pub wake_entry_id: Uuid,
    pub wake_entry_label: String,
    pub change_event_seq: Uuid,
    pub status: WakeInvocationStatus,
    pub started_at: OffsetDateTime,
    pub finished_at: Option<OffsetDateTime>,
    pub turn_count: u16,
    pub cost_usd: f64,
    pub recipe_sha256: Option<String>,
    pub resolved_inference_target_ref: Option<String>,
    pub failure_reason: Option<String>,
    pub exit_code: Option<i32>,
    pub duration_ms: Option<u64>,
    pub stdout_tail: Option<String>,
    pub stderr_tail: Option<String>,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub logs: Vec<WakeInvocationLogRow>,
}
