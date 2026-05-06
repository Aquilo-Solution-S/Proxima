//! Proxima code flavor — Rust + TypeScript cAST chunker and Fact schemas.
//!
//! See docs/08 for flavor architecture.

pub mod calls;
pub mod chunker;
pub mod ingest;
pub mod local_git_source;
pub mod mcp;
pub mod migrations;
pub mod operators;
pub mod payloads;
pub mod repos;

pub use ingest::{
    CODE_BLOB_BYTE_RANGE_SCHEMA, CODE_BLOB_SCHEMA, CODE_BLOB_WHOLE_SCHEMA,
    CODE_COMMIT_OBJECT_SCHEMA, CODE_COMMIT_WHOLE_SCHEMA, IngestError, LOCAL_GIT_SOURCE_ID,
    build_engine, build_engine_with, ingest_code_chunk, ingest_commit, ingest_file_revision,
};
pub use local_git_source::{IndexError, IndexReport, IngestProgress, LocalGitSource};
pub use migrations::migrator;
pub use operators::{CommitSummaryOperator, f2a_operator_registry};
pub use payloads::{
    CodeChunkV1, CommitSummaryV1, CommitV1, EdgeCallsV1, FileRevisionV1, FileState,
};
pub use repos::{
    RepoEraseReceipt, RepoIngestionRun, RepoRecord, RepoRegistryError, RunStage, RunStatus,
    StageCounters, advance_stage, begin_run, delete_repo, erase_repo, get_active_run, get_repo,
    get_run, list_repos, mark_failed, mark_succeeded, register_repo, start_run,
    start_run_with_created, sweep_orphaned_runs, update_cursor,
};

use proxima_core::{RelationClass, RelationDescriptor, SchemaId, SchemaRef, SchemaVersion};

proxima_core::proxima_flavor! {
    name = "proxima-code",
    fact_schemas = [
        payloads::CommitV1,
        payloads::FileRevisionV1,
        payloads::CodeChunkV1,
    ],
    abstraction_schemas = [
        payloads::CommitSummaryV1,
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
        ),
    ],
    mcp_tools = [
        mcp::CodeSearchChunksTool,
        mcp::CodeOpenFileRevisionTool,
        mcp::CodeSearchCommitsTool,
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
        // Abstraction schemas
        assert!(schema_ids.contains("proxima-code/commit-summary-v1"));
        // Edge schemas
        assert!(schema_ids.contains("proxima-code/calls"));

        // Check relations
        let relations = frozen.list_relations();
        let relation_ids: HashSet<_> = relations.iter().map(|r| r.relation.as_str()).collect();
        assert!(relation_ids.contains("proxima-code/calls"));
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
        assert!(names.contains("proxima-code/code_search_chunks"));
        assert!(names.contains("proxima-code/code_open_file_revision"));
        assert!(names.contains("proxima-code/code_search_commits"));
    }
}
