use super::{PayloadKind, SchemaId, SchemaVersion, ScopeKind};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum FlavorRegistryError {
    #[error("duplicate schema registered: {schema_id} v{schema_version} {kind:?}")]
    DuplicateSchema {
        schema_id: SchemaId,
        schema_version: SchemaVersion,
        kind: PayloadKind,
    },
    /// A Memory stores no schema version, so F/A/P registrations must have a
    /// unique `(kind, schema_id)` selector at freeze.
    #[error(
        "duplicate Memory schema selector: {schema_id} {kind:?} has v{first_version} and v{conflicting_version}"
    )]
    DuplicateMemorySchemaSelector {
        schema_id: SchemaId,
        kind: PayloadKind,
        first_version: SchemaVersion,
        conflicting_version: SchemaVersion,
    },
    #[error("duplicate tool name registered: {name}")]
    DuplicateTool { name: &'static str },
    #[error("duplicate flavor descriptor registered: {flavor_id}")]
    DuplicateFlavor { flavor_id: String },
    #[error(
        "schema {schema_id} v{schema_version} {kind:?} has invalid capability tag {tag:?}: {message}"
    )]
    InvalidCapabilityTag {
        schema_id: SchemaId,
        schema_version: SchemaVersion,
        kind: PayloadKind,
        tag: String,
        message: String,
    },
    #[error("tool name {name:?} is invalid for prefix {expected_prefix:?}: {message}")]
    InvalidToolName {
        name: &'static str,
        expected_prefix: String,
        message: String,
    },
    #[error("duplicate owner resolver registered")]
    DuplicateOwnerResolver,
    #[error(
        "schema {schema_id} v{schema_version} {kind:?} has mismatched typed-ingress registration"
    )]
    SchemaIngressMismatch {
        schema_id: SchemaId,
        schema_version: SchemaVersion,
        kind: PayloadKind,
    },
    #[error(
        "schema {schema_id} v{schema_version} {kind:?} is opaque; only CitedObject and CitationMapping schemas may be opaque"
    )]
    OpaqueSchemaKind {
        schema_id: SchemaId,
        schema_version: SchemaVersion,
        kind: PayloadKind,
    },
    #[error(
        "schema capability tags reference unregistered schema: {schema_id} v{schema_version} {kind:?}"
    )]
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
    #[error(
        "tool {name} declares no ANNOTATIONS, so the owner-role gate cannot tell a read \
         from a write and will demand write access; set `const ANNOTATIONS` on the tool"
    )]
    UndeclaredToolBehavior { name: &'static str },
    /// A tool whose `Args` is an internally tagged enum — so its schema
    /// carries `x-proxima-actions` and MCP clients see a dispatcher —
    /// declared no `ACTION_ARG_SPECS`. Nothing then enumerates its
    /// actions: the scope gate falls back to whole-tool grants, the
    /// catalog lists none, REST serves no action route, and arguments are
    /// validated against every variant's fields merged together.
    #[error(
        "tool {name} has an internally tagged `Args` (its schema carries \
         x-proxima-actions) but declares no ACTION_ARG_SPECS, so nothing enumerates its \
         actions: set `const ACTION_ARG_SPECS` on the tool, or give it a plain struct \
         `Args`"
    )]
    DispatcherWithoutActionSpecs { name: &'static str },
    /// A tool's `ACTION_ARG_SPECS` and its schemars-derived
    /// `x-proxima-actions` do not describe the same dispatcher.
    #[error("tool {name} has inconsistent ACTION_ARG_SPECS: {message}")]
    InvalidActionSpecs { name: &'static str, message: String },
    /// A tool declared both `ACTION_ARG_SPECS` and `ARGV_ACTION_SPECS`.
    /// Each is THE enumeration of the tool's action set under its own
    /// dispatch shape; with both live, the scope gate, the catalog, and the
    /// validator would each have to pick which vocabulary names an action.
    #[error(
        "tool {name} declares both ACTION_ARG_SPECS and ARGV_ACTION_SPECS; each is the \
         single enumeration of the tool's actions under its own dispatch shape, so \
         declare exactly one"
    )]
    ConflictingActionVocabularies { name: &'static str },
    /// Two flavors claim the same ordinal. Ordinals are load-bearing at
    /// runtime (unscoped search is `ordinal == 0`), so they cannot collide.
    #[error(
        "flavor {flavor_id} claims ordinal {ordinal}, which another flavor already holds; \
         ordinals are load-bearing at runtime and cannot collide"
    )]
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
    #[error(
        "schema {schema_id} declares lifecycle scope {kind}, which no linked flavor \
         contract declares; add a ScopeDecl naming its registry table, id column and \
         owner columns"
    )]
    ScopeNotDeclared {
        schema_id: SchemaId,
        kind: ScopeKind,
    },
    /// Two contracts declare the same [`ScopeKind`]. The kind is the fence
    /// key's namespace and the sole selector for the liveness probe, so a
    /// second declaration is not a wider claim but an undecided one: an
    /// admission could not tell which registry table its scope lives in.
    #[error(
        "flavors {first_flavor_id} and {conflicting_flavor_id} both declare lifecycle \
         scope {kind}; the kind is one fence namespace and one registry, so it has one \
         declaration or none"
    )]
    DuplicateScopeDeclaration {
        kind: ScopeKind,
        first_flavor_id: &'static str,
        conflicting_flavor_id: &'static str,
    },
    /// A scope declaration names something storage cannot splice — an
    /// unqualified registry table or an empty column name.
    #[error(
        "flavor {flavor_id}'s declaration of lifecycle scope {kind} is not spliceable: \
         {message}"
    )]
    InvalidScopeDeclaration {
        flavor_id: &'static str,
        kind: ScopeKind,
        message: &'static str,
    },
    /// A flavor other than #0 declared `proxima://` resources. Resources are
    /// substrate-only: a flavor resource needs its own scope-key namespace,
    /// URI-template parser and pagination contract.
    #[error(
        "flavor {flavor_id} declares proxima:// resources, which only flavor #0 may do: a \
         flavor resource needs its own scope-key namespace, URI-template parser and \
         pagination contract"
    )]
    ResourcesNotPermitted { flavor_id: &'static str },
    /// Contracts were registered but none of them is flavor #0. Core is
    /// non-removable — the two registry-reflection resources
    /// (`proxima://schemas`, `proxima://tools`) live in its contract.
    #[error("flavor contracts were registered but none is flavor #0; core is non-removable")]
    MissingCoreContract,
    /// A contract entry's schema id does not carry its flavor's prefix.
    #[error("flavor {flavor_id} declares schema {schema_id}, which does not carry its prefix")]
    ContractSchemaPrefix {
        flavor_id: &'static str,
        schema_id: SchemaId,
    },
    /// A surface declares itself exportable while carrying neither an owner
    /// column nor a key with a home table, so no statement can reach it from
    /// the owner. It would go missing from every bundle.
    #[error(
        "flavor {flavor_id} declares {table} exportable, but it carries no owner \
         column and its key has no home table, so no export statement can reach \
         its owner"
    )]
    UnreachableExportSurface {
        flavor_id: &'static str,
        table: &'static str,
    },
    /// A schema declared itself listenable and published no JSON schema.
    ///
    /// A captured event names its `dataschema`
    /// (`proxima://schema/{id}/{version}`), and a consumer that follows it
    /// has to find something there. A listenable schema with no
    /// `json_schema()` would emit events pointing at a catalog entry that
    /// resolves to nothing, for the lifetime of every one of those events.
    #[error(
        "schema {schema_id} declares LISTENABLE = true but returns no json_schema(); a \
         captured event's dataschema names the local catalog, so a listenable schema \
         must publish one"
    )]
    ListenableWithoutSchema { schema_id: SchemaId },
    /// A surface declares an erase no leg can perform: keyed on something
    /// the erase builds no selection set for, and claimed by no bespoke
    /// leg. The erase would skip it in silence and report `Completed` over
    /// rows that survived the owner they belong to.
    #[error(
        "flavor {flavor_id} declares an erase for {table} that no leg can perform: it is \
         keyed on neither a memory, a goal nor a blob, and no bespoke erase leg claims \
         it, so an owner erase would skip it and still report success"
    )]
    UndeletableSurface {
        flavor_id: &'static str,
        table: &'static str,
    },
    /// A declared bespoke erase leg that names nothing it could own — a
    /// table the flavor does not declare, or one whose declaration says no
    /// statement runs at all.
    #[error("flavor {flavor_id} declares a bespoke erase leg for {table}, which {why}")]
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
    #[error(
        "flavor {flavor_id} declares a transfer for {table} that no leg can perform: it \
         is keyed on neither a memory nor an entity t, and no bespoke transfer leg \
         claims it, so a transfer would skip it and still report success — leaving rows \
         the source owner can read after the memory became someone else's"
    )]
    UnmovableSurface {
        flavor_id: &'static str,
        table: &'static str,
    },
    /// A declared bespoke transfer leg that names nothing it could own — a
    /// table the flavor does not declare, or one whose declaration says no
    /// statement runs at all.
    #[error("flavor {flavor_id} declares a bespoke transfer leg for {table}, which {why}")]
    BespokeTransferLegMismatch {
        flavor_id: &'static str,
        table: &'static str,
        why: &'static str,
    },
    /// A surface declares that forget destroys or preserves its rows and the
    /// forget reaches none of them: `DeleteWithMemory` over a key it builds
    /// no `t` for, or `DumpThenCascade` without its MemoryT/completeness
    /// proof. The rows would outlive the memory that declared their fate.
    #[error(
        "flavor {flavor_id} declares that forget deletes {table} with the memory, and \
         the forget reaches none of its rows: the key is neither a memory nor an entity \
         t, and no completeness constraint claims them either"
    )]
    UnforgettableSurface {
        flavor_id: &'static str,
        table: &'static str,
    },
    /// A schema's `EmbeddingRecipe` and the units the drain resolves for it
    /// disagree about whether it embeds. The recipe is the claim; the units
    /// are what the machinery will actually be handed.
    #[error(
        "flavor {flavor_id} declares {schema_id} with EmbeddingRecipe::Never = \
         {recipe_is_never}, but the drain resolves embed units for it = \
         {machinery_embeds}; a schema that declares it embeds and hands the \
         drain nothing files embedding jobs the drain can only drop, and one \
         that declares Never while resolving a unit is silently embedded"
    )]
    EmbeddabilityDisagreement {
        flavor_id: &'static str,
        schema_id: SchemaId,
        recipe_is_never: bool,
        machinery_embeds: bool,
    },
    /// A schema declares `EmbeddingRecipe::Units(&[])` — the claim
    /// `Never { why }` exists to carry, with the reason deleted, and wearing
    /// the arm that means "embeds".
    #[error(
        "flavor {flavor_id} declares {schema_id} with EmbeddingRecipe::Units(&[]); \
         an empty unit list yields the drain no text, which is what \
         EmbeddingRecipe::Never says — declare Never and state why, because \
         Units answers `false` to is_never() and the enqueue lane believes it"
    )]
    EmptyEmbeddingUnits {
        flavor_id: &'static str,
        schema_id: SchemaId,
    },
    /// A schema's contract names different natural key columns than the
    /// payload trait the ingest actually reads.
    #[error(
        "flavor {flavor_id} declares natural key columns for {schema_id} that are \
         not the ones the ingest reads off the payload trait"
    )]
    NaturalKeyDisagreement {
        flavor_id: &'static str,
        schema_id: SchemaId,
    },
    /// A tool's contract names different dispatcher actions, or names them
    /// in a different order, than the descriptor the registry holds.
    #[error(
        "flavor {flavor_id} declares actions for {name} that are not, in order, \
         the actions its registered dispatcher accepts"
    )]
    ToolActionsDisagreement {
        flavor_id: &'static str,
        name: &'static str,
    },
    /// A tool's contract and its resolved MCP annotations disagree about
    /// whether calling it twice is the same as calling it once.
    #[error(
        "flavor {flavor_id} declares {name} idempotent = {declared}; its resolved \
         MCP annotations say {resolved}, and the wire believes the annotations"
    )]
    ToolIdempotenceDisagreement {
        flavor_id: &'static str,
        name: &'static str,
        declared: bool,
        resolved: bool,
    },
    /// A schema declared `NotTransferable` without naming where the refusal
    /// is enforced. A refusal nothing backs is a comment.
    #[error(
        "flavor {flavor_id} declares {schema_id} NotTransferable but names no enforcement \
         site; a refusal nothing backs is a comment"
    )]
    UnenforcedTransferRefusal {
        flavor_id: &'static str,
        schema_id: SchemaId,
    },
    /// The contract declares a schema that was never registered — erase,
    /// export and forget would walk a surface no write can produce.
    #[error(
        "flavor {flavor_id} declares {schema_id} v{schema_version} {kind:?} in its \
         contract but never registered it"
    )]
    ContractSchemaNotRegistered {
        flavor_id: &'static str,
        schema_id: SchemaId,
        schema_version: SchemaVersion,
        kind: PayloadKind,
    },
    /// A schema was registered under a flavor's prefix but its contract does
    /// not declare it — the drift that makes a registry walk miss a surface.
    #[error(
        "schema {schema_id} v{schema_version} {kind:?} is registered under flavor \
         {flavor_id} but its contract does not declare it, so every registry walk \
         (erase, export, forget, transfer) would miss its surfaces"
    )]
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
    #[error(
        "flavor {flavor_id} is linked into this binary and declares no FlavorContract, \
         so every registry walk (erase, export, forget, transfer) skips whatever it \
         registers and its Memory writes are refused later by a flavor_surface \
         constraint naming none of this; declare a FlavorContract for {flavor_id} and \
         register it, which for a flavor built with proxima_flavor! means adding \
         `contract = &<YOUR_CONTRACT>` to the macro"
    )]
    UnclaimedRegistration { flavor_id: String },
    /// A projected schema's sidecar declares no surface keyed on the
    /// memory `t`. The projection generator spells each row's key from
    /// that column, so the schema has no projection statement to generate.
    #[error(
        "flavor {flavor_id} declares {schema_id} a search projection over sidecar \
         {table}, and no surface of this flavor declares {table} keyed on the memory t; \
         the projection generator spells each projection row's key from that column, so \
         there is no statement to generate -- declare a surface for {table} with \
         `key: KeyShape::MemoryT {{ column: .. }}`"
    )]
    ProjectedSidecarNotMemoryKeyed {
        flavor_id: &'static str,
        schema_id: SchemaId,
        table: &'static str,
    },
    /// An embedding schema's sidecar declares no surface keyed on the
    /// memory `t`. The drain's text read filters that column, so the schema
    /// has no statement to generate — the twin of
    /// [`Self::ProjectedSidecarNotMemoryKeyed`] on the embedding lane.
    #[error(
        "flavor {flavor_id} declares {schema_id} an embed unit on sidecar {table}, and \
         no surface of this flavor declares {table} keyed on the memory t; the drain \
         filters the text read on that column, so there is no statement to generate -- \
         declare a surface for {table} with `key: KeyShape::MemoryT {{ column: .. }}`"
    )]
    EmbeddedSidecarNotMemoryKeyed {
        flavor_id: &'static str,
        schema_id: SchemaId,
        table: &'static str,
    },
    /// A non-core schema declares a search projection no request shape can
    /// scan. Every write to it pays a projection row and a GIN index entry
    /// for a corpus no query reaches.
    #[error(
        "flavor {flavor_id} declares {schema_id} as a search projection that \
         core_search_memories can never scan: {why}. Every write to it still pays a \
         projection row and a GIN index entry -- declare a tag_column so a \
         tag-filtered request reaches it, declare SearchProjectionDecl::None {{ why }} \
         if it is not a search surface, or declare \
         RankSource::SidecarWithProjectionOwner {{ why }} if the flavor's own tools \
         rank it"
    )]
    UnreachableSearchProjection {
        flavor_id: &'static str,
        schema_id: SchemaId,
        /// Which reachability condition the declaration fails.
        why: &'static str,
    },
    /// The contract names an MCP tool that was never registered.
    #[error(
        "flavor {flavor_id} declares MCP tool {name} in its contract but never \
         registered it"
    )]
    ContractToolNotRegistered {
        flavor_id: &'static str,
        name: &'static str,
    },
    /// One projection unit declares more distinct weight levels than
    /// `PostgreSQL` has tsvector weight classes.
    #[error(
        "flavor {flavor_id} schema {schema_id} declares {levels} distinct field \
         weights, but `PostgreSQL` stores a two-bit weight per lexeme position and \
         offers exactly {classes} tsvector classes (A, B, C, D — see `PostgreSQL` \
         12.3.1); collapsing two levels into one class would make ts_rank's weight \
         array describe a document it is not scoring"
    )]
    ProjectionWeightLevels {
        flavor_id: &'static str,
        schema_id: SchemaId,
        levels: usize,
        classes: usize,
    },
    /// A cited-object or citation-mapping schema declared a sidecar table.
    /// Those tables point at a blob row by convention rather than by
    /// constraint, so the shared-blob dedupe arm's remap cannot find them.
    #[error(
        "flavor {flavor_id} schema {schema_id} declares sidecar table {table} for a \
         citation payload; a cross-owner transfer now dedupes a shared blob onto a \
         NEW blob row, and the columns that must follow it are the ones declared on \
         `TransferRule::FollowOrDedupe` -- a citation sidecar points at a blob by \
         convention with no SQL foreign key, so nothing would repoint it and the \
         rows would keep naming the source owner's row"
    )]
    CitationSidecarNotRemappable {
        flavor_id: &'static str,
        schema_id: SchemaId,
        table: &'static str,
    },
    /// A `LanguagePolicy::PerRow` names a projection column the projection
    /// table does not have.
    #[error(
        "flavor {flavor_id} schema {schema_id} declares LanguagePolicy::PerRow on \
         projection column {declared}, but its projection table's language column is \
         {}; the generator writes one column per projection table, so a second name \
         would be a declaration nothing renders and every row would be stamped and \
         ranked under a configuration the contract never named",
        .projection_column.unwrap_or("absent")
    )]
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
    #[error(
        "flavor {flavor_id} declares RankSource::Projection, so one statement serves \
         all of its projected schemas -- but schema {schema_id} declares a different \
         {property} from the flavor's first projected schema, and one statement can \
         spell {property} only once"
    )]
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
    #[error(
        "flavor {flavor_id} declares RankSource::Projection, whose renderer resolves \
         its arms by band name -- but schema {schema_id} declares no band named \
         {missing:?}, so that arm would have no window to score in"
    )]
    ProjectionBandName {
        flavor_id: &'static str,
        schema_id: SchemaId,
        missing: &'static str,
    },
    /// A flavor claims `BandComparability::CoreBands` while one of its
    /// schemas declares a band outside flavor #0's `[0.0, 1.0]` window.
    /// The claim is what a cross-flavor merge compares scores on, so it has
    /// to be earned rather than decorated.
    #[error(
        "flavor {flavor_id} claims BandComparability::CoreBands, but schema \
         {schema_id} declares band {band:?} as {window}, outside flavor #0's \
         [0, 1] window; a merge that compared those scores numerically would be \
         comparing two different scales"
    )]
    ProjectionBandOutsideCoreWindow {
        flavor_id: &'static str,
        schema_id: SchemaId,
        band: &'static str,
        /// The offending window, rendered — the enum derives `Eq`, and an
        /// `f32` pair would not.
        window: String,
    },
}
