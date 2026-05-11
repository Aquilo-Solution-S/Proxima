use proxima_core::personality::{
    WakeEntryAuthoredBy, WakeEntryExecutionMode, WakeEntryGoalScope, WakeEntryTriggerKind,
    WakeExecutionMode, WakeInvocationStatus,
};
use proxima_core::{ModelTier, Owner, Principal};

pub(super) fn owner_from_parts(kind: &str, principal_id: uuid::Uuid, org_id: uuid::Uuid) -> Owner {
    Owner {
        principal: match kind {
            "User" => Principal::User(proxima_core::UserId::new(principal_id)),
            _ => Principal::Group(proxima_core::GroupId::new(principal_id)),
        },
        org_id: proxima_core::OrgId::new(org_id),
    }
}

pub(super) fn parse_trigger_kind(value: &str) -> WakeEntryTriggerKind {
    match value {
        "on_edge" => WakeEntryTriggerKind::OnEdge,
        _ => WakeEntryTriggerKind::OnMemory,
    }
}

pub(super) fn parse_execution_mode(value: &str) -> WakeExecutionMode {
    match value {
        "workspace" => WakeExecutionMode::Workspace,
        _ => WakeExecutionMode::SubstrateOnly,
    }
}

pub(super) fn parse_row_execution_mode(value: &str) -> WakeEntryExecutionMode {
    match value {
        "workspace" => WakeEntryExecutionMode::Workspace,
        _ => WakeEntryExecutionMode::SubstrateOnly,
    }
}

pub(super) fn parse_goal_scope(value: &str) -> WakeEntryGoalScope {
    match value {
        "trigger_goal_assigned" => WakeEntryGoalScope::TriggerGoalAssigned,
        _ => WakeEntryGoalScope::None,
    }
}

pub(super) fn parse_row_authored_by(value: &str) -> WakeEntryAuthoredBy {
    match value {
        "self" => WakeEntryAuthoredBy::SelfAuthor,
        "other" => WakeEntryAuthoredBy::Other,
        _ => WakeEntryAuthoredBy::Any,
    }
}

pub(super) fn parse_model_tier(value: &str) -> ModelTier {
    match value {
        "fast" => ModelTier::Fast,
        "deep" => ModelTier::Deep,
        _ => ModelTier::Standard,
    }
}

pub(super) fn parse_wake_invocation_status(value: &str) -> WakeInvocationStatus {
    match value {
        "running" => WakeInvocationStatus::Running,
        "truncated" => WakeInvocationStatus::Truncated,
        "failed" => WakeInvocationStatus::Failed,
        _ => WakeInvocationStatus::Succeeded,
    }
}

pub(super) fn model_tier_str(tier: ModelTier) -> &'static str {
    match tier {
        ModelTier::Fast => "fast",
        ModelTier::Standard => "standard",
        ModelTier::Deep => "deep",
    }
}
