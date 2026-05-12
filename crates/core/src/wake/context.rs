//! Wake context assembly — the four fixed parameters every wake
//! receives (spec docs/superpowers/specs/2026-05-07 lines 285–306).
//!
//! No per-entry configuration, no DSL: every WakeEntry on every
//! Personality gets the same four envelopes. The dispatcher attaches
//! them to `HarnessProgram::context_params`; wake-entry instructions
//! reference whichever are relevant for the prompt.

use serde::Serialize;
use uuid::Uuid;

use crate::error::ProtocolError;
use crate::outbox::{ChangeEventKind, EntityRef};
use crate::personality::{PersonalityInstanceId, SidecarSpec};
use crate::storage::Storage;
use crate::{MemoryId, Owner};

/// The four fixed envelopes the dispatcher attaches to
/// `HarnessProgram::context_params` for every wake.
#[derive(Debug, Clone, Serialize)]
pub struct WakeContext {
    pub root_perspective: RootPerspectiveEnvelope,
    pub active_goals: Vec<ActiveGoalEnvelope>,
    pub trigger_event: TriggerEventEnvelope,
    pub triggering_memory: TriggeringMemoryEnvelope,
}

/// Spec line 291: `{ instance_id, memory_id, display_name, purpose,
/// system_prompt }`. Read fresh per wake from the personality runtime
/// row so identity edits land on the next wake.
#[derive(Debug, Clone, Serialize)]
pub struct RootPerspectiveEnvelope {
    pub instance_id: Uuid,
    pub memory_id: Uuid,
    pub display_name: String,
    pub purpose: String,
    pub system_prompt: String,
}

/// Spec line 292: `{ goal_payload, motivation_via }`.
///
/// `goal_payload` is the full typed sidecar payload of the active Goal
/// (whatever shape the flavor authored). `motivation_via` is the list of
/// memory ids whose `core/inspires` edges trace from the goal head row to
/// the active personality. With the v1 schema that's a direct
/// Goal -> Perspective edge, so the path goes through the root
/// perspective memory id.
#[derive(Debug, Clone, Serialize)]
pub struct ActiveGoalEnvelope {
    pub goal_id: Uuid,
    pub schema_id: String,
    pub title: String,
    pub goal_payload: serde_json::Value,
    pub motivation_via: Vec<Uuid>,
}

/// Spec line 293: ChangeEvent envelope (kind, sequence, schema_id,
/// owner, author, wake_chain_depth).
///
/// `change_event_seq` is `Uuid` (Phase 1a schema), not `i64`.
#[derive(Debug, Clone, Serialize)]
pub struct TriggerEventEnvelope {
    pub kind: String,
    pub change_event_seq: Uuid,
    pub schema_id: String,
    pub owner: Owner,
    pub author: Uuid,
    pub wake_chain_depth: i32,
}

/// Spec line 294: the memory row the ChangeEvent points at, with its
/// typed sidecar payload resolved.
#[derive(Debug, Clone, Serialize)]
pub struct TriggeringMemoryEnvelope {
    pub memory_id: Uuid,
    pub schema_id: String,
    pub schema_version: i32,
    pub typed_payload: serde_json::Value,
}

/// Read the four fixed wake parameters from storage.
///
/// The query order matters: the personality runtime row is read first so
/// the assembler knows which root-perspective memory to resolve. The
/// triggering memory is then resolved by following the change event's
/// entity / edge target.
///
/// `sidecars` is the registered sidecar list — `load_memory_by_id`
/// matches by `schema_id` to populate the triggering memory's
/// `typed_payload`. The dispatcher passes the engine's frozen
/// registry sidecars; tests can pass whatever sidecars the fixture
/// installs.
///
/// # Errors
///
/// Returns `ProtocolError::NotFound` when the personality, root
/// perspective sidecar, change event, or triggering memory row is
/// absent. Returns `ProtocolError::Internal` for storage failures.
pub async fn assemble_wake_context(
    storage: &dyn Storage,
    owner: &Owner,
    personality_instance_id: PersonalityInstanceId,
    change_event_seq: Uuid,
    sidecars: &[SidecarSpec],
) -> Result<WakeContext, ProtocolError> {
    let runtime = storage
        .fetch_personality_runtime(owner, personality_instance_id)
        .await
        .map_err(|e| ProtocolError::internal(format!("fetch_personality_runtime: {e}")))?
        .ok_or_else(|| {
            ProtocolError::not_found(format!(
                "personality runtime not found: instance_id={}",
                personality_instance_id.into_inner()
            ))
        })?;

    let root_memory_id = runtime.current_root_perspective_memory_id;

    let root_payload = storage
        .fetch_root_personality_perspective(owner, root_memory_id)
        .await
        .map_err(|e| ProtocolError::internal(format!("fetch_root_personality_perspective: {e}")))?
        .ok_or_else(|| {
            ProtocolError::not_found(format!(
                "root personality perspective sidecar missing: memory_id={}",
                root_memory_id.into_inner()
            ))
        })?;

    // v1 system prompt is the Root Perspective's `purpose` — the
    // dedicated `system_prompt` column was dropped in migration
    // 20260507000050; wake entries layer per-trigger context on top via
    // `HarnessProgram::context_params`. Falls back to display_name if
    // purpose is empty so the assertion `system_prompt.len() > 0`
    // always holds for a well-formed personality.
    let system_prompt = if root_payload.purpose.trim().is_empty() {
        root_payload.display_name.clone()
    } else {
        root_payload.purpose.clone()
    };

    let root_perspective = RootPerspectiveEnvelope {
        instance_id: personality_instance_id.into_inner(),
        memory_id: root_memory_id.into_inner(),
        display_name: root_payload.display_name,
        purpose: root_payload.purpose,
        system_prompt,
    };

    // Active Goals attached to this Root Perspective via core/inspires.
    // The list_active_goals query already filters supersession + state
    // forward to current Active heads. motivation_via is the path back
    // to the personality, which (with the v1 direct-edge schema) is
    // simply the root perspective memory id.
    let active_goal_rows = storage
        .list_active_goals(owner, root_memory_id, 100)
        .await
        .map_err(|e| ProtocolError::internal(format!("list_active_goals: {e}")))?;

    let active_goals = active_goal_rows
        .into_iter()
        .map(|row| {
            let goal_payload = if row.payload.is_empty() {
                serde_json::Value::Null
            } else {
                serde_json::from_slice::<serde_json::Value>(&row.payload).unwrap_or_else(|_| {
                    serde_json::Value::String(String::from_utf8_lossy(&row.payload).into_owned())
                })
            };
            ActiveGoalEnvelope {
                goal_id: row.goal_id.into_inner(),
                schema_id: row.schema_id.into_inner(),
                title: row.title,
                goal_payload,
                motivation_via: vec![root_memory_id.into_inner()],
            }
        })
        .collect();

    let change_event = storage
        .fetch_change_event_for_wake(owner, change_event_seq)
        .await
        .map_err(|e| ProtocolError::internal(format!("fetch_change_event_for_wake: {e}")))?
        .ok_or_else(|| {
            ProtocolError::not_found(format!("change event not found: seq={change_event_seq}"))
        })?;

    let (kind_str, schema_id_str, triggering_memory_id) =
        derive_event_descriptor(&change_event.event.kind)?;

    let author = change_event
        .event
        .authoring_personality_instance_id
        .unwrap_or_else(Uuid::nil);
    let wake_chain_depth = i32::from(change_event.event.wake_chain_depth);

    let trigger_event = TriggerEventEnvelope {
        kind: kind_str,
        change_event_seq,
        schema_id: schema_id_str,
        owner: owner.clone(),
        author,
        wake_chain_depth,
    };

    let memory_snapshot = storage
        .load_memory_by_id(owner, triggering_memory_id, sidecars)
        .await
        .map_err(|e| ProtocolError::internal(format!("load_memory_by_id: {e}")))?
        .ok_or_else(|| {
            ProtocolError::not_found(format!(
                "triggering memory not found: memory_id={}",
                triggering_memory_id.into_inner()
            ))
        })?;

    let triggering_memory = TriggeringMemoryEnvelope {
        memory_id: memory_snapshot.memory_id.into_inner(),
        schema_id: memory_snapshot.schema_id.into_inner(),
        schema_version: i32::try_from(memory_snapshot.schema_version.into_inner()).unwrap_or(1),
        typed_payload: memory_snapshot.payload_json,
    };

    Ok(WakeContext {
        root_perspective,
        active_goals,
        trigger_event,
        triggering_memory,
    })
}

/// Resolve a `ChangeEventKind` into the `(kind, schema_id, memory_id)`
/// triple the trigger envelope and triggering-memory lookup need.
///
/// For `EntityAppend` of a memory, the memory id is the entity. For
/// `EntityAppend` of a Goal, no memory is associated; wake entries
/// that trigger on Goal-only events would need a different
/// `triggering_memory` strategy, so we return `NotFound` for now.
///
/// For `EdgeAppend`, the triggering memory is the edge's target when it
/// is a memory, otherwise the source. Edges with no memory endpoint
/// (Goal -> Goal) return `NotFound`.
fn derive_event_descriptor(
    kind: &ChangeEventKind,
) -> Result<(String, String, MemoryId), ProtocolError> {
    match kind {
        ChangeEventKind::EntityAppend {
            entity, schema_id, ..
        } => match entity {
            EntityRef::Memory(memory_id) => Ok((
                "EntityAppend".to_string(),
                schema_id.as_str().to_string(),
                *memory_id,
            )),
            EntityRef::Goal(_) => Err(ProtocolError::not_found(
                "trigger event references a Goal entity; no triggering memory available",
            )),
        },
        ChangeEventKind::EdgeAppend {
            relation,
            source,
            target,
            ..
        } => {
            let memory_id = match (target, source) {
                (EntityRef::Memory(m), _) => *m,
                (_, EntityRef::Memory(m)) => *m,
                (EntityRef::Goal(_), EntityRef::Goal(_)) => {
                    return Err(ProtocolError::not_found(
                        "trigger edge has no memory endpoint",
                    ));
                }
            };
            Ok(("EdgeAppend".to_string(), relation.clone(), memory_id))
        }
    }
}
