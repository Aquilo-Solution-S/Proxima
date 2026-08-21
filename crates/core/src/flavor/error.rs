use super::{PayloadKind, SchemaId, SchemaVersion};

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum FlavorRegistryError {
    DuplicateSchema {
        schema_id: SchemaId,
        schema_version: SchemaVersion,
        kind: PayloadKind,
    },
    DuplicateTool {
        name: &'static str,
    },
    DuplicateFlavor {
        flavor_id: String,
    },
    InvalidCapabilityTag {
        schema_id: SchemaId,
        schema_version: SchemaVersion,
        kind: PayloadKind,
        tag: String,
        message: String,
    },
    InvalidToolName {
        name: &'static str,
        expected_prefix: String,
        message: String,
    },
    DuplicateOwnerResolver,
    SchemaIngressMismatch {
        schema_id: SchemaId,
        schema_version: SchemaVersion,
        kind: PayloadKind,
    },
    OpaqueSchemaKind {
        schema_id: SchemaId,
        schema_version: SchemaVersion,
        kind: PayloadKind,
    },
    UnregisteredSchemaCapabilityTags {
        schema_id: SchemaId,
        schema_version: SchemaVersion,
        kind: PayloadKind,
    },
    /// A registered MCP tool has no resolvable behaviour declaration, so the
    /// owner-role gate cannot tell a read from a write and has to assume a
    /// write. Substrate flat tools may answer through the core manifest; a
    /// flavor flat tool has only `ANNOTATIONS`. Dispatchers resolve through
    /// their action specs and are not subject to this error.
    UndeclaredToolBehavior {
        name: &'static str,
    },
    /// A tool whose `Args` is an internally tagged enum — so its schema
    /// carries `x-proxima-actions` and MCP clients see a dispatcher —
    /// declared no `ACTION_ARG_SPECS`. Nothing then enumerates its
    /// actions: the scope gate falls back to whole-tool grants, the
    /// catalog lists none, REST serves no action route, and arguments are
    /// validated against every variant's fields merged together.
    DispatcherWithoutActionSpecs {
        name: &'static str,
    },
    /// A tool's `ACTION_ARG_SPECS` and its schemars-derived
    /// `x-proxima-actions` do not describe the same dispatcher.
    InvalidActionSpecs {
        name: &'static str,
        message: String,
    },
    /// Two flavors claim the same ordinal. Ordinals are load-bearing at
    /// runtime (unscoped search is `ordinal == 0`), so they cannot collide.
    DuplicateFlavorOrdinal {
        ordinal: u16,
        flavor_id: &'static str,
    },
    /// A flavor other than #0 declared `proxima://` resources. Resources are
    /// substrate-only: a flavor resource needs its own scope-key namespace,
    /// URI-template parser and pagination contract.
    ResourcesNotPermitted {
        flavor_id: &'static str,
    },
    /// Contracts were registered but none of them is flavor #0. Core is
    /// non-removable — the two registry-reflection resources
    /// (`proxima://schemas`, `proxima://tools`) live in its contract.
    MissingCoreContract,
    /// A contract entry's schema id does not carry its flavor's prefix.
    ContractSchemaPrefix {
        flavor_id: &'static str,
        schema_id: SchemaId,
    },
    /// A schema declared `NotTransferable` without naming where the refusal
    /// is enforced. A refusal nothing backs is a comment.
    UnenforcedTransferRefusal {
        flavor_id: &'static str,
        schema_id: SchemaId,
    },
    /// The contract declares a schema that was never registered — erase,
    /// export and forget would walk a surface no write can produce.
    ContractSchemaNotRegistered {
        flavor_id: &'static str,
        schema_id: SchemaId,
        schema_version: SchemaVersion,
        kind: PayloadKind,
    },
    /// A schema was registered under a flavor's prefix but its contract does
    /// not declare it — the drift that makes a registry walk miss a surface.
    SchemaWithoutContract {
        flavor_id: &'static str,
        schema_id: SchemaId,
        schema_version: SchemaVersion,
        kind: PayloadKind,
    },
    /// The contract names an MCP tool that was never registered.
    ContractToolNotRegistered {
        flavor_id: &'static str,
        name: &'static str,
    },
    /// One projection unit declares more distinct weight levels than
    /// `PostgreSQL` has tsvector weight classes.
    ProjectionWeightLevels {
        flavor_id: &'static str,
        schema_id: SchemaId,
        levels: usize,
        classes: usize,
    },
    /// A cited-object or citation-mapping schema declared a sidecar table.
    /// Those tables point at a blob row by convention rather than by
    /// constraint, so the shared-blob dedupe arm's remap cannot find them.
    CitationSidecarNotRemappable {
        flavor_id: &'static str,
        schema_id: SchemaId,
        table: &'static str,
    },
    /// A `LanguagePolicy::PerRow` names a projection column the projection
    /// table does not have.
    ProjectionLanguageColumn {
        flavor_id: &'static str,
        schema_id: SchemaId,
        declared: &'static str,
        projection_column: Option<&'static str>,
    },
    /// A flavor declaring `RankSource::Projection` is served by ONE
    /// statement per flavor, and a property that statement can only spell
    /// once differs between two of its projected schemas.
    ///
    /// Caught at freeze rather than at query-build time on purpose: finding
    /// out that a flavor cannot be rendered is a `StorageError` on a hot
    /// path, and the answer never depends on the request.
    ProjectionRenderNotUniform {
        flavor_id: &'static str,
        schema_id: SchemaId,
        /// `"language"` or `"bands"`.
        property: &'static str,
    },
    /// A `RankSource::Projection` schema does not declare a band under a
    /// name the core renderer resolves. A `&[Band]` is an unordered set
    /// with a `name` on each member, so the renderer's lookup is by string
    /// — and this is the check that keeps the string honest.
    ProjectionBandName {
        flavor_id: &'static str,
        schema_id: SchemaId,
        missing: &'static str,
    },
    /// A flavor claims `BandComparability::CoreBands` while one of its
    /// schemas declares a band outside flavor #0's `[0.0, 1.0]` window.
    /// The claim is what a cross-flavor merge compares scores on, so it has
    /// to be earned rather than decorated.
    ProjectionBandOutsideCoreWindow {
        flavor_id: &'static str,
        schema_id: SchemaId,
        band: &'static str,
        /// The offending window, rendered — the enum derives `Eq`, and an
        /// `f32` pair would not.
        window: String,
    },
}

impl std::fmt::Display for FlavorRegistryError {
    // Every variant renders its own message; splitting the match would put
    // half the vocabulary in a second place to forget one.
    #[allow(clippy::too_many_lines)]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateSchema {
                schema_id,
                schema_version,
                kind,
            } => write!(
                f,
                "duplicate schema registered: {schema_id} v{schema_version} {kind:?}"
            ),
            Self::DuplicateTool { name } => {
                write!(f, "duplicate tool name registered: {name}")
            }
            Self::DuplicateFlavor { flavor_id } => {
                write!(f, "duplicate flavor descriptor registered: {flavor_id}")
            }
            Self::InvalidCapabilityTag {
                schema_id,
                schema_version,
                kind,
                tag,
                message,
            } => write!(
                f,
                "schema {schema_id} v{schema_version} {kind:?} has invalid capability tag {tag:?}: {message}"
            ),
            Self::InvalidToolName {
                name,
                expected_prefix,
                message,
            } => write!(
                f,
                "tool name {name:?} is invalid for prefix {expected_prefix:?}: {message}"
            ),
            Self::DuplicateOwnerResolver => f.write_str("duplicate owner resolver registered"),
            Self::SchemaIngressMismatch {
                schema_id,
                schema_version,
                kind,
            } => write!(
                f,
                "schema {schema_id} v{schema_version} {kind:?} has mismatched typed-ingress registration"
            ),
            Self::OpaqueSchemaKind {
                schema_id,
                schema_version,
                kind,
            } => write!(
                f,
                "schema {schema_id} v{schema_version} {kind:?} is opaque; only CitedObject and CitationMapping schemas may be opaque"
            ),
            Self::UnregisteredSchemaCapabilityTags {
                schema_id,
                schema_version,
                kind,
            } => write!(
                f,
                "schema capability tags reference unregistered schema: {schema_id} v{schema_version} {kind:?}"
            ),
            Self::UndeclaredToolBehavior { name } => write!(
                f,
                "tool {name} declares no ANNOTATIONS, so the owner-role gate cannot tell a read \
                 from a write and will demand write access; set `const ANNOTATIONS` on the tool"
            ),
            Self::DispatcherWithoutActionSpecs { name } => write!(
                f,
                "tool {name} has an internally tagged `Args` (its schema carries \
                 x-proxima-actions) but declares no ACTION_ARG_SPECS, so nothing enumerates its \
                 actions: set `const ACTION_ARG_SPECS` on the tool, or give it a plain struct \
                 `Args`"
            ),
            Self::InvalidActionSpecs { name, message } => {
                write!(
                    f,
                    "tool {name} has inconsistent ACTION_ARG_SPECS: {message}"
                )
            }
            Self::DuplicateFlavorOrdinal { ordinal, flavor_id } => write!(
                f,
                "flavor {flavor_id} claims ordinal {ordinal}, which another flavor already holds; \
                 ordinals are load-bearing at runtime and cannot collide"
            ),
            Self::ResourcesNotPermitted { flavor_id } => write!(
                f,
                "flavor {flavor_id} declares proxima:// resources, which only flavor #0 may do: a \
                 flavor resource needs its own scope-key namespace, URI-template parser and \
                 pagination contract"
            ),
            Self::MissingCoreContract => f.write_str(
                "flavor contracts were registered but none is flavor #0; core is non-removable",
            ),
            Self::ContractSchemaPrefix {
                flavor_id,
                schema_id,
            } => write!(
                f,
                "flavor {flavor_id} declares schema {schema_id}, which does not carry its prefix"
            ),
            Self::UnenforcedTransferRefusal {
                flavor_id,
                schema_id,
            } => write!(
                f,
                "flavor {flavor_id} declares {schema_id} NotTransferable but names no enforcement \
                 site; a refusal nothing backs is a comment"
            ),
            Self::ContractSchemaNotRegistered {
                flavor_id,
                schema_id,
                schema_version,
                kind,
            } => write!(
                f,
                "flavor {flavor_id} declares {schema_id} v{schema_version} {kind:?} in its \
                 contract but never registered it"
            ),
            Self::SchemaWithoutContract {
                flavor_id,
                schema_id,
                schema_version,
                kind,
            } => write!(
                f,
                "schema {schema_id} v{schema_version} {kind:?} is registered under flavor \
                 {flavor_id} but its contract does not declare it, so every registry walk \
                 (erase, export, forget, transfer) would miss its surfaces"
            ),
            Self::ContractToolNotRegistered { flavor_id, name } => write!(
                f,
                "flavor {flavor_id} declares MCP tool {name} in its contract but never \
                 registered it"
            ),
            Self::ProjectionWeightLevels {
                flavor_id,
                schema_id,
                levels,
                classes,
            } => write!(
                f,
                "flavor {flavor_id} schema {schema_id} declares {levels} distinct field \
                 weights, but `PostgreSQL` stores a two-bit weight per lexeme position and \
                 offers exactly {classes} tsvector classes (A, B, C, D — see `PostgreSQL` \
                 12.3.1); collapsing two levels into one class would make ts_rank's weight \
                 array describe a document it is not scoring"
            ),
            Self::CitationSidecarNotRemappable {
                flavor_id,
                schema_id,
                table,
            } => write!(
                f,
                "flavor {flavor_id} schema {schema_id} declares sidecar table {table} for a \
                 citation payload; a cross-owner transfer now dedupes a shared blob onto a \
                 NEW blob row, and the columns that must follow it are the ones declared on \
                 `TransferRule::FollowOrDedupe` -- a citation sidecar points at a blob by \
                 convention with no SQL foreign key, so nothing would repoint it and the \
                 rows would keep naming the source owner's row"
            ),
            Self::ProjectionLanguageColumn {
                flavor_id,
                schema_id,
                declared,
                projection_column,
            } => write!(
                f,
                "flavor {flavor_id} schema {schema_id} declares LanguagePolicy::PerRow on \
                 projection column {declared}, but its projection table's language column is \
                 {}; the generator writes one column per projection table, so a second name \
                 would be a declaration nothing renders and every row would be stamped and \
                 ranked under a configuration the contract never named",
                projection_column.unwrap_or("absent")
            ),
            Self::ProjectionRenderNotUniform {
                flavor_id,
                schema_id,
                property,
            } => write!(
                f,
                "flavor {flavor_id} declares RankSource::Projection, so one statement serves \
                 all of its projected schemas -- but schema {schema_id} declares a different \
                 {property} from the flavor's first projected schema, and one statement can \
                 spell {property} only once"
            ),
            Self::ProjectionBandName {
                flavor_id,
                schema_id,
                missing,
            } => write!(
                f,
                "flavor {flavor_id} declares RankSource::Projection, whose renderer resolves \
                 its arms by band name -- but schema {schema_id} declares no band named \
                 {missing:?}, so that arm would have no window to score in"
            ),
            Self::ProjectionBandOutsideCoreWindow {
                flavor_id,
                schema_id,
                band,
                window,
            } => write!(
                f,
                "flavor {flavor_id} claims BandComparability::CoreBands, but schema \
                 {schema_id} declares band {band:?} as {window}, outside flavor #0's \
                 [0, 1] window; a merge that compared those scores numerically would be \
                 comparing two different scales"
            ),
        }
    }
}

impl std::error::Error for FlavorRegistryError {}
