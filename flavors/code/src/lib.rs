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
    CODE_REPO_SCOPE, CODE_REPO_SCOPE_DECL, MAX_SCOPE_GLOB_LEN, MAX_SCOPE_GLOBS, RepoEraseReceipt,
    RepoIngestionRun, RepoRecord, RepoRegistryError, RepoScope, RunStage, RunStatus, ScopeError,
    ScopeMatcher, StageCounters,
};

#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
pub mod testkit {
    use crate::CodeFlavorStore;

    pub use crate::ingest::{
        append_code_slice, build_engine, build_engine_with, ingest_commit, ingest_file_revision,
    };
    pub use crate::repos::runs::{
        advance_stage, begin_run, get_active_run, get_run, mark_failed, mark_succeeded, start_run,
        start_run_with_created, sweep_orphaned_runs,
    };
    pub use crate::repos::{
        erase_footprint, erase_repo, get_repo, list_repos, reference_closure_sql, register_repo,
        set_repo_scope, set_repo_target_branch, update_cursor,
    };

    /// Run repository erasure with the verified owner witness carried by the
    /// test store. This keeps direct fixture calls on the same scoped path as
    /// the production host request.
    pub async fn erase_repo_with_scope(
        store: &CodeFlavorStore,
        owner: &proxima_core::Owner,
        repo_id: uuid::Uuid,
        scope: &proxima_core::OwnerScope,
    ) -> Result<crate::repos::RepoEraseReceipt, crate::repos::RepoRegistryError> {
        erase_repo(
            &store.clone().with_owner_scope(Some(scope.clone())),
            owner,
            repo_id,
        )
        .await
    }
}

proxima::flavor_bundle! {
    bundle = CodeFlavor,
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
        mcp::CodeStartIngestHeadSnapshotTool,
        mcp::CodeGetIngestRunTool,
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
    migrations = migrator(),
}

#[cfg(test)]
mod tests {

    use proxima_core::{EntityKind, FlavorRegistry, MemoryId, PerspectivePayload};

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
}
