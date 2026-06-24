/// Stable source-id namespace for `LocalGitSource` events.
pub const LOCAL_GIT_SOURCE_ID: &str = "proxima-code/local-git";

// Per-fact-type citation schema-ids (docs/11). Raw file-revision
// Facts cite the observed file blob keyed by `content_sha256`.
// Derived code-slice Abstractions do not carry citations; provenance
// closes to file/commit Facts through `core/derived-from` edges.

/// CitedObject schema for a file blob (idempotency_key = blob's
/// `content_sha256`). Used by `file-revision-v1`.
pub const CODE_BLOB_SCHEMA: &str = "proxima-code/code-blob-v1";

/// CitedObject schema for a git commit object
/// (idempotency_key = blake3(commit sha)).
pub const CODE_COMMIT_OBJECT_SCHEMA: &str = "proxima-code/code-commit-object-v1";

/// CitationMapping for "this Fact references the whole blob"
/// (used by `file-revision-v1`).
pub const CODE_BLOB_WHOLE_SCHEMA: &str = "proxima-code/code-blob-whole-v1";

/// CitationMapping for "this Fact references the whole commit object"
/// (used by `commit-v1`).
pub const CODE_COMMIT_WHOLE_SCHEMA: &str = "proxima-code/code-commit-whole-v1";

pub const EXECUTION_REQUEST_OBJECT_SCHEMA: &str = "proxima-code/execution-request-object-v1";
pub const EXECUTION_REQUEST_WHOLE_SCHEMA: &str = "proxima-code/execution-request-whole-v1";
pub const ACCEPTANCE_CRITERIA_OBJECT_SCHEMA: &str = "proxima-code/acceptance-criteria-object-v1";
pub const ACCEPTANCE_CRITERIA_WHOLE_SCHEMA: &str = "proxima-code/acceptance-criteria-whole-v1";
pub const TEST_REQUEST_OBJECT_SCHEMA: &str = "proxima-code/test-request-object-v1";
pub const TEST_REQUEST_WHOLE_SCHEMA: &str = "proxima-code/test-request-whole-v1";
pub const EXECUTION_RESULT_OBJECT_SCHEMA: &str = "proxima-code/execution-result-object-v1";
pub const EXECUTION_RESULT_WHOLE_SCHEMA: &str = "proxima-code/execution-result-whole-v1";
pub const TEST_RESULT_OBJECT_SCHEMA: &str = "proxima-code/test-result-object-v1";
pub const TEST_RESULT_WHOLE_SCHEMA: &str = "proxima-code/test-result-whole-v1";
pub const ACCEPTANCE_VERIFICATION_OBJECT_SCHEMA: &str =
    "proxima-code/acceptance-verification-object-v1";
pub const ACCEPTANCE_VERIFICATION_WHOLE_SCHEMA: &str =
    "proxima-code/acceptance-verification-whole-v1";

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
    let mut flavor = proxima_core::FlavorRegistry::new();
    extra(&mut flavor);
    crate::register(&mut flavor);
    flavor.freeze()
}
