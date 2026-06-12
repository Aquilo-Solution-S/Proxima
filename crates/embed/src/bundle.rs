//! Compile-time flavor composition: a `FlavorBundle` is one flavor's
//! vocabulary (register fn) plus its sidecar migrations. Tuples of
//! bundles compose statically — duplicate ids fail at registry freeze.

use proxima_core::FlavorRegistry;

use crate::NamedMigrator;

pub trait FlavorBundle {
    fn register(registry: &mut FlavorRegistry);
    /// By-value, order-preserving. The facade force-sets
    /// `ignore_missing(true)` on every returned migrator at boot.
    fn migrators() -> Vec<NamedMigrator>;
}

macro_rules! impl_flavor_bundle_tuple {
    ($($name:ident),+) => {
        impl<$($name: FlavorBundle),+> FlavorBundle for ($($name,)+) {
            fn register(registry: &mut FlavorRegistry) {
                $($name::register(registry);)+
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

    use proxima_core::FlavorRegistry;
    use sqlx::migrate::{Migration, MigrationType, Migrator};

    use super::FlavorBundle;
    use crate::NamedMigrator;

    mod alpha {
        proxima_core::proxima_flavor! {
            name = "proxima-embed-test-alpha",
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
            name = "proxima-embed-test-beta",
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
        fn register(registry: &mut FlavorRegistry) {
            alpha::register(registry);
        }

        fn migrators() -> Vec<NamedMigrator> {
            vec![NamedMigrator::new("alpha", migrator(&[1, 2]))]
        }
    }

    impl FlavorBundle for BetaBundle {
        fn register(registry: &mut FlavorRegistry) {
            beta::register(registry);
        }

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
                    Cow::Owned(format!("SELECT {version};")),
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
        <(AlphaBundle, BetaBundle) as FlavorBundle>::register(&mut registry);

        let frozen = registry.freeze();
        let flavor_ids: Vec<_> = frozen
            .list_flavors()
            .iter()
            .map(|flavor| flavor.flavor_id.as_str())
            .collect();

        assert!(frozen.flavor("proxima-embed-test-alpha").is_some());
        assert!(frozen.flavor("proxima-embed-test-beta").is_some());
        assert!(
            flavor_ids
                .windows(2)
                .any(|ids| ids == ["proxima-embed-test-alpha", "proxima-embed-test-beta"])
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
    #[should_panic(expected = "duplicate FlavorDescriptor flavor_id registered")]
    fn same_flavor_twice_panics_at_freeze() {
        let mut registry = FlavorRegistry::new();
        <(AlphaBundle, AlphaBundle) as FlavorBundle>::register(&mut registry);
        let _ = registry.freeze();
    }
}
