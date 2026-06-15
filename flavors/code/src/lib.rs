//! Proxima code flavor — Rust + TypeScript cAST chunker and Fact schemas.
//!
//! See docs/08 for flavor architecture.

pub mod calls;
pub mod chunker;
pub mod dependency;
pub mod ingest;
pub mod local_git_source;
pub mod mcp;
pub mod migrations;
pub mod payloads;
pub mod repos;
pub mod verification;

pub use ingest::{
    CODE_BLOB_BYTE_RANGE_SCHEMA, CODE_BLOB_SCHEMA, CODE_BLOB_WHOLE_SCHEMA,
    CODE_COMMIT_OBJECT_SCHEMA, CODE_COMMIT_WHOLE_SCHEMA, EXECUTION_REQUEST_OBJECT_SCHEMA,
    EXECUTION_REQUEST_WHOLE_SCHEMA, IngestError, LOCAL_GIT_SOURCE_ID, TEST_REQUEST_OBJECT_SCHEMA,
    TEST_REQUEST_WHOLE_SCHEMA, build_engine, build_engine_with, ingest_code_chunk, ingest_commit,
    ingest_file_revision,
};
pub use local_git_source::{
    HeadSnapshotOutcome, IndexError, IndexReport, IngestProgress, LocalGitSource,
};
pub use migrations::migrator;
pub use payloads::{
    AcceptanceCriteriaV1, AcceptanceCriterionV1, AcceptanceVerifierKind, AcceptanceVerifierSpecV1,
    CodeChunkV1, CodeCommitSummarizerSelfV1, CodeDevelopmentPerspectiveV1, CodeEngineerSelfV1,
    CommitSummaryV1, CommitV1, EdgeCallsV1, ExecutionRequestV1, FileRevisionV1, FileState,
    TestRequestV1, VerificationArtifactRefsV1, VerificationEvidenceStatus, VerificationEvidenceV1,
};

use proxima_core::{
    AuthorshipKindMask, EntityKindMask, RelationClass, RelationDescriptor, SchemaId, SchemaRef,
    SchemaVersion,
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
        payloads::CodeChunkV1,
        payloads::ExecutionRequestV1,
        payloads::TestRequestV1,
        payloads::AcceptanceCriteriaV1,
        payloads::VerificationEvidenceV1,
    ],
    abstraction_schemas = [
        payloads::CommitSummaryV1,
    ],
    perspective_schemas = [
        payloads::CodeDevelopmentPerspectiveV1,
        payloads::CodeCommitSummarizerSelfV1,
        payloads::CodeEngineerSelfV1,
    ],
    edge_schemas = [
        payloads::EdgeCallsV1,
    ],
    relations = [
        RelationDescriptor::typed(
            "proxima-code/calls",
            RelationClass::Structural,
            SchemaRef::new(
                SchemaId::new("proxima-code/calls".into()),
                SchemaVersion::new(1),
            ),
            EntityKindMask::fact(),
            EntityKindMask::fact(),
            AuthorshipKindMask::event_source(),
        ),
        RelationDescriptor::substrate(
            mcp::CODE_TARGETS_EXECUTION_REQUEST_RELATION,
            RelationClass::Causal,
            EntityKindMask::perspective(),
            EntityKindMask::fact(),
            AuthorshipKindMask::external_agent(),
        ),
        RelationDescriptor::substrate(
            mcp::CODE_HAS_ACCEPTANCE_CRITERIA_RELATION,
            RelationClass::Provenance,
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
    ],
    dependency_satisfaction_rules = [
        dependency::TestRequestSatisfied,
    ],
}

#[cfg(test)]
mod tests {
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
        assert!(schema_ids.contains("proxima-code/code-chunk-v1"));
        assert!(schema_ids.contains("proxima-code/execution-request-v1"));
        assert!(schema_ids.contains("proxima-code/test-request-v1"));
        assert!(schema_ids.contains("proxima-code/acceptance-criteria-v1"));
        assert!(schema_ids.contains("proxima-code/verification-evidence-v1"));
        // Abstraction schemas
        assert!(schema_ids.contains("proxima-code/commit-summary-v1"));
        // Perspective schemas
        assert!(schema_ids.contains("proxima-code/development-perspective-v1"));
        assert!(schema_ids.contains("proxima-code/commit-summarizer-self-v1"));
        assert!(schema_ids.contains("proxima-code/engineer-self-v1"));
        // Edge schemas
        assert!(schema_ids.contains("proxima-code/calls"));

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
        assert!(names.contains("proxima-code/code_list_repos"));
        assert!(names.contains("proxima-code/code_register_repo"));
        assert!(names.contains("proxima-code/code_search_chunks"));
        assert!(names.contains("proxima-code/code_open_file_revision"));
        assert!(names.contains("proxima-code/code_search_commits"));
        assert!(names.contains("proxima-code/code_emit_execution_request"));
        assert!(names.contains("proxima-code/code_emit_execution_plan"));
        assert!(names.contains("proxima-code/code_retry_execution_request"));
    }
}
