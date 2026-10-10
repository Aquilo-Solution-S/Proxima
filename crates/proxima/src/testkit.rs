//! Test support for flavor crates: `proxima = { features = ["testkit"] }`.
//!
//! Everything a flavor's `tests/support` used to rebuild: the Postgres
//! fixtures (`proxima-pg-testkit`, re-exported whole), an owner-scoped
//! [`AuthzContext`], split platform/runtime databases ([`SplitRoleDb`]), a
//! migrated split-role database per test ([`HostTemplate`]), and the
//! declaration/presence trigger drift check.

pub use proxima_pg_testkit::*;

use proxima_core::{AuthPath, AuthzContext, FlavorRegistry, Owner, Role, UserId};
use proxima_storage_pg::{
    PgPoolConfig, PgSidecarRegistry, PgStorage, PgTuning, register_core_pg_sidecars,
};
use sqlx::PgPool;

use crate::bundle::FlavorBundle;
use crate::migrations::{MigrationError, NamedMigrator, check_lineup, lineup, run_lineup};

/// A migrated, split-role template database a host suite clones once per
/// test.
///
/// The template holds the split roles and everything a boot would migrate:
/// core, the flavors and the host's own migrators, applied as the platform
/// role. [`Self::clone_split_role`] hands each test a [`SplitRoleDb`] cloned
/// from it, so a boot over the clone finds every migration applied and
/// applies none, and the roles are the ones every [`SplitRoleDb`] has: the
/// runtime role is `NOSUPERUSER NOBYPASSRLS`, owns nothing and cannot write a
/// migration ledger, so owner RLS is enforced in the clone as in production.
///
/// The template's name is a [`TemplateFamily`] prefix and 16 hex digits of a
/// hash of everything the migrators contribute: for core and each migrator,
/// its source id, its ledger and every `(version, checksum)` it applies, in
/// order, and `fingerprint`. A changed migration is a different template, and
/// the old one is dropped once nothing is connected to it. `fingerprint`
/// carries only what the migrators cannot show, such as seed SQL a host
/// applies on top.
///
/// ```compile_fail,E0308
/// // The family is a validated `TemplateFamily`; a host never passes a raw name.
/// let _ = proxima::testkit::HostTemplate::new("host_", "seed-v1", Vec::new());
/// ```
///
/// A lineup the runner refuses has no template, so none is ever looked up:
///
/// ```compile_fail,E0599
/// fn name(family: proxima::testkit::TemplateFamily) -> String {
///     proxima::testkit::HostTemplate::new(family, "seed-v1", Vec::new()).template_name()
/// }
/// ```
#[derive(Debug)]
pub struct HostTemplate {
    family: TemplateFamily,
    hash: u64,
    /// Core first, then the host's migrators: what a build runs and the hash
    /// covers.
    sources: Vec<NamedMigrator>,
}

impl HostTemplate {
    /// A template of `family` for `migrators`, added to core's. Nothing
    /// touches the database until [`Self::clone_split_role`].
    ///
    /// `migrators` are the flavors' and the host's own, as the host's
    /// `FlavorBundle::migrators` returns them; a host passes the same list to
    /// its boot, so what the template holds is what the boot expects.
    ///
    /// # Errors
    ///
    /// Returns the [`MigrationError`] a run refuses the lineup with before it
    /// connects: a repeated version, a shared ledger. Such a lineup gets no
    /// template name, because the hash cannot tell it from a valid one (it
    /// reads each lane through `NamedMigrator::lane`, which lists a repeated
    /// version once) and a name could match a cached template.
    pub fn new(
        family: TemplateFamily,
        fingerprint: &str,
        migrators: Vec<NamedMigrator>,
    ) -> Result<Self, MigrationError> {
        let sources = lineup(migrators);
        check_lineup(&sources)?;
        Ok(Self {
            hash: fingerprint_hash(fingerprint, &sources),
            family,
            sources,
        })
    }

    /// The name of the template database, for diagnostics: the family prefix
    /// and 16 hex digits.
    #[must_use]
    pub fn template_name(&self) -> String {
        self.family.template_name(self.hash)
    }

    /// Clone the template into `<prefix>_<uuid>` and return it as a
    /// [`SplitRoleDb`], building the template first if the server does not
    /// have it.
    ///
    /// The clone is adopted by a [`DbGuard`]: dropping the returned value
    /// after a passing test drops the database; after a panic it keeps it
    /// and prints a redacted `psql` URL. `schemas` is what
    /// [`SplitRoleDb::create`] takes for its flavor schemas. The template is
    /// already prepared, so providing the roles on the clone re-asserts the
    /// two roles and the database grant and returns early.
    ///
    /// Safe to call from many tests at once: the template is built once per
    /// server, under an advisory lock, and the others wait for it.
    ///
    /// # Errors
    ///
    /// Returns [`sqlx::Error::Configuration`] when `PROXIMA_TEST_PG_URL` is
    /// unset, [`sqlx::Error::Protocol`] carrying the text of a migration
    /// error from the build (a refused ledger, a failing statement), and the
    /// admin, clone or role-provisioning errors of the steps around it. A
    /// failed build leaves no template behind.
    pub async fn clone_split_role(
        &self,
        prefix: &str,
        schemas: &[&str],
    ) -> Result<SplitRoleDb, sqlx::Error> {
        // The lease holds the template against collection until it is cloned.
        let lease = ensure_template_in(&self.family, self.hash, |pool| self.build(pool)).await?;
        let cloned = SplitRoleDb::from_template(prefix, &lease, schemas).await;
        let released = lease.release().await;
        let clone = cloned?;
        released?;
        Ok(clone)
    }

    /// Provision the split roles on the staging database `pool` points at,
    /// then migrate it as the platform role, which owns what it creates.
    async fn build(&self, pool: PgPool) -> Result<(), sqlx::Error> {
        let staging: String = sqlx::query_scalar("SELECT current_database()")
            .fetch_one(&pool)
            .await?;
        // Nothing may stay connected to a database that is renamed into place.
        pool.close().await;
        let (_, platform_url) = split_role_urls(&staging).await?;
        let pg = PgStorage::connect_for_migrations_with_config(
            &platform_url,
            PgPoolConfig::default(),
            PgTuning::default(),
        )
        .await
        .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
        let migrated = run_lineup(&pg, &self.sources).await;
        pg.clone_pool_for_backend().close().await;
        migrated
            .map(|_| ())
            .map_err(|error| sqlx::Error::Protocol(error.to_string()))
    }
}

/// The template hash: `fingerprint`, then each source's id, ledger and
/// `(version, checksum)` pairs in run order, from the enumeration the ledger
/// checks and the migration plan use ([`NamedMigrator::lane`]). Every field
/// is length-prefixed, so two different inputs cannot spell the same bytes.
fn fingerprint_hash(fingerprint: &str, sources: &[NamedMigrator]) -> u64 {
    fn feed(hash: u64, bytes: &[u8]) -> u64 {
        fnv1a64_extend(
            fnv1a64_extend(hash, &(bytes.len() as u64).to_be_bytes()),
            bytes,
        )
    }
    let mut hash = feed(FNV_OFFSET_BASIS, fingerprint.as_bytes());
    for source in sources {
        hash = feed(hash, source.source().as_bytes());
        hash = feed(hash, source.ledger().as_bytes());
        let lane = source.lane();
        hash = feed(hash, &(lane.len() as u64).to_be_bytes());
        for migration in lane {
            hash = fnv1a64_extend(hash, &migration.version.to_be_bytes());
            hash = feed(hash, migration.checksum.as_ref());
        }
    }
    hash
}

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
