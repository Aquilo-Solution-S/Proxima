//! The code flavor's declaration.
//!
//! Until now this flavor registered thirty-two schemas and eleven tools
//! through `proxima_flavor!` and declared nothing about what its rows *are*.
//! Two cross-checks were inert as a direct result:
//! `validate_contract_schemas` never saw its schemas, and
//! `check_owner_pinned_against_contracts` skipped it outright — its own doc
//! comment said so ("the code flavor ships no `FlavorContract`, so its
//! sixteen sidecars are exempt"). Every lane that should have iterated its
//! declarations named its tables by hand instead, which is why
//! `code_repo_erase` deletes five of sixteen sidecars and reports counters
//! it never counted.
//!
//! Style follows `crates/core/src/flavor/flavor0.rs` exactly: `const`
//! everything, `const fn` helpers for the repeated shapes, every field
//! named, and a `why` on every declared absence.

use proxima_core::SearchProjectionColumnKind as ColumnKind;
use proxima_core::flavor::{
    BAND_EXACT, BAND_RESCUE, BAND_SUBSTRING, Band, BandComparability, DbConstraint, EmbedUnit,
    EmbeddingRecipe, EraseRule, ExportRule, FlavorContract, ForgetRule, KeyShape, LanguagePolicy,
    ProjectionDecl, ProjectionSpec, Provenance, SLOT_DEFAULT, SchemaContract, SchemaRef,
    SearchProjectionDecl, SubstringArm, Surface, ToolContract, TransferRule, WEIGHT_UNIFORM,
    WeightedField,
};
use proxima_core::verbs::schema::PayloadKind;

/// The prefix every code schema id and tool name already carries.
pub const FLAVOR_ID: &str = "proxima-code";

/// Non-zero, so the flavor stays out of unscoped `core_search_memories`
/// (`FlavorContract::declares_sidecar_table` is asked of flavor #0 only).
pub const CODE_ORDINAL: u16 = 1;

/// Commit search's three arms, once the exact arm is banded.
const COMMIT_BANDS: &[Band] = &[BAND_EXACT, BAND_RESCUE, BAND_SUBSTRING];

// Chunk search does not score on core's scale and never has. Its four
// lexical arms are based at 1.0/2.0/3.0/4.0 with a 0.6 width, and three
// additive literal bonuses (+10 exact path, +6 path LIKE, +4 text LIKE) can
// carry a hit to 24.9. Declaring core's `BANDS` here would be a false
// statement about a merge that has not been written yet; declaring the real
// windows is what lets the deployment layer discover the divergence from
// the contract instead of from a score it cannot explain.
pub const CHUNK_BAND_STRICT: Band = Band {
    name: "chunk-strict",
    floor: 4.0,
    ceiling: 4.6,
};
pub const CHUNK_BAND_RARE_ALL: Band = Band {
    name: "chunk-rare-all",
    floor: 3.0,
    ceiling: 3.6,
};
pub const CHUNK_BAND_RARE_ANY: Band = Band {
    name: "chunk-rare-any",
    floor: 2.0,
    ceiling: 2.6,
};
pub const CHUNK_BAND_RESCUE_ANY: Band = Band {
    name: "chunk-rescue-any",
    floor: 1.0,
    ceiling: 1.6,
};
/// A band as SQL renders it: `(floor, ceiling - floor)`, at the two
/// decimals the bands are declared with.
///
/// Rendered rather than printed through `f32`'s `Display` for the same
/// reason core's `band_parts` is: `0.45f32 - 0.25f32` is `0.19999999`,
/// a different NUMBER from the `0.2` the SQL carried.
#[must_use]
pub fn band_parts(band: Band) -> (String, String) {
    (
        format!("{:.2}", band.floor),
        format!("{:.2}", band.ceiling - band.floor),
    )
}

const CHUNK_BANDS: &[Band] = &[
    CHUNK_BAND_STRICT,
    CHUNK_BAND_RARE_ALL,
    CHUNK_BAND_RARE_ANY,
    CHUNK_BAND_RESCUE_ANY,
];

/// Every code sidecar is keyed on `memory.t` and carries no `owner_id`, so
/// it reaches its owner through the Memory and follows it. EMPTY
/// `owner_columns` is that claim, and it is what
/// `check_owner_pinned_against_contracts` now compares against
/// `pg_sidecar!`'s `owner_pinned` flag — none of the sixteen sets it.
const fn memory_sidecar(table: &'static str, t_fkey: &'static str) -> Surface {
    Surface {
        table,
        key: KeyShape::MemoryT,
        owner_columns: &[],
        transfer: TransferRule::StaysOnKey,
        erase: EraseRule::ByKey,
        export: ExportRule::Rows,
        forget: ForgetRule::DumpThenDelete,
        lexical_language_column: None,
        counter: Some("sidecar_rows"),
        completeness: Some(DbConstraint {
            relation: table,
            name: t_fkey,
        }),
    }
}

/// A detail table keyed off another sidecar's `t` with `ON DELETE CASCADE`.
/// It emits no statement in any inverse: the constraint is the proof.
const fn detail_table(table: &'static str, parent_fkey: &'static str) -> Surface {
    Surface {
        table,
        key: KeyShape::Custom(&["memory_id"]),
        owner_columns: &[],
        transfer: TransferRule::StaysOnKey,
        erase: EraseRule::Cascade {
            via: DbConstraint {
                relation: table,
                name: parent_fkey,
            },
        },
        export: ExportRule::Rows,
        forget: ForgetRule::DeleteWithMemory,
        lexical_language_column: None,
        counter: None,
        completeness: Some(DbConstraint {
            relation: table,
            name: parent_fkey,
        }),
    }
}

/// A typed sidecar that is neither a search surface nor embeddable: the
/// bulk of this flavor's schemas are structured records read by key.
const fn record_schema(
    name: &'static str,
    kind: PayloadKind,
    table: &'static str,
    surfaces: &'static [Surface],
    why: &'static str,
) -> SchemaContract {
    SchemaContract {
        id: SchemaRef::new(FLAVOR_ID, name, 1),
        kind,
        sidecar_table: Some(table),
        search: SearchProjectionDecl::None { why },
        embedding: EmbeddingRecipe::Never { why },
        transfer: TransferRule::StaysOnKey,
        provenance: Provenance::None,
        surfaces,
        natural_key_columns: &[],
        special_category: false,
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
        special_category: false,
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
    special_category: false,
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
    special_category: false,
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
    provenance: Provenance::None,
    surfaces: &[
        memory_sidecar("proxima_code.code_chunk_v1", "code_chunk_v1_t_fkey"),
        detail_table(
            "proxima_code.code_chunk_call_v1",
            "code_chunk_call_v1_caller_memory_id_fkey",
        ),
    ],
    natural_key_columns: &[],
    special_category: false,
};

/// A file revision is a *path* surface, not a lexical projection: its index
/// is an expression GIN over `to_tsvector('simple', file_path)` serving
/// path prefix lookups, and it carries no `search_tsv` column. It is still
/// embeddable — `embed_text` is a generated column — which is exactly the
/// pair the old `SearchProjection` could not express, because it carried
/// `embed_text_column` inside the *search* declaration.
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
    special_category: false,
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
);

const TEST_REQUESTED_V1: SchemaContract = record_schema(
    "test-requested",
    PayloadKind::Fact,
    "proxima_code.test_requested_v1",
    &[
        memory_sidecar("proxima_code.test_requested_v1", "test_requested_v1_t_fkey"),
        detail_table(
            "proxima_code.test_requested_criterion_v1",
            "test_requested_criterion_v1_test_requested_memory_id_fkey",
        ),
    ],
    RECORD_WHY,
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
            "acceptance_criterion_v1_criteria_memory_id_fkey",
        ),
    ],
    RECORD_WHY,
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
);

const EXECUTION_PLAN_V1: SchemaContract = record_schema(
    "execution-plan",
    PayloadKind::Abstraction,
    "proxima_code.execution_plan_v1",
    &[
        memory_sidecar("proxima_code.execution_plan_v1", "execution_plan_v1_t_fkey"),
        detail_table(
            "proxima_code.execution_plan_item_v1",
            "execution_plan_item_v1_plan_memory_id_fkey",
        ),
    ],
    RECORD_WHY,
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
);

// ── Flavor state ────────────────────────────────────────────────────────

/// A repo registration names a working tree on the host that registered it.
///
/// `RetainAtSource` is the honest reading of what transfer does today,
/// which is nothing: no code path reassigns `repos.owner_id` (every
/// `UPDATE proxima_code.repos` uses the owner only in `WHERE`). Saying so
/// makes "a repo registration does not follow its memories" a decision
/// rather than an omission.
///
/// `completeness: None` is equally deliberate: neither state table has a
/// foreign key to `proxima_core.owners`, so no constraint proves the
/// inverse reaches them.
const STATE_SURFACES: &[Surface] = &[
    Surface {
        table: "proxima_code.repos",
        key: KeyShape::Custom(&["owner_kind", "owner_id", "repo_id"]),
        owner_columns: &["owner_id"],
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
        counter: Some("repo_rows"),
        completeness: None,
    },
    Surface {
        table: "proxima_code.repo_ingestion_runs",
        key: KeyShape::Custom(&["run_id"]),
        owner_columns: &["owner_id"],
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
        counter: Some("ingestion_run_rows"),
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
    kernel_surfaces: &[],
    tools: TOOLS,
    resources: &[],
    projection: ProjectionDecl::Table(CODE_PROJECTION),
};

/// The code flavor's projection: three search surfaces, one table, one
/// composite GIN — byte-identical DDL to core's modulo the schema name and
/// the index name, which is the slimness rule made checkable.
const CODE_PROJECTION: ProjectionSpec = ProjectionSpec {
    table: "proxima_code.projection",
    index: "code_projection_owner_tsv_gin",
    // RESERVED, UNCONSUMED. Chunk search overfetches 4x its limit today;
    // the cap recorded here is core's, so a shard-aware merge starts from
    // one number rather than three.
    overfetch_k: 1_000,
    // Reserved, unconsumed: this flavor's bands are not on core's scale,
    // and plan §3's band-aware merge is the first thing that will need to
    // know. The divergence is chunk search's, not commit search's.
    band_comparability: BandComparability::Divergent {
        why: "proxima-code/code-chunk-v1 scores on four arms based at 1.0/2.0/3.0/4.0 with \
              additive literal bonuses up to +20.3, so a chunk hit is not comparable to a \
              core hit without a rescale; commit search does score inside core's bands",
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
    /// never declared any, so compliance is by absence — pin it so a future
    /// `proxima://code/...` has to argue with a test.
    #[test]
    fn the_flavor_declares_no_resources_and_is_not_core() {
        assert!(CODE_FLAVOR_CONTRACT.resources.is_empty());
        assert!(!CODE_FLAVOR_CONTRACT.is_core());
        assert_ne!(CODE_ORDINAL, proxima_core::flavor::CORE_ORDINAL);
    }

    /// No code sidecar carries its own `owner_id`, and
    /// `check_owner_pinned_against_contracts` now compares that against
    /// `pg_sidecar!(owner_pinned)` for the first time. An accidental
    /// `RetainAtSource` here would fail the storage freeze at boot with a
    /// message about a flag nobody changed.
    #[test]
    fn no_schema_retains_at_source() {
        assert!(CODE_FLAVOR_CONTRACT.retain_at_source_tables().is_empty());
    }
}
