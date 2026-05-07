//! Proto <-> core conversions for `InferenceTarget` / `WakeEntry` / Tier.

use proxima_core::{
    InferenceTargetConfig, InferenceTargetRow, InferenceTierBindingRow,
    LocalCliConfig as CoreLocalCli, ModelTier, RemoteModelConfig as CoreRemoteModel,
    WakeEntryAuthoredBy, WakeEntryDraft, WakeEntryExecutionMode, WakeEntryRow,
    WakeEntryTriggerKind, WakeExecutionMode,
};
use tonic::Status;

use crate::pb;

use super::primitives::timestamp_to_proto;

pub fn tier_from_proto(tier: i32) -> Result<ModelTier, Status> {
    match pb::ModelTierProto::try_from(tier).unwrap_or(pb::ModelTierProto::Unspecified) {
        pb::ModelTierProto::Fast => Ok(ModelTier::Fast),
        pb::ModelTierProto::Standard => Ok(ModelTier::Standard),
        pb::ModelTierProto::Deep => Ok(ModelTier::Deep),
        pb::ModelTierProto::Unspecified => Err(Status::invalid_argument("model_tier must be set")),
    }
}

pub fn tier_to_proto(tier: ModelTier) -> i32 {
    match tier {
        ModelTier::Fast => pb::ModelTierProto::Fast as i32,
        ModelTier::Standard => pb::ModelTierProto::Standard as i32,
        ModelTier::Deep => pb::ModelTierProto::Deep as i32,
    }
}

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

pub fn execution_mode_from_proto(mode: i32) -> Result<WakeExecutionMode, Status> {
    match pb::ExecutionMode::try_from(mode).unwrap_or(pb::ExecutionMode::Unspecified) {
        pb::ExecutionMode::SubstrateOnly => Ok(WakeExecutionMode::SubstrateOnly),
        pb::ExecutionMode::Workspace => Ok(WakeExecutionMode::Workspace),
        pb::ExecutionMode::Unspecified => {
            Err(Status::invalid_argument("execution_mode must be set"))
        }
    }
}

pub fn execution_mode_to_proto(mode: WakeEntryExecutionMode) -> i32 {
    match mode {
        WakeEntryExecutionMode::SubstrateOnly => pb::ExecutionMode::SubstrateOnly as i32,
        WakeEntryExecutionMode::Workspace => pb::ExecutionMode::Workspace as i32,
    }
}

pub fn wake_entry_to_proto(row: &WakeEntryRow) -> pb::WakeEntry {
    pb::WakeEntry {
        wake_entry_id: row.wake_entry_id.to_string(),
        trigger_kind: trigger_kind_to_proto(row.trigger_kind),
        trigger_id: row.trigger_id.clone(),
        label: row.label.clone(),
        enabled: row.enabled,
        execution_mode: execution_mode_to_proto(row.execution_mode),
        authored_by: authored_by_to_proto(row.authored_by),
        probability_promille: u32::from(row.probability_promille),
        recipe_ref: row.recipe_ref.clone(),
        model_tier: tier_to_proto(row.model_tier),
        inference_target_ref: row.inference_target_ref.clone(),
        substrate_tool_palette: row.substrate_tool_palette.clone(),
        workspace_tool_palette: row.workspace_tool_palette.clone(),
        max_rounds: u32::from(row.max_rounds),
        disabled_reason: row.disabled_reason.clone(),
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
        execution_mode: execution_mode_from_proto(proto.execution_mode)?,
        authored_by: authored_by_from_proto(proto.authored_by)?,
        probability_promille: u16::try_from(proto.probability_promille)
            .map_err(|_| Status::invalid_argument("probability_promille > u16::MAX"))?,
        recipe_ref: proto.recipe_ref,
        model_tier: tier_from_proto(proto.model_tier)?,
        inference_target_ref: proto.inference_target_ref,
        substrate_tool_palette: proto.substrate_tool_palette,
        workspace_tool_palette: proto.workspace_tool_palette,
        max_rounds: u16::try_from(proto.max_rounds)
            .map_err(|_| Status::invalid_argument("max_rounds > u16::MAX"))?,
    })
}

pub fn inference_target_to_proto(row: &InferenceTargetRow) -> pb::InferenceTarget {
    pb::InferenceTarget {
        target_ref: row.target_ref.clone(),
        config: Some(inference_config_to_proto(&row.config)),
        created_at: Some(timestamp_to_proto(row.created_at)),
        updated_at: Some(timestamp_to_proto(row.updated_at)),
    }
}

pub fn inference_config_to_proto(config: &InferenceTargetConfig) -> pb::InferenceTargetConfig {
    use pb::inference_target_config::Kind;
    let kind = match config {
        InferenceTargetConfig::LocalCli(local) => Kind::LocalCli(pb::LocalCliConfig {
            command: local.command.clone(),
            profile: local.profile.clone(),
            env_overrides: local
                .env_overrides
                .iter()
                .map(|(key, value)| pb::EnvOverride {
                    key: key.clone(),
                    value: value.clone(),
                })
                .collect(),
        }),
        InferenceTargetConfig::RemoteModel(remote) => Kind::RemoteModel(pb::RemoteModelConfig {
            vendor: remote.vendor.clone(),
            dialect: remote.dialect.clone(),
            model_id: remote.model_id.clone(),
            credentials_ref: remote.credentials_ref.clone(),
        }),
    };
    pb::InferenceTargetConfig { kind: Some(kind) }
}

pub fn inference_config_from_proto(
    proto: pb::InferenceTargetConfig,
) -> Result<InferenceTargetConfig, Status> {
    use pb::inference_target_config::Kind;
    match proto.kind {
        Some(Kind::LocalCli(local)) => Ok(InferenceTargetConfig::LocalCli(CoreLocalCli {
            command: local.command,
            profile: local.profile,
            env_overrides: local
                .env_overrides
                .into_iter()
                .map(|e| (e.key, e.value))
                .collect(),
        })),
        Some(Kind::RemoteModel(remote)) => {
            Ok(InferenceTargetConfig::RemoteModel(CoreRemoteModel {
                vendor: remote.vendor,
                dialect: remote.dialect,
                model_id: remote.model_id,
                credentials_ref: remote.credentials_ref,
            }))
        }
        None => Err(Status::invalid_argument(
            "InferenceTargetConfig.kind must be set",
        )),
    }
}

pub fn tier_binding_to_proto(row: &InferenceTierBindingRow) -> pb::InferenceTierBinding {
    pb::InferenceTierBinding {
        tier: tier_to_proto(row.tier),
        target_ref: row.target_ref.clone(),
    }
}
