use proxima_core::{AbstractionPayload, PayloadReference, ScopeKind, proxima_schema_id};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::repos::CODE_REPO_SCOPE;

use crate::payloads::file_revision::FileState;

/// The lexical language every code chunk is pinned to, on all three
/// surfaces that must agree: the schema contract's
/// `LanguagePolicy::Pinned`, which is what the projection statement
/// inlines, the stored sidecar column (the flavor baseline migration),
/// and the tsquery builders in `search_chunks.rs` (SQL literals — keep
/// them equal to this). The ingest draft is deliberately NOT one of them:
/// a pinned schema reads no language off the write.
/// Code is not prose in the deployment's language: identifiers,
/// keywords, and comments are English-dominant, and following the
/// database default would retokenise code search as collateral of a
/// `set_lexical_config` switch.
pub const CODE_LEXICAL_LANGUAGE: &str = "english";

/// One place in this chunk where a call expression appears. Byte offsets
/// are file-level, as the tree-sitter extraction reports them.
///
/// A site is not a connection. Ten sites pointing at the same callee are
/// ten entries here and exactly one row in the index — the index answers
/// "is there a connection", this answers "what is it" (docs/16 §The Model).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CodeCallSiteV1 {
    #[schemars(description = "File-level byte offset where the call expression starts.")]
    pub byte_start: u32,
    #[schemars(description = "File-level byte offset where the call expression ends.")]
    pub byte_end: u32,
    #[schemars(
        description = "Identifier the call names — the rightmost segment for path and method calls."
    )]
    pub callee_name: String,
    #[schemars(
        description = "True when the syntactic call form is method-style (`obj.method(...)`)."
    )]
    pub is_dynamic: bool,
}

/// Every call this chunk makes into one callee chunk.
///
/// `callee_memory_id` is the schema-declared reference field: ingest reads
/// it and asserts one `reference` index row per entry, in the chunk's own
/// write transaction. The callee is a field and the site data is in
/// `sites`, so the index row carries nothing of its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CodeCallV1 {
    #[schemars(description = "Memory id of the callee `code-chunk-v1` Abstraction.")]
    pub callee_memory_id: uuid::Uuid,
    #[schemars(description = "Call sites in this chunk that reach that callee, in file order.")]
    pub sites: Vec<CodeCallSiteV1>,
}

/// Derived code-slice projection produced by the local-git F→A operator
/// over `file-revision-v1` Facts. It is code intelligence, not an
/// external observation: identity is scoped to the source file revision
/// plus slice index, and provenance lands as `origin` index rows back to
/// the file/commit Facts the write declared it was made from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CodeChunkV1 {
    pub repo_id: uuid::Uuid,
    pub file_path: String,
    pub chunk_index: u32,
    pub text: String,
    pub language: Option<String>,
    pub chunk_type: String,
    pub byte_range_start: u32,
    pub byte_range_end: u32,
    pub line_range_start: u32,
    pub line_range_end: u32,
    pub state: FileState,
    /// Callees this chunk calls, one entry per callee chunk. Resolution is
    /// intra-file; a call whose callee is not a chunk of the same file
    /// resolves to nothing and is simply not recorded.
    #[serde(default)]
    pub calls: Vec<CodeCallV1>,
}

impl AbstractionPayload for CodeChunkV1 {
    const SCHEMA_ID: &'static str = proxima_schema_id!("code-chunk-v1");
    const SCHEMA_VERSION: u32 = 1;
    /// Repo-scoped. The substrate takes the `code-repo` fence and re-asks
    /// whether the repository is still registered on EVERY admission of
    /// this payload, whoever the writer is.
    const SCOPE_KIND: Option<ScopeKind> = Some(CODE_REPO_SCOPE);
    fn scope_id(&self) -> Option<uuid::Uuid> {
        Some(self.repo_id)
    }

    fn sidecar_table() -> &'static str {
        "proxima_code.code_chunk_v1"
    }

    /// `file_path` and `text`, in that order — the exact arguments the
    /// generated `search_tsv` column is built from.
    ///
    /// `language` and `chunk_type` are one lexeme each against a chunk
    /// body's few hundred, so they never lift a result; they are already
    /// explicit filters on `proxima-code_search_chunks`. Every field listed
    /// here has to appear in the projection's vector expression.
    fn json_schema() -> Option<serde_json::Value> {
        Some(
            serde_json::to_value(schemars::schema_for!(Self))
                .expect("CodeChunkV1 schema serializes"),
        )
    }

    /// Call targets live on this sidecar (`calls`), not as kernel pins.
    /// Intra-file callees are named by series handle before their `t`
    /// exists, so they cannot be `memory.refs`.
    fn references(&self) -> Vec<PayloadReference> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use crate::contract::CODE_FLAVOR_CONTRACT;
    use proxima_core::AbstractionPayload;
    use proxima_core::flavor::SLOT_DEFAULT;

    #[test]
    fn chunk_embedding_recipe_names_the_stored_column() {
        let schema = CODE_FLAVOR_CONTRACT
            .schemas
            .iter()
            .find(|schema| schema.schema_id().as_str() == super::CodeChunkV1::SCHEMA_ID)
            .expect("code-chunk-v1 is declared");
        let units = schema.embedding.resolve(schema.sidecar_table);
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].table, Some("proxima_code.code_chunk_v1"));
        assert_eq!(units[0].column, "embed_text");
        assert_eq!(units[0].slot, SLOT_DEFAULT);
    }
}
