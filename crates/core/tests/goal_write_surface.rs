use proxima_core::mcp::{McpActionArgSpec, McpTool, McpToolAnnotations, McpToolCtx, McpToolError};
use proxima_core::protocol::{action as protocol_action, tool as protocol_tool};
use proxima_core::verbs::goal_write::{
    GoalAssignmentTarget, GoalAuthorship, GoalCreateRequest, GoalPayloadWrite, GoalWakeConfigWrite,
    GoalWakeToolId, GoalWakeTrigger, GoalWriteBuildError, IdempotencyKey,
};
use proxima_core::{
    FlavorRegistry, GoalPayload, MemoryId, Owner, OwnerRef, PayloadKeyBuilder, SchemaId,
    SchemaVersion, TrimmedLenViolation, UserId,
};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct ProductGoalPayload {
    stable_key: String,
}

impl GoalPayload for ProductGoalPayload {
    const SCHEMA_ID: &'static str = "test/product-goal-v1";
    const SCHEMA_VERSION: u32 = 7;

    fn goal_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_str("stable_key", &self.stable_key);
        key.finish()
    }
}

#[test]
fn typed_goal_payload_write_uses_goal_key_and_sidecar_metadata() {
    let payload = ProductGoalPayload {
        stable_key: "onboarding:first-goal".to_string(),
    };
    let expected_key = payload.goal_key();

    let write = GoalPayloadWrite::from_payload("  First goal  ", "  Learn daily  ", payload)
        .expect("valid typed goal payload write");

    assert_eq!(write.schema_id.as_str(), ProductGoalPayload::SCHEMA_ID);
    assert_eq!(write.schema_version, SchemaVersion::new(7));
    assert_eq!(write.title, "First goal");
    assert_eq!(write.text, "Learn daily");
    assert_eq!(write.payload, expected_key);

    let sidecar = write.sidecar_payload.as_ref().expect(
        "typed product goals carry a sidecar payload for storage backends that registered one",
    );
    assert_eq!(sidecar.schema_id.as_str(), ProductGoalPayload::SCHEMA_ID);
    assert_eq!(sidecar.schema_version, SchemaVersion::new(7));
}

#[test]
fn typed_goal_payload_write_rejects_invalid_display_fields() {
    let err = GoalPayloadWrite::from_payload(
        " ",
        "body",
        ProductGoalPayload {
            stable_key: "k".to_string(),
        },
    )
    .expect_err("empty title rejected");
    assert_eq!(
        err,
        GoalWriteBuildError::InvalidTitle(TrimmedLenViolation::Blank)
    );

    let err = GoalPayloadWrite::from_payload(
        "title",
        " ",
        ProductGoalPayload {
            stable_key: "k".to_string(),
        },
    )
    .expect_err("empty text rejected");
    assert_eq!(
        err,
        GoalWriteBuildError::InvalidText(TrimmedLenViolation::Blank)
    );
}

/// Every embedding-host length rejection in this file must tell a blank
/// value from an over-long one. The four checks below were written as
/// `count == 0 || count > max` behind a single sentence, so a host handed
/// `"  "` was told `goal title must be 1..=240 chars` — a range two spaces
/// satisfy, which reads as a server fault rather than "send a title".
///
/// The MCP tools are guarded by `a_length_rejection_names_the_bound_that_was
/// _broken` in `agent_memory_tools_pg.rs`; this is the same property one
/// layer down, where the only caller is a host embedding the crate.
#[test]
fn an_embedded_host_is_told_which_bound_it_broke() {
    let payload = || ProductGoalPayload {
        stable_key: "k".to_string(),
    };
    let long = "a".repeat(20_001);

    let cases: [(&str, String, String); 4] = [
        (
            "goal title",
            GoalPayloadWrite::from_payload("  ", "text", payload())
                .expect_err("blank title")
                .to_string(),
            GoalPayloadWrite::from_payload("a".repeat(241), "text", payload())
                .expect_err("long title")
                .to_string(),
        ),
        (
            "goal text",
            GoalPayloadWrite::from_payload("title", "  ", payload())
                .expect_err("blank text")
                .to_string(),
            GoalPayloadWrite::from_payload("title", &long, payload())
                .expect_err("long text")
                .to_string(),
        ),
        (
            "idempotency_key",
            IdempotencyKey::new("  ").expect_err("blank key"),
            IdempotencyKey::new("a".repeat(IdempotencyKey::MAX_CHARS + 1)).expect_err("long key"),
        ),
        (
            "source_batch_key",
            IdempotencyKey::new_named("source_batch_key", "  ").expect_err("blank key"),
            IdempotencyKey::new_named(
                "source_batch_key",
                "a".repeat(IdempotencyKey::MAX_CHARS + 1),
            )
            .expect_err("long key"),
        ),
    ];

    for (field, blank, over) in cases {
        assert_ne!(
            blank, over,
            "{field}: one message for two mistakes tells neither",
        );
        assert!(
            blank.starts_with(field) && over.starts_with(field),
            "{field}: both messages must name the field: {blank} / {over}",
        );
        assert!(
            !blank.contains(char::is_numeric),
            "{field}: a blank value must not be quoted a bound it meets: {blank}",
        );
    }
}

/// The wake prompt takes the same path but reports through `ProtocolError`,
/// which prefixes `invalid argument {field}: `, so it is checked apart from
/// the four above rather than bent into their shape.
#[test]
fn a_wake_prompt_rejection_names_the_bound_it_broke() {
    let long = "a".repeat(GoalWakeConfigWrite::MAX_PROMPT_CHARS + 1);
    let refuse = |prompt: &str| {
        GoalWakeConfigWrite::new(
            GoalWakeTrigger::FactMemory {
                memory_id: MemoryId::new(uuid::Uuid::now_v7()),
            },
            vec![],
            prompt,
            &[] as &[MemoryId],
        )
        .expect_err("prompt rejected before the empty toolset is looked at")
        .message
    };

    let blank = refuse("   ");
    let over = refuse(&long);
    assert_ne!(blank, over, "one message for two mistakes tells neither");
    assert!(
        !blank.contains("20000") && !blank.contains("20001"),
        "a blank prompt must not be quoted a bound it meets: {blank}",
    );
    assert!(
        over.contains("20000") && over.contains("20001"),
        "an over-long prompt must be told the cap and what it sent: {over}",
    );
}

/// `GoalWakeToolId::parse` bounded `value.len()` — bytes — behind a message
/// that said "characters". A 120-character Cyrillic id is 240 bytes, so it
/// was refused for exceeding a limit it was nowhere near in the unit the
/// message named.
#[test]
fn a_wake_tool_id_is_bounded_in_the_unit_its_message_names() {
    let mut registry = FlavorRegistry::default();
    let frozen = std::mem::take(&mut registry).freeze_or_panic_for_tests();
    let cyrillic = "я".repeat(120);
    assert_eq!(cyrillic.len(), 240, "two bytes per char");

    let err = GoalWakeToolId::parse(&cyrillic, &frozen).expect_err("not a registered tool");
    assert!(
        !err.message.contains("200"),
        "a 120-character id must not be refused for exceeding 200 characters: {}",
        err.message,
    );
}

#[test]
fn product_goal_create_request_defaults_to_user_authorship_and_explicit_self_target() {
    let owner: Owner = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
    let target_self = MemoryId::new(uuid::Uuid::now_v7());
    let request_id = IdempotencyKey::new("onboarding:initial-goal:1").expect("stable key");

    let target = GoalAssignmentTarget::perspective(target_self);
    let request = GoalCreateRequest::product(
        owner,
        target,
        request_id,
        "Initial goal",
        "Practice every weekday",
        ProductGoalPayload {
            stable_key: "weekday-practice".to_string(),
        },
    );

    assert_eq!(request.owner, owner);
    assert_eq!(request.topology.assignment(), target);
    assert_eq!(request.author_self_perspective_id, None);
    assert!(request.topology.evidence().is_empty());
    assert!(request.topology.dependencies().is_empty());
    assert_eq!(request.authorship, GoalAuthorship::User);
}

#[test]
fn goal_wake_tool_id_requires_leaf_scope_for_grouped_core_tools() {
    let registry = FlavorRegistry::new().freeze_or_panic_for_tests();

    let err = GoalWakeToolId::parse(protocol_tool::CORE_GOAL, &registry)
        .expect_err("grouped action-dispatch tool requires an exact leaf scope key");
    assert!(err.message.contains("leaf action scope required"));

    let leaf = GoalWakeToolId::parse(protocol_action::CORE_GOAL_SET, &registry)
        .expect("registered leaf action scope key is valid");
    assert_eq!(leaf.as_str(), protocol_action::CORE_GOAL_SET);

    let flat = GoalWakeToolId::parse(protocol_tool::CORE_SEARCH_MEMORIES, &registry)
        .expect("flat registered non-action tool is valid");
    assert_eq!(flat.as_str(), protocol_tool::CORE_SEARCH_MEMORIES);
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
#[expect(
    dead_code,
    reason = "the derived schema is the subject, not the values"
)]
enum StubDispatchArgs {
    Look {
        #[schemars(description = "Which thing to look at.")]
        id: String,
    },
}

#[derive(Debug)]
struct StubDispatchTool;

impl McpTool for StubDispatchTool {
    const NAME: &'static str = "proxima-stub_dispatch";
    const DESCRIPTION: &'static str = "A flavor dispatcher.";
    // A write, because a flavor dispatcher has nowhere to put a per-action
    // annotation and `try_freeze` refuses `read_only(true)` at tool level for
    // that reason (docs/08 §Freeze Guards). Nothing below reads this; the
    // subject here is leaf parsing.
    const ANNOTATIONS: Option<McpToolAnnotations> =
        Some(McpToolAnnotations::new().read_only(false).open_world(false));
    const ACTION_ARG_SPECS: &'static [McpActionArgSpec] = &[McpActionArgSpec {
        action: "look",
        allowed_fields: &["id"],
        required_fields: &["id"],
        annotations: None,
    }];
    type Args = StubDispatchArgs;
    type Output = ();

    fn call(
        _ctx: McpToolCtx,
        _args: Self::Args,
    ) -> futures::future::BoxFuture<'static, Result<(), McpToolError>> {
        Box::pin(async { Ok(()) })
    }
}

/// A wake config may name a flavor dispatcher's leaf, not a bare tool id
/// that would grant the whole dispatcher.
#[test]
fn goal_wake_tool_id_accepts_a_flavor_dispatcher_leaf() {
    let mut registry = FlavorRegistry::new();
    registry.add_mcp_tool_or_panic_for_tests::<StubDispatchTool>("proxima-stub");
    let registry = registry.freeze_or_panic_for_tests();

    let err = GoalWakeToolId::parse(StubDispatchTool::NAME, &registry)
        .expect_err("a dispatcher's bare name is not a wake target");
    assert!(err.message.contains("leaf action scope required"));

    let leaf = GoalWakeToolId::parse(format!("{}:look", StubDispatchTool::NAME), &registry)
        .expect("a declared flavor action leaf is valid");
    assert_eq!(leaf.as_str(), "proxima-stub_dispatch:look");

    assert!(
        GoalWakeToolId::parse(format!("{}:vanish", StubDispatchTool::NAME), &registry).is_err(),
        "an action the tool does not declare is not a wake target",
    );
}

#[test]
fn goal_wake_config_normalizes_tool_ids_and_rejects_duplicate_hard_memory() {
    let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
    let search =
        GoalWakeToolId::parse(protocol_tool::CORE_SEARCH_MEMORIES, &registry).expect("valid tool");
    let goal_set = GoalWakeToolId::parse(protocol_action::CORE_GOAL_SET, &registry)
        .expect("valid leaf action");
    let hard_memory = MemoryId::new(uuid::Uuid::now_v7());

    let config = GoalWakeConfigWrite::new(
        GoalWakeTrigger::FactSchema {
            schema_id: SchemaId::new("core/agent-note-v1".into()),
            schema_version: SchemaVersion::new(1),
        },
        vec![goal_set.clone(), search.clone(), search],
        "  wake prompt  ",
        &[hard_memory],
    )
    .expect("valid wake config");
    assert_eq!(
        config
            .tool_ids()
            .iter()
            .map(GoalWakeToolId::as_str)
            .collect::<Vec<_>>(),
        [
            protocol_action::CORE_GOAL_SET,
            protocol_tool::CORE_SEARCH_MEMORIES
        ]
    );
    assert_eq!(config.prompt(), "wake prompt");

    let err = GoalWakeConfigWrite::new(
        GoalWakeTrigger::FactSchema {
            schema_id: SchemaId::new("core/agent-note-v1".into()),
            schema_version: SchemaVersion::new(1),
        },
        vec![goal_set],
        "wake prompt",
        &[hard_memory, hard_memory],
    )
    .expect_err("duplicate hard memory ids are rejected");
    assert!(err.message.contains("duplicate hard memory id"));
}
