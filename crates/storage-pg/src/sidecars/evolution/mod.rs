//! Spiice-owned transformations, executed only by the migration runner.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use proxiima_core::spiice::SchemaEvolution;
use proxiima_core::verbs::schema::PayloadKind;
use proxiima_core::{
    AbstractionPayload, FactPayload, GoalId, GoalPayload, PerspectivePayload, SchemaVersion,
    SidecarPayload, StorageError,
};
use sqlx::{PgConnection, Postgres, Transaction};

use super::{
    PgGoalSidecar, PgMemoryPayload, PgMemorySidecar, PgSidecarKey, PgSidecarReadCtx,
    PgSidecarRegistry, PgSidecarRegistryFrozen,
};

mod apply;
mod collaboration;
mod derived;
mod execute;
mod finish;
mod graph;
mod plan;
mod read;
mod rows;
mod target_rows;
mod validate;
mod write;

#[cfg(test)]
mod blob_pg_tests;
#[cfg(test)]
mod fence_pg_tests;
#[cfg(test)]
mod graph_pg_tests;

pub use plan::{
    MigrationAdmission, MigrationGoalPins, MigrationInput, MigrationPins, MigrationPlacement,
    MigrationPlan, MigrationSeries,
};
pub(crate) use read::read_sources;
pub use rows::{MigrationBlob, MigrationGoal, MigrationMemory};

pub trait PgMigrationGoalPayload: GoalPayload + Sized {
    fn load<'a>(ctx: PgSidecarReadCtx<'a>, ids: &'a [GoalId]) -> GoalPayloadFuture<'a, Self>;
}

type GoalPayloadFuture<'a, P> =
    Pin<Box<dyn Future<Output = Result<Vec<(GoalId, P)>, StorageError>> + Send + 'a>>;

type GoalLoaderFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<(GoalId, SidecarPayload)>, StorageError>> + Send + 'a>>;
type GoalLoader = for<'a> fn(PgSidecarReadCtx<'a>, &'a [GoalId]) -> GoalLoaderFuture<'a>;

/// A pure transformation of one owner's source records. The runner owns all
/// database access, identities, rollback and activation; a callback receives no
/// connection, transaction, engine, or publication port.
pub type SchemaMigrationFn = fn(&MigrationInput, &mut MigrationPlan) -> Result<(), StorageError>;

/// Sidecar codecs retained exclusively as migration inputs/intermediate outputs.
/// These registrations never enter the ordinary serving registry.
#[derive(Debug, Clone, Default)]
pub struct PgMigrationSchemas {
    registry: PgSidecarRegistry,
    goal_loaders: HashMap<PgSidecarKey, GoalLoader>,
}

impl PgMigrationSchemas {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_fact<P: FactPayload + PgMemorySidecar + PgMemoryPayload>(&mut self) {
        self.registry.add_fact::<P>();
    }
    pub fn add_abstraction<P: AbstractionPayload + PgMemorySidecar + PgMemoryPayload>(&mut self) {
        self.registry.add_abstraction::<P>();
    }
    pub fn add_perspective<P: PerspectivePayload + PgMemorySidecar + PgMemoryPayload>(&mut self) {
        self.registry.add_perspective::<P>();
    }
    pub fn add_goal<P: GoalPayload + PgGoalSidecar>(&mut self) {
        self.registry.add_goal::<P>();
    }

    pub fn add_goal_source<P: GoalPayload + PgGoalSidecar + PgMigrationGoalPayload>(&mut self) {
        self.registry.add_goal::<P>();
        let key = PgSidecarKey::new(
            PayloadKind::Goal,
            <P as GoalPayload>::schema_id(),
            SchemaVersion::new(<P as GoalPayload>::SCHEMA_VERSION),
        );
        self.goal_loaders.insert(key, load_goal::<P>);
    }
}

pub(super) const fn kind_name(kind: PayloadKind) -> &'static str {
    match kind {
        PayloadKind::Fact => "fact",
        PayloadKind::Abstraction => "abstraction",
        PayloadKind::Perspective => "perspective",
        PayloadKind::Goal => "goal",
        PayloadKind::CitedObject => "cited_object",
    }
}

fn load_goal<'a, P: PgMigrationGoalPayload>(
    ctx: PgSidecarReadCtx<'a>,
    ids: &'a [GoalId],
) -> GoalLoaderFuture<'a> {
    Box::pin(async move {
        Ok(P::load(ctx, ids)
            .await?
            .into_iter()
            .map(|(id, payload)| (id, SidecarPayload::goal(payload)))
            .collect())
    })
}

/// The executable bound to the very declaration the core catalog publishes.
#[derive(Debug, Clone)]
pub struct PgSchemaEvolution {
    pub(super) declaration: &'static SchemaEvolution,
    pub(super) schemas: PgMigrationSchemas,
    pub(super) transform: SchemaMigrationFn,
}

type FrozenEvolutionSet = (
    Arc<[SchemaEvolution]>,
    Arc<HashMap<&'static str, PgSchemaEvolution>>,
);

impl PgSchemaEvolution {
    #[must_use]
    pub fn new(
        declaration: &'static SchemaEvolution,
        schemas: PgMigrationSchemas,
        transform: SchemaMigrationFn,
    ) -> Self {
        Self {
            declaration,
            schemas,
            transform,
        }
    }
}

pub(super) fn freeze_evolutions(
    registry: &proxiima_core::SpiiceRegistryFrozen,
    programs: HashMap<&'static str, PgSchemaEvolution>,
) -> Result<FrozenEvolutionSet, StorageError> {
    let declarations: Vec<_> = registry.evolutions().iter().map(|decl| **decl).collect();
    for declaration in &declarations {
        let program = programs.get(declaration.id).ok_or_else(|| {
            refused(format!(
                "schema evolution {} has no executable migration",
                declaration.id
            ))
        })?;
        if program.declaration != declaration {
            return Err(refused(format!(
                "schema evolution {} executable disagrees with its contract",
                declaration.id
            )));
        }
        execute::check_codecs(declaration, &program.schemas)?;
    }
    if let Some(id) = programs
        .keys()
        .find(|id| !declarations.iter().any(|decl| decl.id == **id))
    {
        return Err(refused(format!(
            "executable schema evolution {id} has no declared contract"
        )));
    }
    Ok((declarations.into(), Arc::new(programs)))
}

impl PgSidecarRegistryFrozen {
    #[must_use]
    pub fn schema_evolutions(&self) -> &[SchemaEvolution] {
        &self.evolutions
    }

    /// List unapplied executable steps from the same declaration graph the runner uses.
    ///
    /// # Errors
    /// Refuses a changed completed declaration or a stale serving composition.
    pub async fn pending_schema_evolutions(
        &self,
        connection: &mut PgConnection,
    ) -> Result<Vec<SchemaEvolution>, StorageError> {
        execute::pending(self, connection).await
    }

    /// Validate the persisted completion graph without installing or migrating.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the completion graph is missing or stale.
    pub async fn ensure_schema_evolutions(
        &self,
        connection: &mut PgConnection,
    ) -> Result<(), StorageError> {
        execute::ensure(self, connection).await
    }

    /// Execute one declared step on the runner's already-held transaction.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the typed migration cannot be applied.
    pub async fn run_schema_evolution(
        &self,
        transaction: &mut Transaction<'_, Postgres>,
        declaration: &SchemaEvolution,
    ) -> Result<(), StorageError> {
        execute::run(self, transaction, declaration).await
    }

    /// Flush completed-step data constraints while retaining the authority
    /// permits and deferred mode needed by the next migration step.
    ///
    /// # Errors
    ///
    /// Returns a storage error when a deferred data constraint cannot be
    /// validated or restored.
    pub async fn checkpoint_schema_evolution(
        &self,
        transaction: &mut Transaction<'_, Postgres>,
    ) -> Result<(), StorageError> {
        finish::checkpoint_data_constraints(self, transaction).await
    }

    /// Verify the complete mapping closure before activating any target.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the mapping closure or activation state is invalid.
    pub async fn finish_schema_evolutions(
        &self,
        transaction: &mut Transaction<'_, Postgres>,
    ) -> Result<(), StorageError> {
        execute::finish(self, transaction).await
    }
}

fn refused(message: impl Into<String>) -> StorageError {
    StorageError::ConstraintViolation(message.into())
}
