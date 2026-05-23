/// Stable source-id namespace for `LocalGitSource` events.
pub const LOCAL_GIT_SOURCE_ID: &str = "proxima-code/local-git";

// Per-fact-type citation schema-ids (docs/11). file_revision and chunk
// Facts cite the same artefact (the file blob, keyed by content_sha256)
// via `code-blob-v1`, so the substrate's UNIQUE on
// `(owner, schema_id, content_hash)` deduplicates the CitedObject row
// and the chunks share a `cited_object_id` with their parent revision
// — no embedded MemoryId FK in the chunk payload.
//
// CitationMapping schemas differentiate the annotation:
// `whole` for facts that reference the whole artefact, `byte-range`
// for chunks. The byte/line ranges themselves stay on the chunk Fact
// payload (the substrate doesn't store typed CitationMapping bodies
// yet; the schema_id is currently a label, not a sidecar key).

/// CitedObject schema for a file blob (idempotency_key = blob's
/// `content_sha256`). Shared by `file-revision-v1` and `code-chunk-v1`.
pub const CODE_BLOB_SCHEMA: &str = "proxima-code/code-blob-v1";

/// CitedObject schema for a git commit object
/// (idempotency_key = blake3(commit sha)).
pub const CODE_COMMIT_OBJECT_SCHEMA: &str = "proxima-code/code-commit-object-v1";

/// CitationMapping for "this Fact references the whole blob"
/// (used by `file-revision-v1`).
pub const CODE_BLOB_WHOLE_SCHEMA: &str = "proxima-code/code-blob-whole-v1";

/// CitationMapping for "this Fact references a byte/line range of
/// the blob" (used by `code-chunk-v1`).
pub const CODE_BLOB_BYTE_RANGE_SCHEMA: &str = "proxima-code/code-blob-byte-range-v1";

/// CitationMapping for "this Fact references the whole commit object"
/// (used by `commit-v1`).
pub const CODE_COMMIT_WHOLE_SCHEMA: &str = "proxima-code/code-commit-whole-v1";

pub const EXECUTION_REQUEST_OBJECT_SCHEMA: &str = "proxima-code/execution-request-object-v1";
pub const EXECUTION_REQUEST_WHOLE_SCHEMA: &str = "proxima-code/execution-request-whole-v1";
pub const TEST_REQUEST_OBJECT_SCHEMA: &str = "proxima-code/test-request-object-v1";
pub const TEST_REQUEST_WHOLE_SCHEMA: &str = "proxima-code/test-request-whole-v1";
pub const WORKSPACE_DECISION_OBJECT_SCHEMA: &str = "proxima-code/workspace-decision-object-v1";
pub const WORKSPACE_DECISION_WHOLE_SCHEMA: &str = "proxima-code/workspace-decision-whole-v1";

#[must_use]
pub fn schema_registry() -> proxima_core::verbs::schema::FlavorRegistryFrozen {
    schema_registry_with(|_| {})
}

/// Build the code-flavor `FlavorRegistryFrozen` with extra flavor
/// registrations layered in (e.g. substrate). Used by Tauri Shell to
/// compose substrate + code into the engine's registry without forcing
/// a substrate dep on this crate's headless callers.
#[must_use]
pub fn schema_registry_with(
    extra: impl FnOnce(&mut proxima_core::FlavorRegistry),
) -> proxima_core::verbs::schema::FlavorRegistryFrozen {
    schema_registry_with_config(extra)
}

pub(crate) fn schema_registry_with_config(
    extra: impl FnOnce(&mut proxima_core::FlavorRegistry),
) -> proxima_core::verbs::schema::FlavorRegistryFrozen {
    use proxima_core::verbs::schema::{PayloadKind, SchemaInfo};
    use proxima_core::{FlavorRegistry, SchemaId, SchemaVersion};

    let mut flavor = FlavorRegistry::new();
    extra(&mut flavor);
    crate::register(&mut flavor);
    let flavor = flavor.freeze();
    let mut extra_schemas = Vec::new();

    // CitedObject schemas — file blob (shared by file_revision + chunk)
    // and commit object.
    for cited in [
        CODE_BLOB_SCHEMA,
        CODE_COMMIT_OBJECT_SCHEMA,
        EXECUTION_REQUEST_OBJECT_SCHEMA,
        TEST_REQUEST_OBJECT_SCHEMA,
        WORKSPACE_DECISION_OBJECT_SCHEMA,
    ] {
        extra_schemas.push(SchemaInfo::opaque(
            SchemaId::new(cited.into()),
            SchemaVersion::new(1),
            PayloadKind::CitedObject,
        ));
    }

    // CitationMapping schemas — typed per fact-type.
    for mapping in [
        CODE_BLOB_WHOLE_SCHEMA,
        CODE_BLOB_BYTE_RANGE_SCHEMA,
        CODE_COMMIT_WHOLE_SCHEMA,
        EXECUTION_REQUEST_WHOLE_SCHEMA,
        TEST_REQUEST_WHOLE_SCHEMA,
        WORKSPACE_DECISION_WHOLE_SCHEMA,
    ] {
        extra_schemas.push(SchemaInfo::opaque(
            SchemaId::new(mapping.into()),
            SchemaVersion::new(1),
            PayloadKind::CitationMapping,
        ));
    }

    flavor.with_additional_schemas(extra_schemas)
}
