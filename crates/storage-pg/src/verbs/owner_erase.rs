// The erase entry points take (pool, cold store, authorization, owner parts,
// source scope) — every argument is a distinct authority the erase has to be
// handed, and bundling them into a struct would only move the arity to its
// constructor.
#![allow(clippy::too_many_arguments)]

use proxima_core::flavor::{EraseLeg, KeyShape};
use proxima_core::owner_inverse::{
    EraseAuthorization, OwnerEraseCounts, OwnerEraseOutcome, OwnerEraseRefusal, OwnerSurfaces,
};
use proxima_core::{ColdObjectStore, GroupId, OwnerRef, SourceId, StorageError, UserId};
use sqlx::{PgPool, Postgres, Transaction};

use crate::access::owner_columns::{
    lock_group_membership_tx, lock_owner_fence_exclusive_tx, lock_owner_fence_shared_tx,
    lock_source_fence_exclusive_tx, owner_binds,
};
use crate::error::map_err;
use crate::pg_ident::PgIdent;
use crate::verbs::forget::ColdPurgePlan;

type Tx<'a> = Transaction<'a, Postgres>;

#[derive(Debug, Clone, Copy)]
enum SelectionScope<'a> {
    Owner,
    Source(&'a SourceId),
}

impl<'a> SelectionScope<'a> {
    /// The source bind every scope-narrowed statement carries. A whole-owner
    /// erase binds `NULL`, which the `$n IS NULL` arm of each predicate admits
    /// unconditionally, so one statement serves both scopes instead of a
    /// matched pair that differs by a single `AND`.
    ///
    /// Safe for the source scope too: a non-NULL bind compares with `=`, which
    /// is false for a NULL `source_id`, so unsourced rows stay out of a source
    /// erase exactly as the paired statements had them.
    fn source_bind(self) -> Option<&'a str> {
        match self {
            Self::Owner => None,
            Self::Source(source_id) => Some(source_id.as_str()),
        }
    }
}

/// Begin a bulk-erase transaction.
///
/// A full-owner erase DELETEs across every owner-scoped table and can
/// legitimately run longer than the pool's request-serving
/// `statement_timeout`; `SET LOCAL` scopes that override to this transaction
/// alone. `SET CONSTRAINTS ALL DEFERRED` moves any DEFERRABLE constraint
/// check to commit; a constraint declared NOT DEFERRABLE is unaffected and
/// still checked per statement, which is why the delete order below is
/// load-bearing rather than incidental.
async fn begin_bulk_erase_tx(pool: &PgPool) -> Result<Tx<'_>, StorageError> {
    let mut tx = pool.begin().await.map_err(map_err)?;
    sqlx::query("SET LOCAL statement_timeout = 0")
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;
    sqlx::query("SET CONSTRAINTS ALL DEFERRED")
        .execute(&mut *tx)
        .await
        .map_err(map_err)?;
    Ok(tx)
}

pub async fn erase_group_owner(
    pool: &PgPool,
    cold: &dyn ColdObjectStore,
    auth: &EraseAuthorization,
    group_id: GroupId,
    object_purge_planned: bool,
    surfaces: &OwnerSurfaces,
) -> Result<OwnerEraseOutcome, StorageError> {
    let owner = OwnerRef::Group(group_id);
    let mut tx = begin_bulk_erase_tx(pool).await?;
    lock_group_membership_tx(&mut tx, group_id).await?;
    if group_member_count(&mut tx, group_id).await? > 0 {
        return Ok(refused(auth, OwnerEraseRefusal::OwnerNotAbandoned));
    }
    let cold_purge = erase_selected(&mut tx, owner, SelectionScope::Owner, surfaces).await?;
    let counts = final_counts(&mut tx).await?;
    let outcome = OwnerEraseOutcome::Completed {
        operation_id: auth.audit().operation_id(),
        counts,
        cited_object_purge_pending: object_purge_planned,
        cold_object_purge_pending: !cold_purge.is_empty(),
    };
    tx.commit().await.map_err(map_err)?;
    Ok(finalize_cold_purge(pool, cold, &cold_purge, outcome).await)
}

pub async fn erase_personal_owner(
    pool: &PgPool,
    cold: &dyn ColdObjectStore,
    auth: &EraseAuthorization,
    user_id: UserId,
    object_purge_planned: bool,
    surfaces: &OwnerSurfaces,
) -> Result<OwnerEraseOutcome, StorageError> {
    let owner = OwnerRef::Personal(user_id);
    let mut tx = begin_bulk_erase_tx(pool).await?;
    let cold_purge = erase_selected(&mut tx, owner, SelectionScope::Owner, surfaces).await?;
    let counts = final_counts(&mut tx).await?;
    let outcome = OwnerEraseOutcome::Completed {
        operation_id: auth.audit().operation_id(),
        counts,
        cited_object_purge_pending: object_purge_planned,
        cold_object_purge_pending: !cold_purge.is_empty(),
    };
    tx.commit().await.map_err(map_err)?;
    Ok(finalize_cold_purge(pool, cold, &cold_purge, outcome).await)
}

pub async fn erase_group_source_scope(
    pool: &PgPool,
    cold: &dyn ColdObjectStore,
    auth: &EraseAuthorization,
    group_id: GroupId,
    source_id: &SourceId,
    surfaces: &OwnerSurfaces,
) -> Result<OwnerEraseOutcome, StorageError> {
    let owner = OwnerRef::Group(group_id);
    let mut tx = begin_bulk_erase_tx(pool).await?;
    lock_group_membership_tx(&mut tx, group_id).await?;
    if group_member_count(&mut tx, group_id).await? > 0 {
        return Ok(refused(auth, OwnerEraseRefusal::SourceScopeOwnerStillLive));
    }
    let cold_purge =
        erase_selected(&mut tx, owner, SelectionScope::Source(source_id), surfaces).await?;
    let counts = final_counts(&mut tx).await?;
    let outcome = OwnerEraseOutcome::Completed {
        operation_id: auth.audit().operation_id(),
        counts,
        cited_object_purge_pending: false,
        cold_object_purge_pending: !cold_purge.is_empty(),
    };
    tx.commit().await.map_err(map_err)?;
    Ok(finalize_cold_purge(pool, cold, &cold_purge, outcome).await)
}

pub async fn erase_personal_source_scope(
    pool: &PgPool,
    cold: &dyn ColdObjectStore,
    auth: &EraseAuthorization,
    user_id: UserId,
    source_id: &SourceId,
    surfaces: &OwnerSurfaces,
) -> Result<OwnerEraseOutcome, StorageError> {
    let owner = OwnerRef::Personal(user_id);
    let mut tx = begin_bulk_erase_tx(pool).await?;
    let cold_purge =
        erase_selected(&mut tx, owner, SelectionScope::Source(source_id), surfaces).await?;
    let counts = final_counts(&mut tx).await?;
    let outcome = OwnerEraseOutcome::Completed {
        operation_id: auth.audit().operation_id(),
        counts,
        cited_object_purge_pending: false,
        cold_object_purge_pending: !cold_purge.is_empty(),
    };
    tx.commit().await.map_err(map_err)?;
    Ok(finalize_cold_purge(pool, cold, &cold_purge, outcome).await)
}

async fn finalize_cold_purge(
    pool: &PgPool,
    cold: &dyn ColdObjectStore,
    plan: &ColdPurgePlan,
    outcome: OwnerEraseOutcome,
) -> OwnerEraseOutcome {
    let purge = super::forget::purge_cold_objects_after_commit(pool, cold, plan).await;
    let OwnerEraseOutcome::Completed {
        operation_id,
        counts,
        cited_object_purge_pending,
        ..
    } = outcome
    else {
        return outcome;
    };
    OwnerEraseOutcome::Completed {
        operation_id,
        counts,
        cited_object_purge_pending,
        cold_object_purge_pending: purge.pending,
    }
}

async fn group_member_count(tx: &mut Tx<'_>, group_id: GroupId) -> Result<i64, StorageError> {
    sqlx::query_scalar(
        "SELECT count(*)::bigint FROM proxima_core.group_memberships WHERE group_id = $1",
    )
    .bind(group_id.into_inner())
    .fetch_one(&mut **tx)
    .await
    .map_err(map_err)
}

/// Which selection set a generated `ByKey` statement joins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyedSet {
    Memories,
    Goals,
    Blobs,
}

impl KeyedSet {
    const fn table(self) -> &'static str {
        match self {
            Self::Memories => "selected_memories",
            Self::Goals => "selected_goals",
            Self::Blobs => "selected_blobs",
        }
    }

    const fn column(self) -> &'static str {
        match self {
            Self::Memories => "memory_id",
            Self::Goals => "goal_id",
            Self::Blobs => "blob_id",
        }
    }
}

/// Which selection set a keyed leg joins, from the key shape the flavor
/// declared.
///
/// The classification itself is [`EraseLeg`], resolved by the contract at
/// registry time and refused at freeze when it comes back
/// `Unreachable`. All that is left here is the mapping from a homed key to
/// the temp table this transaction filled for it.
const fn keyed_set(key: KeyShape) -> Option<(KeyedSet, &'static str)> {
    match key {
        KeyShape::MemoryT { column } => Some((KeyedSet::Memories, column)),
        KeyShape::GoalT { column } => Some((KeyedSet::Goals, column)),
        KeyShape::BlobId { column } => Some((KeyedSet::Blobs, column)),
        // An entity `t` has two home tables and therefore two selection
        // sets; the erase fills one per home and can join neither
        // unambiguously. All four such surfaces are declared bespoke erase
        // legs, and a flavor that forgets the exemption gets
        // `UndeletableSurface` rather than this returning a guess.
        KeyShape::EntityT { .. } | KeyShape::OwnerId | KeyShape::Custom(_) => None,
    }
}

/// Delete every surface the contract keys on one selection set, tallying
/// each into the counter it declares.
async fn delete_keyed_surfaces(
    tx: &mut Tx<'_>,
    surfaces: &OwnerSurfaces,
    set: KeyedSet,
) -> Result<u64, StorageError> {
    // Keyed sidecars deliberately use the sealed selection alone: the scope
    // fence, sorted handle/t locks, and exact owner/source revalidation above
    // prove that every selected key still belongs to this erase.  Their
    // declarations provide only the key column, so manufacturing a second
    // owner predicate here would either be impossible or silently assume a
    // column the sidecar contract does not promise.
    let mut total = 0;
    for surface in surfaces.surfaces() {
        let EraseLeg::Keyed(key) = surfaces.erase_leg(surface.table) else {
            continue;
        };
        let Some((declared, column)) = keyed_set(key) else {
            continue;
        };
        if declared != set {
            continue;
        }
        let rows =
            delete_fixed_by_selected(tx, surface.table, column, set.table(), set.column()).await?;
        if let Some(counter) = surface.counter.key() {
            record_count(tx, counter, rows).await?;
        }
        total += rows;
    }
    Ok(total)
}

/// Erase surfaces the contract says carry their own owner.
///
/// The owner-pinned half is the transfer doctrine: these rows record an act,
/// not a Memory. An owner transfer moves the Memory and leaves them behind,
/// so reaching them through `selected_memories` would make them unerasable
/// by the owner that wrote them (its Memory is gone) and erasable by the
/// owner that received it (which never owned them) — zombie rows on one
/// side, someone else's audit trail on the other.
///
/// Source-scoped erase still asks the Memory which source a call belongs to,
/// deliberately without an owner predicate on that lookup: the row being
/// erased is already proven to be this owner's, and the Memory is only being
/// consulted for its `source_id`. A retained audit row can name a Memory that
/// has since transferred, so adding the source owner's predicate here would
/// make the row permanently undeletable by the actor who created it.
async fn delete_owned_surfaces(
    tx: &mut Tx<'_>,
    surfaces: &OwnerSurfaces,
    owner: OwnerRef,
    scope: SelectionScope<'_>,
) -> Result<u64, StorageError> {
    let (_owner_kind, owner_id) = owner_binds(&owner);
    let mut total = 0;
    for surface in surfaces.surfaces() {
        let EraseLeg::Owned { source_scoped } = surfaces.erase_leg(surface.table) else {
            continue;
        };
        let ident = PgIdent::table(surface.table)?;
        // `EraseLeg::Owned` is the arm for a surface that carries its OWN
        // owner, so the predicate reads the column it declares. `m.t` and
        // `c.t` below are the kernel tables' own keys and are fixed.
        let Some(declared_owner) = surface.owner_column else {
            return Err(StorageError::Internal(format!(
                "{} resolved to an owned erase leg while declaring no owner column, so it \
                 has no owner predicate of its own; its rows are reached through the owner \
                 of their key",
                surface.table
            )));
        };
        let owner_column = PgIdent::column(declared_owner)?;
        // SQL-POLICY: PgIdent
        let sql = match scope {
            SelectionScope::Owner => {
                format!(
                    "DELETE FROM {tbl} WHERE {owner} = $1",
                    tbl = ident.as_str(),
                    owner = owner_column.as_str(),
                )
            }
            SelectionScope::Source(_) => {
                let Some(column) = source_scoped else {
                    continue;
                };
                let key = PgIdent::column(column)?;
                format!(
                    "DELETE FROM {tbl} a
                      WHERE a.{owner} = $1
                        AND (EXISTS (SELECT 1 FROM proxima_core.memory m
                                      WHERE m.t = a.{key} AND m.source_id = $2)
                          OR EXISTS (SELECT 1 FROM proxima_core.cooled c
                                      WHERE c.t = a.{key} AND c.source_id = $2))",
                    tbl = ident.as_str(),
                    owner = owner_column.as_str(),
                    key = key.as_str(),
                )
            }
        };
        // SQL-POLICY: PgIdent
        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql)).bind(owner_id);
        if let SelectionScope::Source(source_id) = scope {
            query = query.bind(source_id.as_str());
        }
        let rows = query
            .execute(&mut **tx)
            .await
            .map_err(map_err)?
            .rows_affected();
        if let Some(counter) = surface.counter.key() {
            record_count(tx, counter, rows).await?;
        }
        total += rows;
    }
    Ok(total)
}

/// `proxima_core.sketch` is keyed on a memory `t` OR a goal `t` — one
/// column, two home tables, which is also why it carries no foreign key.
/// No generated statement spans two selection sets, so this leg is named.
async fn delete_selected_sketches(tx: &mut Tx<'_>) -> Result<u64, StorageError> {
    Ok(sqlx::query(
        "DELETE FROM proxima_core.sketch s
          WHERE EXISTS (SELECT 1 FROM selected_memories sm WHERE sm.memory_id = s.t)
             OR EXISTS (SELECT 1 FROM selected_goals sg WHERE sg.goal_id = s.t)",
    )
    .execute(&mut **tx)
    .await
    .map_err(map_err)?
    .rows_affected())
}

async fn erase_selected(
    tx: &mut Tx<'_>,
    owner: OwnerRef,
    scope: SelectionScope<'_>,
    surfaces: &OwnerSurfaces,
) -> Result<ColdPurgePlan, StorageError> {
    open_erase_bookkeeping(tx, surfaces, owner, scope).await?;

    let delegated_authority_grants = delete_delegated_authority_grants(tx, owner, scope).await?;
    record_count(tx, "delegated_authority_grants", delegated_authority_grants).await?;

    let change_events = delete_change_events(tx, owner).await?;
    record_count(tx, "change_events", change_events).await?;

    let source_cursors = delete_source_cursors(tx, owner, scope).await?;
    record_count(tx, "source_cursors", source_cursors).await?;

    // The whole sidecar story, read off the declarations. A surface whose
    // inverse is `ByKey` and whose key is a memory or goal `t` is deleted
    // through the matching selection set; owner-pinned surfaces are held out
    // of it, because their rows do not follow a transfer and the selection
    // set is the wrong set for them in both directions.
    delete_keyed_surfaces(tx, surfaces, KeyedSet::Memories).await?;
    delete_keyed_surfaces(tx, surfaces, KeyedSet::Goals).await?;

    let sketches = delete_selected_sketches(tx).await?;
    record_count(tx, "sketches", sketches).await?;

    let embedding_jobs = delete_embeddings(tx, "proxima_core.embedding_jobs").await?;
    record_count(tx, "embedding_jobs", embedding_jobs).await?;
    let embedding_heads = delete_embeddings(tx, "proxima_core.embedding_heads").await?;
    let embeddings = delete_embeddings(tx, "proxima_core.embeddings").await?;
    record_count(tx, "embeddings", embeddings.saturating_add(embedding_heads)).await?;

    delete_owned_surfaces(tx, surfaces, owner, scope).await?;

    let content_ids = selected_content_ids(tx, owner, scope).await?;
    let memories = delete_selected_memories(tx, owner, scope).await?;
    let (cooled, cold_purge) = delete_selected_cooled(tx, owner, scope).await?;
    super::content::gc_unreferenced_content_batch(tx, &content_ids).await?;
    // The three deletes above and the Goal delete below repeat the
    // owner/source predicate as a backstop. Under the scope fence that
    // predicate cannot exclude anything the selection holds, so a short count
    // means the backstop fired and rows were left behind. Say so instead of
    // reporting a smaller erase as a complete one — this receipt is what a
    // compliance answer is built from.
    assert_selection_fully_deleted(tx, Selection::Memories, memories + cooled).await?;
    record_count(tx, "memories", memories.saturating_add(cooled)).await?;
    let goals = delete_selected_goals(tx, owner).await?;
    assert_selection_fully_deleted(tx, Selection::Goals, goals).await?;
    record_count(tx, "goals", goals).await?;
    let wake_configs = delete_wake_configs(tx, owner, scope).await?;
    record_count(tx, "wake_configs", wake_configs).await?;
    let blobs = delete_blobs(tx, owner, scope, surfaces).await?;
    record_count(tx, "blob_uploads", blobs.uploads).await?;
    record_count(tx, "blobs", blobs.blobs).await?;
    sync_selected_heads(tx, owner.stored_owner_id()).await?;
    let mut object_keys = cold_purge.object_keys().to_vec();
    object_keys.extend_from_slice(blobs.cold_purge.object_keys());
    object_keys.sort_unstable();
    object_keys.dedup();
    Ok(ColdPurgePlan::from_keys(object_keys))
}

/// Build the selection sets and open the per-transaction count table the
/// deletions below tally into, seeded with a zero for every counter the
/// frozen contracts declare.
///
/// The seeding is what makes the receipt COMPLETE rather than merely
/// correct: a declared counter whose leg deleted nothing is present at zero,
/// so a host reading the receipt can tell "the erase counted none" from "the
/// erase does not count this". A count nothing declares cannot appear, and a
/// declared counter cannot be missing — a property no fixed struct
/// definition can hold.
///
/// Core keeps no erase journal: the erase is one transaction, a crash rolls
/// it back whole, and what the host owes its users is the host's record to
/// keep.
async fn open_erase_bookkeeping(
    tx: &mut Tx<'_>,
    surfaces: &OwnerSurfaces,
    owner: OwnerRef,
    scope: SelectionScope<'_>,
) -> Result<(), StorageError> {
    // The scope fence comes first, before the selection reads anything. Held
    // this way the snapshot is exact by construction: an admission for this
    // owner needs the fence shared, and a transfer needs both endpoints
    // exclusively, so neither can commit into the scope between the selection
    // and the deletes. Source scope is exact one level down for the same
    // reason — the shared owner fence excludes transfer, the exclusive source
    // fence excludes same-source admission, and a different-source admission
    // was never in scope.
    //
    // Selecting first and revalidating afterwards was the earlier shape, and
    // it could not make progress under load. The window between the selection
    // and the fence is a full scan of the owner, so on a busy owner some
    // writer had almost always crossed it; every attempt paid two scans to
    // discover that and handed the caller back a `Retryable` it could only
    // answer by starting over.
    match scope {
        SelectionScope::Owner => lock_owner_fence_exclusive_tx(tx, &owner).await?,
        SelectionScope::Source(source_id) => {
            // Source erase remains compatible with other source admissions;
            // the owner shared fence only excludes a full-owner erase.
            lock_owner_fence_shared_tx(tx, &owner).await?;
            lock_source_fence_exclusive_tx(tx, &owner, source_id.as_str()).await?;
        }
    }
    create_selected_sets(tx, owner, scope).await?;
    lock_selected_memory_handles(tx).await?;
    lock_selected_lifecycle_targets(tx).await?;
    capture_selected_handles(tx).await?;
    sqlx::query("CREATE TEMP TABLE erase_counts(name text PRIMARY KEY, count bigint NOT NULL) ON COMMIT DROP")
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    for counter in surfaces.counters() {
        record_count(tx, counter, 0).await?;
    }
    Ok(())
}

/// Acquire the complete series-handle footprint from the immutable selection
/// snapshot.  The selected `handle` column is part of the snapshot precisely
/// so this step does not consult a mutable hot/cooled row after the scope
/// fence has been acquired.
async fn lock_selected_memory_handles(tx: &mut Tx<'_>) -> Result<(), StorageError> {
    let handles: Vec<uuid::Uuid> =
        sqlx::query_scalar("SELECT handle FROM selected_memories ORDER BY handle")
            .fetch_all(&mut **tx)
            .await
            .map_err(map_err)?;
    super::forget::lock_memory_handles_tx(tx, &handles).await
}

/// Lock the complete Memory ∪ Goal erase footprint before any generated
/// surface or core row lock. The witness DELETE triggers re-enter these same
/// locks as a database backstop.
async fn lock_selected_lifecycle_targets(tx: &mut Tx<'_>) -> Result<(), StorageError> {
    let ids: Vec<uuid::Uuid> = sqlx::query_scalar(
        "SELECT memory_id FROM selected_memories
         UNION
         SELECT goal_id FROM selected_goals",
    )
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)?;
    super::forget::lock_lifecycle_targets_tx(tx, &ids).await
}

async fn create_selected_sets(
    tx: &mut Tx<'_>,
    owner: OwnerRef,
    scope: SelectionScope<'_>,
) -> Result<(), StorageError> {
    // `owner_id` is NOT NULL on every owned table and `OwnerRef` has no
    // id-less kind, so `owner_id` binds non-NULL here and in every erase
    // statement below: plain `=` is exactly `IS NOT DISTINCT FROM` while
    // staying an index condition (`PostgreSQL` has no index strategy for
    // DistinctExpr).
    //
    // Nothing in this function binds the kind. Every selection here reaches
    // its rows by `owner_id` alone, which is unique across kinds because
    // `proxima_core.owners` refuses a second kind for an id already stored.
    let (_owner_kind, owner_id) = owner_binds(&owner);

    sqlx::query("CREATE TEMP TABLE selected_memories(memory_id uuid PRIMARY KEY, handle uuid NOT NULL, kind text NOT NULL) ON COMMIT DROP")
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    match scope {
        SelectionScope::Owner => {
            sqlx::query(
                "INSERT INTO selected_memories(memory_id, handle, kind)
                 SELECT t, handle, kind::text
                   FROM proxima_core.memory
                  WHERE owner_id = $1
                 UNION ALL
                 SELECT t, handle, kind::text
                   FROM proxima_core.cooled
                  WHERE owner_id = $1",
            )
            .bind(owner_id)
            .execute(&mut **tx)
            .await
            .map_err(map_err)?;
        }
        SelectionScope::Source(source_id) => {
            sqlx::query(
                "INSERT INTO selected_memories(memory_id, handle, kind)
                 SELECT m.t, m.handle, m.kind::text
                   FROM proxima_core.memory m
                  WHERE m.owner_id = $1
                    AND m.source_id = $2
                 UNION ALL
                 SELECT c.t, c.handle, c.kind::text
                   FROM proxima_core.cooled c
                  WHERE c.owner_id = $1
                    AND c.source_id = $2",
            )
            .bind(owner_id)
            .bind(source_id.as_str())
            .execute(&mut **tx)
            .await
            .map_err(map_err)?;
        }
    }

    sqlx::query("CREATE TEMP TABLE selected_blobs(blob_id uuid PRIMARY KEY) ON COMMIT DROP")
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    match scope {
        SelectionScope::Owner => {
            sqlx::query(
                "INSERT INTO selected_blobs(blob_id)
                 SELECT blob_id FROM proxima_core.blob WHERE owner_id = $1",
            )
            .bind(owner_id)
            .execute(&mut **tx)
            .await
            .map_err(map_err)?;
        }
        SelectionScope::Source(_) => {
            sqlx::query(
                "INSERT INTO selected_blobs(blob_id)
                 SELECT blob_id FROM (
                     SELECT m.blob_id
                       FROM proxima_core.memory m
                       JOIN selected_memories sm ON sm.memory_id = m.t
                      WHERE m.blob_id IS NOT NULL
                     UNION
                     SELECT c.blob_id
                       FROM proxima_core.cooled c
                       JOIN selected_memories sm ON sm.memory_id = c.t
                      WHERE c.blob_id IS NOT NULL
                 ) selected",
            )
            .execute(&mut **tx)
            .await
            .map_err(map_err)?;
        }
    }

    sqlx::query("CREATE TEMP TABLE selected_goals(goal_id uuid PRIMARY KEY, handle uuid NOT NULL, kind text NOT NULL) ON COMMIT DROP")
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    if matches!(scope, SelectionScope::Owner) {
        sqlx::query(
            "INSERT INTO selected_goals(goal_id, handle, kind)
             SELECT t, handle, 'goal'::text FROM proxima_core.goal
              WHERE owner_id = $1",
        )
        .bind(owner_id)
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    }

    Ok(())
}

async fn capture_selected_handles(tx: &mut Tx<'_>) -> Result<(), StorageError> {
    sqlx::query(
        "CREATE TEMP TABLE selected_memory_handles(handle uuid PRIMARY KEY) ON COMMIT DROP",
    )
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    sqlx::query(
        "INSERT INTO selected_memory_handles(handle)
         SELECT DISTINCT handle FROM selected_memories",
    )
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    sqlx::query("CREATE TEMP TABLE selected_goal_handles(handle uuid PRIMARY KEY) ON COMMIT DROP")
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    sqlx::query(
        "INSERT INTO selected_goal_handles(handle)
         SELECT DISTINCT handle FROM selected_goals",
    )
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(())
}

async fn sync_selected_heads(tx: &mut Tx<'_>, owner_id: uuid::Uuid) -> Result<(), StorageError> {
    sqlx::query(
        "UPDATE proxima_core.memory_head h
            SET t = r.t
           FROM (
                SELECT handle, t
                  FROM (
                    SELECT handle, t,
                           row_number() OVER (PARTITION BY handle ORDER BY t DESC) AS n
                      FROM proxima_core.memory
                     WHERE owner_id = $1
                       AND handle IN (SELECT handle FROM selected_memory_handles)
                  ) ranked
                 WHERE n = 1
           ) r
          WHERE h.handle = r.handle
            AND h.owner_id = $1",
    )
    .bind(owner_id)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    sqlx::query(
        "DELETE FROM proxima_core.memory_head h
          WHERE h.owner_id = $1
            AND h.handle IN (SELECT handle FROM selected_memory_handles)
            AND NOT EXISTS (
                SELECT 1 FROM proxima_core.memory m
                 WHERE m.handle = h.handle AND m.owner_id = $1
            )",
    )
    .bind(owner_id)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    sqlx::query(
        "UPDATE proxima_core.goal_head h
            SET t = r.t
           FROM (
                SELECT handle, t
                  FROM (
                    SELECT handle, t,
                           row_number() OVER (PARTITION BY handle ORDER BY t DESC) AS n
                      FROM proxima_core.goal
                     WHERE owner_id = $1
                       AND handle IN (SELECT handle FROM selected_goal_handles)
                  ) ranked
                 WHERE n = 1
           ) r
          WHERE h.handle = r.handle
            AND h.owner_id = $1",
    )
    .bind(owner_id)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    sqlx::query(
        "DELETE FROM proxima_core.goal_head h
          WHERE h.owner_id = $1
            AND h.handle IN (SELECT handle FROM selected_goal_handles)
            AND NOT EXISTS (
                SELECT 1 FROM proxima_core.goal g
                 WHERE g.handle = h.handle AND g.owner_id = $1
            )",
    )
    .bind(owner_id)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(())
}

fn refused(auth: &EraseAuthorization, reason: OwnerEraseRefusal) -> OwnerEraseOutcome {
    OwnerEraseOutcome::Refused {
        operation_id: auth.audit().operation_id(),
        reason,
    }
}

async fn record_count(tx: &mut Tx<'_>, name: &str, count: u64) -> Result<(), StorageError> {
    sqlx::query(
        "INSERT INTO erase_counts(name, count) VALUES ($1, $2)
         ON CONFLICT (name) DO UPDATE SET count = erase_counts.count + EXCLUDED.count",
    )
    .bind(name)
    .bind(i64::try_from(count).unwrap_or(i64::MAX))
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(())
}

/// The receipt: every counter the transaction tallied, in one read.
///
/// Reading the whole table makes the receipt exactly what the erase counted,
/// including counters a flavor declares that no fixed struct could name.
async fn final_counts(tx: &mut Tx<'_>) -> Result<OwnerEraseCounts, StorageError> {
    let rows: Vec<(String, i64)> =
        sqlx::query_as("SELECT name, count FROM erase_counts ORDER BY name")
            .fetch_all(&mut **tx)
            .await
            .map_err(map_err)?;
    Ok(OwnerEraseCounts::new(
        rows.into_iter()
            .map(|(name, count)| (name, u64::try_from(count).unwrap_or_default()))
            .collect(),
    ))
}

async fn delete_fixed_by_selected(
    tx: &mut Tx<'_>,
    table: &str,
    table_column: &str,
    selected_table: &str,
    selected_column: &str,
) -> Result<u64, StorageError> {
    let table = PgIdent::table(table)?;
    let table_column = PgIdent::column(table_column)?;
    let selected_table = PgIdent::table(selected_table)?;
    let selected_column = PgIdent::column(selected_column)?;
    // SQL-POLICY: PgIdent
    let sql = format!(
        "DELETE FROM {table} t USING {selected_table} s WHERE t.{table_column} = s.{selected_column}",
        table = table.as_str(),
        selected_table = selected_table.as_str(),
        table_column = table_column.as_str(),
        selected_column = selected_column.as_str()
    );
    // SQL-POLICY: PgIdent
    let result = sqlx::query(sqlx::AssertSqlSafe(sql))
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    Ok(result.rows_affected())
}

/// The sealed selections a delete is checked against.
#[derive(Debug, Clone, Copy)]
enum Selection {
    Memories,
    Goals,
}

impl Selection {
    /// A closed pair of literals, not a formatted table name: the selection
    /// tables are fixed by this module, so the count statement never carries a
    /// caller-supplied identifier.
    const fn count_sql(self) -> &'static str {
        match self {
            Self::Memories => "SELECT count(*)::bigint FROM selected_memories",
            Self::Goals => "SELECT count(*)::bigint FROM selected_goals",
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Memories => "selected_memories",
            Self::Goals => "selected_goals",
        }
    }
}

/// Every row the selection sealed must have been deleted.
///
/// `deleted` is summed across the hot and cooled legs for Memory, because the
/// selection holds both and each leg deletes its own table.
async fn assert_selection_fully_deleted(
    tx: &mut Tx<'_>,
    selection: Selection,
    deleted: u64,
) -> Result<(), StorageError> {
    // SQL-POLICY: fixed-fragment
    let selected: i64 = sqlx::query_scalar(selection.count_sql())
        .fetch_one(&mut **tx)
        .await
        .map_err(map_err)?;
    let selected = u64::try_from(selected).unwrap_or(0);
    if deleted == selected {
        return Ok(());
    }
    Err(StorageError::Internal(format!(
        "bulk erase deleted {deleted} of {selected} rows in {}; \
         the scope fence should have made these equal",
        selection.label()
    )))
}

/// Delete the core hot spine only while it still belongs to the erased
/// owner/scope.  The sealed selection is the primary race barrier; repeating
/// the owner/source predicate here is a defensive backstop against a stale
/// `t` ever being routed to this primitive by a future caller.
async fn delete_selected_memories(
    tx: &mut Tx<'_>,
    owner: OwnerRef,
    scope: SelectionScope<'_>,
) -> Result<u64, StorageError> {
    let result = sqlx::query(
        "DELETE FROM proxima_core.memory m
          USING selected_memories s
          WHERE m.t = s.memory_id
            AND m.owner_id = $1
            AND ($2::text IS NULL OR m.source_id = $2)",
    )
    .bind(owner.stored_owner_id())
    .bind(scope.source_bind())
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(result.rows_affected())
}

async fn delete_selected_goals(tx: &mut Tx<'_>, owner: OwnerRef) -> Result<u64, StorageError> {
    let result = sqlx::query(
        "DELETE FROM proxima_core.goal g
          USING selected_goals s
          WHERE g.t = s.goal_id
            AND g.owner_id = $1",
    )
    .bind(owner.stored_owner_id())
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(result.rows_affected())
}

/// Delete cooled stubs for selected admissions and mark their cold objects
/// pending destruction. The objects themselves are destroyed after this
/// transaction commits (see [`super::forget::purge_cold_objects_after_commit`]):
/// deleting them here would destroy the payload of an admission that a
/// rollback puts back.
async fn delete_selected_cooled(
    tx: &mut Tx<'_>,
    owner: OwnerRef,
    scope: SelectionScope<'_>,
) -> Result<(u64, ColdPurgePlan), StorageError> {
    let owner_id = owner.stored_owner_id();
    let keys: Vec<String> = sqlx::query_scalar(
        "INSERT INTO proxima_core.cold_purge_pending (object_key, owner_id)
         SELECT c.object_key, c.owner_id
           FROM proxima_core.cooled c
           JOIN selected_memories sm ON sm.memory_id = c.t
          WHERE c.owner_id = $1
            AND ($2::text IS NULL OR c.source_id = $2)
         ON CONFLICT (object_key) DO UPDATE SET enqueued_at = now()
         RETURNING object_key",
    )
    .bind(owner_id)
    .bind(scope.source_bind())
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)?;
    let deleted = sqlx::query(
        "DELETE FROM proxima_core.cooled c
          WHERE c.owner_id = $1
            AND ($2::text IS NULL OR c.source_id = $2)
            AND EXISTS (
                SELECT 1 FROM selected_memories sm WHERE sm.memory_id = c.t
            )",
    )
    .bind(owner_id)
    .bind(scope.source_bind())
    .execute(&mut **tx)
    .await
    .map_err(map_err)?
    .rows_affected();
    Ok((deleted, ColdPurgePlan::from_keys(keys)))
}

async fn selected_content_ids(
    tx: &mut Tx<'_>,
    owner: OwnerRef,
    scope: SelectionScope<'_>,
) -> Result<Vec<uuid::Uuid>, StorageError> {
    sqlx::query_scalar(
        "SELECT DISTINCT content_id FROM (
             SELECT m.content_id
               FROM proxima_core.memory m
               JOIN selected_memories sm ON sm.memory_id = m.t
              WHERE m.owner_id = $1
                AND ($2::text IS NULL OR m.source_id = $2)
                AND m.content_id IS NOT NULL
             UNION
             SELECT c.content_id
               FROM proxima_core.cooled c
               JOIN selected_memories sm ON sm.memory_id = c.t
              WHERE c.owner_id = $1
                AND ($2::text IS NULL OR c.source_id = $2)
                AND c.content_id IS NOT NULL
         ) x",
    )
    .bind(owner.stored_owner_id())
    .bind(scope.source_bind())
    .fetch_all(&mut **tx)
    .await
    .map_err(map_err)
}

async fn delete_embeddings(tx: &mut Tx<'_>, table: &str) -> Result<u64, StorageError> {
    let table = PgIdent::table(table)?;
    // SQL-POLICY: PgIdent
    let sql = format!(
        "DELETE FROM {table} e
          WHERE EXISTS (SELECT 1 FROM selected_memories sm WHERE sm.memory_id = e.entity_id)
             OR EXISTS (SELECT 1 FROM selected_goals sg WHERE sg.goal_id = e.entity_id)",
        table = table.as_str()
    );
    // SQL-POLICY: PgIdent
    let result = sqlx::query(sqlx::AssertSqlSafe(sql))
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    Ok(result.rows_affected())
}

async fn delete_change_events(tx: &mut Tx<'_>, owner: OwnerRef) -> Result<u64, StorageError> {
    let (_owner_kind, owner_id) = owner_binds(&owner);
    let result = sqlx::query(
        "DELETE FROM proxima_core.announce a
          WHERE a.owner_id = $1
            AND (
                EXISTS (SELECT 1 FROM selected_memories sm WHERE sm.memory_id = a.t)
                OR EXISTS (SELECT 1 FROM selected_goals sg WHERE sg.goal_id = a.t)
            )",
    )
    .bind(owner_id)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(result.rows_affected())
}

/// Delegations are owner-level authority, never source-attributable data.
/// Owner erasure removes grants for that exact owner. Dropping a personal
/// owner additionally removes every grant issued under that subject's bearer,
/// including grants targeting a group owner; otherwise the deleted identity
/// would remain durable in another owner's authority table.
async fn delete_delegated_authority_grants(
    tx: &mut Tx<'_>,
    owner: OwnerRef,
    scope: SelectionScope<'_>,
) -> Result<u64, StorageError> {
    if matches!(scope, SelectionScope::Source(_)) {
        return Ok(0);
    }
    let (owner_kind, owner_id) = owner_binds(&owner);
    let erased_subject = match owner {
        OwnerRef::Personal(user_id) => Some(user_id.into_inner()),
        OwnerRef::Group(_) => None,
    };
    let result = sqlx::query(
        "DELETE FROM proxima_core.delegated_authority_grants dag
          WHERE (dag.owner_kind = $1
                 AND dag.owner_id = $2)
             OR ($3::uuid IS NOT NULL AND dag.subject_user_id = $3)",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(erased_subject)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    Ok(result.rows_affected())
}

/// Destroy the owner's wake configuration. `wake_config` is owner-authored
/// content — a free-text `prompt`, the hard-context `hard_memory_t` set, the
/// armed `tool_ids` — and nothing else collects it: `goal.wake_id` is
/// `ON DELETE RESTRICT` and erase never deletes the `owners` row, so a
/// forgotten statement here leaves the prompt text behind forever.
///
/// Owner erase has already deleted every goal of the owner, so every wake row
/// of that owner goes; a wake row an outside owner's goal still arms fails the
/// RESTRICT FK and aborts the erase rather than silently surviving it.
/// Source-scope erase selects no goals and `wake_config` has no source
/// attribution, so every wake row remains outside that erase scope.
async fn delete_wake_configs(
    tx: &mut Tx<'_>,
    owner: OwnerRef,
    scope: SelectionScope<'_>,
) -> Result<u64, StorageError> {
    let (_owner_kind, owner_id) = owner_binds(&owner);
    let result = match scope {
        SelectionScope::Owner => sqlx::query(
            "DELETE FROM proxima_core.wake_config
              WHERE owner_id = $1",
        )
        .bind(owner_id)
        .execute(&mut **tx)
        .await
        .map_err(map_err)?,
        SelectionScope::Source(_) => return Ok(0),
    };
    Ok(result.rows_affected())
}

/// Rows destroyed by [`delete_blobs`], reported separately because the three
/// tables hold different owner data: content hashes, S3 upload metadata, and
/// whatever a flavor's citation payload carries.
struct BlobEraseCounts {
    uploads: u64,
    blobs: u64,
    cold_purge: ColdPurgePlan,
}

/// Destroy the owner's cited-blob rows: the registered citation sidecar rows
/// keyed on `blob_id`, then `blob_uploads`, then `blob` itself. That order is
/// the FK order — `blob_uploads.blob_id` and a flavor sidecar's own
/// `blob_id` reference both point at `blob` — and `memory.blob_id` is why this
/// runs after the memory deletions above.
///
/// `blob` carries `schema_id` and `content_hash`; `blob_uploads` carries
/// `bucket`, `object_key`, `filename`, `mime`, `sha256`, `etag` and
/// `error_message`. All of it is owner data, and nothing else collects it:
/// erase never deletes the `owners` row, so a forgotten statement here
/// leaves the rows behind forever.
///
/// Owner erase takes every blob row of the owner — the memory rows are
/// already gone, so a surviving
/// reference means the erase is wrong and the `NO ACTION` FK aborts it rather
/// than letting the row survive quietly. Source-scope candidates are captured
/// from the selected hot and cooled admissions before either table is deleted.
/// After those deletions, candidates still referenced by a surviving hot or
/// cooled admission are removed from the set.
///
/// `blob_uploads` rows with a NULL `blob_id` are pending or aborted uploads
/// attributable to the owner and to no source, so source-scope erase leaves
/// them exactly as it leaves owner-level delegated-authority grants.
///
/// ROW deletion is unconditional; OBJECT deletion is not. `blob` rows are
/// per-owner by `UNIQUE (owner_id, schema_id, content_hash)`, so deleting
/// this owner's rows can never touch another owner's. The S3 object is
/// shareable, though, so "the row is going" does not imply "the bytes are
/// going" — [`enqueue_blob_object_keys`] is where that question is asked.
async fn delete_blobs(
    tx: &mut Tx<'_>,
    owner: OwnerRef,
    scope: SelectionScope<'_>,
    surfaces: &OwnerSurfaces,
) -> Result<BlobEraseCounts, StorageError> {
    let (_owner_kind, owner_id) = owner_binds(&owner);
    if matches!(scope, SelectionScope::Source(_)) {
        sqlx::query(
            "DELETE FROM selected_blobs sb
              WHERE EXISTS (
                    SELECT 1 FROM proxima_core.memory m
                     WHERE m.blob_id = sb.blob_id
              )
                 OR EXISTS (
                    SELECT 1 FROM proxima_core.cooled c
                     WHERE c.blob_id = sb.blob_id
                 )",
        )
        .execute(&mut **tx)
        .await
        .map_err(map_err)?;
    }
    // The blob-keyed sweep tallies itself into whatever counter each
    // surface declares, so nothing is returned here to be re-counted.
    delete_keyed_surfaces(tx, surfaces, KeyedSet::Blobs).await?;
    let object_keys = enqueue_blob_object_keys(tx, owner_id, scope).await?;
    let uploads = match scope {
        SelectionScope::Owner => {
            sqlx::query("DELETE FROM proxima_core.blob_uploads WHERE owner_id = $1")
                .bind(owner_id)
                .execute(&mut **tx)
                .await
                .map_err(map_err)?
                .rows_affected()
        }
        SelectionScope::Source(_) => {
            delete_fixed_by_selected(
                tx,
                "proxima_core.blob_uploads",
                "blob_id",
                "selected_blobs",
                "blob_id",
            )
            .await?
        }
    };
    let blobs = delete_fixed_by_selected(
        tx,
        "proxima_core.blob",
        "blob_id",
        "selected_blobs",
        "blob_id",
    )
    .await?;
    Ok(BlobEraseCounts {
        uploads,
        blobs,
        cold_purge: ColdPurgePlan::from_keys(object_keys),
    })
}

/// Which of the erased scope's objects may actually be destroyed.
///
/// REFCOUNT BY QUERY, not by counter — `gc_unreferenced_content`'s idiom,
/// one level down. A mount makes object-to-upload-row many-to-one: two
/// owners' rows may name one object, so erasing one owner must leave the
/// bytes the other still reads.
///
/// The anti-join runs BEFORE the erase deletes this scope's rows, so it has
/// to exclude them explicitly rather than rely on them being gone: the
/// `NOT EXISTS` asks whether any row OUTSIDE the erased scope names the
/// key. Running it after the deletes would be simpler and wrong in the
/// other direction — nothing else in the transaction may observe the rows
/// as deleted before the commit that publishes it.
///
/// A key held back here is not leaked: it stays reachable through the
/// surviving owner's rows, and becomes an ordinary orphan for the bucket
/// sweep only once that owner's rows go too.
async fn enqueue_blob_object_keys(
    tx: &mut Tx<'_>,
    owner_id: uuid::Uuid,
    scope: SelectionScope<'_>,
) -> Result<Vec<String>, StorageError> {
    match scope {
        SelectionScope::Owner => sqlx::query_scalar(
            "INSERT INTO proxima_core.cold_purge_pending (object_key, owner_id)
             SELECT DISTINCT u.object_key, u.owner_id
               FROM proxima_core.blob_uploads u
              WHERE u.owner_id = $1
                AND NOT EXISTS (
                        SELECT 1
                          FROM proxima_core.blob_uploads other
                         WHERE other.object_key = u.object_key
                           AND other.owner_id <> $1
                    )
             ON CONFLICT (object_key) DO UPDATE SET enqueued_at = now()
             RETURNING object_key",
        )
        .bind(owner_id)
        .fetch_all(&mut **tx)
        .await
        .map_err(map_err),
        SelectionScope::Source(_) => sqlx::query_scalar(
            "INSERT INTO proxima_core.cold_purge_pending (object_key, owner_id)
             SELECT DISTINCT u.object_key, u.owner_id
               FROM proxima_core.blob_uploads u
               JOIN selected_blobs sb ON sb.blob_id = u.blob_id
              WHERE NOT EXISTS (
                        SELECT 1
                          FROM proxima_core.blob_uploads other
                         WHERE other.object_key = u.object_key
                           AND other.upload_id <> u.upload_id
                           AND NOT EXISTS (
                                   SELECT 1
                                     FROM selected_blobs sb2
                                    WHERE sb2.blob_id = other.blob_id
                               )
                    )
             ON CONFLICT (object_key) DO UPDATE SET enqueued_at = now()
             RETURNING object_key",
        )
        .fetch_all(&mut **tx)
        .await
        .map_err(map_err),
    }
}

/// Delete persisted projector source cursors for the erased scope inside the
/// erase transaction. Owner erase
/// removes every cursor for the owner; source-scope erase removes only the
/// matching `source`. Cursor bytes stay opaque — this is pure lawful cleanup so
/// a re-provisioned owner/source does not resume from a stale offset.
async fn delete_source_cursors(
    tx: &mut Tx<'_>,
    owner: OwnerRef,
    scope: SelectionScope<'_>,
) -> Result<u64, StorageError> {
    let (owner_kind, owner_id) = owner_binds(&owner);
    let result = match scope {
        SelectionScope::Owner => sqlx::query(
            "DELETE FROM proxima_core.source_cursors
              WHERE owner_kind = $1
                AND owner_id = $2",
        )
        .bind(owner_kind)
        .bind(owner_id)
        .execute(&mut **tx)
        .await
        .map_err(map_err)?,
        SelectionScope::Source(source_id) => sqlx::query(
            "DELETE FROM proxima_core.source_cursors
              WHERE owner_kind = $1
                AND owner_id = $2
                AND source = $3",
        )
        .bind(owner_kind)
        .bind(owner_id)
        .bind(source_id.as_str())
        .execute(&mut **tx)
        .await
        .map_err(map_err)?,
    };
    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::{KeyedSet, keyed_set};

    #[test]
    fn erase_sql_does_not_name_retired_suppression_table() {
        let src = include_str!("owner_erase.rs");
        let needle = format!("{}.{}", "proxima_core", "compliance_suppression_keys");
        assert!(
            !src.contains(&needle),
            "the schema declares no suppression table"
        );
    }

    #[test]
    fn owner_erase_names_cooled() {
        let src = include_str!("owner_erase.rs");
        assert!(
            src.contains("proxima_core.cooled"),
            "owner erase must select and delete cooled"
        );
        assert!(
            src.contains("gc_unreferenced_content"),
            "owner erase must GC Content"
        );
    }

    #[test]
    fn group_abandonment_counts_group_memberships() {
        let src = include_str!("owner_erase.rs");
        let retired = format!("{}.{}", "proxima_core", "resolved_group_memberships");
        let live = format!("{}.{}", "proxima_core", "group_memberships");
        assert!(
            !src.contains(&retired),
            "the resolved-membership view is not part of the schema"
        );
        assert!(
            src.contains(&live),
            "abandonment counts proxima_core.group_memberships"
        );
    }

    /// The registry-driven sweep reaches every core sidecar: the
    /// memory-keyed pass for the memory sidecars, the goal-keyed pass for
    /// `task_goal_v1`.
    #[test]
    fn the_registry_pass_reaches_every_core_sidecar() {
        use proxima_core::flavor::{EraseLeg, KeyShape};

        let registry = proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests();
        let surfaces = proxima_core::owner_inverse::OwnerSurfaces::for_registry(&registry);

        for table in [
            "proxima_core.agent_derivation_v1",
            "proxima_core.agent_note_v1",
            "proxima_core.utterance_v1",
        ] {
            assert_eq!(
                surfaces.erase_leg(table),
                EraseLeg::Keyed(KeyShape::MemoryT { column: "t" }),
                "{table} is a memory sidecar; the memory-keyed sweep must reach it"
            );
        }
        assert_eq!(
            surfaces.erase_leg("proxima_core.task_goal_v1"),
            EraseLeg::Keyed(KeyShape::GoalT { column: "t" }),
            "task_goal_v1 is a goal sidecar; the goal sweep must reach it"
        );
    }

    /// The substrate half of the completeness property.
    ///
    /// Which leg owns a surface is decided in core, at freeze, for every
    /// flavor (`FlavorRegistryError::UndeletableSurface`). What core cannot
    /// know is whether THIS crate can actually execute the answer, and the
    /// two generic loops skip anything they cannot map: a `Keyed` leg whose
    /// key shape has no selection set here would be silently dropped in
    /// exactly the way freeze prevents one flavor-side.
    ///
    /// So: no core surface resolves to `Unreachable`, and every keyed one
    /// maps to a temp table this transaction fills.
    #[test]
    fn every_declared_surface_has_a_leg_this_crate_can_run() {
        use proxima_core::flavor::EraseLeg;

        let registry = proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests();
        let surfaces = proxima_core::owner_inverse::OwnerSurfaces::for_registry(&registry);
        assert!(
            surfaces.surfaces().len() > 20,
            "the registry should carry the whole core contract, got {}",
            surfaces.surfaces().len()
        );
        for surface in surfaces.surfaces() {
            match surfaces.erase_leg(surface.table) {
                EraseLeg::Unreachable => panic!(
                    "{} resolves to Unreachable, which freeze should have refused",
                    surface.table
                ),
                EraseLeg::Keyed(key) => assert!(
                    keyed_set(key).is_some(),
                    "{} is keyed on {key:?}, which this crate builds no selection set for",
                    surface.table
                ),
                EraseLeg::Owned { .. }
                | EraseLeg::Bespoke
                | EraseLeg::Cascade
                | EraseLeg::Never { .. } => {}
            }
        }
    }

    /// The exemption list stays sorted. A list nobody can scan is a list
    /// nobody prunes, and this one is read by two crates.
    #[test]
    fn the_bespoke_leg_list_is_sorted_by_table() {
        let tables = proxima_core::flavor::FLAVOR_0.bespoke_erase_legs;
        let mut sorted = tables.to_vec();
        sorted.sort_unstable();
        assert_eq!(sorted, tables, "keep the exemption list sorted by table");
    }

    /// The three selection sets are the whole vocabulary of a keyed leg.
    #[test]
    fn every_homed_key_shape_maps_to_a_selection_set() {
        use proxima_core::flavor::KeyShape;

        assert_eq!(
            keyed_set(KeyShape::MemoryT { column: "t" }),
            Some((KeyedSet::Memories, "t"))
        );
        assert_eq!(
            keyed_set(KeyShape::GoalT { column: "g" }),
            Some((KeyedSet::Goals, "g"))
        );
        assert_eq!(
            keyed_set(KeyShape::BlobId { column: "b" }),
            Some((KeyedSet::Blobs, "b"))
        );
        assert_eq!(keyed_set(KeyShape::OwnerId), None);
        assert_eq!(keyed_set(KeyShape::Custom(&["a", "b"])), None);
    }
}
