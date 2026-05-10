//! WakeEntry write-time validation pipeline.

use std::collections::HashSet;
use std::path::PathBuf;

use crate::error::ProtocolError;
use crate::inference::recipe_resolve::resolve_recipe_ref;
use crate::inference::recipe_validate::{RecipeValidateError, validate_recipe as goose_validate};
use crate::personality::{substrate_pack, workspace_tool_ids};
use crate::storage::{Storage, StorageError};
use crate::{
    FlavorRegistryFrozen, ModelTier, SetWakeEntriesRequest, SetWakeEntriesResponse, WakeEntryDraft,
    WakeEntryTriggerKind, WakeExecutionMode,
};

pub struct SetWakeEntriesContext<'a> {
    pub storage: &'a dyn Storage,
    pub registry: &'a FlavorRegistryFrozen,
    pub owner_recipes_root: PathBuf,
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
        validate_palettes(entry, &substrate_registered, &workspace_registered)?;
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
        validate_recipe_ref(ctx, entry).await?;
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
        return Ok(());
    }
    if entry.trigger_kind != WakeEntryTriggerKind::OnMemory {
        return Err(ProtocolError::invalid_argument(
            "execution_mode",
            "workspace mode requires an on_memory trigger",
        ));
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
    if entry.recipe_ref.trim().is_empty() {
        return Err(ProtocolError::invalid_argument(
            "recipe_ref",
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
    if entry.max_rounds == 0 {
        return Err(ProtocolError::invalid_argument(
            "max_rounds",
            "must be greater than 0",
        ));
    }
    Ok(())
}

fn validate_palettes(
    entry: &WakeEntryDraft,
    substrate_registered: &HashSet<String>,
    workspace_registered: &HashSet<String>,
) -> Result<(), ProtocolError> {
    for tool_id in &entry.substrate_tool_palette {
        if !substrate_registered.contains(tool_id) {
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

async fn validate_recipe_ref(
    ctx: &SetWakeEntriesContext<'_>,
    entry: &WakeEntryDraft,
) -> Result<(), ProtocolError> {
    let path = resolve_recipe_ref(&entry.recipe_ref, &ctx.owner_recipes_root, ctx.registry)
        .map_err(|_| ProtocolError::recipe_not_found(&entry.recipe_ref))?;

    match goose_validate(&path).await {
        Ok(()) => Ok(()),
        Err(RecipeValidateError::Unavailable) => Err(ProtocolError::goose_cli_unavailable()),
        Err(RecipeValidateError::Invalid { stderr }) => Err(ProtocolError::recipe_invalid(stderr)),
        Err(RecipeValidateError::Timeout(_) | RecipeValidateError::Io(_)) => Err(
            ProtocolError::internal("recipe-validate subprocess failed; check engine logs"),
        ),
    }
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
