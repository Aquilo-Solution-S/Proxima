//! The code flavor's declaration.
//!
//! The cross-checks read these declarations rather than a hand-written
//! list, each over the part it owns: `validate_contract_schemas` walks
//! `schemas` at freeze, and `check_owner_pinned_against_contracts` compares
//! every registered PG sidecar against the retain-at-source set derived
//! from here. Neither has to be edited when a declaration grows.
//!
//! `repos::erase` is the one lane that still names its tables by hand — a
//! flavor's own inverse, spelled as SQL constants — so it is CHECKED
//! against these declarations instead, in both directions:
//! `every_declared_surface_is_reached_by_the_repo_erase_or_named_as_an_exemption`
//! fails on a surface declared here that the erase misses without an
//! exemption, and `the_erase_names_no_table_the_contract_does_not_declare`
//! fails on a table the erase names that is not declared here.
//!
//! Style follows `crates/core/src/flavor/flavor0.rs` exactly: `const`
//! everything, `const fn` helpers for the repeated shapes, every field
//! named, and a `why` on every declared absence.

use proxima_core::ScopeDecl;
use proxima_core::SearchProjectionColumnKind as ColumnKind;
use proxima_core::flavor::{
    Band, BandComparability, CounterRule, DbConstraint, EmbedUnit, EmbeddingRecipe, EraseRule,
    ExportRule, FlavorContract, ForgetRule, KeyShape, LanguagePolicy, ProjectionDecl,
    ProjectionSpec, Provenance, RankSource, SLOT_DEFAULT, SchemaContract, SchemaRef,
    SearchProjectionDecl, SubstringArm, Surface, TS_RANK_NORMALIZATION_LOG_LENGTH_SCALE,
    TS_RANK_NORMALIZATION_NONE, TS_RANK_NORMALIZATION_SCALE, ToolContract, TransferRule,
    WEIGHT_UNIFORM, WeightedField,
};
use proxima_core::flavor0::{BAND_EXACT, BAND_RESCUE, BAND_SUBSTRING};
use proxima_core::verbs::schema::PayloadKind;

/// The prefix every code schema id and tool name already carries.
pub const FLAVOR_ID: &str = "proxima-code";

/// The flavor's declared lifecycle scopes, spelled in `repos::fence` beside
/// the registry statement they have to agree with.
const CODE_SCOPES: &[ScopeDecl] = &[crate::repos::CODE_REPO_SCOPE_DECL];

/// Non-zero, so the flavor stays out of unscoped `core_search_memories`
/// (`FlavorContract::declares_sidecar_table` is asked of flavor #0 only).
pub const CODE_ORDINAL: u16 = 1;

/// Commit search's three arms: flavor #0's windows, referenced rather than
/// respelled — which IS this flavor's band-comparability claim for these
/// two schemas, in the same way `BandComparability::CoreBands` would be at
/// flavor level.
///
/// The exact arm diverges in exactly one declared property: core's passes
/// `ts_rank_cd`'s normalization `32`, this one passes none, and
/// `Band::with_normalization` makes that a declared value rather than an
/// accident of two renderers. The WINDOW is core's: `[0.50, 1.00]`, from
/// `flavor0::BAND_EXACT`.
const COMMIT_BANDS: &[Band] = &[
    BAND_EXACT.with_normalization(TS_RANK_NORMALIZATION_NONE),
    BAND_RESCUE,
    BAND_SUBSTRING,
];

/// Chunk search's four arms.
///
/// Chunk search does not score on core's scale and never has: the arms are
/// based at 1.0/2.0/3.0/4.0 with a 0.6 width, and three additive literal
/// bonuses (+10 exact path, +6 path LIKE, +4 text LIKE) can carry a hit to
/// 24.9. Referencing core's bands here would be a false statement about
/// comparability; declaring the real windows is what lets the deployment
/// layer discover the divergence from the contract instead of from a score
/// it cannot explain.
///
/// The values live HERE, in the declaration that renders them: a band
/// nothing declares is a number with no author.
const CHUNK_BANDS: &[Band] = &[
    Band {
        name: CHUNK_BAND_STRICT,
        floor: 4.0,
        ceiling: 4.6,
        normalization: TS_RANK_NORMALIZATION_SCALE,
    },
    Band {
        name: CHUNK_BAND_RARE_ALL,
        floor: 3.0,
        ceiling: 3.6,
        normalization: TS_RANK_NORMALIZATION_LOG_LENGTH_SCALE,
    },
    Band {
        name: CHUNK_BAND_RARE_ANY,
        floor: 2.0,
        ceiling: 2.6,
        normalization: TS_RANK_NORMALIZATION_LOG_LENGTH_SCALE,
    },
    Band {
        name: CHUNK_BAND_RESCUE_ANY,
        floor: 1.0,
        ceiling: 1.6,
        normalization: TS_RANK_NORMALIZATION_LOG_LENGTH_SCALE,
    },
];

/// The name chunk search's strict `websearch_to_tsquery` arm resolves.
pub const CHUNK_BAND_STRICT: &str = "chunk-strict";
/// The name the all-distinctive-terms arm resolves.
pub const CHUNK_BAND_RARE_ALL: &str = "chunk-rare-all";
/// The name the any-distinctive-term arm resolves.
pub const CHUNK_BAND_RARE_ANY: &str = "chunk-rare-any";
/// The name the whole-query rescue arm resolves.
pub const CHUNK_BAND_RESCUE_ANY: &str = "chunk-rescue-any";

/// The schema ids the three search tools rank, spelled once. They are also
/// the `p.schema_id` literals in the tools' SQL, which
/// `the_schema_ids_the_search_sql_names_are_declared` pins against the
/// declaration.
pub const COMMIT_SCHEMA_ID: &str = "proxima-code/commit-v1";
/// See [`COMMIT_SCHEMA_ID`].
pub const COMMIT_SUMMARY_SCHEMA_ID: &str = "proxima-code/commit-summary-v1";
/// See [`COMMIT_SCHEMA_ID`].
pub const CODE_CHUNK_SCHEMA_ID: &str = "proxima-code/code-chunk-v1";

/// The band this flavor DECLARES for `schema_id` under `name`.
///
/// The SQL builders resolve their arms out of the declaration instead of
/// importing free constants that nothing checks the declaration against, so
/// a band has exactly one place to move.
///
/// # Panics
///
/// When the schema declares no search projection, or no band under `name`.
/// Both are contract bugs rather than runtime conditions, and
/// `every_arm_resolves_the_band_it_renders` fails before a query does.
#[must_use]
pub fn band(schema_id: &str, name: &str) -> Band {
    CODE_FLAVOR_CONTRACT
        .schemas
        .iter()
        .find(|schema| schema.schema_id().as_str() == schema_id)
        .and_then(|schema| schema.search.band(name))
        .unwrap_or_else(|| panic!("proxima-code declares no band {name:?} on {schema_id}"))
}

/// The substring arm `schema_id` DECLARES, or `None` for a schema that is
/// not a search surface.
///
/// This is what gates the three `LIKE` lanes: a schema that declares no arm
/// contributes no `LIKE` statement.
#[must_use]
pub fn substring_arm(schema_id: &str) -> Option<SubstringArm> {
    CODE_FLAVOR_CONTRACT
        .schemas
        .iter()
        .find(|schema| schema.schema_id().as_str() == schema_id)
        .and_then(|schema| schema.search.substring())
}

/// Every code sidecar is keyed on `memory.t` and carries no `owner_id`, so
/// it reaches its owner through the Memory and follows it. A `None`
/// `owner_column` is that claim, and it is what
/// `check_owner_pinned_against_contracts` compares against
/// `pg_sidecar!`'s `owner_pinned` flag — none of the sixteen sets it.
const fn memory_sidecar(table: &'static str, t_fkey: &'static str) -> Surface {
    Surface {
        table,
        key: KeyShape::MemoryT { column: "t" },
        owner_column: None,
        transfer: TransferRule::StaysOnKey,
        erase: EraseRule::ByKey,
        export: ExportRule::Rows,
        forget: ForgetRule::DumpThenDelete,
        lexical_language_column: None,
        counter: CounterRule::Counted("sidecar_rows"),
        completeness: Some(DbConstraint {
            relation: table,
            name: t_fkey,
        }),
    }
}

/// A detail table keyed off another sidecar's `t` with `ON DELETE CASCADE`.
/// Its rows are part of the parent payload, so forget dumps them before the
/// FK cascade and hydrate restores the exact zero-or-many set. It emits no
/// ERASE statement: the constraint is still the deletion proof.
///
/// `key_column` is the column carrying that `t`, and each detail table names
/// it differently: `criteria_memory_id`, `plan_memory_id`,
/// `caller_memory_id`, `test_requested_memory_id`. Export reads these
/// surfaces directly, so a wrong column here drops the table from every
/// owner bundle.
const fn detail_table(
    table: &'static str,
    key_column: &'static str,
    parent_fkey: &'static str,
) -> Surface {
    Surface {
        table,
        // The parent FK targets the parent sidecar's `t`, and that `t` is a
        // `proxima_core.memory` t — so this column holds a memory key under
        // a different name, which is exactly what `MemoryT { column }` says.
        key: KeyShape::MemoryT { column: key_column },
        owner_column: None,
        transfer: TransferRule::StaysOnKey,
        erase: EraseRule::Cascade {
            via: DbConstraint {
                relation: table,
                name: parent_fkey,
            },
        },
        export: ExportRule::Rows,
        forget: ForgetRule::DumpThenCascade,
        lexical_language_column: None,
        counter: CounterRule::Uncounted {
            why: "a detail table's rows go with the parent sidecar row, and the \
                  parent is already counted under `sidecar_rows`. Counting both \
                  would report one memory's destruction twice, at a ratio that \
                  varies with how many criteria or call edges it happened to \
                  have",
        },
        completeness: Some(DbConstraint {
            relation: table,
            name: parent_fkey,
        }),
    }
}

/// A typed sidecar that is neither a search surface nor embeddable: the
/// bulk of this flavor's schemas are structured records read by key.
///
/// `provenance` is a PARAMETER and not a default: `execution-plan-v1` writes
/// an origin on every ingest and `work-assignment-v1` grounds through two
/// payload columns, so a shared default would state something false about
/// them. A helper is exactly where a declaration goes to stop being a
/// declaration.
const fn record_schema(
    name: &'static str,
    kind: PayloadKind,
    table: &'static str,
    surfaces: &'static [Surface],
    why: &'static str,
    provenance: Provenance,
) -> SchemaContract {
    SchemaContract {
        id: SchemaRef::new(FLAVOR_ID, name, 1),
        kind,
        sidecar_table: Some(table),
        search: SearchProjectionDecl::None { why },
        embedding: EmbeddingRecipe::Never { why },
        transfer: TransferRule::StaysOnKey,
        provenance,
        surfaces,
        natural_key_columns: &[],
    }
}

/// A cited object or citation mapping: opaque, no Rust type, no sidecar of
/// its own. The eight/eight pairs are how a code Fact cites a blob.
const fn opaque_schema(name: &'static str, kind: PayloadKind) -> SchemaContract {
    SchemaContract {
        id: SchemaRef::new(FLAVOR_ID, name, 1),
        kind,
        sidecar_table: None,
        search: SearchProjectionDecl::None {
            why: "an opaque citation schema has no columns of its own; the bytes are the blob",
        },
        embedding: EmbeddingRecipe::Never {
            why: "an opaque citation schema names bytes, it does not carry content",
        },
        transfer: TransferRule::StaysOnKey,
        provenance: Provenance::None,
        surfaces: &[],
        natural_key_columns: &[],
    }
}

// ── Search surfaces ─────────────────────────────────────────────────────

const COMMIT_V1: SchemaContract = SchemaContract {
    id: SchemaRef::new(FLAVOR_ID, "commit", 1),
    kind: PayloadKind::Fact,
    sidecar_table: Some("proxima_code.commit_v1"),
    search: SearchProjectionDecl::Projected {
        fields: &[
            WeightedField {
                column: "sha",
                kind: ColumnKind::Text,
                weight: WEIGHT_UNIFORM,
            },
            WeightedField {
                column: "message",
                kind: ColumnKind::Text,
                weight: WEIGHT_UNIFORM,
            },
            WeightedField {
                column: "author_name",
                kind: ColumnKind::Text,
                weight: WEIGHT_UNIFORM,
            },
            WeightedField {
                column: "author_email",
                kind: ColumnKind::Text,
                weight: WEIGHT_UNIFORM,
            },
        ],
        tag_column: None,
        // `proxima_code.commit_search_tsv`, declared instead of called:
        // `simple` first so a word English would stem or drop survives,
        // then `english` so existing English searches keep matching. The
        // ORDER is load-bearing — tsvector concatenation offsets the right
        // operand's positions, and `ts_rank_cd` is position-sensitive.
        language: LanguagePolicy::PinnedUnion(&["simple", "english"]),
        bands: COMMIT_BANDS,
        substring: SubstringArm::SameTableLike,
    },
    embedding: EmbeddingRecipe::Units(&[EmbedUnit::stored("embed_text", SLOT_DEFAULT)]),
    transfer: TransferRule::StaysOnKey,
    provenance: Provenance::None,
    surfaces: &[memory_sidecar("proxima_code.commit_v1", "commit_v1_t_fkey")],
    natural_key_columns: &[],
};

const COMMIT_SUMMARY_V1: SchemaContract = SchemaContract {
    id: SchemaRef::new(FLAVOR_ID, "commit-summary", 1),
    kind: PayloadKind::Abstraction,
    sidecar_table: Some("proxima_code.commit_summary_v1"),
    search: SearchProjectionDecl::Projected {
        fields: &[
            WeightedField {
                column: "commit_sha",
                kind: ColumnKind::Text,
                weight: WEIGHT_UNIFORM,
            },
            WeightedField {
                column: "summary",
                kind: ColumnKind::Text,
                weight: WEIGHT_UNIFORM,
            },
            WeightedField {
                column: "key_files",
                kind: ColumnKind::TextArray,
                weight: WEIGHT_UNIFORM,
            },
        ],
        tag_column: None,
        language: LanguagePolicy::PinnedUnion(&["simple", "english"]),
        bands: COMMIT_BANDS,
        substring: SubstringArm::SameTableLike,
    },
    embedding: EmbeddingRecipe::Units(&[EmbedUnit::stored("embed_text", SLOT_DEFAULT)]),
    transfer: TransferRule::StaysOnKey,
    provenance: Provenance::None,
    surfaces: &[memory_sidecar(
        "proxima_code.commit_summary_v1",
        "commit_summary_v1_t_fkey",
    )],
    natural_key_columns: &[],
};

const CODE_CHUNK_V1: SchemaContract = SchemaContract {
    id: SchemaRef::new(FLAVOR_ID, "code-chunk", 1),
    kind: PayloadKind::Abstraction,
    sidecar_table: Some("proxima_code.code_chunk_v1"),
    search: SearchProjectionDecl::Projected {
        fields: &[
            WeightedField {
                column: "file_path",
                kind: ColumnKind::Text,
                weight: WEIGHT_UNIFORM,
            },
            WeightedField {
                column: "text",
                kind: ColumnKind::Text,
                weight: WEIGHT_UNIFORM,
            },
        ],
        tag_column: None,
        // `proxima_code.code_lexical_config()` returns `english`, and the
        // pin is the point: code search must not follow
        // `proxima_core.set_lexical_config`, which serves the deployment's
        // prose.
        language: LanguagePolicy::Pinned("english"),
        bands: CHUNK_BANDS,
        substring: SubstringArm::SameTableLike,
    },
    embedding: EmbeddingRecipe::Units(&[EmbedUnit::stored("embed_text", SLOT_DEFAULT)]),
    transfer: TransferRule::StaysOnKey,
    // Every chunk is derived from the file revision it was cut out of, and
    // from the commit when one is known (`ingest/blobs.rs` builds that list
    // unconditionally). Anything but `OriginEdges` makes the largest
    // Abstraction population in the tree a lineage dead end: a chunk's
    // `origins` written and never walked.
    provenance: Provenance::OriginEdges,
    surfaces: &[
        memory_sidecar("proxima_code.code_chunk_v1", "code_chunk_v1_t_fkey"),
        detail_table(
            "proxima_code.code_chunk_call_v1",
            "caller_memory_id",
            "code_chunk_call_v1_caller_memory_id_fkey",
        ),
    ],
    natural_key_columns: &[],
};

/// A file revision is a *path* surface, not a lexical projection: its index
/// is an expression GIN over `to_tsvector('simple', file_path)` serving
/// path prefix lookups, and it carries no `search_tsv` column. It is still
/// embeddable — `embed_text` is a generated column — which is why embedding
/// is declared separately from search rather than inside it.
const FILE_REVISION_V1: SchemaContract = SchemaContract {
    id: SchemaRef::new(FLAVOR_ID, "file-revision", 1),
    kind: PayloadKind::Fact,
    sidecar_table: Some("proxima_code.file_revision_v1"),
    search: SearchProjectionDecl::None {
        why: "a path surface, not a lexical one: the index is an expression GIN over \
              to_tsvector('simple', file_path) for path lookup, and the table carries no \
              search_tsv",
    },
    embedding: EmbeddingRecipe::Units(&[EmbedUnit::stored("embed_text", SLOT_DEFAULT)]),
    transfer: TransferRule::StaysOnKey,
    provenance: Provenance::None,
    surfaces: &[memory_sidecar(
        "proxima_code.file_revision_v1",
        "file_revision_v1_t_fkey",
    )],
    natural_key_columns: &["repo_id", "file_path"],
};

// ── Record surfaces ─────────────────────────────────────────────────────

const RECORD_WHY: &str =
    "a structured work record read by key from the bundle tool, not retrieved by content";

const WORK_REQUESTED_V1: SchemaContract = record_schema(
    "work-requested",
    PayloadKind::Fact,
    "proxima_code.work_requested_v1",
    &[memory_sidecar(
        "proxima_code.work_requested_v1",
        "work_requested_v1_t_fkey",
    )],
    RECORD_WHY,
    Provenance::None,
);

const TEST_REQUESTED_V1: SchemaContract = record_schema(
    "test-requested",
    PayloadKind::Fact,
    "proxima_code.test_requested_v1",
    &[
        memory_sidecar("proxima_code.test_requested_v1", "test_requested_v1_t_fkey"),
        detail_table(
            "proxima_code.test_requested_criterion_v1",
            "test_requested_memory_id",
            "test_requested_criterion_v1_test_requested_memory_id_fkey",
        ),
    ],
    RECORD_WHY,
    Provenance::None,
);

const ACCEPTANCE_CRITERIA_V1: SchemaContract = record_schema(
    "acceptance-criteria",
    PayloadKind::Fact,
    "proxima_code.acceptance_criteria_v1",
    &[
        memory_sidecar(
            "proxima_code.acceptance_criteria_v1",
            "acceptance_criteria_v1_t_fkey",
        ),
        detail_table(
            "proxima_code.acceptance_criterion_v1",
            "criteria_memory_id",
            "acceptance_criterion_v1_criteria_memory_id_fkey",
        ),
    ],
    RECORD_WHY,
    Provenance::None,
);

const EXECUTION_RESULT_V1: SchemaContract = record_schema(
    "execution-result",
    PayloadKind::Fact,
    "proxima_code.execution_result_v1",
    &[memory_sidecar(
        "proxima_code.execution_result_v1",
        "execution_result_v1_t_fkey",
    )],
    RECORD_WHY,
    Provenance::None,
);

const TEST_RESULT_V1: SchemaContract = record_schema(
    "test-result",
    PayloadKind::Fact,
    "proxima_code.test_result_v1",
    &[memory_sidecar(
        "proxima_code.test_result_v1",
        "test_result_v1_t_fkey",
    )],
    RECORD_WHY,
    Provenance::None,
);

const ACCEPTANCE_VERIFICATION_V1: SchemaContract = record_schema(
    "acceptance-verification",
    PayloadKind::Fact,
    "proxima_code.acceptance_verification_v1",
    &[memory_sidecar(
        "proxima_code.acceptance_verification_v1",
        "acceptance_verification_v1_t_fkey",
    )],
    RECORD_WHY,
    Provenance::None,
);

const EXECUTION_PLAN_V1: SchemaContract = record_schema(
    "execution-plan",
    PayloadKind::Abstraction,
    "proxima_code.execution_plan_v1",
    &[
        memory_sidecar("proxima_code.execution_plan_v1", "execution_plan_v1_t_fkey"),
        detail_table(
            "proxima_code.execution_plan_item_v1",
            "plan_memory_id",
            "execution_plan_item_v1_plan_memory_id_fkey",
        ),
    ],
    RECORD_WHY,
    // Every plan is authored from the perspective it was planned against:
    // `plan_persistence.rs` builds a one-element origin list and there is
    // no branch that leaves it empty. Anything but `OriginEdges` makes every
    // plan a lineage dead end.
    Provenance::OriginEdges,
);

const ACCEPTANCE_SUMMARY_V1: SchemaContract = record_schema(
    "acceptance-summary",
    PayloadKind::Abstraction,
    "proxima_code.acceptance_summary_v1",
    &[memory_sidecar(
        "proxima_code.acceptance_summary_v1",
        "acceptance_summary_v1_t_fkey",
    )],
    RECORD_WHY,
    Provenance::None,
);

const SELF_WHY: &str =
    "a self-description is a stable identity card addressed by key, not a corpus to search";

const DEVELOPMENT_PERSPECTIVE_V1: SchemaContract = record_schema(
    "development-perspective",
    PayloadKind::Perspective,
    "proxima_code.development_perspective_v1",
    &[memory_sidecar(
        "proxima_code.development_perspective_v1",
        "development_perspective_v1_t_fkey",
    )],
    "a repo-level judgement surfaced by the bundle tool for its repo, not by content search",
    Provenance::None,
);

const COMMIT_SUMMARIZER_SELF_V1: SchemaContract = record_schema(
    "commit-summarizer-self",
    PayloadKind::Perspective,
    "proxima_code.commit_summarizer_self_v1",
    &[memory_sidecar(
        "proxima_code.commit_summarizer_self_v1",
        "commit_summarizer_self_v1_t_fkey",
    )],
    SELF_WHY,
    Provenance::None,
);

const ENGINEER_SELF_V1: SchemaContract = record_schema(
    "engineer-self",
    PayloadKind::Perspective,
    "proxima_code.engineer_self_v1",
    &[memory_sidecar(
        "proxima_code.engineer_self_v1",
        "engineer_self_v1_t_fkey",
    )],
    SELF_WHY,
    Provenance::None,
);

const WORK_ASSIGNMENT_V1: SchemaContract = record_schema(
    "work-assignment",
    PayloadKind::Perspective,
    "proxima_code.work_assignment_v1",
    &[memory_sidecar(
        "proxima_code.work_assignment_v1",
        "work_assignment_v1_t_fkey",
    )],
    "an assignment claim is walked from the work item it names, never searched",
    // An assignment consumes nothing; it grounds through the references its
    // payload carries, which is precisely `PayloadOnly`. Both columns hold a
    // memory id, `references()` names them, and the walk reaches them
    // through those names.
    Provenance::PayloadOnly {
        subject_columns: &["target_perspective_memory_id", "work_item_memory_id"],
    },
);

// ── Flavor state ────────────────────────────────────────────────────────

/// A repo registration names a working tree on the host that registered it.
///
/// `RetainAtSource` states that a repo registration does not follow its
/// memories: no code path reassigns `repos.owner_id`, and every
/// `UPDATE proxima_code.repos` uses the owner only in `WHERE`.
///
/// `completeness: None` is equally deliberate: neither state table has a
/// foreign key to `proxima_core.owners`, so no constraint proves the
/// inverse reaches them.
const STATE_SURFACES: &[Surface] = &[
    Surface {
        table: "proxima_code.repos",
        key: KeyShape::Custom(&["owner_kind", "owner_id", "repo_id"]),
        owner_column: Some("owner_id"),
        transfer: TransferRule::RetainAtSource {
            why: "a repo registration is a path on the host that registered it, plus that \
                  host's ingestion cursor. Transferring memories does not give the \
                  destination the working tree, and re-homing the registration would \
                  point one owner's ingest at another owner's disk",
        },
        erase: EraseRule::ByOwner,
        export: ExportRule::Rows,
        forget: ForgetRule::Keep {
            why: "forgetting one ingested memory must not deregister the repository it came \
                  from; the row holds no foreign key into memory",
        },
        lexical_language_column: None,
        counter: CounterRule::Counted("repo_rows"),
        completeness: None,
    },
    Surface {
        table: "proxima_code.repo_ingestion_runs",
        key: KeyShape::Custom(&["run_id"]),
        owner_column: Some("owner_id"),
        transfer: TransferRule::RetainAtSource {
            why: "an ingestion run is a receipt for work this owner's host did; it proves \
                  admission by that owner and does not travel",
        },
        erase: EraseRule::ByOwner,
        export: ExportRule::Rows,
        forget: ForgetRule::Keep {
            why: "a run receipt survives the memories it produced, the same way \
                  ingest_keys does in the kernel",
        },
        lexical_language_column: None,
        counter: CounterRule::Counted("ingestion_run_rows"),
        completeness: None,
    },
];

// ── Tools ───────────────────────────────────────────────────────────────

/// The eleven wire names, with `idempotent` mirroring each tool's
/// `ANNOTATIONS`. No code tool is a dispatcher, so every `actions` is empty.
const TOOLS: &[ToolContract] = &[
    ToolContract {
        wire_name: "proxima-code_list_repos",
        actions: &[],
        idempotent: true,
    },
    ToolContract {
        wire_name: "proxima-code_register_repo",
        actions: &[],
        idempotent: true,
    },
    ToolContract {
        wire_name: "proxima-code_ingest_head_snapshot",
        actions: &[],
        idempotent: true,
    },
    ToolContract {
        wire_name: "proxima-code_erase_repo",
        actions: &[],
        idempotent: false,
    },
    ToolContract {
        wire_name: "proxima-code_search_chunks",
        actions: &[],
        idempotent: true,
    },
    ToolContract {
        wire_name: "proxima-code_open_file_revision",
        actions: &[],
        idempotent: true,
    },
    ToolContract {
        wire_name: "proxima-code_search_commits",
        actions: &[],
        idempotent: true,
    },
    ToolContract {
        wire_name: "proxima-code_emit_execution_request",
        actions: &[],
        idempotent: false,
    },
    ToolContract {
        wire_name: "proxima-code_emit_execution_plan",
        actions: &[],
        idempotent: false,
    },
    ToolContract {
        wire_name: "proxima-code_retry_execution_request",
        actions: &[],
        idempotent: false,
    },
    ToolContract {
        wire_name: "proxima-code_work_item_bundle",
        actions: &[],
        idempotent: true,
    },
];

/// The code flavor's contract: thirty-two schema registrations (sixteen
/// typed sidecars, sixteen opaque citation schemas), eleven tools, and no
/// resources — the freeze forbids them to any flavor but #0, and this one
/// has never declared any.
pub static CODE_FLAVOR_CONTRACT: FlavorContract = FlavorContract {
    flavor_id: FLAVOR_ID,
    ordinal: CODE_ORDINAL,
    schemas: &[
        // Fact
        COMMIT_V1,
        FILE_REVISION_V1,
        WORK_REQUESTED_V1,
        TEST_REQUESTED_V1,
        ACCEPTANCE_CRITERIA_V1,
        EXECUTION_RESULT_V1,
        TEST_RESULT_V1,
        ACCEPTANCE_VERIFICATION_V1,
        // Abstraction
        CODE_CHUNK_V1,
        COMMIT_SUMMARY_V1,
        EXECUTION_PLAN_V1,
        ACCEPTANCE_SUMMARY_V1,
        // Perspective
        DEVELOPMENT_PERSPECTIVE_V1,
        COMMIT_SUMMARIZER_SELF_V1,
        ENGINEER_SELF_V1,
        WORK_ASSIGNMENT_V1,
        // Opaque cited objects
        opaque_schema("code-blob", PayloadKind::CitedObject),
        opaque_schema("code-commit-object", PayloadKind::CitedObject),
        opaque_schema("execution-request-object", PayloadKind::CitedObject),
        opaque_schema("acceptance-criteria-object", PayloadKind::CitedObject),
        opaque_schema("test-request-object", PayloadKind::CitedObject),
        opaque_schema("execution-result-object", PayloadKind::CitedObject),
        opaque_schema("test-result-object", PayloadKind::CitedObject),
        opaque_schema("acceptance-verification-object", PayloadKind::CitedObject),
        // Opaque citation mappings
        opaque_schema("code-blob-whole", PayloadKind::CitationMapping),
        opaque_schema("code-commit-whole", PayloadKind::CitationMapping),
        opaque_schema("execution-request-whole", PayloadKind::CitationMapping),
        opaque_schema("acceptance-criteria-whole", PayloadKind::CitationMapping),
        opaque_schema("test-request-whole", PayloadKind::CitationMapping),
        opaque_schema("execution-result-whole", PayloadKind::CitationMapping),
        opaque_schema("test-result-whole", PayloadKind::CitationMapping),
        opaque_schema(
            "acceptance-verification-whole",
            PayloadKind::CitationMapping,
        ),
    ],
    state_surfaces: STATE_SURFACES,
    // One declared lifecycle scope. From it the substrate generates the
    // `proxima-scope-fence:code-repo:…` key and the liveness probe over
    // `proxima_code.repos`, and takes both on every admission of a payload
    // that names `CODE_REPO_SCOPE` — including one a host writes straight
    // through `Engine`, which is the whole reason the declaration lives
    // here rather than in each write path.
    scopes: CODE_SCOPES,
    kernel_surfaces: &[],
    tools: TOOLS,
    resources: &[],
    projection: ProjectionDecl::Table(CODE_PROJECTION),
    // Every code surface is a memory-keyed sidecar or the repo state table,
    // and the generic loops reach all of them, so nothing here needs a
    // hand-written statement. Freeze proves that rather than the flavor
    // asserting it.
    bespoke_erase_legs: &[],
    bespoke_transfer_legs: &[],
};

/// The code flavor's projection: three search surfaces, one table, one
/// composite GIN — byte-identical DDL to core's modulo the schema name and
/// the index name, which is the slimness rule made checkable.
const CODE_PROJECTION: ProjectionSpec = ProjectionSpec {
    table: "proxima_code.projection",
    index: "code_projection_owner_tsv_gin",
    // RESERVED, UNCONSUMED. Chunk search overfetches 4x its limit; the cap
    // recorded here is core's, so a shard-aware merge has one number to
    // start from rather than three.
    overfetch_k: 1_000,
    // CONSUMED by core's flavor-scan filter, which admits a non-core
    // projection into core's merge only under `CoreBands`. The divergence
    // is chunk search's, not commit search's.
    band_comparability: BandComparability::Divergent {
        why: "proxima-code/code-chunk-v1 scores on four arms based at 1.0/2.0/3.0/4.0 with \
              additive literal bonuses up to +20.3, so a chunk hit is not comparable to a \
              core hit without a rescale; commit search does score inside core's bands",
    },
    // Both index columns sit on `p`, so the composite
    // `gin(owner_id, search_tsv)` is reached and the owner is an Index Cond,
    // but the top-k is taken on the sidecar because the score reads sidecar
    // columns and the selective filters are sidecar-side. Taking it on the
    // projection would need `repo_id`, `language`, `chunk_type` and `state`
    // there too — a per-flavor projection shape, which the slim generator
    // does not emit.
    rank_source: RankSource::SidecarWithProjectionOwner {
        why: "chunk search's score reads sidecar columns the projection does not carry \
              (chunk_type, an exact file_path match, a path LIKE and a text LIKE contribute \
              up to +20.3, dwarfing the tsvector band), and repo_id / language / chunk_type / \
              state are the selective predicates and live on the sidecar; a projection-side \
              top-k would order by the smaller half of the score and spend the whole \
              candidate budget on the largest repository",
    },
};

#[cfg(test)]
mod tests {
    use super::{CODE_FLAVOR_CONTRACT, CODE_ORDINAL, FLAVOR_ID};

    #[test]
    fn every_declared_schema_id_carries_the_flavor_prefix() {
        for schema in CODE_FLAVOR_CONTRACT.schemas {
            let id = schema.schema_id();
            assert!(
                id.as_str().starts_with(&format!("{FLAVOR_ID}/")),
                "{id} does not carry the flavor prefix"
            );
        }
    }

    /// Thirty-two registrations is the whole surface: sixteen typed
    /// sidecars and sixteen opaque citation schemas. `SchemaWithoutContract`
    /// fires on any registered `proxima-code/*` the contract omits, so this
    /// number is pinned by the freeze as well — but pinning it here says
    /// which half moved.
    #[test]
    fn the_contract_declares_thirty_two_schemas_and_eleven_tools() {
        assert_eq!(CODE_FLAVOR_CONTRACT.schemas.len(), 32);
        let sidecars = CODE_FLAVOR_CONTRACT
            .schemas
            .iter()
            .filter(|schema| schema.sidecar_table.is_some())
            .count();
        assert_eq!(sidecars, 16);
        assert_eq!(CODE_FLAVOR_CONTRACT.tools.len(), 11);
    }

    /// The freeze rejects resources from any flavor but #0. This one has
    /// never declared any, so conformance is by absence — pin it so a future
    /// `proxima://code/...` has to argue with a test.
    #[test]
    fn the_flavor_declares_no_resources_and_is_not_core() {
        assert!(CODE_FLAVOR_CONTRACT.resources.is_empty());
        assert!(!CODE_FLAVOR_CONTRACT.is_core());
        assert_ne!(CODE_ORDINAL, proxima_core::flavor::CORE_ORDINAL);
    }

    /// No code sidecar carries its own `owner_id`, and
    /// `check_owner_pinned_against_contracts` compares that against
    /// `pg_sidecar!(owner_pinned)`. An accidental `RetainAtSource` here
    /// would fail the storage freeze at boot with a message about a flag
    /// nobody changed.
    #[test]
    fn no_schema_retains_at_source() {
        assert!(CODE_FLAVOR_CONTRACT.retain_at_source_tables().is_empty());
    }

    /// The acceptance test for the declaration as a whole: a registry with
    /// core and this flavor in it has to survive every cross-check the
    /// freeze runs, and then the PG registration has to survive
    /// `freeze_against` — schema-to-table agreement, typed inserters,
    /// `owner_pinned` against `TransferRule`, and the projection generator.
    ///
    /// This is the same composition `ProximaBuilder::boot` performs, so a
    /// declaration that would fail at boot fails here instead.
    #[test]
    fn the_composed_registry_freezes_with_this_contract_and_its_pg_sidecars() {
        let mut registry = proxima_core::FlavorRegistry::new();
        crate::register(&mut registry).expect("the code flavor registers");
        let frozen = registry.try_freeze().expect("core plus code freeze");
        assert!(
            frozen
                .contracts()
                .iter()
                .any(|contract| contract.flavor_id == FLAVOR_ID),
            "the frozen registry must carry this contract, not just its schemas"
        );

        let projected: Vec<&str> = frozen
            .search_projections()
            .iter()
            .map(|projection| projection.schema_id.as_str())
            .filter(|id| id.starts_with(FLAVOR_ID))
            .collect();
        assert_eq!(
            projected,
            vec![
                "proxima-code/commit-v1",
                "proxima-code/code-chunk-v1",
                "proxima-code/commit-summary-v1",
            ],
            "three search surfaces in declaration order, and the projection \
             generator reached all three"
        );

        let mut sidecars = proxima_storage_pg::PgSidecarRegistry::new();
        proxima_storage_pg::register_core_pg_sidecars(&mut sidecars);
        crate::register_pg_sidecars(&mut sidecars);
        sidecars
            .freeze_against(&frozen)
            .expect("the PG registrations agree with the contract");
    }
}
