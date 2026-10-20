//! Authority facts protected on a trusted host's own transaction.

use std::collections::{BTreeMap, BTreeSet};

use proxima_core::{GroupId, OwnerId, OwnerRoles, Relation, StorageError, UserId};
use sqlx::{PgConnection, Postgres, Transaction};
use uuid::Uuid;

use super::owner_columns::{lock_group_membership_tx, resolve_membership};
use crate::error::map_err;

/// The complete selection for one host authority operation, including the
/// groups and people its membership edits will touch. These IDs select facts;
/// they grant no owner authorization.
#[derive(Debug, Clone, Default)]
pub struct AuthorityTargets {
    pub groups: Vec<GroupId>,
    pub people: Vec<UserId>,
    pub owners: Vec<OwnerId>,
}

/// One directly written person relation, without inherited roles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectUserMembership {
    pub user: UserId,
    pub relation: Relation,
}

/// One directly written group relation. In an ancestry result the group is
/// the parent reached by that step; in a child roster it is the child.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectGroupMembership {
    pub group: GroupId,
    pub relation: Relation,
}

/// The registered kind of an existing owner, read under its kind fence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegisteredOwner {
    Personal(UserId),
    Group(GroupId),
}

/// A directional topology fact, distinct from a membership relation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupContainment {
    SameGroup,
    ContainsDescendant,
    Unrelated,
}

#[derive(Debug, Clone, Copy, sqlx::Type)]
#[sqlx(type_name = "proxima_core.owner_kind", rename_all = "lowercase")]
enum PgOwnerKind {
    Personal,
    Group,
}

/// Fenced authority facts on the caller's actual transaction.
///
/// Construct through [`crate::PgPlatformScope::authority_on`]. The host remains
/// responsible for its own conditional writes, commit and whole-transaction
/// retry. `PostgreSQL` retains all fences until that transaction ends, even if
/// this Rust borrow is dropped. Results are facts, not authorization witnesses.
///
/// Declare the complete selection before construction. Host binding locks
/// precede this gateway; host revision/claim mutation locks follow it. Keep
/// membership edits within the declared selection. A newly discovered target
/// requires restarting the transaction, rather than acquiring a later tree
/// lock. Arbitrary earlier host row locks retain `PostgreSQL`'s ordinary
/// deadlock/serialization retry requirements.
///
/// ```no_run
/// # use proxima_storage_pg::{AuthorityTargets, PgPlatformScope};
/// # use sqlx::{Postgres, Transaction};
/// # async fn use_facts(scope: &PgPlatformScope, tx: &mut Transaction<'_, Postgres>) -> Result<(), proxima_core::StorageError> {
/// let mut facts = scope.authority_on(tx, AuthorityTargets::default()).await?;
/// let _ = facts.transaction();
/// # Ok(()) }
/// ```
///
/// ```compile_fail
/// # use proxima_storage_pg::{AuthorityTargets, PgPlatformScope};
/// # use sqlx::{Postgres, Transaction};
/// # async fn commit_while_borrowed(scope: &PgPlatformScope, mut tx: Transaction<'_, Postgres>) {
/// let mut facts = scope.authority_on(&mut tx, AuthorityTargets::default()).await.unwrap();
/// tx.commit().await.unwrap();
/// let _ = facts.transaction();
/// # }
/// ```
#[derive(Debug)]
pub struct PgAuthoritySession<'session, 'connection> {
    tx: &'session mut Transaction<'connection, Postgres>,
    groups: BTreeSet<GroupId>,
    people: BTreeSet<UserId>,
    owners: BTreeSet<OwnerId>,
}

impl<'session, 'connection> PgAuthoritySession<'session, 'connection> {
    pub(crate) async fn new(
        platform: &crate::PgPlatformScope,
        tx: &'session mut Transaction<'connection, Postgres>,
        targets: AuthorityTargets,
    ) -> Result<Self, StorageError> {
        platform.bind_transaction(tx).await?;
        let isolation: String = sqlx::query_scalar("SHOW transaction_isolation")
            .fetch_one(&mut **tx)
            .await
            .map_err(map_err)?;
        if isolation != "read committed" {
            return Err(StorageError::MembershipIsolation);
        }
        let groups: BTreeSet<_> = targets.groups.into_iter().collect();
        let people: BTreeSet<_> = targets.people.into_iter().collect();
        let owners: BTreeSet<_> = targets.owners.into_iter().collect();
        let selected: Vec<_> = groups
            .iter()
            .map(|id| id.into_inner())
            .chain(people.iter().map(|id| id.into_inner()))
            .chain(owners.iter().map(|id| id.into_inner()))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut footprint: BTreeSet<_> = selected.iter().copied().collect();
        footprint.extend(direct_groups_for_ids(tx, &selected).await?);
        let roots = tree_roots(tx, &footprint.into_iter().collect::<Vec<_>>()).await?;
        let protected: BTreeSet<_> = roots.values().copied().collect();
        for root in &protected {
            // The existing write gateway also takes a personal member's UUID
            // as a tree key before determining its kind. Keep those private
            // pseudoroots in this same acquisition phase.
            lock_group_membership_tx(tx, GroupId::new(*root)).await?;
        }
        let ids: Vec<_> = roots.keys().copied().collect();
        if tree_roots(tx, &ids).await? != roots {
            return Err(changed_footprint());
        }
        let subjects: Vec<_> = people.iter().map(|id| id.into_inner()).collect();
        sqlx::query("SELECT proxima_core.lock_authority_targets($1, $2)")
            .bind(&ids)
            .bind(&subjects)
            .execute(&mut **tx)
            .await
            .map_err(map_err)?;

        // A preceding writer can add a direct group while construction waits.
        // Never obtain its new tree after taking subject or host claim locks.
        let current_groups: Vec<_> = direct_groups_for_ids(tx, &selected)
            .await?
            .into_iter()
            .collect();
        if tree_roots(tx, &current_groups)
            .await?
            .values()
            .any(|root| !protected.contains(root))
        {
            return Err(changed_footprint());
        }
        for group in &groups {
            match owner_kind(tx, OwnerId::from(*group)).await? {
                Some(RegisteredOwner::Group(_)) => {}
                Some(RegisteredOwner::Personal(_)) => {
                    return Err(refused("authority group is registered as a person"));
                }
                None => return Err(StorageError::NotFound),
            }
        }
        for person in &people {
            require_person(tx, *person).await?;
        }
        for owner in &owners {
            owner_kind(tx, *owner)
                .await?
                .ok_or(StorageError::NotFound)?;
        }
        Ok(Self {
            tx,
            groups,
            people,
            owners,
        })
    }

    /// The original transaction for the host's own SQL and declared membership
    /// edits. This gateway never commits, rolls back or retries it.
    pub fn transaction(&mut self) -> &mut Transaction<'connection, Postgres> {
        self.tx
    }

    /// Select a declared, registered group without taking any later locks.
    ///
    /// # Errors
    ///
    /// Refuses an undeclared group.
    pub fn group(&mut self, group: GroupId) -> Result<PgAuthorityGroup<'_>, StorageError> {
        self.require_group(group)?;
        Ok(PgAuthorityGroup {
            conn: self.tx,
            group,
        })
    }

    /// Select a declared person. An unregistered person is valid for effective
    /// role resolution; a registered group is refused. The kind is read again
    /// so earlier host writes on this transaction are visible.
    ///
    /// # Errors
    ///
    /// Refuses undeclared people and group-registered subjects; returns database
    /// failures without changing transaction ownership.
    pub async fn person(&mut self, person: UserId) -> Result<PgAuthorityPerson<'_>, StorageError> {
        if !self.people.contains(&person) {
            return Err(undeclared());
        }
        require_person(self.tx, person).await?;
        Ok(PgAuthorityPerson {
            conn: self.tx,
            person,
        })
    }

    /// Read a selected owner's current registered kind. An absent registry row
    /// is refused rather than returning a durable negative registration fact.
    ///
    /// # Errors
    ///
    /// Refuses undeclared owners and returns [`StorageError::NotFound`] for an
    /// unregistered person selected only for effective-role reads.
    pub async fn registered_owner(
        &mut self,
        owner: OwnerId,
    ) -> Result<RegisteredOwner, StorageError> {
        if !self.owners.contains(&owner)
            && !self.groups.contains(&GroupId::new(owner.into_inner()))
            && !self.people.contains(&UserId::new(owner.into_inner()))
        {
            return Err(undeclared());
        }
        owner_kind(self.tx, owner)
            .await?
            .ok_or(StorageError::NotFound)
    }

    /// Whether one declared group contains another. Equality has its own
    /// result and does not invent a self-membership relation.
    ///
    /// # Errors
    ///
    /// Refuses undeclared groups and returns database failures.
    pub async fn containment(
        &mut self,
        ancestor: GroupId,
        descendant: GroupId,
    ) -> Result<GroupContainment, StorageError> {
        self.require_group(ancestor)?;
        self.require_group(descendant)?;
        if ancestor == descendant {
            return Ok(GroupContainment::SameGroup);
        }
        let ancestors = self.group(descendant)?.ancestors().await?;
        Ok(if ancestors.iter().any(|step| step.group == ancestor) {
            GroupContainment::ContainsDescendant
        } else {
            GroupContainment::Unrelated
        })
    }

    fn require_group(&self, group: GroupId) -> Result<(), StorageError> {
        if self.groups.contains(&group) {
            Ok(())
        } else {
            Err(undeclared())
        }
    }
}

/// A declared group borrowing the fenced transaction. Each read sees earlier
/// changes on that same transaction; no authority answers are cached.
#[derive(Debug)]
pub struct PgAuthorityGroup<'session> {
    conn: &'session mut PgConnection,
    group: GroupId,
}

impl PgAuthorityGroup<'_> {
    #[must_use]
    pub const fn id(&self) -> GroupId {
        self.group
    }

    /// Direct users in stable user/relation order, optionally one relation.
    /// Child groups and inherited memberships are separate facts.
    ///
    /// # Errors
    ///
    /// Returns database failures.
    pub async fn direct_users(
        &mut self,
        relation: Option<Relation>,
    ) -> Result<Vec<DirectUserMembership>, StorageError> {
        let rows: Vec<(Uuid, Relation)> = sqlx::query_as(
            "SELECT member_user_id, relation FROM proxima_core.group_memberships
              WHERE group_id = $1
                AND ($2::proxima_core.membership_relation IS NULL OR relation = $2)
              ORDER BY member_user_id, relation",
        )
        .bind(self.group.into_inner())
        .bind(relation)
        .fetch_all(&mut *self.conn)
        .await
        .map_err(map_err)?;
        Ok(rows
            .into_iter()
            .map(|(user, relation)| DirectUserMembership {
                user: UserId::new(user),
                relation,
            })
            .collect())
    }

    /// Count distinct direct users under the same relation predicate as
    /// [`Self::direct_users`]. A user with several relations counts once.
    ///
    /// # Errors
    ///
    /// Returns database failures.
    pub async fn direct_user_count(
        &mut self,
        relation: Option<Relation>,
    ) -> Result<i64, StorageError> {
        sqlx::query_scalar(
            "SELECT count(DISTINCT member_user_id) FROM proxima_core.group_memberships
              WHERE group_id = $1
                AND ($2::proxima_core.membership_relation IS NULL OR relation = $2)",
        )
        .bind(self.group.into_inner())
        .bind(relation)
        .fetch_one(&mut *self.conn)
        .await
        .map_err(map_err)
    }

    /// Direct child groups in group order, without interpreting them as users.
    ///
    /// # Errors
    ///
    /// Returns database failures.
    pub async fn direct_child_groups(
        &mut self,
    ) -> Result<Vec<DirectGroupMembership>, StorageError> {
        let rows: Vec<(Uuid, Relation)> = sqlx::query_as(
            "SELECT child_group_id, relation FROM proxima_core.group_edges
              WHERE parent_group_id = $1 ORDER BY child_group_id",
        )
        .bind(self.group.into_inner())
        .fetch_all(&mut *self.conn)
        .await
        .map_err(map_err)?;
        Ok(group_memberships(rows))
    }

    /// The named principal's directly written relations, from either the
    /// person-membership or child-group source. Inherited roles do not enter.
    ///
    /// # Errors
    ///
    /// Returns database failures.
    pub async fn direct_relations(
        &mut self,
        member: OwnerId,
    ) -> Result<Vec<Relation>, StorageError> {
        sqlx::query_scalar(
            "SELECT relation FROM proxima_core.group_memberships
              WHERE group_id = $1 AND member_user_id = $2
             UNION ALL
             SELECT relation FROM proxima_core.group_edges
              WHERE parent_group_id = $1 AND child_group_id = $2
             ORDER BY relation",
        )
        .bind(self.group.into_inner())
        .bind(member.into_inner())
        .fetch_all(&mut *self.conn)
        .await
        .map_err(map_err)
    }

    /// The direct parent and the relation written on that edge, if attached.
    ///
    /// # Errors
    ///
    /// Returns database failures.
    pub async fn parent(&mut self) -> Result<Option<DirectGroupMembership>, StorageError> {
        let row: Option<(Uuid, Relation)> = sqlx::query_as(
            "SELECT parent_group_id, relation FROM proxima_core.group_edges
              WHERE child_group_id = $1",
        )
        .bind(self.group.into_inner())
        .fetch_optional(&mut *self.conn)
        .await
        .map_err(map_err)?;
        Ok(row.map(|(group, relation)| DirectGroupMembership {
            group: GroupId::new(group),
            relation,
        }))
    }

    /// Each parent step from the immediate parent to the root. Each relation
    /// belongs to that step's edge, without computing an effective role.
    ///
    /// # Errors
    ///
    /// Returns database failures.
    pub async fn ancestors(&mut self) -> Result<Vec<DirectGroupMembership>, StorageError> {
        let rows: Vec<(Uuid, Relation)> = sqlx::query_as(
            "WITH RECURSIVE ancestors(group_id, relation, depth) AS (
                 SELECT parent_group_id, relation, 1
                   FROM proxima_core.group_edges WHERE child_group_id = $1
                 UNION ALL
                 SELECT e.parent_group_id, e.relation, a.depth + 1
                   FROM ancestors a JOIN proxima_core.group_edges e
                     ON e.child_group_id = a.group_id
             ) SELECT group_id, relation FROM ancestors ORDER BY depth",
        )
        .bind(self.group.into_inner())
        .fetch_all(&mut *self.conn)
        .await
        .map_err(map_err)?;
        Ok(group_memberships(rows))
    }

    /// The current root through the existing tree resolver.
    ///
    /// # Errors
    ///
    /// Returns database failures.
    pub async fn root(&mut self) -> Result<GroupId, StorageError> {
        sqlx::query_scalar::<_, Uuid>("SELECT proxima_core.group_tree_root($1)")
            .bind(self.group.into_inner())
            .fetch_one(&mut *self.conn)
            .await
            .map(GroupId::new)
            .map_err(map_err)
    }
}

/// A declared person borrowing the fenced transaction. Direct relations and
/// effective inherited roles are read separately from their existing sources.
#[derive(Debug)]
pub struct PgAuthorityPerson<'session> {
    conn: &'session mut PgConnection,
    person: UserId,
}

impl PgAuthorityPerson<'_> {
    #[must_use]
    pub const fn id(&self) -> UserId {
        self.person
    }

    /// Reverse direct memberships in stable group/relation order.
    ///
    /// # Errors
    ///
    /// Returns database failures.
    pub async fn direct_groups(&mut self) -> Result<Vec<DirectGroupMembership>, StorageError> {
        let rows: Vec<(Uuid, Relation)> = sqlx::query_as(
            "SELECT group_id, relation FROM proxima_core.group_memberships
              WHERE member_user_id = $1 ORDER BY group_id, relation",
        )
        .bind(self.person.into_inner())
        .fetch_all(&mut *self.conn)
        .await
        .map_err(map_err)?;
        Ok(group_memberships(rows))
    }

    /// Effective roles through the existing projection decoder and kernel
    /// `OwnerRoles` constructor, including the subject's personal owner.
    ///
    /// # Errors
    ///
    /// Returns database failures and invalid membership-role refusals.
    pub async fn effective_roles(&mut self) -> Result<OwnerRoles, StorageError> {
        let rows = resolve_membership(self.conn, &OwnerId::from(self.person)).await?;
        OwnerRoles::for_subject(
            self.person,
            rows.into_iter()
                .map(|row| (OwnerId::from(row.group), row.role)),
        )
        .map_err(|error| refused(&error.to_string()))
    }
}

fn group_memberships(rows: Vec<(Uuid, Relation)>) -> Vec<DirectGroupMembership> {
    rows.into_iter()
        .map(|(group, relation)| DirectGroupMembership {
            group: GroupId::new(group),
            relation,
        })
        .collect()
}

async fn owner_kind(
    conn: &mut PgConnection,
    owner: OwnerId,
) -> Result<Option<RegisteredOwner>, StorageError> {
    let kind: Option<PgOwnerKind> =
        sqlx::query_scalar("SELECT kind FROM proxima_core.owners WHERE owner_id = $1")
            .bind(owner.into_inner())
            .fetch_optional(conn)
            .await
            .map_err(map_err)?;
    Ok(kind.map(|kind| match kind {
        PgOwnerKind::Personal => RegisteredOwner::Personal(UserId::new(owner.into_inner())),
        PgOwnerKind::Group => RegisteredOwner::Group(GroupId::new(owner.into_inner())),
    }))
}

async fn require_person(conn: &mut PgConnection, person: UserId) -> Result<(), StorageError> {
    match owner_kind(conn, OwnerId::from(person)).await? {
        Some(RegisteredOwner::Group(_)) => {
            Err(refused("authority subject is registered as a group"))
        }
        Some(RegisteredOwner::Personal(_)) | None => Ok(()),
    }
}

async fn direct_groups_for_ids(
    conn: &mut PgConnection,
    ids: &[Uuid],
) -> Result<BTreeSet<Uuid>, StorageError> {
    let groups: Vec<Uuid> = sqlx::query_scalar(
        "SELECT DISTINCT group_id FROM proxima_core.group_memberships
          WHERE member_user_id = ANY($1::uuid[])",
    )
    .bind(ids)
    .fetch_all(conn)
    .await
    .map_err(map_err)?;
    Ok(groups.into_iter().collect())
}

async fn tree_roots(
    conn: &mut PgConnection,
    ids: &[Uuid],
) -> Result<BTreeMap<Uuid, Uuid>, StorageError> {
    let roots: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT id, proxima_core.group_tree_root(id) FROM unnest($1::uuid[]) AS ids(id)",
    )
    .bind(ids)
    .fetch_all(conn)
    .await
    .map_err(map_err)?;
    Ok(roots.into_iter().collect())
}

fn changed_footprint() -> StorageError {
    StorageError::Retryable("authority footprint changed while acquiring fences".into())
}

fn undeclared() -> StorageError {
    refused("authority target was not declared before fencing")
}

fn refused(message: &str) -> StorageError {
    StorageError::ConstraintViolation(message.to_owned())
}
