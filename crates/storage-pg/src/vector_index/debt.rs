//! What a store owes its host index after a head change, and the sweep that
//! compares the index with the heads (#519).
//!
//! A trigger on `embedding_heads` writes the debt in the change's own
//! transaction. The drain pays it each pass; an erase pays its owner's before
//! it commits. The sweep repairs what the debt cannot reach: an entry no head
//! names, an entry under the wrong owner, a head the index lost.

use std::collections::BTreeMap;

use proxima_core::storage_ports::{VectorEntry, VectorIndex, VectorKey};
use proxima_core::{EmbeddingDim, EmbeddingSpace, OwnerId, StorageError};
use sqlx::PgConnection;
use uuid::Uuid;

use super::StoreVectors;
use crate::error::map_err;
use crate::verbs::fact_embeddings::try_lock_embedding_key;

/// Keys per index call, so a call's body stays small however many heads one
/// transaction changed.
const KEYS_PER_CALL: usize = 500;

/// One row of index debt, parsed where it leaves the database.
struct Debt {
    key: VectorKey,
    owner: OwnerId,
    op: DebtOp,
}

/// What a debt row asks of the index; the column's CHECK names the same
/// two.
enum DebtOp {
    Delete,
    Owner,
}

type DebtRow = (Uuid, String, i16, Uuid, String);

fn space_of(model_id: String, dim: i16) -> Result<EmbeddingSpace, StorageError> {
    let dim = usize::try_from(dim)
        .ok()
        .and_then(EmbeddingDim::from_width)
        .ok_or_else(|| {
            StorageError::ConstraintViolation(format!("index debt names width {dim}"))
        })?;
    Ok(EmbeddingSpace::new(model_id, dim))
}

fn parse_debt((entity_id, model_id, dim, owner, op): DebtRow) -> Result<Debt, StorageError> {
    let op = match op.as_str() {
        "delete" => DebtOp::Delete,
        "owner" => DebtOp::Owner,
        other => {
            return Err(StorageError::ConstraintViolation(format!(
                "index debt names operation {other:?}"
            )));
        }
    };
    Ok(Debt {
        key: VectorKey {
            entity_id,
            space: space_of(model_id, dim)?,
        },
        owner: OwnerId::new(owner),
        op,
    })
}

async fn delete_keys(index: &dyn VectorIndex, keys: &[VectorKey]) -> Result<(), StorageError> {
    for keys in keys.chunks(KEYS_PER_CALL) {
        index.delete(keys).await?;
    }
    Ok(())
}

async fn move_keys(
    index: &dyn VectorIndex,
    moves: BTreeMap<OwnerId, Vec<VectorKey>>,
) -> Result<(), StorageError> {
    for (owner, keys) in moves {
        for keys in keys.chunks(KEYS_PER_CALL) {
            index.set_owner(keys, owner).await?;
        }
    }
    Ok(())
}

/// Apply debt to the host index: the deleted keys, then each new owner's.
async fn apply(index: &dyn VectorIndex, rows: Vec<DebtRow>) -> Result<(), StorageError> {
    let mut deletes = Vec::new();
    let mut moves: BTreeMap<OwnerId, Vec<VectorKey>> = BTreeMap::new();
    for row in rows {
        let debt = parse_debt(row)?;
        match debt.op {
            DebtOp::Delete => deletes.push(debt.key),
            DebtOp::Owner => moves.entry(debt.owner).or_default().push(debt.key),
        }
    }
    delete_keys(index, &deletes).await?;
    move_keys(index, moves).await
}

/// Pay, before an erase of `owner` commits, the debt its transaction left
/// and every older debt naming `owner`: an erase reports done only once the
/// index has deleted the vectors, those a forget left for the next drain
/// included. An index failure fails the erase and its transaction rolls
/// back; what the index already deleted stays deleted, and the sweep
/// repairs it. A store that keeps its vectors locally deleted them in the
/// transaction already and only clears the rows.
pub(crate) async fn settle_owner_debt(
    tx: &mut PgConnection,
    vectors: &StoreVectors,
    owner: OwnerId,
) -> Result<(), StorageError> {
    let rows: Vec<DebtRow> = sqlx::query_as(
        "DELETE FROM proxima_core.vector_index_debt
          WHERE xact = pg_current_xact_id() OR owner_id = $1
         RETURNING entity_id, model_id, dim, owner_id, op",
    )
    .bind(owner.into_inner())
    .fetch_all(&mut *tx)
    .await
    .map_err(map_err)?;
    match vectors {
        StoreVectors::Local => Ok(()),
        StoreVectors::Remote(index) => apply(index.as_ref(), rows).await,
    }
}

/// Pay up to `limit` rows of index debt on a platform transaction the
/// caller commits. Each row is its key's newest change, so any order pays
/// the same; a row another drain holds is skipped. Returns how many rows
/// were paid.
pub(crate) async fn drain_debt(
    tx: &mut PgConnection,
    vectors: &StoreVectors,
    limit: i64,
) -> Result<u64, StorageError> {
    let rows: Vec<DebtRow> = sqlx::query_as(
        "DELETE FROM proxima_core.vector_index_debt debt
          WHERE (debt.entity_id, debt.model_id, debt.dim) IN (
                SELECT entity_id, model_id, dim FROM proxima_core.vector_index_debt
                 LIMIT $1
                 FOR UPDATE SKIP LOCKED)
         RETURNING entity_id, model_id, dim, owner_id, op",
    )
    .bind(limit)
    .fetch_all(&mut *tx)
    .await
    .map_err(map_err)?;
    let paid = u64::try_from(rows.len()).unwrap_or(u64::MAX);
    match vectors {
        StoreVectors::Local => {}
        StoreVectors::Remote(index) => apply(index.as_ref(), rows).await?,
    }
    Ok(paid)
}

/// Index entries, and heads, compared per page by the sweep.
const SWEEP_PAGE: u32 = 500;

/// A key in the order both sides page in: entity, model id bytewise, dim as the column holds it.
type Key = (Uuid, String, i16);

/// What one head holds: its owner and version.
type Head = (OwnerId, i32);

/// The sweep key of an index entry's key; a width no column holds is a constraint violation.
fn key_of(key: &VectorKey) -> Result<Key, StorageError> {
    let (entity, model, width) = key.order();
    let dim = i16::try_from(width).map_err(|_| {
        StorageError::ConstraintViolation(format!("index entry names width {width}"))
    })?;
    Ok((entity, model.to_owned(), dim))
}

fn vector_key((entity_id, model_id, dim): &Key) -> Result<VectorKey, StorageError> {
    Ok(VectorKey {
        entity_id: *entity_id,
        space: space_of(model_id.clone(), *dim)?,
    })
}

/// What [`sweep_index`] repaired.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IndexSweep {
    /// Entries the index held for no head.
    pub(crate) entries_deleted: u64,
    /// Heads the index held no matching entry for: their vectors were lost,
    /// so reconciliation embeds them again.
    pub(crate) heads_deleted: u64,
}

/// Up to [`SWEEP_PAGE`] heads after `after`, in key order.
async fn heads_after(
    tx: &mut PgConnection,
    after: Option<&Key>,
) -> Result<Vec<(Key, Head)>, StorageError> {
    let rows: Vec<(Uuid, String, i16, Uuid, i32)> = sqlx::query_as(
        "SELECT entity_id, model_id, dim, owner_id, embedding_version
           FROM proxima_core.embedding_heads
          WHERE $1::uuid IS NULL
             OR (entity_id, model_id COLLATE \"C\", dim) > ($1, $2::text COLLATE \"C\", $3)
          ORDER BY entity_id, model_id COLLATE \"C\", dim
          LIMIT $4",
    )
    .bind(after.map(|key| key.0))
    .bind(after.map(|key| key.1.clone()))
    .bind(after.map(|key| key.2))
    .bind(i64::from(SWEEP_PAGE))
    .fetch_all(&mut *tx)
    .await
    .map_err(map_err)?;
    Ok(rows
        .into_iter()
        .map(|(entity, model, dim, owner, version)| {
            ((entity, model, dim), (OwnerId::new(owner), version))
        })
        .collect())
}

/// The keys where the index and the heads disagree, by what each side held.
fn disagreements(
    entries: &[(Key, VectorEntry)],
    heads: &BTreeMap<Key, Head>,
) -> BTreeMap<Key, (Option<Head>, Option<Head>)> {
    let mut seen: BTreeMap<Key, (Option<Head>, Option<Head>)> = BTreeMap::new();
    for (key, entry) in entries {
        seen.entry(key.clone()).or_default().0 = Some((entry.owner, entry.version));
    }
    for (key, head) in heads {
        seen.entry(key.clone()).or_default().1 = Some(*head);
    }
    seen.retain(|_, (entry, head)| entry != head);
    seen
}

/// Keys repaired per transaction, each holding its head lock until commit.
const REPAIRS_PER_TRANSACTION: usize = 50;

/// What the index and the heads held for one key.
type Suspect = (Key, (Option<Head>, Option<Head>));

/// Repair `suspects` under each key's head lock, acting only where the head
/// is still what the page read. A key a drain is writing is skipped, as is
/// one whose head changed since: the next sweep compares it again.
async fn repair(
    tx: &mut PgConnection,
    index: &dyn VectorIndex,
    suspects: &[Suspect],
    swept: &mut IndexSweep,
) -> Result<(), StorageError> {
    let mut stale = Vec::new();
    let mut moves: BTreeMap<OwnerId, Vec<VectorKey>> = BTreeMap::new();
    let mut lost: Vec<(Key, i32)> = Vec::new();
    for (key, (entry, read)) in suspects.iter().cloned() {
        if !try_lock_embedding_key(&mut *tx, key.0, &key.1, key.2).await? {
            continue;
        }
        let now: Option<(Uuid, i32)> = sqlx::query_as(
            "SELECT owner_id, embedding_version FROM proxima_core.embedding_heads
              WHERE entity_id = $1 AND model_id = $2 AND dim = $3
                FOR UPDATE",
        )
        .bind(key.0)
        .bind(&key.1)
        .bind(key.2)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_err)?;
        if now.map(|(owner, version)| (OwnerId::new(owner), version)) != read {
            continue;
        }
        match (entry, read) {
            (Some(_), None) => stale.push(vector_key(&key)?),
            (Some((_, held)), Some((owner, version))) if held == version => {
                moves.entry(owner).or_default().push(vector_key(&key)?);
            }
            (_, Some((_, version))) => lost.push((key, version)),
            (None, None) => {}
        }
    }
    delete_keys(index, &stale).await?;
    swept.entries_deleted += u64::try_from(stale.len()).unwrap_or(u64::MAX);
    move_keys(index, moves).await?;
    if !lost.is_empty() {
        let mut columns = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for ((entity, model, dim), version) in lost {
            columns.0.push(entity);
            columns.1.push(model);
            columns.2.push(dim);
            columns.3.push(version);
        }
        swept.heads_deleted += sqlx::query(
            "DELETE FROM proxima_core.embedding_heads head
              USING unnest($1::uuid[], $2::text[], $3::smallint[], $4::int[])
                    AS lost(entity_id, model_id, dim, version)
              WHERE head.entity_id = lost.entity_id
                AND head.model_id = lost.model_id
                AND head.dim = lost.dim
                AND head.embedding_version = lost.version",
        )
        .bind(&columns.0)
        .bind(&columns.1)
        .bind(&columns.2)
        .bind(&columns.3)
        .execute(&mut *tx)
        .await
        .map_err(map_err)?
        .rows_affected();
    }
    Ok(())
}

/// Compare the host index with the heads, a page of each at a time in key
/// order, and repair what differs: an entry with no head is deleted, an
/// entry under another owner moves to the head's owner, and a head with no
/// entry at its version is deleted so reconciliation embeds it again. The
/// heads are read before the entries, and each repair takes the drain's
/// lock and rechecks its head, so a concurrent drain is never undone; the
/// store reads every candidate under its owner rules either way.
pub(crate) async fn sweep_index(
    pool: &sqlx::PgPool,
    platform: &crate::PgPlatformScope,
    index: &dyn VectorIndex,
) -> Result<IndexSweep, StorageError> {
    let mut swept = IndexSweep::default();
    let mut after: Option<Key> = None;
    let page = usize::try_from(SWEEP_PAGE).unwrap_or(usize::MAX);
    loop {
        let mut tx = crate::platform_scope::begin_platform_transaction(pool, platform).await?;
        let heads = heads_after(tx.as_mut(), after.as_ref()).await?;
        tx.commit().await.map_err(map_err)?;
        let after_key = after.as_ref().map(vector_key).transpose()?;
        let entries = index
            .entries(after_key.as_ref(), SWEEP_PAGE)
            .await?
            .into_iter()
            .map(|entry| Ok((key_of(&entry.key)?, entry)))
            .collect::<Result<Vec<(Key, VectorEntry)>, StorageError>>()?;
        // Both sides are complete up to the smaller last key of a full page.
        let cutoff = [
            (entries.len() >= page)
                .then(|| entries.last().map(|(key, _)| key.clone()))
                .flatten(),
            (heads.len() >= page)
                .then(|| heads.last().map(|(key, _)| key.clone()))
                .flatten(),
        ]
        .into_iter()
        .flatten()
        .min();
        let within = |key: &Key| cutoff.as_ref().is_none_or(|cutoff| key <= cutoff);
        let entries: Vec<(Key, VectorEntry)> =
            entries.into_iter().filter(|(key, _)| within(key)).collect();
        let heads: BTreeMap<Key, Head> = heads.into_iter().filter(|(key, _)| within(key)).collect();
        let suspects: Vec<Suspect> = disagreements(&entries, &heads).into_iter().collect();
        for suspects in suspects.chunks(REPAIRS_PER_TRANSACTION) {
            let mut tx = crate::platform_scope::begin_platform_transaction(pool, platform).await?;
            repair(tx.as_mut(), index, suspects, &mut swept).await?;
            tx.commit().await.map_err(map_err)?;
        }
        match cutoff {
            Some(cutoff) => after = Some(cutoff),
            None => return Ok(swept),
        }
    }
}

#[cfg(test)]
mod tests;
