use super::*;

mod manifest_tests {
    use std::{
        collections::{BTreeMap, BTreeSet},
        sync::Arc,
    };

    use super::*;
    use crate::{
        AuthPath, AuthzContext, EdgeId, FlavorRegistry, GoalId, MemoryId, OwnerRef, UserId,
    };

    #[test]
    fn provider_safe_tool_name_replaces_runner_invalid_separators() {
        assert_eq!(
            provider_safe_tool_name("core/emit_abstraction"),
            "core_emit_abstraction"
        );
        assert_eq!(provider_safe_tool_name("core_remember"), "core_remember");
        assert_eq!(provider_safe_tool_name("a..b"), "a._b");
    }

    #[test]
    fn core_resources_manifest_has_expected_shape() {
        let resources = all_core_resources().collect::<Vec<_>>();

        assert_eq!(resources.len(), 7);
        assert_eq!(
            resources
                .iter()
                .filter(|resource| !resource.is_template)
                .count(),
            4
        );
        assert_eq!(
            resources
                .iter()
                .filter(|resource| resource.is_template)
                .count(),
            3
        );
        assert!(
            resources
                .iter()
                .all(|resource| resource.scope_key.starts_with("resource:"))
        );
    }

    #[test]
    fn protocol_errors_map_to_json_rpc_error_classes() {
        let forbidden = McpToolError::from(crate::error::ProtocolError::forbidden(
            "source ingest denied",
        ));
        assert_eq!(forbidden.kind(), McpToolErrorKind::InvalidRequest);
        assert!(
            forbidden.to_string().contains("source ingest denied"),
            "message: {forbidden}"
        );

        let invalid = McpToolError::from(crate::error::ProtocolError::invalid_argument(
            "fact",
            "expected Fact id",
        ));
        assert_eq!(invalid.kind(), McpToolErrorKind::InvalidInput);
        assert!(
            invalid.to_string().contains("expected Fact id"),
            "message: {invalid}"
        );
    }

    #[test]
    fn internal_tool_error_client_message_is_generic() {
        let storage = McpToolError::from(crate::StorageError::Internal(
            "postgres password=secret".into(),
        ));
        assert_eq!(storage.kind(), McpToolErrorKind::Internal);
        assert_eq!(storage.client_message(), "internal server error");

        let invalid = McpToolError::InvalidInput("expected Fact id".into());
        assert_eq!(invalid.client_message(), "invalid input: expected Fact id");
    }

    #[test]
    fn core_actions_manifest_is_internally_consistent() {
        let allowed_tools = BTreeSet::from(["core_goal", "core_fact", "core_membership"]);
        let expected_counts = BTreeMap::from([
            ("core_goal", 5_usize),
            ("core_fact", 4),
            ("core_membership", 3),
        ]);
        let mut seen_scope_keys = BTreeSet::new();
        let mut counts = BTreeMap::<&'static str, usize>::new();

        for meta in all_core_actions() {
            assert!(
                seen_scope_keys.insert(meta.scope_key),
                "duplicate scope_key {}",
                meta.scope_key
            );
            assert_eq!(
                meta.scope_key,
                format!("{}:{}", meta.tool, meta.action),
                "scope_key must equal <tool>:<action>"
            );
            assert!(
                allowed_tools.contains(meta.tool),
                "unexpected tool {}",
                meta.tool
            );
            *counts.entry(meta.tool).or_default() += 1;
        }

        assert_eq!(counts, expected_counts);
    }

    #[tokio::test]
    async fn dispatcher_rejects_cross_action_goal_fields_before_execution() {
        let ctx = prefixed_ctx();
        let goal = ctx.format_goal(GoalId::new(uuid::Uuid::now_v7()));
        let desc = ctx
            .registry
            .list_mcp_tools()
            .iter()
            .find(|tool| tool.name == "core_goal")
            .expect("core_goal registered");

        let err = (desc.call)(
            ctx,
            serde_json::json!({
                "action": "transition",
                "goal": goal,
                "transition": "pause",
                "title": "belongs to set/modify",
            }),
        )
        .await
        .expect_err("foreign action field must be rejected before execution");
        assert_eq!(err.kind(), McpToolErrorKind::InvalidInput);
        let message = err.to_string();
        assert!(message.contains("title"), "message: {message}");
        assert!(message.contains("transition"), "message: {message}");
    }

    #[tokio::test]
    async fn dispatcher_rejects_cross_action_fact_fields_before_execution() {
        let ctx = prefixed_ctx();
        let fact = ctx.format_fact_memory(MemoryId::new(uuid::Uuid::now_v7()));
        let desc = ctx
            .registry
            .list_mcp_tools()
            .iter()
            .find(|tool| tool.name == "core_fact")
            .expect("core_fact registered");

        let err = (desc.call)(
            ctx,
            serde_json::json!({
                "action": "citation_of_fact",
                "fact": fact,
                "confirm": true,
                "expect_handle": "F:wrong",
            }),
        )
        .await
        .expect_err("foreign action fields must be rejected before execution");
        assert_eq!(err.kind(), McpToolErrorKind::InvalidInput);
        let message = err.to_string();
        assert!(message.contains("confirm"), "message: {message}");
        assert!(message.contains("expect_handle"), "message: {message}");
        assert!(message.contains("citation_of_fact"), "message: {message}");
    }

    #[tokio::test]
    async fn prefixed_ids_round_trip_through_ctx_helpers() {
        let ctx = prefixed_ctx();
        let fact = MemoryId::new(uuid::Uuid::now_v7());
        let abstraction = MemoryId::new(uuid::Uuid::now_v7());
        let perspective = MemoryId::new(uuid::Uuid::now_v7());
        let goal = GoalId::new(uuid::Uuid::now_v7());
        let edge = EdgeId::new(uuid::Uuid::now_v7());

        let fact_ref = ctx.format_fact_memory(fact);
        let abstraction_ref = ctx.format_abstraction_memory(abstraction);
        let perspective_ref = ctx.format_perspective_memory(perspective);
        let goal_ref = ctx.format_goal(goal);
        let edge_ref = ctx.format_edge(edge);

        assert_prefixed_uuid(&fact_ref, 'F');
        assert_prefixed_uuid(&abstraction_ref, 'A');
        assert_prefixed_uuid(&perspective_ref, 'P');
        assert_prefixed_uuid(&goal_ref, 'G');
        assert_prefixed_uuid(&edge_ref, 'E');

        assert_eq!(ctx.resolve_fact_memory(&fact_ref).expect("fact"), fact);
        assert_eq!(
            ctx.resolve_abstraction_memory(&abstraction_ref)
                .expect("abstraction"),
            abstraction
        );
        assert_eq!(
            ctx.resolve_perspective_memory(&perspective_ref)
                .expect("perspective"),
            perspective
        );
        assert_eq!(ctx.resolve_memory(&fact_ref).expect("any fact"), fact);
        assert_eq!(
            ctx.resolve_memory(&abstraction_ref)
                .expect("any abstraction"),
            abstraction
        );
        assert_eq!(
            ctx.resolve_memory(&perspective_ref)
                .expect("any perspective"),
            perspective
        );
        assert_eq!(ctx.resolve_goal(&goal_ref).expect("goal"), goal);
        assert_eq!(ctx.resolve_edge(&edge_ref).expect("edge"), edge);
    }

    #[tokio::test]
    async fn prefixed_ids_ctx_rejects_wrong_class() {
        let ctx = prefixed_ctx();
        let fact = ctx.format_fact_memory(MemoryId::new(uuid::Uuid::now_v7()));
        let err = ctx.resolve_goal(&fact).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("expected Goal id"), "message: {msg}");
        assert!(msg.contains("got prefix 'F'"), "message: {msg}");
    }

    fn prefixed_ctx() -> McpToolCtx {
        let owner = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
        McpToolCtx {
            owner,
            authz: AuthzContext::single_owner(&owner, AuthPath::System),
            handles: None,
            mode: OutputMode::PrefixedIds,
            registry: Arc::new(FlavorRegistry::new().freeze_or_panic_for_tests()),
            author: McpAuthorContext {
                model_id: "t".into(),
                client_name: "t".into(),
                client_version: "0".into(),
                caller_self_perspective: None,
            },
            caller_self_perspective: None,
            master_token_id: None,
            extensions: McpToolExtensions::default(),
            engine: None,
        }
    }

    fn assert_prefixed_uuid(raw: &str, expected_prefix: char) {
        let (prefix, uuid_part) = raw.split_once(':').expect("prefixed uuid");
        let mut expected = [0; 4];
        assert_eq!(prefix, expected_prefix.encode_utf8(&mut expected));
        uuid::Uuid::parse_str(uuid_part).expect("uuid body");
    }
}

mod ctx_engine_tests {
    use super::*;
    use crate::{AuthPath, AuthzContext};
    use crate::{Engine, FlavorRegistry, OwnerRef, UserId};
    use std::sync::Arc;

    #[tokio::test]
    async fn ctx_engine_returns_none_when_unwired() {
        let owner = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
        let ctx = McpToolCtx {
            owner,
            authz: AuthzContext::single_owner(&owner, AuthPath::System),
            handles: Some(Arc::new(HandleTable::new())),
            mode: OutputMode::Handles,
            registry: Arc::new(FlavorRegistry::new().freeze_or_panic_for_tests()),
            author: McpAuthorContext {
                model_id: "t".into(),
                client_name: "t".into(),
                client_version: "0".into(),
                caller_self_perspective: None,
            },
            caller_self_perspective: None,
            master_token_id: None,
            extensions: McpToolExtensions::default(),
            engine: None,
        };
        assert!(ctx.engine().is_none());
    }

    #[tokio::test]
    async fn ctx_engine_returns_some_when_wired() {
        let owner = OwnerRef::Personal(UserId::new(uuid::Uuid::now_v7()));
        let engine = Arc::new(Engine::new(
            FlavorRegistry::new().freeze_or_panic_for_tests(),
        ));
        let ctx = McpToolCtx {
            owner,
            authz: AuthzContext::single_owner(&owner, AuthPath::System),
            handles: Some(Arc::new(HandleTable::new())),
            mode: OutputMode::Handles,
            registry: Arc::new(FlavorRegistry::new().freeze_or_panic_for_tests()),
            author: McpAuthorContext {
                model_id: "t".into(),
                client_name: "t".into(),
                client_version: "0".into(),
                caller_self_perspective: None,
            },
            caller_self_perspective: None,
            master_token_id: None,
            extensions: McpToolExtensions::default(),
            engine: Some(engine.clone()),
        };
        assert!(ctx.engine().is_some());
    }
}
