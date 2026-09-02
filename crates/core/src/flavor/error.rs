use super::{PayloadKind, SchemaId, SchemaVersion, ScopeKind};

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum FlavorRegistryError {
    DuplicateSchema {
        schema_id: SchemaId,
        schema_version: SchemaVersion,
        kind: PayloadKind,
    },
    /// A Memory stores no schema version, so F/A/P registrations must have a
    /// unique `(kind, schema_id)` selector at freeze.
    DuplicateMemorySchemaSelector {
        schema_id: SchemaId,
        kind: PayloadKind,
        first_version: SchemaVersion,
        conflicting_version: SchemaVersion,
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
    /// A tool declared both `ACTION_ARG_SPECS` and `ARGV_ACTION_SPECS`.
    /// Each is THE enumeration of the tool's action set under its own
    /// dispatch shape; with both live, the scope gate, the catalog, and the
    /// validator would each have to pick which vocabulary names an action.
    ConflictingActionVocabularies {
        name: &'static str,
    },
    /// Two flavors claim the same ordinal. Ordinals are load-bearing at
    /// runtime (unscoped search is `ordinal == 0`), so they cannot collide.
    DuplicateFlavorOrdinal {
        ordinal: u16,
        flavor_id: &'static str,
    },
    /// A registered payload names a lifecycle scope no linked flavor
    /// declares (docs/03 §Scope declaration). Without the declaration
    /// storage has no registry table to probe and no columns to key the
    /// probe on, so every admission of that payload would either skip the
    /// fence or fail at the first write. Refused here, where the
    /// registration that named it can still be pointed at.
    ScopeNotDeclared {
        schema_id: SchemaId,
        kind: ScopeKind,
    },
    /// Two contracts declare the same [`ScopeKind`]. The kind is the fence
    /// key's namespace and the sole selector for the liveness probe, so a
    /// second declaration is not a wider claim but an undecided one: an
    /// admission could not tell which registry table its scope lives in.
    DuplicateScopeDeclaration {
        kind: ScopeKind,
        first_flavor_id: &'static str,
        conflicting_flavor_id: &'static str,
    },
    /// A scope declaration names something storage cannot splice — an
    /// unqualified registry table or an empty column name.
    InvalidScopeDeclaration {
        flavor_id: &'static str,
        kind: ScopeKind,
        message: &'static str,
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
    /// A surface declares itself exportable while carrying neither an owner
    /// column nor a key with a home table, so no statement can reach it from
    /// the owner. It would go missing from every bundle.
    UnreachableExportSurface {
        flavor_id: &'static str,
        table: &'static str,
    },
    /// A surface declares an erase no leg can perform: keyed on something
    /// the erase builds no selection set for, and claimed by no bespoke
    /// leg. The erase would skip it in silence and report `Completed` over
    /// rows that survived the owner they belong to.
    UndeletableSurface {
        flavor_id: &'static str,
        table: &'static str,
    },
    /// A declared bespoke erase leg that names nothing it could own — a
    /// table the flavor does not declare, or one whose declaration says no
    /// statement runs at all.
    BespokeEraseLegMismatch {
        flavor_id: &'static str,
        table: &'static str,
        why: &'static str,
    },
    /// A surface declares a transfer no leg can perform: keyed on
    /// something the transfer builds no statement for, and claimed by no
    /// bespoke leg. The transfer would skip it in silence and report
    /// success, leaving rows the SOURCE owner can still read after the
    /// memory they belong to became someone else's.
    UnmovableSurface {
        flavor_id: &'static str,
        table: &'static str,
    },
    /// A declared bespoke transfer leg that names nothing it could own — a
    /// table the flavor does not declare, or one whose declaration says no
    /// statement runs at all.
    BespokeTransferLegMismatch {
        flavor_id: &'static str,
        table: &'static str,
        why: &'static str,
    },
    /// A surface declares that forget destroys or preserves its rows and the
    /// forget reaches none of them: `DeleteWithMemory` over a key it builds
    /// no `t` for, or `DumpThenCascade` without its MemoryT/completeness
    /// proof. The rows would outlive the memory that declared their fate.
    UnforgettableSurface {
        flavor_id: &'static str,
        table: &'static str,
    },
    /// A schema's `EmbeddingRecipe` and the units the drain resolves for it
    /// disagree about whether it embeds. The recipe is the claim; the units
    /// are what the machinery will actually be handed.
    EmbeddabilityDisagreement {
        flavor_id: &'static str,
        schema_id: SchemaId,
        recipe_is_never: bool,
        machinery_embeds: bool,
    },
    /// A schema declares `EmbeddingRecipe::Units(&[])` — the claim
    /// `Never { why }` exists to carry, with the reason deleted, and wearing
    /// the arm that means "embeds".
    EmptyEmbeddingUnits {
        flavor_id: &'static str,
        schema_id: SchemaId,
    },
    /// A schema's contract names different natural key columns than the
    /// payload trait the ingest actually reads.
    NaturalKeyDisagreement {
        flavor_id: &'static str,
        schema_id: SchemaId,
    },
    /// A tool's contract names different dispatcher actions, or names them
    /// in a different order, than the descriptor the registry holds.
    ToolActionsDisagreement {
        flavor_id: &'static str,
        name: &'static str,
    },
    /// A tool's contract and its resolved MCP annotations disagree about
    /// whether calling it twice is the same as calling it once.
    ToolIdempotenceDisagreement {
        flavor_id: &'static str,
        name: &'static str,
        declared: bool,
        resolved: bool,
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
    /// A flavor registered a `FlavorDescriptor` — it is linked into this
    /// binary — and declared no `FlavorContract`. Everything it registers
    /// is then invisible to erase, export, forget and transfer, and its
    /// Memory writes are refused later by a `flavor_surface` constraint
    /// that names none of the cause.
    UnclaimedRegistration {
        flavor_id: String,
    },
    /// A projected schema's sidecar declares no surface keyed on the
    /// memory `t`. The projection generator spells each row's key from
    /// that column, so the schema has no projection statement to generate.
    ProjectedSidecarNotMemoryKeyed {
        flavor_id: &'static str,
        schema_id: SchemaId,
        table: &'static str,
    },
    /// An embedding schema's sidecar declares no surface keyed on the
    /// memory `t`. The drain's text read filters that column, so the schema
    /// has no statement to generate — the twin of
    /// [`Self::ProjectedSidecarNotMemoryKeyed`] on the embedding lane.
    EmbeddedSidecarNotMemoryKeyed {
        flavor_id: &'static str,
        schema_id: SchemaId,
        table: &'static str,
    },
    /// A non-core schema declares a search projection no request shape can
    /// scan. Every write to it pays a projection row and a GIN index entry
    /// for a corpus no query reaches.
    UnreachableSearchProjection {
        flavor_id: &'static str,
        schema_id: SchemaId,
        /// Which reachability condition the declaration fails.
        why: &'static str,
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
    // The dispatch is exhaustive on purpose: a new variant does not compile until
    // it is routed to a family, and that family is the single place its wording
    // lives.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateSchema { .. }
            | Self::DuplicateMemorySchemaSelector { .. }
            | Self::DuplicateTool { .. }
            | Self::DuplicateFlavor { .. }
            | Self::DuplicateOwnerResolver
            | Self::DuplicateFlavorOrdinal { .. } => self.fmt_duplicate_registration(f),
            Self::InvalidCapabilityTag { .. }
            | Self::SchemaIngressMismatch { .. }
            | Self::OpaqueSchemaKind { .. }
            | Self::UnregisteredSchemaCapabilityTags { .. }
            | Self::NaturalKeyDisagreement { .. }
            | Self::ContractSchemaPrefix { .. }
            | Self::ContractSchemaNotRegistered { .. }
            | Self::SchemaWithoutContract { .. } => self.fmt_schema_registration(f),
            Self::InvalidToolName { .. }
            | Self::UndeclaredToolBehavior { .. }
            | Self::DispatcherWithoutActionSpecs { .. }
            | Self::InvalidActionSpecs { .. }
            | Self::ConflictingActionVocabularies { .. }
            | Self::ToolActionsDisagreement { .. }
            | Self::ToolIdempotenceDisagreement { .. }
            | Self::ContractToolNotRegistered { .. } => self.fmt_tool_declaration(f),
            Self::ScopeNotDeclared { .. }
            | Self::DuplicateScopeDeclaration { .. }
            | Self::InvalidScopeDeclaration { .. }
            | Self::ResourcesNotPermitted { .. }
            | Self::MissingCoreContract
            | Self::UnclaimedRegistration { .. } => self.fmt_contract_wiring(f),
            Self::UnreachableExportSurface { .. }
            | Self::UndeletableSurface { .. }
            | Self::BespokeEraseLegMismatch { .. }
            | Self::UnmovableSurface { .. }
            | Self::BespokeTransferLegMismatch { .. }
            | Self::UnforgettableSurface { .. }
            | Self::UnenforcedTransferRefusal { .. }
            | Self::CitationSidecarNotRemappable { .. } => self.fmt_lifecycle_surface(f),
            Self::EmbeddabilityDisagreement { .. }
            | Self::EmptyEmbeddingUnits { .. }
            | Self::EmbeddedSidecarNotMemoryKeyed { .. } => self.fmt_embedding(f),
            Self::ProjectedSidecarNotMemoryKeyed { .. }
            | Self::UnreachableSearchProjection { .. }
            | Self::ProjectionWeightLevels { .. } => self.fmt_search_projection(f),
            Self::ProjectionLanguageColumn { .. }
            | Self::ProjectionRenderNotUniform { .. }
            | Self::ProjectionBandName { .. }
            | Self::ProjectionBandOutsideCoreWindow { .. } => self.fmt_projection_rendering(f),
        }
    }
}

impl FlavorRegistryError {
    /// The escape hatch for a variant that reached the wrong family helper.
    ///
    /// `fmt` routes every variant to exactly one family and each family covers
    /// exactly what is routed to it, so nothing reaches this today. It renders
    /// the `Debug` form rather than panicking: `Display` runs on the reporting
    /// path, and an error that panics while being logged takes down the thing
    /// trying to tell you about it.
    fn fmt_misrouted(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }

    /// Registrations that collide: one selector, name, id or ordinal claimed
    /// twice.
    fn fmt_duplicate_registration(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateSchema {
                schema_id,
                schema_version,
                kind,
            } => write!(
                f,
                "duplicate schema registered: {schema_id} v{schema_version} {kind:?}"
            ),
            Self::DuplicateMemorySchemaSelector {
                schema_id,
                kind,
                first_version,
                conflicting_version,
            } => write!(
                f,
                "duplicate Memory schema selector: {schema_id} {kind:?} has v{first_version} and v{conflicting_version}"
            ),
            Self::DuplicateTool { name } => {
                write!(f, "duplicate tool name registered: {name}")
            }
            Self::DuplicateFlavor { flavor_id } => {
                write!(f, "duplicate flavor descriptor registered: {flavor_id}")
            }
            Self::DuplicateOwnerResolver => f.write_str("duplicate owner resolver registered"),
            Self::DuplicateFlavorOrdinal { ordinal, flavor_id } => write!(
                f,
                "flavor {flavor_id} claims ordinal {ordinal}, which another flavor already holds; \
                 ordinals are load-bearing at runtime and cannot collide"
            ),
            other => other.fmt_misrouted(f),
        }
    }

    /// Schema registrations that do not hold up: bad tags, mismatched ingress,
    /// and registration disagreeing with the declaring contract.
    fn fmt_schema_registration(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
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
            Self::NaturalKeyDisagreement {
                flavor_id,
                schema_id,
            } => write!(
                f,
                "flavor {flavor_id} declares natural key columns for {schema_id} that are \
                 not the ones the ingest reads off the payload trait"
            ),
            Self::ContractSchemaPrefix {
                flavor_id,
                schema_id,
            } => write!(
                f,
                "flavor {flavor_id} declares schema {schema_id}, which does not carry its prefix"
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
            other => other.fmt_misrouted(f),
        }
    }

    /// MCP tool declarations: names, behaviour, action vocabularies, and the
    /// contract agreeing with what was registered.
    fn fmt_tool_declaration(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidToolName {
                name,
                expected_prefix,
                message,
            } => write!(
                f,
                "tool name {name:?} is invalid for prefix {expected_prefix:?}: {message}"
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
            Self::ConflictingActionVocabularies { name } => write!(
                f,
                "tool {name} declares both ACTION_ARG_SPECS and ARGV_ACTION_SPECS; each is the \
                 single enumeration of the tool's actions under its own dispatch shape, so \
                 declare exactly one"
            ),
            Self::ToolActionsDisagreement { flavor_id, name } => write!(
                f,
                "flavor {flavor_id} declares actions for {name} that are not, in order, \
                 the actions its registered dispatcher accepts"
            ),
            Self::ToolIdempotenceDisagreement {
                flavor_id,
                name,
                declared,
                resolved,
            } => write!(
                f,
                "flavor {flavor_id} declares {name} idempotent = {declared}; its resolved \
                 MCP annotations say {resolved}, and the wire believes the annotations"
            ),
            Self::ContractToolNotRegistered { flavor_id, name } => write!(
                f,
                "flavor {flavor_id} declares MCP tool {name} in its contract but never \
                 registered it"
            ),
            other => other.fmt_misrouted(f),
        }
    }

    /// How a flavor is wired into the binary: lifecycle scopes, resources, and
    /// the contract itself.
    fn fmt_contract_wiring(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ScopeNotDeclared { schema_id, kind } => write!(
                f,
                "schema {schema_id} declares lifecycle scope {kind}, which no linked flavor \
                 contract declares; add a ScopeDecl naming its registry table, id column and \
                 owner columns"
            ),
            Self::DuplicateScopeDeclaration {
                kind,
                first_flavor_id,
                conflicting_flavor_id,
            } => write!(
                f,
                "flavors {first_flavor_id} and {conflicting_flavor_id} both declare lifecycle \
                 scope {kind}; the kind is one fence namespace and one registry, so it has one \
                 declaration or none"
            ),
            Self::InvalidScopeDeclaration {
                flavor_id,
                kind,
                message,
            } => write!(
                f,
                "flavor {flavor_id}'s declaration of lifecycle scope {kind} is not spliceable: \
                 {message}"
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
            Self::UnclaimedRegistration { flavor_id } => write!(
                f,
                "flavor {flavor_id} is linked into this binary and declares no FlavorContract, \
                 so every registry walk (erase, export, forget, transfer) skips whatever it \
                 registers and its Memory writes are refused later by a flavor_surface \
                 constraint naming none of this; declare a FlavorContract for {flavor_id} and \
                 register it, which for a flavor built with proxima_flavor! means adding \
                 `contract = &<YOUR_CONTRACT>` to the macro"
            ),
            other => other.fmt_misrouted(f),
        }
    }

    /// Declared surfaces no registry walk can reach: erase, export, forget and
    /// transfer legs, and the citation columns a transfer has to repoint.
    fn fmt_lifecycle_surface(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnreachableExportSurface { flavor_id, table } => write!(
                f,
                "flavor {flavor_id} declares {table} exportable, but it carries no owner \
                 column and its key has no home table, so no export statement can reach \
                 its owner"
            ),
            Self::UndeletableSurface { flavor_id, table } => write!(
                f,
                "flavor {flavor_id} declares an erase for {table} that no leg can perform: it is \
                 keyed on neither a memory, a goal nor a blob, and no bespoke erase leg claims \
                 it, so an owner erase would skip it and still report success"
            ),
            Self::BespokeEraseLegMismatch {
                flavor_id,
                table,
                why,
            } => write!(
                f,
                "flavor {flavor_id} declares a bespoke erase leg for {table}, which {why}"
            ),
            Self::UnmovableSurface { flavor_id, table } => write!(
                f,
                "flavor {flavor_id} declares a transfer for {table} that no leg can perform: it \
                 is keyed on neither a memory nor an entity t, and no bespoke transfer leg \
                 claims it, so a transfer would skip it and still report success — leaving rows \
                 the source owner can read after the memory became someone else's"
            ),
            Self::BespokeTransferLegMismatch {
                flavor_id,
                table,
                why,
            } => write!(
                f,
                "flavor {flavor_id} declares a bespoke transfer leg for {table}, which {why}"
            ),
            Self::UnforgettableSurface { flavor_id, table } => write!(
                f,
                "flavor {flavor_id} declares that forget deletes {table} with the memory, and \
                 the forget reaches none of its rows: the key is neither a memory nor an entity \
                 t, and no completeness constraint claims them either"
            ),
            Self::UnenforcedTransferRefusal {
                flavor_id,
                schema_id,
            } => write!(
                f,
                "flavor {flavor_id} declares {schema_id} NotTransferable but names no enforcement \
                 site; a refusal nothing backs is a comment"
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
            other => other.fmt_misrouted(f),
        }
    }

    /// What the embedding drain can and cannot resolve for a declared schema.
    fn fmt_embedding(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmbeddabilityDisagreement {
                flavor_id,
                schema_id,
                recipe_is_never,
                machinery_embeds,
            } => write!(
                f,
                "flavor {flavor_id} declares {schema_id} with EmbeddingRecipe::Never = \
                 {recipe_is_never}, but the drain resolves embed units for it = \
                 {machinery_embeds}; a schema that declares it embeds and hands the \
                 drain nothing files embedding jobs the drain can only drop, and one \
                 that declares Never while resolving a unit is silently embedded"
            ),
            Self::EmptyEmbeddingUnits {
                flavor_id,
                schema_id,
            } => write!(
                f,
                "flavor {flavor_id} declares {schema_id} with EmbeddingRecipe::Units(&[]); \
                 an empty unit list yields the drain no text, which is what \
                 EmbeddingRecipe::Never says — declare Never and state why, because \
                 Units answers `false` to is_never() and the enqueue lane believes it"
            ),
            Self::EmbeddedSidecarNotMemoryKeyed {
                flavor_id,
                schema_id,
                table,
            } => write!(
                f,
                "flavor {flavor_id} declares {schema_id} an embed unit on sidecar {table}, and \
                 no surface of this flavor declares {table} keyed on the memory t; the drain \
                 filters the text read on that column, so there is no statement to generate -- \
                 declare a surface for {table} with `key: KeyShape::MemoryT {{ column: .. }}`"
            ),
            other => other.fmt_misrouted(f),
        }
    }

    /// Search projections: the sidecar they are keyed on, whether a request can
    /// reach them, and what `ts_rank` can be handed.
    fn fmt_search_projection(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ProjectedSidecarNotMemoryKeyed {
                flavor_id,
                schema_id,
                table,
            } => write!(
                f,
                "flavor {flavor_id} declares {schema_id} a search projection over sidecar \
                 {table}, and no surface of this flavor declares {table} keyed on the memory t; \
                 the projection generator spells each projection row's key from that column, so \
                 there is no statement to generate -- declare a surface for {table} with \
                 `key: KeyShape::MemoryT {{ column: .. }}`"
            ),
            Self::UnreachableSearchProjection {
                flavor_id,
                schema_id,
                why,
            } => write!(
                f,
                "flavor {flavor_id} declares {schema_id} as a search projection that \
                 core_search_memories can never scan: {why}. Every write to it still pays a \
                 projection row and a GIN index entry -- declare a tag_column so a \
                 tag-filtered request reaches it, declare SearchProjectionDecl::None {{ why }} \
                 if it is not a search surface, or declare \
                 RankSource::SidecarWithProjectionOwner {{ why }} if the flavor's own tools \
                 rank it"
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
            other => other.fmt_misrouted(f),
        }
    }

    /// What the shared projection renderer can spell: one language column, one
    /// statement per flavor, and bands it can resolve and compare.
    fn fmt_projection_rendering(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
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
            other => other.fmt_misrouted(f),
        }
    }
}

impl std::error::Error for FlavorRegistryError {}
