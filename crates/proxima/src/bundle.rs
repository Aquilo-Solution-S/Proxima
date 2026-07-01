//! Compile-time flavor composition: a `FlavorBundle` is one flavor's
//! vocabulary (register fn) plus its sidecar migrations. Tuples of
//! bundles compose statically — duplicate ids fail at registry freeze.

use proxima_core::{FlavorRegistry, FlavorRegistryError};
use proxima_storage_pg::PgSidecarRegistry;

use crate::NamedMigrator;

pub trait FlavorBundle {
    /// # Errors
    ///
    /// Returns a registry error when a linked flavor registration is invalid.
    fn register(registry: &mut FlavorRegistry) -> Result<(), FlavorRegistryError>;
    fn register_pg_sidecars(_registry: &mut PgSidecarRegistry) {}
    /// By-value, order-preserving. The facade force-sets
    /// `ignore_missing(true)` on every returned migrator at boot.
    fn migrators() -> Vec<NamedMigrator>;
}

impl FlavorBundle for () {
    fn register(_registry: &mut FlavorRegistry) -> Result<(), FlavorRegistryError> {
        Ok(())
    }

    fn migrators() -> Vec<NamedMigrator> {
        Vec::new()
    }
}

macro_rules! impl_flavor_bundle_tuple {
    ($($name:ident),+) => {
        impl<$($name: FlavorBundle),+> FlavorBundle for ($($name,)+) {
            fn register(registry: &mut FlavorRegistry) -> Result<(), FlavorRegistryError> {
                $($name::register(registry)?;)+
                Ok(())
            }

            fn register_pg_sidecars(registry: &mut PgSidecarRegistry) {
                $($name::register_pg_sidecars(registry);)+
            }

            fn migrators() -> Vec<NamedMigrator> {
                let mut out = Vec::new();
                $(out.extend($name::migrators());)+
                out
            }
        }
    };
}

impl_flavor_bundle_tuple!(A);
impl_flavor_bundle_tuple!(A, B);
impl_flavor_bundle_tuple!(A, B, C);
impl_flavor_bundle_tuple!(A, B, C, D);
impl_flavor_bundle_tuple!(A, B, C, D, E);
impl_flavor_bundle_tuple!(A, B, C, D, E, F);
impl_flavor_bundle_tuple!(A, B, C, D, E, F, G);
impl_flavor_bundle_tuple!(A, B, C, D, E, F, G, H);

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use proxima_core::{FlavorRegistry, FlavorRegistryError};
    use proxima_storage_pg::PgSidecarRegistry;
    use sqlx::SqlSafeStr;
    use sqlx::migrate::{Migration, MigrationType, Migrator};

    use super::FlavorBundle;
    use crate::NamedMigrator;

    mod alpha {
        proxima_core::proxima_flavor! {
            name = "proxima-test-alpha",
            fact_schemas = [],
            abstraction_schemas = [],
            perspective_schemas = [],
            goal_schemas = [],
            edge_schemas = [],
            relations = [],
            mcp_tools = [],
        }
    }

    mod beta {
        proxima_core::proxima_flavor! {
            name = "proxima-test-beta",
            fact_schemas = [],
            abstraction_schemas = [],
            perspective_schemas = [],
            goal_schemas = [],
            edge_schemas = [],
            relations = [],
            mcp_tools = [],
        }
    }

    struct AlphaBundle;
    struct BetaBundle;

    impl FlavorBundle for AlphaBundle {
        fn register(registry: &mut FlavorRegistry) -> Result<(), FlavorRegistryError> {
            alpha::register(registry)
        }

        fn register_pg_sidecars(_registry: &mut PgSidecarRegistry) {}

        fn migrators() -> Vec<NamedMigrator> {
            vec![NamedMigrator::new("alpha", migrator(&[1, 2]))]
        }
    }

    impl FlavorBundle for BetaBundle {
        fn register(registry: &mut FlavorRegistry) -> Result<(), FlavorRegistryError> {
            beta::register(registry)
        }

        fn register_pg_sidecars(_registry: &mut PgSidecarRegistry) {}

        fn migrators() -> Vec<NamedMigrator> {
            vec![NamedMigrator::new("beta", migrator(&[3]))]
        }
    }

    fn migrator(versions: &[i64]) -> Migrator {
        let migrations = versions
            .iter()
            .map(|version| {
                Migration::new(
                    *version,
                    Cow::Owned(format!("test {version}")),
                    MigrationType::Simple,
                    sqlx::AssertSqlSafe(format!("SELECT {version};")).into_sql_str(),
                    false,
                )
            })
            .collect();
        Migrator {
            migrations: Cow::Owned(migrations),
            ..Migrator::DEFAULT
        }
    }

    #[test]
    fn tuple_registers_flavors_in_order() {
        let mut registry = FlavorRegistry::new();
        <(AlphaBundle, BetaBundle) as FlavorBundle>::register(&mut registry).unwrap();

        let frozen = registry.freeze_or_panic_for_tests();
        let flavor_ids: Vec<_> = frozen
            .list_flavors()
            .iter()
            .map(|flavor| flavor.flavor_id.as_str())
            .collect();

        assert!(frozen.flavor("proxima-test-alpha").is_some());
        assert!(frozen.flavor("proxima-test-beta").is_some());
        assert!(
            flavor_ids
                .windows(2)
                .any(|ids| ids == ["proxima-test-alpha", "proxima-test-beta"])
        );
    }

    #[test]
    fn tuple_preserves_migrator_order() {
        let migrators = <(AlphaBundle, BetaBundle) as FlavorBundle>::migrators();
        let versions: Vec<_> = migrators
            .iter()
            .flat_map(|migrator| {
                migrator
                    .migrator()
                    .iter()
                    .map(|migration| migration.version)
            })
            .collect();

        assert_eq!(versions, [1, 2, 3]);
    }

    #[test]
    fn same_flavor_twice_is_typed_error_at_freeze() {
        let mut registry = FlavorRegistry::new();
        <(AlphaBundle, AlphaBundle) as FlavorBundle>::register(&mut registry).unwrap();
        let err = registry.try_freeze().unwrap_err();
        assert!(matches!(
            err,
            FlavorRegistryError::DuplicateFlavor { ref flavor_id }
                if flavor_id == "proxima-test-alpha"
        ));
    }
}
