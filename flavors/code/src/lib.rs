//! Proxima code flavor — Rust + TypeScript cAST chunker and Fact schemas.
//!
//! See docs/08 for flavor architecture.

pub mod chunker;
pub mod ingest;
pub mod local_git_source;
pub mod migrations;
pub mod payloads;

pub use ingest::{
    build_engine, ingest_code_chunk, ingest_commit, ingest_file_revision,
    CITATION_MAPPING_SCHEMA, CITED_OBJECT_SCHEMA, IngestError, LOCAL_GIT_SOURCE_ID,
};
pub use local_git_source::{IndexError, IndexReport, LocalGitSource};
pub use migrations::migrator;
pub use payloads::{
    CommitV1, CodeChunkV1, FileRevisionV1, FileState,
};

proxima_core::proxima_flavor! {
    name = "proxima-code",
    fact_schemas = [
        payloads::CommitV1,
        payloads::FileRevisionV1,
        payloads::CodeChunkV1,
    ],
}

#[cfg(test)]
mod tests {
    use proxima_core::FlavorRegistry;
    use std::collections::HashSet;

    #[test]
    fn registry_contains_all_three_schemas() {
        let mut registry = FlavorRegistry::new();
        super::register(&mut registry);
        let frozen = registry.freeze();

        let schemas = frozen.list();
        let schema_ids: HashSet<_> = schemas
            .iter()
            .map(|s| s.schema_id.as_str())
            .collect();

        assert!(schema_ids.contains("proxima-code/commit-v1"));
        assert!(schema_ids.contains("proxima-code/file-revision-v1"));
        assert!(schema_ids.contains("proxima-code/code-chunk-v1"));
    }
}
