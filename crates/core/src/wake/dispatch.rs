//! Phase 1d Task 9: dispatcher tick body.
//!
//! `dispatch_tick` scans the change-event log per active personality,
//! matches each event against the personality's active wake entries, and
//! fires a wake invocation for every match that survives the
//! `authored_by`, self-wake, chain-depth, and probability filters. The
//! invocation row's `(owner, instance, wake_entry, change_event_seq)`
//! primary key makes the fire idempotent: a second tick over the same
//! window writes zero new rows.
//!
//! Cursor advance is per-personality: after processing a window for one
//! personality we bump its `personality_wake_cursor.last_considered_seq`
//! to the highest seq seen, regardless of how many entries fired. The
//! next tick starts from that seq + 1.
//!
//! The probability roll is best-effort pseudo-random — Phase 1a doesn't
//! ship the `rand` crate, and the dispatcher's correctness contract only
//! requires `0` never fires and `1000` always fires. Anything in
//! between converges over many ticks; we extract pseudo-randomness from
//! a fresh v4 UUID so the cost is one syscall per check.

use uuid::Uuid;

use crate::MemoryId;
use crate::Owner;
use crate::engine::Engine;
use crate::error::ProtocolError;
use crate::outbox::{ChangeEventKind, EntityRef};
use crate::personality::{
    ChangeEventForWake, MAX_WAKE_CHAIN_DEPTH, PersonalityInstanceId, WakeDispatchEntryRow,
    WakeEntryAuthoredBy, WakeEntryDraft, WakeEntryExecutionMode, WakeEntryRow, WakeEntryTriggerKind,
    WakeExecutionMode, WakeInvocationFinalize, WakeInvocationStatus,
};
use crate::wake::fire::{FireWakeEntryInput, fire_wake_entry};

/// Per-tick scan limit on change events fetched per (owner, personality).
/// Bounds memory + worst-case round-trip latency; the next tick picks up
/// where this one left off via the cursor.
const CHANGE_EVENT_SCAN_LIMIT: usize = 256;

/// Run one dispatcher tick. Returns the count of wake fires that wrote a
/// fresh invocation row (i.e. `fire_wake_entry` returned `Ok(true)`,
/// minus rows already present from a prior tick).
///
/// # Errors
///
/// Propagates the first storage or fire error encountered. Per-fire
/// failures (target adapter, recipe IO, etc.) come back as `Ok(_)` so
/// the loop continues — only plumbing failures (storage scan, cursor
/// advance, missing target adapter) abort the tick.
pub async fn dispatch_tick(engine: &Engine) -> Result<usize, ProtocolError> {
    let mut fired = 0usize;

    // 1. Scan all active wake entries across all owners + personalities.
    //    `list_active_wake_entries` joins entries to the wake cursor so
    //    each row carries its personality's `last_considered_seq`. We
    //    group by (owner, personality_instance) so we share one event
    //    fetch + cursor advance per group.
    let entries = engine
        .storage()
        .list_active_wake_entries()
        .await
        .map_err(|e| ProtocolError::internal(format!("list_active_wake_entries: {e}")))?;
    if entries.is_empty() {
        return Ok(0);
    }

    let groups = group_by_personality(entries);

    // 2. For each personality, scan its event window and try every
    //    entry against every event.
    for group in groups {
        let events = engine
            .storage()
            .list_change_events_after(
                &group.owner,
                group.last_considered_seq,
                CHANGE_EVENT_SCAN_LIMIT,
            )
            .await
            .map_err(|e| ProtocolError::internal(format!("list_change_events_after: {e}")))?;
        if events.is_empty() {
            continue;
        }

        let mut highest_seq = group.last_considered_seq;
        for event in &events {
            if event.event.seq > highest_seq {
                highest_seq = event.event.seq;
            }
            for entry in &group.entries {
                if !triggers_match(entry, event) {
                    continue;
                }
                if !authored_by_matches(
                    entry.authored_by,
                    event.authoring_personality_instance_id,
                    group.personality_instance_id,
                ) {
                    continue;
                }
                // Defense-in-depth self-wake guard: the authored_by
                // filter above already drops self-authored events when
                // `authored_by != Any`, but `Any` would otherwise let a
                // self-edit walk back into a wake. fire_wake_entry has
                // its own guard for the race; we belt-and-brace here.
                if event.authoring_personality_instance_id
                    == Some(group.personality_instance_id)
                {
                    continue;
                }
                if event.wake_chain_depth.into_inner() >= MAX_WAKE_CHAIN_DEPTH {
                    write_chain_depth_exhausted(engine, &group, entry, event).await?;
                    continue;
                }
                if !probability_roll(entry.probability_promille) {
                    continue;
                }

                // Surface the adapter — late-bound so tests can inject
                // a mock via `set_target_adapter`. Missing adapter is a
                // misconfiguration: the dispatcher loop must not have
                // been started without one.
                let adapter = engine.target_adapter().ok_or_else(|| {
                    ProtocolError::internal(
                        "dispatcher fired before target adapter was installed — \
                         call Engine::start (or with_target_adapter) first",
                    )
                })?;

                let triggering_memory_id = match triggering_memory(event) {
                    Some(m) => m,
                    None => continue,
                };

                let wake_entry_row = wake_entry_draft_to_row(entry);
                let input = FireWakeEntryInput {
                    owner: group.owner.clone(),
                    personality_instance_id: group.personality_instance_id,
                    wake_entry: wake_entry_row,
                    change_event_seq: event.event.seq,
                    triggering_memory_id: triggering_memory_id.into_inner(),
                };

                match fire_wake_entry(engine, adapter.as_ref(), input).await {
                    Ok(true) => fired += 1,
                    Ok(false) => {} // skipped (self-wake race)
                    Err(e) if is_idempotency_conflict(&e) => {
                        // Already fired by an earlier tick; PRIMARY KEY
                        // bounce. Counts as no-op for this tick.
                    }
                    Err(e) => return Err(e),
                }
            }
        }

        // 3. Advance cursor to the highest seq we considered, regardless
        //    of whether anything fired. Correctness: all earlier seqs
        //    are now either acted on (with idempotent invocation row),
        //    filtered out, or a chain-exhausted noop. None of them
        //    should reappear on the next tick.
        if highest_seq != group.last_considered_seq {
            engine
                .storage()
                .advance_wake_cursor(
                    &group.owner,
                    group.personality_instance_id,
                    highest_seq,
                )
                .await
                .map_err(|e| ProtocolError::internal(format!("advance_wake_cursor: {e}")))?;
        }
    }

    Ok(fired)
}

/// One personality's worth of dispatch state — the cursor row joined
/// with all of its active wake entries.
struct PersonalityGroup {
    owner: Owner,
    personality_instance_id: PersonalityInstanceId,
    last_considered_seq: Uuid,
    entries: Vec<WakeEntryDraft>,
}

fn group_by_personality(rows: Vec<WakeDispatchEntryRow>) -> Vec<PersonalityGroup> {
    let mut groups: Vec<PersonalityGroup> = Vec::new();
    for row in rows {
        // Linear scan is fine: dispatch operates on a small number of
        // active personalities per owner. A HashMap would force an
        // owner-equality bound we don't otherwise need.
        let key_owner = row.owner.clone();
        let key_instance = row.personality_instance_id;
        if let Some(existing) = groups
            .iter_mut()
            .find(|g| g.owner == key_owner && g.personality_instance_id == key_instance)
        {
            existing.entries.push(row.wake_entry);
        } else {
            groups.push(PersonalityGroup {
                owner: row.owner,
                personality_instance_id: row.personality_instance_id,
                last_considered_seq: row.last_considered_seq,
                entries: vec![row.wake_entry],
            });
        }
    }
    groups
}

fn triggers_match(entry: &WakeEntryDraft, event: &ChangeEventForWake) -> bool {
    if !entry.enabled {
        return false;
    }
    match (&entry.trigger_kind, &event.event.kind) {
        (
            WakeEntryTriggerKind::OnMemory,
            ChangeEventKind::EntityAppend {
                schema_id, ..
            },
        ) => entry.trigger_id == schema_id.as_str(),
        (
            WakeEntryTriggerKind::OnEdge,
            ChangeEventKind::EdgeAppend { relation, .. },
        ) => entry.trigger_id == relation.as_str(),
        _ => false,
    }
}

fn authored_by_matches(
    filter: WakeEntryAuthoredBy,
    event_author: Option<PersonalityInstanceId>,
    self_instance: PersonalityInstanceId,
) -> bool {
    match filter {
        WakeEntryAuthoredBy::Any => true,
        WakeEntryAuthoredBy::SelfAuthor => event_author == Some(self_instance),
        WakeEntryAuthoredBy::Other => match event_author {
            None => true,                             // external/event-source counts as Other
            Some(author) => author != self_instance,
        },
    }
}

/// Best-effort probability gate. `0` never fires, `1000` always fires;
/// values in between converge over the long run. UUID v4 reads from
/// system random so this is safe enough for v1 — Phase 2 swaps in a
/// proper PRNG when we care about test determinism.
fn probability_roll(promille: u16) -> bool {
    if promille >= 1000 {
        return true;
    }
    if promille == 0 {
        return false;
    }
    let bytes = Uuid::new_v4().as_u128();
    let n = u16::try_from(bytes % 1000).unwrap_or(0);
    n < promille
}

/// Pull the triggering memory id off a change event — `EntityAppend`
/// with a `Memory` entity ref. `Goal` events and `EdgeAppend` events
/// can't currently feed a wake context; we drop them rather than fail.
fn triggering_memory(event: &ChangeEventForWake) -> Option<MemoryId> {
    match &event.event.kind {
        ChangeEventKind::EntityAppend {
            entity: EntityRef::Memory(m),
            ..
        } => Some(*m),
        _ => None,
    }
}

fn wake_entry_draft_to_row(draft: &WakeEntryDraft) -> WakeEntryRow {
    WakeEntryRow {
        wake_entry_id: draft.wake_entry_id,
        trigger_kind: draft.trigger_kind,
        trigger_id: draft.trigger_id.clone(),
        label: draft.label.clone(),
        enabled: draft.enabled,
        execution_mode: match draft.execution_mode {
            WakeExecutionMode::SubstrateOnly => WakeEntryExecutionMode::SubstrateOnly,
            WakeExecutionMode::Workspace => WakeEntryExecutionMode::Workspace,
        },
        authored_by: draft.authored_by,
        probability_promille: draft.probability_promille,
        recipe_ref: draft.recipe_ref.clone(),
        model_tier: draft.model_tier,
        inference_target_ref: draft.inference_target_ref.clone(),
        substrate_tool_palette: draft.substrate_tool_palette.clone(),
        workspace_tool_palette: draft.workspace_tool_palette.clone(),
        max_rounds: draft.max_rounds,
        disabled_reason: None,
    }
}

fn is_idempotency_conflict(err: &ProtocolError) -> bool {
    use crate::error::ErrorCode;
    matches!(err.code, ErrorCode::IdempotencyConflict)
        || err.message.contains("idempotency_conflict")
}

/// Write a `failed / wake_chain_depth_exhausted` invocation row when an
/// otherwise-matching event hit the chain-depth ceiling. Idempotent on
/// the natural key like every other invocation.
async fn write_chain_depth_exhausted(
    engine: &Engine,
    group: &PersonalityGroup,
    entry: &WakeEntryDraft,
    event: &ChangeEventForWake,
) -> Result<(), ProtocolError> {
    use crate::personality::WakeInvocationStart;

    let start = WakeInvocationStart {
        owner: group.owner.clone(),
        personality_instance_id: group.personality_instance_id,
        wake_entry_id: entry.wake_entry_id,
        change_event_seq: event.event.seq,
        wake_token: Uuid::nil(),
        recipe_sha256: String::new(),
        resolved_inference_target_ref: String::new(),
    };
    let inserted = engine
        .storage()
        .start_wake_invocation(&start)
        .await
        .map_err(|e| ProtocolError::internal(format!("start_wake_invocation: {e}")))?;
    if !inserted {
        // Already recorded by an earlier tick; nothing more to do.
        return Ok(());
    }
    engine
        .storage()
        .finalize_wake_invocation(&WakeInvocationFinalize {
            owner: group.owner.clone(),
            personality_instance_id: group.personality_instance_id,
            wake_entry_id: entry.wake_entry_id,
            change_event_seq: event.event.seq,
            status: WakeInvocationStatus::Failed,
            turn_count: None,
            cost_usd: None,
            failure_reason: Some("wake_chain_depth_exhausted".to_string()),
        })
        .await
        .map_err(|e| ProtocolError::internal(format!("finalize_wake_invocation: {e}")))?;
    Ok(())
}

