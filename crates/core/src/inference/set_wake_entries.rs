//! `WakeEntry` write-time validation pipeline.

use std::collections::HashSet;

use crate::error::ProtocolError;
use crate::personality::{broad_emit_kind, parse_scoped_emit_tool_id};
use crate::storage::{Storage, StorageError};
use crate::{
    FlavorRegistryFrozen, ModelTier, SchemaId, SchemaVersion, SetWakeEntriesRequest,
    SetWakeEntriesResponse, WakeEntryDraft, WakeEntryTriggerKind,
};

pub struct SetWakeEntriesContext<'a> {
    pub storage: &'a dyn Storage,
    pub registry: &'a FlavorRegistryFrozen,
}

impl std::fmt::Debug for SetWakeEntriesContext<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SetWakeEntriesContext")
            .finish_non_exhaustive()
    }
}

/// Validate and persist a personality's wake-entry set.
///
/// # Errors
///
/// Returns `ProtocolError::InvalidArgument` / `DuplicateTriggerInRequest` /
/// `ToolNotRegistered` for malformed entries, `InferenceTargetMissing` or
/// `TierUnbound` when the target/tier is unresolved, `NotFound` when the
/// personality instance is absent, `TriggerConflict` on the active-trigger
/// uniqueness constraint, and `Internal` for other storage failures.
pub async fn set_wake_entries(
    ctx: &SetWakeEntriesContext<'_>,
    req: &SetWakeEntriesRequest,
) -> Result<SetWakeEntriesResponse, ProtocolError> {
    validate_wake_entries_static_config(ctx.registry, &req.entries)?;

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

/// Run the storage-independent validations over a wake-entry set.
///
/// # Errors
///
/// Returns `ProtocolError::DuplicateTriggerInRequest` for repeated
/// trigger pairs, `InvalidArgument` for malformed entry fields or
/// unproducible `required_produced_schema_ids`, and `ToolNotRegistered`
/// for unknown palette tool ids.
pub fn validate_wake_entries_static_config(
    registry: &FlavorRegistryFrozen,
    entries: &[WakeEntryDraft],
) -> Result<(), ProtocolError> {
    validate_unique_triggers(entries)?;
    for entry in entries {
        validate_entry_shape(entry)?;
    }
    let substrate_registered = registry.mcp_tool_ids();
    for entry in entries {
        validate_palettes(registry, entry, &substrate_registered)?;
        validate_required_produced_schemas(registry, entry)?;
    }
    Ok(())
}

fn validate_required_produced_schemas(
    registry: &FlavorRegistryFrozen,
    entry: &WakeEntryDraft,
) -> Result<(), ProtocolError> {
    if entry.required_produced_schema_ids.is_empty() {
        return Ok(());
    }
    let produced = produced_schema_ids_for_palette(registry, &entry.substrate_tool_palette);
    for schema_id in &entry.required_produced_schema_ids {
        if schema_id.trim().is_empty() {
            return Err(ProtocolError::invalid_argument(
                "required_produced_schema_ids",
                "schema ids must be non-empty",
            ));
        }
        if !produced.contains(schema_id) {
            return Err(ProtocolError::invalid_argument(
                "required_produced_schema_ids",
                format!("schema id {schema_id:?} is not produced by this substrate_tool_palette"),
            ));
        }
    }
    Ok(())
}

fn produced_schema_ids_for_palette(
    registry: &FlavorRegistryFrozen,
    palette: &[String],
) -> HashSet<String> {
    let mut schema_ids = HashSet::new();
    for palette_id in palette {
        if let Some(kind) = broad_emit_kind(palette_id) {
            schema_ids.extend(
                registry
                    .list()
                    .into_iter()
                    .filter(|schema| schema.kind == kind)
                    .map(|schema| schema.schema_id.into_inner()),
            );
            continue;
        }
        if let Ok(Some(scoped)) = parse_scoped_emit_tool_id(palette_id)
            && registry
                .lookup_payload(
                    &SchemaId::new(scoped.schema_id.clone()),
                    SchemaVersion::new(scoped.schema_version),
                    scoped.kind,
                )
                .is_some()
        {
            schema_ids.insert(scoped.schema_id);
        }
    }
    for tool in registry.list_mcp_tools() {
        if palette.iter().any(|id| id == tool.name) {
            schema_ids.extend(tool.produces_schema_ids.iter().map(|id| (*id).to_string()));
        }
    }
    schema_ids
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
) -> Result<(), ProtocolError> {
    for tool_id in &entry.substrate_tool_palette {
        if !substrate_tool_registered(tool_id, substrate_registered, registry) {
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
            .lookup_payload(
                &SchemaId::new(scoped.schema_id),
                SchemaVersion::new(scoped.schema_version),
                scoped.kind,
            )
            .is_some()
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
                first.map_or("unknown", |entry| entry.trigger_kind.as_str()),
                first.map_or("unknown", |entry| entry.trigger_id.as_str()),
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
