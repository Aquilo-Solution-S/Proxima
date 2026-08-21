//! What the inverses reach, asked where BOTH flavors are visible.
//!
//! `proxima-storage-pg` depends on no flavor, so every completeness test it
//! carries can only ever see flavor #0. That is exactly half the registry a
//! shipped host composes, and the missing half is where an out-of-tree
//! flavor's surfaces would be. This crate declares the second contract and
//! depends on the substrate, so it is the one place the whole frozen
//! registry and the statements generated from it are in scope together.
#![allow(clippy::doc_markdown)]

mod common;

use std::collections::BTreeSet;

use common::migrated_db;
use proxima_core::flavor::ExportRule;
use proxima_core::owner_inverse::{ExportAuthorization, OwnerExportTarget, OwnerSurfaces};
use proxima_core::storage_ports::OwnerInversePort;
use proxima_core::{FlavorRegistry, FlavorRegistryFrozen, UserId};
use proxima_pg_testkit::{db_url, drop_db};
use uuid::Uuid;

/// Core plus code, frozen, exactly as the shipped host composes it.
fn both_flavors() -> FlavorRegistryFrozen {
    let mut registry = FlavorRegistry::new();
    proxima_code::register(&mut registry).expect("code schema registration");
    registry.try_freeze().expect("core + code freeze")
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
    let (db_name, pg) = migrated_db().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let surfaces = OwnerSurfaces::for_registry(&both_flavors());
        let expected: BTreeSet<&str> = surfaces
            .surfaces()
            .iter()
            .filter(|surface| !matches!(surface.export, ExportRule::Excluded { .. }))
            .map(|surface| surface.table)
            .collect();
        assert!(
            expected
                .iter()
                .any(|table| table.starts_with("proxima_code.")),
            "the code flavor must contribute exportable surfaces, or this test is the \
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
    let _ = drop_db(&db_name).await;
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
/// Migration bookkeeping is excluded structurally (`_sqlx_migrations`); it
/// is not owner data under any reading and no flavor owns it.
const UNDECLARED_BUT_INTENTIONAL: &[(&str, &str)] = &[
    (
        "proxima_core.closed_handle",
        "A tombstone for a handle, not a row about an owner: it records that \
         a handle may never be reused, which has to outlive every owner that \
         ever held it. Erasing it would let the id come back.",
    ),
    (
        "proxima_core.flavor_surface",
        "The registry's own stamp domain — which tables a memory row may \
         stamp. Schema metadata written by migrations, identical for every \
         owner, and the relation the surface declarations are checked \
         AGAINST.",
    ),
    (
        "proxima_core.lexical_languages",
        "The FK domain of permitted regconfig values. Schema metadata \
         written by migrations, identical for every owner.",
    ),
    (
        "proxima_core.lexical_default",
        "The deployment's default lexical configuration — one row, an \
         operator setting, owned by nobody.",
    ),
];

#[tokio::test]
async fn every_base_table_is_a_declared_surface_or_a_stated_exemption() {
    let (db_name, _pg) = migrated_db().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let surfaces = OwnerSurfaces::for_registry(&both_flavors());
        let declared: BTreeSet<&str> = surfaces
            .surfaces()
            .iter()
            .map(|surface| surface.table)
            .collect();
        let exempt: BTreeSet<&str> = UNDECLARED_BUT_INTENTIONAL
            .iter()
            .map(|(table, _)| *table)
            .collect();
        for (table, why) in UNDECLARED_BUT_INTENTIONAL {
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
              WHERE table_schema IN ('proxima_core', 'proxima_code')
                AND table_type = 'BASE TABLE'
                AND table_name <> '_sqlx_migrations'
              ORDER BY 1",
        )
        .fetch_all(&sqlx::PgPool::connect(&db_url(&db_name)).await?)
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
             UNDECLARED_BUT_INTENTIONAL saying why not."
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
    let _ = drop_db(&db_name).await;
    result.expect("catalog-side declaration completeness failed");
}
