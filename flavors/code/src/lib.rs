//! Proxima code flavor — Rust + TypeScript cAST chunker and Fact schemas.
//!
//! See docs/08 for flavor architecture.

pub mod calls;
pub mod chunker;
pub mod ingest;
pub mod local_git_source;
pub mod mcp;
pub mod migrations;
pub mod payloads;
pub mod repos;

pub use ingest::{
    ACCEPTANCE_CRITERIA_OBJECT_SCHEMA, ACCEPTANCE_CRITERIA_WHOLE_SCHEMA,
    ACCEPTANCE_VERIFICATION_OBJECT_SCHEMA, ACCEPTANCE_VERIFICATION_WHOLE_SCHEMA, CODE_BLOB_SCHEMA,
    CODE_BLOB_WHOLE_SCHEMA, CODE_COMMIT_OBJECT_SCHEMA, CODE_COMMIT_WHOLE_SCHEMA,
    EXECUTION_REQUEST_OBJECT_SCHEMA, EXECUTION_REQUEST_WHOLE_SCHEMA,
    EXECUTION_RESULT_OBJECT_SCHEMA, EXECUTION_RESULT_WHOLE_SCHEMA, IngestError,
    LOCAL_GIT_SOURCE_ID, TEST_REQUEST_OBJECT_SCHEMA, TEST_REQUEST_WHOLE_SCHEMA,
    TEST_RESULT_OBJECT_SCHEMA, TEST_RESULT_WHOLE_SCHEMA, append_code_slice, build_engine,
    build_engine_with, ingest_commit, ingest_file_revision,
};
pub use local_git_source::{
    HeadSnapshotOutcome, IndexError, IndexReport, IngestProgress, LocalGitSource,
};
pub use migrations::migrator;
pub use payloads::{
    AcceptanceCriteriaV1, AcceptanceCriterionV1, AcceptanceSummaryV1, AcceptanceVerificationStatus,
    AcceptanceVerificationV1, AcceptanceVerifierKind, AcceptanceVerifierSpecV1, CodeChunkV1,
    CodeCommitSummarizerSelfV1, CodeDevelopmentPerspectiveV1, CodeEngineerSelfV1,
    CodeExecutionPlanItemKind, CodeExecutionPlanItemV1, CodeExecutionPlanV1, CommitSummaryV1,
    CommitV1, EdgeCallsV1, ExecutionRequestV1, ExecutionResultV1, FileRevisionV1, FileState,
    TestRequestV1, TestRequestedV1, TestResultV1, WorkRequestedV1, WorkResultStatus,
};

use proxima_core::{
    AuthorshipKindMask, EndpointBinding, EntityKindMask, RelationClass, RelationDescriptor,
    SchemaId, SchemaRef, SchemaVersion,
};
pub use repos::{
    RepoEraseReceipt, RepoIngestionRun, RepoRecord, RepoRegistryError, RunStage, RunStatus,
    StageCounters, advance_stage, begin_run, delete_repo, erase_repo, get_active_run, get_repo,
    get_run, infer_missing_target_branch, list_repos, mark_failed, mark_succeeded, register_repo,
    set_repo_target_branch, start_run, start_run_with_created, sweep_orphaned_runs, update_cursor,
};

proxima_core::proxima_flavor! {
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
    ],
    edge_schemas = [
        payloads::EdgeCallsV1,
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
    relations = [
        RelationDescriptor::typed(
            "proxima-code/calls",
            RelationClass::Structural,
            SchemaRef::new(
                SchemaId::new("proxima-code/calls".into()),
                SchemaVersion::new(1),
            ),
            EndpointBinding::Pin,
            EndpointBinding::Pin,
            EntityKindMask::abstraction(),
            EntityKindMask::abstraction(),
            AuthorshipKindMask::operator_f_to_a(),
        ),
        RelationDescriptor::substrate(
            mcp::CODE_TARGETS_EXECUTION_REQUEST_RELATION,
            RelationClass::Causal,
            EndpointBinding::Pin,
            EndpointBinding::Pin,
            EntityKindMask::perspective(),
            EntityKindMask::fact(),
            AuthorshipKindMask::external_agent(),
        ),
        RelationDescriptor::substrate(
            mcp::CODE_HAS_ACCEPTANCE_CRITERIA_RELATION,
            RelationClass::Provenance,
            EndpointBinding::Pin,
            EndpointBinding::Pin,
            EntityKindMask::fact(),
            EntityKindMask::fact(),
            AuthorshipKindMask::external_agent(),
        ),
    ],
    mcp_tools = [
        mcp::CodeListReposTool,
        mcp::CodeRegisterRepoTool,
        mcp::CodeIngestHeadSnapshotTool,
        mcp::CodeSearchChunksTool,
        mcp::CodeOpenFileRevisionTool,
        mcp::CodeSearchCommitsTool,
        mcp::CodeEmitExecutionRequestTool,
        mcp::CodeEmitExecutionPlanTool,
        mcp::CodeRetryExecutionRequestTool,
        mcp::CodeWorkItemBundleTool,
    ],
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
    registry.add_edge::<payloads::EdgeCallsV1>();
}

#[derive(Debug)]
pub struct CodeFlavor;

impl proxima::FlavorBundle for CodeFlavor {
    fn register(registry: &mut proxima_core::FlavorRegistry) {
        self::register(registry);
    }

    fn register_pg_sidecars(registry: &mut proxima::PgSidecarRegistry) {
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
    use proxima_core::{CORE_DERIVED_FROM_RELATION, FlavorRegistry};
    use std::collections::HashSet;

    #[test]
    fn registry_contains_all_schemas_and_relations() {
        let mut registry = FlavorRegistry::new();
        super::register(&mut registry);
        let frozen = registry.freeze();

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
        // Edge schemas
        assert!(schema_ids.contains("proxima-code/calls"));
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

        // Check relations
        let relations = frozen.list_relations();
        let relation_ids: HashSet<_> = relations.iter().map(|r| r.relation.as_str()).collect();
        assert!(relation_ids.contains("proxima-code/calls"));
        assert!(relation_ids.contains("proxima-code/targets-execution-request"));
        assert!(relation_ids.contains("proxima-code/has-acceptance-criteria"));
        assert!(relation_ids.contains(CORE_DERIVED_FROM_RELATION));

        let calls = frozen
            .resolve_relation("proxima-code/calls")
            .expect("typed calls relation resolves");
        assert_eq!(
            calls.payload_sidecar_table,
            Some("proxima_code.code_calls_v1")
        );

        let derived_from = frozen
            .resolve_relation(CORE_DERIVED_FROM_RELATION)
            .expect("core provenance relation resolves");
        assert_eq!(derived_from.payload_sidecar_table, None);
    }

    #[test]
    fn registry_lists_all_mcp_tools() {
        let mut registry = FlavorRegistry::new();
        super::register(&mut registry);
        let frozen = registry.freeze();

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
}
