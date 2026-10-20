//! What the inverses reach, asked where core AND a flavor are visible.
//!
//! `proxima-storage-pg` depends on no flavor, so every completeness test it
//! carries can only ever see flavor #0. That is exactly half the registry a
//! host composes, and the missing half is where an out-of-tree flavor's
//! surfaces would be. Here the example flavor declares the second contract
//! next to core, so the whole frozen registry and the statements generated
//! from it are in scope together.
#![allow(clippy::doc_markdown)]

#[path = "fixtures/example_engine.rs"]
#[allow(dead_code, reason = "each suite uses part of the shared fixture")]
mod example_engine;

use std::collections::BTreeSet;

use example_engine::{described, expect_described, initialize_store_number, migrated_db};
use proxima::flavor::{FlavorBundle, PgSidecarRegistry, register_core_pg_sidecars};
use proxima_core::flavor::ExportRule;
use proxima_core::owner_inverse::{ExportAuthorization, OwnerExportTarget, OwnerSurfaces};
use proxima_core::storage_ports::OwnerInversePort;
use proxima_core::{FlavorRegistry, FlavorRegistryFrozen, UserId};
use proxima_example::ExampleFlavor;
use proxima_storage_pg::{PgPlatformScope, PgStorage};
use uuid::Uuid;

/// Core plus the example flavor, frozen, as a host composes them.
fn both_flavors() -> FlavorRegistryFrozen {
    let mut registry = FlavorRegistry::new();
    ExampleFlavor::register(&mut registry).expect("example schema registration");
    registry.try_freeze().expect("core + example freeze")
}

/// The runtime storage over a migrated clone, wired to both flavors.
async fn storage(db: &proxima::testkit::SplitRoleDb) -> PgStorage {
    initialize_store_number(db)
        .await
        .unwrap_or_else(|why| panic!("{why}"));
    let registry = both_flavors();
    let mut sidecars = PgSidecarRegistry::new();
    register_core_pg_sidecars(&mut sidecars);
    ExampleFlavor::register_pg_sidecars(&mut sidecars);
    let sidecars = sidecars
        .freeze_against(&registry)
        .expect("the example sidecars match the registry");
    let platform_pool = expect_described(
        "platform connection",
        sqlx::PgPool::connect(db.platform_url()).await,
    )
    .await;
    let platform_scope = expect_described(
        "platform validation",
        PgPlatformScope::new(platform_pool, &["proxima_core", "proxima_example"]).await,
    )
    .await;
    expect_described(
        "runtime connection",
        PgStorage::connect(db.runtime_url(), platform_scope, 0).await,
    )
    .await
    .with_sidecars(sidecars)
    .with_flavors(&registry)
}

/// Acceptance 2 of #478 over a composed registry: no surface the erase
/// deletes is Excluded from the export. Each is carried as rows, as an
/// allowlist, or derived from a surface that is. (Freeze refuses the
/// opposite; this pins the composed answer.)
#[test]
fn no_erased_surface_of_core_or_example_is_excluded_from_the_export() {
    let surfaces = OwnerSurfaces::for_registry(&both_flavors());
    let offenders: Vec<&str> = surfaces
        .surfaces()
        .iter()
        .filter(|surface| {
            !matches!(
                surfaces.erase_leg(surface.table),
                proxima_core::flavor::EraseLeg::Never { .. }
            ) && matches!(surface.export, ExportRule::Excluded { .. })
        })
        .map(|surface| surface.table)
        .collect();
    assert!(
        offenders.is_empty(),
        "erased but excluded from the export: {offenders:?}"
    );
}

/// The bundle carries EXACTLY the surfaces both contracts declare
/// exportable — including the ones that came back empty.
///
/// `OwnerExportBundle`'s own doc states that invariant, and nothing checked
/// it. The differential harness could not: it strips empty sections before
/// comparing, deliberately, because its goldens were captured from a corpus
/// and a corpus writes what it writes. So a mutation dropping one surface
/// from the generator's loop passed the entire workspace, and that surface
/// simply stopped being exported — the precise failure that "the
/// declaration generates the statement" was supposed to make impossible.
///
/// A FRESH owner is the right subject. Every table is empty, so the only
/// thing the assertion can be measuring is which surfaces the generator
/// visited; a seeded owner would let a present-but-unwritten table hide
/// behind a present-and-empty one.
#[tokio::test]
async fn the_bundle_carries_every_exportable_surface_of_both_flavors() {
    let db = migrated_db("proxima_owner_inverse_reach").await;
    let pg = storage(&db).await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let surfaces = OwnerSurfaces::for_registry(&both_flavors());
        let expected: BTreeSet<&str> = surfaces
            .surfaces()
            .iter()
            .filter(|surface| surface.export.carries_rows())
            .map(|surface| surface.table)
            .collect();
        assert!(
            expected
                .iter()
                .any(|table| table.starts_with("proxima_example.")),
            "the example flavor must contribute exportable surfaces, or this test is the \
             flavor-0 test again: {expected:?}"
        );

        let auth = ExportAuthorization::new_for_tests(OwnerExportTarget::PersonalOwner {
            user_id: UserId::new(Uuid::now_v7()),
        });
        let bundle = pg.export_owner_bundle(&auth, &surfaces).await?;
        let actual: BTreeSet<&str> = bundle.tables.keys().map(String::as_str).collect();
        assert_eq!(
            actual,
            expected,
            "the bundle's tables must be exactly the declared exportable surfaces; \
             missing {:?}, unexpected {:?}",
            expected.difference(&actual).collect::<Vec<_>>(),
            actual.difference(&expected).collect::<Vec<_>>()
        );
        Ok(())
    }
    .await;
    result.expect("cross-flavor export completeness failed");
}

/// A relation no contract declares is invisible to every completeness gate
/// there is.
///
/// The erase partition, the export bundle and the transfer sweep all
/// enumerate DECLARED surfaces. That is the whole design — but it means the
/// question "did anyone forget to declare a table?" is not one any of them
/// can ask. This asks it from the other side: start at the catalog, and
/// require every base table to be either a declared surface or an entry on
/// this list with a stated reason.
///
/// The core list is shipped data (`proxima_storage_pg::STORE_WIDE_TABLES`,
/// checked against `proxima_core` alone in storage-pg). This test reuses it
/// and adds the example flavor's schema, so the declared set is both
/// flavors'. The flavor's own exemptions are listed after the core ones.
fn undeclared_but_intentional() -> Vec<(&'static str, &'static str)> {
    proxima_storage_pg::STORE_WIDE_TABLES
        .iter()
        .copied()
        .chain([(
            "proxima_example.settings",
            "ownerless flavor configuration (RLS class ownerless): it holds no owner's \
             data, so no owner erase, export or transfer has anything to address in it",
        )])
        .collect()
}

#[tokio::test]
async fn every_base_table_is_a_declared_surface_or_a_stated_exemption() {
    let db = migrated_db("proxima_owner_inverse_reach").await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let surfaces = OwnerSurfaces::for_registry(&both_flavors());
        let declared: BTreeSet<&str> = surfaces
            .surfaces()
            .iter()
            .map(|surface| surface.table)
            .collect();
        let exemptions = undeclared_but_intentional();
        let exempt: BTreeSet<&str> = exemptions.iter().map(|(table, _)| *table).collect();
        for (table, why) in &exemptions {
            assert!(
                why.len() > 40,
                "{table}'s exemption needs a reason, not a placeholder"
            );
            assert!(
                !declared.contains(table),
                "{table} is a declared surface; drop its exemption"
            );
        }

        let present: Vec<String> = sqlx::query_scalar(
            "SELECT table_schema || '.' || table_name
               FROM information_schema.tables
              WHERE table_schema IN ('proxima_core', 'proxima_example')
                AND table_type = 'BASE TABLE'
                AND table_name <> '_sqlx_migrations'
              ORDER BY 1",
        )
        .fetch_all(
            &described(
                "admin connection",
                sqlx::PgPool::connect(&db.admin_url()).await,
            )
            .await?,
        )
        .await?;
        assert!(
            present.len() > 30,
            "the migrated schema should carry both flavors, got {}",
            present.len()
        );

        let undeclared: Vec<&String> = present
            .iter()
            .filter(|table| !declared.contains(table.as_str()) && !exempt.contains(table.as_str()))
            .collect();
        assert!(
            undeclared.is_empty(),
            "these relations exist and no contract declares them, so no erase, export or \
             transfer can see them: {undeclared:?}. Declare a Surface, or add an entry to \
             undeclared_but_intentional saying why not."
        );

        let stale: Vec<&&str> = exempt
            .iter()
            .filter(|table| !present.iter().any(|row| row == *table))
            .collect();
        assert!(
            stale.is_empty(),
            "these exemptions name relations the schema does not have: {stale:?}"
        );
        Ok(())
    }
    .await;
    result.expect("catalog-side declaration completeness failed");
}
