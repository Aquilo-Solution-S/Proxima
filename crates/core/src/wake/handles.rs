//! Wake-bootstrap handle pre-seeding.
//!
//! Registers the three entities the wake context already names with
//! the wake's `HandleTable`. The returned `PreSeededHandles` struct
//! is the model's contract for round 1 — `{triggering,
//! root_perspective, self_instance}` resolve before any tool call.

use crate::mcp::PreSeededHandles;
use crate::wake::token_store::WakeTokenContext;

#[must_use]
pub fn pre_seed_wake_handles(ctx: &WakeTokenContext) -> PreSeededHandles {
    let triggering = ctx.handles.assign_memory_with_class(
        ctx.triggering_event_memory_id,
        ctx.triggering_event_memory_class,
    );
    let root_perspective = ctx.handles.assign_memory_with_class(
        ctx.current_root_perspective_memory_id,
        ctx.current_root_perspective_memory_class,
    );
    let self_instance = ctx
        .handles
        .assign_personality(ctx.personality_instance_id());
    PreSeededHandles {
        triggering,
        root_perspective,
        self_instance,
        continuation_decision: None,
        continuation_request: None,
        continuation_wake_trace: None,
        continuation_original_triggering: None,
    }
}

/// Format the round-1 wake-context preamble. Reads handle strings from
/// `PreSeededHandles` — never hard-codes `F1`/`P1`/`I1`.
///
/// The preamble is prepended to the personality's `system_prompt` by
/// the wake bootstrap. It names the three handles the model can rely
/// on being addressable in round 1.
#[must_use]
pub fn format_wake_context_preamble(
    seeded: &PreSeededHandles,
    triggering_schema_id: Option<&str>,
    triggering_kind: &str,
) -> String {
    let kind_clause = match triggering_kind {
        "Fact" => "Fact memory",
        "Abstraction" => "Abstraction memory",
        "Perspective" => "Perspective memory",
        _ => "memory",
    };
    let schema_clause = triggering_schema_id.filter(|s| !s.is_empty()).map_or_else(
        || format!("a {kind_clause}"),
        |s| format!("a `{s}` {kind_clause}"),
    );
    format!(
        "You were woken by {triggering}, {schema_clause}. \
Your current root perspective is {root}. You are {self_p}. \
Use handles according to the kind labels in wake context and tool schemas.\n\n",
        triggering = seeded.triggering.as_str(),
        root = seeded.root_perspective.as_str(),
        self_p = seeded.self_instance.as_str(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::HandleTable;
    use crate::personality::WakeChainDepth;
    use crate::wake::token_store::WakeTokenContext;
    use crate::{EdgeId, GoalId, MemoryId, OrgId, Owner, PersonalityInstanceId, Principal, UserId};
    use std::sync::Arc;
    use uuid::Uuid;

    fn make_ctx() -> WakeTokenContext {
        let triggering = MemoryId::new(Uuid::now_v7());
        let root = MemoryId::new(Uuid::now_v7());
        let pid = PersonalityInstanceId::new(Uuid::now_v7());
        WakeTokenContext {
            invocation_id: Uuid::now_v7(),
            personality_instance_id: pid.into_inner(),
            wake_entry_id: Uuid::now_v7(),
            change_event_seq: Uuid::now_v7(),
            owner: Owner {
                principal: Principal::User(UserId::new(Uuid::now_v7())),
                org_id: OrgId::new(Uuid::now_v7()),
            },
            palette: Vec::new(),
            model_id: "test/model".into(),
            max_rounds: 16,
            current_root_perspective_memory_id: root,
            current_root_perspective_memory_class: crate::mcp::MemoryHandleClass::Perspective,
            triggering_event_memory_id: triggering,
            triggering_event_memory_class: crate::mcp::MemoryHandleClass::Fact,
            triggering_event_depth: WakeChainDepth::new(0),
            read_log: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            handles: Arc::new(HandleTable::new()),
        }
    }

    #[test]
    fn pre_seed_assigns_f1_p1_i1_at_construction_time() {
        let ctx = make_ctx();
        let seeded = pre_seed_wake_handles(&ctx);
        assert_eq!(seeded.triggering.as_str(), "F1");
        assert_eq!(seeded.root_perspective.as_str(), "P1");
        assert_eq!(seeded.self_instance.as_str(), "I1");
    }

    #[test]
    fn pre_seed_struct_survives_n_subsequent_assignments() {
        let ctx = make_ctx();
        let triggering = ctx.triggering_event_memory_id;
        let seeded = pre_seed_wake_handles(&ctx);

        for _ in 0..32 {
            let _ = ctx
                .handles
                .assign_fact_memory(MemoryId::new(Uuid::now_v7()));
            let _ = ctx.handles.assign_edge(EdgeId::new(Uuid::now_v7()));
            let _ = ctx.handles.assign_goal(GoalId::new(Uuid::now_v7()));
        }

        let resolved = ctx
            .handles
            .resolve_memory(seeded.triggering.as_str())
            .expect("triggering handle still resolves");
        assert_eq!(resolved, triggering);
    }

    #[test]
    fn pre_seed_is_idempotent() {
        let ctx = make_ctx();
        let first = pre_seed_wake_handles(&ctx);
        let second = pre_seed_wake_handles(&ctx);
        assert_eq!(first.triggering, second.triggering);
        assert_eq!(first.root_perspective, second.root_perspective);
        assert_eq!(first.self_instance, second.self_instance);
    }

    #[test]
    fn preamble_uses_pre_seeded_handles() {
        let ctx = make_ctx();
        let seeded = pre_seed_wake_handles(&ctx);
        let preamble = format_wake_context_preamble(&seeded, None, "Fact");
        assert!(preamble.contains(seeded.triggering.as_str()));
        assert!(preamble.contains(seeded.root_perspective.as_str()));
        assert!(preamble.contains(seeded.self_instance.as_str()));
    }

    #[test]
    fn preamble_includes_triggering_schema_when_provided() {
        let ctx = make_ctx();
        let seeded = pre_seed_wake_handles(&ctx);
        let preamble =
            format_wake_context_preamble(&seeded, Some("proxima-goal/goal-activated-v1"), "Fact");
        assert!(preamble.contains("proxima-goal/goal-activated-v1"));
    }

    #[test]
    fn preamble_uses_triggering_memory_kind() {
        let ctx = make_ctx();
        let seeded = pre_seed_wake_handles(&ctx);
        let preamble = format_wake_context_preamble(
            &seeded,
            Some("proxima-goal/goal-activated-v1"),
            "Abstraction",
        );
        assert!(preamble.contains("Abstraction memory"));
        assert!(!preamble.contains("goal-activated-v1` Fact"));
    }

    #[test]
    fn preamble_omits_schema_clause_when_empty() {
        let ctx = make_ctx();
        let seeded = pre_seed_wake_handles(&ctx);
        let with = format_wake_context_preamble(&seeded, Some(""), "Fact");
        let without = format_wake_context_preamble(&seeded, None, "Fact");
        assert_eq!(with, without);
    }

    #[test]
    fn preamble_does_not_hardcode_f1_i1() {
        let ctx = make_ctx();
        // Mint unrelated entities first to perturb counter state so the
        // pre-seed handles aren't F1/P1/I1.
        for _ in 0..5 {
            let _ = ctx
                .handles
                .assign_fact_memory(MemoryId::new(Uuid::now_v7()));
        }
        let seeded = pre_seed_wake_handles(&ctx);
        let preamble = format_wake_context_preamble(&seeded, None, "Fact");
        assert!(preamble.contains(seeded.triggering.as_str()));
        assert_ne!(seeded.triggering.as_str(), "F1");
        assert!(!preamble.contains(" F1 "));
        assert!(!preamble.contains(" F1."));
        assert!(!preamble.contains(" F1,"));
    }
}
