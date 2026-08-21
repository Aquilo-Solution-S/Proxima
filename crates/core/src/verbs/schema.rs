//! Schema verb — registry introspection.
//!
//! See docs/14-protocol-surface.md §"Schema" and
//! docs/03-schema-registry.md.

use crate::authz::{AuthorizationHook, AuthzContext, AuthzInput, AuthzOutcome, OwnerResolver};
use crate::error::ProtocolError;
use crate::flavor::contract::{
    Band, BandComparability, FlavorContract, LanguagePolicy, RankSource, SearchProjectionDecl,
    SubstringArm,
};
use crate::mcp::RequestBehavior;
use crate::{
    CapabilityTag, FlavorDescriptor, McpToolDescriptor, Owner, SchemaId, SchemaVersion,
    SearchProjectionColumnKind, SidecarPayload,
};
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

pub type ProtocolPayloadIngress = fn(&serde_json::Value) -> Result<ProtocolPayload, String>;

#[derive(Debug, Clone)]
pub struct ProtocolPayload {
    pub key_bytes: Option<Vec<u8>>,
    pub sidecar_payload: SidecarPayload,
    pub rendered_text: Option<String>,
    pub content_hash: Option<[u8; 32]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaCapabilityTags {
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub kind: PayloadKind,
    pub tags: BTreeSet<CapabilityTag>,
}

#[derive(Debug, Clone)]
pub(crate) struct ProtocolPayloadIngressEntry {
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub kind: PayloadKind,
    pub ingress: ProtocolPayloadIngress,
    pub json_schema: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum PayloadKind {
    Fact,
    Abstraction,
    Perspective,
    Goal,
    CitedObject,
    CitationMapping,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SchemaTombstone {
    pub column: String,
    pub value: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SchemaInfo {
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub kind: PayloadKind,
    pub filter_keys: Vec<String>,
    /// Sidecar table identifier (qualified, e.g. `proxima_code.code_chunk_v1`)
    /// when the payload trait declares one, including typed `CitedObject` and
    /// `CitationMapping` sidecars. `None` is valid for typed Fact and Goal
    /// schemas whose payload needs no schema-owned columns, and for opaque
    /// citation schemas.
    pub sidecar_table: Option<String>,
    /// Natural-key columns for stateful Fact schemas (docs/03 §Stateful
    /// Fact schemas). Empty for stateless / non-Fact schemas. Decides
    /// which series `handle` an ingest lands on, which is what makes
    /// heads-only `Query` return one row per key.
    pub natural_key_columns: Vec<String>,
    /// Build-time catalog discriminator for stateful Fact deletion
    /// observations. Query still returns the hot head.
    pub tombstone: Option<SchemaTombstone>,
    /// Typed/opaque discriminant. The frozen registry owns the actual
    /// process-local protocol-ingress function pointers.
    pub has_typed_ingress: bool,
    /// `CitedObjectPayload` schema id accepted by a `CitationMappingPayload`.
    /// Populated only for citation-mapping schemas.
    pub cited_object_schema: Option<SchemaId>,
    /// `FactPayload::EMBEDDABLE`. `true` for every other kind: derived
    /// memories are embedded inside their own write and cited
    /// objects/mappings are not memories at all, so the flag would have
    /// nothing to gate.
    pub embeddable: bool,
}

impl SchemaInfo {
    /// Construct an *opaque* schema — one with no Rust payload type.
    /// Used for content-addressed `CitedObject`s and structural
    /// `CitationMapping`s whose payload is an opaque blob addressed by
    /// content hash. An opaque schema carries no validator, no JSON
    /// ingress parser, no JSON schema, and no sidecar table.
    ///
    /// `has_typed_ingress == false` is the typed/opaque discriminant the
    /// registry enforces: `FlavorRegistry::try_freeze` asserts every schema
    /// either has a protocol-ingress parser or is an opaque `CitedObject` /
    /// `CitationMapping` schema.
    /// See docs/03 §Registry rules.
    #[must_use]
    pub(crate) fn opaque(
        schema_id: SchemaId,
        schema_version: SchemaVersion,
        kind: PayloadKind,
    ) -> Self {
        Self {
            schema_id,
            schema_version,
            kind,
            filter_keys: Vec::new(),
            sidecar_table: None,
            natural_key_columns: Vec::new(),
            tombstone: None,
            has_typed_ingress: false,
            cited_object_schema: None,
            // An opaque schema is a cited object or a citation mapping,
            // never a memory, so nothing here is ever a candidate for a
            // vector. `true` keeps the default meaning "nobody opted out".
            embeddable: true,
        }
    }
}

#[must_use]
pub fn sidecar_tables(schemas: &[SchemaInfo], kind: PayloadKind) -> Vec<String> {
    let mut tables = schemas
        .iter()
        .filter(|schema| schema.kind == kind)
        .filter_map(|schema| schema.sidecar_table.clone())
        .collect::<Vec<_>>();
    tables.sort();
    tables.dedup();
    tables
}

#[derive(Debug, Clone, PartialEq)]
pub struct MemorySearchProjectionField {
    pub column: String,
    pub kind: SearchProjectionColumnKind,
    /// The declared relative weight. Uniform across a unit in v0.0.8.
    pub weight: f32,
}

/// One search surface, as the SQL builders need it — the runtime reading of
/// a `SchemaContract` whose `search` is `Projected`.
///
/// Built from the flavor contracts at freeze. There used to be a second
/// source, `FactPayload::search_projection()`, which the contract's
/// declaration shadowed without governing; a projection nothing enforced is
/// exactly the drift the contract exists to remove.
#[derive(Debug, Clone, PartialEq)]
pub struct MemorySearchProjection {
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub kind: PayloadKind,
    /// The schema's own sidecar — still the home of the raw text, which
    /// the substring arm scans and the top-k snippet join reads.
    pub sidecar_table: String,
    /// The flavor's projection table, where the vector lives.
    pub projection_table: String,
    pub fields: Vec<MemorySearchProjectionField>,
    /// Sidecar column the projection's `tag` array was copied from.
    pub tag_column: Option<String>,
    /// Whether the row carries its own configuration, or which one is
    /// pinned for the whole surface.
    pub language: LanguagePolicy,
    /// `ts_rank`'s `{D, C, B, A}` array when the unit declares more than one
    /// weight level. `None` — the uniform case — passes no array, which is
    /// what keeps the score identical to the unweighted vector's.
    pub rank_weights: Option<[f32; 4]>,
    /// The score windows the arms over this schema render, resolved by
    /// [`Band::name`]. Read at query-build time; freeze holds a
    /// projection-ranked flavor to the three names the core renderer
    /// resolves.
    pub bands: &'static [Band],
    /// Whether this schema opts into a substring arm, and in which shape.
    /// [`SubstringArm::Off`] means the arm contributes no statement and no
    /// rows — which is what makes deleting the blanket `LIKE` retry a
    /// mechanism change rather than a recall cut.
    pub substring: SubstringArm,
    /// The owning flavor's candidate budget for ONE statement over its
    /// projection ([`ProjectionSpec::overfetch_k`]).
    ///
    /// [`ProjectionSpec::overfetch_k`]: crate::flavor::contract::ProjectionSpec::overfetch_k
    pub overfetch_k: u32,
    /// The owning flavor's band-comparability claim
    /// ([`ProjectionSpec::band_comparability`]). Core's merge admits a
    /// non-core projection only under [`BandComparability::CoreBands`].
    ///
    /// [`ProjectionSpec::band_comparability`]: crate::flavor::contract::ProjectionSpec::band_comparability
    pub band_comparability: BandComparability,
    /// The owning flavor's read shape ([`ProjectionSpec::rank_source`]).
    /// Core's renderer serves [`RankSource::Projection`] flavors only.
    ///
    /// [`ProjectionSpec::rank_source`]: crate::flavor::contract::ProjectionSpec::rank_source
    pub rank_source: RankSource,
}

/// One `(sidecar table, column)` the embedding drain reads text from.
///
/// Split out of `MemorySearchProjection`, which used to carry
/// `embed_text_column` inside a *search* declaration. That conflation made
/// a schema that embeds but does not search — `proxima-code/file-revision-v1`
/// is exactly one — unstateable: it had to declare a search projection it
/// did not have in order to be embedded at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryEmbedUnit {
    pub schema_id: SchemaId,
    pub schema_version: SchemaVersion,
    pub kind: PayloadKind,
    pub sidecar_table: String,
    pub column: String,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SchemaRequest;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SchemaResponse {
    pub schemas: Vec<SchemaInfo>,
}

/// Build-time lookup acceleration for `FlavorRegistryFrozen`. Each map
/// stores the position of the *first* matching entry in the owning
/// `Vec`, mirroring the `.iter().find()` first-wins semantics of the
/// linear scans it replaces. The frozen vocabulary has no mutation
/// surface, so a stored index never goes stale.
///
/// Only the collections that scale with *schema* count and sit on the
/// `FactIngest` / `GoalWrite` paths are indexed. `flavors` scales with
/// linked crate count and stays a linear scan — indexing a handful of
/// entries would not earn its keep.
#[derive(Debug, Clone, Default)]
struct FrozenIndex {
    /// `schemas` keyed by `(schema_id, version)`, kind-agnostic.
    schema_by_id_version: HashMap<(SchemaId, SchemaVersion), usize>,
    /// `schemas` keyed by `(schema_id, version, kind)` — required
    /// because one payload type may register across F/A/P layers.
    schema_by_id_version_kind: HashMap<(SchemaId, SchemaVersion, PayloadKind), usize>,
    /// Protocol-ingress parsers keyed by `(schema_id, version, kind)`.
    protocol_ingress_by_key: HashMap<(SchemaId, SchemaVersion, PayloadKind), usize>,
    /// Schema ids of Fact schemas that declared `EMBEDDABLE = false`.
    ///
    /// Precomputed as a flat `Vec<String>` because its consumer is a SQL
    /// bind: the enqueue-side queries exclude these ids, and building the
    /// list per call would rescan every schema on every reconcile. Sorted
    /// and deduplicated so the bound array is stable across processes —
    /// a query plan should not depend on registration order.
    non_embeddable_schema_ids: Vec<String>,
}

impl FrozenIndex {
    fn build(schemas: &[SchemaInfo], protocol_ingress: &[ProtocolPayloadIngressEntry]) -> Self {
        let mut index = Self::default();
        for (position, schema) in schemas.iter().enumerate() {
            index
                .schema_by_id_version
                .entry((schema.schema_id.clone(), schema.schema_version))
                .or_insert(position);
            index
                .schema_by_id_version_kind
                .entry((schema.schema_id.clone(), schema.schema_version, schema.kind))
                .or_insert(position);
        }
        for (position, ingress) in protocol_ingress.iter().enumerate() {
            index
                .protocol_ingress_by_key
                .entry((
                    ingress.schema_id.clone(),
                    ingress.schema_version,
                    ingress.kind,
                ))
                .or_insert(position);
        }
        index.non_embeddable_schema_ids = schemas
            .iter()
            .filter(|schema| schema.kind == PayloadKind::Fact && !schema.embeddable)
            .map(|schema| schema.schema_id.as_str().to_owned())
            .collect();
        index.non_embeddable_schema_ids.sort();
        index.non_embeddable_schema_ids.dedup();
        index
    }
}

#[derive(Debug, Clone)]
pub struct FlavorRegistryFrozen {
    schemas: Vec<SchemaInfo>,
    schema_capability_tags:
        HashMap<(SchemaId, SchemaVersion, PayloadKind), BTreeSet<CapabilityTag>>,
    search_projections: Vec<MemorySearchProjection>,
    embed_units: Vec<MemoryEmbedUnit>,
    protocol_ingress: Vec<ProtocolPayloadIngressEntry>,
    mcp_tools: Vec<McpToolDescriptor>,
    request_behaviors: Vec<Arc<dyn RequestBehavior>>,
    flavors: Vec<FlavorDescriptor>,
    contracts: Vec<&'static FlavorContract>,
    owner_resolver: Option<Arc<dyn OwnerResolver>>,
    authorization_hooks: Vec<Arc<dyn AuthorizationHook>>,
    /// Lookup acceleration built during successful freeze. Not part of the
    /// logical registry — derived purely from the `Vec`s above.
    index: FrozenIndex,
}

/// Every search surface the linked contracts declare, in contract order.
///
/// This is the collapse: one vocabulary, read once, at freeze. A schema
/// whose contract says `SearchProjectionDecl::None` contributes nothing —
/// and, unlike the old `search_projection() -> None`, it said why.
fn contract_search_projections(
    contracts: &[&'static FlavorContract],
) -> Vec<MemorySearchProjection> {
    let mut out = Vec::new();
    for contract in contracts {
        let Some(spec) = contract.projection.spec() else {
            continue;
        };
        for (schema, sidecar_table) in contract.projected_schemas() {
            // Nothing is discarded here any more. `bands` and `substring`
            // used to fall into the `..`, which is what made a declared
            // band a value no reader could reach and the substring arm a
            // blanket retry rather than an opt-in.
            let SearchProjectionDecl::Projected {
                fields,
                tag_column,
                language,
                bands,
                substring,
            } = &schema.search
            else {
                continue;
            };
            out.push(MemorySearchProjection {
                schema_id: schema.schema_id(),
                schema_version: schema.schema_version(),
                kind: schema.kind,
                sidecar_table: sidecar_table.to_owned(),
                projection_table: spec.table.to_owned(),
                fields: fields
                    .iter()
                    .map(|field| MemorySearchProjectionField {
                        column: field.column.to_owned(),
                        kind: field.kind,
                        weight: field.weight,
                    })
                    .collect(),
                tag_column: tag_column.map(str::to_owned),
                language: *language,
                rank_weights: schema.search.rank_weight_array(),
                bands,
                substring: *substring,
                overfetch_k: spec.overfetch_k,
                band_comparability: spec.band_comparability,
                rank_source: spec.rank_source,
            });
        }
    }
    out
}

/// Every stored embed-text column the linked contracts declare.
fn contract_embed_units(contracts: &[&'static FlavorContract]) -> Vec<MemoryEmbedUnit> {
    let mut out = Vec::new();
    for contract in contracts {
        for schema in contract.schemas {
            for unit in schema.embed_units() {
                let (Some(table), Some(column)) = (unit.table, unit.column) else {
                    continue;
                };
                out.push(MemoryEmbedUnit {
                    schema_id: schema.schema_id(),
                    schema_version: schema.schema_version(),
                    kind: schema.kind,
                    sidecar_table: table.to_owned(),
                    column: column.to_owned(),
                });
            }
        }
    }
    out
}

impl FlavorRegistryFrozen {
    /// Freeze a `FlavorRegistry` into its immutable, index-accelerated
    /// form — called by `FlavorRegistry::try_freeze` after validation.
    /// Consumes the builder whole and rehomes
    /// every vocabulary field. A new vocabulary kind is added in the
    /// two struct definitions plus the destructure below, which the
    /// compiler keeps exhaustive; the constructor signature never
    /// widens.
    pub(crate) fn from_registry(registry: crate::FlavorRegistry) -> Self {
        let crate::FlavorRegistry {
            schemas,
            schema_capability_tags,
            protocol_ingress,
            mcp_tools,
            request_behaviors,
            flavors,
            contracts,
            owner_resolver,
            authorization_hooks,
        } = registry;
        let schema_capability_tags = crate::flavor::schema_capability_map(&schema_capability_tags);
        let index = FrozenIndex::build(&schemas, &protocol_ingress);
        let search_projections = contract_search_projections(&contracts);
        let embed_units = contract_embed_units(&contracts);
        Self {
            schemas,
            schema_capability_tags,
            search_projections,
            embed_units,
            protocol_ingress,
            mcp_tools,
            request_behaviors,
            flavors,
            contracts,
            owner_resolver,
            authorization_hooks,
            index,
        }
    }

    /// One flavor's contract, by id.
    ///
    /// Deliberately the only lookup on this type. The declarations are
    /// `static`s: a consumer that knows which flavor it means reads
    /// [`crate::FLAVOR_0`] directly and needs no registry at all. This
    /// exists for the consumers that do NOT know — the storage sidecar
    /// registry cross-checking whatever flavors happen to be linked. A
    /// broader accessor surface here would be API nobody calls, with the
    /// second copy of every walk that implies.
    /// Every linked flavor's contract, in registration order.
    ///
    /// The projection guardrail needs the whole set, not one by name: what
    /// it checks is that the DATABASE and the LINKED FLAVORS agree, and
    /// neither side can be asked schema by schema.
    #[must_use]
    pub fn contracts(&self) -> &[&'static FlavorContract] {
        &self.contracts
    }

    #[must_use]
    pub fn flavor_contract(&self, flavor_id: &str) -> Option<&'static FlavorContract> {
        self.contracts
            .iter()
            .copied()
            .find(|contract| contract.flavor_id == flavor_id)
    }

    /// Memory sidecar tables whose rows stay with the SOURCE owner on
    /// transfer — [`crate::flavor::TransferRule::RetainAtSource`], the
    /// declaration that replaces `pg_sidecar!(owner_pinned: true)` as the
    /// authority.
    ///
    /// Owner erase and export select these by the sidecar's own owner
    /// rather than through the Memory, because a transfer leaves them
    /// behind: joining through the Memory would put them out of the writing
    /// owner's reach and into the receiving owner's bundle.
    #[must_use]
    pub fn retain_at_source_sidecar_tables(&self) -> Vec<String> {
        let mut tables = self
            .contracts
            .iter()
            .flat_map(|contract| contract.retain_at_source_tables())
            .collect::<Vec<_>>();
        tables.sort();
        tables.dedup();
        tables
    }

    #[must_use]
    pub fn search_projections(&self) -> &[MemorySearchProjection] {
        &self.search_projections
    }

    /// The `(table, column)` pairs the embedding drain reads text from.
    #[must_use]
    pub fn embed_units(&self) -> &[MemoryEmbedUnit] {
        &self.embed_units
    }

    #[must_use]
    pub fn schemas(&self) -> &[SchemaInfo] {
        &self.schemas
    }

    /// Whether a Fact written under `schema_id` earns a vector.
    ///
    /// UNKNOWN SCHEMAS ARE EMBEDDABLE, and that direction is chosen. The
    /// two failure modes are not symmetric: a vector nobody wanted is
    /// waste, while a missing vector is a memory that is semantically
    /// invisible and reports no error anywhere. Opting out is a
    /// declaration, so it must be present to take effect.
    ///
    /// Version-agnostic, because embeddability is a statement about what
    /// a schema's text IS rather than about its shape. A v2 that wanted
    /// the opposite answer would be describing different content.
    #[must_use]
    pub fn schema_is_embeddable(&self, schema_id: &str) -> bool {
        !self
            .index
            .non_embeddable_schema_ids
            .iter()
            .any(|id| id == schema_id)
    }

    /// Fact schema ids that declared `EMBEDDABLE = false`, sorted.
    ///
    /// For the enqueue-side queries, which exclude them: the inline write
    /// path is not enough on its own, because reconciliation would find
    /// the rows it skipped and enqueue them anyway.
    #[must_use]
    pub fn non_embeddable_schema_ids(&self) -> &[String] {
        &self.index.non_embeddable_schema_ids
    }

    #[must_use]
    pub fn schema_capability_tags(
        &self,
        schema_id: &SchemaId,
        version: SchemaVersion,
        kind: PayloadKind,
    ) -> Option<&BTreeSet<CapabilityTag>> {
        self.schema_capability_tags
            .get(&(schema_id.clone(), version, kind))
    }

    #[must_use]
    pub fn list_mcp_tools(&self) -> &[McpToolDescriptor] {
        &self.mcp_tools
    }

    /// The descriptor registered under `name`, if any.
    ///
    /// Several call sites open-coded this scan — both `ScopeGateBehavior`
    /// halves and wake-tool validation among them, the last of which
    /// allocated a `HashSet<String>` of every registered tool id per call.
    /// Exact-name only: the REST router keeps its own lookup because it also
    /// accepts a request's provider-safe alias for a canonical name.
    #[must_use]
    pub fn mcp_tool(&self, name: &str) -> Option<&McpToolDescriptor> {
        self.mcp_tools.iter().find(|tool| tool.name == name)
    }

    #[must_use]
    pub fn request_behaviors(&self) -> &[Arc<dyn RequestBehavior>] {
        &self.request_behaviors
    }

    #[must_use]
    pub fn payload_json_schema(
        &self,
        schema_id: &SchemaId,
        schema_version: SchemaVersion,
        kind: PayloadKind,
    ) -> Option<&serde_json::Value> {
        let position =
            *self
                .index
                .protocol_ingress_by_key
                .get(&(schema_id.clone(), schema_version, kind))?;
        self.protocol_ingress[position].json_schema.as_ref()
    }

    #[must_use]
    pub fn mcp_tool_ids(&self) -> std::collections::HashSet<String> {
        self.mcp_tools
            .iter()
            .map(|tool| tool.name.to_string())
            .collect()
    }

    /// # Errors
    ///
    /// Returns the registered resolver error when owner resolution denies.
    pub(crate) fn resolve_owner(
        &self,
        authz: &AuthzContext,
        requested: &Owner,
    ) -> Result<Owner, ProtocolError> {
        match &self.owner_resolver {
            Some(resolver) => resolver.resolve(authz, requested),
            None => Ok(*requested),
        }
    }

    /// # Errors
    ///
    /// Returns `Forbidden` when an authorization hook vetoes the request.
    pub(crate) fn run_authorization_vetoes(
        &self,
        input: &AuthzInput<'_>,
    ) -> Result<(), ProtocolError> {
        for hook in &self.authorization_hooks {
            hook.veto(input)
                .map_err(|veto| ProtocolError::forbidden(veto.0))?;
        }
        Ok(())
    }

    pub(crate) fn run_authorization_observers(
        &self,
        input: &AuthzInput<'_>,
        outcome: AuthzOutcome,
    ) {
        for hook in &self.authorization_hooks {
            hook.observe(input, outcome);
        }
    }

    /// All `FlavorDescriptor`s registered through `proxima_flavor!`.
    /// Order matches macro invocation order.
    #[must_use]
    pub fn list_flavors(&self) -> &[FlavorDescriptor] {
        &self.flavors
    }

    /// Lookup a flavor descriptor by its `flavor_id`
    /// (e.g. `"proxima-code"`).
    #[must_use]
    pub fn flavor(&self, flavor_id: &str) -> Option<&FlavorDescriptor> {
        self.flavors.iter().find(|f| f.flavor_id == flavor_id)
    }

    #[must_use]
    pub fn list(&self) -> Vec<SchemaInfo> {
        self.schemas.clone()
    }

    /// Lookup by `(schema_id, schema_version)`. Used by
    /// `FactIngest` / `GoalWrite` to validate incoming payloads.
    #[must_use]
    pub fn lookup(&self, schema_id: &SchemaId, version: SchemaVersion) -> Option<&SchemaInfo> {
        let position = *self
            .index
            .schema_by_id_version
            .get(&(schema_id.clone(), version))?;
        Some(&self.schemas[position])
    }

    /// Lookup by `(schema_id, schema_version, kind)`. Required when one
    /// typed payload is registered for multiple F/A/P layers.
    #[must_use]
    pub fn lookup_payload(
        &self,
        schema_id: &SchemaId,
        version: SchemaVersion,
        kind: PayloadKind,
    ) -> Option<&SchemaInfo> {
        let position =
            *self
                .index
                .schema_by_id_version_kind
                .get(&(schema_id.clone(), version, kind))?;
        Some(&self.schemas[position])
    }

    /// Convert a protocol JSON payload into the build-time registered
    /// Rust payload type. Opaque schemas are not valid on this path;
    /// callers that accept opaque content use the explicit opaque
    /// citation APIs instead.
    ///
    /// # Errors
    ///
    /// Returns an error string when `payload` is not a JSON object or when
    /// the registered typed parser rejects it.
    pub fn ingest_protocol_payload(
        &self,
        schema_id: &SchemaId,
        version: SchemaVersion,
        kind: PayloadKind,
        payload: &serde_json::Value,
    ) -> Result<ProtocolPayload, String> {
        if !payload.is_object() {
            return Err("typed payload must be a JSON object".into());
        }

        if let Some(&position) =
            self.index
                .protocol_ingress_by_key
                .get(&(schema_id.clone(), version, kind))
        {
            return (self.protocol_ingress[position].ingress)(payload);
        }

        Err(format!(
            "schema {} v{} {:?} has no typed protocol ingress",
            schema_id.as_str(),
            version.into_inner(),
            kind,
        ))
    }

    #[must_use]
    pub fn handle(&self, _req: &SchemaRequest) -> SchemaResponse {
        SchemaResponse {
            schemas: self.list(),
        }
    }
}
