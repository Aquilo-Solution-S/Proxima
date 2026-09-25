//! Compile-time flavor composition: a `FlavorBundle` is one flavor's
//! vocabulary (register fn) plus its sidecar migrations. Tuples of
//! bundles compose statically — duplicate ids fail at registry freeze.

use proxima_core::{FlavorRegistry, FlavorRegistryError};
use proxima_storage_pg::PgSidecarRegistry;

use crate::NamedMigrator;
use crate::workers::{FlavorWorker, FlavorWorkerContext};

pub trait FlavorBundle {
    /// # Errors
    ///
    /// Returns a registry error when a linked flavor registration is invalid.
    fn register(registry: &mut FlavorRegistry) -> Result<(), FlavorRegistryError>;
    fn register_pg_sidecars(_registry: &mut PgSidecarRegistry) {}
    /// By-value, order-preserving. The facade force-sets
    /// `ignore_missing(true)` on every returned migrator at boot.
    fn migrators() -> Vec<NamedMigrator>;

    /// Spawn this flavor's background workers. Default: none.
    ///
    /// The serving runtime ([`Proxima::run`](crate::Proxima::run)) calls
    /// this once after boot and stores the returned workers; they run for
    /// the lifetime of the serving app and are joined by
    /// [`RunningProxima::shutdown`](crate::RunningProxima::shutdown).
    /// Tuple bundles chain element workers in tuple order. The serverless
    /// [`Proxima::build`](crate::Proxima::build) variant never calls this;
    /// hosts driving a `BuiltProxima` own their own background tasks.
    ///
    /// Contract:
    ///
    /// - Every returned worker MUST terminate when `ctx.cancel` is
    ///   cancelled — select on the token in the work loop, mirroring the
    ///   core embedding worker.
    /// - A panicking worker never takes the host down: its join error is
    ///   logged at shutdown, not propagated.
    /// - `ctx.service::<CitedBlobService>()` and
    ///   `ctx.service::<CitedBlobReadService>()` resolve the same host-wired
    ///   backend tools receive, and are `None` unless S3 is configured. A
    ///   worker that needs one MUST fail its job typed when it is absent — a
    ///   no-op turns a misconfigured host into a silently idle one.
    /// - `ctx.service::<DelegatedAuthorityService>()` resolves the exact
    ///   runtime-composed authority service only when a host authenticator is
    ///   configured. A durable worker queues `DelegationId`, redeems a fresh
    ///   `DelegatedPhase` at claim and at each subsequent phase boundary, and
    ///   supplies that phase only to the explicitly delegated-capable
    ///   Engine/blob methods.
    ///   It never reconstructs or serializes `AuthzContext`.
    ///
    /// To unit-test an implementation without booting the serving
    /// runtime, build the context with
    /// [`FlavorWorkerContext::new_for_tests`] (available under `cfg(test)`,
    /// the `testkit` feature, or debug builds).
    ///
    /// ```rust,no_run
    /// use proxima::flavor::{
    ///     FlavorBundle, FlavorRegistry, FlavorRegistryError, FlavorWorker, FlavorWorkerContext,
    ///     NamedMigrator,
    /// };
    ///
    /// struct OcrFlavor;
    ///
    /// impl FlavorBundle for OcrFlavor {
    ///     fn register(_registry: &mut FlavorRegistry) -> Result<(), FlavorRegistryError> {
    ///         Ok(())
    ///     }
    ///
    ///     fn migrators() -> Vec<NamedMigrator> {
    ///         Vec::new()
    ///     }
    ///
    ///     fn spawn_workers(ctx: &FlavorWorkerContext) -> Vec<FlavorWorker> {
    ///         let cancel = ctx.cancel.clone();
    ///         let engine = ctx.engine.clone();
    ///         vec![FlavorWorker {
    ///             name: "ocr-jobs",
    ///             handle: tokio::spawn(async move {
    ///                 loop {
    ///                     tokio::select! {
    ///                         () = cancel.cancelled() => break,
    ///                         () = tokio::time::sleep(std::time::Duration::from_secs(5)) => {
    ///                             let _ = engine.registry();
    ///                             /* drive one round of jobs */
    ///                         }
    ///                     }
    ///                 }
    ///             }),
    ///         }]
    ///     }
    /// }
    /// ```
    #[must_use]
    fn spawn_workers(_ctx: &FlavorWorkerContext) -> Vec<FlavorWorker> {
        Vec::new()
    }
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

            fn spawn_workers(ctx: &FlavorWorkerContext) -> Vec<FlavorWorker> {
                let mut out = Vec::new();
                $(out.extend($name::spawn_workers(ctx));)+
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

/// One declaration for a whole flavor: [`proxima_flavor!`](crate::flavor::proxima_flavor)'s registration,
/// the PG sidecar registration derived from the same schema lists, the
/// flavor's own migration ledger ([`NamedMigrator::flavor`]), and the
/// [`FlavorBundle`] (plus, with `app`, [`FlavorApp`](crate::FlavorApp)) impls.
///
/// Keys are [`proxima_flavor!`](crate::flavor::proxima_flavor)'s, in its order, framed by the bundle's own:
///
/// | Key | Emits |
/// |---|---|
/// | `bundle = Name` (first) | `pub struct Name;` implementing [`FlavorBundle`] |
/// | `name` … `contract` | `pub fn register` via `proxima_flavor!` |
/// | (derived) | `pub fn register_pg_sidecars`: `add_fact` / `add_abstraction` / `add_perspective` / `add_goal` / `add_cited_object` / `add_citation_mapping` for every typed schema listed |
/// | `migrations = expr` | `migrators()` = `[NamedMigrator::flavor(name, expr)]`, ledger `public._sqlx_migrations_<name>`; absent: none |
/// | `workers = path` | `spawn_workers` = `path(ctx)` |
/// | `app = { title, id?, version?, configure?, services? }` | [`FlavorApp`](crate::FlavorApp); `id` defaults to `name`, `version` to the flavor crate's `CARGO_PKG_VERSION`, the fns to the trait defaults |
///
/// Every listed typed schema must implement its PG sidecar trait
/// (`pg_sidecar!` emits them); a flavor whose sidecars differ from its
/// schema lists, or whose app mounts HTTP, keeps hand-written impls.
///
/// ```ignore
/// proxima::flavor_bundle! {
///     bundle = OcrFlavor,
///     name = "acme-ocr",
///     fact_schemas = [payloads::PageScannedV1],
///     contract = &contract::OCR_FLAVOR_CONTRACT,
///     migrations = sqlx::migrate!("./migrations"),
///     app = { title = "Acme OCR" },
/// }
/// ```
#[macro_export]
macro_rules! flavor_bundle {
    (
        bundle = $bundle:ident,
        name = $name:literal
        $(, display_name = $display_name:literal)?
        $(, fact_schemas = [ $($fact:ty),* $(,)? ])?
        $(, abstraction_schemas = [ $($abs:ty),* $(,)? ])?
        $(, perspective_schemas = [ $($persp:ty),* $(,)? ])?
        $(, goal_schemas = [ $($goal:ty),* $(,)? ])?
        $(, cited_object_schemas = [ $($cited:ty),* $(,)? ])?
        $(, citation_mapping_schemas = [ $($citemap:ty),* $(,)? ])?
        $(, opaque_cited_object_schemas = [ $($opaque_cited:expr),* $(,)? ])?
        $(, opaque_citation_mapping_schemas = [ $($opaque_citemap:expr),* $(,)? ])?
        $(, schema_capability_tags = [ $(($cap_kind:ident, $cap_ty:ty) => [ $($cap_tag:expr),* $(,)? ]),* $(,)? ])?
        $(, mcp_tools = [ $($tool:ty),* $(,)? ])?
        $(, contract = $contract:expr)?
        $(, migrations = $migrator:expr)?
        $(, workers = $workers:path)?
        $(, app = {
            title = $title:literal
            $(, id = $app_id:literal)?
            $(, version = $version:expr)?
            $(, configure = $configure:path)?
            $(, services = $services:path)?
            $(,)?
        })?
        $(,)?
    ) => {
        $crate::flavor::proxima_flavor! {
            name = $name
            $(, display_name = $display_name)?
            $(, fact_schemas = [ $($fact),* ])?
            $(, abstraction_schemas = [ $($abs),* ])?
            $(, perspective_schemas = [ $($persp),* ])?
            $(, goal_schemas = [ $($goal),* ])?
            $(, cited_object_schemas = [ $($cited),* ])?
            $(, citation_mapping_schemas = [ $($citemap),* ])?
            $(, opaque_cited_object_schemas = [ $($opaque_cited),* ])?
            $(, opaque_citation_mapping_schemas = [ $($opaque_citemap),* ])?
            $(, schema_capability_tags = [ $(($cap_kind, $cap_ty) => [ $($cap_tag),* ]),* ])?
            $(, mcp_tools = [ $($tool),* ])?
            $(, contract = $contract)?
        }

        /// Generated by `flavor_bundle!`: every typed schema's PG sidecar.
        #[allow(unused_variables)]
        pub fn register_pg_sidecars(registry: &mut $crate::flavor::PgSidecarRegistry) {
            $($( registry.add_fact::<$fact>(); )*)?
            $($( registry.add_abstraction::<$abs>(); )*)?
            $($( registry.add_perspective::<$persp>(); )*)?
            $($( registry.add_goal::<$goal>(); )*)?
            $($( registry.add_cited_object::<$cited>(); )*)?
            $($( registry.add_citation_mapping::<$citemap>(); )*)?
        }

        /// Generated by `flavor_bundle!`.
        #[derive(Debug, Clone, Copy, Default)]
        pub struct $bundle;

        impl $crate::flavor::FlavorBundle for $bundle {
            fn register(
                registry: &mut $crate::flavor::FlavorRegistry,
            ) -> ::std::result::Result<(), $crate::flavor::FlavorRegistryError> {
                self::register(registry)
            }

            fn register_pg_sidecars(registry: &mut $crate::flavor::PgSidecarRegistry) {
                self::register_pg_sidecars(registry);
            }

            fn migrators() -> ::std::vec::Vec<$crate::flavor::NamedMigrator> {
                #[allow(unused_mut)]
                let mut migrators = ::std::vec::Vec::new();
                $(
                    const _: () = ::std::assert!(
                        $crate::flavor::is_flavor_ledger_id($name),
                        "flavor_bundle! name must be 1-40 bytes of [a-z0-9_-] starting with a letter: it names the flavor's ledger",
                    );
                    migrators.push($crate::flavor::NamedMigrator::flavor($name, $migrator));
                )?
                migrators
            }

            $(
                fn spawn_workers(
                    ctx: &$crate::flavor::FlavorWorkerContext,
                ) -> ::std::vec::Vec<$crate::flavor::FlavorWorker> {
                    $workers(ctx)
                }
            )?
        }

        $(
            impl $crate::FlavorApp for $bundle {
                fn app_info() -> $crate::AppInfo {
                    #[allow(unused_assignments, unused_mut)]
                    let mut id: &'static str = $name;
                    $(id = $app_id;)?
                    #[allow(unused_assignments, unused_mut)]
                    let mut version: &'static str = ::std::env!("CARGO_PKG_VERSION");
                    $(version = $version;)?
                    $crate::AppInfo {
                        id,
                        title: $title,
                        version,
                    }
                }

                $(
                    fn configure(builder: $crate::RuntimeBuilder) -> $crate::RuntimeBuilder {
                        $configure(builder)
                    }
                )?

                $(
                    fn services(
                        ctx: &$crate::AppContext,
                    ) -> ::std::result::Result<
                        $crate::flavor::FlavorServices,
                        $crate::flavor::FlavorServiceError,
                    > {
                        $services(ctx)
                    }
                )?
            }
        )?
    };
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use proxima_core::{FlavorRegistry, FlavorRegistryError};
    use proxima_storage_pg::PgSidecarRegistry;
    use sqlx::SqlSafeStr;
    use sqlx::migrate::{Migration, MigrationType, Migrator};

    use super::FlavorBundle;
    use crate::NamedMigrator;

    /// A declaration for a flavor that registers nothing. Freeze refuses a
    /// linked flavor without one, and the subject of these tests is bundle
    /// composition, so every field but the identity is empty.
    const fn empty_contract(
        flavor_id: &'static str,
        ordinal: u16,
    ) -> proxima_core::flavor::FlavorContract {
        proxima_core::flavor::FlavorContract {
            flavor_id,
            ordinal,
            schemas: &[],
            state_surfaces: &[],
            scopes: &[],
            kernel_surfaces: &[],
            tools: &[],
            resources: &[],
            bespoke_erase_legs: &[],
            bespoke_transfer_legs: &[],
            projection: proxima_core::flavor::ProjectionDecl::None {
                why: "a bundle fixture registers no search surface",
            },
        }
    }

    // Distinct non-zero ordinals: 0 is core's, and two claims on one
    // ordinal are a freeze error.
    static ALPHA_CONTRACT: proxima_core::flavor::FlavorContract =
        empty_contract("proxima-test-alpha", 7);

    mod alpha {
        proxima_core::proxima_flavor! {
            name = "proxima-test-alpha",
            fact_schemas = [],
            abstraction_schemas = [],
            perspective_schemas = [],
            goal_schemas = [],
            mcp_tools = [],
            contract = &super::ALPHA_CONTRACT,
        }
    }

    struct AlphaBundle;

    impl FlavorBundle for AlphaBundle {
        fn register(registry: &mut FlavorRegistry) -> Result<(), FlavorRegistryError> {
            alpha::register(registry)
        }

        fn register_pg_sidecars(_registry: &mut PgSidecarRegistry) {}

        fn migrators() -> Vec<NamedMigrator> {
            vec![NamedMigrator::new("alpha", migrator(&[1, 2]))]
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
