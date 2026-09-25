//! One registry per contract cross-check, each shaped to trip exactly that
//! check and nothing else.
//!
//! The refusals are table-driven on purpose: an unpinned check is one a
//! refactor can delete without a single test going red. Each refusal is
//! paired with the shape it must NOT refuse, because a refusal with no
//! accepted twin proves only that freeze can fail.

use super::fixtures::{
    CITATION_WITH_A_SIDECAR, DECLARES_A_RESOURCE, DUPLICATE_ORDINAL, EMPTY, FIXTURE_FLAVOR,
    FOREIGN_SCHEMA, TOOL_ACTIONS_DISAGREE, TOOL_IDEMPOTENCE_DISAGREES, UNENFORCED_REFUSAL,
    UNREGISTERED_SCHEMA, UNREGISTERED_TOOL, register_fixture_schema,
    register_fixture_schema_of_kind, register_fixture_tool, register_uniformity_schemas,
    registration_without_a_contract,
};
use super::leg_fixtures::{
    BESPOKE_LEG_FOR_NOTHING, BESPOKE_LEG_OVER_A_CASCADE, BESPOKE_TRANSFER_LEG_FOR_NOTHING,
    BESPOKE_TRANSFER_LEG_OVER_A_NON_MOVE, FOLLOW_WITH_NOTHING_TO_SET,
    HOST_MANAGED_OWNERLESS_EXPORTS, PER_ROW_ON_THE_WRONG_COLUMN, UNDELETABLE_SURFACE,
    UNFORGETTABLE_SURFACE, UNMOVABLE_SURFACE, UNREACHABLE_ALLOWLIST_EXPORT_SURFACE,
    UNREACHABLE_EXPORT_SURFACE,
};
use super::projection_fixtures::{
    BANDS_NOT_UNIFORM, EMBEDDED_SIDECAR_RIGHT_KEY, EMBEDDED_SIDECAR_WRONG_KEY,
    EMBEDDING_DISAGREEMENT, EMBEDDING_NEVER_ON_AN_EMBEDDED_ID, EMPTY_EMBEDDING_UNITS,
    LANGUAGE_NOT_UNIFORM, NATURAL_KEY_DISAGREEMENT, PROJECTED_SIDECAR_UNDECLARED,
    PROJECTED_SIDECAR_WRONG_KEY, PROJECTION_A_QUERY_REACHES, PROJECTION_NO_QUERY_REACHES,
    PROJECTION_OUTSIDE_THE_MERGE, TOO_MANY_WEIGHT_LEVELS, WEIGHTS_NOT_UNIFORM,
};
use crate::verbs::schema::PayloadKind;
use crate::{FlavorRegistry, FlavorRegistryError};

/// Register a fixture contract's schema, the way the typed
/// `add_*_schema` methods would.
///
/// `validate_contract_schemas` runs BEFORE
/// `validate_contract_projection`, so a fixture whose subject is a
/// PROJECTION rule has to be registered to reach it. Registering is two
/// lines; reordering the two validators instead would change which error
/// every genuinely broken contract reports at boot.
///
/// A typed registration needs its ingress entry too, or
/// `SchemaIngressMismatch` fires first and the fixture proves that
/// instead.
/// A listenable schema promises consumers a resolvable `dataschema`.
/// A registration that cannot keep that promise is a build error, not a
/// runtime surprise on the first admission.
#[test]
fn a_listenable_schema_without_a_json_schema_fails_the_build() {
    let mut registry = FlavorRegistry::new();
    registry.add_contract_or_panic_for_tests(&crate::test_fixtures::SCHEMALESS_PROBE_FLAVOR);
    registry
        .add_fact_schema_or_panic_for_tests::<crate::test_fixtures::SchemalessListenableProbeV1>();
    let err = registry
        .try_freeze()
        .expect_err("a listenable schema with no json_schema() must not freeze");
    assert!(
        matches!(
            err,
            FlavorRegistryError::ListenableWithoutSchema { ref schema_id }
                if schema_id.as_str() == "probe/schemaless-v1"
        ),
        "unexpected error: {err:?}"
    );
}

/// Every contract cross-check, each with a registry shaped to trip it
/// and nothing else.
///
/// These are the checks that make "everything is a flavor" structural.
/// An unpinned check is one a refactor can delete without a single test
/// going red — and the resource rejection in particular is five lines
/// standing for the whole resources-are-substrate rule.
#[test]
// One line per cross-check plus its fixture reference. Splitting it
// would put half the checks in a second function to forget one in.
#[allow(clippy::too_many_lines)]
fn each_contract_cross_check_rejects_its_own_shape() {
    #[allow(clippy::type_complexity)]
    let cases: Vec<(
        &'static str,
        fn(&mut FlavorRegistry),
        fn(&FlavorRegistryError) -> bool,
    )> = vec![
        (
            "a flavor other than #0 declares a proxima:// resource",
            |registry| registry.contracts.push(&DECLARES_A_RESOURCE),
            |err| {
                matches!(
                    err,
                    FlavorRegistryError::ResourcesNotPermitted { flavor_id }
                        if *flavor_id == FIXTURE_FLAVOR
                )
            },
        ),
        (
            "two contracts claim the same ordinal",
            |registry| registry.contracts.push(&DUPLICATE_ORDINAL),
            |err| {
                matches!(
                    err,
                    FlavorRegistryError::DuplicateFlavorOrdinal { ordinal: 0, .. }
                )
            },
        ),
        (
            "contracts were registered but core's is not among them",
            |registry| {
                registry.contracts.clear();
                registry.contracts.push(&EMPTY);
            },
            |err| matches!(err, FlavorRegistryError::MissingCoreContract),
        ),
        (
            "a contract entry's schema id carries another flavor's prefix",
            |registry| registry.contracts.push(&FOREIGN_SCHEMA),
            |err| matches!(err, FlavorRegistryError::ContractSchemaPrefix { .. }),
        ),
        (
            "a NotTransferable schema names no enforcement site",
            |registry| registry.contracts.push(&UNENFORCED_REFUSAL),
            |err| matches!(err, FlavorRegistryError::UnenforcedTransferRefusal { .. }),
        ),
        (
            "the contract declares a schema nothing registered",
            |registry| registry.contracts.push(&UNREGISTERED_SCHEMA),
            |err| matches!(err, FlavorRegistryError::ContractSchemaNotRegistered { .. }),
        ),
        (
            "a schema was registered under a flavor that does not declare it",
            |registry| {
                registry.schemas.push(registration_without_a_contract());
                registry.contracts.push(&EMPTY);
            },
            |err| matches!(err, FlavorRegistryError::SchemaWithoutContract { .. }),
        ),
        (
            "one projection unit declares more weight levels than PG has classes",
            |registry| registry.contracts.push(&TOO_MANY_WEIGHT_LEVELS),
            |err| {
                matches!(
                    err,
                    FlavorRegistryError::ProjectionWeightLevels {
                        levels: 5,
                        classes: 4,
                        ..
                    }
                )
            },
        ),
        (
            "a citation payload declares a sidecar the blob remap cannot reach",
            |registry| registry.contracts.push(&CITATION_WITH_A_SIDECAR),
            |err| {
                matches!(
                    err,
                    FlavorRegistryError::CitationSidecarNotRemappable {
                        table: "test_flavor.thing_v1",
                        ..
                    }
                )
            },
        ),
        (
            "a PerRow policy names a column the projection table does not have",
            |registry| registry.contracts.push(&PER_ROW_ON_THE_WRONG_COLUMN),
            |err| {
                matches!(
                    err,
                    FlavorRegistryError::ProjectionLanguageColumn {
                        declared: "row_config",
                        projection_column: Some("lexical_language"),
                        ..
                    }
                )
            },
        ),
        (
            "two projection-ranked schemas disagree about the lexical configuration",
            |registry| {
                register_uniformity_schemas(registry);
                registry.contracts.push(&LANGUAGE_NOT_UNIFORM);
            },
            |err| {
                matches!(
                    err,
                    FlavorRegistryError::ProjectionRenderNotUniform {
                        property: "language",
                        ..
                    }
                )
            },
        ),
        (
            "two projection-ranked schemas disagree about the score windows",
            |registry| {
                register_uniformity_schemas(registry);
                registry.contracts.push(&BANDS_NOT_UNIFORM);
            },
            |err| {
                matches!(
                    err,
                    FlavorRegistryError::ProjectionRenderNotUniform {
                        property: "bands",
                        ..
                    }
                )
            },
        ),
        (
            "two projection-ranked schemas disagree about the ts_rank weight array",
            |registry| {
                register_uniformity_schemas(registry);
                registry.contracts.push(&WEIGHTS_NOT_UNIFORM);
            },
            |err| {
                matches!(
                    err,
                    FlavorRegistryError::ProjectionRenderNotUniform {
                        property: "rank_weights",
                        ..
                    }
                )
            },
        ),
        (
            "a surface declares an erase no leg can perform",
            |registry| registry.contracts.push(&UNDELETABLE_SURFACE),
            |err| {
                matches!(
                    err,
                    FlavorRegistryError::UndeletableSurface {
                        table: "test_flavor.thing_v1",
                        ..
                    }
                )
            },
        ),
        (
            "a surface declares a transfer no leg can perform",
            |registry| registry.contracts.push(&UNMOVABLE_SURFACE),
            |err| {
                matches!(
                    err,
                    FlavorRegistryError::UnmovableSurface {
                        table: "test_flavor.thing_v1",
                        ..
                    }
                )
            },
        ),
        (
            "a surface declares Follow with no owner column to set",
            |registry| registry.contracts.push(&FOLLOW_WITH_NOTHING_TO_SET),
            |err| {
                matches!(
                    err,
                    FlavorRegistryError::UnmovableSurface {
                        table: "test_flavor.thing_v1",
                        ..
                    }
                )
            },
        ),
        (
            "a bespoke transfer leg names a table the flavor does not declare",
            |registry| registry.contracts.push(&BESPOKE_TRANSFER_LEG_FOR_NOTHING),
            |err| {
                matches!(
                    err,
                    FlavorRegistryError::BespokeTransferLegMismatch {
                        table: "test_flavor.gone_v1",
                        ..
                    }
                )
            },
        ),
        (
            "a bespoke transfer leg claims a surface nothing moves",
            |registry| {
                registry
                    .contracts
                    .push(&BESPOKE_TRANSFER_LEG_OVER_A_NON_MOVE);
            },
            |err| {
                matches!(
                    err,
                    FlavorRegistryError::BespokeTransferLegMismatch {
                        table: "test_flavor.thing_v1",
                        ..
                    }
                )
            },
        ),
        (
            "a surface declares a forget that reaches none of its rows",
            |registry| registry.contracts.push(&UNFORGETTABLE_SURFACE),
            |err| {
                matches!(
                    err,
                    FlavorRegistryError::UnforgettableSurface {
                        table: "test_flavor.thing_v1",
                        ..
                    }
                )
            },
        ),
        (
            "a bespoke erase leg names a table the flavor does not declare",
            |registry| registry.contracts.push(&BESPOKE_LEG_FOR_NOTHING),
            |err| {
                matches!(
                    err,
                    FlavorRegistryError::BespokeEraseLegMismatch {
                        table: "test_flavor.gone_v1",
                        ..
                    }
                )
            },
        ),
        (
            "a bespoke erase leg claims a surface a constraint removes",
            |registry| registry.contracts.push(&BESPOKE_LEG_OVER_A_CASCADE),
            |err| {
                matches!(
                    err,
                    FlavorRegistryError::BespokeEraseLegMismatch {
                        table: "test_flavor.thing_v1",
                        ..
                    }
                )
            },
        ),
        (
            "an exportable surface has neither an owner column nor a key with a home",
            |registry| registry.contracts.push(&UNREACHABLE_EXPORT_SURFACE),
            |err| {
                matches!(
                    err,
                    FlavorRegistryError::UnreachableExportSurface {
                        table: "test_flavor.thing_v1",
                        ..
                    }
                )
            },
        ),
        (
            "an allowlisted export has neither an owner column nor a key with a home",
            |registry| {
                registry
                    .contracts
                    .push(&UNREACHABLE_ALLOWLIST_EXPORT_SURFACE);
            },
            |err| {
                matches!(
                    err,
                    FlavorRegistryError::UnreachableExportSurface {
                        table: "test_flavor.thing_v1",
                        ..
                    }
                )
            },
        ),
        (
            "an embedding recipe names a unit the schema has no table to resolve it against",
            |registry| {
                register_fixture_schema(registry, "thing", "test_flavor.thing_v1");
                registry.contracts.push(&EMBEDDING_DISAGREEMENT);
            },
            |err| {
                matches!(
                    err,
                    FlavorRegistryError::EmbeddabilityDisagreement {
                        recipe_is_never: false,
                        machinery_embeds: false,
                        ..
                    }
                )
            },
        ),
        (
            // The costly direction; the case above is its mirror.
            "a recipe says Never on an id another registration embeds",
            |registry| {
                register_fixture_schema_of_kind(
                    registry,
                    "thing",
                    "test_flavor.thing_v1",
                    PayloadKind::Abstraction,
                );
                register_fixture_schema(registry, "thing", "test_flavor.thing_v1");
                registry.contracts.push(&EMBEDDING_NEVER_ON_AN_EMBEDDED_ID);
            },
            |err| {
                matches!(
                    err,
                    FlavorRegistryError::EmbeddabilityDisagreement {
                        recipe_is_never: true,
                        machinery_embeds: true,
                        ..
                    }
                )
            },
        ),
        (
            // `validate_contract_schemas` runs first, so this is refused
            // on its own account rather than as a behaviour disagreement.
            "a recipe declares an empty unit list, which is Never without the reason",
            |registry| {
                register_fixture_schema(registry, "thing", "test_flavor.thing_v1");
                registry.contracts.push(&EMPTY_EMBEDDING_UNITS);
            },
            |err| matches!(err, FlavorRegistryError::EmptyEmbeddingUnits { .. }),
        ),
        (
            "a contract names natural key columns the ingest does not read",
            |registry| {
                register_fixture_schema(registry, "thing", "test_flavor.thing_v1");
                registry.contracts.push(&NATURAL_KEY_DISAGREEMENT);
            },
            |err| matches!(err, FlavorRegistryError::NaturalKeyDisagreement { .. }),
        ),
        (
            "the contract names an MCP tool nothing registered",
            |registry| registry.contracts.push(&UNREGISTERED_TOOL),
            |err| {
                matches!(
                    err,
                    FlavorRegistryError::ContractToolNotRegistered { name, .. }
                        if *name == "test_flavor_absent"
                )
            },
        ),
        (
            "the contract's action list is not the dispatcher's",
            |registry| {
                register_fixture_tool(registry, false);
                registry.contracts.push(&TOOL_ACTIONS_DISAGREE);
            },
            |err| {
                matches!(
                    err,
                    FlavorRegistryError::ToolActionsDisagreement { name, .. }
                        if *name == "test_flavor_flat"
                )
            },
        ),
        (
            "a linked flavor declares no contract at all",
            |registry| {
                registry.flavors.push(crate::flavor::FlavorDescriptor {
                    flavor_id: FIXTURE_FLAVOR.to_owned(),
                    display_name: "Fixture".to_owned(),
                    package_version: "0.0.0".to_owned(),
                    author: None,
                    provenance: crate::flavor::FlavorProvenance::Builtin,
                });
            },
            |err| {
                matches!(
                    err,
                    FlavorRegistryError::UnclaimedRegistration { flavor_id }
                        if flavor_id == FIXTURE_FLAVOR
                )
            },
        ),
        (
            // Not a skip. An empty contracts list is every registration
            // undeclared, and core is the flavor that cannot be the
            // missing one.
            "nothing declares a contract, so the registry declares no core",
            |registry| registry.contracts.clear(),
            |err| matches!(err, FlavorRegistryError::MissingCoreContract),
        ),
        (
            "a projected schema's sidecar declares no surface to key the projection on",
            |registry| {
                register_fixture_schema(registry, "thing", "test_flavor.thing_v1");
                registry.contracts.push(&PROJECTED_SIDECAR_UNDECLARED);
            },
            |err| {
                matches!(
                    err,
                    FlavorRegistryError::ProjectedSidecarNotMemoryKeyed {
                        table: "test_flavor.thing_v1",
                        ..
                    }
                )
            },
        ),
        (
            "a projected schema's sidecar is keyed on something the generator cannot spell",
            |registry| {
                register_fixture_schema(registry, "thing", "test_flavor.thing_v1");
                registry.contracts.push(&PROJECTED_SIDECAR_WRONG_KEY);
            },
            |err| {
                matches!(
                    err,
                    FlavorRegistryError::ProjectedSidecarNotMemoryKeyed {
                        table: "test_flavor.thing_v1",
                        ..
                    }
                )
            },
        ),
        (
            "an embed unit's sidecar declares no surface to key the text read on",
            |registry| {
                register_fixture_schema(registry, "thing", "test_flavor.thing_v1");
                registry.contracts.push(&EMBEDDED_SIDECAR_WRONG_KEY);
            },
            |err| {
                matches!(
                    err,
                    FlavorRegistryError::EmbeddedSidecarNotMemoryKeyed {
                        table: "test_flavor.thing_v1",
                        ..
                    }
                )
            },
        ),
        (
            "a non-core projection declares no tag column, so no query shape reaches it",
            |registry| {
                register_fixture_schema(registry, "thing", "test_flavor.thing_v1");
                registry.contracts.push(&PROJECTION_NO_QUERY_REACHES);
            },
            |err| {
                matches!(
                    err,
                    FlavorRegistryError::UnreachableSearchProjection { why, .. }
                        if why.contains("tag_column")
                )
            },
        ),
        (
            "a non-core projection's flavor declares a score the merge cannot compare",
            |registry| {
                register_fixture_schema(registry, "thing", "test_flavor.thing_v1");
                registry.contracts.push(&PROJECTION_OUTSIDE_THE_MERGE);
            },
            |err| {
                matches!(
                    err,
                    FlavorRegistryError::UnreachableSearchProjection { why, .. }
                        if why.contains("CoreBands")
                )
            },
        ),
        (
            "the contract claims an idempotence the registration denies",
            |registry| {
                register_fixture_tool(registry, false);
                registry.contracts.push(&TOOL_IDEMPOTENCE_DISAGREES);
            },
            |err| {
                matches!(
                    err,
                    FlavorRegistryError::ToolIdempotenceDisagreement {
                        declared: true,
                        resolved: false,
                        ..
                    }
                )
            },
        ),
    ];

    for (shape, break_it, expected) in cases {
        let mut registry = FlavorRegistry::new();
        break_it(&mut registry);
        let Err(err) = registry.try_freeze() else {
            panic!("freeze accepted a registry where {shape}");
        };
        assert!(expected(&err), "{shape}: freeze reported {err} instead");
    }
}

/// The other half of each new refusal: the shape it must NOT refuse.
///
/// A refusal with no accepted twin proves only that freeze can fail.
/// Both cases here are one edit away from the rejected fixtures the table
/// draws on —
/// a contract registered beside the descriptor; a `tag_column` and a
/// memory-keyed sidecar surface declared beside the projection — so
/// what the checks cost a correct flavor is pinned alongside what they
/// catch.
#[test]
fn the_shapes_the_new_refusals_must_accept_freeze() {
    let mut declared = FlavorRegistry::new();
    declared.flavors.push(crate::flavor::FlavorDescriptor {
        flavor_id: FIXTURE_FLAVOR.to_owned(),
        display_name: "Fixture".to_owned(),
        package_version: "0.0.0".to_owned(),
        author: None,
        provenance: crate::flavor::FlavorProvenance::Builtin,
    });
    declared.contracts.push(&EMPTY);
    if let Err(err) = declared.try_freeze() {
        panic!("a linked flavor that declares a contract must freeze: {err}");
    }

    let mut reachable = FlavorRegistry::new();
    register_fixture_schema(&mut reachable, "thing", "test_flavor.thing_v1");
    reachable.contracts.push(&PROJECTION_A_QUERY_REACHES);
    if let Err(err) = reachable.try_freeze() {
        panic!("a tag-filtered request reaches this projection: {err}");
    }

    // The embedding lane's twin: the same embed unit over a sidecar
    // whose surface DOES key on the memory `t` freezes, and the unit
    // the drain reads carries that column rather than a convention.
    let mut embedded = FlavorRegistry::new();
    register_fixture_schema(&mut embedded, "thing", "test_flavor.thing_v1");
    embedded.contracts.push(&EMBEDDED_SIDECAR_RIGHT_KEY);
    let frozen = match embedded.try_freeze() {
        Ok(frozen) => frozen,
        Err(err) => panic!("an embed unit over a memory-keyed sidecar must freeze: {err}"),
    };
    let unit = frozen
        .embed_units()
        .iter()
        .find(|unit| unit.sidecar_table == "test_flavor.thing_v1")
        .expect("the fixture's recipe resolves one unit");
    assert_eq!(
        unit.key_column, "t",
        "the unit carries the column the surface declares, for the drain to filter on"
    );

    let mut lifecycle_owned_exports = FlavorRegistry::new();
    lifecycle_owned_exports
        .flavors
        .push(crate::flavor::FlavorDescriptor {
            flavor_id: FIXTURE_FLAVOR.to_owned(),
            display_name: "Fixture".to_owned(),
            package_version: "0.0.0".to_owned(),
            author: None,
            provenance: crate::flavor::FlavorProvenance::Builtin,
        });
    lifecycle_owned_exports
        .contracts
        .push(&HOST_MANAGED_OWNERLESS_EXPORTS);
    if let Err(err) = lifecycle_owned_exports.try_freeze() {
        panic!("host lifecycle can export its ownerless Rows and Allowlist surfaces: {err}");
    }
}
