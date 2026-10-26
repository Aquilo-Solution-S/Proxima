//! Proxima engine core.
extern crate self as proxima_core;

/// The Proxima release this build belongs to, as reported to MCP clients on
/// `initialize` and to operators.
///
/// Deliberately not `CARGO_PKG_VERSION`: workspace crates are `0.1.0` with
/// `publish = false`. Releases are git tags; bump this when cutting one.
pub const RELEASE_VERSION: &str = "0.0.62";

pub mod access;
pub mod auth;
pub mod authz;
pub mod canonical_json;
pub mod capability;
pub mod citations;
pub mod cold;
pub mod count;
pub mod cursor;
pub mod edge;
pub mod engine;
pub mod entity;
pub mod env;
pub mod error;
pub mod flavor;
pub mod goal;
pub mod ids;
pub mod lexical_language;
pub mod llm;
pub mod mcp;
pub mod memory;
mod memory_uuid;
pub mod models;
pub mod net;
pub mod operator_proofs;
pub mod owner;
pub mod owner_inverse;
pub mod owner_scope;
pub mod payload;
pub mod protocol;
pub mod publication;
pub mod quiesce;
pub mod read_models;
pub mod scope;
mod stage_timing;
pub mod storage;
pub mod storage_ports;
// Also under plain `cfg(test)`: the in-crate suites use the same probe
// payloads and probe flavor the storage backend's tests do, and a fixture
// that existed only under a feature flag would be a second copy.
#[cfg(any(test, feature = "test-fixtures"))]
pub mod test_fixtures;
pub mod text_bounds;
pub mod tool;
pub mod verbs;
pub mod write_wake;

pub use access::*;
pub use auth::*;
pub use authz::*;
pub use canonical_json::canonical_json_bytes;
pub use capability::*;
pub use citations::*;
pub use cold::{
    ColdObjectStore, MAX_OBJECT_STORE_ID_BYTES, MAX_OPERATOR_STORE_ID_BYTES, ObjectStoreId,
    ObjectStoreIdError, PurgeDeleted,
};
pub use count::count_u64;
pub use cursor::*;
pub use edge::*;
pub use engine::*;
pub use entity::*;
pub use env::{env_from_pairs, env_value, parse_bool_word, process_env};
pub use error::*;
pub use flavor::*;
pub use goal::{SimpleTextGoalV1, TaskGoalV1, TaskPriority};
pub use ids::*;
pub use llm::*;
pub use mcp::{
    CoreActionMeta, JudgedKey, KeyDeclaration, MAX_OPERATOR_LABEL_CHARS, McpAuthorContext,
    McpCallFn, McpHostToolCall, McpTool, McpToolAnnotations, McpToolAudience, McpToolCtx,
    McpToolDescriptor, McpToolError, McpToolErrorKind, McpToolOrigin, McpUnknownFieldPolicy,
    MemoryHandleClass, Next, OperatorLabelConflict, Replay, RequestBehavior, ScopeGateBehavior,
    TerminalDispatch, ToolContent, ToolDescriptorView, ToolEffect, ToolReply, ToolSource,
    UNKNOWN_OPERATOR_LABEL, all_core_actions, all_core_resources, canonical_scope_keys,
    canonical_scope_keys_excluding, canonical_served_read_keys, core_action_meta,
    how_to_resource_declaration, how_to_resource_key, operator_label, provider_safe_tool_name,
    resolve_operator_label, tool_name_matches,
};
pub use memory::*;
pub use memory_uuid::{memory_store_number, new_memory_uuid, new_memory_uuid_after};
pub use models::*;
pub use net::*;
pub use operator_proofs::*;
pub use owner::*;
pub use owner_inverse::{
    OwnerEraseCounts, OwnerEraseOutcome, OwnerEraseRefusal, OwnerEraseRequest, OwnerEraseTarget,
    OwnerExportBundle, OwnerExportRequest, OwnerExportTarget,
};
pub use owner_scope::OwnerScope;
pub use payload::*;
pub use publication::{
    CLOUDEVENTS_DATA_CONTENT_TYPE, CLOUDEVENTS_SPEC_VERSION, DEFAULT_MAX_PAYLOAD_BYTES,
    DEFAULT_MAX_PENDING, ExtensionValue, MAX_EXTENSION_NAME_CHARS, MAX_EXTENSION_VALUE_BYTES,
    MAX_PUBLICATION_EXTENSIONS, PublicationConfig, PublicationDraft, PublicationError,
    PublicationExtensions, PublicationExtensionsError, PublicationLimits, PublicationPlan,
    PublicationSource, PublicationSourceError, SealedPublication, data_schema_uri,
};
pub use quiesce::{FeatureControlError, FeatureStatus, GateWorker, QuiesceGate};
pub use read_models::*;
pub use scope::{ScopeDecl, ScopeKind, ScopeRef, scope_set};
pub use storage::*;
pub use text_bounds::*;
pub use tool::*;

pub use verbs::fact_ingest::{
    AuthorizedFactWithCitation, AuthorizedFactWithCitationRef, AuthorizedFactWrite,
    FactIngestOutcome, FactReceiptDraft, FactWriteCommand, IngestReceipt, OCCURRED_AT_FUTURE_BOUND,
};
pub use verbs::goal_write::{
    GoalAssignmentTarget, GoalDependencyRef, GoalEvidenceRef, GoalTopologyWrite, IdempotencyKey,
};
pub use verbs::idempotency::{
    ContentFingerprint, KeyConflict, KeyScope, KeyVerdict, RequestKey, judge,
};
pub use verbs::own_erase::{
    EraseMode, MAX_ERASE_SERIES_PER_CALL, MAX_ERASE_VERSIONS_PER_CALL, SeriesEraseError,
    SeriesEraseReceipt, SeriesEraseRefusal, SeriesEraseRefusalKind, SeriesSelection,
};
pub use verbs::*;

pub use verbs::schema::{FlavorRegistryFrozen, composed_schema_names, sidecar_tables};

/// Expands to `&'static str` with the calling crate's name
/// prefixed. Per docs/host-integration.md
///
/// `proxima_schema_id!("note")` in a crate named
/// `proxima-notes` evaluates to `"proxima-notes/note"`.
#[macro_export]
macro_rules! proxima_schema_id {
    ($short:literal) => {
        ::std::concat!(::std::env!("CARGO_PKG_NAME"), "/", $short)
    };
}

/// Build-time registration macro. v1 subset — supports
/// `fact_schemas`, `abstraction_schemas`, `perspective_schemas`,
/// `goal_schemas`, `cited_object_schemas`, `opaque_cited_object_schemas`,
/// `schema_capability_tags`, `mcp_tools`.
/// Expands to a
/// `pub fn register(registry: &mut FlavorRegistry) -> Result<(), FlavorRegistryError>`
/// that adds each schema.
///
/// There is no `relations` or `edge_schemas` arm: edge kinds are a
/// closed core vocabulary (docs/concepts.md) and edges carry no
/// payload. A flavor connects nodes by declaring reference fields on its
/// payloads, or by authoring an interpretation node.
///
/// Schema and `mcp_tools` prefixes resolve to associated `const`s or
/// literals, so they are checked by a `const` assertion — a misprefix
/// fails the build.
///
/// Embedding capability types live in `crate::models` (`EmbedCaps`).
/// Hosts inject [`crate::llm::EmbeddingClient`] at boot; this macro does
/// not bind models, and there is no inference-target registry.
///
/// Unknown keys are rejected at expansion time.
///
/// ```ignore
/// proxima_flavor! {
///     name = "proxima-notes",
///     fact_schemas = [ NoteV1, NoteEditV1 ],
///     cited_object_schemas = [ AttachmentV1 ],
///     opaque_cited_object_schemas = [ "proxima-notes/attachment-blob-v1" ],
///     mcp_tools = [ MyTool ],
/// }
/// ```
///
#[macro_export]
macro_rules! proxima_flavor {
    // Internal: register one schema-kind list, compile-checking each
    // `SCHEMA_ID` carries the flavor prefix. The check is a `const`
    // assertion — a misprefixed `SCHEMA_ID` fails the build, not the
    // first boot.
    (@schemas $registry:ident $name:literal $trait:ident $add:ident [ $($ty:ty),* $(,)? ]) => {
        $(
            const _: () = ::std::assert!(
                $crate::schema_id_has_prefix(
                    <$ty as $crate::$trait>::SCHEMA_ID,
                    ::std::concat!($name, "/"),
                ),
                ::std::concat!(
                    ::std::stringify!($trait), " SCHEMA_ID for ",
                    ::std::stringify!($ty),
                    " must start with \"", $name, "/\"",
                ),
            );
            $registry.$add::<$ty>()?;
        )*
    };
    // Internal: register opaque cited-object schemas that intentionally
    // have no Rust payload type or sidecar table.
    (@opaque_schemas $registry:ident $name:literal [ $($schema_id:expr),* $(,)? ]) => {
        $(
            {
                let schema_id: &str = $schema_id;
                let schema_id = $crate::SchemaId::new(schema_id.to_string());
                if !schema_id.as_str().starts_with(::std::concat!($name, "/")) {
                    return ::std::result::Result::Err($crate::FlavorRegistryError::SchemaIngressMismatch {
                        schema_id,
                        schema_version: $crate::SchemaVersion::new(1),
                        kind: $crate::verbs::schema::PayloadKind::CitedObject,
                    });
                }
                $registry.add_opaque_cited_object_schema(
                    schema_id,
                    $crate::SchemaVersion::new(1),
                );
            }
        )*
    };
    (@schema_capability_tags $registry:ident $name:literal [ $(($kind:ident, $ty:ty) => [ $($tag:expr),* $(,)? ]),* $(,)? ]) => {
        $(
            $crate::proxima_flavor!(@schema_capability_tag $registry $name $kind $ty [ $($tag),* ]);
        )*
    };
    (@schema_capability_tag $registry:ident $name:literal Fact $ty:ty [ $($tag:expr),* $(,)? ]) => {
        const _: () = ::std::assert!(
            $crate::schema_id_has_prefix(
                <$ty as $crate::FactPayload>::SCHEMA_ID,
                ::std::concat!($name, "/"),
            ),
            ::std::concat!(
                "FactPayload SCHEMA_ID for ",
                ::std::stringify!($ty),
                " must start with \"", $name, "/\"",
            ),
        );
        $registry.try_add_schema_capability_tags(
            $crate::verbs::schema::PayloadKind::Fact,
            <$ty as $crate::FactPayload>::schema_id(),
            $crate::SchemaVersion::new(<$ty as $crate::FactPayload>::SCHEMA_VERSION),
            [ $($tag),* ],
        )?;
    };
    (@schema_capability_tag $registry:ident $name:literal Abstraction $ty:ty [ $($tag:expr),* $(,)? ]) => {
        const _: () = ::std::assert!(
            $crate::schema_id_has_prefix(
                <$ty as $crate::AbstractionPayload>::SCHEMA_ID,
                ::std::concat!($name, "/"),
            ),
            ::std::concat!(
                "AbstractionPayload SCHEMA_ID for ",
                ::std::stringify!($ty),
                " must start with \"", $name, "/\"",
            ),
        );
        $registry.try_add_schema_capability_tags(
            $crate::verbs::schema::PayloadKind::Abstraction,
            <$ty as $crate::AbstractionPayload>::schema_id(),
            $crate::SchemaVersion::new(<$ty as $crate::AbstractionPayload>::SCHEMA_VERSION),
            [ $($tag),* ],
        )?;
    };
    (@schema_capability_tag $registry:ident $name:literal Perspective $ty:ty [ $($tag:expr),* $(,)? ]) => {
        const _: () = ::std::assert!(
            $crate::schema_id_has_prefix(
                <$ty as $crate::PerspectivePayload>::SCHEMA_ID,
                ::std::concat!($name, "/"),
            ),
            ::std::concat!(
                "PerspectivePayload SCHEMA_ID for ",
                ::std::stringify!($ty),
                " must start with \"", $name, "/\"",
            ),
        );
        $registry.try_add_schema_capability_tags(
            $crate::verbs::schema::PayloadKind::Perspective,
            <$ty as $crate::PerspectivePayload>::schema_id(),
            $crate::SchemaVersion::new(<$ty as $crate::PerspectivePayload>::SCHEMA_VERSION),
            [ $($tag),* ],
        )?;
    };
    (@schema_capability_tag $registry:ident $name:literal Goal $ty:ty [ $($tag:expr),* $(,)? ]) => {
        const _: () = ::std::assert!(
            $crate::schema_id_has_prefix(
                <$ty as $crate::GoalPayload>::SCHEMA_ID,
                ::std::concat!($name, "/"),
            ),
            ::std::concat!(
                "GoalPayload SCHEMA_ID for ",
                ::std::stringify!($ty),
                " must start with \"", $name, "/\"",
            ),
        );
        $registry.try_add_schema_capability_tags(
            $crate::verbs::schema::PayloadKind::Goal,
            <$ty as $crate::GoalPayload>::schema_id(),
            $crate::SchemaVersion::new(<$ty as $crate::GoalPayload>::SCHEMA_VERSION),
            [ $($tag),* ],
        )?;
    };
    (
        name = $name:literal
        $(, display_name = $display_name:literal)?
        $(, fact_schemas = [ $($fact:ty),* $(,)? ])?
        $(, abstraction_schemas = [ $($abs:ty),* $(,)? ])?
        $(, perspective_schemas = [ $($persp:ty),* $(,)? ])?
        $(, goal_schemas = [ $($goal:ty),* $(,)? ])?
        $(, cited_object_schemas = [ $($cited:ty),* $(,)? ])?
        $(, opaque_cited_object_schemas = [ $($opaque_cited:expr),* $(,)? ])?
        $(, schema_capability_tags = [ $(($cap_kind:ident, $cap_ty:ty) => [ $($cap_tag:expr),* $(,)? ]),* $(,)? ])?
        $(, mcp_tools = [ $($tool:ty),* $(,)? ])?
        $(, contract = $contract:expr)?
        $(,)?
    ) => {
        /// Generated by `proxima_flavor!`. Composite binaries
        /// call this once per linked flavor at startup.
        pub fn register(registry: &mut $crate::FlavorRegistry) -> ::std::result::Result<(), $crate::FlavorRegistryError> {
            {
                #[allow(unused_assignments, unused_mut)]
                let mut display_name: &str = $name;
                $(display_name = $display_name;)?
                let author: ::std::option::Option<::std::string::String> =
                    ::std::option_env!("CARGO_PKG_AUTHORS")
                        .filter(|s: &&str| !s.is_empty())
                        .map(|s: &str| {
                            s.split(':').next().unwrap_or(s).trim().to_string()
                        });
                registry.try_add_flavor($crate::FlavorDescriptor {
                    flavor_id: $name.to_string(),
                    display_name: display_name.to_string(),
                    package_version: ::std::env!("CARGO_PKG_VERSION").to_string(),
                    author,
                    provenance: $crate::FlavorProvenance::Builtin,
                })?;
            }
            $($crate::proxima_flavor!(@schemas registry $name
                FactPayload try_add_fact_schema [ $($fact),* ]);)?
            $($crate::proxima_flavor!(@schemas registry $name
                AbstractionPayload try_add_abstraction_schema [ $($abs),* ]);)?
            $($crate::proxima_flavor!(@schemas registry $name
                PerspectivePayload try_add_perspective_schema [ $($persp),* ]);)?
            $($crate::proxima_flavor!(@schemas registry $name
                GoalPayload try_add_goal_schema [ $($goal),* ]);)?
            $($crate::proxima_flavor!(@schemas registry $name
                CitedObjectPayload try_add_cited_object_schema [ $($cited),* ]);)?
            $($crate::proxima_flavor!(@opaque_schemas registry $name
                [ $($opaque_cited),* ]);)?
            $($crate::proxima_flavor!(@schema_capability_tags registry $name [
                $(($cap_kind, $cap_ty) => [ $($cap_tag),* ]),*
            ]);)?
            $($(
                {
                    // Tool wire names may use the "<flavor>/" namespace separator
                    // OR "<flavor>_": the latter keeps the MCP tool name valid
                    // under Anthropic's ^[a-zA-Z0-9_-]{1,64}$ rule. Schema ids
                    // (asserted elsewhere) still require "<flavor>/".
                    const _: () = ::std::assert!(
                        $crate::schema_id_has_prefix(
                            <$tool as $crate::McpTool>::NAME,
                            ::std::concat!($name, "/"),
                        ) || $crate::schema_id_has_prefix(
                            <$tool as $crate::McpTool>::NAME,
                            ::std::concat!($name, "_"),
                        ),
                        ::std::concat!(
                            "McpTool::NAME for ", ::std::stringify!($tool),
                            " must start with \"", $name, "/\" or \"", $name, "_\"",
                        ),
                    );
                    registry.try_add_tool::<$tool>($name)?;
                }
            )*)?
            // The contract lands beside the registrations it describes, so
            // a flavor cannot ship declarations and registrations that were
            // linked separately — the same reason flavor #0 registers its
            // own inside `FlavorRegistry::default()`.
            $(registry.try_add_contract($contract)?;)?
            ::std::result::Result::Ok(())
        }
    };
}

pub use storage_ports::{
    CitationPort, DelegatedAuthorityError, DelegatedAuthorityService, DelegatedCommand,
    DelegationId, DelegationIssued, DelegationRevocation, EMBEDDING_RECONCILE_DEFAULT_LIMIT,
    EmbeddingAnnObservability, EmbeddingJobBacklog, EmbeddingJobDiagnostic, EmbeddingJobPort,
    EmbeddingJobState, EmbeddingJobStateParseError, EmbeddingJobStatusCounts,
    EmbeddingMaintenancePort, EmbeddingOrphanCounts, EmbeddingOrphanSweepOutcome,
    EmbeddingPurgeOutcome, EmbeddingRecallCanary, EmbeddingReconcileOptions,
    EmbeddingReconcileOutcome, EmbeddingReconcileScope, EmbeddingSpaceCounts,
    EmbeddingSpaceCoverage, EmbeddingSpaceRole, EmbeddingTextPort, EmbeddingWriteOutcome,
    EmbeddingWritePort, FactIngestPort, GoalReadPort, GoalWritePort,
    HostStateParticipantDescriptor, HostStateParticipantId, HostStateWriteOrigin,
    HostStateWritePermit, InboundPinQuery, MAX_EMBEDDING_DIAGNOSTIC_UNITS, MemoryAuthoringPort,
    MemoryInspectPort, MemoryReadPort, OperatorMaintenanceProof, OwnerAccessReadPort,
    OwnerDropProofPort, OwnerEraseAuthorityPort, OwnerInversePort, OwnerMembershipAdminPort,
    OwnerTransferPort, PinTargetKind, SourceCursorPort, StateSurfaceName, StoragePorts,
    check_diagnostic_unit_count,
};
