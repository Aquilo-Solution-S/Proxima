use super::*;

mod manifest_tests {
    use std::{collections::BTreeSet, sync::Arc};

    use super::*;
    use crate::protocol::resource as protocol_resource;
    use crate::protocol::tool as protocol_tool;
    use crate::{
        AuthPath, AuthzContext, FlavorRegistry, FlavorServices, GoalId, MemoryId, OwnerId, UserId,
    };

    #[test]
    fn resource_constants_match_manifest_scope_keys() {
        let expected = BTreeSet::from([
            protocol_resource::SCHEMAS,
            protocol_resource::SCHEMA,
            protocol_resource::TOOLS,
            protocol_resource::GRAPH,
            protocol_resource::MEMORY,
            protocol_resource::MEMORIES,
            protocol_resource::MEMORY_COUNTS,
            protocol_resource::MEMORY_LINEAGE,
            protocol_resource::GOALS,
            protocol_resource::GOAL,
        ]);
        let actual = all_core_resources()
            .map(|resource| resource.scope_key)
            .collect::<BTreeSet<_>>();

        assert_eq!(actual, expected);
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

    #[tokio::test]
    async fn dispatcher_rejects_cross_action_goal_fields_before_execution() {
        let ctx = wire_ctx();
        let goal = (GoalId::new(uuid::Uuid::now_v7())).into_inner().to_string();
        let desc = ctx
            .registry
            .list_mcp_tools()
            .iter()
            .find(|tool| tool.name == protocol_tool::CORE_GOAL)
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
    async fn dispatcher_refuses_a_pending_that_does_not_fit_the_transition() {
        let desc_ctx = wire_ctx();
        let desc = desc_ctx
            .registry
            .list_mcp_tools()
            .iter()
            .find(|tool| tool.name == protocol_tool::CORE_GOAL)
            .expect("core_goal registered");
        // None of these reaches the engine (the test context has none): each
        // is refused where the argument is parsed.
        for (transition, pending, named) in [
            ("pause", None, vec!["keep", "discard"]),
            (
                "resume",
                Some("keep"),
                vec!["only when transition is pause"],
            ),
            (
                "abandon",
                Some("discard"),
                vec!["only when transition is pause"],
            ),
            ("pause", Some("maybe"), vec!["maybe", "keep"]),
        ] {
            let ctx = wire_ctx();
            let goal = (GoalId::new(uuid::Uuid::now_v7())).into_inner().to_string();
            let mut args = serde_json::json!({
                "action": "transition",
                "goal": goal,
                "transition": transition,
            });
            if let Some(pending) = pending {
                args["pending"] = pending.into();
            }
            let err = (desc.call)(ctx, args)
                .await
                .expect_err("a pending that does not fit the transition is refused");
            assert_eq!(err.kind(), McpToolErrorKind::InvalidInput, "{transition}");
            let message = err.to_string();
            for name in named {
                assert!(message.contains(name), "{transition}: {message}");
            }
        }
    }

    #[tokio::test]
    async fn dispatcher_rejects_cross_action_fact_fields_before_execution() {
        let ctx = wire_ctx();
        let fact = (MemoryId::new(uuid::Uuid::now_v7()))
            .into_inner()
            .to_string();
        let desc = ctx
            .registry
            .list_mcp_tools()
            .iter()
            .find(|tool| tool.name == protocol_tool::CORE_FACT)
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
    async fn wire_ctx_rejects_prefixed_ids() {
        let ctx = wire_ctx();
        let id = uuid::Uuid::now_v7();
        for prefix in ['F', 'A', 'P', 'G', 'R'] {
            let raw = format!("{prefix}:{id}");
            assert!(ctx.resolve_memory(&raw).is_err());
            assert!(ctx.resolve_goal(&raw).is_err());
        }
        assert_eq!(
            ctx.resolve_memory(&id.to_string())
                .expect("UUID")
                .into_inner(),
            id
        );
    }

    fn wire_ctx() -> McpToolCtx {
        let owner = OwnerId::from(UserId::new(uuid::Uuid::now_v7()));
        McpToolCtx {
            owner,
            authz: AuthzContext::single_owner(&owner, AuthPath::HostBearer),
            registry: Arc::new(FlavorRegistry::new().freeze_or_panic_for_tests()),
            author: McpAuthorContext {
                model_id: "t".into(),
                trusted_model_id: None,
                client_name: "t".into(),
                client_version: "0".into(),
                caller_self_perspective: None,
            },
            caller_self_perspective: None,
            services: FlavorServices::default(),
            engine: None,
        }
    }
}
