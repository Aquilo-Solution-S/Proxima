//! Fresh authority for an edit to one existing Goal, without owner-role widening.

use proxiima_core::goal_collaboration::GoalEditPermit;
use proxiima_core::verbs::goal_write::GoalEvidenceRef;
use proxiima_core::{
    AccessKind, EdgeEndpoint, EntityKind, GoalCommandPermit, GoalId, MemoryId, OwnerRoles,
    StorageError,
};
use sqlx::{Postgres, Transaction};

use crate::ports::goal_collaboration::source_reads::SourceReads;
use crate::ports::goal_collaboration::{current_goal_role, fenced_roles};
use crate::{PgStorage, error::map_err, pg_enums::PgMemoryKind};

pub(crate) struct GoalEditFence<'a> {
    command: &'a GoalCommandPermit,
    prior: GoalId,
    roles: OwnerRoles,
    sources: SourceReads,
    endpoints: Vec<EdgeEndpoint>,
}

impl GoalEditFence<'_> {
    pub(crate) async fn revalidate(
        &self,
        storage: &PgStorage,
        tx: &mut Transaction<'_, Postgres>,
    ) -> Result<(), StorageError> {
        let generation: Option<i64> = sqlx::query_scalar(
            "SELECT h.authority_generation FROM proxiima_core.goal_head h
             JOIN proxiima_core.goal g ON g.handle=h.handle AND g.owner_id=h.owner_id
             WHERE h.handle=$1 AND h.owner_id=$2 AND g.t=$3",
        )
        .bind(self.command.goal().into_inner())
        .bind(self.command.controller().into_inner())
        .bind(self.prior.into_inner())
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_err)?;
        let generation = generation.ok_or(StorageError::NotFound)?;
        let role = current_goal_role(tx, self.command, &self.roles, generation)
            .await?
            .ok_or(StorageError::NotFound)?;
        if !role.may_write(AccessKind::Goal) {
            return Err(StorageError::Forbidden("Goal-level write required".into()));
        }
        self.sources
            .revalidate(
                storage,
                tx,
                self.command.scope(),
                &self.roles,
                &self.endpoints,
            )
            .await
    }
}

/// Select the existing owner transaction or fence a single Goal route. New
/// source statements are admitted under the original caller RLS before any
/// platform materialization; carried prior statements are not new reads.
pub(crate) async fn begin_goal_edit<'a>(
    storage: &PgStorage,
    permit: GoalEditPermit<'a>,
    prior: GoalId,
    evidence: &[GoalEvidenceRef],
    author_self: Option<MemoryId>,
) -> Result<(Transaction<'static, Postgres>, Option<GoalEditFence<'a>>), StorageError> {
    let GoalEditPermit::Goal(command) = permit else {
        return Ok((
            crate::owner_scope::begin_owner_transaction(
                &storage.pool,
                crate::owner_scope::require_owner_scope(permit.scope())?,
            )
            .await?,
            None,
        ));
    };
    let mut tx = storage.platform_transaction().await?;
    crate::owner_scope::bind_owner_scope(&mut tx, command.scope()).await?;
    let local: Vec<_> = evidence
        .iter()
        .filter(|item| item.remote_origin().is_none())
        .map(|item| item.memory_id().into_inner())
        .collect();
    let rows: Vec<(uuid::Uuid, PgMemoryKind)> = sqlx::query_as(
        "SELECT t,kind FROM proxiima_core.memory /* FORGET-POLICY: live-only (new Goal evidence under owner RLS) */
         WHERE t=ANY($1::uuid[]) AND forgotten_at IS NULL",
    ).bind(&local).fetch_all(&mut *tx).await.map_err(map_err)?;
    if local
        .iter()
        .any(|id| !rows.iter().any(|(found, _)| found == id))
    {
        return Err(StorageError::NotFound);
    }
    let mut endpoints: Vec<_> = rows
        .into_iter()
        .map(|(id, kind)| EdgeEndpoint::memory(EntityKind::from(kind), MemoryId::new(id)))
        .collect();
    endpoints.push(EdgeEndpoint::goal(prior));
    if let Some(id) = author_self {
        endpoints.push(EdgeEndpoint::memory(EntityKind::Perspective, id));
    }
    let sources = SourceReads::discover(storage, &mut tx, command.scope(), &endpoints).await?;
    let owners: Vec<_> = sources.owners().chain([command.controller()]).collect();
    let roles = fenced_roles(storage, &mut tx, command.scope(), &owners, &[]).await?;
    sources
        .lock_goals(&mut tx, (command.goal(), command.controller()))
        .await?;
    let fence = GoalEditFence {
        command,
        prior,
        roles,
        sources,
        endpoints,
    };
    fence.revalidate(storage, &mut tx).await?;
    Ok((tx, Some(fence)))
}

pub(crate) async fn revalidate_goal_edit(
    storage: &PgStorage,
    tx: &mut Transaction<'_, Postgres>,
    fence: Option<&GoalEditFence<'_>>,
) -> Result<(), StorageError> {
    if let Some(fence) = fence {
        fence.revalidate(storage, tx).await?;
    }
    Ok(())
}
