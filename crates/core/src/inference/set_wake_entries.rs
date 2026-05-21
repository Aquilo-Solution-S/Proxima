//! WakeEntry write-time validation pipeline.

use std::collections::HashSet;

use crate::error::ProtocolError;
use crate::personality::{parse_scoped_emit_tool_id, substrate_pack, workspace_tool_ids};
use crate::storage::{Storage, StorageError};
use crate::{
    FlavorRegistryFrozen, ModelTier, SchemaId, SchemaVersion, SetWakeEntriesRequest,
    SetWakeEntriesResponse, WakeEntryDraft, WakeEntryTriggerKind, WakeExecutionMode,
};

pub struct SetWakeEntriesContext<'a> {
    pub storage: &'a dyn Storage,
    pub registry: &'a FlavorRegistryFrozen,
}

pub async fn set_wake_entries(
    ctx: &SetWakeEntriesContext<'_>,
    req: &SetWakeEntriesRequest,
) -> Result<SetWakeEntriesResponse, ProtocolError> {
    validate_unique_triggers(&req.entries)?;
    for entry in &req.entries {
        validate_entry_shape(entry)?;
    }

    let mut substrate_registered = ctx.registry.mcp_tool_ids();
    substrate_registered.extend(
        substrate_pack()
            .iter()
            .map(|tool| tool.tool_id().to_string()),
    );
    let workspace_registered = workspace_tool_ids();
    for entry in &req.entries {
        validate_palettes(
            ctx.registry,
            entry,
            &substrate_registered,
            &workspace_registered,
        )?;
        validate_workspace_trigger(ctx.registry, entry)?;
    }

    let owner_targets = ctx
        .storage
        .list_inference_targets(&req.owner)
        .await
        .map_err(|err| ProtocolError::internal(err.to_string()))?;
    let owner_target_refs: HashSet<&str> = owner_targets
        .iter()
        .map(|target| target.target_ref.as_str())
        .collect();
    let owner_tier_bindings = ctx
        .storage
        .list_inference_tier_bindings(&req.owner)
        .await
        .map_err(|err| ProtocolError::internal(err.to_string()))?;
    let bound_tiers: HashSet<ModelTier> = owner_tier_bindings
        .iter()
        .map(|binding| binding.tier)
        .collect();

    for entry in &req.entries {
        validate_target_or_tier(entry, &owner_target_refs, &bound_tiers)?;
    }

    ctx.storage
        .set_wake_entries(req)
        .await
        .map_err(|err| map_set_wake_entries_storage_err(err, &req.entries))
}

fn validate_workspace_trigger(
    registry: &FlavorRegistryFrozen,
    entry: &WakeEntryDraft,
) -> Result<(), ProtocolError> {
    if entry.execution_mode != WakeExecutionMode::Workspace {
        if entry.workspace_binding.is_some() {
            return Err(ProtocolError::invalid_argument(
                "workspace_binding",
                "workspace_binding requires execution_mode = workspace",
            ));
        }
        return Ok(());
    }
    if entry.trigger_kind != WakeEntryTriggerKind::OnMemory {
        return Err(ProtocolError::invalid_argument(
            "execution_mode",
            "workspace mode requires an on_memory trigger",
        ));
    }
    if entry.workspace_binding.is_some() {
        return Ok(());
    }
    if !registry.is_workspace_trigger(&entry.trigger_id) {
        return Err(ProtocolError::invalid_argument(
            "trigger_id",
            format!("not workspace-eligible: {}", entry.trigger_id),
        ));
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

fn validate_palettes(
    registry: &FlavorRegistryFrozen,
    entry: &WakeEntryDraft,
    substrate_registered: &HashSet<String>,
    workspace_registered: &HashSet<String>,
) -> Result<(), ProtocolError> {
    for tool_id in &entry.substrate_tool_palette {
        if !substrate_tool_registered(tool_id, substrate_registered, registry) {
            return Err(ProtocolError::tool_not_registered(tool_id));
        }
    }
    for tool_id in &entry.workspace_tool_palette {
        if !workspace_registered.contains(tool_id) {
            return Err(ProtocolError::tool_not_registered(tool_id));
        }
    }
    Ok(())
}

fn substrate_tool_registered(
    tool_id: &str,
    substrate_registered: &HashSet<String>,
    registry: &FlavorRegistryFrozen,
) -> bool {
    if substrate_registered.contains(tool_id) {
        return true;
    }
    let Ok(Some(scoped)) = parse_scoped_emit_tool_id(tool_id) else {
        return false;
    };
    substrate_registered.contains(scoped.base_tool_id)
        && registry
            .lookup(
                &SchemaId::new(scoped.schema_id),
                SchemaVersion::new(scoped.schema_version),
            )
            .is_some_and(|schema| schema.kind == scoped.kind)
}

fn validate_target_or_tier(
    entry: &WakeEntryDraft,
    owner_target_refs: &HashSet<&str>,
    bound_tiers: &HashSet<ModelTier>,
) -> Result<(), ProtocolError> {
    if let Some(target_ref) = &entry.inference_target_ref {
        if !owner_target_refs.contains(target_ref.as_str()) {
            return Err(ProtocolError::inference_target_missing(target_ref));
        }
    } else if !bound_tiers.contains(&entry.model_tier) {
        return Err(ProtocolError::tier_unbound(model_tier_str(
            entry.model_tier,
        )));
    }
    Ok(())
}

fn map_set_wake_entries_storage_err(
    err: StorageError,
    entries: &[WakeEntryDraft],
) -> ProtocolError {
    match err {
        StorageError::NotFound => ProtocolError::not_found("personality instance not found"),
        StorageError::ConstraintViolation(msg)
            if msg.contains("personality_wake_entries_active_trigger_uq") =>
        {
            let first = entries.first();
            ProtocolError::trigger_conflict(
                first
                    .map(|entry| entry.trigger_kind.as_str())
                    .unwrap_or("unknown"),
                first
                    .map(|entry| entry.trigger_id.as_str())
                    .unwrap_or("unknown"),
            )
        }
        other => ProtocolError::internal(other.to_string()),
    }
}

fn model_tier_str(tier: ModelTier) -> &'static str {
    match tier {
        ModelTier::Fast => "fast",
        ModelTier::Standard => "standard",
        ModelTier::Deep => "deep",
    }
}
