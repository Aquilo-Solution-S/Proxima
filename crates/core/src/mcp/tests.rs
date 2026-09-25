use super::*;

mod manifest_tests {
    use std::{collections::BTreeSet, sync::Arc};

    use super::*;
    use crate::protocol::resource as protocol_resource;
    use crate::protocol::tool as protocol_tool;
    use crate::{
        AuthPath, AuthzContext, FlavorRegistry, FlavorServices, GoalId, MemoryId, OwnerRef, UserId,
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
            protocol_resource::MEMORY_LINEAGE,
            protocol_resource::CHANGE_EVENTS,
            protocol_resource::WAKE_CANDIDATES,
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
        let ctx = prefixed_ctx();
        let goal = ctx.format_goal(GoalId::new(uuid::Uuid::now_v7()));
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
    async fn dispatcher_rejects_cross_action_fact_fields_before_execution() {
        let ctx = prefixed_ctx();
        let fact = ctx.format_fact_memory(MemoryId::new(uuid::Uuid::now_v7()));
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
