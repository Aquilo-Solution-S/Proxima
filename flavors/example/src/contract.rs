//! The example flavor's contract. A test and tutorial fixture, not a product.

use proxima::flavor::{
    CounterRule, DbConstraint, EmbeddingRecipe, EraseRule, ExportRule, FlavorContract, ForgetRule,
    KeyShape, ProjectionDecl, Provenance, SchemaContract, SchemaRef, SearchProjectionDecl, Surface,
    ToolContract, TransferRule,
};
use proxima::flavor::{PayloadKind, ScopeDecl};

use crate::payload::NOTEBOOK_SCOPE;

/// The flavor id; tool names carry it as their `proxima-example_` prefix.
pub const FLAVOR_ID: &str = "proxima-example";

/// The flavor's Postgres schema.
pub const PG_SCHEMA: &str = "proxima_example";

/// Ordinal 11: clear of 1..=5, which hosts reserve.
pub const EXAMPLE_ORDINAL: u16 = 11;

/// The notebook scope: its registry is `proxima_example.notebooks`.
pub const NOTEBOOK_SCOPE_DECL: ScopeDecl = ScopeDecl {
    kind: NOTEBOOK_SCOPE,
    registry_table: "proxima_example.notebooks",
    id_column: "notebook_id",
    owner_id_column: "owner_id",
};

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

const NOTE_V1: SchemaContract = SchemaContract {
    id: SchemaRef::new(FLAVOR_ID, "note", 1),
    kind: PayloadKind::Fact,
    sidecar_table: Some("proxima_example.note_v1"),
    search: SearchProjectionDecl::None {
        why: "a test fixture: search is covered by core's own projection",
    },
    embedding: EmbeddingRecipe::Never {
        why: "a test fixture: embedding is covered by core's own schemas",
    },
    transfer: TransferRule::StaysOnKey,
    provenance: Provenance::None,
    surfaces: &[memory_sidecar("proxima_example.note_v1", "note_v1_t_fkey")],
    natural_key_columns: &["notebook_id", "key"],
};

const DIGEST_V1: SchemaContract = SchemaContract {
    id: SchemaRef::new(FLAVOR_ID, "digest", 1),
    kind: PayloadKind::Abstraction,
    sidecar_table: Some("proxima_example.digest_v1"),
    search: SearchProjectionDecl::None {
        why: "a test fixture for derived supersession, not a search surface",
    },
    embedding: EmbeddingRecipe::Never {
        why: "a test fixture for derived supersession, not a memory to embed",
    },
    transfer: TransferRule::StaysOnKey,
    provenance: Provenance::None,
    surfaces: &[memory_sidecar(
        "proxima_example.digest_v1",
        "digest_v1_t_fkey",
    )],
    natural_key_columns: &[],
};

const STAMP_V1: SchemaContract = SchemaContract {
    id: SchemaRef::new(FLAVOR_ID, "stamp", 1),
    kind: PayloadKind::Fact,
    sidecar_table: Some("proxima_example.stamp_v1"),
    search: SearchProjectionDecl::None {
        why: "a test fixture for owner:follows transfer, not a search surface",
    },
    embedding: EmbeddingRecipe::Never {
        why: "a test fixture for owner:follows transfer, not a memory to embed",
    },
    transfer: TransferRule::Follow,
    provenance: Provenance::None,
    surfaces: &[Surface {
        table: "proxima_example.stamp_v1",
        key: KeyShape::MemoryT { column: "t" },
        owner_column: Some("owner_id"),
        transfer: TransferRule::Follow,
        erase: EraseRule::ByKey,
        export: ExportRule::Rows,
        forget: ForgetRule::DumpThenDelete,
        lexical_language_column: None,
        counter: CounterRule::Counted("sidecar_rows"),
        completeness: Some(DbConstraint {
            relation: "proxima_example.stamp_v1",
            name: "stamp_v1_t_fkey",
        }),
    }],
    natural_key_columns: &[],
};

const STATE_SURFACES: &[Surface] = &[Surface {
    table: "proxima_example.notebooks",
    key: KeyShape::Custom(&["owner_id", "notebook_id"]),
    owner_column: Some("owner_id"),
    transfer: TransferRule::RetainAtSource {
        why: "a notebook registration belongs to the owner that registered it",
    },
    erase: EraseRule::ByOwner,
    export: ExportRule::Rows,
    forget: ForgetRule::Keep {
        why: "forgetting one note must not deregister its notebook",
    },
    lexical_language_column: None,
    counter: CounterRule::Counted("notebook_rows"),
    completeness: None,
}];

/// Wire name of the flavor's one tool.
pub const LIST_NOTEBOOKS_TOOL: &str = "proxima-example_list_notebooks";

const TOOLS: &[ToolContract] = &[ToolContract {
    wire_name: LIST_NOTEBOOKS_TOOL,
    actions: &[],
}];

/// Baseline schemas only (note + digest). Tutorial kit flavors that splice
/// [`crate::BASELINE_SQL`] use this so stamp triggers are not required.
pub static EXAMPLE_BASELINE_CONTRACT: FlavorContract = FlavorContract {
    flavor_id: FLAVOR_ID,
    ordinal: EXAMPLE_ORDINAL,
    schemas: &[NOTE_V1, DIGEST_V1],
    state_surfaces: STATE_SURFACES,
    scopes: &[NOTEBOOK_SCOPE_DECL],
    kernel_surfaces: &[],
    tools: TOOLS,
    resources: &[],
    bespoke_erase_legs: &[],
    bespoke_transfer_legs: &[],
    projection: ProjectionDecl::None {
        why: "a test fixture registers no search surface",
    },
};

/// Full example contract: baseline schemas plus the stamp Fact (`owner: follows`).
pub static EXAMPLE_CONTRACT: FlavorContract = FlavorContract {
    flavor_id: FLAVOR_ID,
    ordinal: EXAMPLE_ORDINAL,
    schemas: &[NOTE_V1, DIGEST_V1, STAMP_V1],
    state_surfaces: STATE_SURFACES,
    scopes: &[NOTEBOOK_SCOPE_DECL],
    kernel_surfaces: &[],
    tools: TOOLS,
    resources: &[],
    bespoke_erase_legs: &[],
    bespoke_transfer_legs: &[],
    projection: ProjectionDecl::None {
        why: "a test fixture registers no search surface",
    },
};
