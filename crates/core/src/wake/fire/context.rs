//! Model-context projection for wake fire path.

use std::collections::{HashMap, HashSet};

use uuid::Uuid;

use crate::engine::Engine;
use crate::error::ProtocolError;
use crate::harness::HarnessToolProjection;
use crate::mcp::{HandleTable, MemoryHandleClass};
use crate::personality::PersonalityInstanceId;
use crate::verbs::query::{QueryRequest, SupersessionStatus};
use crate::wake::context::WakeContext;
use crate::wake::contract::build_wake_contract;
use crate::wake::fire::input::FireWakeContinuation;
use crate::{EntityKind, GoalId, MemoryId, Owner};

use super::input::FireWakeEntryInput;

const ACTIVE_PERSPECTIVE_TEXT_LIMIT: usize = 2_000;

pub(super) fn build_system_prompt(
    wake_context: &WakeContext,
    seeded: &crate::mcp::PreSeededHandles,
    continuation: Option<&FireWakeContinuation>,
) -> String {
    let schema_id = wake_context.triggering_memory.schema_id.as_str();
    let schema_arg = if schema_id.is_empty() {
        None
    } else {
        Some(schema_id)
    };
    let mut prompt = crate::wake::handles::format_wake_context_preamble(
        seeded,
        schema_arg,
        wake_context.triggering_memory.kind.as_str(),
    );
    if let Some(continuation) = continuation {
        prompt.push_str(&format_continuation_preamble(seeded, continuation));
    }
    prompt.push_str(&wake_context.root_perspective.system_prompt);
    prompt
}

pub(super) fn format_continuation_preamble(
    seeded: &crate::mcp::PreSeededHandles,
    continuation: &FireWakeContinuation,
) -> String {
    format!(
        "\nContinuation:\n\
         - This invocation continues a prior truncated wake. Use persisted Proxima state as the continuity source; provider chat session state is not available.\n\
         - Open these handles before acting:\n\
         - continuation.intervention_decision.handle: {}\n\
         - continuation.intervention_request.handle: {}\n\
         - continuation.prior_wake_trace.handle: {}\n\
         - continuation.original_triggering_memory.handle: {}\n\
         - granted_rounds: {}\n\
         - supervisor_rationale: {}\n\
         - Inspect the prior trace or lineage before repeating work.\n\n",
        seeded
            .continuation_decision
            .as_ref()
            .map_or("<unavailable>", crate::mcp::Handle::as_str),
        seeded
            .continuation_request
            .as_ref()
            .map_or("<unavailable>", crate::mcp::Handle::as_str),
        seeded
            .continuation_wake_trace
            .as_ref()
            .map_or("<unavailable>", crate::mcp::Handle::as_str),
        seeded
            .continuation_original_triggering
            .as_ref()
            .map_or("<unavailable>", crate::mcp::Handle::as_str),
        continuation.grant_rounds,
        continuation.rationale.trim(),
    )
}

pub(super) fn continuation_context_params(
    seeded: &crate::mcp::PreSeededHandles,
    continuation: &FireWakeContinuation,
) -> serde_json::Value {
    serde_json::json!({
        "intervention_decision": {
            "handle": seeded.continuation_decision.as_ref().map(crate::mcp::Handle::as_str),
        },
        "intervention_request": {
            "handle": seeded.continuation_request.as_ref().map(crate::mcp::Handle::as_str),
        },
        "prior_wake_trace": {
            "handle": seeded.continuation_wake_trace.as_ref().map(crate::mcp::Handle::as_str),
        },
        "original_triggering_memory": {
            "handle": seeded.continuation_original_triggering.as_ref().map(crate::mcp::Handle::as_str),
        },
        "grant_rounds": continuation.grant_rounds,
        "rationale": continuation.rationale.as_str(),
        "instruction": "Inspect the prior trace or lineage before repeating work. Provider chat session state is unavailable; persisted graph state is the continuity source.",
    })
}

pub(super) async fn build_context_params(
    engine: &Engine,
    input: &FireWakeEntryInput,
    wake_context: &WakeContext,
    seeded_handles: &crate::mcp::PreSeededHandles,
    handles: &HandleTable,
    tool_projection: &[HarnessToolProjection],
) -> Result<HashMap<String, serde_json::Value>, ProtocolError> {
    let mut context_params: HashMap<String, serde_json::Value> = HashMap::new();
    context_params.insert(
        "root_perspective".into(),
        project_root_perspective(wake_context, handles),
    );
    context_params.insert(
        "active_perspectives".into(),
        project_active_perspectives(wake_context, handles),
    );
    context_params.insert(
        "active_goals".into(),
        project_active_goals(wake_context, handles),
    );
    context_params.insert("trigger_event".into(), project_trigger_event(wake_context));
    let payload_memory_classes = load_payload_memory_classes(
        engine,
        &input.owner,
        &wake_context.triggering_memory.typed_payload,
    )
    .await?;
    context_params.insert(
        "triggering_memory".into(),
        project_triggering_memory(wake_context, handles, &payload_memory_classes),
    );
    context_params.insert(
        "wake_contract".into(),
        super::fire::context_value(build_wake_contract(
            &input.wake_entry,
            tool_projection,
            handles,
        ))?,
    );
    let coordination_context = crate::chat::build_wake_coordination_context(
        engine,
        &input.owner,
        input.personality_instance_id,
        &input.wake_entry,
    )
    .await
    .map_err(|err| ProtocolError::internal(format!("build_wake_coordination_context: {err}")))?;
    context_params.insert(
        "coordination_context".into(),
        project_coordination_context(&coordination_context, handles),
    );
    if let Some(continuation) = input.continuation.as_ref() {
        context_params.insert(
            "continuation".into(),
            continuation_context_params(seeded_handles, continuation),
        );
    }
    Ok(context_params)
}

pub(super) fn project_root_perspective(
    wake_context: &WakeContext,
    handles: &HandleTable,
) -> serde_json::Value {
    serde_json::json!({
        "personality": handles
            .assign_personality(PersonalityInstanceId::new(wake_context.root_perspective.instance_id))
            .as_str()
            .to_string(),
        "root_perspective": handles
            .assign_perspective_memory(MemoryId::new(wake_context.root_perspective.memory_id))
            .as_str()
            .to_string(),
        "display_name": wake_context.root_perspective.display_name.as_str(),
        "purpose": wake_context.root_perspective.purpose.as_str(),
        "system_prompt": wake_context.root_perspective.system_prompt.as_str(),
    })
}

pub(super) fn project_active_perspectives(
    wake_context: &WakeContext,
    handles: &HandleTable,
) -> serde_json::Value {
    serde_json::Value::Array(
        wake_context
            .active_perspectives
            .iter()
            .map(|perspective| {
                let (text, truncated) =
                    truncate_context_text(&perspective.text, ACTIVE_PERSPECTIVE_TEXT_LIMIT);
                serde_json::json!({
                    "perspective": handles
                        .assign_perspective_memory(MemoryId::new(perspective.memory_id))
                        .as_str()
                        .to_string(),
                    "schema_id": perspective.schema_id.as_str(),
                    "schema_version": perspective.schema_version,
                    "text": text,
                    "wake_chain_depth": perspective.wake_chain_depth,
                    "truncated": truncated,
                })
            })
            .collect(),
    )
}

pub(super) fn truncate_context_text(text: &str, max_chars: usize) -> (String, bool) {
    let mut chars = text.chars();
    let truncated: String = chars.by_ref().take(max_chars).collect();
    let was_truncated = chars.next().is_some();
    (truncated, was_truncated)
}

pub(super) fn project_active_goals(
    wake_context: &WakeContext,
    handles: &HandleTable,
) -> serde_json::Value {
    serde_json::Value::Array(
        wake_context
            .active_goals
            .iter()
            .map(|goal| {
                serde_json::json!({
                    "goal": handles.assign_goal(GoalId::new(goal.goal_id)).as_str().to_string(),
                    "goal_activated_memory": goal
                        .goal_activated_memory_id
                        .map(|id| handles.assign_fact_memory(MemoryId::new(id)).as_str().to_string()),
                    "title": goal.title.as_str(),
                    "motivation_via": goal
                        .motivation_via
                        .iter()
                        .map(|id| handles.assign_perspective_memory(MemoryId::new(*id)).as_str().to_string())
                        .collect::<Vec<_>>(),
                })
            })
            .collect(),
    )
}

pub(super) fn project_trigger_event(wake_context: &WakeContext) -> serde_json::Value {
    serde_json::json!({
        "kind": wake_context.trigger_event.kind.as_str(),
        "schema_id": wake_context.trigger_event.schema_id.as_str(),
        "wake_chain_depth": wake_context.trigger_event.wake_chain_depth,
    })
}

pub(super) fn project_triggering_memory(
    wake_context: &WakeContext,
    handles: &HandleTable,
    memory_classes: &HashMap<Uuid, MemoryHandleClass>,
) -> serde_json::Value {
    serde_json::json!({
        "memory": handles
            .assign_memory_kind(MemoryId::new(wake_context.triggering_memory.memory_id), &wake_context.triggering_memory.kind)
            .as_str()
            .to_string(),
        "kind": wake_context.triggering_memory.kind.as_str(),
        "schema_id": wake_context.triggering_memory.schema_id.as_str(),
        "schema_version": wake_context.triggering_memory.schema_version,
        "typed_payload": project_model_value(&wake_context.triggering_memory.typed_payload, None, handles, memory_classes),
    })
}

pub(super) fn project_coordination_context(
    context: &crate::chat::WakeCoordinationContext,
    handles: &HandleTable,
) -> serde_json::Value {
    serde_json::json!({
        "chat_targets": context
            .chat_targets
            .iter()
            .map(|target| {
                serde_json::json!({
                    "personality": handles
                        .assign_personality(PersonalityInstanceId::new(target.personality_instance_id))
                        .as_str()
                        .to_string(),
                    "display_name": target.display_name.as_str(),
                    "root_perspective": handles
                        .assign_perspective_memory(MemoryId::new(target.root_perspective_memory_id))
                        .as_str()
                        .to_string(),
                    "chat_message_wake_entries": target
                        .chat_message_wake_entry_ids
                        .iter()
                        .map(|id| handles.assign_wake_entry(*id).as_str().to_string())
                        .collect::<Vec<_>>(),
                })
            })
            .collect::<Vec<_>>(),
        "wake_path": {
            "upstream": context
                .wake_path
                .upstream
                .iter()
                .map(|node| project_wake_path_node(node, handles))
                .collect::<Vec<_>>(),
            "current": project_wake_path_node(&context.wake_path.current, handles),
            "downstream": context
                .wake_path
                .downstream
                .iter()
                .map(|node| project_wake_path_node(node, handles))
                .collect::<Vec<_>>(),
        }
    })
}

pub(super) fn project_wake_path_node(
    node: &crate::chat::WakePathNode,
    handles: &HandleTable,
) -> serde_json::Value {
    serde_json::json!({
        "personality": handles
            .assign_personality(PersonalityInstanceId::new(node.personality_instance_id))
            .as_str()
            .to_string(),
        "display_name": node.display_name.as_str(),
        "root_perspective": if node.root_perspective_memory_id == Uuid::nil() {
            serde_json::Value::Null
        } else {
            serde_json::Value::String(
                handles
                    .assign_perspective_memory(MemoryId::new(node.root_perspective_memory_id))
                    .as_str()
                    .to_string(),
            )
        },
        "wake_entry": handles.assign_wake_entry(node.wake_entry_id).as_str().to_string(),
        "wake_entry_label": node.wake_entry_label.as_str(),
        "trigger_schema_id": node.trigger_schema_id.as_str(),
        "produces_schema_ids": node.produces_schema_ids.clone(),
    })
}

pub(super) fn project_model_value(
    value: &serde_json::Value,
    key: Option<&str>,
    handles: &HandleTable,
    memory_classes: &HashMap<Uuid, MemoryHandleClass>,
) -> serde_json::Value {
    match value {
        serde_json::Value::String(raw) => project_model_string(raw, key, handles, memory_classes),
        serde_json::Value::Array(values) => serde_json::Value::Array(
            values
                .iter()
                .map(|value| project_model_value(value, key, handles, memory_classes))
                .collect(),
        ),
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.iter()
                .map(|(field, value)| {
                    (
                        field.clone(),
                        project_model_value(value, Some(field.as_str()), handles, memory_classes),
                    )
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

pub(super) fn project_model_string(
    raw: &str,
    key: Option<&str>,
    handles: &HandleTable,
    memory_classes: &HashMap<Uuid, MemoryHandleClass>,
) -> serde_json::Value {
    let Some(uuid) = Uuid::parse_str(raw).ok() else {
        return serde_json::Value::String(redact_uuid_substrings(raw));
    };
    let Some(key) = key else {
        return serde_json::Value::String("<opaque-uuid>".to_string());
    };
    let normalized = normalize_reference_key(key);
    if normalized == "goal_id" || normalized.ends_with("_goal_id") {
        return serde_json::Value::String(
            handles.assign_goal(GoalId::new(uuid)).as_str().to_string(),
        );
    }
    if normalized == "repo_id" || normalized.ends_with("_repo_id") {
        return serde_json::Value::String(
            handles
                .assign_flavor_object("code/repository", uuid, 'R')
                .as_str()
                .to_string(),
        );
    }
    if fact_memory_field(&normalized) {
        return serde_json::Value::String(
            handles
                .assign_fact_memory(MemoryId::new(uuid))
                .as_str()
                .to_string(),
        );
    }
    if perspective_memory_field(&normalized) {
        return serde_json::Value::String(
            handles
                .assign_perspective_memory(MemoryId::new(uuid))
                .as_str()
                .to_string(),
        );
    }
    if generic_memory_field(&normalized) {
        if let Some(class) = memory_classes.get(&uuid).copied() {
            return serde_json::Value::String(
                handles
                    .assign_memory_with_class(MemoryId::new(uuid), class)
                    .as_str()
                    .to_string(),
            );
        }
        return serde_json::Value::String("<opaque-memory-uuid>".to_string());
    }
    if normalized == "personality_instance_id" || normalized.ends_with("_personality_instance_id") {
        return serde_json::Value::String(
            handles
                .assign_personality(PersonalityInstanceId::new(uuid))
                .as_str()
                .to_string(),
        );
    }
    if normalized == "wake_entry_id" || normalized.ends_with("_wake_entry_id") {
        return serde_json::Value::String(handles.assign_wake_entry(uuid).as_str().to_string());
    }
    if normalized == "edge_id" || normalized.ends_with("_edge_id") {
        return serde_json::Value::String(
            handles
                .assign_edge(crate::EdgeId::new(uuid))
                .as_str()
                .to_string(),
        );
    }
    serde_json::Value::String("<opaque-uuid>".to_string())
}

pub(super) fn redact_uuid_substrings(raw: &str) -> String {
    const UUID_LEN: usize = 36;
    if raw.len() < UUID_LEN {
        return raw.to_string();
    }
    let mut output = String::with_capacity(raw.len());
    let mut cursor = 0;
    while cursor < raw.len() {
        let Some(remaining) = raw.get(cursor..) else {
            break;
        };
        if remaining.len() >= UUID_LEN
            && let Some(candidate) = raw.get(cursor..cursor + UUID_LEN)
            && Uuid::parse_str(candidate).is_ok()
        {
            output.push_str("<opaque-uuid>");
            cursor += UUID_LEN;
            continue;
        }
        let Some(ch) = remaining.chars().next() else {
            break;
        };
        output.push(ch);
        cursor += ch.len_utf8();
    }
    output
}

pub(super) fn fact_memory_field(normalized: &str) -> bool {
    matches!(
        normalized,
        "goal_activated_memory_id"
            | "intervention_request_memory_id"
            | "intervention_decision_memory_id"
            | "wake_trace_memory_id"
            | "workspace_run_memory_id"
            | "workspace_review_memory_id"
            | "workspace_decision_memory_id"
            | "execution_request_memory_id"
            | "prior_execution_request_memory_id"
            | "message_memory_id"
            | "reply_memory_id"
    )
}

pub(super) fn perspective_memory_field(normalized: &str) -> bool {
    matches!(
        normalized,
        "root_perspective_memory_id" | "current_root_perspective_memory_id"
    )
}

pub(super) fn generic_memory_field(normalized: &str) -> bool {
    normalized == "memory_id" || normalized.ends_with("_memory_id")
}

pub(super) fn normalize_reference_key(key: &str) -> String {
    if let Some(stem) = key.strip_suffix("_ids_used") {
        format!("{stem}_id")
    } else if let Some(stem) = key.strip_suffix("_ids") {
        format!("{stem}_id")
    } else {
        key.strip_suffix('s').unwrap_or(key).to_string()
    }
}

pub(super) async fn load_payload_memory_classes(
    engine: &Engine,
    owner: &Owner,
    payload: &serde_json::Value,
) -> Result<HashMap<Uuid, MemoryHandleClass>, ProtocolError> {
    let mut ids = HashSet::new();
    collect_generic_memory_ids(payload, None, &mut ids);
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let mut req = QueryRequest::for_principal(owner.principal.clone());
    req.memory_ids = ids.into_iter().map(MemoryId::new).collect();
    req.limit = u32::try_from(req.memory_ids.len()).unwrap_or(u32::MAX);
    req.include_payloads = false;
    req.supersession = SupersessionStatus::IncludeSuperseded;
    let response = engine
        .storage()
        .query_memories(&req, engine.registry().list().as_slice())
        .await
        .map_err(|err| ProtocolError::internal(format!("query payload memory classes: {err}")))?;
    Ok(response
        .memories
        .into_iter()
        .filter_map(|memory| {
            memory_class_for_entity_kind(memory.kind).map(|class| (memory.id.into_inner(), class))
        })
        .collect())
}

pub(super) fn collect_generic_memory_ids(
    value: &serde_json::Value,
    key: Option<&str>,
    ids: &mut HashSet<Uuid>,
) {
    match value {
        serde_json::Value::String(raw) => {
            let Some(key) = key else {
                return;
            };
            let normalized = normalize_reference_key(key);
            if generic_memory_field(&normalized)
                && let Ok(uuid) = Uuid::parse_str(raw)
            {
                ids.insert(uuid);
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                collect_generic_memory_ids(value, key, ids);
            }
        }
        serde_json::Value::Object(map) => {
            for (field, value) in map {
                collect_generic_memory_ids(value, Some(field.as_str()), ids);
            }
        }
        _ => {}
    }
}

pub(super) fn memory_class_for_entity_kind(kind: EntityKind) -> Option<MemoryHandleClass> {
    match kind {
        EntityKind::Fact => Some(MemoryHandleClass::Fact),
        EntityKind::Abstraction => Some(MemoryHandleClass::Abstraction),
        EntityKind::Perspective => Some(MemoryHandleClass::Perspective),
        EntityKind::Goal => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continuation_preamble_uses_persisted_proxima_state_not_provider_state() {
        let handles = HandleTable::new();
        let continuation = FireWakeContinuation {
            intervention_decision_memory_id: MemoryId::new(Uuid::now_v7()),
            intervention_request_memory_id: MemoryId::new(Uuid::now_v7()),
            original_invocation_id: Uuid::now_v7(),
            original_change_event_seq: Uuid::now_v7(),
            wake_trace_memory_id: MemoryId::new(Uuid::now_v7()),
            original_triggering_memory_id: MemoryId::new(Uuid::now_v7()),
            grant_rounds: 3,
            rationale: "made progress".into(),
        };
        let seeded = crate::mcp::PreSeededHandles {
            triggering: handles.assign_fact_memory(MemoryId::new(Uuid::now_v7())),
            root_perspective: handles.assign_perspective_memory(MemoryId::new(Uuid::now_v7())),
            self_instance: handles.assign_personality(PersonalityInstanceId::new(Uuid::now_v7())),
            continuation_decision: Some(
                handles.assign_fact_memory(continuation.intervention_decision_memory_id),
            ),
            continuation_request: Some(
                handles.assign_fact_memory(continuation.intervention_request_memory_id),
            ),
            continuation_wake_trace: Some(
                handles.assign_fact_memory(continuation.wake_trace_memory_id),
            ),
            continuation_original_triggering: Some(
                handles.assign_fact_memory(continuation.original_triggering_memory_id),
            ),
        };

        let preamble = format_continuation_preamble(&seeded, &continuation);

        assert!(preamble.contains("persisted Proxima state"));
        assert!(preamble.contains("provider chat session state is not available"));
        assert!(preamble.contains("continuation.intervention_decision.handle"));
        assert!(preamble.contains("continuation.intervention_request.handle"));
        assert!(preamble.contains("continuation.prior_wake_trace.handle"));
        assert!(preamble.contains("continuation.original_triggering_memory.handle"));
        assert!(preamble.contains("granted_rounds: 3"));
        assert!(preamble.contains("supervisor_rationale: made progress"));
        assert!(!preamble.contains(&continuation.original_invocation_id.to_string()));
        assert!(!preamble.contains(&continuation.original_change_event_seq.to_string()));

        let params = continuation_context_params(&seeded, &continuation);
        assert_eq!(
            params["intervention_decision"]["handle"],
            seeded.continuation_decision.as_ref().unwrap().as_str()
        );
        assert_eq!(
            params["intervention_request"]["handle"],
            seeded.continuation_request.as_ref().unwrap().as_str()
        );
        assert_eq!(
            params["prior_wake_trace"]["handle"],
            seeded.continuation_wake_trace.as_ref().unwrap().as_str()
        );
        assert_eq!(
            params["original_triggering_memory"]["handle"],
            seeded
                .continuation_original_triggering
                .as_ref()
                .unwrap()
                .as_str()
        );
    }

    #[test]
    fn model_payload_projection_turns_reference_uuids_into_handles() {
        let handles = HandleTable::new();
        let goal_id = Uuid::now_v7();
        let memory_id = Uuid::now_v7();
        let repo_id = Uuid::now_v7();
        let projected = project_model_value(
            &serde_json::json!({
                "goal_id": goal_id,
                "goal_activated_memory_id": memory_id,
                "repo_id": repo_id,
            }),
            None,
            &handles,
            &HashMap::new(),
        );

        assert_eq!(projected["goal_id"], "G1");
        assert_eq!(projected["goal_activated_memory_id"], "F1");
        assert_eq!(projected["repo_id"], "R1");
        assert_eq!(
            handles
                .resolve_goal("G1")
                .expect("goal handle")
                .into_inner(),
            goal_id
        );
        assert_eq!(
            handles
                .resolve_memory("F1")
                .expect("memory handle")
                .into_inner(),
            memory_id
        );
        assert_eq!(
            handles
                .resolve_flavor_object("R1", "code/repository")
                .expect("repo handle"),
            repo_id
        );
    }

    #[test]
    fn model_payload_projection_preserves_generic_memory_handles_by_class() {
        let handles = HandleTable::new();
        let fact_id = Uuid::now_v7();
        let abstraction_id = Uuid::now_v7();
        let perspective_id = Uuid::now_v7();
        let unknown_id = Uuid::now_v7();
        let mut memory_classes = HashMap::new();
        memory_classes.insert(fact_id, MemoryHandleClass::Fact);
        memory_classes.insert(abstraction_id, MemoryHandleClass::Abstraction);
        memory_classes.insert(perspective_id, MemoryHandleClass::Perspective);

        let projected = project_model_value(
            &serde_json::json!({
                "context_memory_ids": [fact_id, abstraction_id, perspective_id],
                "context_memory_ids_used": [abstraction_id],
                "unrelated_memory_id": unknown_id,
            }),
            None,
            &handles,
            &memory_classes,
        );

        assert_eq!(projected["context_memory_ids"][0], "F1");
        assert_eq!(projected["context_memory_ids"][1], "A1");
        assert_eq!(projected["context_memory_ids"][2], "P1");
        assert_eq!(projected["context_memory_ids_used"][0], "A1");
        assert_eq!(projected["unrelated_memory_id"], "<opaque-memory-uuid>");
    }

    #[test]
    fn model_payload_projection_redacts_embedded_uuid_substrings() {
        let handles = HandleTable::new();
        let raw_uuid = Uuid::now_v7();
        let projected = project_model_value(
            &serde_json::json!({
                "worktree_path": format!("/tmp/worktrees/{raw_uuid}/repo"),
                "branch_name": format!("proxima/wake/{raw_uuid}"),
            }),
            None,
            &handles,
            &HashMap::new(),
        );

        assert_eq!(
            projected["worktree_path"],
            "/tmp/worktrees/<opaque-uuid>/repo"
        );
        assert_eq!(projected["branch_name"], "proxima/wake/<opaque-uuid>");
    }
}
