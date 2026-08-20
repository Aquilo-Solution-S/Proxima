//! The flavor contract — declarations, not code.
//!
//! A flavor states, per schema, what its rows *are*: whether they are a
//! search surface, how their embed text is produced, what a transfer does
//! to them, and which physical surfaces erase / export / forget must walk.
//! Core consumes the declarations by iterating the registry, so a surface
//! cannot be missed by forgetting to add it to a hand-written list — the
//! bug class the v0.0.8 plan cites (#215, #223/#224, `lexical_language_forget`).
//!
//! Three rules shape every type here:
//!
//! 1. **Declared absence is a value.** [`SearchProjectionDecl::None`] and
//!    [`EmbeddingRecipe::Never`] carry a `why` and are distinct from "nobody
//!    wrote a declaration". Today `FactPayload::search_projection() -> None`
//!    is the same value three different ways
//!    (`flavor/schema_registration.rs`: no projection, empty fields, no
//!    sidecar table), which is precisely why a non-surface cannot be told
//!    from an oversight.
//! 2. **Every rule is non-optional on a [`Surface`].** A new table that
//!    forgets to say what forget does will not compile.
//! 3. **A constraint beats a list.** [`Surface::completeness`] names the
//!    FK/CHECK that already proves the surface is reached; the standing rule
//!    for generators is that a surface with `completeness: Some(_)`
//!    contributes zero lines to any emitted enumeration.
//!
//! Everything is `const`-constructible: a contract is a `static`, so the
//! declaration is available before a database connection exists.

use crate::verbs::schema::PayloadKind;
use crate::{SchemaId, SchemaVersion, SearchProjectionColumnKind};

// ── Identity ────────────────────────────────────────────────────────────

/// A schema id in its parts. Renders `"<flavor>/<name>-v<version>"` — the
/// `_vN` idiom the tree already uses (`core/agent-note-v1`), and the shape
/// `proxima_flavor!`'s existing compile-time prefix assertion enforces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SchemaRef {
    pub flavor: &'static str,
    pub name: &'static str,
    pub version: u32,
}

impl SchemaRef {
    #[must_use]
    pub const fn new(flavor: &'static str, name: &'static str, version: u32) -> Self {
        Self {
            flavor,
            name,
            version,
        }
    }

    /// `"<flavor>/<name>-v<version>"`.
    #[must_use]
    pub fn render(&self) -> String {
        format!("{}/{}-v{}", self.flavor, self.name, self.version)
    }

    #[must_use]
    pub fn schema_id(&self) -> SchemaId {
        SchemaId::new(self.render())
    }

    #[must_use]
    pub fn schema_version(&self) -> SchemaVersion {
        SchemaVersion::new(self.version)
    }
}

// ── Search ──────────────────────────────────────────────────────────────

/// `PostgreSQL`'s four tsvector weight classes, **lowest first**.
///
/// `D` leads because `D` is what an unweighted `to_tsvector` produces:
/// "The default weight is `D` for lexemes that have no explicit weight"
/// (`PostgreSQL` *12.3.1*). A projection unit that declares one level is
/// therefore rank-identical to today's unweighted vector, which is what the
/// membership-identity re-proof rests on.
pub const TSVECTOR_WEIGHT_CLASSES: [&str; 4] = ["D", "C", "B", "A"];

/// `PostgreSQL`'s default `ts_rank` weight array, `{D, C, B, A}`
/// (*12.3.3 Ranking Search Results*). Classes a projection unit does not
/// use keep these values.
pub const DEFAULT_RANK_WEIGHTS: [f32; 4] = [0.1, 0.2, 0.4, 1.0];

/// One projected column with its **relative** weight.
///
/// The weight is an arbitrary float, not a letter: `PostgreSQL` forces four
/// classes on the *storage*, and encoding that limit in the declaration
/// would make a flavor choose a bucket before it has said what it means.
/// A flavor states relative importance; the generator buckets the distinct
/// values it finds into `setweight` classes (lowest → `D`) and the same
/// floats become `ts_rank`'s weight array at read time. More than
/// [`TSVECTOR_WEIGHT_CLASSES`]`.len()` distinct levels on one unit is a
/// freeze error naming the `PostgreSQL` mechanism, not a silent collapse.
///
/// `kind` is the existing [`SearchProjectionColumnKind`], so the contract
/// cannot describe a column shape the search builder cannot render.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WeightedField {
    pub column: &'static str,
    pub kind: SearchProjectionColumnKind,
    pub weight: f32,
}

/// The one weight every flavor #0 field declares in v0.0.8.
///
/// Uniform ⇒ one distinct level ⇒ every lexeme lands in class `D` and the
/// emitted vector is textually the vector the generated column already
/// produced. The projection move is provably score-free; honouring
/// non-uniform weights is a separate, measured retrieval-quality change.
pub const WEIGHT_UNIFORM: f32 = 1.0;

/// Which lexical configuration produces and ranks a row's vector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LanguagePolicy {
    /// The row carries its own `regconfig` in `column` on the projection,
    /// FK-stamped against `proxima_core.lexical_languages` so
    /// `lexical_language_forget` enumerates nothing. The value is the
    /// writing caller's language.
    PerRow { column: &'static str },
    /// One configuration for the whole surface, whatever the caller asked
    /// for (the code flavor pins `english`: code search must not follow the
    /// deployment's prose configuration).
    Pinned(&'static str),
    /// The union of several pinned configurations, concatenated in the
    /// declared order — `lexical_tsv(c1, txt) || lexical_tsv(c2, txt)`.
    ///
    /// Vocabulary extension rather than a generator feature (map §2.0.1):
    /// `proxima_code.commit_search_tsv` indexes commit prose under
    /// `simple` *and* `english` so non-English words survive English
    /// stop-word rules, and no single-configuration arm can say that. The
    /// FIRST configuration is the one stamped on the row.
    PinnedUnion(&'static [&'static str]),
}

impl LanguagePolicy {
    /// The configuration stamped on the projection row when the policy
    /// fixes one, i.e. everything except [`Self::PerRow`].
    #[must_use]
    pub const fn pinned_config(&self) -> Option<&'static str> {
        match self {
            Self::PerRow { .. } => None,
            Self::Pinned(config) => Some(*config),
            Self::PinnedUnion(configs) => {
                if configs.is_empty() {
                    None
                } else {
                    Some(configs[0])
                }
            }
        }
    }

    /// Every configuration the emitted vector is built from, in order.
    /// Empty for [`Self::PerRow`], whose configuration is a row value.
    #[must_use]
    pub fn configs(&self) -> Vec<&'static str> {
        match self {
            Self::PerRow { .. } => Vec::new(),
            Self::Pinned(config) => vec![*config],
            Self::PinnedUnion(configs) => configs.to_vec(),
        }
    }
}

/// Substring / `LIKE` search, opt-in per plan §4.2.3.
///
/// Default is [`SubstringArm::Off`]: the arm adds zero rows for natural
/// multi-word queries and is the only arm for all-stopword and
/// partial-word classes, so a flavor that needs those says so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SubstringArm {
    Off,
    /// The shipped shape: a memory-first nested loop, deliberately NOT
    /// routed at the composite index (probe-measured regression).
    MemoryFirstNestedLoop,
}

/// A score band — the cross-flavor merge contract. Raw `ts_rank` is not
/// comparable across corpora; a band is.
///
/// The three constants below are today's inline literals in
/// `storage-pg/src/verbs/query/search.rs`, named. Naming them does not move
/// a single score.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Band {
    pub name: &'static str,
    pub floor: f32,
    pub ceiling: f32,
}

/// Exact `tsquery` match: `0.5 + LEAST(ts_rank_cd(..), 1.0) * 0.5`.
pub const BAND_EXACT: Band = Band {
    name: "exact",
    floor: 0.50,
    ceiling: 1.00,
};
/// Rescue `any_tsq` arm: `0.25 + LEAST(ts_rank(..) * 100, 1.0) * 0.2`.
pub const BAND_RESCUE: Band = Band {
    name: "rescue",
    floor: 0.25,
    ceiling: 0.45,
};
/// Substring arm: the flat `0.25::real`.
pub const BAND_SUBSTRING: Band = Band {
    name: "substring",
    floor: 0.25,
    ceiling: 0.25,
};

/// Whether a schema is a search surface — and, when it is not, *why*.
///
/// The map calls this `Searchable::{Never, Projected}`; the plan spells the
/// absent arm `SearchProjection::None`. Same value either way: a declared
/// non-surface, distinguishable from a missing declaration. The registry
/// refuses to emit a projection row for [`Self::None`] and refuses to
/// register a schema that declares neither.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SearchProjectionDecl {
    /// No projection row is ever written for this schema. STRUCTURAL, not a
    /// filter.
    None { why: &'static str },
    Projected {
        fields: &'static [WeightedField],
        /// Sidecar column the projection's `tag` array is copied from.
        tag_column: Option<&'static str>,
        language: LanguagePolicy,
        bands: &'static [Band],
        substring: SubstringArm,
    },
}

impl SearchProjectionDecl {
    #[must_use]
    pub const fn is_projected(&self) -> bool {
        matches!(self, Self::Projected { .. })
    }

    #[must_use]
    pub const fn tag_column(&self) -> Option<&'static str> {
        match self {
            Self::None { .. } => None,
            Self::Projected { tag_column, .. } => *tag_column,
        }
    }

    #[must_use]
    pub const fn fields(&self) -> &'static [WeightedField] {
        match self {
            Self::None { .. } => &[],
            Self::Projected { fields, .. } => fields,
        }
    }

    #[must_use]
    pub const fn language(&self) -> Option<LanguagePolicy> {
        match self {
            Self::None { .. } => None,
            Self::Projected { language, .. } => Some(*language),
        }
    }

    /// The distinct declared weight levels, **ascending**.
    ///
    /// Ascending because [`TSVECTOR_WEIGHT_CLASSES`] is ascending: the
    /// lowest declared level becomes `D`, which is what an unweighted
    /// vector already is.
    ///
    /// # Errors
    ///
    /// Returns the number of distinct levels when it exceeds
    /// [`TSVECTOR_WEIGHT_CLASSES`]`.len()`. `PostgreSQL` stores a two-bit
    /// weight per lexeme position and offers exactly four classes
    /// (*12.3.1 Parsing Documents*), so a fifth level has nowhere to go and
    /// silently collapsing two levels into one class would make `ts_rank`'s
    /// weight array describe a document it is not scoring.
    pub fn weight_levels(&self) -> Result<Vec<f32>, usize> {
        let mut levels = self
            .fields()
            .iter()
            .map(|field| field.weight)
            .collect::<Vec<_>>();
        levels.sort_by(f32::total_cmp);
        levels.dedup_by(|a, b| a.total_cmp(b).is_eq());
        if levels.len() > TSVECTOR_WEIGHT_CLASSES.len() {
            return Err(levels.len());
        }
        Ok(levels)
    }

    /// `setweight` class for one declared weight, or `None` when the unit
    /// declares more levels than `PostgreSQL` has classes.
    #[must_use]
    pub fn weight_class(&self, weight: f32) -> Option<&'static str> {
        let levels = self.weight_levels().ok()?;
        let index = levels
            .iter()
            .position(|level| level.total_cmp(&weight).is_eq())?;
        TSVECTOR_WEIGHT_CLASSES.get(index).copied()
    }

    /// `ts_rank`'s `{D, C, B, A}` weight array, or `None` when the unit is
    /// uniform.
    ///
    /// Uniform is `None` rather than `[w, .., w]` on purpose: one level
    /// means every lexeme is class `D`, and `PostgreSQL`'s own default array
    /// is then the array that reproduces today's unweighted score exactly.
    /// Passing a rewritten array would move every score for no declared
    /// reason.
    #[must_use]
    pub fn rank_weight_array(&self) -> Option<[f32; 4]> {
        let levels = self.weight_levels().ok()?;
        if levels.len() < 2 {
            return None;
        }
        let mut weights = DEFAULT_RANK_WEIGHTS;
        for (index, level) in levels.iter().enumerate() {
            weights[index] = *level;
        }
        Some(weights)
    }
}

// ── Embedding recipe (plan §4.5.1) ──────────────────────────────────────

/// A named abstract model target, bound to a concrete model by deployment
/// config. This is what lets a flavor say "embed this field with the code
/// model" without hardcoding a model id into the declaration.
///
/// v0.0.8 binds exactly one slot, [`SLOT_DEFAULT`]. Per-slot vector storage
/// needs its own column or table (dimensionality differs per model) and is
/// Phase 2+ machinery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EmbeddingSlot(pub &'static str);

impl EmbeddingSlot {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

/// The only slot bound in v0.0.8.
pub const SLOT_DEFAULT: EmbeddingSlot = EmbeddingSlot("default");

/// Where one unit's text comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EmbedText {
    /// A pre-computed column on the schema's sidecar table — the shipped
    /// idiom (`SearchProjection::embed_text_column`), read by
    /// `storage-pg/src/verbs/fact_embeddings/text.rs`.
    StoredColumn(&'static str),
    /// The memory row's rendered text.
    Render,
    /// Concatenate sidecar columns generator-side.
    Concat(&'static [&'static str]),
}

/// One `(text, slot)` pair: the output grain of a recipe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EmbedUnit {
    pub text: EmbedText,
    pub slot: EmbeddingSlot,
}

impl EmbedUnit {
    #[must_use]
    pub const fn stored(column: &'static str, slot: EmbeddingSlot) -> Self {
        Self {
            text: EmbedText::StoredColumn(column),
            slot,
        }
    }
}

/// Per schema: typed sidecar in → list of embed units out.
///
/// Supersedes the bare `FactPayload::EMBEDDABLE` / `embed_text_column`
/// pair: `Never` carries a reason instead of being a naked `false`, and the
/// unit list reserves the shape per-field target models need later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EmbeddingRecipe {
    /// Structurally non-embeddable, with the reason attached. Feeds
    /// `FlavorRegistryFrozen::non_embeddable_schema_ids`.
    Never {
        why: &'static str,
    },
    Units(&'static [EmbedUnit]),
}

/// One resolved embed unit: the concrete `(table, column, slot)` a drain
/// reads. `None` column means the text is not a stored column and the
/// caller must render it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ResolvedEmbedUnit {
    pub table: Option<&'static str>,
    pub column: Option<&'static str>,
    pub slot: EmbeddingSlot,
}

impl EmbeddingRecipe {
    #[must_use]
    pub const fn units(&self) -> &'static [EmbedUnit] {
        match self {
            Self::Never { .. } => &[],
            Self::Units(units) => units,
        }
    }

    #[must_use]
    pub const fn is_never(&self) -> bool {
        matches!(self, Self::Never { .. })
    }

    /// Typed sidecar in → embed units out, bound to the schema's sidecar
    /// table. This is the recipe *applied*: the pair a drain would read.
    #[must_use]
    pub fn resolve(&self, sidecar_table: Option<&'static str>) -> Vec<ResolvedEmbedUnit> {
        self.units()
            .iter()
            .map(|unit| match unit.text {
                EmbedText::StoredColumn(column) => ResolvedEmbedUnit {
                    table: sidecar_table,
                    column: Some(column),
                    slot: unit.slot,
                },
                EmbedText::Render | EmbedText::Concat(_) => ResolvedEmbedUnit {
                    table: sidecar_table,
                    column: None,
                    slot: unit.slot,
                },
            })
            .collect()
    }
}

// ── Enforcement, transfer ───────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DbConstraint {
    pub relation: &'static str,
    pub name: &'static str,
}

/// A trigger, for the cases where no declarative CHECK/FK is available.
/// `goal_head_t_only` is the only member today, and it exists *because*
/// removing the World owner deleted the CHECK constraints that used to back
/// goals-don't-transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DbTrigger {
    pub relation: &'static str,
    pub name: &'static str,
}

/// One place a declaration is actually enforced. A declaration that claims
/// a refusal without naming where it happens is a comment, not a contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Enforcement {
    Constraint(DbConstraint),
    Trigger(DbTrigger),
    /// The engine verb refuses before storage is reached.
    EngineRefusal {
        at: &'static str,
    },
    /// Storage refuses even when a caller bypasses the engine.
    StorageBackstop {
        at: &'static str,
    },
}

/// What a transfer does to one surface.
///
/// Six arms describe the shipped tree; the seventh
/// ([`TransferRule::FollowOrDedupe`]) is reserved vocabulary with exactly
/// one member (`content`) — the landing pad for the Phase-2 shared-blob
/// dedupe arm. `blob` deliberately stays [`TransferRule::FollowIfUnshared`]
/// and keeps refusing with `Conflict` until then (plan §4.5.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransferRule {
    /// `UPDATE` every `owner_columns` to the destination.
    Follow,
    /// Move only when no other live series under a different owner
    /// references the row; `Conflict` otherwise.
    FollowIfUnshared { shared_by: &'static [&'static str] },
    /// Move, or — when the destination already owns an identical row —
    /// remap the referring columns and GC the orphan.
    FollowOrDedupe {
        dedupe_key: &'static [&'static str],
        remaps: &'static [&'static str],
    },
    /// The row is deleted rather than moved (`ingest_keys`: a receipt
    /// proves admission by *this* owner and does not travel).
    Drop { why: &'static str },
    /// The row carries its own owner and stays with the SOURCE. The
    /// destination must never gain read access to it. This is what
    /// `pg_sidecar!(owner_pinned: true)` means, lifted out of storage so
    /// the core registry can see it.
    RetainAtSource { why: &'static str },
    /// The entity does not transfer at all; the attempt is refused.
    /// `enforced_by` must be non-empty — the registry rejects a
    /// `NotTransferable` that names no enforcement site.
    NotTransferable {
        why: &'static str,
        enforced_by: &'static [Enforcement],
    },
    /// Reached through its key's owner; there is nothing to move. EMPTY
    /// `owner_columns` is the matching claim.
    StaysOnKey,
}

impl TransferRule {
    /// Whether transfer leaves the row with the source owner. Both
    /// [`Self::RetainAtSource`] and [`Self::NotTransferable`] do, for
    /// different reasons.
    #[must_use]
    pub const fn retains_at_source(&self) -> bool {
        matches!(self, Self::RetainAtSource { .. })
    }

    #[must_use]
    pub const fn is_not_transferable(&self) -> bool {
        matches!(self, Self::NotTransferable { .. })
    }
}

/// How a node's provenance is reachable. Declared rather than implied, so
/// `core_think`'s edge walk does not have to guess (checkpoint 9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Provenance {
    /// Writes `origins[]` pins; the lineage walk traverses them.
    OriginEdges,
    /// Grounds only through payload columns; the lineage walk will NOT
    /// reach the subjects.
    PayloadOnly {
        subject_columns: &'static [&'static str],
    },
    /// Derives from nothing — a Fact observes.
    None,
}

// ── Surfaces ────────────────────────────────────────────────────────────

/// What a surface's rows are keyed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyShape {
    MemoryT,
    GoalT,
    BlobId,
    OwnerId,
    Custom(&'static [&'static str]),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EraseRule {
    /// Deleted through the selection set for its key.
    ByKey,
    /// Deleted by the surface's own `owner_id`.
    ByOwner,
    /// A constraint removes it; erase emits no statement.
    Cascade {
        via: DbConstraint,
    },
    Never {
        why: &'static str,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExportRule {
    /// Whole rows.
    Rows,
    /// An explicit field allowlist — a storage-only column added later must
    /// not leak into a supported serialized contract merely because the
    /// table changed.
    Allowlist(&'static [&'static str]),
    /// Deliberately absent from the bundle, with the reason stated.
    Excluded { why: &'static str },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ForgetRule {
    /// Dumped into the cold record, then deleted from the hot table.
    DumpThenDelete,
    /// Deleted with the memory, not preserved.
    DeleteWithMemory,
    /// Untouched by forget, with the reason stated.
    Keep { why: &'static str },
}

/// One physical relation a flavor (or the kernel) owns, with every rule the
/// compliance and transfer lanes need. No field is optional-by-omission:
/// adding a table without saying what forget does is a compile error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Surface {
    pub table: &'static str,
    pub key: KeyShape,
    /// Columns carrying an owner. EMPTY IS A CLAIM, not an omission: it
    /// asserts the row is reached through its key's owner.
    pub owner_columns: &'static [&'static str],
    pub transfer: TransferRule,
    pub erase: EraseRule,
    pub export: ExportRule,
    pub forget: ForgetRule,
    /// The `regconfig` column this surface stamps, FK-stamped against
    /// `proxima_core.lexical_languages`. `lexical_language_forget` refuses
    /// through those FKs and so enumerates nothing; the migration guardrail's
    /// expected FK set is a projection of exactly this field, which is what
    /// retires its hardcoded five-table `IN (...)`.
    pub lexical_language_column: Option<&'static str>,
    /// Audit counter this surface contributes to. `None` is a declared
    /// non-count.
    pub counter: Option<&'static str>,
    /// A constraint that already proves completeness. Present ⇒ no list is
    /// generated anywhere; the constraint is the proof.
    pub completeness: Option<DbConstraint>,
}

// ── Projection ──────────────────────────────────────────────────────────

/// How this flavor's score bands compare to flavor #0's.
///
/// RESERVED AND UNCONSUMED in v0.0.8, exactly as `EraseRule` / `ForgetRule`
/// / `ExportRule` were reserved in Phase 1. The scatter-gather deployment
/// layer (plan §3) merges per-flavor result sets and cannot do it on raw
/// `ts_rank`, which is not comparable across corpora. What it needs is a
/// per-*flavor* statement, not a per-row column — see the map's R3/R7.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BandComparability {
    /// Every projected surface scores inside flavor #0's `BAND_*` windows,
    /// so a merge may compare scores directly.
    CoreBands,
    /// At least one arm scores outside them; `why` names which.
    Divergent { why: &'static str },
}

/// The per-flavor lexical projection table.
///
/// One table per flavor, in the flavor's own schema, holding one row per
/// searchable memory discriminated by `schema_id` — so a flavor's whole
/// lexical surface is one composite-index scan instead of one scan per
/// sidecar, and provisioning or tearing a flavor down stays a single
/// schema-level operation.
///
/// It is deliberately NOT seeded into `proxima_core.flavor_surface`: that
/// table's domain is "tables a `memory` row may stamp in `sidecar_tables`",
/// and a projection row is derived, never stamped. Registering it there
/// would widen `assert_sidecar_stamp_declared` for nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProjectionSpec {
    /// Qualified table name, e.g. `proxima_core.projection`.
    pub table: &'static str,
    /// The composite `gin (owner_id, search_tsv)` index name.
    pub index: &'static str,
    /// RESERVED, UNCONSUMED in v0.0.8. Rows a shard fetches before the
    /// merge trims to the caller's limit. Reserving it costs a field;
    /// not reserving it means the deployment layer's first change reopens
    /// the contract.
    pub overfetch_k: u32,
    /// RESERVED, UNCONSUMED in v0.0.8. See [`BandComparability`].
    pub band_comparability: BandComparability,
}

impl ProjectionSpec {
    /// The [`Surface`] the projection table is, so erase / transfer /
    /// export / forget walk it with no special case.
    ///
    /// `erase` is [`EraseRule::Cascade`] through the memory FK, which is
    /// what makes owner erase, memory forget and flavor repo erase all
    /// reach the projection with no new list, no new counter and no code
    /// that knows the word "projection". That is the inverse-at-scope
    /// property: compliance never learns about this table.
    #[must_use]
    pub const fn surface(&self) -> Surface {
        Surface {
            table: self.table,
            key: KeyShape::MemoryT,
            owner_columns: &["owner_id"],
            transfer: TransferRule::Follow,
            erase: EraseRule::Cascade {
                via: DbConstraint {
                    relation: self.table,
                    name: PROJECTION_MEMORY_FK,
                },
            },
            export: ExportRule::Excluded {
                why: "a derived lexical index; every byte in it is a function of the \
                      sidecar rows the bundle already carries",
            },
            forget: ForgetRule::DeleteWithMemory,
            lexical_language_column: Some("lexical_language"),
            counter: None,
            completeness: Some(DbConstraint {
                relation: self.table,
                name: PROJECTION_MEMORY_FK,
            }),
        }
    }
}

/// The name `PostgreSQL` mints for the projection's memory FK.
///
/// Identical for every flavor because the table is always named
/// `projection` and the column is always `memory_id`, and constraint names
/// are per-schema. That is the slimness rule in §2.0.1 made checkable: two
/// flavors' emitted DDL differ only in the schema name and the index name.
pub const PROJECTION_MEMORY_FK: &str = "projection_memory_id_fkey";

/// The bare relation name every flavor's projection table carries.
pub const PROJECTION_TABLE_NAME: &str = "projection";

/// Whether a flavor has a projection table — and, when it does not, why.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProjectionDecl {
    /// Declared absence: no schema of this flavor is a search surface.
    None {
        why: &'static str,
    },
    Table(ProjectionSpec),
}

impl ProjectionDecl {
    #[must_use]
    pub const fn spec(&self) -> Option<&ProjectionSpec> {
        match self {
            Self::None { .. } => None,
            Self::Table(spec) => Some(spec),
        }
    }

    #[must_use]
    pub const fn table(&self) -> Option<&'static str> {
        match self {
            Self::None { .. } => None,
            Self::Table(spec) => Some(spec.table),
        }
    }
}

// ── MCP surface ─────────────────────────────────────────────────────────

/// One registered MCP tool, as the contract sees it: a wire name and the
/// action leaves the scope gate authorizes at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ToolContract {
    pub wire_name: &'static str,
    /// Empty ⇒ flat tool, scope key is the wire name.
    /// Non-empty ⇒ scope keys are `"<wire_name>:<action>"`.
    pub actions: &'static [&'static str],
    pub idempotent: bool,
}

/// One `proxima://` resource.
///
/// A palette assembled from tools alone denies every resource read, because
/// `read_resource` funnels through the same flat-string gate with the
/// resource's scope key standing in for a tool name. So resources are
/// first-class contract entries — and they carry a **typed** `read_only`,
/// which replaces the `tool.starts_with("resource:")` string test the
/// owner-role gate performs today.
///
/// Only flavor #0 may declare resources; the registry rejects a non-empty
/// list from any other ordinal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ResourceContract {
    /// RFC 6570 template, e.g. `proxima://memory/{id}{?expand_neighbors}`.
    pub uri_template: &'static str,
    /// First path segment after `proxima://` — what dispatch matches on.
    pub path: &'static str,
    /// Advertised resource name, e.g. `proxima-memory`.
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    /// Palette entry, e.g. `resource:memory`.
    pub scope_key: &'static str,
    pub is_template: bool,
    /// TYPED. An MCP resource is a read by definition; saying so here is
    /// what lets the gate stop inferring it from a string prefix.
    pub read_only: bool,
    /// Relations the handler reads, for the erase/export reach argument.
    pub reads: &'static [&'static str],
}

// ── The two levels ──────────────────────────────────────────────────────

/// Everything a flavor declares about one schema.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SchemaContract {
    pub id: SchemaRef,
    pub kind: PayloadKind,
    /// The schema's own sidecar table, when it has one.
    pub sidecar_table: Option<&'static str>,
    pub search: SearchProjectionDecl,
    pub embedding: EmbeddingRecipe,
    /// What a transfer does to rows written under this schema.
    pub transfer: TransferRule,
    pub provenance: Provenance,
    /// The physical surfaces this schema owns. Usually one (its sidecar);
    /// empty for a schema with no storage of its own.
    pub surfaces: &'static [Surface],
    pub natural_key_columns: &'static [&'static str],
    pub special_category: bool,
}

impl SchemaContract {
    #[must_use]
    pub fn schema_id(&self) -> SchemaId {
        self.id.schema_id()
    }

    #[must_use]
    pub fn schema_version(&self) -> SchemaVersion {
        self.id.schema_version()
    }

    /// The `(table, column, slot)` triples this schema's recipe resolves to.
    #[must_use]
    pub fn embed_units(&self) -> Vec<ResolvedEmbedUnit> {
        self.embedding.resolve(self.sidecar_table)
    }
}

/// A whole flavor's declaration. `ordinal` 0 is core, which is
/// non-removable and the only declarer of resources.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FlavorContract {
    pub flavor_id: &'static str,
    pub ordinal: u16,
    pub schemas: &'static [SchemaContract],
    /// Flavor-owned state that is not a memory sidecar (`proxima_code.repos`,
    /// `proxima_core.goal`). Same [`Surface`] rules; erase and transfer walk
    /// them identically.
    pub state_surfaces: &'static [Surface],
    /// Kernel relations this flavor's contract speaks for. Only flavor #0
    /// populates it: the kernel is not a flavor, but its surfaces still have
    /// to be declared somewhere the registry can walk.
    pub kernel_surfaces: &'static [Surface],
    pub tools: &'static [ToolContract],
    /// Non-empty only for ordinal 0.
    pub resources: &'static [ResourceContract],
}

/// The ordinal that marks core. Load-bearing at runtime in exactly two
/// places — unscoped search staying on core sidecars, and flavor #0 being
/// non-removable — and both are named rather than inferred from a table
/// name prefix.
pub const CORE_ORDINAL: u16 = 0;

impl FlavorContract {
    #[must_use]
    pub const fn is_core(&self) -> bool {
        self.ordinal == CORE_ORDINAL
    }

    /// Every surface this flavor declares, in declaration order: schema
    /// sidecars, then flavor state, then (for flavor #0) the kernel spine.
    pub fn all_surfaces(&self) -> impl Iterator<Item = &'static Surface> {
        self.schemas
            .iter()
            .flat_map(|schema| schema.surfaces.iter())
            .chain(self.state_surfaces.iter())
            .chain(self.kernel_surfaces.iter())
    }

    /// Sidecar tables whose rows stay with the source owner on transfer.
    /// This is the list `compliance_erase` / `compliance_export` / `forget`
    /// hold out of the Memory-keyed sweep.
    #[must_use]
    pub fn retain_at_source_tables(&self) -> Vec<String> {
        let mut tables = self
            .schemas
            .iter()
            .filter(|schema| schema.transfer.retains_at_source())
            .filter_map(|schema| schema.sidecar_table)
            .map(str::to_owned)
            .collect::<Vec<_>>();
        tables.sort();
        tables.dedup();
        tables
    }

    /// Surfaces that stamp a `lexical_language` column. The migration
    /// guardrail's expected FK list is exactly this set, so it stops being
    /// a hardcoded five-table `IN (...)`.
    #[must_use]
    pub fn lexical_stamped_tables(&self) -> Vec<&'static str> {
        let mut tables = self
            .all_surfaces()
            .filter(|surface| surface.lexical_language_column.is_some())
            .map(|surface| surface.table)
            .collect::<Vec<_>>();
        tables.sort_unstable();
        tables.dedup();
        tables
    }

    /// Whether `table` is one of this flavor's schema sidecars.
    ///
    /// Unscoped `core_search_memories` stays on flavor #0's sidecars. That
    /// used to be a `starts_with("proxima_core.")` test, which is a schema
    /// name standing in for an ordinal: it happens to be true today and
    /// says nothing a flavor could not accidentally satisfy.
    #[must_use]
    pub fn declares_sidecar_table(&self, table: &str) -> bool {
        self.schemas
            .iter()
            .filter_map(|schema| schema.sidecar_table)
            .any(|declared| declared == table)
    }

    /// The resource this flavor declares under `scope_key`, if any.
    ///
    /// The authorization gate calls this instead of testing the scope key
    /// for a `resource:` prefix: a resource read is a read because the
    /// contract says [`ResourceContract::read_only`], not because of how
    /// its palette entry is spelled.
    #[must_use]
    pub fn resource_by_scope_key(&self, scope_key: &str) -> Option<&'static ResourceContract> {
        self.resources
            .iter()
            .find(|resource| resource.scope_key == scope_key)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BAND_EXACT, BAND_RESCUE, BAND_SUBSTRING, EmbedText, EmbedUnit, EmbeddingRecipe,
        LanguagePolicy, SLOT_DEFAULT, SchemaRef, SearchProjectionDecl, SubstringArm,
        WEIGHT_UNIFORM, WeightedField,
    };

    #[test]
    fn a_schema_ref_renders_the_vn_idiom() {
        assert_eq!(
            SchemaRef::new("core", "agent-note", 1).render(),
            "core/agent-note-v1"
        );
    }

    #[test]
    fn a_never_recipe_resolves_to_no_units() {
        let recipe = EmbeddingRecipe::Never { why: "a receipt" };
        assert!(recipe.resolve(Some("proxima_core.upload_v1")).is_empty());
    }

    #[test]
    fn a_stored_column_recipe_resolves_to_the_pair_the_drain_reads() {
        let recipe = EmbeddingRecipe::Units(&[EmbedUnit {
            text: EmbedText::StoredColumn("embed_text"),
            slot: SLOT_DEFAULT,
        }]);
        let resolved = recipe.resolve(Some("proxima_core.agent_note_v1"));
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].table, Some("proxima_core.agent_note_v1"));
        assert_eq!(resolved[0].column, Some("embed_text"));
        assert_eq!(resolved[0].slot, SLOT_DEFAULT);
    }

    fn projected(weights: &'static [f32]) -> SearchProjectionDecl {
        // A leaked slice keeps the fixture `'static` without a macro.
        let fields: &'static [WeightedField] = Box::leak(
            weights
                .iter()
                .map(|weight| WeightedField {
                    column: "c",
                    kind: crate::SearchProjectionColumnKind::Text,
                    weight: *weight,
                })
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        );
        SearchProjectionDecl::Projected {
            fields,
            tag_column: None,
            language: LanguagePolicy::Pinned("simple"),
            bands: &[],
            substring: SubstringArm::Off,
        }
    }

    /// The identity claim, at the level the contract can state it: a
    /// uniform unit has ONE level, so every lexeme is class `D` — which is
    /// what an unweighted `to_tsvector` already produces — and no weight
    /// array is passed, so `ts_rank_cd` scores exactly what it scores today.
    #[test]
    fn uniform_weights_are_the_unweighted_case() {
        let decl = projected(&[WEIGHT_UNIFORM, WEIGHT_UNIFORM, WEIGHT_UNIFORM]);
        assert_eq!(decl.weight_levels(), Ok(vec![WEIGHT_UNIFORM]));
        assert_eq!(decl.weight_class(WEIGHT_UNIFORM), Some("D"));
        assert_eq!(decl.rank_weight_array(), None);
    }

    /// Ascending: the lowest declared level takes the class an unweighted
    /// lexeme already has, so adding a heavier field never silently
    /// re-scores the fields that were there before.
    #[test]
    fn distinct_levels_bucket_ascending_from_d() {
        let decl = projected(&[1.0, 0.25, 0.5]);
        assert_eq!(decl.weight_levels(), Ok(vec![0.25, 0.5, 1.0]));
        assert_eq!(decl.weight_class(0.25), Some("D"));
        assert_eq!(decl.weight_class(0.5), Some("C"));
        assert_eq!(decl.weight_class(1.0), Some("B"));
        assert_eq!(decl.rank_weight_array(), Some([0.25, 0.5, 1.0, 1.0]));
    }

    #[test]
    fn a_fifth_level_has_nowhere_to_go() {
        assert_eq!(
            projected(&[1.0, 2.0, 3.0, 4.0, 5.0]).weight_levels(),
            Err(5)
        );
        assert!(
            projected(&[1.0, 2.0, 3.0, 4.0, 5.0])
                .weight_class(3.0)
                .is_none()
        );
    }

    /// The bands are today's inline literals in `lexical_sidecar_sql`,
    /// named. If naming them moved a number the goldens would move with it.
    #[test]
    fn the_bands_are_the_shipped_score_windows() {
        assert_eq!((BAND_EXACT.floor, BAND_EXACT.ceiling), (0.50, 1.00));
        assert_eq!((BAND_RESCUE.floor, BAND_RESCUE.ceiling), (0.25, 0.45));
        assert_eq!((BAND_SUBSTRING.floor, BAND_SUBSTRING.ceiling), (0.25, 0.25));
    }
}
