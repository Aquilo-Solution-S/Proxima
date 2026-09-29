//! Flavor-scoped series erase: the storage half of
//! `UnitOfWork::erase_own_series` (docs/13 §Flavor-scoped erase).
//!
//! The engine has authorized the owner and resolved the named flavor's
//! memory schemas. This module turns the selection into a closed footprint —
//! every version of every selected series, plus every series whose rows
//! reference the footprint through a foreign key — refuses the footprint when
//! any of it lies outside the owner, the schemas or the cap, locks it, and
//! erases it through [`erase_memory_series`]. Nothing is deleted before the
//! last refusal has been asked.
//!
//! It runs on the transaction `PgStorage::begin_series_erase` opened: the
//! exclusive lifecycle fence and the owner fence are already held, and they
//! were the transaction's first locks.

use std::collections::{BTreeMap, BTreeSet};

use proxima_core::flavor::KeyShape;
use proxima_core::owner_inverse::OwnerSurfaces;
use proxima_core::verbs::own_erase::{
    SeriesEraseOutcome, SeriesEraseRefusal, SeriesEraseRefusalKind, SeriesEraseReport,
    SeriesEraseRequest, SeriesSelection,
};
use proxima_core::{MemoryId, Owner, StorageError};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use super::forget::{
    ColdPurgePlan, cited_blobs_locked_for_erase, erase_memory_series, lock_admissions_for_erase,
};
use crate::error::map_err;
use crate::pg_ident::PgIdent;
use crate::sidecars::PgSidecarRegistryFrozen;

/// One foreign key into `proxima_core.memory (t)` that makes a row depend on
/// a memory OTHER than its own.
///
/// A row keyed on the memory it references is that memory's own dependent
/// and goes with it; only a column that differs from the row's key can
/// point from one series into another.
#[derive(Debug, Clone)]
struct ReferencePair {
    table: String,
    column: String,
    /// The table's own memory key, from its declared `KeyShape::MemoryT`.
    /// `None`: the row belongs to no series the erase could join.
    key: Option<&'static str>,
}

/// Every `NO ACTION` / `RESTRICT` single-column foreign key into
/// `proxima_core.memory (t)`, from the catalog rather than a list: a flavor
/// adding a referencing column is covered the day its migration runs.
/// `CASCADE` and `SET NULL` keys are the database's to resolve.
const REFERENCE_KEYS_SQL: &str = "\
SELECT n.nspname || '.' || cl.relname AS tbl, a.attname::text AS col
  FROM pg_catalog.pg_constraint c
  JOIN pg_catalog.pg_class cl ON cl.oid = c.conrelid
  JOIN pg_catalog.pg_namespace n ON n.oid = cl.relnamespace
  JOIN pg_catalog.pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = c.conkey[1]
 WHERE c.contype = 'f'
   AND c.confrelid = 'proxima_core.memory'::regclass
   AND cardinality(c.conkey) = 1
   AND c.confdeltype IN ('a', 'r')
 ORDER BY 1, 2";

async fn reference_pairs(
    tx: &mut Transaction<'_, Postgres>,
    surfaces: &OwnerSurfaces,
) -> Result<Vec<ReferencePair>, StorageError> {
    let keys: Vec<(String, String)> = sqlx::query_as(REFERENCE_KEYS_SQL)
        .fetch_all(tx.as_mut())
        .await
        .map_err(map_err)?;
    Ok(keys
        .into_iter()
        .filter_map(|(table, column)| {
            let key = surfaces.surfaces().iter().find_map(|surface| {
                match (surface.table == table, surface.key) {
                    (true, KeyShape::MemoryT { column }) => Some(column),
                    _ => None,
                }
            });
            (key != Some(column.as_str())).then_some(ReferencePair { table, column, key })
        })
        .collect())
}

/// One version the footprint holds.
#[derive(Debug, Clone)]
struct Version {
    handle: Uuid,
    owner_id: Uuid,
    /// The series' schema: `memory.schema_id` for a hot version, the
    /// content's for a cooled one. `None` when a cooled version names no
    /// content, which no schema check can pass.
    schema_id: Option<String>,
}

/// Every version of every series named by `ts`, any owner, with its schema.
const VERSIONS_OF_SQL: &str = "\
WITH handles AS (
    SELECT handle FROM proxima_core.memory WHERE t = ANY($1::uuid[])
    UNION
    SELECT handle FROM proxima_core.cooled WHERE t = ANY($1::uuid[])
)
SELECT m.t, m.handle, m.owner_id, m.schema_id
  FROM proxima_core.memory m JOIN handles h ON h.handle = m.handle
UNION ALL
SELECT c.t, c.handle, c.owner_id, k.schema_id
  FROM proxima_core.cooled c JOIN handles h ON h.handle = c.handle
  LEFT JOIN proxima_core.content k ON k.content_id = c.content_id";

/// The footprint: a closed set of whole series.
#[derive(Debug, Default)]
struct Footprint {
    versions: BTreeMap<Uuid, Version>,
    /// Series first reached through a referencing row, and the key that
    /// reached them, so a refusal names something an operator can look at.
    joined_via: BTreeMap<Uuid, String>,
    /// Rows of tables with no memory key that reference the footprint.
    unerasable: Vec<String>,
}

impl Footprint {
    fn handles(&self) -> BTreeSet<Uuid> {
        self.versions
            .values()
            .map(|version| version.handle)
            .collect()
    }

    fn ts(&self) -> Vec<Uuid> {
        self.versions.keys().copied().collect()
    }

    /// Every key is the handle of a version inserted with it.
    fn referencing_series(&self) -> usize {
        self.joined_via.len()
    }
}

/// Close `seeds` over series membership and foreign-key references.
///
/// A joint fixpoint: a referencing row's series is expanded whole, and its
/// other versions may be referenced in turn. Terminates because the version
/// set only grows and is bounded by the table.
async fn close_footprint(
    tx: &mut Transaction<'_, Postgres>,
    pairs: &[ReferencePair],
    seeds: &[Uuid],
) -> Result<Footprint, StorageError> {
    let mut footprint = Footprint::default();
    let mut frontier: Vec<(Uuid, Option<String>)> = seeds.iter().map(|t| (*t, None)).collect();
    while !frontier.is_empty() {
        let ts: Vec<Uuid> = frontier.iter().map(|(t, _)| *t).collect();
        let via: BTreeMap<Uuid, String> = frontier
            .into_iter()
            .filter_map(|(t, via)| via.map(|via| (t, via)))
            .collect();
        let rows: Vec<(Uuid, Uuid, Uuid, Option<String>)> = sqlx::query_as(VERSIONS_OF_SQL)
            .bind(&ts)
            .fetch_all(tx.as_mut())
            .await
            .map_err(map_err)?;
        let mut added: Vec<Uuid> = Vec::new();
        for (t, handle, owner_id, schema_id) in rows {
            if footprint.versions.contains_key(&t) {
                continue;
            }
            if let Some(via) = via.get(&t) {
                footprint
                    .joined_via
                    .entry(handle)
                    .or_insert_with(|| via.clone());
            }
            footprint.versions.insert(
                t,
                Version {
                    handle,
                    owner_id,
                    schema_id,
                },
            );
            added.push(t);
        }
        frontier = Vec::new();
        if added.is_empty() {
            break;
        }
        for pair in pairs {
            let table = PgIdent::table(&pair.table)?;
            let column = PgIdent::column(&pair.column)?;
            // A keyed table answers with the referencing row's own memory,
            // which joins the footprint; an unkeyed one answers with the
            // referenced `t`, which only a refusal can report.
            let sql = match pair.key {
                Some(key) => {
                    let key = PgIdent::column(key)?;
                    // SQL-POLICY: PgIdent
                    format!(
                        "SELECT {key} FROM {tbl} WHERE {col} = ANY($1::uuid[])",
                        key = key.as_str(),
                        tbl = table.as_str(),
                        col = column.as_str()
                    )
                }
                // SQL-POLICY: PgIdent
                None => format!(
                    "SELECT {col} FROM {tbl} WHERE {col} = ANY($1::uuid[]) LIMIT 20",
                    tbl = table.as_str(),
                    col = column.as_str()
                ),
            };
            // SQL-POLICY: PgIdent (the statement above interpolates only validated identifiers)
            let found: Vec<Uuid> = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
                .bind(&added)
                .fetch_all(tx.as_mut())
                .await
                .map_err(map_err)?;
            let via = format!("{}.{}", pair.table, pair.column);
            if pair.key.is_some() {
                frontier.extend(
                    found
                        .into_iter()
                        .filter(|t| !footprint.versions.contains_key(t))
                        .map(|t| (t, Some(via.clone()))),
                );
            } else {
                footprint
                    .unerasable
                    .extend(found.into_iter().map(|t| format!("{via} t={t}")));
            }
        }
        frontier.sort_unstable();
        frontier.dedup_by_key(|(t, _)| *t);
    }
    Ok(footprint)
}

/// The first reason the footprint is out of scope, cap excluded.
fn scope_refusal(
    footprint: &Footprint,
    owner_id: Uuid,
    own_schemas: &[String],
) -> Option<SeriesEraseRefusal> {
    if !footprint.unerasable.is_empty() {
        return Some(SeriesEraseRefusal {
            kind: SeriesEraseRefusalKind::UnerasableReference,
            offending: footprint.unerasable.clone(),
        });
    }
    let name = |t: &Uuid, version: &Version| match footprint.joined_via.get(&version.handle) {
        Some(via) => format!("{via} t={t}"),
        None => format!("t={t}"),
    };
    let foreign_owner: Vec<String> = footprint
        .versions
        .iter()
        .filter(|(_, version)| version.owner_id != owner_id)
        .map(|(t, version)| name(t, version))
        .collect();
    if !foreign_owner.is_empty() {
        return Some(SeriesEraseRefusal {
            kind: SeriesEraseRefusalKind::CrossOwner,
            offending: foreign_owner,
        });
    }
    let foreign_schema: Vec<String> = footprint
        .versions
        .iter()
        .filter(|(_, version)| {
            version
                .schema_id
                .as_ref()
                .is_none_or(|schema| !own_schemas.contains(schema))
        })
        .map(|(t, version)| {
            format!(
                "{} schema={}",
                name(t, version),
                version.schema_id.as_deref().unwrap_or("unknown")
            )
        })
        .collect();
    if !foreign_schema.is_empty() {
        return Some(SeriesEraseRefusal {
            kind: SeriesEraseRefusalKind::ForeignSchema,
            offending: foreign_schema,
        });
    }
    None
}

fn over_cap(footprint: &Footprint, request: &SeriesEraseRequest<'_>) -> Option<Vec<String>> {
    let series = footprint.handles().len();
    let versions = footprint.versions.len();
    (series > request.max_series || versions > request.max_versions).then(|| {
        vec![
            format!("series={series} (max {})", request.max_series),
            format!("versions={versions} (max {})", request.max_versions),
        ]
    })
}

/// `newest`: the newest version's `t` of every series of schema `$2` with a
/// version under owner `$1`, hot or cooled, over EVERY version of the
/// series. The one reading of "newest version" both age selections share.
macro_rules! newest_versions_cte {
    () => {
        "\
WITH own AS (
    SELECT handle FROM proxima_core.memory
     WHERE owner_id = $1 AND schema_id = $2
    UNION
    SELECT c.handle FROM proxima_core.cooled c
      JOIN proxima_core.content k ON k.content_id = c.content_id
     WHERE c.owner_id = $1 AND k.schema_id = $2
), versions AS (
    SELECT m.handle, m.t FROM proxima_core.memory m JOIN own USING (handle)
    UNION ALL
    SELECT c.handle, c.t FROM proxima_core.cooled c JOIN own USING (handle)
), newest AS (
    SELECT DISTINCT ON (handle) t FROM versions ORDER BY handle, t DESC
)
"
    };
}

mod selection;

/// Series of `schema` under `owner_id` whose newest version, over every
/// version of the series, is older than `bound`, oldest first. The seed is
/// the newest version's `t`.
const ADMITTED_BEFORE_SQL: &str = concat!(
    newest_versions_cte!(),
    "SELECT t FROM newest
 WHERE t < $3
 ORDER BY t
 LIMIT $4"
);

/// The smallest `UUIDv7` of the millisecond `cutoff` falls in. A `t` below
/// it was minted in an earlier millisecond; `uuid` compares bytewise and a
/// `UUIDv7` leads with its big-endian millisecond timestamp.
fn uuid_v7_floor(cutoff: time::OffsetDateTime) -> Uuid {
    let millis = cutoff.unix_timestamp_nanos().div_euclid(1_000_000);
    let millis = u64::try_from(millis.clamp(0, (1_i128 << 48) - 1)).unwrap_or(0);
    let mut bytes = [0_u8; 16];
    bytes[..6].copy_from_slice(&millis.to_be_bytes()[2..]);
    bytes[6] = 0x70;
    Uuid::from_bytes(bytes)
}

/// Rows referencing the footprint that the footprint does not own, asked
/// again after the lock. The lock stops new ones; this catches one that
/// committed between the closure and the lock.
async fn footprint_grew(
    tx: &mut Transaction<'_, Postgres>,
    pairs: &[ReferencePair],
    footprint: &Footprint,
) -> Result<bool, StorageError> {
    let ts = footprint.ts();
    let handles: Vec<Uuid> = footprint.handles().into_iter().collect();
    let now: Vec<Uuid> = sqlx::query_scalar(
        "SELECT t FROM proxima_core.memory WHERE handle = ANY($1::uuid[])
         UNION
         SELECT t FROM proxima_core.cooled WHERE handle = ANY($1::uuid[])
         ORDER BY 1",
    )
    .bind(&handles)
    .fetch_all(tx.as_mut())
    .await
    .map_err(map_err)?;
    if now != ts {
        return Ok(true);
    }
    for pair in pairs {
        let table = PgIdent::table(&pair.table)?;
        let column = PgIdent::column(&pair.column)?;
        let sql = match pair.key {
            Some(key) => {
                let key = PgIdent::column(key)?;
                // SQL-POLICY: PgIdent
                format!(
                    "SELECT EXISTS (SELECT 1 FROM {tbl} WHERE {col} = ANY($1::uuid[]) \
                     AND NOT {key} = ANY($1::uuid[]))",
                    key = key.as_str(),
                    tbl = table.as_str(),
                    col = column.as_str()
                )
            }
            // SQL-POLICY: PgIdent
            None => format!(
                "SELECT EXISTS (SELECT 1 FROM {tbl} WHERE {col} = ANY($1::uuid[]))",
                tbl = table.as_str(),
                col = column.as_str()
            ),
        };
        // SQL-POLICY: PgIdent (the statement above interpolates only validated identifiers)
        let found: bool = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
            .bind(&ts)
            .fetch_one(tx.as_mut())
            .await
            .map_err(map_err)?;
        if found {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Delete the referencing rows before the per-version erase.
///
/// Every one of them belongs to a version in the footprint, so each would
/// go with its own memory anyway — but `NO ACTION` keys are checked per
/// statement, and the erase deletes versions in `t` order, so a row pointing
/// at an earlier version would abort the delete of that version first.
async fn delete_referencing_rows(
    tx: &mut Transaction<'_, Postgres>,
    pairs: &[ReferencePair],
    ts: &[Uuid],
) -> Result<(), StorageError> {
    for pair in pairs.iter().filter(|pair| pair.key.is_some()) {
        let table = PgIdent::table(&pair.table)?;
        let column = PgIdent::column(&pair.column)?;
        // SQL-POLICY: PgIdent
        let sql = format!(
            "DELETE FROM {tbl} WHERE {col} = ANY($1::uuid[])",
            tbl = table.as_str(),
            col = column.as_str()
        );
        // SQL-POLICY: PgIdent (the statement above interpolates only validated identifiers)
        sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(ts)
            .execute(tx.as_mut())
            .await
            .map_err(map_err)?;
    }
    Ok(())
}

/// Hot memories outside the footprint whose pins name one of its versions.
const DANGLING_PINS_SQL: &str = "\
SELECT count(*) FROM proxima_core.memory
 WHERE (origins && $1::uuid[] OR refs && $1::uuid[])
   AND NOT t = ANY($1::uuid[])";

fn over(offending: Vec<String>) -> SeriesEraseRefusal {
    SeriesEraseRefusal {
        kind: SeriesEraseRefusalKind::OverCap,
        offending,
    }
}

/// The closed footprint of `request`'s selection, and whether matching
/// series were left for a later call. `Err` is an over-cap refusal.
async fn resolve_footprint(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    pairs: &[ReferencePair],
    owner_id: Uuid,
    request: &SeriesEraseRequest<'_>,
) -> Result<Result<(Footprint, bool), SeriesEraseRefusal>, StorageError> {
    // One past the cap, so a full page knows whether more wait.
    let limit = i64::try_from(request.max_series.saturating_add(1)).unwrap_or(i64::MAX);
    let seeds: Vec<Uuid> = match request.selection {
        SeriesSelection::Ids(ids) => {
            let seeds: Vec<Uuid> = ids.iter().map(|id| id.into_inner()).collect();
            let footprint = close_footprint(tx, pairs, &seeds).await?;
            return Ok(match over_cap(&footprint, request) {
                Some(offending) => Err(over(offending)),
                None => Ok((footprint, false)),
            });
        }
        SeriesSelection::AdmittedBefore { schema, cutoff } => {
            sqlx::query_scalar(ADMITTED_BEFORE_SQL)
                .bind(owner_id)
                .bind(schema.as_str())
                .bind(uuid_v7_floor(*cutoff))
                .bind(limit)
                .fetch_all(tx.as_mut())
                .await
                .map_err(map_err)?
        }
        SeriesSelection::SidecarEquals { schema, predicates } => {
            selection::sidecar_equals_seeds(
                tx,
                sidecars,
                owner_id,
                schema,
                request.sidecar_table,
                predicates,
                limit,
            )
            .await?
        }
        SeriesSelection::DeclaredBefore {
            schema,
            column,
            cutoff,
        } => {
            selection::declared_before_seeds(
                tx,
                sidecars,
                owner_id,
                schema,
                request.sidecar_table,
                column,
                *cutoff,
                limit,
            )
            .await?
        }
    };
    page_footprint(tx, pairs, request, seeds).await
}

/// Close the longest prefix of `seeds` whose footprint fits the cap.
///
/// `seeds` is one page past the cap, oldest first. Halve until the closure
/// fits, so a later series is never erased while an older one waits; a
/// single seed over the cap is refused.
async fn page_footprint(
    tx: &mut Transaction<'_, Postgres>,
    pairs: &[ReferencePair],
    request: &SeriesEraseRequest<'_>,
    mut seeds: Vec<Uuid>,
) -> Result<Result<(Footprint, bool), SeriesEraseRefusal>, StorageError> {
    let mut more = seeds.len() > request.max_series;
    seeds.truncate(request.max_series);
    loop {
        let footprint = close_footprint(tx, pairs, &seeds).await?;
        match over_cap(&footprint, request) {
            None => return Ok(Ok((footprint, more))),
            Some(mut offending) if seeds.len() <= 1 => {
                offending.insert(0, format!("t={} alone", seeds[0]));
                return Ok(Err(over(offending)));
            }
            Some(_) => {
                seeds.truncate(seeds.len() / 2);
                more = true;
            }
        }
    }
}

/// Resolve, refuse or erase one selection. The returned plan is the cold
/// debt the caller settles after commit.
///
/// # Errors
///
/// Storage faults; `Retryable` when the footprint changed between its
/// closure and its lock.
pub(crate) async fn erase_series(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    context: &crate::PgHostStateEraseContext,
    owner: &Owner,
    request: &SeriesEraseRequest<'_>,
) -> Result<(SeriesEraseOutcome, ColdPurgePlan), StorageError> {
    let owner_id = owner.stored_owner_id();
    let pairs = reference_pairs(tx, &context.surfaces).await?;
    let (footprint, more_remaining) =
        match resolve_footprint(tx, sidecars, &pairs, owner_id, request).await? {
            Ok(resolved) => resolved,
            Err(refusal) => {
                return Ok((
                    SeriesEraseOutcome::Refused(refusal),
                    ColdPurgePlan::default(),
                ));
            }
        };
    if let Some(refusal) = scope_refusal(&footprint, owner_id, request.own_schemas) {
        return Ok((
            SeriesEraseOutcome::Refused(refusal),
            ColdPurgePlan::default(),
        ));
    }
    let ts = footprint.ts();
    if ts.is_empty() {
        return Ok((
            SeriesEraseOutcome::Erased(SeriesEraseReport {
                more_remaining,
                ..SeriesEraseReport::default()
            }),
            ColdPurgePlan::default(),
        ));
    }

    lock_admissions_for_erase(tx, owner, &ts).await?;
    if footprint_grew(tx, &pairs, &footprint).await? {
        return Err(StorageError::Retryable(
            "erase footprint changed before its lock".into(),
        ));
    }

    let dangling_pins: i64 = sqlx::query_scalar(DANGLING_PINS_SQL)
        .bind(&ts)
        .fetch_one(tx.as_mut())
        .await
        .map_err(map_err)?;
    let cited = cited_blobs_locked_for_erase(tx, owner, &ts).await?;
    delete_referencing_rows(tx, &pairs, &ts).await?;
    let (erased, cold_purge) = erase_memory_series(tx, sidecars, context, owner, &ts).await?;
    if usize::try_from(erased).ok() != Some(ts.len()) {
        return Err(StorageError::Internal(format!(
            "series erase deleted {erased} admissions of a locked footprint of {}",
            ts.len()
        )));
    }
    let blobs_removed: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM unnest($1::uuid[]) AS b(blob_id)
          WHERE NOT EXISTS (
                SELECT 1 FROM proxima_core.blob k WHERE k.blob_id = b.blob_id)",
    )
    .bind(&cited)
    .fetch_one(tx.as_mut())
    .await
    .map_err(map_err)?;

    let report = SeriesEraseReport {
        versions: ts.iter().copied().map(MemoryId::new).collect(),
        series_erased: count(footprint.handles().len()),
        referencing_series: count(footprint.referencing_series()),
        blobs_removed: u64::try_from(blobs_removed).unwrap_or(0),
        cold_objects_pending: count(cold_purge.entries().len()),
        dangling_pins: u64::try_from(dangling_pins).unwrap_or(0),
        more_remaining,
    };
    Ok((SeriesEraseOutcome::Erased(report), cold_purge))
}

fn count(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::uuid_v7_floor;

    #[test]
    fn the_floor_orders_below_every_uuid_v7_of_its_millisecond_and_above_the_last() {
        let cutoff = time::OffsetDateTime::from_unix_timestamp(1_790_000_000).expect("valid");
        let floor = uuid_v7_floor(cutoff);
        let millis = u64::try_from(cutoff.unix_timestamp() * 1000).expect("positive");
        let at = uuid::Uuid::new_v7(uuid::Timestamp::from_unix(
            uuid::NoContext,
            millis / 1000,
            0,
        ));
        let before = uuid::Uuid::new_v7(uuid::Timestamp::from_unix(
            uuid::NoContext,
            millis / 1000 - 1,
            999_000_000,
        ));
        assert!(before < floor, "a t minted a millisecond earlier is before");
        assert!(at >= floor, "a t minted in the cutoff millisecond is not");
        assert_eq!(floor.get_version_num(), 7);
    }
}
