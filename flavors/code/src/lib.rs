//! Proxima code flavor — Rust + TypeScript cAST chunker and Fact schemas.
//!
//! See docs/08 for flavor architecture.

pub mod calls;
pub mod chunker;
pub mod contract;
mod ingest;
pub mod local_git_source;
pub mod mcp;
pub mod migrations;
pub mod payloads;
mod repos;
mod store;

pub use ingest::{
    ACCEPTANCE_CRITERIA_OBJECT_SCHEMA, ACCEPTANCE_CRITERIA_WHOLE_SCHEMA,
    ACCEPTANCE_VERIFICATION_OBJECT_SCHEMA, ACCEPTANCE_VERIFICATION_WHOLE_SCHEMA, CODE_BLOB_SCHEMA,
    CODE_BLOB_WHOLE_SCHEMA, CODE_COMMIT_OBJECT_SCHEMA, CODE_COMMIT_WHOLE_SCHEMA,
    EXECUTION_REQUEST_OBJECT_SCHEMA, EXECUTION_REQUEST_WHOLE_SCHEMA,
    EXECUTION_RESULT_OBJECT_SCHEMA, EXECUTION_RESULT_WHOLE_SCHEMA, IngestError,
    LOCAL_GIT_SOURCE_ID, TEST_REQUEST_OBJECT_SCHEMA, TEST_REQUEST_WHOLE_SCHEMA,
    TEST_RESULT_OBJECT_SCHEMA, TEST_RESULT_WHOLE_SCHEMA, schema_registry, schema_registry_with,
};
pub use local_git_source::{
    CodeIngestContext, HeadSnapshotOutcome, IndexError, IndexReport, IngestProgress, LocalGitSource,
};
pub use migrations::migrator;
pub use payloads::{
    AcceptanceCriteriaV1, AcceptanceCriterionV1, AcceptanceSummaryV1, AcceptanceVerificationStatus,
    AcceptanceVerificationV1, AcceptanceVerifierKind, AcceptanceVerifierSpecV1, CodeCallSiteV1,
    CodeCallV1, CodeChunkV1, CodeCommitSummarizerSelfV1, CodeDevelopmentPerspectiveV1,
    CodeEngineerSelfV1, CodeExecutionPlanItemKind, CodeExecutionPlanItemV1, CodeExecutionPlanV1,
    CodeWorkAssignmentV1, CommitSummaryV1, CommitV1, ExecutionRequestV1, ExecutionResultV1,
    FileRevisionV1, FileState, TestRequestV1, TestRequestedV1, TestResultV1, WorkRequestedV1,
    WorkResultStatus,
};
pub use store::CodeFlavorStore;

pub use repos::{
    MAX_SCOPE_GLOB_LEN, MAX_SCOPE_GLOBS, RepoEraseReceipt, RepoIngestionRun, RepoRecord,
    RepoRegistryError, RepoScope, RunStage, RunStatus, ScopeError, ScopeMatcher, StageCounters,
};

#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
pub mod testkit {
    pub use crate::ingest::{
        append_code_slice, build_engine, build_engine_with, close_local_git_batch, ingest_commit,
        ingest_file_revision,
    };
    pub use crate::repos::runs::{
        advance_stage, begin_run, get_active_run, get_run, mark_failed, mark_succeeded, start_run,
        start_run_with_created, sweep_orphaned_runs,
    };
    pub use crate::repos::{
        erase_footprint, erase_repo, get_repo, list_repos, reference_closure_sql, register_repo,
        set_repo_scope, set_repo_target_branch, update_cursor,
    };
}

proxima::flavor::proxima_flavor! {
    name = "proxima-code",
    display_name = "Code",
    fact_schemas = [
        payloads::CommitV1,
        payloads::FileRevisionV1,
        payloads::WorkRequestedV1,
        payloads::TestRequestedV1,
        payloads::AcceptanceCriteriaV1,
        payloads::ExecutionResultV1,
        payloads::TestResultV1,
        payloads::AcceptanceVerificationV1,
    ],
    abstraction_schemas = [
        payloads::CodeChunkV1,
        payloads::CommitSummaryV1,
        payloads::CodeExecutionPlanV1,
        payloads::AcceptanceSummaryV1,
    ],
    perspective_schemas = [
        payloads::CodeDevelopmentPerspectiveV1,
        payloads::CodeCommitSummarizerSelfV1,
        payloads::CodeEngineerSelfV1,
        payloads::CodeWorkAssignmentV1,
    ],
    opaque_cited_object_schemas = [
        CODE_BLOB_SCHEMA,
        CODE_COMMIT_OBJECT_SCHEMA,
        EXECUTION_REQUEST_OBJECT_SCHEMA,
        ACCEPTANCE_CRITERIA_OBJECT_SCHEMA,
        TEST_REQUEST_OBJECT_SCHEMA,
        EXECUTION_RESULT_OBJECT_SCHEMA,
        TEST_RESULT_OBJECT_SCHEMA,
        ACCEPTANCE_VERIFICATION_OBJECT_SCHEMA,
    ],
    opaque_citation_mapping_schemas = [
        CODE_BLOB_WHOLE_SCHEMA,
        CODE_COMMIT_WHOLE_SCHEMA,
        EXECUTION_REQUEST_WHOLE_SCHEMA,
        ACCEPTANCE_CRITERIA_WHOLE_SCHEMA,
        TEST_REQUEST_WHOLE_SCHEMA,
        EXECUTION_RESULT_WHOLE_SCHEMA,
        TEST_RESULT_WHOLE_SCHEMA,
        ACCEPTANCE_VERIFICATION_WHOLE_SCHEMA,
    ],
    mcp_tools = [
        mcp::CodeListReposTool,
        mcp::CodeRegisterRepoTool,
        mcp::CodeIngestHeadSnapshotTool,
        mcp::CodeEraseRepoTool,
        mcp::CodeSearchChunksTool,
        mcp::CodeOpenFileRevisionTool,
        mcp::CodeSearchCommitsTool,
        mcp::CodeEmitExecutionRequestTool,
        mcp::CodeEmitExecutionPlanTool,
        mcp::CodeRetryExecutionRequestTool,
        mcp::CodeWorkItemBundleTool,
    ],
    contract = &contract::CODE_FLAVOR_CONTRACT,
}

pub fn register_pg_sidecars(registry: &mut proxima_storage_pg::PgSidecarRegistry) {
    registry.add_fact::<payloads::CommitV1>();
    registry.add_fact::<payloads::FileRevisionV1>();
    registry.add_fact::<payloads::WorkRequestedV1>();
    registry.add_fact::<payloads::TestRequestedV1>();
    registry.add_fact::<payloads::AcceptanceCriteriaV1>();
    registry.add_fact::<payloads::ExecutionResultV1>();
    registry.add_fact::<payloads::TestResultV1>();
    registry.add_fact::<payloads::AcceptanceVerificationV1>();
    registry.add_abstraction::<payloads::CodeChunkV1>();
    registry.add_abstraction::<payloads::CommitSummaryV1>();
    registry.add_abstraction::<payloads::CodeExecutionPlanV1>();
    registry.add_abstraction::<payloads::AcceptanceSummaryV1>();
    registry.add_perspective::<payloads::CodeDevelopmentPerspectiveV1>();
    registry.add_perspective::<payloads::CodeCommitSummarizerSelfV1>();
    registry.add_perspective::<payloads::CodeEngineerSelfV1>();
    registry.add_perspective::<payloads::CodeWorkAssignmentV1>();
}

#[derive(Debug)]
pub struct CodeFlavor;

impl proxima::flavor::FlavorBundle for CodeFlavor {
    fn register(
        registry: &mut proxima_core::FlavorRegistry,
    ) -> Result<(), proxima_core::FlavorRegistryError> {
        self::register(registry)
    }

    fn register_pg_sidecars(registry: &mut proxima::flavor::PgSidecarRegistry) {
        self::register_pg_sidecars(registry);
    }

    fn migrators() -> Vec<proxima::NamedMigrator> {
        vec![proxima::NamedMigrator::new("proxima-code", migrator())]
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ACCEPTANCE_CRITERIA_OBJECT_SCHEMA, ACCEPTANCE_CRITERIA_WHOLE_SCHEMA,
        ACCEPTANCE_VERIFICATION_OBJECT_SCHEMA, ACCEPTANCE_VERIFICATION_WHOLE_SCHEMA,
        CODE_BLOB_SCHEMA, CODE_BLOB_WHOLE_SCHEMA, CODE_COMMIT_OBJECT_SCHEMA,
        CODE_COMMIT_WHOLE_SCHEMA, EXECUTION_REQUEST_OBJECT_SCHEMA, EXECUTION_REQUEST_WHOLE_SCHEMA,
        EXECUTION_RESULT_OBJECT_SCHEMA, EXECUTION_RESULT_WHOLE_SCHEMA, TEST_REQUEST_OBJECT_SCHEMA,
        TEST_REQUEST_WHOLE_SCHEMA, TEST_RESULT_OBJECT_SCHEMA, TEST_RESULT_WHOLE_SCHEMA,
    };
    use proxima_core::{EntityKind, FlavorRegistry, MemoryId, PerspectivePayload};
    use std::collections::HashSet;

    #[test]
    fn registry_contains_all_schemas() {
        let mut registry = FlavorRegistry::new();
        super::register(&mut registry).unwrap();
        let frozen = registry.try_freeze().unwrap();

        let schemas = frozen.list();
        let schema_ids: HashSet<_> = schemas.iter().map(|s| s.schema_id.as_str()).collect();

        // Fact schemas
        assert!(schema_ids.contains("proxima-code/commit-v1"));
        assert!(schema_ids.contains("proxima-code/file-revision-v1"));
        assert!(schema_ids.contains("proxima-code/work-requested-v1"));
        assert!(schema_ids.contains("proxima-code/test-requested-v1"));
        assert!(schema_ids.contains("proxima-code/acceptance-criteria-v1"));
        assert!(schema_ids.contains("proxima-code/execution-result-v1"));
        assert!(schema_ids.contains("proxima-code/test-result-v1"));
        assert!(schema_ids.contains("proxima-code/acceptance-verification-v1"));
        // Abstraction schemas
        assert!(schema_ids.contains("proxima-code/code-chunk-v1"));
        assert!(schema_ids.contains("proxima-code/commit-summary-v1"));
        assert!(schema_ids.contains("proxima-code/execution-plan-v1"));
        assert!(schema_ids.contains("proxima-code/acceptance-summary-v1"));
        // Perspective schemas
        assert!(schema_ids.contains("proxima-code/development-perspective-v1"));
        assert!(schema_ids.contains("proxima-code/commit-summarizer-self-v1"));
        assert!(schema_ids.contains("proxima-code/engineer-self-v1"));
        assert!(schema_ids.contains("proxima-code/work-assignment-v1"));
        // `proxima-code/calls` is not a schema: a flavor cannot declare a
        // typed edge payload, and call sites live on the caller chunk's
        // payload.
        assert!(!schema_ids.contains("proxima-code/calls"));
        // Opaque cited-object / citation-mapping schemas
        assert!(schema_ids.contains(CODE_BLOB_SCHEMA));
        assert!(schema_ids.contains(CODE_COMMIT_OBJECT_SCHEMA));
        assert!(schema_ids.contains(EXECUTION_REQUEST_OBJECT_SCHEMA));
        assert!(schema_ids.contains(ACCEPTANCE_CRITERIA_OBJECT_SCHEMA));
        assert!(schema_ids.contains(TEST_REQUEST_OBJECT_SCHEMA));
        assert!(schema_ids.contains(EXECUTION_RESULT_OBJECT_SCHEMA));
        assert!(schema_ids.contains(TEST_RESULT_OBJECT_SCHEMA));
        assert!(schema_ids.contains(ACCEPTANCE_VERIFICATION_OBJECT_SCHEMA));
        assert!(schema_ids.contains(CODE_BLOB_WHOLE_SCHEMA));
        assert!(schema_ids.contains(CODE_COMMIT_WHOLE_SCHEMA));
        assert!(schema_ids.contains(EXECUTION_REQUEST_WHOLE_SCHEMA));
        assert!(schema_ids.contains(ACCEPTANCE_CRITERIA_WHOLE_SCHEMA));
        assert!(schema_ids.contains(TEST_REQUEST_WHOLE_SCHEMA));
        assert!(schema_ids.contains(EXECUTION_RESULT_WHOLE_SCHEMA));
        assert!(schema_ids.contains(TEST_RESULT_WHOLE_SCHEMA));
        assert!(schema_ids.contains(ACCEPTANCE_VERIFICATION_WHOLE_SCHEMA));
    }

    /// Every connection this flavor creates is a field on a payload, and the
    /// payload says so itself.
    ///
    /// This is the checkable half of docs/16 §The Model. There is no relation
    /// registry to interrogate, so the assertion is against the schemas that
    /// own the statements instead.
    #[test]
    fn every_connection_is_a_field_on_a_payload_that_owns_it() {
        use super::payloads::{
            AcceptanceCriteriaV1, CodeCallSiteV1, CodeCallV1, CodeChunkV1, CodeWorkAssignmentV1,
            FileState,
        };
        use proxima_core::{AbstractionPayload, FactPayload};

        let callee = uuid::Uuid::now_v7();
        // proxima-code/calls: the callee lives on the caller payload. Two
        // sites collapse to one payload entry. Kernel refs stay empty —
        // callees are named by series handle before their t exists.
        let chunk = CodeChunkV1 {
            repo_id: uuid::Uuid::now_v7(),
            file_path: "src/lib.rs".into(),
            chunk_index: 0,
            text: "fn caller() { callee(); callee(); }".into(),
            language: Some("rust".into()),
            chunk_type: "block".into(),
            byte_range_start: 0,
            byte_range_end: 34,
            line_range_start: 1,
            line_range_end: 1,
            state: FileState::Present,
            calls: vec![CodeCallV1 {
                callee_memory_id: callee,
                sites: vec![
                    CodeCallSiteV1 {
                        byte_start: 14,
                        byte_end: 22,
                        callee_name: "callee".into(),
                        is_dynamic: false,
                    },
                    CodeCallSiteV1 {
                        byte_start: 24,
                        byte_end: 32,
                        callee_name: "callee".into(),
                        is_dynamic: false,
                    },
                ],
            }],
        };
        assert_eq!(chunk.calls.len(), 1, "two sites are one connection");
        assert_eq!(chunk.calls[0].sites.len(), 2);
        assert_eq!(chunk.calls[0].callee_memory_id, callee);
        let references = <CodeChunkV1 as AbstractionPayload>::references(&chunk);
        assert!(
            references.is_empty(),
            "call graph is sidecar-local, not a kernel pin"
        );

        // proxima-code/has-acceptance-criteria: the criteria Fact points at
        // the request it is the bar for.
        let work_item = uuid::Uuid::now_v7();
        let criteria = AcceptanceCriteriaV1 {
            work_item_memory_id: work_item,
            criteria: Vec::new(),
        };
        let references = <AcceptanceCriteriaV1 as FactPayload>::references(&criteria);
        assert_eq!(references.len(), 1);
        assert_eq!(references[0].field, "work_item_memory_id");
        assert_eq!(
            references[0].target,
            proxima_core::EdgeEndpoint::memory(EntityKind::Fact, MemoryId::new(work_item))
        );

        // proxima-code/targets-execution-request: neither endpoint owns the
        // claim, so it is a node that names both.
        let worker = uuid::Uuid::now_v7();
        let assignment = CodeWorkAssignmentV1 {
            repo_id: uuid::Uuid::now_v7(),
            target_perspective_memory_id: worker,
            work_item_memory_id: work_item,
            reason: "retry".into(),
        };
        let references = <CodeWorkAssignmentV1 as PerspectivePayload>::references(&assignment);
        assert_eq!(references.len(), 2);
        for reference in &references {
            reference.validate().expect("binding matches address form");
        }
        assert_eq!(
            references[0].target,
            proxima_core::EdgeEndpoint::memory(EntityKind::Perspective, MemoryId::new(worker))
        );
        assert_eq!(
            references[1].target,
            proxima_core::EdgeEndpoint::memory(EntityKind::Fact, MemoryId::new(work_item))
        );
    }

    /// Every tool this flavor serves declares what it does to the world.
    ///
    /// Not cosmetic. `ScopeGateBehavior::enforce_owner_role` asks whether a
    /// tool is read-only and demands WRITE access when it cannot tell, so an
    /// undeclared read tool is billed as a write and a viewer is refused a
    /// search. The other half is `erase_repo`, which is irreversible and must
    /// advertise that to a client deciding what to auto-approve.
    ///
    /// Asserted over the whole registered set rather than tool by tool, so
    /// a tool added later cannot ship silent.
    #[test]
    fn every_served_tool_declares_its_behavior() {
        let mut registry = FlavorRegistry::new();
        super::register(&mut registry).unwrap();
        let frozen = registry.try_freeze().unwrap();

        let mut read_only = Vec::new();
        for tool in frozen.list_mcp_tools() {
            if !tool.name.starts_with("proxima-code") {
                continue; // core's own tools answer through core's table.
            }
            let annotations = tool.annotations.unwrap_or_else(|| {
                panic!(
                    "{} declares no ANNOTATIONS, so the owner-role gate will bill it as a write",
                    tool.name
                )
            });
            assert_eq!(
                annotations.open_world,
                Some(false),
                "{} reaches only this deployment's database",
                tool.name
            );
            if annotations.read_only == Some(true) {
                read_only.push(tool.name);
            }
        }

        assert!(
            read_only.contains(&"proxima-code_search_chunks"),
            "the search tools must be callable by a read-only role: {read_only:?}"
        );
        assert!(
            read_only.contains(&"proxima-code_search_commits"),
            "{read_only:?}"
        );
        assert!(
            read_only.contains(&"proxima-code_list_repos"),
            "{read_only:?}"
        );
        assert!(
            read_only.contains(&"proxima-code_open_file_revision"),
            "{read_only:?}"
        );

        let erase = frozen
            .list_mcp_tools()
            .iter()
            .find(|tool| tool.name == "proxima-code_erase_repo")
            .and_then(|tool| tool.annotations)
            .expect("erase_repo is registered and annotated");
        assert_eq!(
            erase.destructive,
            Some(true),
            "erase_repo is irreversible and must say so before a client auto-approves it"
        );
        assert_eq!(erase.read_only, Some(false));
    }

    #[test]
    fn registry_lists_all_mcp_tools() {
        let mut registry = FlavorRegistry::new();
        super::register(&mut registry).unwrap();
        let frozen = registry.try_freeze().unwrap();

        let names: HashSet<_> = frozen
            .list_mcp_tools()
            .iter()
            .map(|tool| tool.name)
            .collect();
        assert!(names.contains("proxima-code_list_repos"));
        assert!(names.contains("proxima-code_register_repo"));
        assert!(names.contains("proxima-code_search_chunks"));
        assert!(names.contains("proxima-code_open_file_revision"));
        assert!(names.contains("proxima-code_search_commits"));
        assert!(names.contains("proxima-code_emit_execution_request"));
        assert!(names.contains("proxima-code_emit_execution_plan"));
        assert!(names.contains("proxima-code_retry_execution_request"));
        assert!(names.contains("proxima-code_work_item_bundle"));
    }

    #[test]
    fn composed_registry_mcp_tool_schemas_are_client_safe() {
        fn describes_an_object(schema: &serde_json::Value) -> bool {
            if schema.get("type").and_then(serde_json::Value::as_str) == Some("object") {
                return true;
            }
            for key in ["anyOf", "oneOf"] {
                if let Some(branches) = schema.get(key).and_then(serde_json::Value::as_array) {
                    return !branches.is_empty() && branches.iter().all(describes_an_object);
                }
            }
            false
        }

        fn contains_key(value: &serde_json::Value, key: &str) -> bool {
            match value {
                serde_json::Value::Object(map) => {
                    map.contains_key(key) || map.values().any(|v| contains_key(v, key))
                }
                serde_json::Value::Array(items) => items.iter().any(|v| contains_key(v, key)),
                _ => false,
            }
        }

        let mut registry = FlavorRegistry::default();
        super::register(&mut registry).unwrap();
        let frozen = registry.try_freeze().unwrap();
        let names: HashSet<_> = frozen
            .list_mcp_tools()
            .iter()
            .map(|tool| tool.name)
            .collect();
        assert!(names.contains("core_goal"));
        assert!(names.contains("proxima-code_search_chunks"));

        for tool in frozen.list_mcp_tools() {
            assert_eq!(
                tool.args_schema
                    .get("type")
                    .and_then(serde_json::Value::as_str),
                Some("object"),
                "tool {} must expose object-root args schema: {:#}",
                tool.name,
                tool.args_schema,
            );
            assert!(
                tool.args_schema
                    .get("properties")
                    .is_some_and(serde_json::Value::is_object),
                "tool {} must expose root properties: {:#}",
                tool.name,
                tool.args_schema,
            );
            for keyword in ["oneOf", "anyOf", "allOf"] {
                assert!(
                    tool.args_schema.get(keyword).is_none(),
                    "tool {} must not expose root {keyword}: {:#}",
                    tool.name,
                    tool.args_schema,
                );
            }
            assert!(
                !contains_key(&tool.args_schema, "$ref"),
                "tool {} must be $ref-free: {:#}",
                tool.name,
                tool.args_schema,
            );
            assert!(
                !contains_key(&tool.args_schema, "$defs"),
                "tool {} must be $defs-free: {:#}",
                tool.name,
                tool.args_schema,
            );
            assert!(
                !contains_key(&tool.output_schema, "$ref")
                    && !contains_key(&tool.output_schema, "$defs"),
                "tool {} output schema must be $ref/$defs-free: {:#}",
                tool.name,
                tool.output_schema,
            );
            // MCP carries a typed reply in `structuredContent`, which is a
            // JSON object. A flavor tool answering with a scalar, array, or
            // unit would advertise a shape its own replies violate — wrap it
            // in a struct instead. Union roots are fine if every branch is an
            // object, which is what the untagged dispatchers answer with.
            assert!(
                describes_an_object(&tool.output_schema),
                "tool {} advertises an output that is not an object: {:#}",
                tool.name,
                tool.output_schema,
            );
        }
    }

    /// `proxima-code_work_item_bundle` answers with an internally tagged enum
    /// (`#[serde(tag = "kind")]`) nested under `payload`. That is the exact
    /// shape the *argument* normalization flattens into a dispatcher, and
    /// doing so here would tell a client that a `work` reply may carry the
    /// `test` variant's fields. The output pass leaves the union alone.
    #[test]
    fn a_tagged_output_enum_is_not_flattened_into_a_dispatcher() {
        let mut registry = FlavorRegistry::default();
        super::register(&mut registry).unwrap();
        let frozen = registry.try_freeze().unwrap();
        let bundle = frozen
            .list_mcp_tools()
            .iter()
            .find(|tool| tool.name == "proxima-code_work_item_bundle")
            .expect("work item bundle tool is registered");
        assert!(
            bundle
                .output_schema
                .pointer("/properties/payload/oneOf")
                .is_some_and(serde_json::Value::is_array),
            "the tagged payload union must survive into the output schema: {:#}",
            bundle.output_schema,
        );
        assert!(
            bundle
                .output_schema
                .pointer("/properties/payload/x-proxima-actions")
                .is_none(),
            "an output union must not be given dispatcher action metadata: {:#}",
            bundle.output_schema,
        );
    }
}
