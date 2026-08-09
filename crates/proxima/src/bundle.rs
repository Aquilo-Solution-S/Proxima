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

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use proxima_core::storage_ports::{
        CitedBlobHeld, CitedBlobPort, CitedBlobReadUrl, CitedBlobService, CitedBlobStaged,
        CitedBlobUploadAborted, CitedBlobUploadPrepared,
    };
    use proxima_core::{
        AuthzContext, FlavorRegistry, FlavorRegistryError, FlavorServices, OwnerRef, StorageError,
    };
    use proxima_storage_pg::PgSidecarRegistry;
    use sqlx::SqlSafeStr;
    use sqlx::migrate::{Migration, MigrationType, Migrator};

    use super::FlavorBundle;
    use crate::NamedMigrator;
    use crate::workers::{FlavorWorker, FlavorWorkerContext};

    mod alpha {
        proxima_core::proxima_flavor! {
            name = "proxima-test-alpha",
            fact_schemas = [],
            abstraction_schemas = [],
            perspective_schemas = [],
            goal_schemas = [],
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

    /// Contributes two named workers; every other test bundle keeps the
    /// default empty `spawn_workers`.
    struct GammaBundle;

    impl FlavorBundle for GammaBundle {
        fn register(_registry: &mut FlavorRegistry) -> Result<(), FlavorRegistryError> {
            Ok(())
        }

        fn migrators() -> Vec<NamedMigrator> {
            Vec::new()
        }

        fn spawn_workers(ctx: &FlavorWorkerContext) -> Vec<FlavorWorker> {
            ["gamma-first", "gamma-second"]
                .into_iter()
                .map(|name| {
                    let cancel = ctx.cancel.clone();
                    FlavorWorker {
                        name,
                        handle: tokio::spawn(cancel.cancelled_owned()),
                    }
                })
                .collect()
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

    /// Every method fails `Unavailable`: the tests here only need a
    /// `CitedBlobPort` that exists, never one that works.
    struct StubBlobPort;

    #[async_trait::async_trait]
    impl CitedBlobPort for StubBlobPort {
        async fn prepare_upload(
            &self,
            _authz: &AuthzContext,
            _owner: OwnerRef,
            _filename: &str,
            _mime: &str,
            _byte_len: u64,
        ) -> Result<CitedBlobUploadPrepared, StorageError> {
            Err(StorageError::Unavailable("stub".into()))
        }

        async fn stage_upload(
            &self,
            _authz: &AuthzContext,
            _owner: OwnerRef,
            _upload_id: &str,
        ) -> Result<CitedBlobStaged, StorageError> {
            Err(StorageError::Unavailable("stub".into()))
        }

        async fn finish_upload(
            &self,
            _authz: &AuthzContext,
            _owner: OwnerRef,
            _upload_id: &str,
            _cited_object_id: uuid::Uuid,
        ) -> Result<(), StorageError> {
            Err(StorageError::Unavailable("stub".into()))
        }

        async fn abort_upload(
            &self,
            _authz: &AuthzContext,
            _owner: OwnerRef,
            _upload_id: &str,
        ) -> Result<CitedBlobUploadAborted, StorageError> {
            Err(StorageError::Unavailable("stub".into()))
        }

        async fn read_url(
            &self,
            _authz: &AuthzContext,
            _owner: OwnerRef,
            _cited_object_id: uuid::Uuid,
        ) -> Result<CitedBlobReadUrl, StorageError> {
            Err(StorageError::Unavailable("stub".into()))
        }

        async fn find_held_blobs(
            &self,
            _authz: &AuthzContext,
            _owner: OwnerRef,
            _content_hashes: &[[u8; 32]],
        ) -> Result<Vec<CitedBlobHeld>, StorageError> {
            Err(StorageError::Unavailable("stub".into()))
        }
    }

    /// The context defaults to no services. Attaching a composed set doubles
    /// as the compile-check that a flavor can implement the exported
    /// `CitedBlobPort` from `proxima::flavor` alone.
    #[test]
    fn test_context_has_no_blob_service_until_one_is_attached() {
        let ctx = FlavorWorkerContext::new_for_tests(
            std::sync::Arc::new(proxima_core::Engine::new(
                FlavorRegistry::new().freeze_or_panic_for_tests(),
            )),
            tokio_util::sync::CancellationToken::new(),
        );
        assert!(
            ctx.service::<CitedBlobService>().is_none(),
            "a bare test context wires no S3"
        );

        let ctx = ctx.with_services(FlavorServices::with(CitedBlobService(std::sync::Arc::new(
            StubBlobPort,
        ))));
        assert!(
            ctx.service::<CitedBlobService>().is_some(),
            "with_services attaches the service"
        );
    }

    #[tokio::test]
    async fn tuple_chains_spawn_workers_in_order_and_default_is_empty() {
        let ctx = FlavorWorkerContext::new_for_tests(
            std::sync::Arc::new(proxima_core::Engine::new(
                FlavorRegistry::new().freeze_or_panic_for_tests(),
            )),
            tokio_util::sync::CancellationToken::new(),
        );

        assert!(
            <(AlphaBundle, BetaBundle) as FlavorBundle>::spawn_workers(&ctx).is_empty(),
            "bundles on the default spawn_workers contribute nothing"
        );

        let workers = <(AlphaBundle, GammaBundle) as FlavorBundle>::spawn_workers(&ctx);
        let names: Vec<_> = workers.iter().map(|worker| worker.name).collect();
        assert_eq!(names, ["gamma-first", "gamma-second"]);

        ctx.cancel.cancel();
        for worker in workers {
            worker.handle.await.expect("worker terminates on cancel");
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
