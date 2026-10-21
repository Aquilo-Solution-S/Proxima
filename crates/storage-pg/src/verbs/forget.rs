//! Forget / erase.
#![allow(clippy::missing_errors_doc, clippy::doc_markdown)]

use std::collections::BTreeSet;

use proxima_core::flavor::{EraseLeg, ForgetLeg, KeyShape};
use proxima_core::owner_inverse::OwnerSurfaces;
use proxima_core::{ColdObjectStore, ObjectStoreId, OwnerId, PurgeDeleted, StorageError};

use crate::access::owner_columns::{other_upload_implies_key_sql, upload_implied_keys_sql};
use crate::verbs::maintenance::PurgeBackoff;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::map_err;
use crate::pg_enums::PgMemoryKind;
use crate::pg_ident::PgIdent;
use crate::sidecars::PgSidecarRegistryFrozen;

/// The in-process purge stub dev and test hosts wire when no object store
/// is configured. It is [`ObjectStoreId::memory`], so it drains only debts it
/// was handed and never an S3 store's.
#[derive(Debug, Default)]
pub struct MemoryColdStore;

static MEMORY_STORE_ID: std::sync::LazyLock<ObjectStoreId> =
    std::sync::LazyLock::new(ObjectStoreId::memory);

#[async_trait::async_trait]
impl ColdObjectStore for MemoryColdStore {
    fn store_id(&self) -> &ObjectStoreId {
        // This cannot be an S3 store. A host that boots without its S3
        // configuration must not acknowledge that store's durable debts.
        &MEMORY_STORE_ID
    }

    async fn delete(&self, _key: &str) -> Result<PurgeDeleted, StorageError> {
        Ok(PurgeDeleted::Absent)
    }
}

/// The rows a lifecycle call may reach: one owner's, of every stored Memory
/// kind up to the permit's write limit. Forget and restore see rows in
/// either state, so this is where the rule narrows their rows.
#[derive(Debug, Clone)]
pub(crate) struct Reach {
    owner_id: Uuid,
    kinds: Vec<PgMemoryKind>,
}

impl Reach {
    pub(crate) fn of(permit: &proxima_core::storage_ports::OwnerWritePermit) -> Self {
        Self {
            owner_id: permit.owner().into_inner(),
            kinds: kinds_within(permit.access_kind()),
        }
    }
}

/// The stored Memory kinds at or below `limit`.
pub(crate) fn kinds_within(limit: proxima_core::AccessKind) -> Vec<PgMemoryKind> {
    use proxima_core::AccessKind;
    [
        (AccessKind::Fact, PgMemoryKind::Fact),
        (AccessKind::Abstraction, PgMemoryKind::Abstraction),
        (AccessKind::Perspective, PgMemoryKind::Perspective),
    ]
    .into_iter()
    .filter(|(access, _)| access.rank() <= limit.rank())
    .map(|(_, kind)| kind)
    .collect()
}

/// Owner-pinned sidecars do not take part in forget/restore at all.
///
/// They are not the Memory's data: they belong to the owner that acted, and
/// after a transfer that is not the owner doing the forgetting. Hiding them
/// with the Memory would let the receiving owner hide — or, by erasing the
/// Memory, destroy — another owner's audit trail. They simply stay where
/// they are and stay visible, which is safe because they hold no foreign key
/// into `memory`.
///
/// The same predicate decides which stamps get a presence trigger: an
/// owner-pinned stamp is a record of a past write, not a claim about the
/// present, so `stamp ⊆ rows` cannot be asked of it. One function, so the
/// forget lane and the write-time constraint cannot drift apart.
fn is_owner_pinned(sidecars: &PgSidecarRegistryFrozen, table: &str) -> bool {
    sidecars.is_owner_pinned_memory_sidecar_table(table)
}

/// Blob objects a committed erase still owes the object store.
///
/// An erase marks each locator in `proxima_core.cold_purge_pending` inside the
/// transaction that deletes its upload row, and destroys the object only
/// after that transaction commits (`purge_cold_objects_after_commit`).
/// Deleting from inside the transaction loses the object outright when the
/// transaction later rolls back — the locator comes back, the bytes do not —
/// and a crash between commit and destruction leaves a reclaimable mark rather
/// than a row naming nothing.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ColdPurgePlan {
    entries: Vec<ColdPurgeEntry>,
}

/// One debt: the key, and the object store that holds those bytes.
///
/// The store travels with the key because the queue outlives the process
/// that wrote it, and a redeployment may wire another store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColdPurgeEntry {
    pub object_key: String,
    /// The stored [`ObjectStoreId`] text the debt was incurred against.
    pub object_store: String,
}

impl ColdPurgePlan {
    pub(crate) fn from_entries(entries: Vec<ColdPurgeEntry>) -> Self {
        Self { entries }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[must_use]
    pub fn entries(&self) -> &[ColdPurgeEntry] {
        &self.entries
    }

    #[must_use]
    pub fn object_keys(&self) -> Vec<String> {
        self.entries
            .iter()
            .map(|entry| entry.object_key.clone())
            .collect()
    }
}

/// Result of one post-commit exact-key purge attempt.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ColdPurgeOutcome {
    pub attempted: u64,
    /// Deleted, or already retired by another drain.
    pub purged: u64,
    /// The key held nothing; the debt is cleared.
    pub absent: u64,
    pub failed: u64,
    /// At least one durable row could not be reconciled. This deliberately
    /// over-reports when object deletion succeeded but the database write did
    /// not: repeating an exact-key delete is safe.
    pub pending: bool,
}

/// Destroy the objects a committed erase marked pending, clearing each
/// mark as its object goes.
///
/// Best effort by construction: the erase is already committed, so a refusing
/// object store cannot undo it. A key whose destruction (or whose mark clear)
/// fails keeps its `cold_purge_pending` row, which is the durable record the
/// drain reads; the failed attempt is recorded so backoff starts at the first
/// failure. Counts reconciled plan entries, including debts already retired
/// by another drain before provider I/O begins.
#[cfg(test)]
pub(crate) async fn purge_cold_objects_after_commit(
    pool: &PgPool,
    platform: &crate::PgPlatformScope,
    cold: &dyn ColdObjectStore,
    plan: &ColdPurgePlan,
) -> ColdPurgeOutcome {
    purge_cold_objects_after_commit_with_platform(pool, platform, cold, plan).await
}

pub(crate) async fn purge_cold_objects_after_commit_with_platform(
    pool: &PgPool,
    platform: &crate::PgPlatformScope,
    cold: &dyn ColdObjectStore,
    plan: &ColdPurgePlan,
) -> ColdPurgeOutcome {
    purge_cold_objects_with_backoff(pool, platform, cold, plan, PurgeBackoff::default()).await
}

/// How one debt was discharged.
enum Discharged {
    Removed,
    Absent,
    /// Another drain cleared it before this one looked.
    Retired,
}

pub(crate) async fn purge_cold_objects_with_backoff(
    pool: &PgPool,
    platform: &crate::PgPlatformScope,
    cold: &dyn ColdObjectStore,
    plan: &ColdPurgePlan,
    backoff: PurgeBackoff,
) -> ColdPurgeOutcome {
    let mut outcome = ColdPurgeOutcome {
        attempted: u64::try_from(plan.entries().len()).unwrap_or(u64::MAX),
        ..ColdPurgeOutcome::default()
    };
    for entry in plan.entries() {
        let key = entry.object_key.as_str();
        // Never delete against the wrong store, and never silently drop the
        // debt either: the row stays, `failed` counts it, and the receipt
        // reports the erase as still owing bytes. A drain wired to the store
        // that holds the bytes discharges it.
        if entry.object_store != cold.store_id().as_str() {
            outcome.failed = outcome.failed.saturating_add(1);
            tracing::warn!(
                key,
                row_store = entry.object_store,
                wired_store = %cold.store_id(),
                "purge debt names an object store this process is not wired to; leaving it queued"
            );
            continue;
        }
        match purge_current_cold_debt(pool, platform, cold, key, backoff).await {
            Ok(Discharged::Removed | Discharged::Retired) => {
                outcome.purged = outcome.purged.saturating_add(1);
            }
            Ok(Discharged::Absent) => outcome.absent = outcome.absent.saturating_add(1),
            Err(error) => {
                outcome.failed = outcome.failed.saturating_add(1);
                tracing::warn!(
                    %error,
                    key,
                    "object outlives its committed erase; the purge-pending mark stays for retry"
                );
            }
        }
    }
    outcome.pending = outcome.purged.saturating_add(outcome.absent) != outcome.attempted;
    outcome
}

/// A queue row's version, used only for the duration of one provider call.
/// These PostgreSQL stamps are not persistent object or transaction IDs.
/// Chosen over a schema `version` column so the guard needs no migration
/// (AGENTS.md migration policy); a row moved by VACUUM FULL or a HOT update
/// simply fails the compare, which leaves the debt queued for retry.
#[derive(sqlx::FromRow)]
struct ColdPurgeVersion {
    row_xmin: String,
    row_ctid: String,
}

async fn purge_current_cold_debt(
    pool: &PgPool,
    platform: &crate::PgPlatformScope,
    cold: &dyn ColdObjectStore,
    object_key: &str,
    backoff: PurgeBackoff,
) -> Result<Discharged, StorageError> {
    let mut tx = crate::platform_scope::begin_platform_transaction(pool, platform).await?;
    let version = sqlx::query_as::<_, ColdPurgeVersion>(
        "SELECT xmin::text AS row_xmin, ctid::text AS row_ctid
           FROM proxima_core.cold_purge_pending
          WHERE object_store = $1 AND object_key = $2",
    )
    .bind(cold.store_id().as_str())
    .bind(object_key)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?;
    tx.commit().await.map_err(map_err)?;
    let Some(version) = version else {
        // Another drain already retired this entry. A stale plan no longer
        // authorizes deleting whatever may now occupy the same object key.
        return Ok(Discharged::Retired);
    };
    // No database transaction or connection remains held during provider I/O.
    // A producer may publish bytes and renew this debt while delete is in
    // flight; its new row version must survive our earlier acknowledgement.
    let discharged = match cold.delete(object_key).await {
        Ok(PurgeDeleted::Absent) | Err(StorageError::NotFound) => Discharged::Absent,
        Ok(_) => Discharged::Removed,
        Err(error) => {
            // The same version guard as the clear: a debt renewed while the
            // delete was in flight is due now and keeps its fresh schedule.
            if let Err(record) = record_purge_failure(
                pool,
                platform,
                cold.store_id(),
                object_key,
                &version,
                backoff,
                &error,
            )
            .await
            {
                tracing::warn!(%record, key = object_key, "could not record a failed purge attempt");
            }
            return Err(error);
        }
    };
    if !clear_cold_purge_pending(pool, platform, cold.store_id(), object_key, &version).await? {
        return Err(StorageError::Unavailable(format!(
            "cold purge debt for {object_key} changed during deletion; leaving it for retry"
        )));
    }
    Ok(discharged)
}

/// Longest failure text a debt keeps, in bytes.
const MAX_PURGE_ERROR_BYTES: usize = 1024;

/// The failure text a debt records: control characters replaced, bounded on
/// a character boundary.
fn purge_error_text(error: &StorageError) -> String {
    let text: String = error
        .to_string()
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if text.len() <= MAX_PURGE_ERROR_BYTES {
        return text;
    }
    let mut end = MAX_PURGE_ERROR_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

/// Record one failed attempt and back the debt off, if it is still the
/// version the attempt observed.
async fn record_purge_failure(
    pool: &PgPool,
    platform: &crate::PgPlatformScope,
    store: &ObjectStoreId,
    object_key: &str,
    version: &ColdPurgeVersion,
    backoff: PurgeBackoff,
    error: &StorageError,
) -> Result<bool, StorageError> {
    let mut tx = crate::platform_scope::begin_platform_transaction(pool, platform).await?;
    // Delay after the nth failure: min(base * 2^(n-1), max). `attempts` is
    // the count before this failure, so it is the exponent.
    let updated = sqlx::query(
        "UPDATE proxima_core.cold_purge_pending
            SET attempts = attempts + 1,
                last_attempt_at = now(),
                last_error = $5,
                next_attempt_at = now() + make_interval(secs => LEAST(
                    $6::double precision * power(2::double precision, LEAST(attempts, 40)),
                    $7::double precision))
          WHERE object_store = $1 AND object_key = $2
            AND xmin::text = $3 AND ctid::text = $4",
    )
    .bind(store.as_str())
    .bind(object_key)
    .bind(&version.row_xmin)
    .bind(&version.row_ctid)
    .bind(purge_error_text(error))
    .bind(backoff.base.as_secs_f64())
    .bind(backoff.max.as_secs_f64())
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;
    tx.commit().await.map_err(map_err)?;
    Ok(updated.rows_affected() == 1)
}

/// A completed provider deletion retires only the queue version it observed.
///
/// The queue IS the debt. The drain answers "is anything owed" by counting
/// rows, not by trusting a flag that a crash between two writes could leave
/// set forever.
async fn clear_cold_purge_pending(
    pool: &PgPool,
    platform: &crate::PgPlatformScope,
    store: &ObjectStoreId,
    object_key: &str,
    version: &ColdPurgeVersion,
) -> Result<bool, StorageError> {
    let mut tx = crate::platform_scope::begin_platform_transaction(pool, platform).await?;
    let deleted = sqlx::query(
        "DELETE FROM proxima_core.cold_purge_pending
          WHERE object_store = $1 AND object_key = $2
            AND xmin::text = $3 AND ctid::text = $4",
    )
    .bind(store.as_str())
    .bind(object_key)
    .bind(&version.row_xmin)
    .bind(&version.row_ctid)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;
    tx.commit().await.map_err(map_err)?;
    Ok(deleted.rows_affected() == 1)
}

/// Take a sorted, deduplicated advisory-lock set in one round-trip.
///
/// The ordering guarantee — the whole point of these locks — comes from two
/// facts, not from an `ORDER BY`. `unnest` over an array plans as a bare
/// `Function Scan` that yields elements in array order with no sort node
/// above it, and `pg_advisory_xact_lock` is `VOLATILE` and `PARALLEL
/// RESTRICTED`, so the planner can neither hoist the call nor push it into a
/// worker that would evaluate it out of order. The array is therefore sorted
/// here, in Rust, and the statement carries no `ORDER BY`: one would sort the
/// returned rows, which nothing reads, and imply an ordering guarantee the
/// projection does not get from it.
///
/// A per-id statement would be the same locks in the same order; it would
/// just pay a network round-trip for each one, and these sets are as large as
/// an admission's pin count or a series erase's version count.
async fn lock_advisory_ids_tx(
    tx: &mut Transaction<'_, Postgres>,
    namespace: &str,
    ids: &[Uuid],
) -> Result<(), StorageError> {
    if ids.is_empty() {
        return Ok(());
    }
    let mut sorted = ids.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    sqlx::query(
        "SELECT pg_advisory_xact_lock(hashtextextended($1::text || id::text, 0))
           FROM unnest($2::uuid[]) AS id",
    )
    .bind(namespace)
    .bind(&sorted)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;
    Ok(())
}

/// Acquire the one advisory lock vocabulary used by Memory series handles.
///
/// Handle locks are deliberately a different namespace from the `t` locks
/// below: a caller-supplied handle may equal a caller-supplied `t`, and
/// conflating those identities would make an unrelated series and admission
/// serialize. Memory admission, per-entity lifecycle, and series erase acquire
/// their complete handle set in sorted order before any lifecycle `t` lock.
pub(crate) async fn lock_memory_handles_tx(
    tx: &mut Transaction<'_, Postgres>,
    handles: &[Uuid],
) -> Result<(), StorageError> {
    lock_advisory_ids_tx(tx, "proxima-memory-handle:", handles).await
}

/// Acquire the one lifecycle lock vocabulary used by admission, hydration,
/// transfer, forget and erase. Callers hand in the complete target set before
/// taking any row/blob lock; sorting makes crossed declarations wait rather
/// than deadlock. Memory callers acquire all handle locks before entering this
/// helper; this helper itself never acquires a handle lock.
pub(crate) async fn lock_lifecycle_targets_tx(
    tx: &mut Transaction<'_, Postgres>,
    targets: &[Uuid],
) -> Result<(), StorageError> {
    lock_advisory_ids_tx(tx, "proxima-forget:", targets).await
}

/// Lock the source and every live or forgotten non-Fact depender before the
/// source row is locked or forgotten. The memory forget-grounding trigger
/// takes the live set as a SQL backstop; the re-read rejects a growing set rather than extending an
/// already-held advisory set out of UUID order.
async fn lock_forget_footprint_tx(
    tx: &mut Transaction<'_, Postgres>,
    source_t: Uuid,
    source_handle: Uuid,
) -> Result<(), StorageError> {
    // Resolve every dependent handle before taking any lifecycle lock. A
    // dependent append takes its handle first and may name `source_t`; taking
    // only the source handle here would invert the order and leave the
    // dependent handle outside this operation's serialization domain.
    let dependent_handles: Vec<Uuid> = sqlx::query_scalar(
        "SELECT DISTINCT handle FROM proxima_core.memory /* FORGET-POLICY: includes-forgotten (forget and restore see the row in either state) */
          WHERE kind <> 'fact'
            AND t <> $1
            AND (origins @> ARRAY[$1]::uuid[] OR refs @> ARRAY[$1]::uuid[])
          ORDER BY handle",
    )
    .bind(source_t)
    .fetch_all(tx.as_mut())
    .await
    .map_err(map_err)?;
    let mut handles = Vec::with_capacity(1 + dependent_handles.len());
    handles.push(source_handle);
    handles.extend(dependent_handles.iter().copied());
    lock_memory_handles_tx(tx, &handles).await?;

    let grounded: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT t, handle FROM proxima_core.memory /* FORGET-POLICY: includes-forgotten (forget and restore see the row in either state) */
          WHERE kind <> 'fact'
            AND t <> $1
            AND (origins @> ARRAY[$1]::uuid[] OR refs @> ARRAY[$1]::uuid[])
          ORDER BY t",
    )
    .bind(source_t)
    .fetch_all(tx.as_mut())
    .await
    .map_err(map_err)?;
    // The SQL result is sorted; prepending the source made `handles` unsorted.
    // Search its ordered tail to avoid a linear scan for every depender.
    let handle_was_locked =
        |handle: &Uuid| *handle == source_handle || dependent_handles.binary_search(handle).is_ok();
    if grounded
        .iter()
        .any(|(_, handle)| !handle_was_locked(handle))
    {
        return Err(StorageError::Retryable(
            "forget depender handle set grew before lifecycle lock acquisition".into(),
        ));
    }
    let dependents: Vec<Uuid> = grounded.into_iter().map(|(t, _)| t).collect();
    let mut targets = Vec::with_capacity(1 + dependents.len());
    targets.push(source_t);
    targets.extend(dependents.iter().copied());
    lock_lifecycle_targets_tx(tx, &targets).await?;

    let after: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT t, handle FROM proxima_core.memory /* FORGET-POLICY: includes-forgotten (forget and restore see the row in either state) */
          WHERE kind <> 'fact'
            AND t <> $1
            AND (origins @> ARRAY[$1]::uuid[] OR refs @> ARRAY[$1]::uuid[])
          ORDER BY t",
    )
    .bind(source_t)
    .fetch_all(tx.as_mut())
    .await
    .map_err(map_err)?;
    // Both queries `ORDER BY t` and exclude `source_t`, so target membership is
    // a binary search over `dependents` rather than a scan of `targets`.
    if after.iter().all(|(target, handle)| {
        dependents.binary_search(target).is_ok() && handle_was_locked(handle)
    }) {
        return Ok(());
    }
    Err(StorageError::Retryable(
        "forget depender footprint grew after lifecycle lock acquisition".into(),
    ))
}

/// The owner transfer's half of the forget serialization: the same
/// per-memory advisory lock [`lock_lifecycle_targets_tx`] takes, over every `t`
/// of the series, in sorted order. Forget takes its single lock before any
/// row lock, and the transfer calls this before writing any row, so the two
/// paths cannot form an advisory-lock cycle — a forget on a locked `t`
/// simply queues behind the transfer (and vice versa).
pub(crate) async fn lock_forget_memories_tx(
    tx: &mut Transaction<'_, Postgres>,
    ts: &[Uuid],
) -> Result<(), StorageError> {
    lock_lifecycle_targets_tx(tx, ts).await
}

/// Every row one forgotten `t` takes with it, from two iteration sources
/// and through one statement shape.
///
/// **The stamp, for the `Dumped` legs.** `memory.sidecar_tables` records
/// what THIS row actually wrote, and that is why forget walks it rather than
/// the registry: a registry that gained a sidecar after this row was written
/// must not delete from a table the row never touched, and one that LOST a
/// table must still forget rows written before it went.
/// `forget_dumps_only_stamped_tables_and_skips_unregistered_scan` pins that
/// property by stamping a registered-but-nonexistent table and requiring the
/// forget to succeed. The PG registry is the veto (a stamp naming an
/// unregistered table is a constraint violation) and the owner-pin filter;
/// the DB backs the other direction with `assert_sidecar_stamp_declared`.
///
/// **The contract, for the `Deleted` legs.** The embedding triple is a set
/// of derived rows nothing stamps — no lane records them on the memory — so
/// there is no stamp to walk and the declaration is the only list. The
/// sketch is not a leg since #488 S14: a restore needs its text back.
///
/// Stamp legs run before declaration legs. Neither set holds a foreign key
/// into the other.
async fn delete_memory_dependents(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    surfaces: &OwnerSurfaces,
    sidecar_tables: &[String],
    t: Uuid,
) -> Result<(), StorageError> {
    let mut legs: Vec<(&str, &str)> = Vec::with_capacity(sidecar_tables.len() + 4);
    for table in sidecar_tables {
        if !sidecars.is_memory_sidecar_table(table) {
            return Err(StorageError::ConstraintViolation(format!(
                "stamped sidecar table {table} is not registered"
            )));
        }
        // See [`is_owner_pinned`]: forgetting a Memory must not delete
        // somebody else's audit rows.
        if is_owner_pinned(sidecars, table) {
            continue;
        }
        // The declaration names the column when it has an opinion. It does
        // not always: a stamp may name a table the PG registry knows and no
        // contract declares, which arrives here as `Unreachable` and is the
        // case the skip-unregistered-scan test installs deliberately. `t` is
        // the fallback because it is what `pg_sidecar!` requires of every
        // memory sidecar it registers.
        //
        // `Kept` is spelled out rather than folded into that fallback, and
        // the fallback DELETES. A `Keep` declaration reaching a delete is
        // the failure this arm exists to make loud instead of silent: the
        // table was declared as one the forget does not touch, and the walk
        // is about to destroy it. `freeze_against`'s
        // `check_keep_is_owner_pinned` makes that unreachable — `Keep`
        // requires `SidecarOwnerMode::Pinned`, and Pinned tables were
        // skipped above — so this is the assertion that the boot check ran,
        // not a second policy.
        let key_column = match surfaces.forget_leg(table) {
            ForgetLeg::Marked => sidecars.memory_key_column(table).unwrap_or("t"),
            ForgetLeg::Kept { why } => {
                return Err(StorageError::Internal(format!(
                    "{table} declares ForgetRule::Keep ({why}) and is not owner-pinned, \
                     so the forget cannot honour it; freeze_against must refuse this registry"
                )));
            }
            ForgetLeg::Deleted { .. } | ForgetLeg::Cascaded { .. } | ForgetLeg::Unreachable => "t",
        };
        legs.push((table.as_str(), key_column));
    }
    legs.extend(surfaces.generated_forget_legs());
    legs.extend(erase_kept_memory_legs(surfaces));
    run_forget_legs(tx, legs, t).await
}

/// What a forget deletes: the rows search and embedding derive from the
/// memory — every linked flavor's search projection row, and its `Deleted`
/// legs (the embedding triple). The memory row, its sketch and its sidecar
/// rows stay, hidden by `forgotten_at`; before #488 the projection
/// went by cascade with the sidecar row the forget deleted.
async fn delete_derived_dependents(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    surfaces: &OwnerSurfaces,
    t: Uuid,
) -> Result<(), StorageError> {
    let projections = sidecars.projection_tables();
    let mut legs: Vec<(&str, &str)> = projections
        .iter()
        .map(|table| {
            (
                table.as_str(),
                proxima_core::flavor::PROJECTION_MEMORY_COLUMN,
            )
        })
        .collect();
    legs.extend(surfaces.generated_forget_legs());
    run_forget_legs(tx, legs, t).await
}

async fn run_forget_legs(
    tx: &mut Transaction<'_, Postgres>,
    legs: Vec<(&str, &str)>,
    t: Uuid,
) -> Result<(), StorageError> {
    for (table, key_column) in legs {
        let sql = forget_leg_sql(table, key_column)?;
        // SQL-POLICY: generated
        sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(t)
            .execute(tx.as_mut())
            .await
            .map_err(map_err)?;
    }
    Ok(())
}

/// The surfaces [`erase_memory`] deletes with a hand-written statement of
/// its own, because each needs an owner predicate or an ordering the
/// generated `DELETE ... WHERE <key> = $1` does not carry.
const ERASE_MEMORY_EXPLICIT: &[&str] = &["proxima_core.memory", "proxima_core.ingest_keys"];

/// Every memory-keyed surface a forget KEEPS (its rows outlive a forget) and
/// owner erase deletes by key, minus the ones [`erase_memory`] deletes
/// explicitly. A hard erase of one admission owes these rows the same
/// delete owner erase gives them, and reading the set off the declarations
/// means a newly declared kept surface is reached without a named leg
/// (`memory_external_origin`, #478).
fn erase_kept_memory_legs(surfaces: &OwnerSurfaces) -> Vec<(&'static str, &'static str)> {
    surfaces
        .surfaces()
        .iter()
        .filter_map(|surface| {
            let EraseLeg::Keyed(KeyShape::MemoryT { column }) = surfaces.erase_leg(surface.table)
            else {
                return None;
            };
            (matches!(surfaces.forget_leg(surface.table), ForgetLeg::Kept { .. })
                && !ERASE_MEMORY_EXPLICIT.contains(&surface.table))
            .then_some((surface.table, column))
        })
        .collect()
}

/// The one statement a forget leg runs, over the forgotten `t`.
///
/// Every identifier here is a `&'static str` from a `const` contract or a
/// `pg_sidecar!` registration, both freeze-validated, and every contract one
/// is additionally resolved against `information_schema` by
/// `flavor_contract_acceptance::every_column_a_declaration_names_is_a_column_the_catalog_has`.
/// `PgIdent`'s whitelist is what makes the substitution `%I`-equivalent.
fn forget_leg_sql(table: &str, key_column: &str) -> Result<String, StorageError> {
    let table = PgIdent::table(table)?;
    let key = PgIdent::column(key_column)?;
    // SQL-POLICY: PgIdent
    Ok(format!(
        "DELETE FROM {tbl} WHERE {col} = $1",
        tbl = table.as_str(),
        col = key.as_str()
    ))
}

/// Point `memory_head.t` at the newest version that is not forgotten, or
/// delete the head when the series has no row left. `memory.handle` FK
/// requires the memory row to be gone first, so a series whose every version
/// is forgotten keeps its head on the newest forgotten version; owner reads
/// skip that row, so the series reads as absent. The caller must already hold
/// the complete advisory lock for this handle.
pub(crate) async fn sync_memory_head(
    tx: &mut Transaction<'_, Postgres>,
    handle: Uuid,
) -> Result<(), StorageError> {
    let remaining: Option<Uuid> = sqlx::query_scalar(
        "SELECT t FROM proxima_core.memory WHERE handle = $1 ORDER BY (forgotten_at IS NULL) DESC, t DESC LIMIT 1", /* FORGET-POLICY: includes-forgotten (head sync sees forgotten rows, then skips them) */
    )
    .bind(handle)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?;
    match remaining {
        Some(t) => {
            sqlx::query("UPDATE proxima_core.memory_head SET t = $2 WHERE handle = $1")
                .bind(handle)
                .bind(t)
                .execute(tx.as_mut())
                .await
                .map_err(map_err)?;
        }
        None => {
            sqlx::query("DELETE FROM proxima_core.memory_head WHERE handle = $1")
                .bind(handle)
                .execute(tx.as_mut())
                .await
                .map_err(map_err)?;
        }
    }
    Ok(())
}

async fn probe_memory_handle_for_owner(
    tx: &mut Transaction<'_, Postgres>,
    t: Uuid,
    owner_id: Uuid,
) -> Result<Option<Uuid>, StorageError> {
    sqlx::query_scalar(
        "SELECT handle FROM proxima_core.memory /* FORGET-POLICY: includes-forgotten (forget and restore see the row in either state) */
          WHERE t = $1 AND owner_id = $2",
    )
    .bind(t)
    .bind(owner_id)
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)
}

/// The owner's row for `t`, locked, when it is live.
const LOCK_LIVE_OWNED_SQL: &str = "SELECT handle, kind FROM proxima_core.memory /* FORGET-POLICY: includes-forgotten (forget and restore see the row in either state) */
          WHERE t = $1 AND owner_id = $2 AND forgotten_at IS NULL
          FOR UPDATE";

/// Forget one memory: mark its row `forgotten_at`, delete what search and
/// embedding derive from it, and move the series head off it.
///
/// The row and its sidecar rows stay. Runs inside the lifecycle scope, the
/// one place an owner transaction may write a forgotten row, and leaves it
/// before returning. No object store is involved.
///
/// The caller holds `tx`; nothing here commits.
pub(crate) async fn forget_memory(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    surfaces: &OwnerSurfaces,
    t: Uuid,
    expected_owner_id: Uuid,
) -> Result<(), StorageError> {
    forget_marked(tx, sidecars, surfaces, t, expected_owner_id, None).await
}

pub(crate) async fn forget_memory_oneshot_in_transaction(
    mut tx: Transaction<'static, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    surfaces: &OwnerSurfaces,
    t: Uuid,
    reach: &Reach,
) -> Result<(), StorageError> {
    forget_marked(
        &mut tx,
        sidecars,
        surfaces,
        t,
        reach.owner_id,
        Some(&reach.kinds),
    )
    .await?;
    crate::write_wake::commit(tx).await
}

async fn forget_marked(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    surfaces: &OwnerSurfaces,
    t: Uuid,
    expected_owner_id: Uuid,
    kinds: Option<&[PgMemoryKind]>,
) -> Result<(), StorageError> {
    let lifecycle = crate::lifecycle_scope::PgLifecycleScope::enter(tx.as_mut()).await?;
    let result = forget_in_lifecycle(tx, sidecars, surfaces, t, expected_owner_id, kinds).await;
    lifecycle.finish(tx.as_mut(), result).await
}

async fn forget_in_lifecycle(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    surfaces: &OwnerSurfaces,
    t: Uuid,
    expected_owner_id: Uuid,
    kinds: Option<&[PgMemoryKind]>,
) -> Result<(), StorageError> {
    let handle = probe_memory_handle_for_owner(tx, t, expected_owner_id)
        .await?
        .ok_or(StorageError::NotFound)?;
    lock_forget_footprint_tx(tx, t, handle).await?;
    let (locked_handle, kind): (Uuid, PgMemoryKind) = sqlx::query_as(LOCK_LIVE_OWNED_SQL)
        .bind(t)
        .bind(expected_owner_id)
        .fetch_optional(tx.as_mut())
        .await
        .map_err(map_err)?
        .ok_or(StorageError::NotFound)?;
    if locked_handle != handle || kinds.is_some_and(|kinds| !kinds.contains(&kind)) {
        return Err(StorageError::NotFound);
    }
    sqlx::query(
        "UPDATE proxima_core.memory /* FORGET-POLICY: includes-forgotten (forget and restore see the row in either state) */
            SET forgotten_at = now()
          WHERE t = $1 AND owner_id = $2",
    )
    .bind(t)
    .bind(expected_owner_id)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;
    delete_derived_dependents(tx, sidecars, surfaces, t).await?;
    crate::verbs::publication_outbox::cancel_undelivered_for_forget(tx, t).await?;
    sync_memory_head(tx, handle).await
}

/// The owner's row for `t`, locked, in either state.
const LOCK_OWNED_FOR_RESTORE_SQL: &str = "SELECT handle, kind, schema_id, sidecar_tables,
                lexical_language::text, forgotten_at IS NOT NULL
           FROM proxima_core.memory /* FORGET-POLICY: includes-forgotten (forget and restore see the row in either state) */
          WHERE t = $1 AND owner_id = $2
          FOR UPDATE";

/// Restore one forgotten memory: clear its `forgotten_at`, rebuild what
/// [`forget_memory`] deleted, and move the series head back onto it unless a
/// newer live version holds it.
///
/// Rebuilt: the search projection, in the language stamped on the memory
/// row (the projection row that carried it is gone); embedding jobs for the
/// spaces the Owner's route names now. The sidecar rows and the sketch never
/// left. Cancelled publication records stay cancelled. No object store is
/// involved.
///
/// A live `t` is a no-op success. Takes the same lock footprint as a
/// forget, inside the lifecycle scope, and leaves the scope before
/// returning. Commits `tx`.
pub(crate) async fn restore_memory_oneshot_in_transaction(
    mut tx: Transaction<'static, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    t: Uuid,
    reach: &Reach,
    embeddings: crate::verbs::fact_embeddings::RouteSpaces<'_>,
) -> Result<(), StorageError> {
    restore_marked(&mut tx, sidecars, t, reach, embeddings).await?;
    crate::write_wake::commit(tx).await
}

pub(crate) async fn restore_marked(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    t: Uuid,
    reach: &Reach,
    embeddings: crate::verbs::fact_embeddings::RouteSpaces<'_>,
) -> Result<(), StorageError> {
    let lifecycle = crate::lifecycle_scope::PgLifecycleScope::enter(tx.as_mut()).await?;
    let result = restore_in_lifecycle(tx, sidecars, t, reach, embeddings).await;
    lifecycle.finish(tx.as_mut(), result).await
}

async fn restore_in_lifecycle(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    t: Uuid,
    reach: &Reach,
    embeddings: crate::verbs::fact_embeddings::RouteSpaces<'_>,
) -> Result<(), StorageError> {
    let handle = probe_memory_handle_for_owner(tx, t, reach.owner_id)
        .await?
        .ok_or(StorageError::NotFound)?;
    lock_forget_footprint_tx(tx, t, handle).await?;
    let row: Option<(Uuid, PgMemoryKind, String, Vec<String>, String, bool)> =
        sqlx::query_as(LOCK_OWNED_FOR_RESTORE_SQL)
            .bind(t)
            .bind(reach.owner_id)
            .fetch_optional(tx.as_mut())
            .await
            .map_err(map_err)?;
    // A `t` with no memory row has nothing to restore (#488 dropped the
    // pre-#488 stub form with its table).
    let (locked_handle, kind, schema_id, sidecar_tables, language, forgotten) =
        row.ok_or(StorageError::NotFound)?;
    if locked_handle != handle || !reach.kinds.contains(&kind) {
        return Err(StorageError::NotFound);
    }
    if forgotten {
        sqlx::query(
            "UPDATE proxima_core.memory /* FORGET-POLICY: includes-forgotten (forget and restore see the row in either state) */
                SET forgotten_at = NULL
              WHERE t = $1 AND owner_id = $2",
        )
        .bind(t)
        .bind(reach.owner_id)
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
        for table in &sidecar_tables {
            sidecars
                .rebuild_projection_for_table(
                    tx,
                    proxima_core::MemoryId::new(t),
                    table,
                    &schema_id,
                    Some(language.as_str()),
                )
                .await?;
        }
        if !embeddings.spaces.is_empty() && !embeddings.non_embeddable_schemas.contains(&schema_id)
        {
            crate::verbs::fact_embeddings::enqueue_embedding_jobs_in_tx(
                tx,
                reach.owner_id,
                t,
                embeddings.spaces,
            )
            .await?;
        }
        sync_memory_head(tx, handle).await?;
    }
    Ok(())
}

/// Hard-delete one admission of an abandoned owner.
///
/// Returns an empty purge plan: a memory owes the object store nothing since
/// #488. The return type stays because [`erase_memory_series`] merges the
/// blob objects it owes into the same plan.
///
/// Runs in the lifecycle scope, so an owner-scope caller erases a forgotten
/// admission too, and leaves it before returning.
///
/// # Errors
///
/// Returns storage errors from the delete statements.
pub async fn erase_memory(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    context: &crate::PgHostStateEraseContext,
    owner: &OwnerId,
    t: Uuid,
) -> Result<ColdPurgePlan, StorageError> {
    let lifecycle = crate::lifecycle_scope::PgLifecycleScope::enter(tx.as_mut()).await?;
    let result = erase_memory_in_lifecycle(tx, sidecars, context, owner, t).await;
    lifecycle.finish(tx.as_mut(), result).await
}

#[allow(clippy::too_many_lines)] // the ordered hard-delete transaction keeps atomic DML sequencing visible
async fn erase_memory_in_lifecycle(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    context: &crate::PgHostStateEraseContext,
    owner: &OwnerId,
    t: Uuid,
) -> Result<ColdPurgePlan, StorageError> {
    context.lock_before_physical_erase(tx, owner).await?;
    let surfaces = &context.surfaces;
    // Probe the series identity first. The handle lock is the first
    // serialization boundary; a missing target is not a successful no-op.
    let probed_handle = probe_memory_handle_for_owner(tx, t, owner.into_inner())
        .await?
        .ok_or(StorageError::NotFound)?;
    lock_memory_handles_tx(tx, &[probed_handle]).await?;
    lock_lifecycle_targets_tx(tx, &[t]).await?;
    let handle = probe_memory_handle_for_owner(tx, t, owner.into_inner())
        .await?
        .ok_or(StorageError::NotFound)?;
    if handle != probed_handle {
        return Err(StorageError::Retryable(
            "memory target changed series while erasing".into(),
        ));
    }
    let content_id = sqlx::query_scalar::<_, Option<Uuid>>(
        "SELECT content_id FROM proxima_core.memory WHERE t = $1 AND owner_id = $2", /* FORGET-POLICY: includes-forgotten (erase removes forgotten rows too) */
    )
    .bind(t)
    .bind(owner.into_inner())
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?
    .flatten();
    let is_fact: bool = sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1 FROM proxima_core.memory WHERE t = $1 AND owner_id = $2 AND kind = 'fact' /* FORGET-POLICY: includes-forgotten (erase removes forgotten rows too) */
         )",
    )
    .bind(t)
    .bind(owner.into_inner())
    .fetch_one(tx.as_mut())
    .await
    .map_err(map_err)?;
    crate::verbs::owner_erase::invoke_host_lifecycle_exact_erase(
        tx,
        context,
        *owner,
        if is_fact {
            std::slice::from_ref(&t)
        } else {
            &[]
        },
    )
    .await?;
    let stamped: Vec<String> = sqlx::query_scalar(
        "SELECT unnest(sidecar_tables) FROM proxima_core.memory WHERE t = $1 AND owner_id = $2", /* FORGET-POLICY: includes-forgotten (erase removes forgotten rows too) */
    )
    .bind(t)
    .bind(owner.into_inner())
    .fetch_all(tx.as_mut())
    .await
    .map_err(map_err)?;
    // Before the memory row: a kept surface with no owner column
    // (memory_external_origin) finds its owner through its memory, so its
    // delete must run while that parent is still there.
    delete_memory_dependents(tx, sidecars, surfaces, &stamped, t).await?;
    sqlx::query("DELETE FROM proxima_core.memory WHERE t = $1 AND owner_id = $2") /* FORGET-POLICY: includes-forgotten (erase removes forgotten rows too) */
        .bind(t)
        .bind(owner.into_inner())
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
    if let Some(id) = content_id {
        super::content::gc_unreferenced_content(tx, id).await?;
    }
    sync_memory_head(tx, handle).await?;
    sqlx::query("DELETE FROM proxima_core.ingest_keys WHERE t = $1 AND owner_id = $2")
        .bind(t)
        .bind(owner.into_inner())
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
    // The captured publication, if this Fact had one. Hand-written for the
    // same reason as the receipt above: the row carries no foreign key into
    // `memory` (it must outlive a delete of the memory row), so the
    // generated memory-dependent legs do not reach it. Erasing a single
    // admission destroys its captured payload — an undelivered event is
    // still this owner's content, and compliance erasure is the one and
    // only way a record leaves this table.
    //
    // On `t` ALONE, deliberately, and this is the one delete in this
    // function without an `owner_id` predicate. `t` is the outbox's primary
    // key, so the predicate selects exactly this admission's record. Adding
    // `AND owner_id = $2` would look safer and would leak: the surface
    // declares `TransferRule::RetainAtSource`, so a transferred series
    // leaves its already-captured record under the SOURCE owner while the
    // memory row moves to the destination — and the destination erasing the
    // memory would then walk past a record still holding the full typed
    // payload. The caller has already authorized the erase of this `t`; the
    // record is that `t`'s content whichever owner it was captured for.
    sqlx::query("DELETE FROM proxima_core.publication_origin WHERE t = $1")
        .bind(t)
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
    sqlx::query("DELETE FROM proxima_core.publication_outbox WHERE t = $1")
        .bind(t)
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
    // A memory owes the object store nothing since #488 (forget keeps the
    // row); the plan type stays for the blob purges a series erase returns.
    Ok(ColdPurgePlan::default())
}

/// Hard-delete every version of every series the given admissions belong to.
///
/// The kernel's own erase, applied to a set. A flavor tearing down one of
/// its scopes — a repo, a workspace, a project — knows which of ITS rows
/// belong to that scope and nothing about the substrate rows behind them;
/// this is the other half, and it is deliberately flavor-agnostic so the
/// flavor never writes a `proxima_core` statement to get it.
///
/// Expansion to the whole series matters: a caller that collected ids from
/// its own tables, or matched only live versions, would erase some
/// versions and leave the rest of the series behind.
///
/// A cited blob goes when the last admission citing it does. That leg is
/// here rather than in [`erase_memory`] because it is a question about the
/// SET: erasing one version of a series says nothing about whether the
/// blob is still cited, and asking per-version would answer "still cited"
/// for every version but the last. `proxima_core.blob` is referenced by
/// `memory.blob_id` and `blob_uploads.blob_id` and by nothing else, so once the admissions are gone the reference question is
/// answerable in one statement, which is what `gc_unreferenced_blobs`
/// below is.
///
/// Goal references are not foreign keys. Erased evidence resolves as
/// unavailable; source-scoped erasure leaves the Goal row intact.
///
/// Returns the number of admissions erased and the objects the erase
/// still owes the object store; see [`erase_memory`] for why the caller
/// commits first.
///
/// # Errors
///
/// Returns storage errors from the delete statements.
pub async fn erase_memory_series(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    context: &crate::PgHostStateEraseContext,
    owner: &OwnerId,
    ts: &[Uuid],
) -> Result<(u64, ColdPurgePlan), StorageError> {
    if ts.is_empty() {
        return Ok((0, ColdPurgePlan::default()));
    }
    context.lock_before_physical_erase(tx, owner).await?;
    // The lifecycle scope: an owner-scope caller must reach forgotten
    // versions too, or they would outlive the series erase.
    let lifecycle = crate::lifecycle_scope::PgLifecycleScope::enter(tx.as_mut()).await?;
    let owner_id = owner.into_inner();
    // Capture handles and membership in one statement before waiting. The
    // membership is the generation witness: handles are reusable after a
    // complete erase, so a non-empty post-lock series disjoint from this
    // snapshot is a replacement, not a late version of the requested series.
    let result = match snapshot_series_for_erase_tx(tx, owner_id, ts).await {
        Ok(before) => {
            erase_memory_series_after_snapshot(tx, sidecars, context, owner, before).await
        }
        Err(err) => Err(err),
    };
    lifecycle.finish(tx.as_mut(), result).await
}

async fn erase_memory_series_after_snapshot(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    context: &crate::PgHostStateEraseContext,
    owner: &OwnerId,
    before: MemorySeriesSnapshot,
) -> Result<(u64, ColdPurgePlan), StorageError> {
    let owner_id = owner.into_inner();
    let handles = before.handles();
    lock_memory_handles_tx(tx, &handles).await?;
    let after = snapshot_series_for_handles_tx(tx, owner_id, &handles).await?;
    if generation_replaced(&before, &after) {
        return Err(StorageError::Retryable(
            "memory series was replaced while erase waited for its handle lock".into(),
        ));
    }
    let versions = after.versions();

    // Expansion includes every live and forgotten version before any cited blob
    // or row lock. Re-entering these locks in erase_memory is intentional: it
    // protects direct callers while preserving one global lock vocabulary.
    lock_lifecycle_targets_tx(tx, &versions).await?;

    // Captured BEFORE the loop, because the loop is what makes them
    // unreferenced: after it, no row is left to read `blob_id` from.
    let cited = cited_blobs_locked_for_erase(tx, owner, &versions).await?;

    let mut entries = Vec::new();
    let mut erased = 0_u64;
    for t in versions {
        let plan = erase_memory_in_lifecycle(tx, sidecars, context, owner, t).await?;
        entries.extend_from_slice(plan.entries());
        erased += 1;
    }
    entries.extend(gc_unreferenced_blobs(tx, owner_id, &cited).await?);
    Ok((erased, ColdPurgePlan::from_entries(entries)))
}

#[derive(Debug)]
struct MemorySeriesSnapshot {
    members: BTreeSet<(Uuid, Uuid)>,
}

impl MemorySeriesSnapshot {
    fn handles(&self) -> Vec<Uuid> {
        self.members
            .iter()
            .map(|(handle, _)| *handle)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    fn versions(&self) -> Vec<Uuid> {
        self.members
            .iter()
            .map(|(_, t)| *t)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
}

/// Did any handle in `after` turn over completely since `before` was taken?
///
/// Arguments are in chronological order. A handle is *witnessed* when at
/// least one of its `(handle, t)` members survived from `before`: the erase
/// is still looking at the same generation of that series, and the members
/// `before` did not have are late versions the expansion legitimately picks
/// up. A handle present in `after` with no surviving member was erased down
/// to nothing and re-admitted under the same handle, so `after` describes a
/// replacement series that this erase was never authorized to touch.
fn generation_replaced(before: &MemorySeriesSnapshot, after: &MemorySeriesSnapshot) -> bool {
    let witnessed = after
        .members
        .intersection(&before.members)
        .map(|(handle, _)| *handle)
        .collect::<BTreeSet<_>>();
    after
        .members
        .iter()
        .any(|(handle, _)| !witnessed.contains(handle))
}

/// Resolve every series and version named by target admissions in one
/// statement snapshot. Callers that mutate acquire `handles` in sorted order
/// and use the member pairs to distinguish late members from handle reuse.
async fn snapshot_series_for_erase_tx(
    tx: &mut Transaction<'_, Postgres>,
    owner_id: Uuid,
    ts: &[Uuid],
) -> Result<MemorySeriesSnapshot, StorageError> {
    let rows: Vec<(Uuid, Uuid)> = sqlx::query_as(SNAPSHOT_SERIES_FOR_ERASE_SQL)
        .bind(ts)
        .bind(owner_id)
        .fetch_all(tx.as_mut())
        .await
        .map_err(map_err)?;
    Ok(MemorySeriesSnapshot {
        members: rows.into_iter().collect(),
    })
}

const SNAPSHOT_SERIES_FOR_ERASE_SQL: &str = "\
WITH handles AS (
    SELECT DISTINCT handle FROM proxima_core.memory /* FORGET-POLICY: includes-forgotten (erase removes forgotten rows too) */
     WHERE t = ANY($1::uuid[]) AND owner_id = $2
), versions AS (
    SELECT m.handle, m.t FROM proxima_core.memory m /* FORGET-POLICY: includes-forgotten (erase removes forgotten rows too) */
      JOIN handles h ON h.handle = m.handle
     WHERE m.owner_id = $2
)
SELECT handle, t FROM versions ORDER BY handle, t";

async fn snapshot_series_for_handles_tx(
    tx: &mut Transaction<'_, Postgres>,
    owner_id: Uuid,
    handles: &[Uuid],
) -> Result<MemorySeriesSnapshot, StorageError> {
    if handles.is_empty() {
        return Ok(MemorySeriesSnapshot {
            members: BTreeSet::new(),
        });
    }
    let rows: Vec<(Uuid, Uuid)> = sqlx::query_as(SNAPSHOT_SERIES_BY_HANDLES_SQL)
        .bind(handles)
        .bind(owner_id)
        .fetch_all(tx.as_mut())
        .await
        .map_err(map_err)?;
    Ok(MemorySeriesSnapshot {
        members: rows.into_iter().collect(),
    })
}

const SNAPSHOT_SERIES_BY_HANDLES_SQL: &str = "\
SELECT m.handle, m.t FROM proxima_core.memory m /* FORGET-POLICY: includes-forgotten (erase removes forgotten rows too) */
  JOIN unnest($1::uuid[]) h(handle) ON h.handle = m.handle
 WHERE m.owner_id = $2
 ORDER BY handle, t";

/// Take a row lock on admissions a scope erase is about to delete.
///
/// Not an optimisation and not a formality, and flavor-agnostic for the
/// same reason [`erase_memory_series`] is: a flavor computing which of its
/// rows belong to a scope needs several statements to do it, `READ
/// COMMITTED` gives each of them its own snapshot, and a row committed in
/// any of the gaps references a memory that is about to go. With a
/// `NO ACTION` foreign key that is not a leak — it is an abort, after all
/// the work.
///
/// `FOR UPDATE` on the referenced `memory` rows closes the gaps for real.
/// Inserting a row with a foreign key takes `FOR KEY SHARE` on the row it
/// references, and `FOR UPDATE` conflicts with `FOR KEY SHARE`, so a
/// concurrent writer that would create a dangling pointer blocks until this
/// transaction commits and then fails its own foreign key — in its
/// transaction rather than in the erase.
///
/// CALL IT ONCE, OVER THE WHOLE SET. The locks do last until the
/// transaction ends, so calling it per round of a fixpoint "works" — and
/// puts an arbitrarily long window between the rounds in which this
/// transaction holds some of the rows and is not yet asking for the rest.
/// A writer holding `FOR KEY SHARE` on a later row and waiting on an
/// earlier one turns that window into a deadlock, and `PostgreSQL` picks a
/// victim by which transaction is cheapest to abort, which is the one that
/// has not yet done its deletes: the erase. Computing the full set first
/// and locking it in one statement shrinks that window to the inside of
/// this statement.
///
/// The series handles are acquired before this lifecycle set. `ORDER BY t` is
/// the other half: two erases whose sets overlap then take the shared rows in
/// the same order and one waits instead of both dying.
/// It does not help against a writer, which takes its own locks in its own
/// order — nothing does, which is why a caller must also be prepared to
/// retry a `40P01`.
///
/// Handle resolution and the post-lock presence check are both scoped to
/// `owner`. If transfer wins after the caller's earlier ownership check, the
/// stale erase returns [`StorageError::Retryable`] without serializing the
/// destination owner's series behind its transaction lifetime.
///
/// # Errors
///
/// Returns storage errors from the lock statement.
pub(crate) async fn lock_admissions_for_erase(
    tx: &mut Transaction<'_, Postgres>,
    owner: &OwnerId,
    ts: &[Uuid],
) -> Result<(), StorageError> {
    if ts.is_empty() {
        return Ok(());
    }
    // This is the mandatory seam before any admission `t` or row lock. The
    // caller has computed its complete footprint but has not started locking
    // it yet; acquire every named series handle here so a later series erase
    // only re-enters handles this transaction already owns.
    let handles: Vec<Uuid> = sqlx::query_scalar(
        "SELECT DISTINCT handle FROM proxima_core.memory /* FORGET-POLICY: includes-forgotten (erase removes forgotten rows too) */
          WHERE t = ANY($1::uuid[]) AND owner_id = $2
          ORDER BY handle",
    )
    .bind(ts)
    .bind(owner.into_inner())
    .fetch_all(tx.as_mut())
    .await
    .map_err(map_err)?;
    lock_memory_handles_tx(tx, &handles).await?;

    let present: Vec<Uuid> = sqlx::query_scalar(
        "SELECT t FROM proxima_core.memory /* FORGET-POLICY: includes-forgotten (erase removes forgotten rows too) */
          WHERE t = ANY($1::uuid[]) AND owner_id = $2
          ORDER BY t",
    )
    .bind(ts)
    .bind(owner.into_inner())
    .fetch_all(tx.as_mut())
    .await
    .map_err(map_err)?;
    let mut expected = ts.to_vec();
    expected.sort_unstable();
    expected.dedup();
    if present != expected {
        return Err(StorageError::Retryable(
            "erase admission footprint changed before lifecycle lock acquisition".into(),
        ));
    }
    lock_lifecycle_targets_tx(tx, ts).await?;
    sqlx::query(LOCK_ADMISSIONS_SQL)
        .bind(ts)
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
    Ok(())
}

const LOCK_ADMISSIONS_SQL: &str =
    "SELECT t FROM proxima_core.memory WHERE t = ANY($1::uuid[]) ORDER BY t FOR UPDATE"; /* FORGET-POLICY: includes-forgotten (erase removes forgotten rows too) */

/// The blobs the given admissions cite, held.
///
/// Capture and lock in ONE statement, because doing one without the other
/// is never right and a seam between them is a line someone can delete.
/// `gc_unreferenced_blobs` decides "no admission cites this blob" and then
/// deletes it; `memory.blob_id` is `NO ACTION`, so a citation committed
/// between the deciding and the deleting makes the delete wait on the
/// citer's `FOR KEY SHARE`, and then raise `23503` and take the whole erase
/// down with it. `FOR UPDATE` taken here — before the erase loop, while the
/// citing rows are all still there — is what turns that into the citer
/// waiting on us and then failing its own foreign key in its own
/// transaction.
///
/// `FOR UPDATE` and not `FOR NO KEY UPDATE`. `FOR NO KEY UPDATE` is the
/// weaker mode that PERMITS concurrent key-share holders — exactly the
/// writer this has to exclude — so it would leave the race untouched while
/// looking like a fix. Measured on `PostgreSQL` 18, not assumed: against a
/// `FOR NO KEY UPDATE` holder a second session takes `FOR KEY SHARE` on the
/// same row immediately; against a `FOR UPDATE` holder it blocks until the
/// holder's transaction ends.
///
/// Locking every cited blob rather than only the orphans over-locks by
/// design: which of them are orphans is not known until the erase loop has
/// run, and a lock taken after the question is answered is a lock taken too
/// late. They are this owner's blobs and the transaction is short.
///
/// # Errors
///
/// Returns storage errors from the statement.
pub async fn cited_blobs_locked_for_erase(
    tx: &mut Transaction<'_, Postgres>,
    owner: &OwnerId,
    ts: &[Uuid],
) -> Result<Vec<Uuid>, StorageError> {
    if ts.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_scalar(CITED_BLOBS_LOCKED_SQL)
        .bind(ts)
        .bind(owner.into_inner())
        .fetch_all(tx.as_mut())
        .await
        .map_err(map_err)
}

/// The blobs the admissions about to be erased cite, locked against a
/// concurrent citation.
///
/// The `FOR UPDATE` binds to `b` — the one plain table in the outer
/// `FROM` — which is why the two-legged citation question lives in a CTE
/// rather than in a `UNION` the locking clause could not attach to.
const CITED_BLOBS_LOCKED_SQL: &str = "\
WITH cited AS (
    SELECT m.blob_id FROM proxima_core.memory m /* FORGET-POLICY: includes-forgotten (erase removes forgotten rows too) */
     WHERE m.t = ANY($1::uuid[]) AND m.owner_id = $2 AND m.blob_id IS NOT NULL
)
SELECT b.blob_id FROM proxima_core.blob b
 WHERE b.blob_id IN (SELECT blob_id FROM cited)
   AND b.owner_id = $2
 ORDER BY b.blob_id
   FOR UPDATE";

/// Delete each of `cited` that no admission still cites, with its upload
/// rows, and enqueue the objects that no other upload row names.
///
/// `gc_unreferenced_content`'s idiom, at the blob level, plus the refcount
/// a shared object needs when several upload rows name it.
/// `proxima_core.blob` is
/// referenced by exactly two columns, `memory.blob_id` and
/// `blob_uploads.blob_id`, both `NO ACTION` — so "no
/// admission cites it" is the whole reference question. Citation schemas
/// are opaque and have no sidecar table, which is why
/// `OwnerSurfaces::for_registry` finds no cited-object family for
/// core or code to consult.
///
/// The three deletes and the enqueue are one statement so they share one
/// snapshot. That is what makes the object anti-join ask the right
/// question: "does a row OUTSIDE this orphan set name the key", evaluated
/// against the rows as they stood before any of them went, exactly as the
/// owner-erase arm evaluates it.
///
/// One snapshot is not one lock, though: the reference question is asked of
/// this transaction's snapshot and the delete is enforced against the
/// latest one, so a citation committed in between still aborts it. Every
/// candidate is held under `FOR UPDATE` from before the erase loop for
/// that; see [`cited_blobs_locked_for_erase`].
///
/// `b.owner_id = $1` is the Lean citation invariant restated
/// (`memory_cites m b -> memory_owner m = blob_owner b`). It costs nothing
/// when the invariant holds and, if it ever did not, leaves the row rather
/// than deleting another owner's blob.
async fn gc_unreferenced_blobs(
    tx: &mut Transaction<'_, Postgres>,
    owner_id: Uuid,
    cited: &[Uuid],
) -> Result<Vec<ColdPurgeEntry>, StorageError> {
    if cited.is_empty() {
        return Ok(Vec::new());
    }
    // One snapshot is still not one lock for the OBJECT question. The
    // anti-join below asks "does a row outside this orphan set name the key",
    // and another owner's erase asking the mirror-image question concurrently
    // would have both withhold and leak the bytes. The object-key fence is
    // what serializes the two; `FOR UPDATE` on `blob` cannot, because the
    // rows racing here belong to a different owner.
    let object_keys = crate::access::owner_columns::blob_upload_object_keys(tx, cited).await?;
    crate::access::owner_columns::lock_object_keys_tx(tx, &object_keys).await?;
    sqlx::query_as::<_, (String, String)>(GC_UNREFERENCED_BLOBS_SQL)
        .bind(cited)
        .bind(owner_id)
        .fetch_all(tx.as_mut())
        .await
        .map_err(map_err)
        .map(|rows| {
            rows.into_iter()
                .map(|(object_key, object_store)| ColdPurgeEntry {
                    object_key,
                    object_store,
                })
                .collect()
        })
}

const GC_UNREFERENCED_BLOBS_SQL: &str = concat!(
    "\
WITH orphans AS MATERIALIZED (
    SELECT b.blob_id
      FROM proxima_core.blob b
     WHERE b.blob_id = ANY($1::uuid[])
       AND b.owner_id = $2
       AND NOT EXISTS (
               SELECT 1 FROM proxima_core.memory m WHERE m.blob_id = b.blob_id /* FORGET-POLICY: includes-forgotten (a forgotten memory still cites its content) */
           )
),
enqueued AS (
    INSERT INTO proxima_core.cold_purge_pending (object_key, owner_id, object_store)
    SELECT DISTINCT ON (u.object_store, k.object_key) k.object_key, u.owner_id, u.object_store
      FROM proxima_core.blob_uploads u
      JOIN orphans o ON o.blob_id = u.blob_id ",
    upload_implied_keys_sql!(),
    "
     WHERE k.object_key IS NOT NULL
       AND NOT EXISTS (
               SELECT 1
                 FROM proxima_core.blob_uploads other
                WHERE other.upload_id <> u.upload_id
                  AND NOT EXISTS (
                          SELECT 1 FROM orphans o2 WHERE o2.blob_id = other.blob_id
                      )
                  AND ",
    other_upload_implies_key_sql!(),
    "
           )
     ORDER BY u.object_store, k.object_key, u.upload_id
    ON CONFLICT (object_store, object_key) DO UPDATE
       SET enqueued_at = now(), next_attempt_at = now()
    RETURNING object_key, object_store
),
d_uploads AS (
    DELETE FROM proxima_core.blob_uploads u
     USING orphans o
     WHERE u.blob_id = o.blob_id
),
d_blobs AS (
    DELETE FROM proxima_core.blob b
     USING orphans o
     WHERE b.blob_id = o.blob_id
)
-- `d_uploads` and `d_blobs` are read by nothing on purpose: a
-- data-modifying WITH clause runs exactly once and to completion whether
-- or not the primary query reads its output.
SELECT object_key, object_store FROM enqueued"
);

// The forget/erase/hydrate suite. Crate-internal because it works at this
// layer deliberately: it stamps `sidecar_tables` lists by hand — including a
// registered table that does not exist in Postgres — to prove forget reads
// the stamp rather than the registry. The governed port would refuse those
// drafts, which is exactly why the test cannot be written above it.
#[cfg(test)]
#[path = "forget_pg_tests.rs"]
pub(crate) mod forget_pg_tests;

#[cfg(test)]
#[path = "forget_marks_pg_tests.rs"]
mod forget_marks_pg_tests;

#[cfg(test)]
mod tests {
    use super::{ForgetLeg, OwnerSurfaces, forget_leg_sql};

    fn shipped() -> OwnerSurfaces {
        OwnerSurfaces::for_registry(
            &proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests(),
        )
    }

    /// The declared walk reaches exactly the three embedding tables and
    /// nothing else, with the embedding queue first (see
    /// `generated_forget_legs`). The sketch is not one of them since #488
    /// S14: it is the memory's own text, which a restore needs back.
    ///
    /// The key column is pinned too: all three are keyed on `entity_id`,
    /// not `t`. A walk that assumed `t` because the sidecar half does would
    /// delete nothing from any of them, and both lanes share
    /// `forget_leg_sql` precisely so that assumption cannot be made in one
    /// place and not the other.
    #[test]
    fn the_declared_forget_walk_is_exactly_the_derived_row_tables() {
        assert_eq!(
            shipped().generated_forget_legs(),
            vec![
                ("proxima_core.embedding_jobs", "entity_id"),
                ("proxima_core.embedding_heads", "entity_id"),
                ("proxima_core.embeddings", "entity_id"),
            ]
        );
    }

    /// No surface resolves to `Unreachable`, and every leg this crate must
    /// execute is one it can build. The forget counterpart of
    /// `owner_erase`'s `every_declared_surface_has_a_leg_this_crate_can_run`.
    #[test]
    fn every_declared_surface_has_a_forget_leg_this_crate_can_run() {
        let surfaces = shipped();
        assert!(surfaces.surfaces().len() > 20);
        for surface in surfaces.surfaces() {
            match surfaces.forget_leg(surface.table) {
                ForgetLeg::Unreachable => panic!(
                    "{} resolves to Unreachable, which freeze should have refused",
                    surface.table
                ),
                ForgetLeg::Deleted { key_column } => {
                    forget_leg_sql(surface.table, key_column).unwrap_or_else(|e| {
                        panic!("{} is a leg this crate cannot build: {e}", surface.table)
                    });
                }
                ForgetLeg::Marked | ForgetLeg::Cascaded { .. } | ForgetLeg::Kept { .. } => {}
            }
        }
    }

    /// The statement, pinned. One shape serves both iteration sources.
    #[test]
    fn a_forget_leg_deletes_the_declared_key_column() {
        assert_eq!(
            forget_leg_sql("proxima_core.embeddings", "entity_id").expect("generates"),
            "DELETE FROM proxima_core.embeddings WHERE entity_id = $1"
        );
        assert!(
            forget_leg_sql("proxima_core.embeddings", "entity_id; DROP TABLE x").is_err(),
            "the identifier whitelist is what makes this %I-equivalent"
        );
    }
}

#[cfg(test)]
mod erase_kept_partition_tests {
    use super::{ERASE_MEMORY_EXPLICIT, erase_kept_memory_legs};
    use proxima_core::flavor::{EraseLeg, ForgetLeg, KeyShape};
    use proxima_core::owner_inverse::OwnerSurfaces;

    /// The hard erase of one admission reaches every memory-keyed surface a
    /// forget keeps: either by a named statement of its own or by the
    /// generated set, never both and never neither.
    #[test]
    fn explicit_and_generated_sets_partition_every_kept_memory_keyed_surface() {
        let surfaces = OwnerSurfaces::for_registry(
            &proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests(),
        );
        let generated: Vec<&str> = erase_kept_memory_legs(&surfaces)
            .into_iter()
            .map(|(table, _)| table)
            .collect();
        let kept_keyed: Vec<&str> = surfaces
            .surfaces()
            .iter()
            .filter(|surface| {
                matches!(
                    surfaces.erase_leg(surface.table),
                    EraseLeg::Keyed(KeyShape::MemoryT { .. })
                ) && matches!(surfaces.forget_leg(surface.table), ForgetLeg::Kept { .. })
            })
            .map(|surface| surface.table)
            .collect();
        for table in &kept_keyed {
            let explicit = ERASE_MEMORY_EXPLICIT.contains(table);
            assert!(
                explicit != generated.contains(table),
                "{table} must be reached by exactly one of the explicit and generated sets"
            );
        }
        for table in &generated {
            assert!(
                kept_keyed.contains(table),
                "{table} is not a kept memory-keyed surface"
            );
        }
        assert!(
            generated.contains(&"proxima_core.memory_external_origin"),
            "the other-store origins are reached by the generated set: {generated:?}"
        );
    }
}
