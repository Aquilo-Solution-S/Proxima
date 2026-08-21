//! Flavor #0 — core, as a declaration.
//!
//! Core registers 15 schemas, 15 MCP tools and 10 `proxima://` resources
//! across five unlinked sites and, until now, had no `FlavorDescriptor` at
//! all: it was invisible to the very registry that is meant to be the single
//! source of truth. This module is that missing declaration.
//!
//! Two things make flavor #0 asymmetric, and both are named rather than
//! inferred:
//!
//! - **It is non-removable.** `proxima://schemas` and `proxima://tools`
//!   project the registry itself — a kernel axiom's read surface — and they
//!   live here because a second resource list would be a second place to
//!   forget one.
//! - **Its ordinal is load-bearing.** Unscoped search stays on core sidecars;
//!   `ordinal == 0` is what says so, replacing a `"proxima_core."` table-name
//!   prefix test.
//!
//! Nothing here renames anything: every core schema id already carries
//! `core/` and every core tool name already carries `core_`, so the existing
//! compile-time prefix assertions pass with zero renames.

use crate::SearchProjectionColumnKind as ColumnKind;
use crate::flavor::contract::{
    BAND_NAME_EXACT, BAND_NAME_RESCUE, BAND_NAME_SUBSTRING, Band, BandComparability, CORE_ORDINAL,
    CounterRule, DbConstraint, DbTrigger, EmbedUnit, EmbeddingRecipe, Enforcement, EraseRule,
    ExportRule, FlavorContract, ForgetRule, KeyShape, LanguagePolicy, ProjectionDecl,
    ProjectionSpec, Provenance, RankSource, ResourceContract, SLOT_DEFAULT, SchemaContract,
    SchemaRef, SearchProjectionDecl, SubstringArm, Surface, TS_RANK_NORMALIZATION_LOG_LENGTH_SCALE,
    TS_RANK_NORMALIZATION_NONE, TS_RANK_NORMALIZATION_SCALE, ToolContract, TransferRule,
    WEIGHT_UNIFORM, WeightedField,
};
use crate::protocol::resource as scope;
use crate::protocol::tool;
use crate::verbs::schema::PayloadKind;

/// The flavor id every core schema and tool is already prefixed with.
pub const FLAVOR_ID: &str = "core";

/// Exact `tsquery` match: `0.50 + LEAST(ts_rank_cd(.., 32), 1.0) * 0.50`.
///
/// Flavor #0's, not the contract module's. A flavor that writes
/// `proxima_core::flavor0::BAND_EXACT` into its own declaration is saying
/// "my exact band is core's", which is exactly the claim
/// [`BandComparability::CoreBands`] makes at flavor level. As
/// `flavor::contract` vocabulary these three masqueraded as universal while
/// three renderers spelled three different score functions inside them.
pub const BAND_EXACT: Band = Band {
    name: BAND_NAME_EXACT,
    floor: 0.50,
    ceiling: 1.00,
    normalization: TS_RANK_NORMALIZATION_SCALE,
};
/// Rescue `any_tsq` arm: `0.25 + LEAST(ts_rank(.., 1|32) * 100, 1.0) * 0.20`.
pub const BAND_RESCUE: Band = Band {
    name: BAND_NAME_RESCUE,
    floor: 0.25,
    ceiling: 0.45,
    normalization: TS_RANK_NORMALIZATION_LOG_LENGTH_SCALE,
};
/// Substring arm: the flat `0.25::real`. It admits, it does not rank —
/// hence zero width and no `ts_rank` call to normalize.
pub const BAND_SUBSTRING: Band = Band {
    name: BAND_NAME_SUBSTRING,
    floor: 0.25,
    ceiling: 0.25,
    normalization: TS_RANK_NORMALIZATION_NONE,
};

const BANDS: &[Band] = &[BAND_EXACT, BAND_RESCUE, BAND_SUBSTRING];

/// Core's lexical projection: four sidecars, one table, one composite GIN.
///
/// It lives in `proxima_core` because a flavor's projection lives in the
/// flavor's own schema, and it is deliberately absent from
/// `proxima_core.flavor_surface` — a projection row is derived from a
/// sidecar row, never stamped by a memory.
const CORE_PROJECTION: ProjectionSpec = ProjectionSpec {
    table: "proxima_core.projection",
    index: "core_projection_owner_tsv_gin",
    // The candidate budget for ONE statement over this table. It was
    // `SIDECAR_OVERFETCH_CAP` in `storage-pg`, applied per projected
    // schema, so core's four statements could hand the merge 4 000 rows;
    // the collapse makes it what the number always said it was.
    overfetch_k: 1_000,
    band_comparability: BandComparability::CoreBands,
    // Core IS the projection-ranking shape: `core_search_memories` ranks
    // `proxima_core.projection` alone and joins a sidecar only to hydrate
    // the snippet of a row that already made the page.
    rank_source: RankSource::Projection,
};

/// Goals do not transfer, and the refusal is real in three places. There is
/// no CHECK constraint to name: removing the World owner deleted
/// `goal_not_world_owner_chk` and `goal_head_not_world_owner_chk` with it,
/// so the DDL leg is a trigger that freezes `goal_head.owner_id`.
const GOAL_NOT_TRANSFERABLE: TransferRule = TransferRule::NotTransferable {
    why: "a goal series cannot change owner: an armed goal's wake_config, \
          hard-context memory set and tool grants are the receiving owner's \
          authority, not the goal's",
    enforced_by: &[
        Enforcement::EngineRefusal {
            at: "core/src/engine/access_admin.rs::Engine::transfer_to_owner",
        },
        Enforcement::StorageBackstop {
            at: "storage-pg/src/access/owner_columns.rs::transfer_to_owner",
        },
        Enforcement::Trigger(DbTrigger {
            relation: "proxima_core.goal_head",
            name: "goal_head_t_only",
        }),
    ],
};

/// A memory sidecar keyed on `memory.t`: nothing to re-home, because the
/// Memory carries the owner. EMPTY `owner_columns` is the claim.
const fn memory_sidecar(
    table: &'static str,
    lexical_language_column: Option<&'static str>,
    completeness: Option<DbConstraint>,
) -> Surface {
    Surface {
        table,
        key: KeyShape::MemoryT { column: "t" },
        owner_columns: &[],
        transfer: TransferRule::StaysOnKey,
        erase: EraseRule::ByKey,
        export: ExportRule::Rows,
        forget: ForgetRule::DumpThenDelete,
        lexical_language_column,
        counter: CounterRule::Counted("sidecar_rows"),
        completeness,
    }
}

const fn t_fkey(relation: &'static str, name: &'static str) -> DbConstraint {
    DbConstraint { relation, name }
}

// ── Schemas ─────────────────────────────────────────────────────────────

const WRITE_ACT_V1: SchemaContract = SchemaContract {
    id: SchemaRef::new(FLAVOR_ID, "write-act", 1),
    kind: PayloadKind::Fact,
    sidecar_table: Some("proxima_core.write_act_v1"),
    search: SearchProjectionDecl::None {
        why: "an episode token is two columns — t and episode_id — and neither is text",
    },
    embedding: EmbeddingRecipe::Never {
        why: "an episode token has no content; its render is a template",
    },
    transfer: TransferRule::StaysOnKey,
    provenance: Provenance::None,
    surfaces: &[memory_sidecar(
        "proxima_core.write_act_v1",
        None,
        Some(t_fkey("proxima_core.write_act_v1", "write_act_v1_t_fkey")),
    )],
    natural_key_columns: &[],
};

const AGENT_NOTE_V1: SchemaContract = SchemaContract {
    id: SchemaRef::new(FLAVOR_ID, "agent-note", 1),
    kind: PayloadKind::Fact,
    sidecar_table: Some("proxima_core.agent_note_v1"),
    search: SearchProjectionDecl::Projected {
        fields: &[
            WeightedField {
                column: "title",
                kind: ColumnKind::Text,
                weight: WEIGHT_UNIFORM,
            },
            WeightedField {
                column: "body",
                kind: ColumnKind::Text,
                weight: WEIGHT_UNIFORM,
            },
            WeightedField {
                column: "tags",
                kind: ColumnKind::TextArray,
                weight: WEIGHT_UNIFORM,
            },
        ],
        tag_column: Some("tags"),
        language: LanguagePolicy::PerRow {
            column: "lexical_language",
        },
        bands: BANDS,
        substring: SubstringArm::MemoryFirstNestedLoop,
    },
    embedding: EmbeddingRecipe::Units(&[EmbedUnit::stored("embed_text", SLOT_DEFAULT)]),
    transfer: TransferRule::StaysOnKey,
    provenance: Provenance::None,
    surfaces: &[memory_sidecar(
        "proxima_core.agent_note_v1",
        None,
        Some(t_fkey("proxima_core.agent_note_v1", "agent_note_v1_t_fkey")),
    )],
    natural_key_columns: &["note_id"],
};

/// Utterances ARE searchable in this tree, with their own band set. The
/// acceptance case the plan calls "utterances-don't-search" is about an
/// out-of-tree `ChatTurnV1`; what is testable here is that
/// [`SearchProjectionDecl::None`] is a *value* — see `WRITE_ACT_V1` and
/// `MCP_CALL_LOGGED_V1`, which declare it.
const UTTERANCE_V1: SchemaContract = SchemaContract {
    id: SchemaRef::new(FLAVOR_ID, "utterance", 1),
    kind: PayloadKind::Fact,
    sidecar_table: Some("proxima_core.utterance_v1"),
    search: SearchProjectionDecl::Projected {
        fields: &[WeightedField {
            column: "text",
            kind: ColumnKind::Text,
            weight: WEIGHT_UNIFORM,
        }],
        tag_column: None,
        language: LanguagePolicy::PerRow {
            column: "lexical_language",
        },
        bands: BANDS,
        substring: SubstringArm::MemoryFirstNestedLoop,
    },
    embedding: EmbeddingRecipe::Units(&[EmbedUnit::stored("embed_text", SLOT_DEFAULT)]),
    transfer: TransferRule::StaysOnKey,
    provenance: Provenance::None,
    surfaces: &[memory_sidecar(
        "proxima_core.utterance_v1",
        None,
        Some(t_fkey("proxima_core.utterance_v1", "utterance_v1_t_fkey")),
    )],
    natural_key_columns: &[],
};

const UPLOAD_V1: SchemaContract = SchemaContract {
    id: SchemaRef::new(FLAVOR_ID, "upload", 1),
    kind: PayloadKind::Fact,
    sidecar_table: None,
    search: SearchProjectionDecl::None {
        why: "the Fact has no sidecar of its own; the artefact's typed description \
              is the cited object it names",
    },
    embedding: EmbeddingRecipe::Never {
        why: "the Fact is a receipt; the bytes are the blob. Tens of thousands of \
              renders off one template are mutual near-neighbours, which is a \
              retrieval problem before it is a bill",
    },
    transfer: TransferRule::StaysOnKey,
    provenance: Provenance::None,
    surfaces: &[],
    natural_key_columns: &[],
};

/// The one owner-pinned sidecar. Its `owner_id` is the owner that MADE the
/// call and is never rewritten, so the row answers "what did my agents do"
/// for the source owner after the Memory it describes has been transferred
/// away — and the destination never sees it.
const MCP_CALL_LOGGED_V1: SchemaContract = SchemaContract {
    id: SchemaRef::new(FLAVOR_ID, "mcp-call-logged", 1),
    kind: PayloadKind::Fact,
    sidecar_table: Some("proxima_core.mcp_call_logged_v1"),
    search: SearchProjectionDecl::None {
        why: "call telemetry is not retrievable content; the table carries no search_tsv",
    },
    embedding: EmbeddingRecipe::Never {
        why: "call telemetry is not retrievable content",
    },
    transfer: TransferRule::RetainAtSource {
        why: "the row is about the actor, not the memory. Deleting or moving it on \
              transfer destroys history that both the acting owner's erase and its own \
              export are entitled to reach, and would disclose actor_upn to the destination",
    },
    provenance: Provenance::None,
    surfaces: &[Surface {
        table: "proxima_core.mcp_call_logged_v1",
        key: KeyShape::MemoryT { column: "t" },
        owner_columns: &["owner_id"],
        transfer: TransferRule::RetainAtSource {
            why: "see the schema declaration",
        },
        erase: EraseRule::ByOwner,
        export: ExportRule::Rows,
        forget: ForgetRule::Keep {
            why: "forgetting a Memory must not dump or destroy the acting owner's \
                  audit trail; the row holds no foreign key into memory, so it can \
                  simply stay in the hot table",
        },
        lexical_language_column: None,
        counter: CounterRule::Counted("mcp_call_rows"),
        // Deliberately NO FK to memory: the row must outlive the Memory.
        // Completeness rests on owner_id -> owners instead.
        completeness: Some(DbConstraint {
            relation: "proxima_core.mcp_call_logged_v1",
            name: "mcp_call_logged_v1_owner_id_fkey",
        }),
    }],
    natural_key_columns: &[],
};

const AGENT_DERIVATION_SEARCH: SearchProjectionDecl = SearchProjectionDecl::Projected {
    fields: &[
        WeightedField {
            column: "title",
            kind: ColumnKind::Text,
            weight: WEIGHT_UNIFORM,
        },
        WeightedField {
            column: "body",
            kind: ColumnKind::Text,
            weight: WEIGHT_UNIFORM,
        },
        WeightedField {
            column: "tags",
            kind: ColumnKind::TextArray,
            weight: WEIGHT_UNIFORM,
        },
    ],
    tag_column: Some("tags"),
    language: LanguagePolicy::PerRow {
        column: "lexical_language",
    },
    bands: BANDS,
    substring: SubstringArm::MemoryFirstNestedLoop,
};

const AGENT_DERIVATION_SURFACE: Surface = memory_sidecar(
    "proxima_core.agent_derivation_v1",
    None,
    Some(t_fkey(
        "proxima_core.agent_derivation_v1",
        "agent_derivation_v1_t_fkey",
    )),
);

/// Registered twice — once as Abstraction, once as Perspective — because one
/// payload type serves both layers. The registry keys on
/// `(schema_id, version, kind)`, so the contract does too.
const AGENT_DERIVATION_V1_A: SchemaContract = SchemaContract {
    id: SchemaRef::new(FLAVOR_ID, "agent-derivation", 1),
    kind: PayloadKind::Abstraction,
    sidecar_table: Some("proxima_core.agent_derivation_v1"),
    search: AGENT_DERIVATION_SEARCH,
    embedding: EmbeddingRecipe::Units(&[EmbedUnit::stored("embed_text", SLOT_DEFAULT)]),
    transfer: TransferRule::StaysOnKey,
    provenance: Provenance::OriginEdges,
    surfaces: &[AGENT_DERIVATION_SURFACE],
    natural_key_columns: &[],
};

const AGENT_DERIVATION_V1_P: SchemaContract = SchemaContract {
    id: SchemaRef::new(FLAVOR_ID, "agent-derivation", 1),
    kind: PayloadKind::Perspective,
    sidecar_table: Some("proxima_core.agent_derivation_v1"),
    search: AGENT_DERIVATION_SEARCH,
    embedding: EmbeddingRecipe::Units(&[EmbedUnit::stored("embed_text", SLOT_DEFAULT)]),
    transfer: TransferRule::StaysOnKey,
    provenance: Provenance::OriginEdges,
    // The Abstraction registration owns the surface: one table, declared
    // once, so erase and forget cannot delete it twice.
    surfaces: &[],
    natural_key_columns: &[],
};

/// Checkpoint 9: an interpretation grounds through payload columns and
/// writes no `origin` rows, so the lineage walk will not reach its subjects.
/// That is now a declaration instead of a call-site choice.
const INTERPRETATION_V1: SchemaContract = SchemaContract {
    id: SchemaRef::new(FLAVOR_ID, "interpretation", 1),
    kind: PayloadKind::Perspective,
    sidecar_table: Some("proxima_core.interpretation_v1"),
    search: SearchProjectionDecl::Projected {
        fields: &[WeightedField {
            column: "claim",
            kind: ColumnKind::Text,
            weight: WEIGHT_UNIFORM,
        }],
        tag_column: None,
        language: LanguagePolicy::PerRow {
            column: "lexical_language",
        },
        bands: BANDS,
        substring: SubstringArm::MemoryFirstNestedLoop,
    },
    embedding: EmbeddingRecipe::Units(&[EmbedUnit::stored("embed_text", SLOT_DEFAULT)]),
    transfer: TransferRule::StaysOnKey,
    // The columns that carry SUBJECT IDS, which is one column and not two.
    // `subject_kinds` was declared here too and holds no id — it is the
    // positionally-aligned kind vector — so a walk that took the field at
    // its word would look for memory references in an enum array. Nothing
    // read it, so nothing found out.
    provenance: Provenance::PayloadOnly {
        subject_columns: &["subject_memory_ids"],
    },
    surfaces: &[memory_sidecar(
        "proxima_core.interpretation_v1",
        None,
        Some(t_fkey(
            "proxima_core.interpretation_v1",
            "interpretation_v1_t_fkey",
        )),
    )],
    natural_key_columns: &[],
};

const SIMPLE_TEXT_GOAL_V1: SchemaContract = SchemaContract {
    id: SchemaRef::new(FLAVOR_ID, "simple-text", 1),
    kind: PayloadKind::Goal,
    sidecar_table: None,
    search: SearchProjectionDecl::None {
        why: "search_memories returns empty for EntityKind::Goal; a goal is reached \
              through the goal read models, not the memory corpus",
    },
    embedding: EmbeddingRecipe::Never {
        why: "no embeddings row is ever keyed on a goal t",
    },
    transfer: GOAL_NOT_TRANSFERABLE,
    provenance: Provenance::None,
    surfaces: &[],
    natural_key_columns: &[],
};

const TASK_GOAL_V1: SchemaContract = SchemaContract {
    id: SchemaRef::new(FLAVOR_ID, "task", 1),
    kind: PayloadKind::Goal,
    sidecar_table: Some("proxima_core.task_goal_v1"),
    search: SearchProjectionDecl::None {
        why: "search_memories returns empty for EntityKind::Goal",
    },
    embedding: EmbeddingRecipe::Never {
        why: "no embeddings row is ever keyed on a goal t",
    },
    transfer: GOAL_NOT_TRANSFERABLE,
    provenance: Provenance::None,
    surfaces: &[Surface {
        table: "proxima_core.task_goal_v1",
        key: KeyShape::GoalT { column: "t" },
        owner_columns: &[],
        transfer: GOAL_NOT_TRANSFERABLE,
        erase: EraseRule::ByKey,
        export: ExportRule::Rows,
        forget: ForgetRule::Keep {
            why: "no goal-forget verb exists: Abandoned is an append, not a delete. \
                  Only owner erase ever removes a goal",
        },
        lexical_language_column: None,
        counter: CounterRule::Counted("sidecar_rows"),
        completeness: Some(t_fkey("proxima_core.task_goal_v1", "task_goal_v1_t_fkey")),
    }],
    natural_key_columns: &[],
};

/// Cited objects and citation mappings carry no sidecar of their own in
/// core: the whole mapping is `memory.blob_id`, and the artefact's typed
/// description lives on the blob row.
const fn citation_schema(
    name: &'static str,
    kind: PayloadKind,
    why: &'static str,
) -> SchemaContract {
    SchemaContract {
        id: SchemaRef::new(FLAVOR_ID, name, 1),
        kind,
        sidecar_table: None,
        search: SearchProjectionDecl::None { why },
        embedding: EmbeddingRecipe::Never { why },
        transfer: TransferRule::StaysOnKey,
        provenance: Provenance::None,
        surfaces: &[],
        natural_key_columns: &[],
    }
}

const UPLOADED_BLOB_V1: SchemaContract = citation_schema(
    "uploaded-blob",
    PayloadKind::CitedObject,
    "a cited object is not a memory; it carries no embed or search surface",
);
const UPLOADED_BLOB_WHOLE_V1: SchemaContract = citation_schema(
    "uploaded-blob-whole",
    PayloadKind::CitationMapping,
    "a pure link: the whole mapping is memory.blob_id",
);
const UPLOADED_BLOB_PAGE_SPAN_V1: SchemaContract = citation_schema(
    "uploaded-blob-page-span",
    PayloadKind::CitationMapping,
    "a pure link: the whole mapping is memory.blob_id",
);
const MCP_CALL_IO_V1: SchemaContract = citation_schema(
    "mcp-call-io",
    PayloadKind::CitedObject,
    "call payload bytes are evidence, not retrievable content",
);
const MCP_CALL_IO_CITATION_V1: SchemaContract = citation_schema(
    "mcp-call-io-citation",
    PayloadKind::CitationMapping,
    "a pure link: the whole mapping is memory.blob_id",
);

// ── Flavor-#0 state surfaces (not memory sidecars) ──────────────────────

const STATE_SURFACES: &[Surface] = &[
    Surface {
        table: "proxima_core.goal",
        key: KeyShape::GoalT { column: "t" },
        owner_columns: &["owner_id"],
        transfer: GOAL_NOT_TRANSFERABLE,
        erase: EraseRule::ByOwner,
        export: ExportRule::Rows,
        forget: ForgetRule::Keep {
            why: "no goal-forget verb exists; Abandoned is an append, not a delete",
        },
        lexical_language_column: None,
        counter: CounterRule::Counted("goals"),
        // WAS goal_not_world_owner_chk. That CHECK went with the World
        // owner; the DDL backstop is now the goal_head_t_only trigger, which
        // TransferRule::NotTransferable names directly.
        completeness: None,
    },
    Surface {
        table: "proxima_core.goal_head",
        key: KeyShape::Custom(&["handle"]),
        owner_columns: &["owner_id"],
        transfer: GOAL_NOT_TRANSFERABLE,
        erase: EraseRule::ByOwner,
        export: ExportRule::Excluded {
            why: "the head is derivable from the goal series",
        },
        forget: ForgetRule::Keep {
            why: "see proxima_core.goal",
        },
        lexical_language_column: None,
        counter: CounterRule::Uncounted {
            why: "the head is a POINTER into `goal`, not a row of its own: \
                  counting it would report every goal twice on one receipt",
        },
        completeness: None,
    },
    Surface {
        table: "proxima_core.wake_config",
        key: KeyShape::Custom(&["wake_id"]),
        owner_columns: &["owner_id"],
        transfer: GOAL_NOT_TRANSFERABLE,
        erase: EraseRule::ByOwner,
        export: ExportRule::Excluded {
            why: "DECLARED GAP — wake_config is erased (delete_wake_configs) and never \
                  exported, so a portability bundle omits the owner's own prompt text. \
                  Stated rather than left to be discovered",
        },
        forget: ForgetRule::Keep {
            why: "the goal lifecycle owns it",
        },
        lexical_language_column: None,
        counter: CounterRule::Counted("wake_configs"),
        completeness: None,
    },
];

// ── Kernel surfaces ─────────────────────────────────────────────────────
//
// The kernel is explicitly not a flavor, but its relations still need a
// declared transfer/erase/export/forget rule somewhere a registry walk can
// reach. Flavor #0 speaks for them because it is the non-removable flavor;
// no other flavor may declare a `proxima_core.*` surface.

const KERNEL_SURFACES: &[Surface] = &[
    Surface {
        table: "proxima_core.memory",
        key: KeyShape::MemoryT { column: "t" },
        owner_columns: &["owner_id"],
        transfer: TransferRule::Follow,
        erase: EraseRule::ByKey,
        export: ExportRule::Rows,
        forget: ForgetRule::DumpThenDelete,
        // The Memory row itself carries no text and no `lexical_language`:
        // ranking happens in the sidecars and in `sketch`.
        lexical_language_column: None,
        counter: CounterRule::Counted("memories"),
        completeness: None,
    },
    Surface {
        table: "proxima_core.memory_head",
        key: KeyShape::Custom(&["handle"]),
        owner_columns: &["owner_id"],
        transfer: TransferRule::Follow,
        // NOT a cascade, though it declared one until Phase C. The
        // constraint it named (`memory.handle -> memory_head.handle`) points
        // the OTHER way and is `NO ACTION`: it forces the series rows to be
        // deleted first and never removes the head. The head is rewound to
        // the surviving newest `t` and deleted when the series is empty —
        // an explicit statement, named by `owner_erase`'s leg table.
        erase: EraseRule::ByOwner,
        export: ExportRule::Excluded {
            why: "the head is derivable from the memory series",
        },
        // CORRECTED in Phase 4 (plan §4.12 R2). `DeleteWithMemory` was true
        // for the last revision of a series and false for every other one:
        // `sync_memory_head` REWINDS the head to the surviving newest `t`
        // and deletes only when the series empties. The vocabulary gets no
        // one-member `Rewind` arm for it — §4.6.1 forbids vocabulary with a
        // single speaker — because `Keep` already means "the generated
        // forget leg does not destroy this", and the statement that does
        // touch it is named by the erase side's bespoke list.
        forget: ForgetRule::Keep {
            why: "the head is rewound to the surviving newest t and deleted only when the \
                  series empties; the statement is sync_memory_head, which the erase's \
                  bespoke leg list already names",
        },
        lexical_language_column: None,
        counter: CounterRule::Uncounted {
            why: "the head is a POINTER into `memory`, and the erase deletes it \
                  through the same statement that takes the series it names",
        },
        completeness: None,
    },
    Surface {
        table: "proxima_core.cooled",
        key: KeyShape::MemoryT { column: "t" },
        owner_columns: &["owner_id"],
        transfer: TransferRule::Follow,
        erase: EraseRule::ByKey,
        export: ExportRule::Rows,
        forget: ForgetRule::Keep {
            why: "cooled IS the forgotten form; forget writes it rather than deleting it",
        },
        lexical_language_column: None,
        counter: CounterRule::Counted("memories"),
        completeness: None,
    },
    Surface {
        table: "proxima_core.sketch",
        // A memory t OR a goal t, in one column, with no discriminator —
        // which is exactly what `EntityT` names. It said `Custom` until
        // Phase 4, and `Custom` is the arm for a key this crate cannot
        // reason about at all.
        key: KeyShape::EntityT { column: "t" },
        owner_columns: &["owner_id"],
        transfer: TransferRule::Follow,
        erase: EraseRule::ByKey,
        export: ExportRule::Allowlist(&["t", "owner_id", "kind", "text"]),
        forget: ForgetRule::DeleteWithMemory,
        // The sketch has no reader: the search verb scans exactly the four
        // declared sidecars, and export strips the column. Its `search_tsv`,
        // its GIN and its stamp go with the projection move rather than
        // being carried to a new home nothing reads.
        lexical_language_column: None,
        // Recorded by erase today and dropped on the floor: there is no
        // `sketches` key in the final counts and no audit-log column.
        // Declaring it is what makes the gap addressable.
        counter: CounterRule::Counted("sketches"),
        // No FK: `t` is a Memory t OR a Goal t, and there is no constraint
        // that can span two home tables.
        completeness: None,
    },
    Surface {
        table: "proxima_core.embeddings",
        key: KeyShape::EntityT {
            column: "entity_id",
        },
        owner_columns: &["owner_id"],
        transfer: TransferRule::Follow,
        erase: EraseRule::ByKey,
        export: ExportRule::Excluded {
            why: "a vector is a derived index over text the bundle already carries",
        },
        forget: ForgetRule::DeleteWithMemory,
        lexical_language_column: None,
        counter: CounterRule::Counted("embeddings"),
        completeness: None,
    },
    Surface {
        table: "proxima_core.embedding_heads",
        key: KeyShape::EntityT {
            column: "entity_id",
        },
        owner_columns: &["owner_id"],
        transfer: TransferRule::Follow,
        erase: EraseRule::ByKey,
        export: ExportRule::Excluded {
            why: "derived index bookkeeping",
        },
        forget: ForgetRule::DeleteWithMemory,
        lexical_language_column: None,
        counter: CounterRule::Counted("embeddings"),
        completeness: None,
    },
    Surface {
        table: "proxima_core.embedding_jobs",
        key: KeyShape::EntityT {
            column: "entity_id",
        },
        owner_columns: &["owner_id"],
        transfer: TransferRule::Follow,
        erase: EraseRule::ByKey,
        export: ExportRule::Excluded {
            why: "queue state, not owner data",
        },
        forget: ForgetRule::DeleteWithMemory,
        lexical_language_column: None,
        counter: CounterRule::Counted("embedding_jobs"),
        completeness: None,
    },
    Surface {
        table: "proxima_core.ingest_keys",
        key: KeyShape::MemoryT { column: "t" },
        owner_columns: &["owner_id"],
        transfer: TransferRule::Drop {
            why: "a receipt proves admission by THIS owner. It does not travel, so a \
                  received series has structurally zero receipts and 'receipts' is not \
                  a complete audit trail for transferred content",
        },
        erase: EraseRule::ByKey,
        export: ExportRule::Rows,
        // CORRECTED in Phase 4. It declared `DeleteWithMemory` and the
        // shipped verb has never done that: `core_forget` cools a version
        // and leaves the receipt, and says so on the wire — its own tool
        // description reads "…delete hot row, announce.forget. ingest_keys
        // stay." The only statement that removes one is `erase_memory`'s,
        // a different verb with a different promise. Generating a forget
        // leg from the declaration as written would have destroyed an
        // admission receipt on every cool.
        forget: ForgetRule::Keep {
            why: "cooling a version does not un-admit it: the receipt proves this owner \
                  accepted this (source, ingest_key) and stays until the version is \
                  erased, which is what keeps a re-ingest of the same key idempotent \
                  across a cool",
        },
        lexical_language_column: None,
        counter: CounterRule::Counted("receipts"),
        completeness: None,
    },
    Surface {
        table: "proxima_core.announce",
        key: KeyShape::Custom(&["seq"]),
        owner_columns: &["owner_id"],
        // The log is append-only (`announce_append_only`), so no existing
        // row is ever re-homed: what the transfer does is APPEND two rows
        // in the same transaction, one under the prior owner's lane and one
        // under the destination's, so the source's projectors learn the
        // series left and the destination's pull consumers learn it
        // arrived. That is the transfers-announce-everywhere invariant, and
        // the rows already written stay where they were written.
        transfer: TransferRule::RetainAtSource {
            why: "an announce row records what happened to an owner's view, not to \
                  the memory; a transfer appends to both lanes rather than moving \
                  the log",
        },
        erase: EraseRule::ByKey,
        export: ExportRule::Excluded {
            why: "the pull log is projector state, exported as source_batches (empty by \
                  declaration) rather than as owner content",
        },
        forget: ForgetRule::Keep {
            why: "forget APPENDS an announce row; it never removes one",
        },
        lexical_language_column: None,
        counter: CounterRule::Counted("change_events"),
        completeness: None,
    },
    Surface {
        table: "proxima_core.blob",
        key: KeyShape::BlobId { column: "blob_id" },
        owner_columns: &["owner_id"],
        // The dedupe arm. A blob shared across owners used to refuse the
        // transfer outright; now the destination gets its own row over the
        // same object.
        //
        // CORRECTED in Phase 4. `remaps` named three columns and the
        // shipped SQL performed two. `blob_uploads.blob_id` is never
        // repointed at another blob row: the in-place case moves the upload
        // row's OWNER and leaves its `blob_id` alone, and the dedupe case
        // INSERTs a fresh mounted row already naming the destination's new
        // blob. Declaration and behaviour agreed only by coincidence, which
        // is the whole defect an unread declaration has. `remaps` is the
        // list of referring columns a generated UPDATE repoints, and there
        // are two.
        transfer: TransferRule::FollowOrDedupe {
            dedupe_key: &["owner_id", "schema_id", "content_hash"],
            remaps: &["memory.blob_id", "cooled.blob_id"],
        },
        erase: EraseRule::ByOwner,
        export: ExportRule::Allowlist(&["blob_id", "schema_id", "content_hash"]),
        forget: ForgetRule::Keep {
            why: "the citation outlives the cooling of the Fact that names it",
        },
        lexical_language_column: None,
        counter: CounterRule::Counted("blobs"),
        completeness: None,
    },
    Surface {
        table: "proxima_core.blob_uploads",
        key: KeyShape::Custom(&["upload_id"]),
        owner_columns: &["owner_id"],
        // Moves with the blob row it describes: the read path requires both
        // to name the same owner.
        transfer: TransferRule::Follow,
        erase: EraseRule::ByOwner,
        export: ExportRule::Excluded {
            why: "DECLARED GAP — upload coordinates and object-store bytes are erased \
                  but never exported",
        },
        forget: ForgetRule::Keep {
            why: "upload coordination outlives the Fact",
        },
        lexical_language_column: None,
        counter: CounterRule::Counted("blob_uploads"),
        completeness: None,
    },
    Surface {
        table: "proxima_core.content",
        key: KeyShape::Custom(&["content_id"]),
        owner_columns: &["owner_id"],
        // The arm's original member, and the shape `blob` was made to
        // copy: ensure a destination-owned row, remap the referring
        // columns, GC the orphan. The difference is that `content` has an
        // orphan and nothing else, while `blob` also has an object in S3
        // that two owners may now name.
        transfer: TransferRule::FollowOrDedupe {
            dedupe_key: &["owner_id", "schema_id", "content_hash"],
            remaps: &["memory.content_id", "cooled.content_id"],
        },
        // NOT a cascade, though it declared one until Phase C: the name it
        // gave (`gc_unreferenced_content`) is a Rust function, not a
        // constraint, so the claim was unfalsifiable by the catalog. The row
        // is reached through the selection set for its key and deleted only
        // when no surviving admission still names it — the refcount guard
        // `owner_erase`'s leg table records.
        erase: EraseRule::ByKey,
        export: ExportRule::Excluded {
            why: "DECLARED GAP — the A/P body is erased but never exported",
        },
        forget: ForgetRule::Keep {
            why: "many admissions may share one ContentId",
        },
        lexical_language_column: None,
        counter: CounterRule::Uncounted {
            why: "content is refcounted and shared: the rows an owner erase \
                  destroys here are the ones NO surviving owner still cites, \
                  which is a number about the deployment's deduplication and \
                  not about this owner",
        },
        completeness: None,
    },
    Surface {
        table: "proxima_core.source_cursors",
        key: KeyShape::Custom(&["owner_kind", "owner_id", "source"]),
        owner_columns: &["owner_id"],
        transfer: TransferRule::StaysOnKey,
        erase: EraseRule::ByOwner,
        export: ExportRule::Rows,
        forget: ForgetRule::Keep {
            why: "external ingest cursors are owner policy, not memory content",
        },
        lexical_language_column: None,
        counter: CounterRule::Counted("source_cursors"),
        completeness: None,
    },
    Surface {
        table: "proxima_core.delegated_authority_grants",
        key: KeyShape::Custom(&["delegation_id"]),
        owner_columns: &["owner_id"],
        transfer: TransferRule::StaysOnKey,
        erase: EraseRule::ByOwner,
        export: ExportRule::Allowlist(&[
            "subject_user_id",
            "owner_kind",
            "owner_id",
            "tool_name",
            "action_name",
            "read_ceiling",
            "write_ceiling",
            "expires_at",
            "auth_epoch",
            "issued_at",
            "revoked_at",
            "revoked_by_user_id",
        ]),
        forget: ForgetRule::Keep {
            why: "owner-level authority, never memory content",
        },
        lexical_language_column: None,
        counter: CounterRule::Counted("delegated_authority_grants"),
        completeness: None,
    },
    Surface {
        table: "proxima_core.cold_purge_pending",
        key: KeyShape::Custom(&["object_key"]),
        owner_columns: &["owner_id"],
        transfer: TransferRule::StaysOnKey,
        // It declared `ByOwner` until Phase C, and no erase ever deleted
        // from it — which was the only reason a generated statement had not
        // yet destroyed the queue the erase itself enqueues into. The debt
        // outlives the transaction that recorded it BY CONSTRUCTION: the row
        // is what survives a crash between commit and destruction.
        erase: EraseRule::Never {
            why: "the purge queue is the erase's own outbox; the drain deletes the row \
                  once the object is destroyed, and an erase that deleted it would lose \
                  the bytes it promised to reclaim",
        },
        export: ExportRule::Excluded {
            why: "erase bookkeeping",
        },
        forget: ForgetRule::Keep {
            why: "the purge queue outlives the transaction that enqueued it",
        },
        lexical_language_column: None,
        counter: CounterRule::Uncounted {
            why: "a work queue, not owned rows. Its entries are enqueued BY the \
                  erase and drained after it commits, so a count taken \
                  inside the transaction would report intent rather than \
                  destruction — `cold_object_purge_pending` on the outcome \
                  is where that number belongs",
        },
        completeness: None,
    },
    Surface {
        table: "proxima_core.owners",
        key: KeyShape::OwnerId,
        owner_columns: &["owner_id"],
        transfer: TransferRule::StaysOnKey,
        erase: EraseRule::Never {
            why: "erase never deletes the owners row: 17 FKs point at it and the \
                  destination owner's row is minted inside the transfer transaction",
        },
        export: ExportRule::Excluded {
            why: "the owner is the bundle's subject, not one of its rows",
        },
        forget: ForgetRule::Keep {
            why: "the star centre",
        },
        lexical_language_column: None,
        counter: CounterRule::Uncounted {
            why: "the erase never deletes the owners row, so there is nothing to \
                  count; `EraseRule::Never` already says why",
        },
        completeness: None,
    },
    // A membership names TWO owners and belongs to neither exclusively, so
    // `owner_columns` is EMPTY and means it: there is no column here whose
    // value makes the row somebody's to erase. That is the whole content of
    // the declaration, and stating it is what turns a recorded follow-up
    // into a position.
    Surface {
        table: "proxima_core.group_memberships",
        key: KeyShape::Custom(&["group_id", "member_user_id", "relation"]),
        owner_columns: &[],
        transfer: TransferRule::StaysOnKey,
        erase: EraseRule::Never {
            why: "a membership is a relation between two owners, not a row about \
                  one. Erasing a personal owner does not shrink the groups it \
                  belonged to: a host that must remove a departed user calls \
                  remove_group_member before erase, which is the same division of \
                  labour retention and legal holds already take. Erase READS this \
                  table — the abandonment precondition counts a group's remaining \
                  members — and writes to it never",
        },
        export: ExportRule::Excluded {
            why: "a membership is the group's row as much as the member's; \
                  exporting a personal owner's bundle would hand out the \
                  membership list of every group they belong to",
        },
        forget: ForgetRule::Keep {
            why: "an access relation, not a memory: no forget reaches it and no \
                  cold record carries it",
        },
        lexical_language_column: None,
        counter: CounterRule::Uncounted {
            why: "the erase never deletes a membership, so there is nothing to \
                  count; `EraseRule::Never` already says why",
        },
        completeness: None,
    },
];

// ── MCP surface ─────────────────────────────────────────────────────────

const TOOLS: &[ToolContract] = &[
    ToolContract {
        wire_name: tool::CORE_SEARCH_MEMORIES,
        actions: &[],
        idempotent: true,
    },
    ToolContract {
        wire_name: tool::CORE_RECALL,
        actions: &[],
        idempotent: true,
    },
    ToolContract {
        wire_name: tool::CORE_THINK,
        actions: &[],
        idempotent: true,
    },
    ToolContract {
        wire_name: tool::CORE_MEMORY_SPACES,
        actions: &[],
        idempotent: true,
    },
    ToolContract {
        wire_name: tool::CORE_REMEMBER,
        actions: &[],
        idempotent: false,
    },
    ToolContract {
        wire_name: tool::CORE_EPISODE_COMMIT,
        actions: &[],
        idempotent: false,
    },
    ToolContract {
        wire_name: tool::CORE_FORGET,
        actions: &[],
        idempotent: false,
    },
    ToolContract {
        wire_name: tool::CORE_RECORD_UTTERANCE,
        actions: &[],
        idempotent: false,
    },
    ToolContract {
        wire_name: tool::CORE_DERIVE,
        actions: &[],
        idempotent: true,
    },
    ToolContract {
        wire_name: tool::CORE_INTERPRET,
        actions: &[],
        idempotent: true,
    },
    ToolContract {
        wire_name: tool::CORE_GOAL,
        actions: &["set", "transition", "modify", "mark_achieved", "decompose"],
        idempotent: false,
    },
    ToolContract {
        wire_name: tool::CORE_FACT,
        actions: &["citation_of_fact", "facts_citing_object"],
        idempotent: true,
    },
    ToolContract {
        wire_name: tool::CORE_MEMBERSHIP,
        actions: &["add_member", "remove_member", "list_members"],
        idempotent: false,
    },
    ToolContract {
        wire_name: tool::CORE_TRANSFER,
        actions: &["transfer_to_owner"],
        idempotent: false,
    },
    ToolContract {
        wire_name: tool::CORE_UPLOAD,
        actions: &["prepare", "complete", "abort", "read_url"],
        idempotent: false,
    },
];

/// The ten `proxima://` resources. Nine handler modules; `goal_reads` backs
/// two. A palette built from tools alone denies every one of these reads,
/// which is why they are contract entries rather than a separate const.
const RESOURCES: &[ResourceContract] = &[
    ResourceContract {
        uri_template: "proxima://schemas{?kind}",
        path: "schemas",
        name: "proxima-schemas",
        title: "Proxima Schemas",
        description: "Registered core and flavor schema catalog, optionally filtered by payload kind.",
        scope_key: scope::SCHEMAS,
        is_template: false,
        read_only: true,
        reads: &[],
    },
    ResourceContract {
        uri_template: "proxima://tools",
        path: "tools",
        name: "proxima-tools",
        title: "Proxima Tools",
        description: "Registered substrate and flavor MCP tool catalog visible to the caller.",
        scope_key: scope::TOOLS,
        is_template: false,
        read_only: true,
        reads: &[],
    },
    ResourceContract {
        uri_template: "proxima://graph",
        path: "graph",
        name: "proxima-graph",
        title: "Proxima Graph",
        description: "Owner-scoped memory graph plus schema and tool catalogs.",
        scope_key: scope::GRAPH,
        is_template: false,
        read_only: true,
        reads: &["proxima_core.embedding_jobs"],
    },
    ResourceContract {
        uri_template: "proxima://memory/{id}{?expand_neighbors}",
        path: "memory",
        name: "proxima-memory",
        title: "Proxima Memory",
        description: "Owner-scoped memory by prefixed id (`F:`/`A:`/`P:`).",
        scope_key: scope::MEMORY,
        is_template: true,
        read_only: true,
        reads: &[
            "proxima_core.memory",
            "proxima_core.memory_head",
            "proxima_core.owners",
        ],
    },
    ResourceContract {
        uri_template: "proxima://memories{?ids}",
        path: "memories",
        name: "proxima-memories",
        title: "Proxima Memories",
        description: "Batch memory read by comma-separated prefixed ids (`F:`/`A:`/`P:`), \
                      at most 100 per call; unknown or invisible ids are reported as missing.",
        scope_key: scope::MEMORIES,
        is_template: true,
        read_only: true,
        reads: &[
            "proxima_core.memory",
            "proxima_core.memory_head",
            "proxima_core.owners",
        ],
    },
    ResourceContract {
        uri_template: "proxima://memory/{id}/lineage{?direction,depth,limit,cursor}",
        path: "memory",
        name: "proxima-memory-lineage",
        title: "Proxima Memory Lineage",
        description: "Owner-scoped origin lineage from a prefixed memory id, \
                      with keyset cursor pagination.",
        scope_key: scope::MEMORY_LINEAGE,
        is_template: true,
        read_only: true,
        reads: &["proxima_core.memory"],
    },
    ResourceContract {
        uri_template: "proxima://change-events{?since,limit}",
        path: "change-events",
        name: "proxima-change-events",
        title: "Proxima Change Events",
        description: "Owner-scoped change-event pull log.",
        scope_key: scope::CHANGE_EVENTS,
        is_template: true,
        read_only: true,
        reads: &[
            "proxima_core.announce",
            "proxima_core.memory",
            "proxima_core.blob",
        ],
    },
    ResourceContract {
        uri_template: "proxima://wake-candidates{?fact,limit}",
        path: "wake-candidates",
        name: "proxima-wake-candidates",
        title: "Proxima Wake Candidates",
        description: "Armed Active Goals admitted for wake planning by a trigger Fact.",
        scope_key: scope::WAKE_CANDIDATES,
        is_template: true,
        read_only: true,
        reads: &[
            "proxima_core.goal",
            "proxima_core.goal_head",
            "proxima_core.wake_config",
            "proxima_core.memory",
        ],
    },
    ResourceContract {
        uri_template: "proxima://goals{?state,limit,cursor}",
        path: "goals",
        name: "proxima-goals",
        title: "Proxima Goals",
        description: "Owner-scoped goal listing with state filter, keyset cursor, and wake-config read-back.",
        scope_key: scope::GOALS,
        is_template: true,
        read_only: true,
        reads: &[
            "proxima_core.goal",
            "proxima_core.goal_head",
            "proxima_core.owners",
            "proxima_core.wake_config",
        ],
    },
    ResourceContract {
        uri_template: "proxima://goal/{id}",
        path: "goal",
        name: "proxima-goal",
        title: "Proxima Goal",
        description: "Single-goal read by G:<uuid> reference, including stored wake configuration.",
        scope_key: scope::GOAL,
        is_template: true,
        read_only: true,
        reads: &[
            "proxima_core.goal",
            "proxima_core.goal_head",
            "proxima_core.owners",
            "proxima_core.wake_config",
        ],
    },
];

/// The kernel surfaces the owner erase reaches with a hand-written
/// statement instead of a generated one.
///
/// A bare table list, and deliberately so (plan §4.11). Each entry used to
/// carry the NAME of the storage-pg function that deletes it, kept honest
/// by a test that grepped that crate's own source for `fn <name>(`. The
/// operator ruled the names ceremony: both freeze checks work on table
/// names alone, the behaviour is already pinned by the differential
/// goldens, and a string-level proof that a string names a function
/// verifies documentation rather than behaviour. What the list must say is
/// WHICH tables the generator does not reach; who reaches them is a fact
/// about `proxima-storage-pg`'s code, and the place to read it is
/// `proxima-storage-pg`.
///
/// Sixteen entries, and every one of them earns the exemption by needing
/// something a generated `DELETE ... USING <selection set>` cannot express:
/// a refcount anti-join before a shared object may go (`blob`,
/// `blob_uploads`, `content`), a cold-purge row enqueued in the same
/// transaction as the delete (`cooled`, `wake_config`), a head table
/// resynchronised rather than emptied (`memory_head`, `goal_head`), an
/// ordering the embedding tables have to be taken in, or the spine itself
/// (`memory`, `goal`) which the selection sets were built FROM.
///
/// The list lives in the contract rather than in `proxima-storage-pg`
/// because freeze reads it: a surface that neither the generator reaches
/// nor this list claims is [`FlavorRegistryError::UndeletableSurface`], and
/// that refusal has to be available to a flavor the substrate crate has
/// never heard of.
///
/// [`FlavorRegistryError::UndeletableSurface`]: crate::flavor::FlavorRegistryError::UndeletableSurface
const BESPOKE_ERASE_LEGS: &[&str] = &[
    "proxima_core.announce",
    "proxima_core.blob",
    "proxima_core.blob_uploads",
    "proxima_core.content",
    "proxima_core.cooled",
    "proxima_core.delegated_authority_grants",
    "proxima_core.embedding_heads",
    "proxima_core.embedding_jobs",
    "proxima_core.embeddings",
    "proxima_core.goal",
    "proxima_core.goal_head",
    "proxima_core.memory",
    "proxima_core.memory_head",
    "proxima_core.sketch",
    "proxima_core.source_cursors",
    "proxima_core.wake_config",
];

/// The kernel surfaces a transfer moves with a hand-written statement
/// instead of a generated one.
///
/// Four entries, each earning the exemption for a reason a generated
/// `UPDATE <table> SET owner_id = $2 WHERE <key> = ANY($1)` cannot express:
///
/// - `memory_head` is a compare-and-set, not a move. Its statement carries
///   the head `t` the series was read at and its `rows_affected` is what
///   DECIDES whether the transfer happened; the two head-advanced races
///   hang off that answer.
/// - `blob_uploads` moves with the blob row it describes, one blob at a
///   time — the read path requires both to name the same owner — and in
///   the dedupe case it is not moved at all but re-minted as a mount.
/// - `blob` and `content` are the two `FollowOrDedupe` surfaces. Their
///   generated halves come off the declaration (`dedupe_key` finds the
///   destination-owned row, `remaps` repoints the referring columns); what
///   sits between those halves — a refcount probe, an OCI-style object
///   mount, an orphan GC — does not.
///
/// `memory_head` and `blob_uploads` are the two that would otherwise be
/// SILENTLY wrong rather than absent: both are keyed on a single column
/// (`handle`, `upload_id`) that is not an entity `t`, so a generated
/// `WHERE <column> = ANY($1)` would run cleanly and match nothing. That is
/// the class `TransferLeg::Unreachable` exists to refuse, and the list is
/// what tells freeze the statement is elsewhere.
const BESPOKE_TRANSFER_LEGS: &[&str] = &[
    "proxima_core.blob",
    "proxima_core.blob_uploads",
    "proxima_core.content",
    "proxima_core.memory_head",
];

/// Core's contract. Fifteen schema registrations over fourteen distinct
/// schema ids (`core/agent-derivation-v1` registers as both Abstraction and
/// Perspective), fifteen tools, ten resources.
pub const FLAVOR_0: FlavorContract = FlavorContract {
    flavor_id: FLAVOR_ID,
    ordinal: CORE_ORDINAL,
    schemas: &[
        WRITE_ACT_V1,
        AGENT_NOTE_V1,
        UTTERANCE_V1,
        UPLOAD_V1,
        MCP_CALL_LOGGED_V1,
        AGENT_DERIVATION_V1_A,
        AGENT_DERIVATION_V1_P,
        INTERPRETATION_V1,
        SIMPLE_TEXT_GOAL_V1,
        TASK_GOAL_V1,
        UPLOADED_BLOB_V1,
        UPLOADED_BLOB_WHOLE_V1,
        UPLOADED_BLOB_PAGE_SPAN_V1,
        MCP_CALL_IO_V1,
        MCP_CALL_IO_CITATION_V1,
    ],
    state_surfaces: STATE_SURFACES,
    kernel_surfaces: KERNEL_SURFACES,
    tools: TOOLS,
    resources: RESOURCES,
    projection: ProjectionDecl::Table(CORE_PROJECTION),
    bespoke_erase_legs: BESPOKE_ERASE_LEGS,
    bespoke_transfer_legs: BESPOKE_TRANSFER_LEGS,
};

/// Look up a flavor-#0 resource by its palette scope key.
///
/// `const fn` so the dispatcher's match arms and the served manifest are
/// *derived* from the contract rather than repeating it: the three parallel
/// lists that used to spell each resource's scope key, path and URI template
/// separately are now one declaration and two projections of it.
///
/// # Panics
///
/// Panics at compile time when `scope_key` names no declared resource.
#[must_use]
pub const fn resource(scope_key: &str) -> &'static ResourceContract {
    let mut index = 0;
    while index < RESOURCES.len() {
        if const_str_eq(RESOURCES[index].scope_key, scope_key) {
            return &RESOURCES[index];
        }
        index += 1;
    }
    panic!("no flavor #0 resource declares that scope key")
}

const fn const_str_eq(left: &str, right: &str) -> bool {
    let (left, right) = (left.as_bytes(), right.as_bytes());
    if left.len() != right.len() {
        return false;
    }
    let mut index = 0;
    while index < left.len() {
        if left[index] != right[index] {
            return false;
        }
        index += 1;
    }
    true
}

/// Register flavor #0's contract. Called from `FlavorRegistry::default()`
/// alongside the schema/tool registrations it describes, so the descriptor
/// and the registrations cannot be linked separately.
pub(crate) fn register(
    registry: &mut crate::FlavorRegistry,
) -> Result<(), crate::FlavorRegistryError> {
    registry.try_add_flavor(crate::FlavorDescriptor {
        flavor_id: FLAVOR_ID.to_string(),
        display_name: "Proxima Core".to_string(),
        package_version: env!("CARGO_PKG_VERSION").to_string(),
        author: option_env!("CARGO_PKG_AUTHORS")
            .filter(|authors| !authors.is_empty())
            .map(|authors| {
                authors
                    .split(':')
                    .next()
                    .unwrap_or(authors)
                    .trim()
                    .to_string()
            }),
        provenance: crate::FlavorProvenance::Builtin,
    })?;
    registry.try_add_contract(&FLAVOR_0)
}

#[cfg(test)]
mod tests {
    use super::{FLAVOR_0, RESOURCES, resource};
    use crate::flavor::contract::{SearchProjectionDecl, TransferRule};
    use crate::protocol::resource as scope;

    #[test]
    fn flavor_zero_is_core_and_holds_the_zero_ordinal() {
        assert_eq!(FLAVOR_0.flavor_id, "core");
        assert!(FLAVOR_0.is_core());
    }

    #[test]
    fn every_declared_schema_id_carries_the_flavor_prefix() {
        for schema in FLAVOR_0.schemas {
            assert!(
                schema.id.render().starts_with("core/"),
                "{} must carry the core/ prefix",
                schema.id.render()
            );
        }
    }

    #[test]
    fn every_declared_tool_carries_the_flavor_prefix() {
        for tool in FLAVOR_0.tools {
            assert!(
                tool.wire_name.starts_with("core_") || tool.wire_name.starts_with("core/"),
                "{} must carry the core prefix",
                tool.wire_name
            );
        }
    }

    #[test]
    fn ten_resources_from_nine_handler_modules() {
        assert_eq!(RESOURCES.len(), 10);
        assert_eq!(resource(scope::GOAL).name, "proxima-goal");
        assert_eq!(resource(scope::GOALS).path, "goals");
        assert!(RESOURCES.iter().all(|entry| entry.read_only));
    }

    /// Case 1 of the plan's acceptance set, as a declaration.
    #[test]
    fn goals_declare_not_transferable_with_real_enforcement() {
        for schema in FLAVOR_0
            .schemas
            .iter()
            .filter(|schema| schema.id.name == "task" || schema.id.name == "simple-text")
        {
            let TransferRule::NotTransferable { enforced_by, .. } = schema.transfer else {
                panic!("{} must declare NotTransferable", schema.id.render());
            };
            assert_eq!(
                enforced_by.len(),
                3,
                "goals are enforced three times over: engine refusal, storage backstop, trigger"
            );
        }
    }

    /// Case 2: declared absence is a value with a reason attached.
    #[test]
    fn declared_absence_carries_a_reason() {
        let write_act = FLAVOR_0
            .schemas
            .iter()
            .find(|schema| schema.id.name == "write-act")
            .expect("write-act is a flavor #0 schema");
        let SearchProjectionDecl::None { why } = write_act.search else {
            panic!("write-act declares itself a non-surface");
        };
        assert!(!why.is_empty(), "a declared non-surface states why");
    }
}
