//! Direct `OwnerRef` column helpers.

use proxima_core::flavor::TransferLeg;
use proxima_core::owner_inverse::OwnerSurfaces;
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
    // The entity discriminant is authorization data. A union over both
    // spines lets a caller relabel a Memory as a Goal (or vice versa), then
    // pass a kind-specific layering check with the other row's `t`.
    let row: Option<(OwnerRefKind, uuid::Uuid)> = match entity {
        EntityId::Memory(memory_id) => {
            sqlx::query_as(
                "SELECT o.kind::text::proxima_core.owner_kind, m.owner_id
               FROM proxima_core.memory m
               JOIN proxima_core.owners o ON o.owner_id = m.owner_id
              WHERE m.t = $1 AND m.owner_id = ANY($2::uuid[])",
            )
            .bind(memory_id.into_inner())
            .bind(&owner_ids)
            .fetch_optional(pool)
            .await
        }
        EntityId::Goal(goal_id) => {
            sqlx::query_as(
                "SELECT o.kind::text::proxima_core.owner_kind, g.owner_id
               FROM proxima_core.goal g
               JOIN proxima_core.owners o ON o.owner_id = g.owner_id
              WHERE g.t = $1 AND g.owner_id = ANY($2::uuid[])",
            )
            .bind(goal_id.into_inner())
            .bind(&owner_ids)
            .fetch_optional(pool)
            .await
        }
    }
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
/// does not touch them at all. `mcp_call_logged_v1` is the one in the core
/// contract: it describes the ACTOR of a tool call (`actor_upn`), not the
/// memory, and it
/// carries its own `owner_id`, stamped at write time with the owner that
/// made the call. The row stays under that owner, so the source keeps
/// answering "what did my agents do" after giving the memory away.
///
/// The destination never sees it, because every read of an owner-pinned
/// sidecar is scoped by the sidecar's OWN owner rather than the memory's:
/// hydrate joins `memory` on `m.owner_id = s.owner_id` so a moved memory
/// stops matching, `read_mcp_call_history` selects on `fact.owner_id` with
/// no `memory` join at all, and owner erase/export select the same
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
    surfaces: &OwnerSurfaces,
    entity: EntityId,
    from_owner: OwnerRef,
    to_owner: OwnerRef,
) -> Result<bool, StorageError> {
    // The backstop, DRIVEN by the declaration rather than merely agreeing
    // with it: the entity picks the surface that speaks for its series, and
    // that surface's resolved leg decides. Re-declare `proxima_core.goal` as
    // a rule that moves rows and goals start transferring, which is what
    // "enforced by the declaration" has to mean if it is to mean anything.
    let memory_id = match entity {
        EntityId::Memory(memory_id) => memory_id,
        EntityId::Goal(_) => {
            return Err(refuse_untransferable_entity(surfaces, GOAL_SPINE));
        }
    };
    if let TransferLeg::Refused { .. } = surfaces.transfer_leg(MEMORY_SPINE) {
        return Err(refuse_untransferable_entity(surfaces, MEMORY_SPINE));
    }
    if from_owner == to_owner {
        return Err(StorageError::ConstraintViolation(
            "transfer destination is the current owner".into(),
        ));
    }
    let from_id = from_owner.stored_owner_id();
    with_bounded_retry(move || async move {
        let mut tx = pool.begin().await.map_err(internal)?;
        let transferred =
            transfer_memory_t(&mut tx, surfaces, memory_id.into_inner(), from_id, to_owner).await?;
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

/// The kernel spine tables the two [`EntityId`] arms name. A transfer asks
/// the contract what it may do to an entity by asking about the surface
/// that IS the entity's series.
const MEMORY_SPINE: &str = "proxima_core.memory";
const GOAL_SPINE: &str = "proxima_core.goal";

/// The refusal a `NotTransferable` surface earns, with the reason its own
/// declaration gave.
///
/// A surface the registry does not carry resolves to
/// [`TransferLeg::Unreachable`], which freeze refuses at boot — so reaching
/// the fallback here means the registry was assembled by hand, and the
/// honest answer is still a refusal.
fn refuse_untransferable_entity(surfaces: &OwnerSurfaces, table: &str) -> StorageError {
    let entity = match table {
        GOAL_SPINE => "goals",
        _ => "these entities",
    };
    match surfaces.transfer_leg(table) {
        // The declared `why` is written headline-first — the sentence a
        // caller needs, a colon, then the rationale an operator needs. The
        // wire refusal carries the headline, so the message is GENERATED
        // from `NotTransferable { why }` rather than restated beside it:
        // rewording the declaration moves this error, which is what the
        // transfer differential golden is for.
        TransferLeg::Refused { why } => {
            let headline = why.split_once(':').map_or(why, |(head, _)| head);
            StorageError::ConstraintViolation(format!("{entity} do not transfer: {headline}"))
        }
        _ => StorageError::Internal(format!(
            "{table} declares no transfer leg this substrate can perform"
        )),
    }
}

async fn transfer_memory_t(
    tx: &mut Transaction<'_, Postgres>,
    surfaces: &OwnerSurfaces,
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
    transfer_memory_handle(tx, surfaces, handle, from_id, to_owner).await
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
    surfaces: &OwnerSurfaces,
    handle: uuid::Uuid,
    from_id: uuid::Uuid,
    to_owner: OwnerRef,
) -> Result<bool, StorageError> {
    // The destination's `owners` row is not seeded anywhere, so mint it (or
    // confirm its kind) BEFORE any statement below binds `to_id` into
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
    // Order, and all of it load-bearing.
    //
    // The two dedupe surfaces run FIRST because both read the series' hot
    // and cooled rows under the SOURCE owner to find what the series
    // cites, and the generated re-home is what stops those reads from
    // matching.
    //
    // The generated legs run SECOND, and BEFORE the head compare-and-set. A
    // transfer that took the head row first would hold that lock across
    // every remaining statement, and `memory_head` is the row a same-series
    // ingest has to read: the transfer would be serializing ordinary writes
    // behind its own row work. `transfer_retries_when_ingest_advances_the_captured_head`
    // pins exactly that, and pins it as a liveness property ("without
    // adding a lock to normal ingest") rather than as an outcome.
    //
    // Doing real writes before the decision is already this transaction's
    // shape: a false persist rolls the whole thing back, which is why
    // `transfer_to_owner` rolls back rather than committing a `false`.
    transfer_cited_blobs(tx, surfaces, handle, from_id, to_id).await?;
    transfer_content_for_handle(tx, surfaces, handle, from_id, to_id).await?;
    run_generated_transfer_legs(tx, surfaces, to_id, &ts).await?;
    if persist_series_head_transfer(tx, handle, from_id, to_id, expected_head_t).await? {
        announce_series_transfer(tx, handle, from_id, to_id).await?;
        return Ok(true);
    }
    Ok(false)
}

/// The head compare-and-set that DECIDES whether the transfer happened.
///
/// This is why `memory_head` is a declared bespoke transfer leg and not a
/// generated one: `rows_affected` carries the question. Zero means either
/// the head advanced under us — a retryable race — or the series left the
/// owner entirely, which is the clean `false`. No generated
/// `UPDATE ... WHERE key = ANY($1)` can carry that question, and the two
/// races that hang off the answer are the ones `owner_transfer.rs` pins.
async fn persist_series_head_transfer(
    tx: &mut Transaction<'_, Postgres>,
    handle: uuid::Uuid,
    from_id: uuid::Uuid,
    to_id: uuid::Uuid,
    expected_head_t: uuid::Uuid,
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
    Ok(true)
}

/// ONE loop over the declarations, in table order.
///
/// Every statement here is a `TransferLeg` the flavor that declared the
/// surface resolved at registry time:
///
/// - [`TransferLeg::Rehomed`] sets the surface's declared owner column,
///   selecting on the column the key names against the series' `t`
///   set. That covers `cooled`, `memory`, `sketch`, the three embedding
///   tables and EVERY flavor's projection with one statement shape.
/// - [`TransferLeg::Dropped`] deletes them — one member, `ingest_keys`,
///   whose receipt proves admission by the SOURCE and does not travel.
///
/// A flavor adding a `Follow` surface cannot have it silently not follow:
/// the surface is in this loop because it is in the contract, and a surface
/// this loop cannot reach is `FlavorRegistryError::UnmovableSurface` at boot
/// rather than a row the source owner can still read after the memory became
/// someone else's.
///
/// Owner-pinned sidecars are untouched BY THEIR DECLARATION.
/// `mcp_call_logged_v1` carries `actor_upn` and its own `owner_id`, stamped
/// with the owner that made the call; `RetainAtSource` resolves to
/// `TransferLeg::Retained`, which is not in `generated_transfer_legs()`, so
/// no statement reaches it. The source keeps answering "what did my agents
/// do" after giving the memory away.
async fn run_generated_transfer_legs(
    tx: &mut Transaction<'_, Postgres>,
    surfaces: &OwnerSurfaces,
    to_id: uuid::Uuid,
    ts: &[uuid::Uuid],
) -> Result<(), StorageError> {
    for (table, leg) in surfaces.generated_transfer_legs() {
        // SQL-POLICY: generated
        sqlx::query(sqlx::AssertSqlSafe(series_leg_sql(table, leg)?))
            .bind(ts)
            .bind(to_id)
            .execute(&mut **tx)
            .await
            .map_err(map_err)?;
    }
    Ok(())
}

/// The statement one generated transfer leg runs, over the series' `t` set.
///
/// Two shapes, both `$1` = the `t` array and `$2` = the destination owner.
/// `Dropped` binds `$2` and never reads it, which costs a bind and buys one
/// call site instead of two — the alternative was a second loop whose only
/// difference is a verb.
///
/// Every identifier substituted here is a `&'static str` from a `const`
/// contract that `try_freeze` has already validated, and every one is
/// additionally checked against `information_schema` by
/// `flavor_contract_acceptance::every_column_a_declaration_names_is_a_column_the_catalog_has`.
/// `PgIdent` is the belt to that suspenders: it is what makes the
/// substitution `%I`-equivalent rather than merely well-intentioned.
fn series_leg_sql(table: &str, leg: TransferLeg) -> Result<String, StorageError> {
    let table = crate::pg_ident::PgIdent::table(table)?;
    match leg {
        TransferLeg::Rehomed {
            key_column,
            owner_column,
        } => {
            let owner = crate::pg_ident::PgIdent::column(owner_column)?;
            let key = crate::pg_ident::PgIdent::column(key_column)?;
            // SQL-POLICY: PgIdent
            Ok(format!(
                "UPDATE {} SET {} = $2 WHERE {} = ANY($1::uuid[])",
                table.as_str(),
                owner.as_str(),
                key.as_str()
            ))
        }
        TransferLeg::Dropped { key_column, .. } => {
            let key = crate::pg_ident::PgIdent::column(key_column)?;
            // SQL-POLICY: PgIdent
            Ok(format!(
                "DELETE FROM {} WHERE {} = ANY($1::uuid[]) AND $2::uuid IS NOT NULL",
                table.as_str(),
                key.as_str()
            ))
        }
        other => Err(StorageError::Internal(format!(
            "{} resolved to {other:?}, which is not a generated transfer leg",
            table.as_str()
        ))),
    }
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
///    whatever it still uses. Moving the row instead would collide with the
///    destination's UNIQUE `(owner_id, schema_id, content_hash)`.
/// 2. **Nothing else references the bytes.** Move the rows in place: same
///    `blob_id`, same `upload_id`, same object, no mount. The common case
///    does not pay for the uncommon one, and a citation id a client already
///    holds does not move under it.
/// 3. **Another owner's live series references the bytes.** The destination
///    gets a row of its own and an upload row that MOUNTS the source's
///    object — OCI's cross-repo blob mount, where a mount is an optimisation
///    over a copy and never a correctness requirement. Nothing in S3 is
///    read, written or copied; ownership is metadata over an immutable
///    store.
///
/// The source's rows are never deleted here. Deleting a blob row decides
/// the fate of an S3 object, and a transfer has no object-store handle in
/// scope; erase does, and its purge is refcounted precisely so a shared
/// object survives one owner leaving. An unreferenced source row is the
/// source owner's own row, reachable by its own erase and reported by
/// reconcile.
async fn transfer_cited_blobs(
    tx: &mut Transaction<'_, Postgres>,
    surfaces: &OwnerSurfaces,
    handle: uuid::Uuid,
    from_id: uuid::Uuid,
    to_id: uuid::Uuid,
) -> Result<(), StorageError> {
    let leg = dedupe_leg(surfaces, BLOB_SURFACE)?;
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
        transfer_one_cited_blob(tx, leg, handle, to_id, blob_id).await?;
    }
    Ok(())
}

/// The tables whose transfer is `FollowOrDedupe`, named where the
/// orchestration lives rather than inside it.
const BLOB_SURFACE: &str = "proxima_core.blob";
const CONTENT_SURFACE: &str = "proxima_core.content";

/// The registry-resolved dedupe leg for one surface, or a typed refusal.
///
/// `freeze` has already proved this resolves — a `FollowOrDedupe` surface
/// that is not in the flavor's bespoke list is `UnmovableSurface` at boot —
/// so the error arm is reachable only from a hand-assembled registry, and
/// refusing is the honest answer there.
fn dedupe_leg(surfaces: &OwnerSurfaces, table: &str) -> Result<TransferLeg, StorageError> {
    match surfaces.transfer_leg(table) {
        leg @ TransferLeg::Deduped { .. } => Ok(leg),
        other => Err(StorageError::Internal(format!(
            "{table} resolved to {other:?}; this statement serves the dedupe arm"
        ))),
    }
}

/// The `SELECT` that answers "does the destination already hold these
/// bytes", generated from the surface's declared `dedupe_key`.
///
/// The key is not decoration: it is the UNIQUE constraint the move would
/// collide with, which is exactly why the arm exists. Declaring it and then
/// hardcoding the same three columns in SQL is how the two drift, and
/// `every_dedupe_key_is_a_uniqueness_the_schema_enforces` asks
/// `pg_constraint` whether each declared key really is one.
///
/// The bind order is the declared column order, and the caller binds
/// exactly `dedupe_key.len()` values — a key of a different length is a
/// contract change that fails here rather than silently matching the wrong
/// row.
fn dedupe_lookup_sql(
    table: &str,
    key_column: &str,
    dedupe_key: &[&'static str],
) -> Result<String, StorageError> {
    let table = crate::pg_ident::PgIdent::table(table)?;
    let key = crate::pg_ident::PgIdent::column(key_column)?;
    let mut predicates = Vec::with_capacity(dedupe_key.len());
    for (n, column) in dedupe_key.iter().enumerate() {
        predicates.push(format!(
            "{} = ${}",
            crate::pg_ident::PgIdent::column(column)?.as_str(),
            n + 1
        ));
    }
    // SQL-POLICY: PgIdent
    Ok(format!(
        "SELECT {} FROM {} WHERE {}",
        key.as_str(),
        table.as_str(),
        predicates.join(" AND ")
    ))
}

/// One referring column repointed from the old row to the new one,
/// generated from the surface's declared `remaps`.
///
/// Scoped by `handle` and not by owner: the hot rows have not changed hands
/// at this point in the transfer, and the cooled rows change hands in the
/// generated re-home later in the same transaction. Scoping by owner here
/// would remap nothing or everything depending on the order.
fn remap_sql(entry: &str) -> Result<String, StorageError> {
    let (table, column) = entry
        .split_once('.')
        .ok_or_else(|| StorageError::Internal(format!("remap {entry} is not <table>.<column>")))?;
    let qualified = format!("proxima_core.{table}");
    let table = crate::pg_ident::PgIdent::table(&qualified)?;
    let column = crate::pg_ident::PgIdent::column(column)?;
    // SQL-POLICY: PgIdent
    Ok(format!(
        "UPDATE {} SET {} = $3 WHERE handle = $1 AND {} = $2",
        table.as_str(),
        column.as_str(),
        column.as_str()
    ))
}

/// One cited blob, one of the three cases.
async fn transfer_one_cited_blob(
    tx: &mut Transaction<'_, Postgres>,
    leg: TransferLeg,
    handle: uuid::Uuid,
    to_id: uuid::Uuid,
    blob_id: uuid::Uuid,
) -> Result<(), StorageError> {
    let TransferLeg::Deduped { dedupe_key, remaps } = leg else {
        return Err(StorageError::Internal(
            "the cited-blob statement serves the dedupe arm".into(),
        ));
    };
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

    // Case 1: the destination already owns an identical row. The three
    // columns compared are `blob`'s declared `dedupe_key`, read off the
    // contract rather than restated here.
    // SQL-POLICY: generated
    let existing: Option<uuid::Uuid> = sqlx::query_scalar(sqlx::AssertSqlSafe(dedupe_lookup_sql(
        BLOB_SURFACE,
        "blob_id",
        dedupe_key,
    )?))
    .bind(to_id)
    .bind(&schema_id)
    .bind(&content_hash)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_err)?;
    if let Some(dest_blob_id) = existing {
        if dest_blob_id != blob_id {
            remap_handle_refs(tx, remaps, handle, blob_id, dest_blob_id).await?;
        }
        return Ok(());
    }

    // Case 2: nobody else's live series names these bytes, so the rows can
    // simply change hands. Read `handle <> $2 AND owner_id <> $3` carefully:
    // `$3` is the DESTINATION, so the source owner's OWN second series
    // satisfies it and does not count as sharing.
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

    mount_blob_for_destination(
        tx,
        remaps,
        handle,
        to_id,
        blob_id,
        &schema_id,
        &content_hash,
    )
    .await
}

/// Case 2: the rows change hands, the object does not move, nothing is
/// minted.
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
    // The upload row moves with the blob row it describes: the read path
    // requires both to name the same owner, so leaving it behind makes a
    // transferred citation unreadable at the destination while still
    // counting against the source. The object itself does not move — its key
    // derives from `upload_id`, which is unchanged.
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
    remaps: &'static [&'static str],
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
    remap_handle_refs(tx, remaps, handle, blob_id, dest_blob_id).await
}

/// Repoint this series' referring columns from one row to another.
///
/// `remaps` IS the loop. The declaration lists the referring columns
/// this crate can see; columns that point at the row by convention rather
/// than by constraint — every flavor's cited-object and citation-mapping
/// sidecars point at a `blob_id` with no SQL FK — cannot be listed there,
/// because the flavor declaring them is not the flavor declaring this
/// surface, and the freeze check for citation sidecars covers those.
async fn remap_handle_refs(
    tx: &mut Transaction<'_, Postgres>,
    remaps: &'static [&'static str],
    handle: uuid::Uuid,
    from_id: uuid::Uuid,
    to_id: uuid::Uuid,
) -> Result<(), StorageError> {
    for entry in remaps {
        // SQL-POLICY: generated
        sqlx::query(sqlx::AssertSqlSafe(remap_sql(entry)?))
            .bind(handle)
            .bind(from_id)
            .bind(to_id)
            .execute(&mut **tx)
            .await
            .map_err(map_err)?;
    }
    Ok(())
}

/// Re-home Content under the destination so `Memory.owner = Content.owner`
/// after the transfer. Shared payloads stay on the origin owner; only this
/// series is remapped.
async fn transfer_content_for_handle(
    tx: &mut Transaction<'_, Postgres>,
    surfaces: &OwnerSurfaces,
    handle: uuid::Uuid,
    from_id: uuid::Uuid,
    to_id: uuid::Uuid,
) -> Result<(), StorageError> {
    let TransferLeg::Deduped { remaps, .. } = dedupe_leg(surfaces, CONTENT_SURFACE)? else {
        unreachable!("dedupe_leg returns only the Deduped arm");
    };
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
            // The same generated remap the blob arm runs, off `content`'s
            // own declared `remaps`. No owner filter is needed: this runs
            // BEFORE the generated re-home, so every row of the handle is
            // still the source's.
            remap_handle_refs(tx, remaps, handle, old_id, new_id).await?;
            crate::verbs::content::gc_unreferenced_content(tx, old_id).await?;
        }
    }
    Ok(())
}

/// # Errors
///
/// Returns `Internal` on sqlx failure.
pub(crate) async fn home_owner(
    pool: &PgPool,
    entity: EntityId,
) -> Result<Option<OwnerRef>, StorageError> {
    // The entity discriminant is authorization data, exactly as in
    // `visible_home_owner`. A union over both spines on the bare uuid lets a
    // caller relabel a Memory as a Goal (or vice versa) and resolve the owner
    // space off the wrong spine, so each arm asks its own spine only.
    let row: Option<(OwnerRefKind, uuid::Uuid)> = match entity {
        EntityId::Memory(memory_id) => {
            sqlx::query_as(
                "SELECT o.kind::text::proxima_core.owner_kind, m.owner_id
               FROM proxima_core.memory m
               JOIN proxima_core.owners o ON o.owner_id = m.owner_id
              WHERE m.t = $1",
            )
            .bind(memory_id.into_inner())
            .fetch_optional(pool)
            .await
        }
        EntityId::Goal(goal_id) => {
            sqlx::query_as(
                "SELECT o.kind::text::proxima_core.owner_kind, g.owner_id
               FROM proxima_core.goal g
               JOIN proxima_core.owners o ON o.owner_id = g.owner_id
              WHERE g.t = $1",
            )
            .bind(goal_id.into_inner())
            .fetch_optional(pool)
            .await
        }
    }
    .map_err(map_err)?;

    Ok(row.map(|(kind, id)| kind.with_uuid(id)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proxima_core::flavor::TransferRule;

    fn shipped() -> OwnerSurfaces {
        let registry = proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests();
        OwnerSurfaces::for_registry(&registry)
    }

    /// The substrate half of the transfer partition's completeness, and the
    /// exact counterpart of `owner_erase`'s
    /// `every_declared_surface_has_a_leg_this_crate_can_run`.
    ///
    /// Freeze decides in core that every surface reaches SOME leg. What core
    /// cannot know is whether THIS crate can execute the answer: a `Rehomed`
    /// leg whose SQL this file declines to build would be skipped in exactly
    /// the way freeze prevents flavor-side. So every generated leg is
    /// asked for its statement here, and every non-generated one must be a
    /// table a named site in this file claims.
    #[test]
    fn every_declared_surface_has_a_transfer_leg_this_crate_can_run() {
        let surfaces = shipped();
        assert!(
            surfaces.surfaces().len() > 20,
            "the registry should carry the whole core contract, got {}",
            surfaces.surfaces().len()
        );
        for surface in surfaces.surfaces() {
            match surfaces.transfer_leg(surface.table) {
                TransferLeg::Unreachable => panic!(
                    "{} resolves to Unreachable, which freeze should have refused",
                    surface.table
                ),
                leg @ (TransferLeg::Rehomed { .. } | TransferLeg::Dropped { .. }) => {
                    series_leg_sql(surface.table, leg).unwrap_or_else(|e| {
                        panic!(
                            "{} is a generated leg this crate cannot build: {e}",
                            surface.table
                        )
                    });
                }
                TransferLeg::Deduped { .. } => assert!(
                    [BLOB_SURFACE, CONTENT_SURFACE].contains(&surface.table),
                    "{} is a dedupe leg no site in this file orchestrates",
                    surface.table
                ),
                TransferLeg::Bespoke => assert!(
                    ["proxima_core.memory_head", "proxima_core.blob_uploads"]
                        .contains(&surface.table),
                    "{} claims a bespoke transfer leg this file does not write",
                    surface.table
                ),
                TransferLeg::StaysOnKey
                | TransferLeg::Retained { .. }
                | TransferLeg::Refused { .. } => {}
            }
        }
    }

    /// The generated set only ever contains the two legs that are statements.
    /// A `StaysOnKey` surface appearing here would run an `UPDATE` over rows
    /// whose owner is already correct by construction.
    #[test]
    fn the_generated_set_is_exactly_the_legs_that_are_statements() {
        let surfaces = shipped();
        let generated = surfaces.generated_transfer_legs();
        assert!(!generated.is_empty(), "the core spine has rows that move");
        for (table, leg) in &generated {
            assert!(
                matches!(
                    leg,
                    TransferLeg::Rehomed { .. } | TransferLeg::Dropped { .. }
                ),
                "{table} is in the generated set as {leg:?}"
            );
        }
        let mut tables: Vec<&str> = generated.iter().map(|(table, _)| *table).collect();
        let sorted = {
            let mut copy = tables.clone();
            copy.sort_unstable();
            copy
        };
        assert_eq!(tables, sorted, "the generated legs run in table order");
        tables.dedup();
        assert_eq!(tables.len(), generated.len(), "one leg per table");
    }

    /// A rehome sets the surface's ONE declared owner column, named as the
    /// declaration names it rather than as `owner_id` by convention.
    #[test]
    fn a_rehomed_leg_sets_the_owner_column_the_surface_declares() {
        let sql = series_leg_sql(
            "test_flavor.thing",
            TransferLeg::Rehomed {
                key_column: "t",
                owner_column: "custodian_id",
            },
        )
        .expect("a rehome is generated");
        assert_eq!(
            sql,
            "UPDATE test_flavor.thing SET custodian_id = $2 WHERE t = ANY($1::uuid[])"
        );
    }

    /// Both generated shapes bind the same two parameters in the same order,
    /// which is what lets `run_generated_transfer_legs` be one loop.
    #[test]
    fn a_dropped_leg_binds_the_same_two_parameters_as_a_rehome() {
        let sql = series_leg_sql(
            "test_flavor.thing",
            TransferLeg::Dropped {
                key_column: "t",
                why: "unused here",
            },
        )
        .expect("a drop is generated");
        assert_eq!(
            sql,
            "DELETE FROM test_flavor.thing WHERE t = ANY($1::uuid[]) \
             AND $2::uuid IS NOT NULL"
        );
    }

    /// The legs that are not statements are refused rather than approximated.
    #[test]
    fn a_leg_that_is_not_a_statement_is_refused_not_guessed() {
        for leg in [
            TransferLeg::StaysOnKey,
            TransferLeg::Bespoke,
            TransferLeg::Unreachable,
            TransferLeg::Retained { why: "x" },
            TransferLeg::Refused { why: "x" },
        ] {
            assert!(
                series_leg_sql("test_flavor.thing", leg).is_err(),
                "{leg:?} produced a statement"
            );
        }
    }

    /// The lookup's placeholders follow the DECLARED column order, because
    /// that is the order the caller binds in.
    #[test]
    fn the_dedupe_lookup_numbers_placeholders_in_declared_order() {
        let sql = dedupe_lookup_sql(
            "proxima_core.blob",
            "blob_id",
            &["owner_id", "schema_id", "content_hash"],
        )
        .expect("the declared key generates");
        assert_eq!(
            sql,
            "SELECT blob_id FROM proxima_core.blob WHERE owner_id = $1 \
             AND schema_id = $2 AND content_hash = $3"
        );
    }

    /// A remap names the REFERRING table, which is not the surface being
    /// deduped: `blob`'s remaps are columns on `memory` and `cooled`.
    #[test]
    fn a_remap_repoints_the_referring_table_not_the_surface() {
        assert_eq!(
            remap_sql("cooled.blob_id").expect("a remap generates"),
            "UPDATE proxima_core.cooled SET blob_id = $3 \
             WHERE handle = $1 AND blob_id = $2"
        );
        assert!(
            remap_sql("blob_id").is_err(),
            "a remap without a table half is not a remap"
        );
    }

    /// Every `remaps` entry flavor #0 declares generates, and every one names
    /// a table other than its own surface.
    #[test]
    fn every_declared_remap_generates_a_statement() {
        let mut seen = 0;
        for surface in shipped().surfaces() {
            let TransferRule::FollowOrDedupe { remaps, .. } = surface.transfer else {
                continue;
            };
            for entry in remaps {
                remap_sql(entry)
                    .unwrap_or_else(|e| panic!("{} declares remap {entry}: {e}", surface.table));
                let (table, _) = entry.split_once('.').expect("checked by remap_sql");
                assert_ne!(
                    format!("proxima_core.{table}"),
                    surface.table,
                    "a remap repoints a REFERRING column, not the surface's own"
                );
                seen += 1;
            }
        }
        assert_eq!(
            seen, 4,
            "blob and content each declare two referring columns"
        );
    }

    /// Goals refuse by DECLARATION, and the refusal quotes the declared
    /// reason rather than a message written next to the `if`.
    #[test]
    fn the_goal_refusal_carries_the_declared_reason() {
        let surfaces = shipped();
        let TransferLeg::Refused { why } = surfaces.transfer_leg(GOAL_SPINE) else {
            panic!("the goal spine declares NotTransferable");
        };
        let StorageError::ConstraintViolation(message) =
            refuse_untransferable_entity(&surfaces, GOAL_SPINE)
        else {
            panic!("an untransferable entity is a constraint the substrate holds");
        };
        let headline = why.split_once(':').map_or(why, |(head, _)| head);
        assert!(
            message.ends_with(headline),
            "the refusal ({message}) should carry the declaration's headline ({headline})"
        );
        assert_eq!(
            message, "goals do not transfer: a goal series cannot change owner",
            "and the wire text is pinned, because the differential golden reads it"
        );

        // A table nothing declares untransferable falls to Internal rather
        // than borrowing the goal wording.
        assert!(matches!(
            refuse_untransferable_entity(&surfaces, MEMORY_SPINE),
            StorageError::Internal(_)
        ));
    }
}
