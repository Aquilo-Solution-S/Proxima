//! Exact source reads carried through the collaboration transaction's fences.

use proxiima_core::{
    AccessKind, EdgeEndpoint, EntityKind, GoalHandle, OwnerId, OwnerRoles, OwnerScope, StorageError,
};
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

use crate::{PgStorage, error::map_err};

#[derive(Debug, PartialEq, Eq)]
struct Source {
    id: Uuid,
    owner: OwnerId,
    kind: AccessKind,
    route: Option<(GoalHandle, OwnerId)>,
}

/// Discovery is a selector, not a permission. Revalidate after acquiring the
/// complete owner and Goal footprint, on the transaction that writes the row.
pub(crate) struct SourceReads {
    sources: Vec<Source>,
}

impl SourceReads {
    pub(crate) async fn discover(
        storage: &PgStorage,
        tx: &mut Transaction<'_, Postgres>,
        scope: &OwnerScope,
        endpoints: &[EdgeEndpoint],
    ) -> Result<Self, StorageError> {
        crate::owner_scope::bind_owner_scope(tx, scope).await?;
        let memories: Vec<_> = endpoints
            .iter()
            .filter_map(|e| e.memory_id())
            .map(proxiima_core::MemoryId::into_inner)
            .collect();
        let goals: Vec<_> = endpoints
            .iter()
            .filter(|e| e.kind == EntityKind::Goal)
            .map(|e| e.entity_id())
            .collect();
        let rows = sqlx::query(
            "WITH routes AS MATERIALIZED (SELECT * FROM proxiima_core.goal_memory_read_routes())
             SELECT m.t, m.owner_id, m.kind::text AS kind, r.goal_handle, r.controller_owner
               FROM proxiima_core.memory m /* FORGET-POLICY: live-only (source admission under owner RLS) */
               LEFT JOIN LATERAL (SELECT goal_handle, controller_owner FROM routes
                                  WHERE memory_t=m.t ORDER BY goal_handle LIMIT 1) r ON true
              WHERE m.t=ANY($1::uuid[]) AND m.forgotten_at IS NULL
             UNION ALL
             SELECT g.t, g.owner_id, 'goal', g.handle, g.owner_id
               FROM proxiima_core.goal g WHERE g.t=ANY($2::uuid[])
             ORDER BY t",
        )
        .bind(&memories)
        .bind(&goals)
        .fetch_all(&mut **tx)
        .await
        .map_err(map_err)?;
        let mut sources = Vec::with_capacity(rows.len());
        for row in rows {
            let kind = match row.get::<String, _>("kind").as_str() {
                "fact" => AccessKind::Fact,
                "abstraction" => AccessKind::Abstraction,
                "perspective" => AccessKind::Perspective,
                "goal" => AccessKind::Goal,
                _ => {
                    return Err(StorageError::ConstraintViolation(
                        "unknown source kind".into(),
                    ));
                }
            };
            let route = row
                .get::<Option<Uuid>, _>("goal_handle")
                .zip(row.get::<Option<Uuid>, _>("controller_owner"))
                .map(|(goal, owner)| (GoalHandle::new(goal), OwnerId::new(owner)));
            sources.push(Source {
                id: row.get("t"),
                owner: OwnerId::new(row.get("owner_id")),
                kind,
                route,
            });
        }
        storage.platform_scope.bind_transaction(tx).await?;
        if endpoints.iter().any(|e| {
            !sources
                .iter()
                .any(|s| s.id == e.entity_id() && s.kind == AccessKind::from(e.kind))
        }) {
            return Err(StorageError::NotFound);
        }
        Ok(Self { sources })
    }

    pub(crate) fn owners(&self) -> impl Iterator<Item = OwnerId> + '_ {
        self.sources
            .iter()
            .flat_map(|s| [Some(s.owner), s.route.map(|(_, owner)| owner)])
            .flatten()
    }

    /// Lock Goal heads in one order, including the destination. All Goal
    /// grant, publication and binding commands take this same head lock.
    pub(crate) async fn lock_goals(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        destination: (GoalHandle, OwnerId),
    ) -> Result<(), StorageError> {
        let goals = self.goal_routes().chain([destination]).collect();
        lock_goal_footprint(tx, goals).await
    }

    pub(crate) fn goal_routes(&self) -> impl Iterator<Item = (GoalHandle, OwnerId)> + '_ {
        self.sources.iter().filter_map(|s| s.route)
    }

    pub(crate) async fn revalidate(
        &self,
        storage: &PgStorage,
        tx: &mut Transaction<'_, Postgres>,
        scope: &OwnerScope,
        roles: &OwnerRoles,
        endpoints: &[EdgeEndpoint],
    ) -> Result<(), StorageError> {
        let current = Self::discover(storage, tx, scope, endpoints).await?;
        if self.sources != current.sources {
            return Err(StorageError::Retryable(
                "source read authority footprint changed".into(),
            ));
        }
        for source in &self.sources {
            let owner_read = super::current_role(scope, roles, source.owner)
                .is_some_and(|role| role.may_read(source.kind));
            let goal_read = if let Some((goal, controller)) = source.route {
                let generation: i64 = sqlx::query_scalar("SELECT authority_generation FROM proxiima_core.goal_head WHERE handle=$1 AND owner_id=$2")
                    .bind(goal.into_inner()).bind(controller.into_inner()).fetch_one(&mut **tx).await.map_err(map_err)?;
                super::role_for_goal(tx, scope, roles, goal, controller, generation)
                    .await?
                    .is_some_and(|role| role.may_read(source.kind))
            } else {
                false
            };
            if !owner_read && !goal_read {
                return Err(StorageError::NotFound);
            }
            if source.kind != AccessKind::Goal {
                let row: Option<Uuid> = sqlx::query_scalar("SELECT owner_id FROM proxiima_core.memory /* FORGET-POLICY: live-only (commit source liveness) */ WHERE t=$1 AND forgotten_at IS NULL FOR SHARE")
                    .bind(source.id).fetch_optional(&mut **tx).await.map_err(map_err)?;
                if row != Some(source.owner.into_inner()) {
                    return Err(StorageError::NotFound);
                }
            }
        }
        Ok(())
    }
}

/// Acquire the complete Goal footprint in stable order, after owner fences.
pub(crate) async fn lock_goal_footprint(
    tx: &mut Transaction<'_, Postgres>,
    mut goals: Vec<(GoalHandle, OwnerId)>,
) -> Result<(), StorageError> {
    goals.sort_by_key(|(goal, _)| goal.into_inner());
    goals.dedup();
    for (goal, expected) in goals {
        let owner: Option<Uuid> = sqlx::query_scalar(
            "SELECT owner_id FROM proxiima_core.goal_head WHERE handle=$1 FOR UPDATE",
        )
        .bind(goal.into_inner())
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_err)?;
        if owner != Some(expected.into_inner()) {
            return Err(StorageError::Retryable(
                "source Goal authority footprint changed".into(),
            ));
        }
    }
    Ok(())
}
