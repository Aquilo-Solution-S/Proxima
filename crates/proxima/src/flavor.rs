//! Flavor SDK exports.

pub use crate::bundle::FlavorBundle;
pub use crate::migrations::NamedMigrator;
/// Background-worker surface for [`FlavorBundle::spawn_workers`]: the
/// runtime handles a spawning flavor receives and the named join handle
/// it returns.
pub use crate::workers::{FlavorWorker, FlavorWorkerContext};
/// The typed artefact inside [`CitedBlobStaged`], and the outcome of
/// [`proxima_core::Engine::complete_upload_as_fact`].
///
/// NAMING A TYPE IS NOT ENOUGH TO RETURN ONE. `stage_upload` returns
/// `CitedBlobStaged`, whose `payload` field is an
/// [`UploadedBlobPayload`] — a struct with no constructor, so an
/// out-of-tree flavor that could name the outer type still could not
/// build one, and the port was unimplementable for exactly as long as
/// this line was missing. That is the recurring shape of a facade gap
/// here: the blocker is never the trait, it is a field type one level
/// down that no `use` can reach. The tier test below constructs the
/// struct rather than only naming it, because only construction
/// exercises the difference.
///
/// [`UploadCompleted`] rides along for the caller's half of the same
/// verb: without it the result of a completion cannot be bound to a
/// named local or returned from a flavor's own function.
pub use proxima_core::citations::UploadedBlobPayload;
pub use proxima_core::engine::UploadCompleted;
/// MCP tool-authoring surface: implement [`McpTool`] with typed
/// [`McpToolCtx`] / [`McpToolError`] instead of reaching into
/// `proxima_core::mcp`. Mirrors what `docs/tutorials/add-first-mcp-tool.md`
/// imports.
///
/// [`McpToolExtensions`] is the seam for anything core cannot name. A host
/// composes its flavors' services into it by overriding
/// `FlavorApp::mcp_tool_extensions`, and a tool resolves one back with
/// `ctx.extensions.get::<MyService>()` — which is exactly how core's own
/// `core_upload` finds the [`CitedBlobService`] below. It is re-exported
/// here because it is the *return type* of that override: without it an
/// out-of-tree flavor cannot write the signature at all, so its tools have
/// no sanctioned route to a database handle or any other host-owned
/// dependency. See `docs/09-developing-flavors.md` § MCP Tools.
///
/// [`McpPresentationExt`] is how a flavor implementing the transport-neutral
/// [`Tool`] trait mints and parses MCP wire references (`F:`/`A:`/`P:`/`G:`
/// prefixed uuids — there is no edge prefix, because an edge has no id).
/// [`McpToolCtx`] carries those as inherent methods,
/// but [`Tool`] is handed a [`ToolCtx`], which deliberately knows nothing
/// about the wire; importing this trait is the sanctioned bridge. Without it
/// each flavor writes the same twelve-method forwarding shim over
/// [`McpToolPresentation`] — `flavors/code` carried one until core took it
/// over.
pub use proxima_core::mcp::{
    McpActionArgSpec, McpAuthorContext, McpPresentationExt, McpTool, McpToolAnnotations,
    McpToolCtx, McpToolError, McpToolErrorKind, McpToolExtensions, McpToolPresentation,
};
/// Host-wired cited-blob lane, handed to workers as
/// [`FlavorWorkerContext::blobs`]. Present only when the host configured
/// S3; the concrete backend (`proxima-blob-s3`) is never named across
/// this seam, so a flavor codes against [`CitedBlobPort`] and can fake it
/// wholesale in tests.
///
/// [`CitedBlobHeld`] and [`MAX_HELD_BLOB_DIGESTS`] are the two halves of
/// `find_held_blobs`, and both have to cross this seam for the same reason:
/// a flavor faking the port must be able to RETURN the outcome type, and a
/// flavor batching its digests must be able to read the bound it is being
/// held to rather than hardcode a copy that drifts from it.
pub use proxima_core::storage_ports::{
    CitedBlobHeld, CitedBlobPort, CitedBlobReadUrl, CitedBlobService, CitedBlobStaged,
    CitedBlobUploadAborted, CitedBlobUploadCompleted, CitedBlobUploadHeader,
    CitedBlobUploadPrepared, MAX_HELD_BLOB_DIGESTS,
};
/// [`FactTombstone`] is the return type of [`FactPayload::tombstone`], so a
/// flavor that declares a *stateful* Fact schema — one with a head per
/// natural key and an explicit deletion observation — cannot write that
/// override without it. The in-tree precedent (`flavors/code`'s
/// `FileRevisionV1`) reaches it through a direct `proxima-core` dependency
/// an out-of-tree flavor does not have. Without the override a schema can
/// still declare `natural_key_columns`, but storage has no discriminator
/// for `PresentOnly` queries, so a deleted entity stays a live head
/// forever.
pub use proxima_core::{
    AbstractionPayload, CapabilitySet, CitationMappingPayload, CitedObjectPayload, FactPayload,
    FactReceiptId, FactTombstone, FlavorDescriptor, FlavorProvenance, FlavorRegistry,
    FlavorRegistryError, FlavorRegistryFrozen, GoalId, GoalPayload, InputContractId, MemoryId,
    ModelId, OperatorId, PayloadKeyBuilder, PerspectivePayload, PromptVersion, SchemaId,
    SchemaVersion, SearchProjection, SearchProjectionColumnKind, SearchProjectionField,
    SidecarPayload, Tool, ToolCall, ToolCallFn, ToolCtx, ToolDescriptor, ToolError, ToolOrigin,
    ToolServices, proxima_flavor, proxima_schema_id,
};
/// Derived-memory authoring: the request/outcome types of
/// [`proxima_core::Engine::author_derived_authorized`], which is how a
/// flavor writes the Abstractions and Perspectives its
/// [`AbstractionPayload`] / [`PerspectivePayload`] schemas describe.
///
/// Without these the SDK could only *declare* derived schemas, never
/// populate them: an out-of-tree flavor depends on `proxima` alone, and
/// the in-tree precedent (`flavors/code`) reaches the same lane through a
/// direct `proxima-storage-pg` dependency it cannot have.
///
/// Provenance is `derived_from` on the request: a slice of [`EdgeEndpoint`]s
/// naming what the write was made from, which lands one
/// [`EdgeKind::Origin`] row each inside the write's own transaction. A
/// re-derivation that replaces an earlier output sets `supersedes`, which
/// is a lineage pointer on the two rows and writes no edge at all.
///
/// A derived memory is embedded *synchronously*, inside the write, where a
/// Fact enqueues a durable job — but the two now agree about failure. A
/// text the provider refuses (or dies on) leaves the memory written with
/// no vector and an embedding job enqueued in the same transaction, and
/// [`AuthorDerivedAuthorizedOutcome::embedding_deferred`] says so; the
/// memory is lexically findable and not semantically findable until a
/// drain runs. Only a provider that fails a liveness probe — one that is
/// genuinely unavailable — still fails the write. A flavor deriving many
/// memories in a worker should checkpoint per output rather than per
/// batch.
pub use proxima_core::{
    AuthorDerivedAuthorizedOutcome, AuthorDerivedRequestInput, EntityKind, MemoryOperatorKind,
};
/// The connection vocabulary a flavor is allowed to speak (docs/16-edges.md).
///
/// A flavor never writes an edge, so none of these is a write surface.
/// [`PayloadReference`] is the whole of what a payload declares — the field
/// it was read from, the binding, and the target — and the defaulted
/// `references()` on every payload trait returns a `Vec` of them, so a
/// schema that points at another node cannot be written without naming this
/// type. [`ReferenceBinding`] is where the retired descriptor's
/// `FollowHead`/`Pin` cell went: a property of the field, decided once by
/// the schema author.
///
/// [`EdgeEndpoint`] is the address form the constructors below mint
/// (`memory`, `goal`, `fact_entity`); [`EdgeKind`] is exported to be *read*
/// — off a listed [`Edge`], or when filtering — never passed to a writer,
/// because the kind follows the operation. [`FactEntityId`] rides along
/// because `PayloadReference::fact_entity_head` cannot be called without it.
pub use proxima_core::{
    Edge, EdgeEndpoint, EdgeKind, EdgeTargetProjection, EntityRef, FactEntityId, PayloadReference,
    ReferenceBinding,
};
/// Shared argument rules for search and paged reads, so a flavor does not
/// have to invent its own — and so the in-tree tools cannot drift apart.
///
/// [`reject_zero_limit`]: every paged read faces the same question, and
/// the in-tree tools once answered it three different ways: one rejected
/// `limit: 0`, one returned a well-formed empty page indistinguishable
/// from "nothing matched", and one clamped to 1 and answered a question
/// nobody asked. The engine has rejected zero from the start; this is how
/// a flavor agrees with it in one line. Upper bounds stay the flavor's own
/// business — clamping those still serves the caller's intent.
///
/// [`validate_search_query`] and [`MAX_QUERY_CHARS`]: three tools carried
/// a byte-identical copy of the same check with `512` inlined in each.
///
/// [`MAX_TEXT_CAP_CHARS`] is the ceiling on a caller-supplied cap over
/// returned text (`body_max_chars`, `snippet_max_chars`). The *default*
/// under it is deliberately not shared — how much of a code chunk versus a
/// memory body is worth returning is a property of the object.
pub use proxima_core::{
    MAX_QUERY_CHARS, MAX_TEXT_CAP_CHARS, reject_zero_limit, validate_search_query,
};
pub use proxima_storage_pg::pg_sidecar;
pub use proxima_storage_pg::sidecars::{
    PgCitationMappingSidecar, PgCitedObjectSidecar, PgGoalSidecar, PgMemoryPayload,
    PgMemoryPayloadFuture, PgMemorySidecar, PgSidecarFuture, PgSidecarReadCtx,
};
pub use proxima_storage_pg::{
    PgSidecarRegistry, PgSidecarRegistryFrozen, register_core_pg_sidecars,
};

mod authorized_read;
pub use authorized_read::{
    authorized_abstraction_payloads, authorized_code_chunk_head_candidates,
    authorized_fact_payloads, authorized_fact_payloads_include_tombstones, authorized_memory_ids,
    nearest_code_chunk_candidates,
};
pub use proxima_storage_pg::query::{CodeChunkVectorCandidate, CodeChunkVectorFilters};
