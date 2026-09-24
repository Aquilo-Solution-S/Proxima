//! Test support for flavor crates: `proxima = { features = ["testkit"] }`.
//!
//! Everything a flavor's `tests/support` used to rebuild: the Postgres
//! fixtures (`proxima-pg-testkit`, re-exported whole), an owner-scoped
//! [`AuthzContext`], split platform/runtime databases ([`SplitRoleDb`]), and
//! the declaration/presence trigger drift check.

pub use proxima_pg_testkit::*;

use proxima_core::{AuthPath, AuthzContext, FlavorRegistry, Owner, Role, UserId};
use proxima_storage_pg::{PgSidecarRegistry, register_core_pg_sidecars};

use crate::bundle::FlavorBundle;

/// A verified [`AuthzContext`] scoped to exactly `owner`, as a trusted host
/// resolves one: a personal owner is its own subject; a group owner is
/// reached by a fresh subject holding `admin` on it.
///
/// The context carries the sealed owner scope only the authentication path
/// mints, so owner-RLS reads and writes run as they do in production.
///
/// # Panics
///
/// Never for a well-formed owner: both constructions narrow to `owner` by
/// construction.
#[must_use]
pub fn scoped_authz(owner: Owner) -> AuthzContext {
    let context = match owner {
        Owner::Personal(_) => AuthzContext::single_owner(&owner, AuthPath::HostBearer),
        Owner::Group(_) => AuthzContext::for_subject_with_role(
            UserId::new(uuid::Uuid::now_v7()),
            [(owner, Role::admin())],
            AuthPath::HostBearer,
        )
        .narrowed_to_owner(owner)
        .expect("an admin on exactly this owner narrows to it"),
    };
    proxima_core::test_fixtures::authenticated_context(context)
}

/// Assert that every declaration and presence trigger the substrate
/// generates for flavor `F`'s memory sidecars appears verbatim in one of
/// `migrations` (the flavor's SQL, e.g. `include_str!` of its files).
///
/// The generated statements are what boot compares the live database
/// against; a hand-copied trigger that drifted from them fails boot on
/// every database that applied it.
///
/// # Panics
///
/// When `F` does not register or freeze, when it registers no memory
/// sidecar under `flavor_id` (nothing to check usually means a wrong id),
/// or when a generated statement is missing, listing each one.
pub fn assert_trigger_migrations<F: FlavorBundle>(flavor_id: &str, migrations: &[&str]) {
    let mut registry = FlavorRegistry::new();
    F::register(&mut registry)
        .unwrap_or_else(|error| panic!("flavor {flavor_id} does not register: {error}"));
    let registry = registry
        .try_freeze()
        .unwrap_or_else(|error| panic!("flavor {flavor_id} does not freeze: {error}"));
    let mut sidecars = PgSidecarRegistry::new();
    register_core_pg_sidecars(&mut sidecars);
    F::register_pg_sidecars(&mut sidecars);
    let sidecars = sidecars
        .freeze_against(&registry)
        .unwrap_or_else(|error| panic!("flavor {flavor_id} PG sidecars do not freeze: {error}"));
    let mut artifacts = sidecars
        .declaration_trigger_artifacts(flavor_id)
        .unwrap_or_else(|error| panic!("declaration triggers for {flavor_id}: {error}"));
    artifacts.extend(
        sidecars
            .presence_trigger_artifacts(flavor_id)
            .unwrap_or_else(|error| panic!("presence triggers for {flavor_id}: {error}")),
    );
    assert!(
        !artifacts.is_empty(),
        "flavor {flavor_id} registers no memory sidecar under that id, so there is no trigger to check"
    );
    let missing: Vec<&str> = artifacts
        .iter()
        .map(|artifact| artifact.forward.as_str())
        .filter(|forward| !migrations.iter().any(|sql| sql.contains(forward)))
        .collect();
    assert!(
        missing.is_empty(),
        "{} of {} generated trigger statements for {flavor_id} appear in no migration \
         (ship them verbatim in a new migration):\n\n{}",
        missing.len(),
        artifacts.len(),
        missing.join("\n\n")
    );
}
