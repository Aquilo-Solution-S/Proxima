//! ── Declared lifecycle scopes ────────────────────────────────────
//!
//! The two halves of a scope declaration are registered in two
//! different places — the kind on the PAYLOAD, the registry table on the
//! CONTRACT — and freeze is the only moment that can see both. These
//! fixtures pin each half against the other.

use super::fixtures::FIXTURE_FLAVOR;
use crate::flavor::contract::{FlavorContract, ProjectionDecl};
use crate::{FlavorRegistry, FlavorRegistryError};

const FIXTURE_SCOPE: crate::ScopeKind = crate::ScopeKind::new("fixture-scope");

const fn scope_decl(registry_table: &'static str) -> crate::ScopeDecl {
    crate::ScopeDecl {
        kind: FIXTURE_SCOPE,
        registry_table,
        id_column: "thing_id",
        owner_kind_column: "owner_kind",
        owner_id_column: "owner_id",
    }
}

const fn scoped_contract(
    flavor_id: &'static str,
    ordinal: u16,
    scopes: &'static [crate::ScopeDecl],
) -> FlavorContract {
    FlavorContract {
        flavor_id,
        ordinal,
        schemas: &[],
        state_surfaces: &[],
        scopes,
        kernel_surfaces: &[],
        tools: &[],
        resources: &[],
        bespoke_erase_legs: &[],
        bespoke_transfer_legs: &[],
        projection: ProjectionDecl::None {
            why: "a fixture registry has no schema that is a search surface",
        },
    }
}

static DECLARES_THE_SCOPE: FlavorContract =
    scoped_contract(FIXTURE_FLAVOR, 7, &[scope_decl("test_flavor.things")]);
static ALSO_DECLARES_THE_SCOPE: FlavorContract = scoped_contract(
    "second-test-flavor",
    8,
    &[scope_decl("second_flavor.things")],
);
static DECLARES_AN_UNQUALIFIED_TABLE: FlavorContract =
    scoped_contract(FIXTURE_FLAVOR, 7, &[scope_decl("things")]);

/// A payload that names a scope. Nothing else about it matters — the
/// registration is the subject.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct ScopedFixtureFact {
    thing_id: uuid::Uuid,
}

impl crate::FactPayload for ScopedFixtureFact {
    const SCHEMA_ID: &'static str = "test-flavor/scoped-thing";
    const SCHEMA_VERSION: u32 = 1;
    const SCOPE_KIND: Option<crate::ScopeKind> = Some(FIXTURE_SCOPE);

    fn scope_id(&self) -> Option<uuid::Uuid> {
        Some(self.thing_id)
    }

    fn receipt_key(&self) -> Vec<u8> {
        let mut key = crate::PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_str("thing_id", &self.thing_id.to_string());
        key.finish()
    }

    fn render(&self) -> String {
        self.thing_id.to_string()
    }
}

/// The payload half: registering a scoped payload records the kind
/// freeze will then demand a declaration for.
///
/// Without this, the refusal below would only prove that a hand-pushed
/// entry is refused — and the entry that matters is the one a real
/// `SCOPE_KIND` produces.
#[test]
fn registering_a_scoped_payload_records_the_kind_it_names() {
    let mut registry = FlavorRegistry::new();
    registry
        .try_add_fact_schema::<ScopedFixtureFact>()
        .expect("a fixture schema registers");
    assert!(
        registry.scoped_schemas.iter().any(|(schema_id, kind)| {
            schema_id.as_str() == <ScopedFixtureFact as crate::FactPayload>::SCHEMA_ID
                && *kind == FIXTURE_SCOPE
        }),
        "a payload's SCOPE_KIND must reach the registry, or freeze has nothing to check"
    );
}

/// A payload names a scope no linked contract declares.
///
/// This is the failure the whole mechanism turns on: storage GENERATES
/// the fence key and the liveness probe from the declaration, so a kind
/// with no declaration is an admission that cannot be fenced. It must
/// fail the build of the composed binary rather than the first write.
#[test]
fn a_scope_kind_no_contract_declares_refuses_the_freeze() {
    let mut registry = FlavorRegistry::new();
    registry
        .try_add_fact_schema::<ScopedFixtureFact>()
        .expect("a fixture schema registers");
    let err = registry
        .try_freeze()
        .expect_err("a scope no contract declares must refuse the freeze");
    assert!(
        matches!(
            &err,
            FlavorRegistryError::ScopeNotDeclared { kind, .. } if *kind == FIXTURE_SCOPE
        ),
        "expected ScopeNotDeclared, got {err}"
    );
}

/// Two flavors declare one kind.
///
/// The kind is hashed into ONE substrate fence namespace, so two
/// declarations of one name are two registry tables sharing a lock lane
/// — a silent collision between flavors that never agreed to share one.
/// Freeze names both flavors instead.
#[test]
fn two_contracts_declaring_one_scope_kind_refuse_the_freeze() {
    let mut registry = FlavorRegistry::new();
    registry.contracts.push(&DECLARES_THE_SCOPE);
    registry.contracts.push(&ALSO_DECLARES_THE_SCOPE);
    let err = registry
        .try_freeze()
        .expect_err("one scope kind declared twice must refuse the freeze");
    assert!(
        matches!(
            &err,
            FlavorRegistryError::DuplicateScopeDeclaration { kind, .. }
                if *kind == FIXTURE_SCOPE
        ),
        "expected DuplicateScopeDeclaration, got {err}"
    );
}

/// A declaration the backend could not splice.
///
/// Core cannot reach `PgIdent`, but it can insist on the shape every
/// backend needs, and refusing here names the FLAVOR rather than
/// failing later inside a generated statement naming a string.
#[test]
fn an_unsplicable_scope_declaration_refuses_the_freeze() {
    let mut registry = FlavorRegistry::new();
    registry.contracts.push(&DECLARES_AN_UNQUALIFIED_TABLE);
    let err = registry
        .try_freeze()
        .expect_err("an unqualified registry table must refuse the freeze");
    assert!(
        matches!(
            &err,
            FlavorRegistryError::InvalidScopeDeclaration { kind, .. } if *kind == FIXTURE_SCOPE
        ),
        "expected InvalidScopeDeclaration, got {err}"
    );
}

/// The accepted twin: a declared scope freezes, and the frozen registry
/// hands the declaration back for storage to generate its fence key and
/// liveness probe from.
///
/// A refusal with no accepted twin proves only that freeze can fail.
/// The payload half of the pair is not registered here — a registration
/// needs a `SchemaContract` entry of its own, which is a different
/// cross-check's subject — so the two halves are pinned to each other
/// by `registering_a_scoped_payload_records_the_kind_it_names` and
/// `a_scope_kind_no_contract_declares_refuses_the_freeze` instead. The
/// shipped pair is the code flavor's, exercised end to end in
/// `flavors/code/tests/repo_fence_pg.rs`.
#[test]
fn a_declared_scope_freezes_and_is_reachable_on_the_frozen_registry() {
    let mut registry = FlavorRegistry::new();
    registry.contracts.push(&DECLARES_THE_SCOPE);
    let frozen = match registry.try_freeze() {
        Ok(frozen) => frozen,
        Err(err) => panic!("a declared scope and its payload must freeze: {err}"),
    };
    let decl = frozen
        .scope_decl(FIXTURE_SCOPE)
        .expect("the frozen registry carries the declaration");
    assert_eq!(decl.registry_table, "test_flavor.things");
    assert_eq!(decl.id_column, "thing_id");
}
