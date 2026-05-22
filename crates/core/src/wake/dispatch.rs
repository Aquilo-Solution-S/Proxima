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
//! Probability is deterministic per `(event, personality, wake_entry)`
//! so explicit missed-wake replay evaluates the same event the same way
//! as live dispatch.

use uuid::Uuid;

use crate::BlockedWakeCandidate;
use crate::MemoryId;
use crate::Owner;
use crate::engine::Engine;
use crate::error::ProtocolError;
use crate::intervention::{INTERVENTION_DECISION_SCHEMA_ID, InterventionContinueCandidate};
use crate::outbox::{ChangeEventKind, EntityRef};
use crate::personality::{
    ChangeEventForWake, MAX_WAKE_CHAIN_DEPTH, PersonalityInstanceId, ReplayWakeEventsOutcome,
    ReplayWakeEventsRequest, SidecarSpec, WakeDispatchEntryRow, WakeEntryAuthoredBy,
    WakeEntryDraft, WakeEntryExecutionMode, WakeEntryGoalScope, WakeEntryRow, WakeEntryTriggerKind,
    WakeExecutionMode, WakeInvocationFinalize, WakeInvocationStatus,
};
use crate::verbs::schema::PayloadKind;
use crate::wake::context::assemble_wake_context;
use crate::wake::fire::input::FireWakeContinuation;
use crate::wake::fire::{FireWakeEntryInput, fire_wake_entry};

/// Per-tick scan limit on change events fetched per (owner, personality).
/// Bounds memory + worst-case round-trip latency; the next tick picks up
/// where this one left off via the cursor.
const CHANGE_EVENT_SCAN_LIMIT: usize = 256;
const BLOCKED_WAKE_SCAN_LIMIT: usize = 256;
const REPLAY_EVENT_LIMIT_DEFAULT: u16 = 256;
const REPLAY_EVENT_LIMIT_MAX: u16 = 1000;
const REPLAY_MAX_INVOCATIONS_DEFAULT: u16 = 1;
const REPLAY_MAX_INVOCATIONS_MAX: u16 = 20;

/// Run one dispatcher tick. Returns the count of wake fires that wrote a
/// fresh invocation row (i.e. `fire_wake_entry` returned `Ok(true)`,
/// minus rows already present from a prior tick).
///
/// # Errors
///
/// Propagates the first storage or fire error encountered. Per-fire
/// failures (target adapter, harness program, etc.) come back as
/// `Ok(_)` so the loop continues — only plumbing failures (storage
/// scan, cursor advance, missing target adapter) abort the tick.
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
        fired += process_blocked_wake_candidates(engine, &group).await?;

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
            if let Some(decision_memory_id) = intervention_decision_memory(event)
                && let Some(candidate) =
                    load_continue_candidate(engine, &group, decision_memory_id).await?
                && fire_continuation_candidate(engine, &group, event, candidate).await?
            {
                fired += 1;
            }
            for entry in &group.entries {
                match prepare_wake_candidate(engine, &group, entry, event).await? {
                    WakeCandidate::Fire {
                        triggering_memory_id,
                    } => {
                        if fire_candidate(engine, &group, entry, event, triggering_memory_id)
                            .await?
                        {
                            fired += 1;
                        }
                    }
                    WakeCandidate::Skip => {}
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
                .advance_wake_cursor(&group.owner, group.personality_instance_id, highest_seq)
                .await
                .map_err(|e| ProtocolError::internal(format!("advance_wake_cursor: {e}")))?;
        }
    }

    Ok(fired)
}

async fn process_blocked_wake_candidates(
    engine: &Engine,
    group: &PersonalityGroup,
) -> Result<usize, ProtocolError> {
    let candidates = engine
        .storage()
        .list_blocked_wake_candidates(
            &group.owner,
            group.personality_instance_id,
            BLOCKED_WAKE_SCAN_LIMIT,
        )
        .await
        .map_err(|e| ProtocolError::internal(format!("list_blocked_wake_candidates: {e}")))?;
    let mut fired = 0usize;
    for candidate in candidates {
        let Some(entry) = group
            .entries
            .iter()
            .find(|entry| entry.wake_entry_id == candidate.wake_entry_id && entry.enabled)
        else {
            continue;
        };
        if !dependencies_satisfied(
            engine,
            group,
            entry,
            candidate.change_event_seq,
            candidate.triggering_memory_id,
        )
        .await?
        {
            continue;
        }
        let Some(event) = engine
            .storage()
            .fetch_change_event_for_wake(&group.owner, candidate.change_event_seq)
            .await
            .map_err(|e| ProtocolError::internal(format!("fetch_change_event_for_wake: {e}")))?
        else {
            engine
                .storage()
                .delete_blocked_wake_candidate(
                    &group.owner,
                    group.personality_instance_id,
                    candidate.wake_entry_id,
                    candidate.change_event_seq,
                )
                .await
                .map_err(|e| {
                    ProtocolError::internal(format!("delete_blocked_wake_candidate: {e}"))
                })?;
            continue;
        };
        if fire_candidate(engine, group, entry, &event, candidate.triggering_memory_id).await? {
            fired += 1;
        }
        engine
            .storage()
            .delete_blocked_wake_candidate(
                &group.owner,
                group.personality_instance_id,
                candidate.wake_entry_id,
                candidate.change_event_seq,
            )
            .await
            .map_err(|e| ProtocolError::internal(format!("delete_blocked_wake_candidate: {e}")))?;
    }
    Ok(fired)
}

/// Replay eligible events that are already behind normal dispatch
/// cursors. This is an operator-driven repair path; it intentionally
/// does not move `personality_wake_cursor`.
pub async fn replay_missed_wakes(
    engine: &Engine,
    req: ReplayWakeEventsRequest,
) -> Result<ReplayWakeEventsOutcome, ProtocolError> {
    let event_limit = clamp_limit(
        req.event_limit,
        REPLAY_EVENT_LIMIT_DEFAULT,
        REPLAY_EVENT_LIMIT_MAX,
    );
    let max_invocations = clamp_limit(
        req.max_invocations,
        REPLAY_MAX_INVOCATIONS_DEFAULT,
        REPLAY_MAX_INVOCATIONS_MAX,
    );

    let rows = engine
        .storage()
        .list_active_wake_entries()
        .await
        .map_err(|e| ProtocolError::internal(format!("list_active_wake_entries: {e}")))?;
    let entries: Vec<_> = rows
        .into_iter()
        .filter(|row| {
            row.owner == req.owner
                && row.personality_instance_id == req.personality_instance_id
                && req
                    .wake_entry_id
                    .is_none_or(|wake_entry_id| row.wake_entry.wake_entry_id == wake_entry_id)
        })
        .collect();

    if entries.is_empty() {
        return Ok(ReplayWakeEventsOutcome {
            considered_events: 0,
            eligible_events: 0,
            started_invocations: 0,
            already_recorded: 0,
            skipped: 0,
            complete: true,
            next_after_seq: req.after_seq,
        });
    }

    let mut groups = group_by_personality(entries);
    let Some(group) = groups.pop() else {
        unreachable!("entries.is_empty checked above");
    };
    let after = req.after_seq.unwrap_or_else(Uuid::nil);
    let events = engine
        .storage()
        .list_change_events_for_replay(&req.owner, after, req.until_seq, usize::from(event_limit))
        .await
        .map_err(|e| ProtocolError::internal(format!("list_change_events_for_replay: {e}")))?;

    let mut outcome = ReplayWakeEventsOutcome {
        considered_events: 0,
        eligible_events: 0,
        started_invocations: 0,
        already_recorded: 0,
        skipped: 0,
        complete: true,
        next_after_seq: req.after_seq,
    };
    let mut hit_invocation_cap = false;

    'events: for event in &events {
        outcome.considered_events += 1;
        outcome.next_after_seq = Some(event.event.seq);
        if let Some(decision_memory_id) = intervention_decision_memory(event)
            && let Some(candidate) =
                load_continue_candidate(engine, &group, decision_memory_id).await?
        {
            outcome.eligible_events += 1;
            if fire_continuation_candidate(engine, &group, event, candidate).await? {
                outcome.started_invocations += 1;
                if outcome.started_invocations >= u32::from(max_invocations) {
                    hit_invocation_cap = true;
                    break 'events;
                }
            } else {
                outcome.already_recorded += 1;
            }
        }
        for entry in &group.entries {
            match prepare_wake_candidate(engine, &group, entry, event).await? {
                WakeCandidate::Fire {
                    triggering_memory_id,
                } => {
                    outcome.eligible_events += 1;
                    if fire_candidate(engine, &group, entry, event, triggering_memory_id).await? {
                        outcome.started_invocations += 1;
                        if outcome.started_invocations >= u32::from(max_invocations) {
                            hit_invocation_cap = true;
                            break 'events;
                        }
                    } else {
                        outcome.already_recorded += 1;
                    }
                }
                WakeCandidate::Skip => {
                    outcome.skipped += 1;
                }
            }
        }
    }

    outcome.complete = !hit_invocation_cap && events.len() < usize::from(event_limit);
    Ok(outcome)
}

fn clamp_limit(value: u16, default: u16, max: u16) -> u16 {
    if value == 0 { default } else { value.min(max) }
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
        (WakeEntryTriggerKind::OnMemory, ChangeEventKind::EntityAppend { schema_id, .. }) => {
            entry.trigger_id == schema_id.as_str()
        }
        (WakeEntryTriggerKind::OnEdge, ChangeEventKind::EdgeAppend { relation, .. }) => {
            entry.trigger_id == relation.as_str()
        }
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
            None => true, // external/event-source counts as Other
            Some(author) => author != self_instance,
        },
    }
}

/// Deterministic probability gate. `0` never fires, `1000` always
/// fires; values in between are stable for a given event/personality/
/// entry tuple so missed-wake replay agrees with live dispatch.
fn probability_roll(
    promille: u16,
    event_seq: Uuid,
    personality_instance_id: PersonalityInstanceId,
    wake_entry_id: Uuid,
) -> bool {
    if promille >= 1000 {
        return true;
    }
    if promille == 0 {
        return false;
    }
    let mixed = event_seq.as_u128()
        ^ personality_instance_id
            .into_inner()
            .as_u128()
            .rotate_left(17)
        ^ wake_entry_id.as_u128().rotate_left(47);
    let mut x = (mixed ^ (mixed >> 64)) as u64;
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    let n = u16::try_from(x % 1000).unwrap_or(0);
    n < promille
}

enum WakeCandidate {
    Fire { triggering_memory_id: MemoryId },
    Skip,
}

async fn prepare_wake_candidate(
    engine: &Engine,
    group: &PersonalityGroup,
    entry: &WakeEntryDraft,
    event: &ChangeEventForWake,
) -> Result<WakeCandidate, ProtocolError> {
    if !triggers_match(entry, event) {
        return Ok(WakeCandidate::Skip);
    }
    if !authored_by_matches(
        entry.authored_by,
        event.authoring_personality_instance_id,
        group.personality_instance_id,
    ) {
        return Ok(WakeCandidate::Skip);
    }
    // Defense-in-depth self-wake guard: the authored_by filter above
    // already drops self-authored events when `authored_by != Any`, but
    // `Any` would otherwise let a self-edit walk back into a wake.
    // fire_wake_entry has its own guard for the race.
    if event.authoring_personality_instance_id == Some(group.personality_instance_id) {
        return Ok(WakeCandidate::Skip);
    }
    if event.wake_chain_depth.into_inner() >= MAX_WAKE_CHAIN_DEPTH {
        write_chain_depth_exhausted(engine, group, entry, event).await?;
        return Ok(WakeCandidate::Skip);
    }
    match goal_scope_matches(engine, group, entry, event).await? {
        GoalScopeDecision::Fire => {}
        GoalScopeDecision::Skip => return Ok(WakeCandidate::Skip),
        GoalScopeDecision::Misconfigured(reason) => {
            write_filter_misconfigured(engine, group, entry, event, reason).await?;
            return Ok(WakeCandidate::Skip);
        }
    }
    if !probability_roll(
        entry.probability_promille,
        event.event.seq,
        group.personality_instance_id,
        entry.wake_entry_id,
    ) {
        return Ok(WakeCandidate::Skip);
    }

    let Some(triggering_memory_id) = triggering_memory(event) else {
        return Ok(WakeCandidate::Skip);
    };
    if !dependencies_satisfied(engine, group, entry, event.event.seq, triggering_memory_id).await? {
        return Ok(WakeCandidate::Skip);
    }
    Ok(WakeCandidate::Fire {
        triggering_memory_id,
    })
}

async fn dependencies_satisfied(
    engine: &Engine,
    group: &PersonalityGroup,
    entry: &WakeEntryDraft,
    change_event_seq: Uuid,
    triggering_memory_id: MemoryId,
) -> Result<bool, ProtocolError> {
    let dependencies = engine
        .storage()
        .list_memory_dependencies(&group.owner, triggering_memory_id)
        .await
        .map_err(|e| ProtocolError::internal(format!("list_memory_dependencies: {e}")))?;
    for dependency in dependencies {
        let Some(rule) = engine
            .registry()
            .dependency_satisfaction_rule(dependency.dependency_schema_id.as_str())
        else {
            record_blocked_dependency(
                engine,
                group,
                entry,
                change_event_seq,
                triggering_memory_id,
                dependency.dependency_memory_id,
                dependency.dependency_schema_id,
                "missing_dependency_satisfaction_rule",
            )
            .await?;
            return Ok(false);
        };
        let satisfied = rule
            .is_satisfied(
                engine.storage().as_ref(),
                &group.owner,
                dependency.dependency_memory_id,
            )
            .await
            .map_err(|e| ProtocolError::internal(format!("dependency_satisfaction: {e}")))?;
        if !satisfied {
            record_blocked_dependency(
                engine,
                group,
                entry,
                change_event_seq,
                triggering_memory_id,
                dependency.dependency_memory_id,
                dependency.dependency_schema_id,
                "dependency_unsatisfied",
            )
            .await?;
            return Ok(false);
        }
    }
    Ok(true)
}

async fn record_blocked_dependency(
    engine: &Engine,
    group: &PersonalityGroup,
    entry: &WakeEntryDraft,
    change_event_seq: Uuid,
    triggering_memory_id: MemoryId,
    dependency_memory_id: MemoryId,
    dependency_schema_id: crate::SchemaId,
    reason: &str,
) -> Result<(), ProtocolError> {
    engine
        .storage()
        .upsert_blocked_wake_candidate(&BlockedWakeCandidate {
            owner: group.owner.clone(),
            personality_instance_id: group.personality_instance_id,
            wake_entry_id: entry.wake_entry_id,
            change_event_seq,
            triggering_memory_id,
            dependency_memory_id,
            dependency_schema_id,
            reason: reason.to_string(),
        })
        .await
        .map_err(|e| ProtocolError::internal(format!("upsert_blocked_wake_candidate: {e}")))
}

async fn fire_candidate(
    engine: &Engine,
    group: &PersonalityGroup,
    entry: &WakeEntryDraft,
    event: &ChangeEventForWake,
    triggering_memory_id: MemoryId,
) -> Result<bool, ProtocolError> {
    // Surface the adapter late so filters and repair rows can still be
    // evaluated without an adapter, but an actual fire fails loudly.
    let adapter = engine.target_adapter().ok_or_else(|| {
        ProtocolError::internal(
            "dispatcher fired before target adapter was installed — \
             call Engine::start (or with_target_adapter) first",
        )
    })?;

    let input = FireWakeEntryInput {
        owner: group.owner.clone(),
        personality_instance_id: group.personality_instance_id,
        wake_entry: wake_entry_draft_to_row(entry),
        change_event_seq: event.event.seq,
        triggering_memory_id: triggering_memory_id.into_inner(),
        continuation: None,
    };

    match fire_wake_entry(engine, adapter.as_ref(), input).await {
        Ok(true) => Ok(true),
        Ok(false) => Ok(false),
        Err(e) if is_idempotency_conflict(&e) => Ok(false),
        Err(e) => Err(e),
    }
}

fn intervention_decision_memory(event: &ChangeEventForWake) -> Option<MemoryId> {
    match &event.event.kind {
        ChangeEventKind::EntityAppend {
            entity: EntityRef::Memory(memory_id),
            schema_id,
            ..
        } if schema_id.as_str() == INTERVENTION_DECISION_SCHEMA_ID => Some(*memory_id),
        _ => None,
    }
}

async fn load_continue_candidate(
    engine: &Engine,
    group: &PersonalityGroup,
    decision_memory_id: MemoryId,
) -> Result<Option<InterventionContinueCandidate>, ProtocolError> {
    let candidate = engine
        .storage()
        .load_intervention_continue_candidate(&group.owner, decision_memory_id)
        .await
        .map_err(|e| {
            ProtocolError::internal(format!("load_intervention_continue_candidate: {e}"))
        })?;
    Ok(candidate.filter(|candidate| {
        candidate.original_personality_instance_id == group.personality_instance_id
            && group
                .entries
                .iter()
                .any(|entry| entry.wake_entry_id == candidate.original_wake_entry_id)
    }))
}

async fn fire_continuation_candidate(
    engine: &Engine,
    group: &PersonalityGroup,
    event: &ChangeEventForWake,
    candidate: InterventionContinueCandidate,
) -> Result<bool, ProtocolError> {
    let adapter = engine.target_adapter().ok_or_else(|| {
        ProtocolError::internal(
            "dispatcher fired before target adapter was installed — \
             call Engine::start (or with_target_adapter) first",
        )
    })?;
    let Some(input) = continuation_fire_input(group, event, candidate) else {
        return Ok(false);
    };

    match fire_wake_entry(engine, adapter.as_ref(), input).await {
        Ok(true) => Ok(true),
        Ok(false) => Ok(false),
        Err(e) if is_idempotency_conflict(&e) => Ok(false),
        Err(e) => Err(e),
    }
}

fn continuation_fire_input(
    group: &PersonalityGroup,
    event: &ChangeEventForWake,
    candidate: InterventionContinueCandidate,
) -> Option<FireWakeEntryInput> {
    let entry = group
        .entries
        .iter()
        .find(|entry| entry.wake_entry_id == candidate.original_wake_entry_id)?;
    let mut wake_entry = wake_entry_draft_to_row(entry);
    wake_entry.max_rounds = candidate.grant_rounds;
    wake_entry.intervention_policy = None;
    Some(FireWakeEntryInput {
        owner: group.owner.clone(),
        personality_instance_id: group.personality_instance_id,
        wake_entry,
        change_event_seq: event.event.seq,
        triggering_memory_id: candidate.original_triggering_memory_id.into_inner(),
        continuation: Some(FireWakeContinuation {
            intervention_decision_memory_id: candidate.intervention_decision_memory_id,
            intervention_request_memory_id: candidate.intervention_request_memory_id,
            original_invocation_id: candidate.original_invocation_id,
            original_change_event_seq: candidate.original_change_event_seq,
            wake_trace_memory_id: candidate.wake_trace_memory_id,
            original_triggering_memory_id: candidate.original_triggering_memory_id,
            grant_rounds: candidate.grant_rounds,
            rationale: candidate.rationale,
        }),
    })
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
        instructions: draft.instructions.clone(),
        model_tier: draft.model_tier,
        inference_target_ref: draft.inference_target_ref.clone(),
        substrate_tool_palette: draft.substrate_tool_palette.clone(),
        workspace_tool_palette: draft.workspace_tool_palette.clone(),
        workspace_binding: draft.workspace_binding.clone(),
        required_produced_schema_ids: draft.required_produced_schema_ids.clone(),
        max_rounds: draft.max_rounds,
        intervention_policy: draft.intervention_policy.clone(),
        disabled_reason: None,
        goal_scope: draft.goal_scope,
    }
}

enum GoalScopeDecision {
    Fire,
    Skip,
    Misconfigured(String),
}

async fn goal_scope_matches(
    engine: &Engine,
    group: &PersonalityGroup,
    entry: &WakeEntryDraft,
    event: &ChangeEventForWake,
) -> Result<GoalScopeDecision, ProtocolError> {
    match entry.goal_scope {
        WakeEntryGoalScope::None => return Ok(GoalScopeDecision::Fire),
        WakeEntryGoalScope::TriggerGoalAssigned => {}
    }

    let sidecars = collect_sidecars(engine);
    let wake_context = assemble_wake_context(
        engine.storage().as_ref(),
        &group.owner,
        group.personality_instance_id,
        event.event.seq,
        &sidecars,
    )
    .await?;

    let Some(goal_id) = wake_context
        .triggering_memory
        .typed_payload
        .get("goal_id")
        .and_then(serde_json::Value::as_str)
    else {
        return Ok(GoalScopeDecision::Misconfigured(
            "wake_goal_scope_missing_goal_id".to_string(),
        ));
    };
    let Ok(goal_id) = Uuid::parse_str(goal_id) else {
        return Ok(GoalScopeDecision::Misconfigured(
            "wake_goal_scope_invalid_goal_id".to_string(),
        ));
    };
    if wake_context
        .active_goals
        .iter()
        .any(|goal| goal.goal_id == goal_id)
    {
        Ok(GoalScopeDecision::Fire)
    } else {
        Ok(GoalScopeDecision::Skip)
    }
}

fn collect_sidecars(engine: &Engine) -> Vec<SidecarSpec> {
    engine
        .registry()
        .list()
        .into_iter()
        .filter(|s| {
            matches!(
                s.kind,
                PayloadKind::Fact | PayloadKind::Abstraction | PayloadKind::Perspective
            )
        })
        .filter_map(|s| {
            s.sidecar_table.map(|table| SidecarSpec {
                schema_id: s.schema_id,
                sidecar_table: table,
            })
        })
        .collect()
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
        invocation_id: Uuid::now_v7(),
        owner: group.owner.clone(),
        personality_instance_id: group.personality_instance_id,
        wake_entry_id: entry.wake_entry_id,
        change_event_seq: event.event.seq,
        wake_token: Uuid::nil(),
        resolved_inference_target_ref: String::new(),
        continuation: None,
    };
    let invocation_id = start.invocation_id;
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
            invocation_id,
            owner: group.owner.clone(),
            personality_instance_id: group.personality_instance_id,
            wake_entry_id: entry.wake_entry_id,
            change_event_seq: event.event.seq,
            status: WakeInvocationStatus::Failed,
            turn_count: None,
            cost_usd: None,
            failure_reason: Some("wake_chain_depth_exhausted".to_string()),
            exit_code: None,
            duration_ms: None,
            stdout_tail: None,
            stderr_tail: None,
            stdout_truncated: false,
            stderr_truncated: false,
        })
        .await
        .map_err(|e| ProtocolError::internal(format!("finalize_wake_invocation: {e}")))?;
    Ok(())
}

async fn write_filter_misconfigured(
    engine: &Engine,
    group: &PersonalityGroup,
    entry: &WakeEntryDraft,
    event: &ChangeEventForWake,
    reason: String,
) -> Result<(), ProtocolError> {
    use crate::personality::WakeInvocationStart;

    let start = WakeInvocationStart {
        invocation_id: Uuid::now_v7(),
        owner: group.owner.clone(),
        personality_instance_id: group.personality_instance_id,
        wake_entry_id: entry.wake_entry_id,
        change_event_seq: event.event.seq,
        wake_token: Uuid::nil(),
        resolved_inference_target_ref: String::new(),
        continuation: None,
    };
    let invocation_id = start.invocation_id;
    let inserted = engine
        .storage()
        .start_wake_invocation(&start)
        .await
        .map_err(|e| ProtocolError::internal(format!("start_wake_invocation: {e}")))?;
    if !inserted {
        return Ok(());
    }
    engine
        .storage()
        .finalize_wake_invocation(&WakeInvocationFinalize {
            invocation_id,
            owner: group.owner.clone(),
            personality_instance_id: group.personality_instance_id,
            wake_entry_id: entry.wake_entry_id,
            change_event_seq: event.event.seq,
            status: WakeInvocationStatus::Failed,
            turn_count: None,
            cost_usd: None,
            failure_reason: Some(reason),
            exit_code: None,
            duration_ms: None,
            stdout_tail: None,
            stderr_tail: None,
            stdout_truncated: false,
            stderr_truncated: false,
        })
        .await
        .map_err(|e| ProtocolError::internal(format!("finalize_wake_invocation: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EntityKind, ModelTier, OrgId, Principal, SchemaId, SchemaVersion, UserId};

    #[test]
    fn continuation_fire_input_uses_original_trigger_as_wake_subject() {
        let owner = Owner {
            principal: Principal::User(UserId::new(Uuid::from_u128(1))),
            org_id: OrgId::new(Uuid::from_u128(2)),
        };
        let personality = PersonalityInstanceId::new(Uuid::from_u128(3));
        let original_wake_entry = Uuid::from_u128(4);
        let original_triggering_memory = MemoryId::new(Uuid::from_u128(5));
        let intervention_decision_memory = MemoryId::new(Uuid::from_u128(6));
        let intervention_request_memory = MemoryId::new(Uuid::from_u128(7));
        let wake_trace_memory = MemoryId::new(Uuid::from_u128(8));
        let decision_event_seq = Uuid::from_u128(9);
        let original_change_event_seq = Uuid::from_u128(10);

        let mut wake_entry = WakeEntryDraft::new(
            original_wake_entry,
            personality,
            WakeEntryTriggerKind::OnMemory,
            "proxima-core/workspace-run-v1",
            "Verifier",
            WakeEntryAuthoredBy::Any,
            1000,
            ModelTier::Standard,
            None,
            vec!["proxima-code/code_emit_workspace_review".into()],
            10,
        )
        .expect("wake entry");
        wake_entry.execution_mode = WakeExecutionMode::Workspace;
        wake_entry.workspace_tool_palette = vec!["workspace_shell".into()];

        let group = PersonalityGroup {
            owner: owner.clone(),
            personality_instance_id: personality,
            last_considered_seq: Uuid::nil(),
            entries: vec![wake_entry],
        };
        let event = ChangeEventForWake {
            event: crate::outbox::ChangeEvent {
                seq: decision_event_seq,
                owner,
                kind: ChangeEventKind::EntityAppend {
                    entity_kind: EntityKind::Fact,
                    entity: EntityRef::Memory(intervention_decision_memory),
                    schema_id: SchemaId::new(INTERVENTION_DECISION_SCHEMA_ID.into()),
                    schema_version: SchemaVersion::new(1),
                    supersedes: None,
                },
                authoring_personality_instance_id: None,
                wake_chain_depth: 0,
            },
            authoring_personality_instance_id: None,
            wake_chain_depth: crate::personality::WakeChainDepth::new(0),
        };
        let candidate = InterventionContinueCandidate {
            intervention_decision_memory_id: intervention_decision_memory,
            intervention_request_memory_id: intervention_request_memory,
            original_invocation_id: Uuid::from_u128(11),
            original_wake_entry_id: original_wake_entry,
            original_personality_instance_id: personality,
            original_change_event_seq,
            original_triggering_memory_id: original_triggering_memory,
            wake_trace_memory_id: wake_trace_memory,
            grant_rounds: 4,
            rationale: "continue from persisted graph state".into(),
        };

        let input = continuation_fire_input(&group, &event, candidate).expect("continuation input");

        assert_eq!(input.change_event_seq, decision_event_seq);
        assert_eq!(
            input.triggering_memory_id,
            original_triggering_memory.into_inner()
        );
        assert_eq!(input.wake_entry.trigger_id, "proxima-core/workspace-run-v1");
        assert_eq!(input.wake_entry.max_rounds, 4);
        assert!(input.wake_entry.intervention_policy.is_none());

        let continuation = input.continuation.expect("continuation metadata");
        assert_eq!(
            continuation.intervention_decision_memory_id,
            intervention_decision_memory
        );
        assert_eq!(
            continuation.intervention_request_memory_id,
            intervention_request_memory
        );
        assert_eq!(
            continuation.original_triggering_memory_id,
            original_triggering_memory
        );
        assert_eq!(
            continuation.original_change_event_seq,
            original_change_event_seq
        );
    }
}
