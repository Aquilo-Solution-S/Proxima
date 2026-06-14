//! Proto <-> core conversions for `WakeEntry`.

use proxima_core::{
    WakeEntryAuthoredBy, WakeEntryDraft, WakeEntryGoalScope, WakeEntryRow, WakeEntryTriggerKind,
};
use tonic::Status;

use crate::pb;

pub fn trigger_kind_from_proto(kind: i32) -> Result<WakeEntryTriggerKind, Status> {
    match pb::TriggerKind::try_from(kind).unwrap_or(pb::TriggerKind::Unspecified) {
        pb::TriggerKind::OnMemory => Ok(WakeEntryTriggerKind::OnMemory),
        pb::TriggerKind::OnEdge => Ok(WakeEntryTriggerKind::OnEdge),
        pb::TriggerKind::Unspecified => Err(Status::invalid_argument("trigger_kind must be set")),
    }
}

pub fn trigger_kind_to_proto(kind: WakeEntryTriggerKind) -> i32 {
    match kind {
        WakeEntryTriggerKind::OnMemory => pb::TriggerKind::OnMemory as i32,
        WakeEntryTriggerKind::OnEdge => pb::TriggerKind::OnEdge as i32,
    }
}

pub fn authored_by_from_proto(authored_by: i32) -> Result<WakeEntryAuthoredBy, Status> {
    match pb::AuthoredBy::try_from(authored_by).unwrap_or(pb::AuthoredBy::Unspecified) {
        pb::AuthoredBy::Any => Ok(WakeEntryAuthoredBy::Any),
        pb::AuthoredBy::Self_ => Ok(WakeEntryAuthoredBy::SelfAuthor),
        pb::AuthoredBy::Other => Ok(WakeEntryAuthoredBy::Other),
        pb::AuthoredBy::Unspecified => Err(Status::invalid_argument("authored_by must be set")),
    }
}

pub fn authored_by_to_proto(authored_by: WakeEntryAuthoredBy) -> i32 {
    match authored_by {
        WakeEntryAuthoredBy::Any => pb::AuthoredBy::Any as i32,
        WakeEntryAuthoredBy::SelfAuthor => pb::AuthoredBy::Self_ as i32,
        WakeEntryAuthoredBy::Other => pb::AuthoredBy::Other as i32,
    }
}

pub fn goal_scope_from_proto(scope: i32) -> Result<WakeEntryGoalScope, Status> {
    match pb::GoalScope::try_from(scope).unwrap_or(pb::GoalScope::Unspecified) {
        pb::GoalScope::None | pb::GoalScope::Unspecified => Ok(WakeEntryGoalScope::None),
        pb::GoalScope::TriggerGoalAssigned => Ok(WakeEntryGoalScope::TriggerGoalAssigned),
    }
}

pub fn goal_scope_to_proto(scope: WakeEntryGoalScope) -> i32 {
    match scope {
        WakeEntryGoalScope::None => pb::GoalScope::None as i32,
        WakeEntryGoalScope::TriggerGoalAssigned => pb::GoalScope::TriggerGoalAssigned as i32,
    }
}

pub fn wake_entry_to_proto(row: &WakeEntryRow) -> pb::WakeEntry {
    pb::WakeEntry {
        wake_entry_id: row.wake_entry_id.to_string(),
        trigger_kind: trigger_kind_to_proto(row.trigger_kind),
        trigger_id: row.trigger_id.clone(),
        label: row.label.clone(),
        enabled: row.enabled,
        authored_by: authored_by_to_proto(row.authored_by),
        probability_promille: u32::from(row.probability_promille),
        disabled_reason: row.disabled_reason.clone(),
        goal_scope: goal_scope_to_proto(row.goal_scope),
    }
}

pub fn wake_entry_draft_from_proto(
    proto: pb::WakeEntryDraft,
    personality_instance_id: proxima_core::PersonalityInstanceId,
) -> Result<WakeEntryDraft, Status> {
    Ok(WakeEntryDraft {
        wake_entry_id: uuid::Uuid::now_v7(),
        personality_instance_id,
        trigger_kind: trigger_kind_from_proto(proto.trigger_kind)?,
        trigger_id: proto.trigger_id,
        label: proto.label,
        enabled: proto.enabled,
        authored_by: authored_by_from_proto(proto.authored_by)?,
        probability_promille: u16::try_from(proto.probability_promille)
            .map_err(|_| Status::invalid_argument("probability_promille > u16::MAX"))?,
        goal_scope: goal_scope_from_proto(proto.goal_scope)?,
        instructions: String::new(),
    })
}
