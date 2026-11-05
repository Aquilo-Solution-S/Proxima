use super::{PayloadKind, SchemaId, SchemaVersion, ScopeKind};
use crate::ScopeKeyError;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SpiiceRegistryError {
    #[error("duplicate retired Fact declaration: {schema_id}")]
    DuplicateRetiredFact { schema_id: SchemaId },
    #[error("retired Fact {schema_id} also has an active schema or surface declaration")]
    ActiveRetiredFactOverlap { schema_id: SchemaId },
    #[error("spiice {spiice_id} cannot retire {schema_id}: {why}")]
    RetiredFactOwnership {
        spiice_id: &'static str,
        schema_id: SchemaId,
        why: &'static str,
    },
    #[error("retired Fact {schema_id} has unusable inverse surface {table}: {why}")]
    RetiredFactInverse {
        schema_id: SchemaId,
        table: &'static str,
        why: &'static str,
    },
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
    #[error("duplicate spiice descriptor registered: {spiice_id}")]
    DuplicateSpiice { spiice_id: String },
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
    /// A tool's, or one of its actions', human title is empty, multi-line,
    /// padded or too long ([`MAX_TITLE_CHARS`](crate::mcp::MAX_TITLE_CHARS)).
    #[error("{key} has an invalid title {title:?}: it {problem}")]
    InvalidTitle {
        key: String,
        title: &'static str,
        problem: &'static str,
    },
    #[error("tool {name} has invalid MCP output schema: {message}")]
    InvalidToolOutputSchema { name: &'static str, message: String },
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
        "schema {schema_id} v{schema_version} {kind:?} is opaque; only CitedObject schemas may be opaque"
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
    /// A registered flat MCP tool declares no `EFFECT`, so the owner-role
    /// gate cannot tell a read from a write and has to assume a write.
    #[error(
        "tool {name} declares no EFFECT, so the owner-role gate cannot tell a read \
         from a write and will demand write access; set `const EFFECT` on the tool"
    )]
    UndeclaredToolBehavior { name: &'static str },
    /// A dispatcher declares a tool-level `EFFECT`. Its effect is the join
    /// of its actions' — each spec's `effect` — so a second, tool-level
    /// answer could only disagree.
    #[error(
        "tool {name} dispatches actions and also declares a tool-level EFFECT; \
         declare the effect on each action spec instead"
    )]
    DispatcherToolEffect { name: &'static str },
    /// A tool whose `Args` is an internally tagged enum — so registration
    /// derived a dispatcher schema and MCP clients see a dispatcher —
    /// declared no `ACTION_ARG_SPECS`. Nothing then enumerates its
    /// actions: the scope gate falls back to whole-tool grants, the
    /// catalog lists none, REST serves no action route, and arguments are
    /// validated against every variant's fields merged together.
    #[error(
        "tool {name} has an internally tagged `Args` (a dispatcher schema) but declares \
         no ACTION_ARG_SPECS, so nothing enumerates its \
         actions: set `const ACTION_ARG_SPECS` on the tool, or give it a plain struct \
         `Args`"
    )]
    DispatcherWithoutActionSpecs { name: &'static str },
    /// A tool's `ACTION_ARG_SPECS` and its schemars-derived
    /// [`McpDispatcherSchema`](crate::mcp::McpDispatcherSchema) do not
    /// describe the same dispatcher.
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
    /// A tool name, or the name of one of its actions, is no scope key, so no
    /// palette could hold the key the scope gate judges the tool by: an
    /// action outside `[A-Za-z0-9_.-]`, or a dispatcher called `resource`,
    /// whose leaves would spell resource keys. Refused at freeze, not at the
    /// first palette build.
    #[error("tool {name} (or one of its actions) has no valid scope key: {reason}")]
    InvalidScopeKey {
        name: &'static str,
        reason: ScopeKeyError,
    },
    /// Two spiices claim the same ordinal. The core ordinal identifies the
    /// non-removable substrate, so spiice identities cannot collide.
    #[error(
        "spiice {spiice_id} claims ordinal {ordinal}, which another spiice already holds; \
         ordinals are load-bearing at runtime and cannot collide"
    )]
    DuplicateSpiiceOrdinal {
        ordinal: u16,
        spiice_id: &'static str,
    },
    /// A registered payload names a lifecycle scope no linked spiice
    /// declares (docs/build-a-spiice.md). Without the declaration
    /// storage has no registry table to probe and no columns to key the
    /// probe on, so every admission of that payload would either skip the
    /// fence or fail at the first write. Refused here, where the
    /// registration that named it can still be pointed at.
    #[error(
        "schema {schema_id} declares lifecycle scope {kind}, which no linked spiice \
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
        "spiices {first_spiice_id} and {conflicting_spiice_id} both declare lifecycle \
         scope {kind}; the kind is one fence namespace and one registry, so it has one \
         declaration or none"
    )]
    DuplicateScopeDeclaration {
        kind: ScopeKind,
        first_spiice_id: &'static str,
        conflicting_spiice_id: &'static str,
    },
    /// A scope declaration names something storage cannot splice — an
    /// unqualified registry table or an empty column name.
    #[error(
        "spiice {spiice_id}'s declaration of lifecycle scope {kind} is not spliceable: \
         {message}"
    )]
    InvalidScopeDeclaration {
        spiice_id: &'static str,
        kind: ScopeKind,
        message: &'static str,
    },
    /// A spiice other than #0 declared `proxiima://` resources. Resources are
    /// substrate-only: a spiice resource needs its own scope-key namespace,
    /// URI-template parser and pagination contract.
    #[error(
        "spiice {spiice_id} declares proxiima:// resources, which only spiice #0 may do: a \
         spiice resource needs its own scope-key namespace, URI-template parser and \
         pagination contract"
    )]
    ResourcesNotPermitted { spiice_id: &'static str },
    /// A spiice other than #0 named a hand-written erase or transfer leg.
    /// Only `proxiima-storage-pg` code runs bespoke legs, and that crate
    /// sees no spiice but core, so a non-core name is a promise nothing
    /// keeps: the verb would report success over rows no statement touched.
    #[error(
        "spiice {spiice_id} names a bespoke {verb} leg, which only spiice #0 may do: \
         hand-written legs run in proxiima-storage-pg, which sees no other spiice, so \
         every non-core surface must be reached by a generated leg"
    )]
    BespokeLegsNotPermitted {
        spiice_id: &'static str,
        verb: &'static str,
    },
    /// A spiice other than #0 declared `TransferRule::FollowOrDedupe`. The
    /// dedupe arm always needs a hand-written orchestrator (the refcount
    /// probe and object mount between its generated halves), which only
    /// core has.
    #[error(
        "spiice {spiice_id} declares TransferRule::FollowOrDedupe on {table}, which only \
         spiice #0 may do: the dedupe arm needs a hand-written orchestrator that only \
         proxiima-storage-pg runs; declare Follow, StaysOnKey, RetainAtSource or \
         NotTransferable instead"
    )]
    DedupeNotPermitted {
        spiice_id: &'static str,
        table: &'static str,
    },
    /// Contracts were registered but none of them is spiice #0. Core is
    /// non-removable — the two registry-reflection resources
    /// (`proxiima://schemas`, `proxiima://tools`) live in its contract.
    #[error("spiice contracts were registered but none is spiice #0; core is non-removable")]
    MissingCoreContract,
    /// A contract entry's schema id does not carry its spiice's prefix.
    #[error("spiice {spiice_id} declares schema {schema_id}, which does not carry its prefix")]
    ContractSchemaPrefix {
        spiice_id: &'static str,
        schema_id: SchemaId,
    },
    /// A surface declares itself exportable while carrying neither an owner
    /// column nor a key with a home table, so no statement can reach it from
    /// the owner. It would go missing from every bundle.
    #[error(
        "spiice {spiice_id} declares {table} ExportRule::Excluded, but the erase deletes it: \
         what an erase destroys the export must carry. Declare Rows, an Allowlist, or \
         DerivedFrom the surface its rows are rebuilt from"
    )]
    ErasedSurfaceNotExported {
        spiice_id: &'static str,
        table: &'static str,
    },
    #[error(
        "spiice {spiice_id} declares {table} DerivedFrom {source_table}, which no spiice \
         declares as a surface"
    )]
    DerivedFromUnknownSource {
        spiice_id: &'static str,
        table: &'static str,
        source_table: &'static str,
    },
    #[error(
        "spiice {spiice_id} declares {table} DerivedFrom {source_table}, but that chain of \
         sources never reaches a surface the bundle carries rows for (it ends Excluded or \
         loops)"
    )]
    DerivedFromUnexported {
        spiice_id: &'static str,
        table: &'static str,
        source_table: &'static str,
    },
    #[error(
        "spiice {spiice_id} declares {table} exportable, but it carries no owner \
         column and its key has no home table, so no export statement can reach \
         its owner"
    )]
    UnreachableExportSurface {
        spiice_id: &'static str,
        table: &'static str,
    },
    #[error(
        "spiice {spiice_id} declares {table} as host-state lifecycle state but no \
         owner_column; an erase fences one owner's rows through that column, so a \
         host-state surface names the column that carries its owner"
    )]
    HostStateWithoutOwnerColumn {
        spiice_id: &'static str,
        table: &'static str,
    },
    /// A schema declared itself listenable and published no JSON schema.
    ///
    /// A captured event names its `dataschema`
    /// (`proxiima://schema/{id}/{version}`), and a consumer that follows it
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
    /// the erase builds no selection set for (and, for core only, claimed
    /// by no bespoke leg). The erase would skip it in silence and report `Completed` over
    /// rows that survived the owner they belong to.
    #[error(
        "spiice {spiice_id} declares an erase for {table} that no leg can perform, so an \
         owner erase would skip it and still report success: key it on a memory, goal or \
         blob, give it an owner column with EraseRule::ByOwner, or declare \
         EraseRule::Cascade or EraseRule::Never"
    )]
    UndeletableSurface {
        spiice_id: &'static str,
        table: &'static str,
    },
    /// A declared bespoke erase leg that names nothing it could own — a
    /// table the spiice does not declare, or one whose declaration says no
    /// statement runs at all.
    /// Core only: any other spiice naming a leg is
    /// [`Self::BespokeLegsNotPermitted`] first.
    #[error("spiice {spiice_id} declares a bespoke erase leg for {table}, which {why}")]
    BespokeEraseLegMismatch {
        spiice_id: &'static str,
        table: &'static str,
        why: &'static str,
    },
    /// A surface declares a transfer no leg can perform: keyed on
    /// something the transfer builds no statement for (and, for core only,
    /// claimed by no bespoke leg). The transfer would skip it in silence and report
    /// success, leaving rows the SOURCE owner can still read after the
    /// memory they belong to became someone else's.
    #[error(
        "spiice {spiice_id} declares a transfer for {table} that no leg can perform, so a \
         transfer would skip it and still report success — leaving rows the source owner \
         can read after the memory became someone else's: key a Follow or Drop surface on \
         a memory or entity t, or declare StaysOnKey, RetainAtSource or NotTransferable"
    )]
    UnmovableSurface {
        spiice_id: &'static str,
        table: &'static str,
    },
    /// A declared bespoke transfer leg that names nothing it could own — a
    /// table the spiice does not declare, or one whose declaration says no
    /// statement runs at all.
    /// Core only: any other spiice naming a leg is
    /// [`Self::BespokeLegsNotPermitted`] first.
    #[error("spiice {spiice_id} declares a bespoke transfer leg for {table}, which {why}")]
    BespokeTransferLegMismatch {
        spiice_id: &'static str,
        table: &'static str,
        why: &'static str,
    },
    /// A surface declares that forget destroys or preserves its rows and the
    /// forget reaches none of them: `DeleteWithMemory` over a key it builds
    /// no `t` for, or `DumpThenCascade` without its MemoryT/completeness
    /// proof. The rows would outlive the memory that declared their fate.
    #[error(
        "spiice {spiice_id} declares that forget deletes {table} with the memory, and \
         the forget reaches none of its rows: the key is neither a memory nor an entity \
         t, and no completeness constraint claims them either"
    )]
    UnforgettableSurface {
        spiice_id: &'static str,
        table: &'static str,
    },
    /// A schema's `EmbeddingRecipe` and the units the drain resolves for it
    /// disagree about whether it embeds. The recipe is the claim; the units
    /// are what the machinery will actually be handed.
    #[error(
        "spiice {spiice_id} declares {schema_id} with EmbeddingRecipe::Never = \
         {recipe_is_never}, but the drain resolves embed units for it = \
         {machinery_embeds}; a schema that declares it embeds and hands the \
         drain nothing files embedding jobs the drain can only drop, and one \
         that declares Never while resolving a unit is silently embedded"
    )]
    EmbeddabilityDisagreement {
        spiice_id: &'static str,
        schema_id: SchemaId,
        recipe_is_never: bool,
        machinery_embeds: bool,
    },
    /// A schema declares `EmbeddingRecipe::Units(&[])` — the claim
    /// `Never { why }` exists to carry, with the reason deleted, and wearing
    /// the arm that means "embeds".
    #[error(
        "spiice {spiice_id} declares {schema_id} with EmbeddingRecipe::Units(&[]); \
         an empty unit list yields the drain no text, which is what \
         EmbeddingRecipe::Never says — declare Never and state why, because \
         Units answers `false` to is_never() and the enqueue lane believes it"
    )]
    EmptyEmbeddingUnits {
        spiice_id: &'static str,
        schema_id: SchemaId,
    },
    /// A contract's natural key disagrees with its registered typed payload,
    /// or names one column more than once.
    #[error(
        "spiice {spiice_id} declares an inconsistent natural key for {schema_id}: \
         payload type, schema, ordered typed fields and unique column names must agree"
    )]
    NaturalKeyDisagreement {
        spiice_id: &'static str,
        schema_id: SchemaId,
    },
    /// A tool's contract names different dispatcher actions, or names them
    /// in a different order, than the descriptor the registry holds.
    #[error(
        "spiice {spiice_id} declares actions for {name} that are not, in order, \
         the actions its registered dispatcher accepts"
    )]
    ToolActionsDisagreement {
        spiice_id: &'static str,
        name: &'static str,
    },
    /// A schema declared `NotTransferable` without naming where the refusal
    /// is enforced. A refusal nothing backs is a comment.
    #[error(
        "spiice {spiice_id} declares {schema_id} NotTransferable but names no enforcement \
         site; a refusal nothing backs is a comment"
    )]
    UnenforcedTransferRefusal {
        spiice_id: &'static str,
        schema_id: SchemaId,
    },
    /// The contract declares a schema that was never registered — erase,
    /// export and forget would walk a surface no write can produce.
    #[error(
        "spiice {spiice_id} declares {schema_id} v{schema_version} {kind:?} in its \
         contract but never registered it"
    )]
    ContractSchemaNotRegistered {
        spiice_id: &'static str,
        schema_id: SchemaId,
        schema_version: SchemaVersion,
        kind: PayloadKind,
    },
    /// A schema was registered under a spiice's prefix but its contract does
    /// not declare it — the drift that makes a registry walk miss a surface.
    #[error(
        "schema {schema_id} v{schema_version} {kind:?} is registered under spiice \
         {spiice_id} but its contract does not declare it, so every registry walk \
         (erase, export, forget, transfer) would miss its surfaces"
    )]
    SchemaWithoutContract {
        spiice_id: &'static str,
        schema_id: SchemaId,
        schema_version: SchemaVersion,
        kind: PayloadKind,
    },
    /// A spiice registered a `SpiiceDescriptor` — it is linked into this
    /// binary — and declared no `SpiiceContract`. Everything it registers
    /// is then invisible to erase, export, forget and transfer, and its
    /// Memory writes are refused later by a `spiice_surface` constraint
    /// that names none of the cause.
    #[error(
        "spiice {spiice_id} is linked into this binary and declares no SpiiceContract, \
         so every registry walk (erase, export, forget, transfer) skips whatever it \
         registers and its Memory writes are refused later by a spiice_surface \
         constraint naming none of this; declare a SpiiceContract for {spiice_id} and \
         register it, which for a spiice built with proxiima_spiice! means adding \
         `contract = &<YOUR_CONTRACT>` to the macro"
    )]
    UnclaimedRegistration { spiice_id: String },
    /// A projected schema's sidecar declares no surface keyed on the
    /// memory `t`. The projection generator spells each row's key from
    /// that column, so the schema has no projection statement to generate.
    #[error(
        "spiice {spiice_id} declares {schema_id} a search projection over sidecar \
         {table}, and no surface of this spiice declares {table} keyed on the memory t; \
         the projection generator spells each projection row's key from that column, so \
         there is no statement to generate -- declare a surface for {table} with \
         `key: KeyShape::MemoryT {{ column: .. }}`"
    )]
    ProjectedSidecarNotMemoryKeyed {
        spiice_id: &'static str,
        schema_id: SchemaId,
        table: &'static str,
    },
    /// An embedding schema's sidecar declares no surface keyed on the
    /// memory `t`. The drain's text read filters that column, so the schema
    /// has no statement to generate — the twin of
    /// [`Self::ProjectedSidecarNotMemoryKeyed`] on the embedding lane.
    #[error(
        "spiice {spiice_id} declares {schema_id} an embed unit on sidecar {table}, and \
         no surface of this spiice declares {table} keyed on the memory t; the drain \
         filters the text read on that column, so there is no statement to generate -- \
         declare a surface for {table} with `key: KeyShape::MemoryT {{ column: .. }}`"
    )]
    EmbeddedSidecarNotMemoryKeyed {
        spiice_id: &'static str,
        schema_id: SchemaId,
        table: &'static str,
    },
    /// A schema declares a search projection no request shape can
    /// scan. Every write to it pays a projection row and a GIN index entry
    /// for a corpus no query reaches.
    #[error(
        "spiice {spiice_id} declares {schema_id} as a search projection that \
         core_search_memories can never scan: {why}. Every write to it still pays a \
         projection row and a GIN index entry -- declare comparable CoreBands, \
         declare SearchProjectionDecl::None {{ why, tag_column: None }} \
         if it is not a search surface, or declare \
         RankSource::SidecarWithProjectionOwner {{ why }} if the spiice's own tools \
         rank it"
    )]
    UnreachableSearchProjection {
        spiice_id: &'static str,
        schema_id: SchemaId,
        /// Which reachability condition the declaration fails.
        why: &'static str,
    },
    /// The contract names an MCP tool that was never registered.
    #[error(
        "spiice {spiice_id} declares MCP tool {name} in its contract but never \
         registered it"
    )]
    ContractToolNotRegistered {
        spiice_id: &'static str,
        name: &'static str,
    },
    /// One projection unit declares more distinct weight levels than
    /// `PostgreSQL` has tsvector weight classes.
    #[error(
        "spiice {spiice_id} schema {schema_id} declares {levels} distinct field \
         weights, but `PostgreSQL` stores a two-bit weight per lexeme position and \
         offers exactly {classes} tsvector classes (A, B, C, D — see `PostgreSQL` \
         12.3.1); collapsing two levels into one class would make ts_rank's weight \
         array describe a document it is not scoring"
    )]
    ProjectionWeightLevels {
        spiice_id: &'static str,
        schema_id: SchemaId,
        levels: usize,
        classes: usize,
    },
    /// A cited-object schema declared a sidecar table.
    /// Those tables point at a blob row by convention rather than by
    /// constraint, so the shared-blob dedupe arm's remap cannot find them.
    #[error(
        "spiice {spiice_id} schema {schema_id} declares sidecar table {table} for a \
         citation payload; a cross-owner transfer now dedupes a shared blob onto a \
         NEW blob row, and the columns that must follow it are the ones declared on \
         `TransferRule::FollowOrDedupe` -- a citation sidecar points at a blob by \
         convention with no SQL foreign key, so nothing would repoint it and the \
         rows would keep naming the source owner's row"
    )]
    CitationSidecarNotRemappable {
        spiice_id: &'static str,
        schema_id: SchemaId,
        table: &'static str,
    },
    /// A `LanguagePolicy::PerRow` names a projection column the projection
    /// table does not have.
    #[error(
        "spiice {spiice_id} schema {schema_id} declares LanguagePolicy::PerRow on \
         projection column {declared}, but its projection table's language column is \
         {}; the generator writes one column per projection table, so a second name \
         would be a declaration nothing renders and every row would be stamped and \
         ranked under a configuration the contract never named",
        .projection_column.unwrap_or("absent")
    )]
    ProjectionLanguageColumn {
        spiice_id: &'static str,
        schema_id: SchemaId,
        declared: &'static str,
        projection_column: Option<&'static str>,
    },
    /// A declared tag column must be backed by a memory-keyed sidecar.
    #[error(
        "spiice {spiice_id} schema {schema_id} declares tag column {tag_column}, but its \
         sidecar is not keyed by memory t"
    )]
    TagColumnNotMemoryKeyed {
        spiice_id: &'static str,
        schema_id: SchemaId,
        tag_column: &'static str,
    },
    /// A spiice declaring `RankSource::Projection` is served by ONE
    /// statement per spiice, and a property that statement can only spell
    /// once differs between two of its projected schemas.
    ///
    /// Caught at freeze rather than at query-build time on purpose: finding
    /// out that a spiice cannot be rendered is a `StorageError` on a hot
    /// path, and the answer never depends on the request.
    #[error(
        "spiice {spiice_id} declares RankSource::Projection, so one statement serves \
         all of its projected schemas -- but schema {schema_id} declares a different \
         {property} from the spiice's first projected schema, and one statement can \
         spell {property} only once"
    )]
    ProjectionRenderNotUniform {
        spiice_id: &'static str,
        schema_id: SchemaId,
        /// `"language"` or `"bands"`.
        property: &'static str,
    },
    /// A `RankSource::Projection` schema does not declare a band under a
    /// name the core renderer resolves. A `&[Band]` is an unordered set
    /// with a `name` on each member, so the renderer's lookup is by string
    /// — and this is the check that keeps the string honest.
    #[error(
        "spiice {spiice_id} declares RankSource::Projection, whose renderer resolves \
         its arms by band name -- but schema {schema_id} declares no band named \
         {missing:?}, so that arm would have no window to score in"
    )]
    ProjectionBandName {
        spiice_id: &'static str,
        schema_id: SchemaId,
        missing: &'static str,
    },
    /// A spiice claims `BandComparability::CoreBands` while one of its
    /// schemas declares a band outside spiice #0's `[0.0, 1.0]` window.
    /// The claim is what a cross-spiice merge compares scores on, so it has
    /// to be earned rather than decorated.
    #[error(
        "spiice {spiice_id} claims BandComparability::CoreBands, but schema \
         {schema_id} declares band {band:?} as {window}, outside spiice #0's \
         [0, 1] window; a merge that compared those scores numerically would be \
         comparing two different scales"
    )]
    ProjectionBandOutsideCoreWindow {
        spiice_id: &'static str,
        schema_id: SchemaId,
        band: &'static str,
        /// The offending window, rendered — the enum derives `Eq`, and an
        /// `f32` pair would not.
        window: String,
    },
}
