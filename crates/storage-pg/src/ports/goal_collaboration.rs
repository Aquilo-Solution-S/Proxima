use proxiima_core::goal_collaboration::{
    GoalAudienceGrantInput, GoalCollaborationPort, GoalCommandPermit, GoalHandle,
    GoalPublicationInput, GoalWorkBindingInput, PublicationUse,
};
use proxiima_core::storage_ports::OwnerWritePermit;
use proxiima_core::{OwnerId, StorageError, UserId};
use sqlx::Row;
use std::time::SystemTime;

pub(crate) mod source_reads;

use crate::{PgStorage, error::map_err, owner_scope::begin_owner_transaction};

/// Lock lifecycle owners before the existing tree/kind authority gateway.
/// The returned roles are current facts, not a replacement `OwnerScope`.
pub(crate) async fn fenced_roles(
    storage: &PgStorage,
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    scope: &proxiima_core::OwnerScope,
    owners: &[OwnerId],
    extra_people: &[UserId],
) -> Result<proxiima_core::OwnerRoles, StorageError> {
    if scope.is_expired() || scope.goal_principal().is_none() {
        return Err(StorageError::NotFound);
    }
    crate::access::owner_columns::lock_owner_fences_shared_tx(tx, owners).await?;
    let mut people = extra_people.to_vec();
    people.push(scope.subject());
    let mut authority = storage
        .platform_scope
        .authority_on(
            tx,
            crate::AuthorityTargets {
                owners: owners.to_vec(),
                people,
                groups: Vec::new(),
            },
        )
        .await?;
    let roles = authority
        .person(scope.subject())
        .await?
        .effective_roles()
        .await?;
    Ok(roles)
}

pub(crate) fn current_role(
    scope: &proxiima_core::OwnerScope,
    roles: &proxiima_core::OwnerRoles,
    owner: OwnerId,
) -> Option<proxiima_core::Role> {
    if scope.is_expired() {
        return None;
    }
    Some(scope.role_for_owner(&owner)?.meet(roles.role_for(&owner)?))
}

pub(crate) fn source_admin(
    scope: &proxiima_core::OwnerScope,
    roles: &proxiima_core::OwnerRoles,
    owner: OwnerId,
) -> bool {
    current_role(scope, roles, owner).is_some_and(|role| {
        role.administers() && (owner == OwnerId::from(scope.subject()) || role.manages())
    })
}

fn ceiling(rank: i16) -> Result<proxiima_core::AccessCeiling, StorageError> {
    use proxiima_core::AccessCeiling;
    match rank {
        0 => Ok(AccessCeiling::None),
        1 => Ok(AccessCeiling::Fact),
        2 => Ok(AccessCeiling::Abstraction),
        3 => Ok(AccessCeiling::Perspective),
        4 => Ok(AccessCeiling::Goal),
        _ => Err(StorageError::ConstraintViolation(
            "invalid stored Goal ceiling".into(),
        )),
    }
}

async fn lock_goal_manager(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    permit: &GoalCommandPermit,
    roles: &proxiima_core::OwnerRoles,
) -> Result<Option<proxiima_core::Role>, StorageError> {
    let row = sqlx::query("SELECT owner_id, authority_generation FROM proxiima_core.goal_head WHERE handle=$1 FOR UPDATE")
        .bind(permit.goal().into_inner()).fetch_optional(&mut **tx).await.map_err(map_err)?
        .ok_or(StorageError::NotFound)?;
    let owner = OwnerId::new(row.get("owner_id"));
    if owner != permit.controller() {
        return Err(StorageError::NotFound);
    }
    Ok(
        current_goal_role(tx, permit, roles, row.get("authority_generation"))
            .await?
            .and_then(|role| {
                if permit.controller() == OwnerId::from(permit.subject()) && role.administers() {
                    Some(proxiima_core::Role::admin())
                } else {
                    role.manages().then_some(role)
                }
            }),
    )
}

/// Join the caller's captured-and-current owner Role with their current Goal
/// guest grant. Call only after owner authority and the Goal head are fenced.
pub(crate) async fn current_goal_role(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    permit: &GoalCommandPermit,
    roles: &proxiima_core::OwnerRoles,
    controller_generation: i64,
) -> Result<Option<proxiima_core::Role>, StorageError> {
    role_for_goal(
        tx,
        permit.scope(),
        roles,
        permit.goal(),
        permit.controller(),
        controller_generation,
    )
    .await
}

pub(crate) async fn role_for_goal(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    scope: &proxiima_core::OwnerScope,
    roles: &proxiima_core::OwnerRoles,
    goal: GoalHandle,
    controller: OwnerId,
    controller_generation: i64,
) -> Result<Option<proxiima_core::Role>, StorageError> {
    if scope.goal_principal().is_none() {
        return Err(StorageError::NotFound);
    }
    let owner_role = current_role(scope, roles, controller);
    let grant = sqlx::query("SELECT read_ceiling, write_ceiling, manage FROM proxiima_core.goal_access_grant WHERE goal_handle=$1 AND subject_id=$2 AND controller_generation=$3 AND (expires_at IS NULL OR expires_at > statement_timestamp())")
        .bind(goal.into_inner()).bind(scope.subject().into_inner())
        .bind(controller_generation).fetch_optional(&mut **tx).await.map_err(map_err)?;
    let grant_role = grant
        .map(|grant| {
            proxiima_core::Role::new(
                ceiling(grant.get("read_ceiling"))?,
                ceiling(grant.get("write_ceiling"))?,
                grant.get("manage"),
            )
            .map_err(|error| StorageError::ConstraintViolation(error.to_string()))
        })
        .transpose()?;
    Ok(match (owner_role, grant_role) {
        (Some(owner), Some(grant)) => Some(owner.join(grant)),
        (owner, grant) => owner.or(grant),
    })
}

fn require_manager(role: Option<proxiima_core::Role>) -> Result<proxiima_core::Role, StorageError> {
    role.ok_or(StorageError::NotFound)
}

fn role_ceiling(role: proxiima_core::Role) -> (i16, i16, bool) {
    (
        i16::from(role.read_ceiling().rank()),
        i16::from(role.write_ceiling().rank()),
        role.manages(),
    )
}

fn require_rows(rows: u64, operation: &str) -> Result<(), StorageError> {
    if rows == 0 {
        Err(StorageError::ConstraintViolation(format!(
            "Goal collaboration {operation} was not admitted"
        )))
    } else {
        Ok(())
    }
}

fn db_expiry(value: Option<SystemTime>) -> Result<Option<time::OffsetDateTime>, StorageError> {
    value
        .map(|instant| {
            let duration = instant
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_err(|_| {
                    StorageError::ConstraintViolation("expiry precedes Unix epoch".into())
                })?;
            let seconds = i64::try_from(duration.as_secs())
                .map_err(|_| StorageError::ConstraintViolation("expiry is out of range".into()))?;
            time::OffsetDateTime::from_unix_timestamp(seconds)
                .map_err(|_| StorageError::ConstraintViolation("expiry is out of range".into()))
        })
        .transpose()
}

fn retained_endpoint(
    rows: &[(uuid::Uuid, String)],
    id: uuid::Uuid,
) -> Result<proxiima_core::EdgeEndpoint, StorageError> {
    let kind = rows
        .iter()
        .find(|(stored, _)| *stored == id)
        .and_then(|(_, kind)| match kind.as_str() {
            "fact" => Some(proxiima_core::EntityKind::Fact),
            "abstraction" => Some(proxiima_core::EntityKind::Abstraction),
            "perspective" => Some(proxiima_core::EntityKind::Perspective),
            _ => None,
        })
        .ok_or(StorageError::NotFound)?;
    Ok(proxiima_core::EdgeEndpoint::memory(
        kind,
        proxiima_core::MemoryId::new(id),
    ))
}

#[async_trait::async_trait]
impl GoalCollaborationPort for PgStorage {
    async fn revision_pins(
        &self,
        permit: &proxiima_core::RevisionCopyPermit,
    ) -> Result<Option<proxiima_core::RevisionPinSnapshot>, StorageError> {
        // The exact prior is selected under the caller's original RLS scope.
        // Only after that admission is proven do we switch to the platform
        // lane to read the declared pin kinds; no ancestor bodies are loaded.
        let mut tx = self.platform_transaction().await?;
        crate::owner_scope::bind_owner_scope(&mut tx, permit.scope()).await?;
        let row = sqlx::query(
            "SELECT owner_id, forked_from, kind::text AS kind, origins, refs, goal_refs
             FROM proxiima_core.memory /* FORGET-POLICY: live-only (revision prior must still be writable) */
             WHERE t=$1 AND forgotten_at IS NULL AND forked_from IS NOT NULL
             ",
        )
        .bind(permit.prior().into_inner())
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_err)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let owner = OwnerId::new(row.get("owner_id"));
        let forked_from = proxiima_core::MemoryId::new(row.get::<uuid::Uuid, _>("forked_from"));
        let kind = match row.get::<String, _>("kind").as_str() {
            "abstraction" => proxiima_core::EntityKind::Abstraction,
            "perspective" => proxiima_core::EntityKind::Perspective,
            _ => {
                return Err(StorageError::ConstraintViolation(
                    "invalid forked revision kind".into(),
                ));
            }
        };
        if owner != permit.destination_owner() || kind != permit.kind() {
            return Ok(None);
        }
        let origin_ids: Vec<uuid::Uuid> = row.get("origins");
        let reference_ids: Vec<uuid::Uuid> = row.get("refs");
        let goal_ids: Vec<uuid::Uuid> = row.get("goal_refs");
        let mut ids = origin_ids.clone();
        ids.extend(reference_ids.iter().copied());
        self.platform_scope.bind_transaction(&mut tx).await?;
        let rows: Vec<(uuid::Uuid, String)> = sqlx::query_as(
            "SELECT t, kind::text FROM proxiima_core.memory /* FORGET-POLICY: includes-forgotten (retained pins need kind only) */
             WHERE t=ANY($1::uuid[])
             UNION ALL
             SELECT t, kind::text FROM proxiima_core.erased_pin_target
             WHERE t=ANY($1::uuid[]) AND kind IN ('fact', 'abstraction', 'perspective')",
        )
        .bind(&ids)
        .fetch_all(&mut *tx)
        .await
        .map_err(map_err)?;
        let origins = origin_ids
            .into_iter()
            .map(|id| retained_endpoint(&rows, id))
            .collect::<Result<Vec<_>, _>>()?;
        let references = reference_ids
            .into_iter()
            .map(|id| retained_endpoint(&rows, id))
            .collect::<Result<Vec<_>, _>>()?;
        let goal_references = goal_ids
            .iter()
            .copied()
            .map(proxiima_core::GoalId::new)
            .collect::<Vec<_>>();
        let mut references = references;
        references.extend(
            goal_references
                .iter()
                .copied()
                .map(proxiima_core::EdgeEndpoint::goal),
        );
        Ok(Some(proxiima_core::RevisionPinSnapshot::new(
            permit.prior(),
            owner,
            forked_from,
            kind,
            origins,
            references,
            goal_references,
        )))
    }

    async fn author_work_memory(
        &self,
        request: &proxiima_core::AuthorDerivedRequest<'_>,
        permit: &proxiima_core::GoalWorkPermit,
        proof: proxiima_core::storage_ports::OperatorWriteProof,
    ) -> Result<proxiima_core::AuthorDerivedOutcome, StorageError> {
        crate::verbs::goal_work::author_work_memory(self, request, permit, proof).await
    }

    async fn fork_memory(
        &self,
        request: &proxiima_core::ForkMemoryRequest,
        permit: &proxiima_core::ForkCommandPermit,
    ) -> Result<proxiima_core::ForkMemoryOutcome, StorageError> {
        crate::verbs::memory_fork::fork_memory(self, request, permit).await
    }

    async fn read_owner_candidates(
        &self,
        scope: &proxiima_core::OwnerScope,
    ) -> Result<Vec<OwnerId>, StorageError> {
        if scope.goal_principal().is_none() {
            return Ok(Vec::new());
        }
        let mut tx = begin_owner_transaction(&self.pool, scope).await?;
        let rows: Vec<(uuid::Uuid,)> = sqlx::query_as(
            "SELECT DISTINCT owner_id FROM proxiima_core.goal_visible_memory_ids()
             UNION
             SELECT DISTINCT g.owner_id FROM proxiima_core.goal g
               JOIN proxiima_core.goal_access_grant a ON a.goal_handle = g.handle
              WHERE a.subject_id = $1 AND a.read_ceiling >= 4",
        )
        .bind(scope.subject().into_inner())
        .fetch_all(&mut *tx)
        .await
        .map_err(map_err)?;
        tx.commit().await.map_err(map_err)?;
        Ok(rows.into_iter().map(|(id,)| OwnerId::new(id)).collect())
    }

    async fn controller_owner(
        &self,
        scope: &proxiima_core::OwnerScope,
        goal: GoalHandle,
    ) -> Result<Option<OwnerId>, StorageError> {
        if scope.is_expired() || scope.goal_principal().is_none() {
            return Ok(None);
        }
        let mut tx = self.platform_transaction().await?;
        let row = sqlx::query(
            "SELECT owner_id, authority_generation FROM proxiima_core.goal_head WHERE handle=$1",
        )
        .bind(goal.into_inner())
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_err)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let owner = OwnerId::new(row.get("owner_id"));
        let ambient = scope
            .role_for_owner(&owner)
            .is_some_and(|role| role.read_ceiling().top().is_some() || role.manages());
        let grant: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM proxiima_core.goal_access_grant WHERE goal_handle=$1 AND subject_id=$2 AND controller_generation=$3 AND (read_ceiling>0 OR manage) AND (expires_at IS NULL OR expires_at>statement_timestamp()))")
            .bind(goal.into_inner()).bind(scope.subject().into_inner())
            .bind(row.get::<i64,_>("authority_generation")).fetch_one(&mut *tx).await.map_err(map_err)?;
        let mut admins = scope.managed_owners();
        if scope
            .role_for_owner(&OwnerId::from(scope.subject()))
            .is_some_and(proxiima_core::Role::administers)
        {
            admins.push(OwnerId::from(scope.subject()));
        }
        let source: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM proxiima_core.goal_memory_publication WHERE goal_handle=$1 AND source_owner=ANY($2::uuid[])) OR EXISTS (SELECT 1 FROM proxiima_core.goal_work_binding WHERE goal_handle=$1 AND source_owner=ANY($2::uuid[]))")
            .bind(goal.into_inner()).bind(admins.into_iter().map(OwnerId::into_inner).collect::<Vec<_>>())
            .fetch_one(&mut *tx).await.map_err(map_err)?;
        tx.commit().await.map_err(map_err)?;
        Ok((ambient || grant || source).then_some(owner))
    }

    async fn goal_handle_for_version(
        &self,
        scope: &proxiima_core::OwnerScope,
        goal: proxiima_core::GoalId,
    ) -> Result<Option<GoalHandle>, StorageError> {
        if scope.is_expired() {
            return Ok(None);
        }
        let mut tx = self.platform_transaction().await?;
        crate::owner_scope::bind_owner_scope(&mut tx, scope).await?;
        let handle = sqlx::query_scalar::<_, uuid::Uuid>(
            "SELECT g.handle FROM proxiima_core.goal g WHERE g.t=$1",
        )
        .bind(goal.into_inner())
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_err)?
        .map(GoalHandle::new);
        tx.commit().await.map_err(map_err)?;
        Ok(handle)
    }

    async fn grant_access(
        &self,
        input: &GoalAudienceGrantInput,
        permit: &GoalCommandPermit,
    ) -> Result<(), StorageError> {
        if input.goal != permit.goal() {
            return Err(StorageError::ConstraintViolation(
                "Goal permit target mismatch".into(),
            ));
        }
        let (read, write, manage) = role_ceiling(input.role);
        let expires_at = db_expiry(input.expires_at)?;
        let mut tx = self.platform_transaction().await?;
        let roles = fenced_roles(
            self,
            &mut tx,
            permit.scope(),
            &[permit.controller()],
            &[input.subject],
        )
        .await?;
        let actor = require_manager(lock_goal_manager(&mut tx, permit, &roles).await?)?;
        let (actor_read, actor_write, actor_manage) = role_ceiling(actor);
        let (requested_read, requested_write, requested_manage) = role_ceiling(input.role);
        if requested_read > actor_read
            || requested_write > actor_write
            || (requested_manage && !actor_manage)
        {
            return Err(StorageError::ConstraintViolation(
                "Goal grant exceeds actor authority".into(),
            ));
        }
        crate::access::owner_columns::ensure_owner_row(&mut tx, &OwnerId::from(input.subject))
            .await?;
        let result = sqlx::query("INSERT INTO proxiima_core.goal_access_grant (goal_handle,subject_id,read_ceiling,write_ceiling,manage,controller_generation,expires_at,granted_by) SELECT $1,$2,$3,$4,$5,authority_generation,$6,$7 FROM proxiima_core.goal_head WHERE handle=$1 AND owner_id=$8 ON CONFLICT (goal_handle,subject_id) DO UPDATE SET read_ceiling=EXCLUDED.read_ceiling, write_ceiling=EXCLUDED.write_ceiling, manage=EXCLUDED.manage, controller_generation=EXCLUDED.controller_generation, expires_at=EXCLUDED.expires_at, granted_by=EXCLUDED.granted_by")
            .bind(input.goal.into_inner()).bind(input.subject.into_inner()).bind(read).bind(write).bind(manage).bind(expires_at).bind(permit.subject().into_inner()).bind(permit.controller().into_inner()).execute(&mut *tx).await.map_err(map_err)?;
        require_rows(result.rows_affected(), "grant")?;
        tx.commit().await.map_err(map_err)
    }

    async fn revoke_access(
        &self,
        goal: GoalHandle,
        subject: UserId,
        permit: &GoalCommandPermit,
    ) -> Result<(), StorageError> {
        if goal != permit.goal() {
            return Err(StorageError::ConstraintViolation(
                "Goal permit target mismatch".into(),
            ));
        }
        let mut tx = self.platform_transaction().await?;
        let roles =
            fenced_roles(self, &mut tx, permit.scope(), &[permit.controller()], &[]).await?;
        require_manager(lock_goal_manager(&mut tx, permit, &roles).await?)?;
        let result = sqlx::query(
            "DELETE FROM proxiima_core.goal_access_grant WHERE goal_handle=$1 AND subject_id=$2",
        )
        .bind(goal.into_inner())
        .bind(subject.into_inner())
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;
        require_rows(result.rows_affected(), "revoke")?;
        tx.commit().await.map_err(map_err)
    }

    async fn publish(
        &self,
        input: &GoalPublicationInput,
        permit: &GoalCommandPermit,
        source: &OwnerWritePermit,
    ) -> Result<(), StorageError> {
        if input.goal != permit.goal() {
            return Err(StorageError::ConstraintViolation(
                "Goal permit target mismatch".into(),
            ));
        }
        let mut tx = self.platform_transaction().await?;
        let roles = fenced_roles(
            self,
            &mut tx,
            permit.scope(),
            &[permit.controller(), *source.owner()],
            &[],
        )
        .await?;
        if source
            .owner_scope()
            .is_none_or(|scope| scope.subject() != permit.subject() || scope.is_expired())
            || !source_admin(permit.scope(), &roles, *source.owner())
        {
            return Err(StorageError::NotFound);
        }
        require_manager(lock_goal_manager(&mut tx, permit, &roles).await?)?;
        let live_source: Option<uuid::Uuid> = sqlx::query_scalar(
            "SELECT t FROM proxiima_core.memory /* FORGET-POLICY: live-only (publish only the current live source admission) */ WHERE t=$1 AND owner_id=$2 AND forgotten_at IS NULL FOR SHARE"
        ).bind(input.memory.into_inner()).bind(source.owner().into_inner())
            .fetch_optional(&mut *tx).await.map_err(map_err)?;
        if live_source.is_none() {
            return Err(StorageError::NotFound);
        }
        let use_value = match input.permitted_use {
            PublicationUse::Read => "read",
            PublicationUse::Fork => "fork",
        };
        let result = sqlx::query("INSERT INTO proxiima_core.goal_memory_publication (goal_handle,memory_t,source_owner,source_generation,controller_generation,permitted_use,published_by,cited_upload_id) SELECT $1,m.t,m.owner_id,mh.authority_generation,gh.authority_generation,$2::proxiima_core.goal_publication_use,$3,cited.upload_id FROM proxiima_core.memory m /* FORGET-POLICY: live-only (publication requires live exact source) */ JOIN proxiima_core.memory_head mh ON mh.handle=m.handle JOIN proxiima_core.goal_head gh ON gh.handle=$1 LEFT JOIN LATERAL (SELECT u.upload_id FROM proxiima_core.blob_uploads u WHERE u.blob_id=m.blob_id AND u.owner_id=m.owner_id AND u.status='completed' ORDER BY u.completed_at DESC NULLS LAST,u.upload_id DESC LIMIT 1) cited ON true WHERE m.t=$4 AND m.owner_id=$5 AND mh.owner_id=m.owner_id AND m.forgotten_at IS NULL AND gh.owner_id=$6 ON CONFLICT (goal_handle,memory_t) DO UPDATE SET cited_upload_id=EXCLUDED.cited_upload_id,permitted_use=EXCLUDED.permitted_use,source_generation=EXCLUDED.source_generation,controller_generation=EXCLUDED.controller_generation,published_by=EXCLUDED.published_by")
            .bind(input.goal.into_inner()).bind(use_value).bind(permit.subject().into_inner()).bind(input.memory.into_inner()).bind(source.owner().into_inner()).bind(permit.controller().into_inner()).execute(&mut *tx).await.map_err(map_err)?;
        require_rows(result.rows_affected(), "publish")?;
        tx.commit().await.map_err(map_err)
    }

    async fn unpublish(
        &self,
        goal: GoalHandle,
        memory: proxiima_core::MemoryId,
        permit: &GoalCommandPermit,
    ) -> Result<(), StorageError> {
        if goal != permit.goal() {
            return Err(StorageError::ConstraintViolation(
                "Goal permit target mismatch".into(),
            ));
        }
        let mut tx = self.platform_transaction().await?;
        let selected_source: Option<uuid::Uuid> = sqlx::query_scalar("SELECT source_owner FROM proxiima_core.goal_memory_publication WHERE goal_handle=$1 AND memory_t=$2")
            .bind(goal.into_inner()).bind(memory.into_inner()).fetch_optional(&mut *tx).await.map_err(map_err)?;
        let mut owners = vec![permit.controller()];
        owners.extend(selected_source.map(OwnerId::new));
        let roles = fenced_roles(self, &mut tx, permit.scope(), &owners, &[]).await?;
        let manager = lock_goal_manager(&mut tx, permit, &roles).await?;
        let actual_source: Option<uuid::Uuid> = sqlx::query_scalar("SELECT source_owner FROM proxiima_core.goal_memory_publication WHERE goal_handle=$1 AND memory_t=$2")
            .bind(goal.into_inner()).bind(memory.into_inner()).fetch_optional(&mut *tx).await.map_err(map_err)?;
        if actual_source != selected_source {
            return Err(StorageError::Retryable(
                "Goal consent source changed while acquiring authority fences".into(),
            ));
        }
        if manager.is_none()
            && !actual_source
                .is_some_and(|owner| source_admin(permit.scope(), &roles, OwnerId::new(owner)))
        {
            return Err(StorageError::NotFound);
        }
        let result = sqlx::query("DELETE FROM proxiima_core.goal_memory_publication WHERE goal_handle=$1 AND memory_t=$2").bind(goal.into_inner()).bind(memory.into_inner()).execute(&mut *tx).await.map_err(map_err)?;
        require_rows(result.rows_affected(), "unpublish")?;
        tx.commit().await.map_err(map_err)
    }

    async fn bind_work(
        &self,
        input: &GoalWorkBindingInput,
        permit: &GoalCommandPermit,
        source: &OwnerWritePermit,
    ) -> Result<(), StorageError> {
        if input.goal != permit.goal() {
            return Err(StorageError::ConstraintViolation(
                "Goal permit target mismatch".into(),
            ));
        }
        let mut tx = self.platform_transaction().await?;
        let roles = fenced_roles(
            self,
            &mut tx,
            permit.scope(),
            &[permit.controller(), *source.owner()],
            &[],
        )
        .await?;
        if source
            .owner_scope()
            .is_none_or(|scope| scope.subject() != permit.subject() || scope.is_expired())
            || !source_admin(permit.scope(), &roles, *source.owner())
        {
            return Err(StorageError::NotFound);
        }
        require_manager(lock_goal_manager(&mut tx, permit, &roles).await?)?;
        let result = sqlx::query("INSERT INTO proxiima_core.goal_work_binding (goal_handle,memory_handle,source_owner,source_generation,controller_generation,bound_by) SELECT $1,$2,mh.owner_id,mh.authority_generation,gh.authority_generation,$3 FROM proxiima_core.memory_head mh JOIN proxiima_core.goal_head gh ON gh.handle=$1 WHERE mh.handle=$2 AND mh.owner_id=$4 AND gh.owner_id=$4 ON CONFLICT (goal_handle,memory_handle) DO UPDATE SET source_generation=EXCLUDED.source_generation,controller_generation=EXCLUDED.controller_generation,bound_by=EXCLUDED.bound_by")
            .bind(input.goal.into_inner()).bind(input.series.into_inner()).bind(permit.subject().into_inner()).bind(source.owner().into_inner()).execute(&mut *tx).await.map_err(map_err)?;
        require_rows(result.rows_affected(), "bind")?;
        tx.commit().await.map_err(map_err)
    }

    async fn unbind_work(
        &self,
        input: &GoalWorkBindingInput,
        permit: &GoalCommandPermit,
    ) -> Result<(), StorageError> {
        if input.goal != permit.goal() {
            return Err(StorageError::ConstraintViolation(
                "Goal permit target mismatch".into(),
            ));
        }
        let mut tx = self.platform_transaction().await?;
        let selected_source: Option<uuid::Uuid> = sqlx::query_scalar("SELECT source_owner FROM proxiima_core.goal_work_binding WHERE goal_handle=$1 AND memory_handle=$2")
            .bind(input.goal.into_inner()).bind(input.series.into_inner()).fetch_optional(&mut *tx).await.map_err(map_err)?;
        let mut owners = vec![permit.controller()];
        owners.extend(selected_source.map(OwnerId::new));
        let roles = fenced_roles(self, &mut tx, permit.scope(), &owners, &[]).await?;
        let manager = lock_goal_manager(&mut tx, permit, &roles).await?;
        let actual_source: Option<uuid::Uuid> = sqlx::query_scalar("SELECT source_owner FROM proxiima_core.goal_work_binding WHERE goal_handle=$1 AND memory_handle=$2")
            .bind(input.goal.into_inner()).bind(input.series.into_inner()).fetch_optional(&mut *tx).await.map_err(map_err)?;
        if actual_source != selected_source {
            return Err(StorageError::Retryable(
                "Goal consent source changed while acquiring authority fences".into(),
            ));
        }
        if manager.is_none()
            && !actual_source
                .is_some_and(|owner| source_admin(permit.scope(), &roles, OwnerId::new(owner)))
        {
            return Err(StorageError::NotFound);
        }
        let result = sqlx::query(
            "DELETE FROM proxiima_core.goal_work_binding WHERE goal_handle=$1 AND memory_handle=$2",
        )
        .bind(input.goal.into_inner())
        .bind(input.series.into_inner())
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;
        require_rows(result.rows_affected(), "unbind")?;
        tx.commit().await.map_err(map_err)
    }
}
