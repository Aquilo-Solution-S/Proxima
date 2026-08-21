//! Direct `OwnerRef` column helpers.

use proxima_core::{
    EntityId, GroupId, MembershipRow, OwnerRef, OwnerRefKind, Relation, StorageError, UserId,
};
use sqlx::{PgPool, Postgres, Transaction};

use crate::error::{internal, map_err, with_bounded_retry};

#[must_use]
pub fn owner_binds(owner: &OwnerRef) -> (OwnerRefKind, uuid::Uuid) {
    owner.columns()
}

#[must_use]
pub fn owner_arrays(owners: &[OwnerRef]) -> (Vec<OwnerRefKind>, Vec<uuid::Uuid>) {
    owners.iter().map(owner_binds).unzip()
}

/// Insert `proxima_core.owners` or confirm the stored kind matches `owner`.
///
/// The only production owners upsert. Memory / goal / wake / citation /
/// cited-blob writes all go through this so an `owner_id` reused under a
/// different kind cannot silently keep the first kind.
///
/// # Errors
///
/// [`StorageError::ConstraintViolation`] when `owner_id` already exists
/// with a different kind. Other storage errors from the upsert.
pub async fn ensure_owner_row(
    conn: &mut sqlx::PgConnection,
    owner: &OwnerRef,
) -> Result<uuid::Uuid, StorageError> {
    let owner_id = owner.stored_owner_id();
    let owner_kind = OwnerRefKind::of(owner).as_str();
    // Two statements: `owners` is append-only (no UPDATE), and a CTE that
    // `DO NOTHING` then SELECTs shares one snapshot — a concurrent first
    // insert waits, then neither arm sees the committed row (`RowNotFound`
    // on complete_upload under concurrency). The SELECT is a new statement
    // so it sees the row the waiter just conflicted with.
    let inserted: Option<String> = sqlx::query_scalar(
        "INSERT INTO proxima_core.owners (owner_id, kind)
         VALUES ($1, $2::proxima_core.owner_kind)
         ON CONFLICT (owner_id) DO NOTHING
         RETURNING kind::text",
    )
    .bind(owner_id)
    .bind(owner_kind)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_err)?;
    let existing = match inserted {
        Some(kind) => kind,
        None => {
            sqlx::query_scalar("SELECT kind::text FROM proxima_core.owners WHERE owner_id = $1")
                .bind(owner_id)
                .fetch_one(&mut *conn)
                .await
                .map_err(map_err)?
        }
    };
    if existing != owner_kind {
        return Err(StorageError::ConstraintViolation(
            "owners.kind conflict for owner_id".into(),
        ));
    }
    Ok(owner_id)
}

/// # Errors
///
/// Returns `Internal` on sqlx failure.
pub(crate) async fn resolve_membership(
    pool: &PgPool,
    owner: &OwnerRef,
) -> Result<Vec<MembershipRow>, StorageError> {
    let OwnerRef::Personal(user) = owner else {
        return Ok(Vec::new());
    };

    let rows: Vec<(uuid::Uuid, Relation)> = sqlx::query_as(
        "SELECT group_id, relation
           FROM proxima_core.group_memberships
          WHERE member_user_id = $1
          ORDER BY group_id, relation",
    )
    .bind(user.into_inner())
    .fetch_all(pool)
    .await
    .map_err(map_err)?;

    Ok(rows
        .into_iter()
        .map(|(group, relation)| MembershipRow {
            group: GroupId::new(group),
            relation,
        })
        .collect())
}

/// # Errors
///
/// Returns `Internal` on sqlx failure.
pub(crate) async fn lock_group_membership_tx(
    tx: &mut Transaction<'_, Postgres>,
    group_id: GroupId,
) -> Result<(), StorageError> {
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended('proxima-group-membership:' || $1::text, 0))",
    )
    .bind(group_id.into_inner())
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(())
}

/// # Errors
///
/// Returns `Conflict` when any Admin row already exists for the group, and
/// `Internal` on sqlx failure.
pub(crate) async fn bootstrap_group_admin(
    pool: &PgPool,
    group_id: GroupId,
    first_admin_user_id: UserId,
    _granted_by: uuid::Uuid,
) -> Result<(), StorageError> {
    // Retry the whole transaction on transient deadlock/serialization,
    // like the other atomic writers.
    with_bounded_retry(move || async move {
        let mut tx = pool.begin().await.map_err(internal)?;
        lock_group_membership_tx(&mut tx, group_id).await?;
        let inserted = sqlx::query(
            "INSERT INTO proxima_core.group_memberships
                (group_id, member_user_id, relation)
             SELECT $1, $2, $3
              WHERE NOT EXISTS (
                  SELECT 1
                    FROM proxima_core.group_memberships
                   WHERE group_id = $1
                     AND relation = $3
              )",
        )
        .bind(group_id.into_inner())
        .bind(first_admin_user_id.into_inner())
        .bind(Relation::Admin)
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;

        if inserted.rows_affected() == 0 {
            tx.rollback().await.map_err(map_err)?;
            return Err(StorageError::Conflict(
                "group already has an Admin membership".into(),
            ));
        }

        tx.commit().await.map_err(map_err)?;
        Ok(())
    })
    .await
}

/// # Errors
///
/// Returns `Internal` on sqlx failure.
pub(crate) async fn add_group_member(
    pool: &PgPool,
    group_id: GroupId,
    member_user_id: UserId,
    relation: Relation,
    _granted_by: uuid::Uuid,
) -> Result<(), StorageError> {
    // Retry the whole transaction on a transient deadlock/serialization
    // failure, matching every other atomic writer.
    with_bounded_retry(move || async move {
        let mut tx = pool.begin().await.map_err(internal)?;
        lock_group_membership_tx(&mut tx, group_id).await?;
        sqlx::query(
            "INSERT INTO proxima_core.group_memberships
                (group_id, member_user_id, relation)
             VALUES ($1, $2, $3)
             ON CONFLICT (group_id, member_user_id, relation) DO NOTHING",
        )
        .bind(group_id.into_inner())
        .bind(member_user_id.into_inner())
        .bind(relation)
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;
        tx.commit().await.map_err(map_err)?;
        Ok(())
    })
    .await
}

/// # Errors
///
/// Returns `Internal` on sqlx failure.
pub(crate) async fn remove_group_member(
    pool: &PgPool,
    group_id: GroupId,
    member_user_id: UserId,
) -> Result<(), StorageError> {
    // Retry the whole transaction on a transient deadlock/serialization
    // failure, matching every other atomic writer.
    with_bounded_retry(move || async move {
        let mut tx = pool.begin().await.map_err(internal)?;
        lock_group_membership_tx(&mut tx, group_id).await?;
        sqlx::query(
            "DELETE FROM proxima_core.group_memberships
              WHERE group_id = $1
                AND member_user_id = $2",
        )
        .bind(group_id.into_inner())
        .bind(member_user_id.into_inner())
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;
        tx.commit().await.map_err(map_err)?;
        Ok(())
    })
    .await
}

/// True iff `member_user_id` currently holds exactly `relation` on `group_id`.
/// A point-in-time single-role probe, distinct from
/// [`resolve_membership`]'s full row enumeration.
///
/// # Errors
///
/// Returns `Internal` on sqlx failure.
pub(crate) async fn has_group_relation(
    pool: &PgPool,
    group_id: GroupId,
    member_user_id: UserId,
    relation: Relation,
) -> Result<bool, StorageError> {
    sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1
               FROM proxima_core.group_memberships
              WHERE group_id = $1
                AND member_user_id = $2
                AND relation = $3
         )",
    )
    .bind(group_id.into_inner())
    .bind(member_user_id.into_inner())
    .bind(relation)
    .fetch_one(pool)
    .await
    .map_err(map_err)
}

/// # Errors
///
/// Returns `Internal` on sqlx failure.
pub(crate) async fn list_group_members(
    pool: &PgPool,
    group_id: GroupId,
) -> Result<Vec<(UserId, Relation)>, StorageError> {
    // Same total order as `list_group_members_page`. The table has no
    // `created_at` (0001_v008 PK is `(group_id, member_user_id, relation)`).
    let rows: Vec<(uuid::Uuid, Relation)> = sqlx::query_as(
        "SELECT member_user_id, relation
           FROM proxima_core.group_memberships
          WHERE group_id = $1
          ORDER BY member_user_id, relation",
    )
    .bind(group_id.into_inner())
    .fetch_all(pool)
    .await
    .map_err(map_err)?;

    Ok(rows
        .into_iter()
        .map(|(member, relation)| (UserId::new(member), relation))
        .collect())
}

/// One page of group members in the keyset total order
/// `(member_user_id, relation)`, starting strictly after `after` when
/// given. Fetches at most `limit` rows; callers over-fetch by one to
/// detect further pages. Relation ties order by the
/// `proxima_core.membership_relation` enum definition.
///
/// # Errors
///
/// Returns `Internal` on sqlx failure.
pub(crate) async fn list_group_members_page(
    pool: &PgPool,
    group_id: GroupId,
    after: Option<(UserId, Relation)>,
    limit: i64,
) -> Result<Vec<(UserId, Relation)>, StorageError> {
    let after_member = after.map(|(member, _)| member.into_inner());
    let after_relation = after.map(|(_, relation)| relation);
    let rows: Vec<(uuid::Uuid, Relation)> = sqlx::query_as(
        "SELECT member_user_id, relation
           FROM proxima_core.group_memberships
          WHERE group_id = $1
            AND ($2::uuid IS NULL
                 OR (member_user_id, relation)
                    > ($2::uuid, $3::proxima_core.membership_relation))
          ORDER BY member_user_id, relation
          LIMIT $4",
    )
    .bind(group_id.into_inner())
    .bind(after_member)
    .bind(after_relation)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;

    Ok(rows
        .into_iter()
        .map(|(member, relation)| (UserId::new(member), relation))
        .collect())
}

/// Home owner when the row is in `read_owners`. Absent and foreign are
/// both `None`.
///
/// # Errors
///
/// Returns `Internal` on sqlx failure.
pub(crate) async fn visible_home_owner(
    pool: &PgPool,
    entity: EntityId,
    read_owners: &[OwnerRef],
) -> Result<Option<OwnerRef>, StorageError> {
    if read_owners.is_empty() {
        return Ok(None);
    }

    let owner_ids: Vec<uuid::Uuid> = read_owners
        .iter()
        .copied()
        .map(OwnerRef::stored_owner_id)
        .collect();
    let row: Option<(OwnerRefKind, uuid::Uuid)> = sqlx::query_as(
        "SELECT o.kind::text::proxima_core.owner_kind, m.owner_id
           FROM proxima_core.memory m
           JOIN proxima_core.owners o ON o.owner_id = m.owner_id
          WHERE m.t = $1 AND m.owner_id = ANY($2::uuid[])
         UNION ALL
         SELECT o.kind::text::proxima_core.owner_kind, g.owner_id
           FROM proxima_core.goal g
           JOIN proxima_core.owners o ON o.owner_id = g.owner_id
          WHERE g.t = $1 AND g.owner_id = ANY($2::uuid[])
         LIMIT 1",
    )
    .bind(entity.uuid())
    .bind(&owner_ids)
    .fetch_optional(pool)
    .await
    .map_err(map_err)?;

    Ok(row.map(|(kind, id)| kind.with_uuid(id)))
}

/// Transfer one memory **series** to `to_owner`.
///
/// Same `(handle, t)`: a transfer is an owner UPDATE, not a copy. Head and
/// every version on the handle move together (`MemoryHeadAligned`), including
/// cooled stubs (owner only: cold keys are owner-free).
/// Returns `true` iff a row under `from_owner` matched and was updated.
///
/// Most sidecar rows stay keyed by `t` and so follow the memory to
/// `to_owner`. Owner-pinned sidecars are the exception and this transaction
/// does not touch them at all. `mcp_call_logged_v1` is the one today: it
/// describes the ACTOR of a tool call (`actor_upn`), not the memory, and it
/// carries its own `owner_id`, stamped at write time with the owner that
/// made the call. The row stays under that owner, so the source keeps
/// answering "what did my agents do" after giving the memory away.
///
/// The destination never sees it, because every read of an owner-pinned
/// sidecar is scoped by the sidecar's OWN owner rather than the memory's:
/// hydrate joins `memory` on `m.owner_id = s.owner_id` so a moved memory
/// stops matching, `read_mcp_call_history` selects on `fact.owner_id` with
/// no `memory` join at all, and compliance erase/export select the same
/// way. Forget skips them in both directions, so cooling or forgetting a
/// received memory cannot dump or destroy the source's audit trail.
///
/// Cited `blob` rows move when no other live series under a different owner
/// still cites them. Embeddings / jobs follow the transferred `t`s so ANN
/// (`emb.owner_id`) stays Tesla-valve. `ingest_keys` for those `t`s are
/// deleted so the prior owner can mint a new series. The same transaction
/// announces the transfer under both lanes: the prior owner's (the series
/// left their owned view) and the destination's (it arrived).
///
/// Goals do not transfer — the engine refuses them before storage; this
/// backstop keeps a direct storage call from transferring one.
///
/// # Errors
///
/// `Conflict` when a cited blob is still referenced by another live
/// series. `ConstraintViolation` for a Goal entity, and for unique /
/// check violations.
pub(crate) async fn transfer_to_owner(
    pool: &PgPool,
    sidecars: &crate::sidecars::PgSidecarRegistryFrozen,
    entity: EntityId,
    from_owner: OwnerRef,
    to_owner: OwnerRef,
) -> Result<bool, StorageError> {
    let memory_id = match entity {
        EntityId::Memory(memory_id) => memory_id,
        EntityId::Goal(_) => {
            return Err(StorageError::ConstraintViolation(
                "goals do not transfer: a goal series cannot change owner".into(),
            ));
        }
    };
    if from_owner == to_owner {
        return Err(StorageError::ConstraintViolation(
            "transfer destination is the current owner".into(),
        ));
    }
    let from_id = from_owner.stored_owner_id();
    let projection_tables = sidecars.projection_tables();
    let projection_tables = &projection_tables;
    with_bounded_retry(move || async move {
        let mut tx = pool.begin().await.map_err(internal)?;
        let transferred = transfer_memory_t(
            &mut tx,
            projection_tables,
            memory_id.into_inner(),
            from_id,
            to_owner,
        )
        .await?;
        if !transferred {
            // A false persist can follow real writes: when the head is gone
            // or changed owner after the series reads, cooled/blob/content
            // rows are already re-homed to the destination in this
            // transaction with no announce row. Roll back so they revert to
            // the prior owner.
            tx.rollback().await.map_err(map_err)?;
            return Ok(false);
        }
        tx.commit().await.map_err(map_err)?;
        Ok(transferred)
    })
    .await
}

async fn transfer_memory_t(
    tx: &mut Transaction<'_, Postgres>,
    projection_tables: &[String],
    t: uuid::Uuid,
    from_id: uuid::Uuid,
    to_owner: OwnerRef,
) -> Result<bool, StorageError> {
    let handle: Option<uuid::Uuid> = sqlx::query_scalar(
        "SELECT handle FROM proxima_core.memory
          WHERE t = $1 AND owner_id = $2",
    )
    .bind(t)
    .bind(from_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_err)?;
    let Some(handle) = handle else {
        return Ok(false);
    };
    transfer_memory_handle(tx, projection_tables, handle, from_id, to_owner).await
}

const SERIES_TS_SQL: &str = "SELECT t FROM proxima_core.memory WHERE handle = $1 AND owner_id = $2
     UNION
     SELECT t FROM proxima_core.cooled WHERE handle = $1 AND owner_id = $2";

/// Lock-acquisition rounds for [`lock_series_ts`] before it gives up with
/// `Retryable`. Growth between rounds needs a concurrent same-series ingest
/// inside a statement-sized window, so one extra round is already rare;
/// three keep an adversary appending versions in a tight loop from wedging
/// a transfer — the bounded work ends in a typed transient error, never in
/// an unlocked t.
const MAX_SERIES_LOCK_ROUNDS: usize = 3;

/// Serialize the transfer against forget before any row write: hold the
/// per-memory advisory lock the forget path takes, over every `t` of the
/// series, so a concurrent cool cannot land between the series reads and
/// the head persist. The set can drift between a read and the locks it
/// names — a forget keeps the hot-union-cooled set stable, an erase
/// shrinks it, an ingest of a new version grows it — so lock and re-read
/// until the locked set covers the read set, acquiring only the missing
/// `t`s each round in sorted order (xact-scoped advisory locks cannot be
/// released; the set only grows). Two transfers acquiring
/// overlapping-but-different sets across rounds can deadlock; that
/// surfaces as 40P01 → `Retryable`, and `with_bounded_retry` re-runs the
/// whole transaction — the established clean path, same as the round
/// budget running out.
///
/// Returns the authoritative (fully locked) set; empty means the series
/// left the owner while locking.
async fn lock_series_ts(
    tx: &mut Transaction<'_, Postgres>,
    handle: uuid::Uuid,
    from_id: uuid::Uuid,
    mut ts: Vec<uuid::Uuid>,
) -> Result<Vec<uuid::Uuid>, StorageError> {
    let mut locked = std::collections::BTreeSet::new();
    for _ in 0..MAX_SERIES_LOCK_ROUNDS {
        let missing: Vec<uuid::Uuid> = ts.iter().copied().filter(|t| !locked.contains(t)).collect();
        if missing.is_empty() {
            return Ok(ts);
        }
        crate::verbs::forget::lock_forget_memories_tx(tx, &missing).await?;
        locked.extend(missing);
        ts = sqlx::query_scalar(SERIES_TS_SQL)
            .bind(handle)
            .bind(from_id)
            .fetch_all(&mut **tx)
            .await
            .map_err(map_err)?;
        if ts.is_empty() {
            return Ok(ts);
        }
    }
    if ts.iter().all(|t| locked.contains(t)) {
        return Ok(ts);
    }
    Err(StorageError::Retryable(
        "series kept growing new versions while acquiring transfer locks".into(),
    ))
}

async fn transfer_memory_handle(
    tx: &mut Transaction<'_, Postgres>,
    projection_tables: &[String],
    handle: uuid::Uuid,
    from_id: uuid::Uuid,
    to_owner: OwnerRef,
) -> Result<bool, StorageError> {
    // The destination's `owners` row is no longer migration-seeded, so mint
    // it (or confirm its kind) BEFORE any statement below binds `to_id` into
    // an `owner_id` FK — `blob`, `content`, `cooled`, `memory_head`,
    // `memory`, `sketch`, embeddings and the announce lanes all reference
    // `proxima_core.owners`.
    let to_id = ensure_owner_row(tx.as_mut(), &to_owner).await?;
    let ts: Vec<uuid::Uuid> = sqlx::query_scalar(SERIES_TS_SQL)
        .bind(handle)
        .bind(from_id)
        .fetch_all(&mut **tx)
        .await
        .map_err(map_err)?;
    if ts.is_empty() {
        return Ok(false);
    }
    let ts = lock_series_ts(tx, handle, from_id, ts).await?;
    if ts.is_empty() {
        return Ok(false);
    }
    let expected_head_t: Option<uuid::Uuid> = sqlx::query_scalar(
        "SELECT t FROM proxima_core.memory_head
          WHERE handle = $1 AND owner_id = $2",
    )
    .bind(handle)
    .bind(from_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_err)?;
    let Some(expected_head_t) = expected_head_t else {
        return Ok(false);
    };
    if !ts.contains(&expected_head_t) {
        return Err(StorageError::Retryable(
            "series head advanced after transfer locked its version set".into(),
        ));
    }
    transfer_cited_blobs(tx, handle, from_id, to_id).await?;
    transfer_content_for_handle(tx, handle, from_id, to_id).await?;
    rehome_cooled_for_handle(tx, handle, from_id, to_id).await?;
    if persist_hot_series_transfer(
        tx,
        projection_tables,
        handle,
        from_id,
        to_id,
        expected_head_t,
        &ts,
    )
    .await?
    {
        announce_series_transfer(tx, handle, from_id, to_id).await?;
        return Ok(true);
    }
    Ok(false)
}

/// Cooled stubs change owner and nothing else.
///
/// Object keys are owner-free (`cold/<t>`), so the bytes a cooled row
/// points at are already correct for whoever holds the row — the transfer
/// performs no object-store work at all. This replaced a re-mint that
/// GET+PUT each cold object to a destination-derived key and then deleted
/// the source copy, which made an owner move O(bytes), non-atomic with the
/// row write, and dependent on the object store being up.
async fn rehome_cooled_for_handle(
    tx: &mut Transaction<'_, Postgres>,
    handle: uuid::Uuid,
    from_id: uuid::Uuid,
    to_id: uuid::Uuid,
) -> Result<(), StorageError> {
    sqlx::query(
        "UPDATE proxima_core.cooled
            SET owner_id = $3
          WHERE handle = $1 AND owner_id = $2",
    )
    .bind(handle)
    .bind(from_id)
    .bind(to_id)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(())
}

async fn persist_hot_series_transfer(
    tx: &mut Transaction<'_, Postgres>,
    projection_tables: &[String],
    handle: uuid::Uuid,
    from_id: uuid::Uuid,
    to_id: uuid::Uuid,
    expected_head_t: uuid::Uuid,
    ts: &[uuid::Uuid],
) -> Result<bool, StorageError> {
    let head = sqlx::query(
        "UPDATE proxima_core.memory_head
            SET owner_id = $3
          WHERE handle = $1 AND owner_id = $2 AND t = $4",
    )
    .bind(handle)
    .bind(from_id)
    .bind(to_id)
    .bind(expected_head_t)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    if head.rows_affected() == 0 {
        let owner_still_matches: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                 SELECT 1 FROM proxima_core.memory_head
                  WHERE handle = $1 AND owner_id = $2
             )",
        )
        .bind(handle)
        .bind(from_id)
        .fetch_one(&mut **tx)
        .await
        .map_err(map_err)?;
        if owner_still_matches {
            return Err(StorageError::Retryable(
                "series head advanced before transfer could persist".into(),
            ));
        }
        return Ok(false);
    }
    sqlx::query(
        "UPDATE proxima_core.memory
            SET owner_id = $3
          WHERE handle = $1 AND owner_id = $2",
    )
    .bind(handle)
    .bind(from_id)
    .bind(to_id)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    sqlx::query("UPDATE proxima_core.sketch SET owner_id = $2 WHERE t = ANY($1::uuid[])")
        .bind(ts)
        .bind(to_id)
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    // The projection is the first memory-keyed surface with its own
    // `owner_id`, so it is the first that does not follow implicitly. The
    // list is the frozen sidecar registry's, never a literal: a flavor that
    // declares a projection follows for free.
    for table in projection_tables {
        // SQL-POLICY: generated
        sqlx::query(sqlx::AssertSqlSafe(
            crate::projection::projection_transfer_sql(table)?,
        ))
        .bind(ts)
        .bind(to_id)
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    }
    follow_embedding_owners(tx, ts, to_id).await?;
    // NOTE: owner-pinned sidecars are deliberately untouched here.
    // `mcp_call_logged_v1` carries `actor_upn` and its own `owner_id`,
    // stamped with the owner that made the call. It stays where it is: the
    // source keeps answering "what did my agents do" after giving the
    // Memory away, and the destination never sees it, because every read of
    // it filters on the sidecar's own owner. Deleting the rows here — the
    // shape this replaced — destroyed audit history that Art. 17 and the
    // owner's own export are both entitled to.
    sqlx::query("DELETE FROM proxima_core.ingest_keys WHERE t = ANY($1::uuid[])")
        .bind(ts)
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    Ok(true)
}

/// One `'transfer'` announce row per lane, same series `(handle, head t)`:
/// the prior owner's projectors learn the series left their owned view, and
/// the destination's pull consumers learn it arrived. Both `announce.owner_id`
/// FKs hold because `transfer_memory_handle` called `ensure_owner_row` for
/// the destination at the top of this same transaction (the prior owner's row
/// already exists — it owns the series being read).
async fn announce_series_transfer(
    tx: &mut Transaction<'_, Postgres>,
    handle: uuid::Uuid,
    from_id: uuid::Uuid,
    to_id: uuid::Uuid,
) -> Result<(), StorageError> {
    let head_t: uuid::Uuid = sqlx::query_scalar(
        "SELECT t FROM proxima_core.memory_head
          WHERE handle = $1 AND owner_id = $2",
    )
    .bind(handle)
    .bind(to_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(map_err)?;
    sqlx::query(
        "INSERT INTO proxima_core.announce (owner_id, op, entity, handle, t)
         VALUES ($1, 'transfer', 'memory', $3, $4),
                ($2, 'transfer', 'memory', $3, $4)",
    )
    .bind(from_id)
    .bind(to_id)
    .bind(handle)
    .bind(head_t)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(())
}

/// Move the handle's cited blobs to the destination, deduping rather than
/// refusing when the bytes are shared.
///
/// `TransferRule::FollowOrDedupe` on `proxima_core.blob`, three cases:
///
/// 1. **The destination already holds these bytes.** Its own row wins;
///    this handle's references are repointed at it and the source keeps
///    whatever it still uses. Before the arm this raised a UNIQUE
///    violation on `(owner_id, schema_id, content_hash)` — the move would
///    collide with the row already sitting there — which is a second bug
///    the arm closes.
/// 2. **Nothing else references the bytes.** Move the rows in place, which
///    is exactly what this did before the arm existed: same `blob_id`, same
///    `upload_id`, same object, no mount. The common case does not pay for
///    the uncommon one, and a citation id a client already holds does not
///    move under it.
/// 3. **Another owner's live series references the bytes.** This is the
///    case that used to be `Conflict`. The destination gets a row of its
///    own and an upload row that MOUNTS the source's object — OCI's
///    cross-repo blob mount, where a mount is an optimisation over a copy
///    and never a correctness requirement. Nothing in S3 is read, written
///    or copied; ownership is metadata over an immutable store.
///
/// The source's rows are never deleted here. Deleting a blob row decides
/// the fate of an S3 object, and a transfer has no object-store handle in
/// scope; erase does, and its purge is refcounted precisely so a shared
/// object survives one owner leaving. An unreferenced source row is the
/// source owner's own row, reachable by its own erase and reported by
/// reconcile.
async fn transfer_cited_blobs(
    tx: &mut Transaction<'_, Postgres>,
    handle: uuid::Uuid,
    from_id: uuid::Uuid,
    to_id: uuid::Uuid,
) -> Result<(), StorageError> {
    let blob_ids: Vec<uuid::Uuid> = sqlx::query_scalar(
        "SELECT blob_id
           FROM proxima_core.memory
          WHERE handle = $1 AND owner_id = $2 AND blob_id IS NOT NULL
         UNION
         SELECT blob_id
           FROM proxima_core.cooled
          WHERE handle = $1 AND owner_id = $2 AND blob_id IS NOT NULL",
    )
    .bind(handle)
    .bind(from_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)?;
    for blob_id in blob_ids {
        transfer_one_cited_blob(tx, handle, to_id, blob_id).await?;
    }
    Ok(())
}

/// One cited blob, one of the three cases.
async fn transfer_one_cited_blob(
    tx: &mut Transaction<'_, Postgres>,
    handle: uuid::Uuid,
    to_id: uuid::Uuid,
    blob_id: uuid::Uuid,
) -> Result<(), StorageError> {
    let Some((schema_id, content_hash)) = sqlx::query_as::<_, (String, Vec<u8>)>(
        "SELECT schema_id, content_hash FROM proxima_core.blob WHERE blob_id = $1",
    )
    .bind(blob_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_err)?
    else {
        return Ok(());
    };

    // Case 1: the destination already owns an identical row.
    let existing: Option<uuid::Uuid> = sqlx::query_scalar(
        "SELECT blob_id
           FROM proxima_core.blob
          WHERE owner_id = $1 AND schema_id = $2 AND content_hash = $3",
    )
    .bind(to_id)
    .bind(&schema_id)
    .bind(&content_hash)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_err)?;
    if let Some(dest_blob_id) = existing {
        if dest_blob_id != blob_id {
            remap_handle_blob_refs(tx, handle, blob_id, dest_blob_id).await?;
        }
        return Ok(());
    }

    // Case 2: nobody else's live series names these bytes, so the rows can
    // simply change hands. `handle <> $2 AND owner_id <> $3` is the
    // predicate this arm inherited, and reading it is worth the moment it
    // takes: `$3` is the DESTINATION, so the source owner's OWN second
    // series satisfies it. That is what the `Conflict` was refusing.
    let shared: bool = sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1
               FROM proxima_core.memory
              WHERE blob_id = $1
                AND handle <> $2
                AND owner_id <> $3
             UNION ALL
             SELECT 1
               FROM proxima_core.cooled
              WHERE blob_id = $1
                AND handle <> $2
                AND owner_id <> $3
         )",
    )
    .bind(blob_id)
    .bind(handle)
    .bind(to_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(map_err)?;
    if !shared {
        return move_blob_rows_in_place(tx, blob_id, to_id).await;
    }

    mount_blob_for_destination(tx, handle, to_id, blob_id, &schema_id, &content_hash).await
}

/// Case 2: the rows change hands, the object does not move, nothing is
/// minted. Byte for byte what a transfer of an uncontested cited blob did
/// before the dedupe arm.
async fn move_blob_rows_in_place(
    tx: &mut Transaction<'_, Postgres>,
    blob_id: uuid::Uuid,
    to_id: uuid::Uuid,
) -> Result<(), StorageError> {
    sqlx::query("UPDATE proxima_core.blob SET owner_id = $2 WHERE blob_id = $1")
        .bind(blob_id)
        .bind(to_id)
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    // The upload row moves with the blob row it describes. The read path
    // requires both to name the same owner, so leaving this behind made a
    // transferred citation unreadable at the destination while still
    // counting against the source. The object itself does not move: its key
    // is derived from `upload_id`, which is unchanged.
    sqlx::query("UPDATE proxima_core.blob_uploads SET owner_id = $2 WHERE blob_id = $1")
        .bind(blob_id)
        .bind(to_id)
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    Ok(())
}

/// Case 3: the destination gets its own row over the source's object.
async fn mount_blob_for_destination(
    tx: &mut Transaction<'_, Postgres>,
    handle: uuid::Uuid,
    to_id: uuid::Uuid,
    blob_id: uuid::Uuid,
    schema_id: &str,
    content_hash: &[u8],
) -> Result<(), StorageError> {
    let dest_blob_id: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO proxima_core.blob (owner_id, schema_id, content_hash)
         VALUES ($1, $2, $3)
         RETURNING blob_id",
    )
    .bind(to_id)
    .bind(schema_id)
    .bind(content_hash)
    .fetch_one(&mut **tx)
    .await
    .map_err(map_err)?;
    // One mounted upload row per completed source upload row, naming the
    // same object. `COALESCE(u.mounted_from_upload_id, u.upload_id)` rather
    // than `u.upload_id`: mounting a mount must still resolve to the row
    // that actually uploaded bytes, because an intermediate row never had
    // an object of its own.
    //
    // `status`, `sha256`, `etag` and the byte length are copied because
    // they describe the OBJECT, which is the thing being shared; the expiry
    // and completion timestamps are copied for the same reason. Only the
    // identity columns differ.
    sqlx::query(
        "INSERT INTO proxima_core.blob_uploads
             (owner_id, bucket, object_key, filename, mime, expected_byte_len,
              status, blob_id, sha256, etag, expires_at, completed_at,
              mounted_from_upload_id)
         SELECT $2, u.bucket, u.object_key, u.filename, u.mime, u.expected_byte_len,
                u.status, $3, u.sha256, u.etag, u.expires_at, u.completed_at,
                COALESCE(u.mounted_from_upload_id, u.upload_id)
           FROM proxima_core.blob_uploads u
          WHERE u.blob_id = $1
            AND u.status = 'completed'",
    )
    .bind(blob_id)
    .bind(to_id)
    .bind(dest_blob_id)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    remap_handle_blob_refs(tx, handle, blob_id, dest_blob_id).await
}

/// Repoint this series' citations from one blob row to another.
///
/// Scoped by `handle` and not by owner: the hot rows have not changed
/// hands yet at this point in the transfer, and the cooled rows change
/// hands later in the same transaction. Scoping by owner here would remap
/// nothing or everything depending on the order.
///
/// This is `TransferRule::FollowOrDedupe { remaps }` executed. The
/// declaration lists the columns this crate can see; the citation sidecars
/// that point at a blob by convention rather than by constraint are
/// checked at freeze instead, because a flavor that declared one would
/// need a remap this function cannot write.
async fn remap_handle_blob_refs(
    tx: &mut Transaction<'_, Postgres>,
    handle: uuid::Uuid,
    from_blob_id: uuid::Uuid,
    to_blob_id: uuid::Uuid,
) -> Result<(), StorageError> {
    sqlx::query(
        "UPDATE proxima_core.memory
            SET blob_id = $3
          WHERE handle = $1 AND blob_id = $2",
    )
    .bind(handle)
    .bind(from_blob_id)
    .bind(to_blob_id)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    sqlx::query(
        "UPDATE proxima_core.cooled
            SET blob_id = $3
          WHERE handle = $1 AND blob_id = $2",
    )
    .bind(handle)
    .bind(from_blob_id)
    .bind(to_blob_id)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(())
}

/// Re-home Content under the destination so `Memory.owner = Content.owner`
/// after the transfer. Shared payloads stay on the origin owner; only this
/// series is remapped.
async fn transfer_content_for_handle(
    tx: &mut Transaction<'_, Postgres>,
    handle: uuid::Uuid,
    from_id: uuid::Uuid,
    to_id: uuid::Uuid,
) -> Result<(), StorageError> {
    let rows: Vec<(uuid::Uuid, String, Vec<u8>)> = sqlx::query_as(
        "SELECT DISTINCT c.content_id, c.schema_id, c.content_hash
           FROM proxima_core.content c
           JOIN (
                 SELECT content_id
                   FROM proxima_core.memory
                  WHERE handle = $1 AND owner_id = $2 AND content_id IS NOT NULL
                 UNION
                 SELECT content_id
                   FROM proxima_core.cooled
                  WHERE handle = $1 AND owner_id = $2 AND content_id IS NOT NULL
                ) series_content ON series_content.content_id = c.content_id",
    )
    .bind(handle)
    .bind(from_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)?;
    for (old_id, schema_id, hash) in rows {
        let hash: [u8; 32] = hash
            .try_into()
            .map_err(|_| StorageError::Internal("content hash is not 32 bytes".into()))?;
        let new_id = crate::verbs::content::ensure_content(tx, to_id, &schema_id, &hash).await?;
        if new_id == old_id {
            sqlx::query("UPDATE proxima_core.content SET owner_id = $2 WHERE content_id = $1")
                .bind(old_id)
                .bind(to_id)
                .execute(&mut **tx)
                .await
                .map_err(map_err)?;
        } else {
            sqlx::query(
                "UPDATE proxima_core.memory
                    SET content_id = $3
                  WHERE handle = $1 AND owner_id = $2 AND content_id = $4",
            )
            .bind(handle)
            .bind(from_id)
            .bind(new_id)
            .bind(old_id)
            .execute(&mut **tx)
            .await
            .map_err(map_err)?;
            sqlx::query(
                "UPDATE proxima_core.cooled
                    SET content_id = $3
                  WHERE handle = $1 AND owner_id = $2 AND content_id = $4",
            )
            .bind(handle)
            .bind(from_id)
            .bind(new_id)
            .bind(old_id)
            .execute(&mut **tx)
            .await
            .map_err(map_err)?;
            crate::verbs::content::gc_unreferenced_content(tx, old_id).await?;
        }
    }
    Ok(())
}

async fn follow_embedding_owners(
    tx: &mut Transaction<'_, Postgres>,
    ts: &[uuid::Uuid],
    to_id: uuid::Uuid,
) -> Result<(), StorageError> {
    sqlx::query(
        "UPDATE proxima_core.embeddings SET owner_id = $2 WHERE entity_id = ANY($1::uuid[])",
    )
    .bind(ts)
    .bind(to_id)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    sqlx::query(
        "UPDATE proxima_core.embedding_heads SET owner_id = $2 WHERE entity_id = ANY($1::uuid[])",
    )
    .bind(ts)
    .bind(to_id)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    sqlx::query(
        "UPDATE proxima_core.embedding_jobs SET owner_id = $2 WHERE entity_id = ANY($1::uuid[])",
    )
    .bind(ts)
    .bind(to_id)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(())
}

/// # Errors
///
/// Returns `Internal` on sqlx failure.
pub(crate) async fn home_owner(
    pool: &PgPool,
    entity: EntityId,
) -> Result<Option<OwnerRef>, StorageError> {
    let row: Option<(OwnerRefKind, uuid::Uuid)> = sqlx::query_as(
        "SELECT o.kind::text::proxima_core.owner_kind, m.owner_id
           FROM proxima_core.memory m
           JOIN proxima_core.owners o ON o.owner_id = m.owner_id
          WHERE m.t = $1
         UNION ALL
         SELECT o.kind::text::proxima_core.owner_kind, g.owner_id
           FROM proxima_core.goal g
           JOIN proxima_core.owners o ON o.owner_id = g.owner_id
          WHERE g.t = $1
         LIMIT 1",
    )
    .bind(entity.uuid())
    .fetch_optional(pool)
    .await
    .map_err(map_err)?;

    Ok(row.map(|(kind, id)| kind.with_uuid(id)))
}
