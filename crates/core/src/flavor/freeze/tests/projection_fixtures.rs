//! Contracts whose search projections disagree — with each other, with the
//! schemas they project, or with what a query can reach.
//!
//! The uniformity fixtures are all built from one function with a single
//! argument changed, so a fixture cannot accidentally trip an earlier check
//! and pass for the one it names.

use super::fixtures::{FIXTURE_FLAVOR, contract, state_surface};
use crate::SearchProjectionColumnKind;
use crate::flavor::contract::{
    EmbeddingRecipe, EraseRule, ExportRule, FlavorContract, KeyShape, LanguagePolicy,
    ProjectionDecl, Provenance, SchemaContract, SchemaRef, SearchProjectionDecl, SubstringArm,
    Surface, TransferRule, WeightedField,
};
use crate::verbs::schema::PayloadKind;

/// One projected schema for a uniformity fixture, differing from its
/// twin in exactly ONE property.
///
/// `validate_contract_projection` checks three properties in order —
/// language, bands, weight array — and returns on the first. A fixture
/// that differs in two of them proves only the earlier one, which is why
/// all three fixtures below are built from this one function with a
/// single argument changed.
pub(super) const fn uniformity_schema(
    name: &'static str,
    table: &'static str,
    fields: &'static [WeightedField],
    language: LanguagePolicy,
    bands: &'static [crate::flavor::contract::Band],
) -> SchemaContract {
    SchemaContract {
        id: SchemaRef::new(FIXTURE_FLAVOR, name, 1),
        kind: PayloadKind::Fact,
        sidecar_table: Some(table),
        search: SearchProjectionDecl::Projected {
            fields,
            tag_column: None,
            language,
            bands,
            substring: SubstringArm::Off,
        },
        embedding: EmbeddingRecipe::Never {
            why: "a fixture, not a memory",
        },
        transfer: TransferRule::StaysOnKey,
        provenance: Provenance::None,
        surfaces: &[],
        natural_key_columns: &[],
    }
}

/// A recipe that names a unit on a schema declaring no sidecar table.
/// The unit resolves against nothing, so the drain is handed no text
/// while the recipe still reads as the embedding arm — jobs filed that
/// nobody can serve.
pub(super) static EMBEDDING_DISAGREEMENT: FlavorContract = uniformity_contract(&[SchemaContract {
    id: SchemaRef::new(FIXTURE_FLAVOR, "thing", 1),
    kind: PayloadKind::Fact,
    sidecar_table: None,
    search: SearchProjectionDecl::None {
        why: "the embedding recipe is what this fixture is about",
    },
    embedding: EmbeddingRecipe::Units(&[crate::flavor::contract::EmbedUnit::stored(
        "embed_text",
        crate::flavor::contract::SLOT_DEFAULT,
    )]),
    transfer: TransferRule::StaysOnKey,
    provenance: Provenance::None,
    surfaces: &[],
    natural_key_columns: &[],
}]);

/// A `Never` arm on an id another layer of the same id embeds.
///
/// The enqueue list is keyed by schema id alone, because the column it
/// is bound against — `memory.schema_id` — carries no kind. An id one
/// registration embeds is therefore embeddable for every registration
/// of that id, so this `Never` is stated and cannot be applied.
pub(super) static EMBEDDING_NEVER_ON_AN_EMBEDDED_ID: FlavorContract = uniformity_contract(&[
    SchemaContract {
        id: SchemaRef::new(FIXTURE_FLAVOR, "thing", 1),
        kind: PayloadKind::Abstraction,
        sidecar_table: Some("test_flavor.thing_v1"),
        search: SearchProjectionDecl::None {
            why: "the embedding recipe is what this fixture is about",
        },
        embedding: EmbeddingRecipe::Units(&[crate::flavor::contract::EmbedUnit::stored(
            "embed_text",
            crate::flavor::contract::SLOT_DEFAULT,
        )]),
        transfer: TransferRule::StaysOnKey,
        provenance: Provenance::None,
        // Declared on this registration only, and required: an embed
        // unit whose sidecar names no memory key is refused before the
        // arm this fixture is about is reached. The sibling below shares
        // the table and declares no surface of its own, which is the
        // `surface_for` rule (one table, one declaring registration).
        surfaces: MEMORY_KEYED_SIDECAR,
        natural_key_columns: &[],
    },
    SchemaContract {
        id: SchemaRef::new(FIXTURE_FLAVOR, "thing", 1),
        kind: PayloadKind::Fact,
        sidecar_table: Some("test_flavor.thing_v1"),
        search: SearchProjectionDecl::None {
            why: "the embedding recipe is what this fixture is about",
        },
        embedding: EmbeddingRecipe::Never {
            why: "the reason the enqueue lane cannot act on",
        },
        transfer: TransferRule::StaysOnKey,
        provenance: Provenance::None,
        surfaces: &[],
        natural_key_columns: &[],
    },
]);

/// `Never` with the reason deleted, wearing the arm that means "embeds".
///
/// `resolve()` yields zero units, so the drain gets nothing — the same
/// outcome as `Never`. But `is_never()` answers `false`, so the enqueue
/// lane reads it as "this embeds" and files jobs against it. Refused on
/// its own account by `validate_contract_schemas`, which runs first;
/// this fixture proves that, rather than the behaviour check catching it
/// in passing.
pub(super) static EMPTY_EMBEDDING_UNITS: FlavorContract = uniformity_contract(&[SchemaContract {
    id: SchemaRef::new(FIXTURE_FLAVOR, "thing", 1),
    kind: PayloadKind::Fact,
    sidecar_table: Some("test_flavor.thing_v1"),
    search: SearchProjectionDecl::None {
        why: "the embedding recipe is what this fixture is about",
    },
    embedding: EmbeddingRecipe::Units(&[]),
    transfer: TransferRule::StaysOnKey,
    provenance: Provenance::None,
    surfaces: &[],
    natural_key_columns: &[],
}]);

/// A contract naming natural key columns the ingest does not read: the
/// registration's list is empty, and the ingest reads the registration.
pub(super) static NATURAL_KEY_DISAGREEMENT: FlavorContract =
    uniformity_contract(&[SchemaContract {
        id: SchemaRef::new(FIXTURE_FLAVOR, "thing", 1),
        kind: PayloadKind::Fact,
        sidecar_table: Some("test_flavor.thing_v1"),
        search: SearchProjectionDecl::None {
            why: "the natural key is what this fixture is about",
        },
        embedding: EmbeddingRecipe::Never {
            why: "a fixture, not a memory",
        },
        transfer: TransferRule::StaysOnKey,
        provenance: Provenance::None,
        surfaces: &[],
        natural_key_columns: &["thing_key"],
    }]);

/// The `RankSource::Projection` wrapper the three uniformity fixtures
/// share: one statement serves the whole flavor, which is what makes
/// disagreement between its schemas a boot refusal.
pub(super) const fn uniformity_contract(schemas: &'static [SchemaContract]) -> FlavorContract {
    FlavorContract {
        flavor_id: FIXTURE_FLAVOR,
        ordinal: 7,
        schemas,
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
            rank_source: crate::flavor::contract::RankSource::Projection,
        }),
    }
}

/// The sidecar surface a projected schema needs: keyed on the memory
/// `t`, which is the column the projection generator reads.
pub(super) static MEMORY_KEYED_SIDECAR: &[Surface] = &[state_surface(
    "test_flavor.thing_v1",
    KeyShape::MemoryT { column: "t" },
    EraseRule::ByKey,
    ExportRule::Rows,
)];

/// The same sidecar keyed on something the generator cannot spell.
pub(super) static CUSTOM_KEYED_SIDECAR: &[Surface] = &[state_surface(
    "test_flavor.thing_v1",
    KeyShape::Custom(&["thing_id"]),
    EraseRule::Never {
        why: "the projection key is what this fixture is about",
    },
    ExportRule::Excluded {
        why: "the projection key is what this fixture is about",
    },
)];

/// [`uniformity_schema`]'s twin for the REACHABILITY fixtures: the tag
/// column and the sidecar's surfaces are the parameters, and everything
/// else is a shape the earlier projection rules accept, so only the arm
/// under test is left to fire.
pub(super) const fn reachability_schema(
    tag_column: Option<&'static str>,
    surfaces: &'static [Surface],
) -> SchemaContract {
    SchemaContract {
        id: SchemaRef::new(FIXTURE_FLAVOR, "thing", 1),
        kind: PayloadKind::Fact,
        sidecar_table: Some("test_flavor.thing_v1"),
        search: SearchProjectionDecl::Projected {
            fields: ONE_LEVEL,
            tag_column,
            language: LanguagePolicy::Pinned("simple"),
            bands: FIXTURE_BANDS,
            substring: SubstringArm::Off,
        },
        embedding: EmbeddingRecipe::Never {
            why: "a fixture, not a memory",
        },
        transfer: TransferRule::StaysOnKey,
        provenance: Provenance::None,
        surfaces,
        natural_key_columns: &[],
    }
}

/// [`uniformity_contract`]'s twin for the band-comparability arm: the
/// same `RankSource::Projection` claim, over bands the flavor says are
/// not core's.
pub(super) const fn divergent_contract(schemas: &'static [SchemaContract]) -> FlavorContract {
    FlavorContract {
        flavor_id: FIXTURE_FLAVOR,
        ordinal: 7,
        schemas,
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
            band_comparability: crate::flavor::contract::BandComparability::Divergent {
                why: "the fixture's whole subject",
            },
            rank_source: crate::flavor::contract::RankSource::Projection,
        }),
    }
}

/// A non-core corpus that pays a projection row and a GIN entry per
/// write and that no request shape scans: unscoped search stays on
/// flavor #0's sidecars, and the tag-filtered query that would reach a
/// flavor skips a projection declaring no `tag_column`.
pub(super) static PROJECTION_NO_QUERY_REACHES: FlavorContract =
    uniformity_contract(&[reachability_schema(None, MEMORY_KEYED_SIDECAR)]);

/// The same corpus with the tag gate satisfied and the OTHER gate
/// closed: `RankSource::Projection` says core's renderer serves this
/// flavor, and `Divergent` says core's merge may not compare its
/// scores. Nothing is left to scan it.
pub(super) static PROJECTION_OUTSIDE_THE_MERGE: FlavorContract =
    divergent_contract(&[reachability_schema(Some("tags"), MEMORY_KEYED_SIDECAR)]);

/// The reachable declaration, which must freeze: a tag-filtered request
/// reaches it, and its flavor claims a score the merge can compare.
pub(super) static PROJECTION_A_QUERY_REACHES: FlavorContract =
    uniformity_contract(&[reachability_schema(Some("tags"), MEMORY_KEYED_SIDECAR)]);

/// A projected schema whose sidecar declares no surface at all: the
/// generator has no `MemoryT` column to key the projection row on.
pub(super) static PROJECTED_SIDECAR_UNDECLARED: FlavorContract =
    uniformity_contract(&[reachability_schema(Some("tags"), &[])]);

/// …and the same sidecar declared under a key the generator cannot
/// spell a projection statement from.
pub(super) static PROJECTED_SIDECAR_WRONG_KEY: FlavorContract =
    uniformity_contract(&[reachability_schema(Some("tags"), CUSTOM_KEYED_SIDECAR)]);

/// A schema that EMBEDS rather than searches, over the same
/// custom-keyed sidecar.
///
/// The embedding lane's twin of the two fixtures above, and it must not
/// be projected: a projected schema is refused by
/// `validate_projection_declarations` first, which would mask the arm
/// under test. `EmbeddingRecipe::resolve` binds a unit to its sidecar
/// TABLE and never sees a `Surface`, so nothing but this refusal keeps
/// the drain's text read off a naming convention.
pub(super) const fn embedding_schema(surfaces: &'static [Surface]) -> SchemaContract {
    SchemaContract {
        id: SchemaRef::new(FIXTURE_FLAVOR, "thing", 1),
        kind: PayloadKind::Fact,
        sidecar_table: Some("test_flavor.thing_v1"),
        search: SearchProjectionDecl::None {
            why: "the embed unit's key column is what this fixture is about",
        },
        embedding: EmbeddingRecipe::Units(EMBED_BODY),
        transfer: TransferRule::StaysOnKey,
        provenance: Provenance::None,
        surfaces,
        natural_key_columns: &[],
    }
}

pub(super) static EMBED_BODY: &[crate::flavor::contract::EmbedUnit] =
    &[crate::flavor::contract::EmbedUnit::stored(
        "body",
        crate::flavor::contract::SLOT_DEFAULT,
    )];

/// A flavor whose only schema embeds, so it declares no projection.
pub(super) const fn embedding_contract(schemas: &'static [SchemaContract]) -> FlavorContract {
    FlavorContract {
        flavor_id: FIXTURE_FLAVOR,
        ordinal: 7,
        schemas,
        state_surfaces: &[],
        scopes: &[],
        kernel_surfaces: &[],
        tools: &[],
        resources: &[],
        bespoke_erase_legs: &[],
        bespoke_transfer_legs: &[],
        projection: ProjectionDecl::None {
            why: "the fixture embeds; it does not search",
        },
    }
}

/// An embed unit over a sidecar no surface keys on the memory `t`.
pub(super) static EMBEDDED_SIDECAR_WRONG_KEY: FlavorContract =
    embedding_contract(&[embedding_schema(CUSTOM_KEYED_SIDECAR)]);

/// The same schema over the sidecar keyed as the drain needs it. Freeze
/// accepts this one, which is what makes the refusal above a statement
/// about the KEY rather than about the fixture.
pub(super) static EMBEDDED_SIDECAR_RIGHT_KEY: FlavorContract =
    embedding_contract(&[embedding_schema(MEMORY_KEYED_SIDECAR)]);

/// One weight level: no array at all.
pub(super) static ONE_LEVEL: &[WeightedField] = &[WeightedField {
    column: "a",
    kind: SearchProjectionColumnKind::Text,
    weight: 1.0,
}];

/// Two levels: an array [`ONE_LEVEL`] does not have.
pub(super) static TWO_LEVELS: &[WeightedField] = &[
    WeightedField {
        column: "a",
        kind: SearchProjectionColumnKind::Text,
        weight: 1.0,
    },
    WeightedField {
        column: "b",
        kind: SearchProjectionColumnKind::Text,
        weight: 2.0,
    },
];

/// Two projected schemas under one `RankSource::Projection` flavor
/// agreeing on language and bands and DISAGREEING on weight levels.
///
/// One statement serves both, and it reads the weight array off the
/// first participating schema — so without this check the second
/// schema's vector would be ranked with an array that describes a
/// document it is not scoring.
pub(super) static WEIGHTS_NOT_UNIFORM: FlavorContract = uniformity_contract(&[
    uniformity_schema(
        "thing",
        "test_flavor.thing_v1",
        ONE_LEVEL,
        LanguagePolicy::Pinned("simple"),
        FIXTURE_BANDS,
    ),
    uniformity_schema(
        "other",
        "test_flavor.other_v1",
        TWO_LEVELS,
        LanguagePolicy::Pinned("simple"),
        FIXTURE_BANDS,
    ),
]);

/// …and disagreeing on the LEXICAL CONFIGURATION, which one statement
/// can spell exactly once.
///
/// Without its own fixture this arm was decorative: deleting it left the
/// whole workspace green, because the weight fixture agreed on language
/// and never reached it.
pub(super) static LANGUAGE_NOT_UNIFORM: FlavorContract = uniformity_contract(&[
    uniformity_schema(
        "thing",
        "test_flavor.thing_v1",
        ONE_LEVEL,
        LanguagePolicy::Pinned("simple"),
        FIXTURE_BANDS,
    ),
    uniformity_schema(
        "other",
        "test_flavor.other_v1",
        ONE_LEVEL,
        LanguagePolicy::Pinned("english"),
        FIXTURE_BANDS,
    ),
]);

/// …and disagreeing on the SCORE WINDOWS, which is what makes two
/// schemas' scores comparable inside one page.
///
/// [`SHIFTED_BANDS`] carries the same three names in the same order, so
/// the band-NAME rule cannot fire and only the uniformity arm is left.
pub(super) static BANDS_NOT_UNIFORM: FlavorContract = uniformity_contract(&[
    uniformity_schema(
        "thing",
        "test_flavor.thing_v1",
        ONE_LEVEL,
        LanguagePolicy::Pinned("simple"),
        FIXTURE_BANDS,
    ),
    uniformity_schema(
        "other",
        "test_flavor.other_v1",
        ONE_LEVEL,
        LanguagePolicy::Pinned("simple"),
        SHIFTED_BANDS,
    ),
]);

/// Core's own windows, which is what makes `CoreBands` above legal and
/// keeps the fixtures from tripping the band-name rule.
pub(super) static FIXTURE_BANDS: &[crate::flavor::contract::Band] = &[
    crate::flavor::flavor0::BAND_EXACT,
    crate::flavor::flavor0::BAND_RESCUE,
    crate::flavor::flavor0::BAND_SUBSTRING,
];

/// The same three names inside `[0, 1]`, at a different exact floor.
/// Staying inside core's window is the point: a band that left it would
/// trip `ProjectionBandOutsideCoreWindow` instead.
pub(super) static SHIFTED_BANDS: &[crate::flavor::contract::Band] = &[
    crate::flavor::contract::Band {
        name: crate::flavor::contract::BAND_NAME_EXACT,
        floor: 0.60,
        ceiling: 1.00,
        normalization: crate::flavor::flavor0::BAND_EXACT.normalization,
    },
    crate::flavor::flavor0::BAND_RESCUE,
    crate::flavor::flavor0::BAND_SUBSTRING,
];

/// Five distinct relative weights on one projection unit. The
/// declaration is free of `PostgreSQL`'s four-class limit right up to
/// the moment the generator has to emit `setweight`, and this is that
/// moment.
pub(super) static TOO_MANY_WEIGHT_LEVELS: FlavorContract = contract(
    7,
    &[SchemaContract {
        id: SchemaRef::new(FIXTURE_FLAVOR, "thing", 1),
        kind: PayloadKind::CitedObject,
        sidecar_table: Some("test_flavor.thing_v1"),
        search: SearchProjectionDecl::Projected {
            fields: &[
                WeightedField {
                    column: "a",
                    kind: SearchProjectionColumnKind::Text,
                    weight: 5.0,
                },
                WeightedField {
                    column: "b",
                    kind: SearchProjectionColumnKind::Text,
                    weight: 4.0,
                },
                WeightedField {
                    column: "c",
                    kind: SearchProjectionColumnKind::Text,
                    weight: 3.0,
                },
                WeightedField {
                    column: "d",
                    kind: SearchProjectionColumnKind::Text,
                    weight: 2.0,
                },
                WeightedField {
                    column: "e",
                    kind: SearchProjectionColumnKind::Text,
                    weight: 1.0,
                },
            ],
            tag_column: None,
            language: LanguagePolicy::Pinned("simple"),
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
    &[],
    &[],
);
