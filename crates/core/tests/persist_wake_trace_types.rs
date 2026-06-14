use proxima_core::verbs::persist_wake_trace::{
    WakeTracePayload, WakeTracePersistInput, WakeTracePersistOutcome,
};
use proxima_core::{GoalId, MemoryId, OrgId, Owner, Principal, SourceBatchId, SourceId, UserId};
use uuid::Uuid;

#[test]
fn input_carries_jsonl_bytes_authoring_instance_and_provenance_targets() {
    let bytes = b"{\"record\":\"start\"}\n".to_vec();
    let hash: [u8; 32] = *blake3::hash(&bytes).as_bytes();
    let now = time::OffsetDateTime::now_utc();

    let input = WakeTracePersistInput {
        owner: test_owner(),
        authoring_personality_instance_id: Uuid::now_v7(),
        root_perspective_memory_id: MemoryId::new(Uuid::now_v7()),
        triggering_memory_id: MemoryId::new(Uuid::now_v7()),
        active_goal_ids: vec![GoalId::new(Uuid::now_v7())],
        jsonl_bytes: bytes.clone(),
        jsonl_content_hash: hash,
        jsonl_line_count: 1,
        jsonl_truncated: false,
        citation_byte_range: Some((0, bytes.len() as u64)),
        wake_trace: sample_wake_trace(),
        source_id: SourceId::new("test/wake-trace"),
        source_batch_id: SourceBatchId::new(Uuid::now_v7()),
        observed_at: now,
        occurred_at: now,
    };

    let _: Box<dyn Send + Sync> = Box::new(input.clone());
    assert_eq!(input.jsonl_bytes, bytes);
    assert_eq!(input.jsonl_content_hash, hash);
    assert_ne!(input.event_id().as_bytes(), &[0u8; 32]);

    let outcome = sample_outcome();
    let _ = outcome.event_id;
    let _ = outcome.fact_memory_id;
    let _ = outcome.cited_object_id;
    let _ = outcome.citation_mapping_id;
    let _ = outcome.change_event_seq;
    let _: bool = outcome.idempotent_replay;
}

fn test_owner() -> Owner {
    Owner {
        principal: Principal::User(UserId::new(Uuid::now_v7())),
        org_id: OrgId::new(Uuid::now_v7()),
    }
}

fn sample_wake_trace() -> WakeTracePayload {
    let now = time::OffsetDateTime::now_utc();
    WakeTracePayload {
        invocation_id: Uuid::now_v7(),
        wake_entry_id: Uuid::now_v7(),
        personality_instance_id: Uuid::now_v7(),
        model_target_ref: "test-target".into(),
        model_id: "test-model".into(),
        started_at: now,
        finished_at: now,
        outcome_kind: proxima_core::WakeTraceOutcomeKind::Succeeded,
        failure_reason: None,
        rounds_used: 1,
        finish_reason: Some("stop".into()),
        total_prompt_tokens: Some(10),
        total_completion_tokens: Some(3),
        tool_call_count: 0,
        jsonl_truncated: false,
    }
}

fn sample_outcome() -> WakeTracePersistOutcome {
    WakeTracePersistOutcome {
        event_id: proxima_core::EventId::new([1u8; 32]),
        fact_memory_id: MemoryId::new(Uuid::now_v7()),
        cited_object_id: Uuid::now_v7(),
        citation_mapping_id: Uuid::now_v7(),
        change_event_seq: Uuid::now_v7(),
        idempotent_replay: false,
    }
}
