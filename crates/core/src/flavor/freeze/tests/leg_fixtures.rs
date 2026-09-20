//! Contracts shaped to leave one lifecycle leg uncovered.
//!
//! Each is a surface the generic erase, transfer or forget loop skips and
//! no bespoke leg claims. Unrefused they freeze cleanly and boot cleanly;
//! what they produce is rows that outlive the operation meant to reach
//! them, which no later assertion is positioned to see.

use super::fixtures::{FIXTURE_FLAVOR, erase_fixture, state_surface, transfer_fixture};
use crate::SearchProjectionColumnKind;
use crate::flavor::contract::{
    CounterRule, DbConstraint, EmbeddingRecipe, EraseRule, ExportRule, FlavorContract, ForgetRule,
    KeyShape, LanguagePolicy, ProjectionDecl, Provenance, SchemaContract, SchemaRef,
    SearchProjectionDecl, SubstringArm, Surface, TransferRule, WeightedField,
};
use crate::verbs::schema::PayloadKind;

/// The out-of-tree flavor this whole check exists for: `ByKey` on a key
/// the erase builds no selection set for, claimed by no bespoke leg.
/// Both generic loops skip it, so nothing deletes these rows and the
/// erase still reports `Completed`.
pub(super) static UNDELETABLE_SURFACE: FlavorContract = erase_fixture(
    &[state_surface(
        "test_flavor.thing_v1",
        KeyShape::Custom(&["thing_id"]),
        EraseRule::ByKey,
        ExportRule::Rows,
    )],
    &[],
);

/// A bespoke leg claiming a table the flavor does not declare — the
/// stale name that let the hand-written lists rot.
pub(super) static BESPOKE_LEG_FOR_NOTHING: FlavorContract = erase_fixture(
    &[state_surface(
        "test_flavor.thing_v1",
        KeyShape::MemoryT { column: "t" },
        EraseRule::ByKey,
        ExportRule::Rows,
    )],
    &["test_flavor.gone_v1"],
);

/// A `Follow` surface whose transfer no leg can perform, and the whole
/// reason the transfer partition exists.
///
/// Keyed on a `Custom` column the transfer builds no `t` set for, and
/// claimed by no bespoke leg. Unrefused it would freeze cleanly, boot
/// cleanly, and leave its rows with the SOURCE owner after every memory
/// that referenced them moved — which is not a stale row, it is a
/// cross-tenant read arrived at by silence.
pub(super) static UNMOVABLE_SURFACE: FlavorContract = transfer_fixture(
    &[Surface {
        table: "test_flavor.thing_v1",
        key: KeyShape::Custom(&["thing_id"]),
        owner_column: Some("owner_id"),
        transfer: TransferRule::Follow,
        erase: EraseRule::Never {
            why: "the transfer rule is what this fixture is about",
        },
        export: ExportRule::Excluded {
            why: "the transfer rule is what this fixture is about",
        },
        forget: ForgetRule::Keep {
            why: "a fixture, not a memory",
        },
        lexical_language_column: None,
        counter: CounterRule::Uncounted {
            why: "a fixture contributes to no receipt",
        },
        completeness: None,
    }],
    &[],
);

/// `Follow` with no owner column to set. The rows are reached through
/// their key's owner, which is what `StaysOnKey` says and what a `None`
/// `owner_column` claims; declaring `Follow` over it asks for an
/// `UPDATE` with an empty `SET`.
pub(super) static FOLLOW_WITH_NOTHING_TO_SET: FlavorContract = transfer_fixture(
    &[Surface {
        table: "test_flavor.thing_v1",
        key: KeyShape::MemoryT { column: "t" },
        owner_column: None,
        transfer: TransferRule::Follow,
        erase: EraseRule::Never {
            why: "the transfer rule is what this fixture is about",
        },
        export: ExportRule::Excluded {
            why: "the transfer rule is what this fixture is about",
        },
        forget: ForgetRule::Keep {
            why: "a fixture, not a memory",
        },
        lexical_language_column: None,
        counter: CounterRule::Uncounted {
            why: "a fixture contributes to no receipt",
        },
        completeness: None,
    }],
    &[],
);

/// A surface claiming forget destroys its rows over a key the forget
/// builds no `t` for, and no constraint to claim completeness either.
/// The rows outlive the memory that declared them gone.
pub(super) static UNFORGETTABLE_SURFACE: FlavorContract = transfer_fixture(
    &[Surface {
        table: "test_flavor.thing_v1",
        key: KeyShape::Custom(&["thing_id"]),
        owner_column: None,
        transfer: TransferRule::StaysOnKey,
        erase: EraseRule::Never {
            why: "the forget rule is what this fixture is about",
        },
        export: ExportRule::Excluded {
            why: "the forget rule is what this fixture is about",
        },
        forget: ForgetRule::DeleteWithMemory,
        lexical_language_column: None,
        counter: CounterRule::Uncounted {
            why: "a fixture contributes to no receipt",
        },
        completeness: None,
    }],
    &[],
);

/// A bespoke transfer leg naming a table the flavor does not declare.
pub(super) static BESPOKE_TRANSFER_LEG_FOR_NOTHING: FlavorContract = transfer_fixture(
    &[state_surface(
        "test_flavor.thing_v1",
        KeyShape::MemoryT { column: "t" },
        EraseRule::Never {
            why: "the transfer rule is what this fixture is about",
        },
        ExportRule::Excluded {
            why: "the transfer rule is what this fixture is about",
        },
    )],
    &["test_flavor.gone_v1"],
);

/// A flavor arguing with itself: `StaysOnKey` says nothing moves, and
/// the exemption list says a hand-written statement moves it.
pub(super) static BESPOKE_TRANSFER_LEG_OVER_A_NON_MOVE: FlavorContract = transfer_fixture(
    &[state_surface(
        "test_flavor.thing_v1",
        KeyShape::MemoryT { column: "t" },
        EraseRule::Never {
            why: "the transfer rule is what this fixture is about",
        },
        ExportRule::Excluded {
            why: "the transfer rule is what this fixture is about",
        },
    )],
    &["test_flavor.thing_v1"],
);

/// A flavor arguing with itself: the declaration says a constraint
/// removes the rows, and the exemption list says a hand-written
/// statement does.
pub(super) static BESPOKE_LEG_OVER_A_CASCADE: FlavorContract = erase_fixture(
    &[state_surface(
        "test_flavor.thing_v1",
        KeyShape::MemoryT { column: "t" },
        EraseRule::Cascade {
            via: DbConstraint {
                relation: "test_flavor.thing_v1",
                name: "thing_v1_t_fkey",
            },
        },
        ExportRule::Rows,
    )],
    &["test_flavor.thing_v1"],
);

/// Exportable while carrying neither an owner column nor a key with a
/// home table: the generator has no statement that reaches it from the
/// owner, so it would go missing from every bundle in silence.
pub(super) static UNREACHABLE_EXPORT_SURFACE: FlavorContract = erase_fixture(
    &[Surface {
        table: "test_flavor.thing_v1",
        key: KeyShape::Custom(&["thing_id"]),
        owner_column: None,
        transfer: TransferRule::StaysOnKey,
        erase: EraseRule::Never {
            why: "the export rule is what this fixture is about",
        },
        export: ExportRule::Rows,
        forget: ForgetRule::Keep {
            why: "a fixture, not a memory",
        },
        lexical_language_column: None,
        counter: CounterRule::Uncounted {
            why: "a fixture contributes to no receipt",
        },
        completeness: None,
    }],
    &[],
);

/// Host-managed ownerless exports are reached by the registered lifecycle
/// callback, which is checked against the exact declared table set by
/// storage at boot. The generic exporter has no owner join to spell.
pub(super) static HOST_MANAGED_OWNERLESS_EXPORTS: FlavorContract = erase_fixture(
    &[
        Surface {
            table: "test_flavor.host_rows",
            key: KeyShape::Custom(&["thing_id"]),
            owner_column: None,
            transfer: TransferRule::RetainAtSource {
                why: "the callback resolves owner from execution",
            },
            erase: EraseRule::HostState {
                whole_owner: crate::flavor::contract::HostStateEraseDisposition::Erase,
                source: crate::flavor::contract::HostStateEraseDisposition::Retain,
                exact_fact: crate::flavor::contract::HostStateEraseDisposition::Retain,
            },
            export: ExportRule::Rows,
            forget: ForgetRule::Keep {
                why: "a fixture, not a memory",
            },
            lexical_language_column: None,
            counter: CounterRule::Uncounted {
                why: "a fixture contributes to no receipt",
            },
            completeness: None,
        },
        Surface {
            table: "test_flavor.host_allowlist",
            key: KeyShape::Custom(&["thing_id"]),
            owner_column: None,
            transfer: TransferRule::RetainAtSource {
                why: "the callback resolves owner from execution",
            },
            erase: EraseRule::HostState {
                whole_owner: crate::flavor::contract::HostStateEraseDisposition::Erase,
                source: crate::flavor::contract::HostStateEraseDisposition::Retain,
                exact_fact: crate::flavor::contract::HostStateEraseDisposition::Retain,
            },
            export: ExportRule::Allowlist(&["thing_id"]),
            forget: ForgetRule::Keep {
                why: "a fixture, not a memory",
            },
            lexical_language_column: None,
            counter: CounterRule::Uncounted {
                why: "a fixture contributes to no receipt",
            },
            completeness: None,
        },
    ],
    &[],
);

pub(super) static UNREACHABLE_ALLOWLIST_EXPORT_SURFACE: FlavorContract = erase_fixture(
    &[Surface {
        table: "test_flavor.thing_v1",
        key: KeyShape::Custom(&["thing_id"]),
        owner_column: None,
        transfer: TransferRule::StaysOnKey,
        erase: EraseRule::Never {
            why: "the export rule is what this fixture is about",
        },
        export: ExportRule::Allowlist(&["thing_id"]),
        forget: ForgetRule::Keep {
            why: "a fixture, not a memory",
        },
        lexical_language_column: None,
        counter: CounterRule::Uncounted {
            why: "a fixture contributes to no receipt",
        },
        completeness: None,
    }],
    &[],
);

/// A `PerRow` policy naming a column that is not the projection
/// table's. The generator emits one language column per projection
/// table and names it `lexical_language`; a second name is a
/// declaration nothing renders.
pub(super) static PER_ROW_ON_THE_WRONG_COLUMN: FlavorContract = FlavorContract {
    flavor_id: FIXTURE_FLAVOR,
    ordinal: 7,
    schemas: &[SchemaContract {
        // A Fact, not a citation payload: a citation schema declaring a
        // sidecar trips CitationSidecarNotRemappable first and this
        // fixture would test that instead.
        id: SchemaRef::new(FIXTURE_FLAVOR, "thing", 1),
        kind: PayloadKind::Fact,
        sidecar_table: Some("test_flavor.thing_v1"),
        search: SearchProjectionDecl::Projected {
            fields: &[WeightedField {
                column: "a",
                kind: SearchProjectionColumnKind::Text,
                weight: 1.0,
            }],
            tag_column: None,
            language: LanguagePolicy::PerRow {
                column: "row_config",
            },
            bands: &[],
            substring: SubstringArm::Off,
        },
        embedding: EmbeddingRecipe::Never {
            why: "a fixture, not a memory",
        },
        transfer: TransferRule::StaysOnKey,
        provenance: Provenance::None,
        surfaces: &[],
        natural_key_columns: &[],
    }],
    state_surfaces: &[],
    scopes: &[],
    kernel_surfaces: &[],
    tools: &[],
    resources: &[],
    bespoke_erase_legs: &[],
    bespoke_transfer_legs: &[],
    projection: ProjectionDecl::Table(crate::flavor::contract::ProjectionSpec {
        table: "test_flavor.projection",
        index: "test_flavor_projection_owner_tsv_gin",
        overfetch_k: 0,
        band_comparability: crate::flavor::contract::BandComparability::CoreBands,
        // Sidecar-ranked so this fixture tests ONE rule. Under
        // `Projection` the empty band set would trip
        // `ProjectionBandName` as well, and a fixture that can fail two
        // ways proves neither.
        rank_source: crate::flavor::contract::RankSource::SidecarWithProjectionOwner {
            why: "a fixture, not a search surface",
        },
    }),
};
