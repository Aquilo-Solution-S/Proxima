//! Compile-time spiice composition. A spiice is what a host serves under
//! its own id; a `SpiiceBundle` is one spiice linked into the host: its
//! vocabulary (register fn) plus its sidecar migrations. Tuples of
//! bundles compose statically — duplicate ids fail at registry freeze.

use proxiima_core::{SpiiceRegistry, SpiiceRegistryError};
use proxiima_storage_pg::PgSidecarRegistry;

use crate::NamedMigrator;
use crate::workers::{SpiiceWorker, SpiiceWorkerContext};

pub trait SpiiceBundle {
    /// # Errors
    ///
    /// Returns a registry error when a linked spiice registration is invalid.
    fn register(registry: &mut SpiiceRegistry) -> Result<(), SpiiceRegistryError>;
    fn register_pg_sidecars(_registry: &mut PgSidecarRegistry) {}
    /// By-value, order-preserving. The facade validates ledger versions
    /// against all composed sources at boot.
    fn migrators() -> Vec<NamedMigrator>;

    /// Spawn this spiice's background workers. Default: none.
    ///
    /// The serving runtime ([`Proxiima::run`](crate::Proxiima::run)) calls
    /// this once after boot and stores the returned workers; they run for
    /// the lifetime of the serving app and are joined by
    /// [`RunningProxiima::shutdown`](crate::RunningProxiima::shutdown).
    /// Tuple bundles chain element workers in tuple order. The serverless
    /// [`Proxiima::build`](crate::Proxiima::build) variant never calls this;
    /// hosts driving a `BuiltProxiima` own their own background tasks.
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
    /// [`SpiiceWorkerContext::new_for_tests`] (available under `cfg(test)`,
    /// the `testkit` feature, or debug builds).
    ///
    /// ```rust,no_run
    /// use proxiima::spiice::{
    ///     SpiiceBundle, SpiiceRegistry, SpiiceRegistryError, SpiiceWorker, SpiiceWorkerContext,
    ///     NamedMigrator,
    /// };
    ///
    /// struct OcrSpiice;
    ///
    /// impl SpiiceBundle for OcrSpiice {
    ///     fn register(_registry: &mut SpiiceRegistry) -> Result<(), SpiiceRegistryError> {
    ///         Ok(())
    ///     }
    ///
    ///     fn migrators() -> Vec<NamedMigrator> {
    ///         Vec::new()
    ///     }
    ///
    ///     fn spawn_workers(ctx: &SpiiceWorkerContext) -> Vec<SpiiceWorker> {
    ///         let cancel = ctx.cancel.clone();
    ///         let engine = ctx.engine.clone();
    ///         vec![SpiiceWorker {
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
    fn spawn_workers(_ctx: &SpiiceWorkerContext) -> Vec<SpiiceWorker> {
        Vec::new()
    }
}

impl SpiiceBundle for () {
    fn register(_registry: &mut SpiiceRegistry) -> Result<(), SpiiceRegistryError> {
        Ok(())
    }

    fn migrators() -> Vec<NamedMigrator> {
        Vec::new()
    }
}

macro_rules! impl_spiice_bundle_tuple {
    ($($name:ident),+) => {
        impl<$($name: SpiiceBundle),+> SpiiceBundle for ($($name,)+) {
            fn register(registry: &mut SpiiceRegistry) -> Result<(), SpiiceRegistryError> {
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

            fn spawn_workers(ctx: &SpiiceWorkerContext) -> Vec<SpiiceWorker> {
                let mut out = Vec::new();
                $(out.extend($name::spawn_workers(ctx));)+
                out
            }
        }
    };
}

impl_spiice_bundle_tuple!(A);
impl_spiice_bundle_tuple!(A, B);
impl_spiice_bundle_tuple!(A, B, C);
impl_spiice_bundle_tuple!(A, B, C, D);
impl_spiice_bundle_tuple!(A, B, C, D, E);
impl_spiice_bundle_tuple!(A, B, C, D, E, F);
impl_spiice_bundle_tuple!(A, B, C, D, E, F, G);
impl_spiice_bundle_tuple!(A, B, C, D, E, F, G, H);

/// One declaration for a whole spiice: [`proxiima_spiice!`](crate::spiice::proxiima_spiice)'s registration,
/// the PG sidecar registration derived from the same schema lists, the
/// spiice's own migration ledger ([`NamedMigrator::spiice`]), and the
/// [`SpiiceBundle`] (plus, with `app`, [`SpiiceApp`](crate::SpiiceApp)) impls.
///
/// Keys are [`proxiima_spiice!`](crate::spiice::proxiima_spiice)'s, in its order, framed by the bundle's own:
///
/// | Key | Emits |
/// |---|---|
/// | `bundle = Name` (first) | `pub struct Name;` implementing [`SpiiceBundle`] |
/// | `name` … `contract` | `pub fn register` via `proxiima_spiice!` |
/// | (derived) | `pub fn register_pg_sidecars`: `add_fact` / `add_abstraction` / `add_perspective` / `add_goal` for every fact, abstraction, perspective and goal schema listed |
/// | `migrations = expr` | `migrators()` = `[NamedMigrator::spiice(name, expr)]`, ledger `public._sqlx_migrations_<name>`; absent: none |
/// | `workers = path` | `spawn_workers` = `path(ctx)` |
/// | `app = { title, id?, version?, configure?, services? }` | [`SpiiceApp`](crate::SpiiceApp); `id` defaults to `name`, `version` to the spiice crate's `CARGO_PKG_VERSION`, the fns to the trait defaults |
///
/// Every listed fact, abstraction, perspective and goal schema must implement
/// its PG sidecar trait (`pg_sidecar!` emits them); citation schemas are
/// opaque and have none. A spiice whose sidecars differ from its
/// schema lists, or whose app mounts HTTP, keeps hand-written impls.
///
/// ```ignore
/// proxiima::spiice_bundle! {
///     bundle = OcrSpiice,
///     name = "acme-ocr",
///     fact_schemas = [payloads::PageScannedV1],
///     contract = &contract::OCR_SPIICE_CONTRACT,
///     migrations = sqlx::migrate!("./migrations"),
///     app = { title = "Acme OCR" },
/// }
/// ```
#[macro_export]
macro_rules! spiice_bundle {
    (
        bundle = $bundle:ident,
        name = $name:literal
        $(, display_name = $display_name:literal)?
        $(, fact_schemas = [ $($fact:ty),* $(,)? ])?
        $(, abstraction_schemas = [ $($abs:ty),* $(,)? ])?
        $(, perspective_schemas = [ $($persp:ty),* $(,)? ])?
        $(, goal_schemas = [ $($goal:ty),* $(,)? ])?
        $(, cited_object_schemas = [ $($cited:ty),* $(,)? ])?
        $(, opaque_cited_object_schemas = [ $($opaque_cited:expr),* $(,)? ])?
        $(, schema_capability_tags = [ $(($cap_kind:ident, $cap_ty:ty) => [ $($cap_tag:expr),* $(,)? ]),* $(,)? ])?
        $(, mcp_tools = [ $($tool:ty),* $(,)? ])?
        $(, contract = $contract:expr)?
        $(, evolutions = [ $($evolution:expr),* $(,)? ])?
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
        $crate::spiice::proxiima_spiice! {
            name = $name
            $(, display_name = $display_name)?
            $(, fact_schemas = [ $($fact),* ])?
            $(, abstraction_schemas = [ $($abs),* ])?
            $(, perspective_schemas = [ $($persp),* ])?
            $(, goal_schemas = [ $($goal),* ])?
            $(, cited_object_schemas = [ $($cited),* ])?
            $(, opaque_cited_object_schemas = [ $($opaque_cited),* ])?
            $(, schema_capability_tags = [ $(($cap_kind, $cap_ty) => [ $($cap_tag),* ]),* ])?
            $(, mcp_tools = [ $($tool),* ])?
            $(, contract = $contract)?
        }

        /// Generated by `spiice_bundle!`: every fact, abstraction, perspective and goal schema's PG sidecar.
        #[allow(unused_variables)]
        pub fn register_pg_sidecars(registry: &mut $crate::spiice::PgSidecarRegistry) {
            $($( registry.add_fact::<$fact>(); )*)?
            $($( registry.add_abstraction::<$abs>(); )*)?
            $($( registry.add_perspective::<$persp>(); )*)?
            $($( registry.add_goal::<$goal>(); )*)?
            $($( registry.add_evolution($evolution); )*)?
        }

        /// Generated by `spiice_bundle!`.
        #[derive(Debug, Clone, Copy, Default)]
        pub struct $bundle;

        impl $crate::spiice::SpiiceBundle for $bundle {
            fn register(
                registry: &mut $crate::spiice::SpiiceRegistry,
            ) -> ::std::result::Result<(), $crate::spiice::SpiiceRegistryError> {
                self::register(registry)
            }

            fn register_pg_sidecars(registry: &mut $crate::spiice::PgSidecarRegistry) {
                self::register_pg_sidecars(registry);
            }


            fn migrators() -> ::std::vec::Vec<$crate::spiice::NamedMigrator> {
                #[allow(unused_mut)]
                let mut migrators = ::std::vec::Vec::new();
                $(
                    const _: () = ::std::assert!(
                        $crate::spiice::is_spiice_ledger_id($name),
                        "spiice_bundle! name must be 1-40 bytes of [a-z0-9_-] starting with a letter: it names the spiice's ledger",
                    );
                    migrators.push($crate::spiice::NamedMigrator::spiice($name, $migrator));
                )?
                migrators
            }

            $(
                fn spawn_workers(
                    ctx: &$crate::spiice::SpiiceWorkerContext,
                ) -> ::std::vec::Vec<$crate::spiice::SpiiceWorker> {
                    $workers(ctx)
                }
            )?
        }

        $(
            impl $crate::SpiiceApp for $bundle {
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
                        $crate::spiice::SpiiceServices,
                        $crate::spiice::SpiiceServiceError,
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

    use proxiima_core::{SpiiceRegistry, SpiiceRegistryError};
    use proxiima_storage_pg::PgSidecarRegistry;
    use sqlx::SqlSafeStr;
    use sqlx::migrate::{Migration, MigrationType, Migrator};

    use super::SpiiceBundle;
    use crate::NamedMigrator;

    /// A declaration for a spiice that registers nothing. Freeze refuses a
    /// linked spiice without one, and the subject of these tests is bundle
    /// composition, so every field but the identity is empty.
    const fn empty_contract(
        spiice_id: &'static str,
        ordinal: u16,
    ) -> proxiima_core::spiice::SpiiceContract {
        proxiima_core::spiice::SpiiceContract {
            retired_facts: &[],
            spiice_id,
            ordinal,
            schemas: &[],
            state_surfaces: &[],
            scopes: &[],
            kernel_surfaces: &[],
            tools: &[],
            resources: &[],
            bespoke_erase_legs: &[],
            bespoke_transfer_legs: &[],
            projection: proxiima_core::spiice::ProjectionDecl::None {
                why: "a bundle fixture registers no search surface",
            },
            evolutions: &[],
        }
    }

    // Distinct non-zero ordinals: 0 is core's, and two claims on one
    // ordinal are a freeze error.
    static ALPHA_CONTRACT: proxiima_core::spiice::SpiiceContract =
        empty_contract("proxiima-test-alpha", 7);

    mod retired {
        use crate::spiice::{
            CounterRule, EraseRule, ExportRule, ForgetRule, KeyShape, RetiredFactContract,
            SchemaRef, SpiiceContract, Surface, TransferRule,
        };

        static CONTRACT: SpiiceContract = SpiiceContract {
            retired_facts: &[RetiredFactContract {
                id: SchemaRef::new("proxiima-test-retired", "removed", 1),
                sidecar: Surface {
                    table: "proxiima_test_retired.removed_v1",
                    key: KeyShape::MemoryT { column: "old_t" },
                    owner_column: None,
                    erase: EraseRule::ByKey,
                    transfer: TransferRule::StaysOnKey,
                    export: ExportRule::Rows,
                    forget: ForgetRule::DumpThenDelete,
                    lexical_language_column: None,
                    counter: CounterRule::Counted("sidecar_rows"),
                    completeness: None,
                },
                inverse_surfaces: &[],
            }],
            ..super::empty_contract("proxiima-test-retired", 8)
        };

        crate::spiice_bundle! {
            bundle = RetiredBundle,
            name = "proxiima-test-retired",
            contract = &CONTRACT,
        }
    }

    #[test]
    fn bundle_composes_a_retired_fact_without_its_payload_type() {
        let mut core = SpiiceRegistry::new();
        retired::RetiredBundle::register(&mut core).unwrap();
        let core = core.try_freeze().unwrap();
        let mut pg = PgSidecarRegistry::new();
        crate::spiice::register_core_pg_sidecars(&mut pg);
        retired::RetiredBundle::register_pg_sidecars(&mut pg);
        let pg = pg.freeze_against(&core).unwrap();
        let [metadata] = pg.retired_fact_sidecars() else {
            panic!("one inverse-only declaration");
        };
        assert_eq!(metadata.sidecar_table(), "proxiima_test_retired.removed_v1");
        assert_eq!(metadata.memory_key_column(), "old_t");
        assert!(!pg.contains(metadata.key()));
        assert!(
            core.lookup(&metadata.key().schema_id, metadata.key().schema_version)
                .is_none()
        );
    }

    mod alpha {
        proxiima_core::proxiima_spiice! {
            name = "proxiima-test-alpha",
            fact_schemas = [],
            abstraction_schemas = [],
            perspective_schemas = [],
            goal_schemas = [],
            mcp_tools = [],
            contract = &super::ALPHA_CONTRACT,
        }
    }

    struct AlphaBundle;

    impl SpiiceBundle for AlphaBundle {
        fn register(registry: &mut SpiiceRegistry) -> Result<(), SpiiceRegistryError> {
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
                    // SQL-POLICY: fixed-fragment — version is an i64, formatted as decimal.
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
    fn same_spiice_twice_is_typed_error_at_freeze() {
        let mut registry = SpiiceRegistry::new();
        <(AlphaBundle, AlphaBundle) as SpiiceBundle>::register(&mut registry).unwrap();
        let err = registry.try_freeze().unwrap_err();
        assert!(matches!(
            err,
            SpiiceRegistryError::DuplicateSpiice { ref spiice_id }
                if spiice_id == "proxiima-test-alpha"
        ));
    }
}
