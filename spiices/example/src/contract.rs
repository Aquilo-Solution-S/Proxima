//! The example spiice's contract. A test and tutorial fixture, not a product.

use proxiima::spiice::{
    CounterRule, DbConstraint, EmbeddingRecipe, Enforcement, EraseRule, ExportRule, ForgetRule,
    KeyShape, NaturalKeyDeclaration, ProjectionDecl, Provenance, SchemaContract, SchemaRef,
    SearchProjectionDecl, SpiiceContract, Surface, ToolContract, TransferRule,
};
use proxiima::spiice::{PayloadKind, ScopeDecl};

use crate::payload::NOTEBOOK_SCOPE;

/// The spiice id; tool names carry it as their `proxiima-example_` prefix.
pub const SPIICE_ID: &str = "proxiima-example";

/// The spiice's Postgres schema.
pub const PG_SCHEMA: &str = "proxiima_example";

/// Ordinal 11: clear of 1..=5, which hosts reserve.
pub const EXAMPLE_ORDINAL: u16 = 11;

/// The notebook scope: its registry is `proxiima_example.notebooks`.
pub const NOTEBOOK_SCOPE_DECL: ScopeDecl = ScopeDecl {
    kind: NOTEBOOK_SCOPE,
    registry_table: "proxiima_example.notebooks",
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
    id: SchemaRef::new(SPIICE_ID, "note", 1),
    kind: PayloadKind::Fact,
    sidecar_table: Some("proxiima_example.note_v1"),
    search: SearchProjectionDecl::None {
        why: "a test fixture: search is covered by core's own projection",
        tag_column: None,
    },
    embedding: EmbeddingRecipe::Never {
        why: "a test fixture: embedding is covered by core's own schemas",
    },
    transfer: TransferRule::StaysOnKey,
    provenance: Provenance::None,
    surfaces: &[memory_sidecar("proxiima_example.note_v1", "note_v1_t_fkey")],
    natural_key_columns: NaturalKeyDeclaration::of::<crate::payload::NoteV1>(),
};

const DIGEST_V1: SchemaContract = SchemaContract {
    id: SchemaRef::new(SPIICE_ID, "digest", 1),
    kind: PayloadKind::Abstraction,
    sidecar_table: Some("proxiima_example.digest_v1"),
    search: SearchProjectionDecl::None {
        why: "a test fixture for derived supersession, not a search surface",
        tag_column: None,
    },
    embedding: EmbeddingRecipe::Never {
        why: "a test fixture for derived supersession, not a memory to embed",
    },
    transfer: TransferRule::StaysOnKey,
    provenance: Provenance::None,
    surfaces: &[memory_sidecar(
        "proxiima_example.digest_v1",
        "digest_v1_t_fkey",
    )],
    natural_key_columns: NaturalKeyDeclaration::EMPTY,
};

/// The stamp sidecar, shared by the stamp and the sealed stamp: its rows
/// follow their memory on transfer.
const STAMP_SURFACE: Surface = Surface {
    table: "proxiima_example.stamp_v1",
    key: KeyShape::MemoryT { column: "t" },
    owner_column: Some("owner_id"),
    transfer: TransferRule::Follow,
    erase: EraseRule::ByKey,
    export: ExportRule::Rows,
    forget: ForgetRule::DumpThenDelete,
    lexical_language_column: None,
    counter: CounterRule::Counted("sidecar_rows"),
    completeness: Some(DbConstraint {
        relation: "proxiima_example.stamp_v1",
        name: "stamp_v1_t_fkey",
    }),
};

const STAMP_V1: SchemaContract = SchemaContract {
    id: SchemaRef::new(SPIICE_ID, "stamp", 1),
    kind: PayloadKind::Fact,
    sidecar_table: Some("proxiima_example.stamp_v1"),
    search: SearchProjectionDecl::None {
        why: "a test fixture for owner:follows transfer, not a search surface",
        tag_column: None,
    },
    embedding: EmbeddingRecipe::Never {
        why: "a test fixture for owner:follows transfer, not a memory to embed",
    },
    transfer: TransferRule::Follow,
    provenance: Provenance::None,
    surfaces: &[STAMP_SURFACE],
    natural_key_columns: NaturalKeyDeclaration::EMPTY,
};

/// A stamp whose schema refuses transfer, over the stamp sidecar that would
/// otherwise follow: the refusal comes from the schema's declaration at the
/// two sites it names, not from the sidecar.
const SEALED_STAMP_V1: SchemaContract = SchemaContract {
    id: SchemaRef::new(SPIICE_ID, "sealed-stamp", 1),
    kind: PayloadKind::Fact,
    sidecar_table: Some("proxiima_example.stamp_v1"),
    search: SearchProjectionDecl::None {
        why: "a test fixture for NotTransferable, not a search surface",
        tag_column: None,
    },
    embedding: EmbeddingRecipe::Never {
        why: "a test fixture for NotTransferable, not a memory to embed",
    },
    transfer: TransferRule::NotTransferable {
        why: "a sealed stamp cannot change owner: it stays with the owner that wrote it",
        enforced_by: &[
            Enforcement::EngineRefusal {
                at: "core/src/engine/access_admin.rs::Engine::transfer_to_owner",
            },
            Enforcement::StorageBackstop {
                at: "storage-pg/src/access/owner_columns.rs::transfer_to_owner",
            },
        ],
    },
    provenance: Provenance::None,
    surfaces: &[STAMP_SURFACE],
    natural_key_columns: NaturalKeyDeclaration::EMPTY,
};

const STATE_SURFACES: &[Surface] = &[Surface {
    table: "proxiima_example.notebooks",
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

/// Wire name of the spiice's one tool.
pub const LIST_NOTEBOOKS_TOOL: &str = "proxiima-example_list_notebooks";

const TOOLS: &[ToolContract] = &[ToolContract {
    wire_name: LIST_NOTEBOOKS_TOOL,
    actions: &[],
}];

/// Baseline schemas only (note + digest). Tutorial kit spiices that splice
/// [`crate::BASELINE_SQL`] use this so stamp triggers are not required.
pub static EXAMPLE_BASELINE_CONTRACT: SpiiceContract = SpiiceContract {
    retired_facts: &[],
    spiice_id: SPIICE_ID,
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
    evolutions: &[],
};

/// Full example contract: baseline schemas plus the stamp Fact (`owner: follows`).
pub static EXAMPLE_CONTRACT: SpiiceContract = SpiiceContract {
    retired_facts: &[],
    spiice_id: SPIICE_ID,
    ordinal: EXAMPLE_ORDINAL,
    schemas: &[NOTE_V1, DIGEST_V1, STAMP_V1, SEALED_STAMP_V1],
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
    evolutions: &[],
};
