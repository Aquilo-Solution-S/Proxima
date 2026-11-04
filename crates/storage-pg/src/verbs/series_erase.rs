//! Spiice-scoped series erase: the storage half of
//! `UnitOfWork::erase_own_series` (docs/data-and-erasure.md).
//!
//! The engine has authorized the owner and resolved the named spiice's
//! memory schemas. This module turns the selection into a closed footprint —
//! every version of every selected series, plus every series whose rows
//! reference the footprint through a foreign key — refuses the footprint when
//! any of it lies outside the owner, the schemas or the cap, locks it, and
//! erases it through [`erase_memory_series`]. Nothing is deleted before the
//! last refusal has been asked.
//!
//! It runs on the transaction `PgStorage::begin_series_erase` opened: the
//! owner's exclusive fence is already held, and it was the transaction's
//! first lock.

use std::collections::{BTreeMap, BTreeSet};

use proxiima_core::owner_inverse::OwnerSurfaces;
use proxiima_core::spiice::KeyShape;
use proxiima_core::verbs::own_erase::{
    SeriesEraseOutcome, SeriesEraseRefusal, SeriesEraseRefusalKind, SeriesEraseReport,
    SeriesEraseRequest, SeriesSelection,
};
use proxiima_core::{MemoryId, OwnerId, StorageError};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use super::forget::{
    ColdPurgePlan, cited_blobs_locked_for_erase, erase_memory_series, lock_admissions_for_erase,
};
use crate::error::map_err;
use crate::pg_ident::PgIdent;
use crate::sidecars::{PgSidecarRegistryFrozen, int_to_u64};

/// One foreign key into `proxiima_core.memory (t)` that makes a row depend on
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
/// `proxiima_core.memory (t)`, from the catalog rather than a list: a spiice
/// adding a referencing column is covered the day its migration runs.
/// `CASCADE` and `SET NULL` keys are the database's to resolve.
const REFERENCE_KEYS_SQL: &str = "\
SELECT n.nspname || '.' || cl.relname AS tbl, a.attname::text AS col
  FROM pg_catalog.pg_constraint c
  JOIN pg_catalog.pg_class cl ON cl.oid = c.conrelid
  JOIN pg_catalog.pg_namespace n ON n.oid = cl.relnamespace
  JOIN pg_catalog.pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = c.conkey[1]
 WHERE c.contype = 'f'
   AND c.confrelid = 'proxiima_core.memory'::regclass
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
    /// The series' schema, `memory.schema_id`. A forgotten version keeps
    /// its row, so every version has one; `None` passes no schema check.
    schema_id: Option<String>,
}

/// Every version of every series named by `ts`, any owner, with its schema.
const VERSIONS_OF_SQL: &str = "\
WITH handles AS (
    SELECT DISTINCT handle FROM proxiima_core.memory WHERE t = ANY($1::uuid[])
)
SELECT m.t, m.handle, m.owner_id, m.schema_id
  FROM proxiima_core.memory m JOIN handles h ON h.handle = m.handle";

/// The footprint: a closed set of whole series.
#[derive(Debug, Default)]
struct Footprint {
    versions: BTreeMap<Uuid, Version>,
    /// Series first reached through a referencing row, and the key that
    /// reached them, so a refusal names something an operator can look at.
    joined_via: BTreeMap<Uuid, String>,
    /// Rows of tables with no memory key that reference the footprint.
    unerasable: Vec<String>,
    /// `(referrer, referenced)`: the `t` whose sidecar row holds a foreign
    /// key to the `t` of another version of the footprint.
    references: Vec<(Uuid, Uuid)>,
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

    /// The versions of each series, by series handle.
    fn series(&self) -> BTreeMap<Uuid, Vec<Uuid>> {
        let mut series: BTreeMap<Uuid, Vec<Uuid>> = BTreeMap::new();
        for (t, version) in &self.versions {
            series.entry(version.handle).or_default().push(*t);
        }
        series
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
            let via = format!("{}.{}", pair.table, pair.column);
            // A keyed table answers with the referencing row's own memory,
            // which joins the footprint, and the version it references; an
            // unkeyed one has no memory to join, so it answers with the
            // referenced `t` twice and only a refusal reports it.
            let key = PgIdent::column(pair.key.unwrap_or(&pair.column))?;
            let limit = if pair.key.is_some() { "" } else { " LIMIT 20" };
            // SQL-POLICY: PgIdent
            let sql = format!(
                "SELECT {key}, {col} FROM {tbl} WHERE {col} = ANY($1::uuid[]){limit}",
                key = key.as_str(),
                tbl = table.as_str(),
                col = column.as_str()
            );
            // SQL-POLICY: PgIdent (the statement above interpolates only validated identifiers)
            let found: Vec<(Uuid, Uuid)> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
                .bind(&added)
                .fetch_all(tx.as_mut())
                .await
                .map_err(map_err)?;
            if pair.key.is_some() {
                frontier.extend(
                    found
                        .iter()
                        .map(|(referrer, _)| *referrer)
                        .filter(|t| !footprint.versions.contains_key(t))
                        .map(|t| (t, Some(via.clone()))),
                );
                footprint.references.extend(found);
            } else {
                footprint
                    .unerasable
                    .extend(found.into_iter().map(|(_, t)| format!("{via} t={t}")));
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

/// An `Ids` list is erased whole or not at all: its closure over either cap
/// is refused. Every other selection is paged, never refused for its size.
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
/// version under owner `$1`, live or forgotten, over EVERY version of the
/// series. The one reading of "newest version" both age selections share.
macro_rules! newest_versions_cte {
    () => {
        "\
WITH own AS (
    SELECT DISTINCT handle FROM proxiima_core.memory
     WHERE owner_id = $1 AND schema_id = $2
), versions AS (
    SELECT m.handle, m.t FROM proxiima_core.memory m JOIN own USING (handle)
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

/// The smallest `UUIDv8` of the millisecond `cutoff` falls in. A `t` below
/// it was minted in an earlier millisecond; `uuid` compares bytewise and a
/// `UUIDv8` leads with its big-endian millisecond timestamp.
fn memory_uuid_floor(cutoff: time::OffsetDateTime) -> Uuid {
    let millis = cutoff.unix_timestamp_nanos().div_euclid(1_000_000);
    let millis = u64::try_from(millis.clamp(0, (1_i128 << 48) - 1)).unwrap_or(0);
    let mut bytes = [0_u8; 16];
    bytes[..6].copy_from_slice(&millis.to_be_bytes()[2..]);
    bytes[6] = 0x80;
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
        "SELECT t FROM proxiima_core.memory WHERE handle = ANY($1::uuid[])
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
SELECT count(*) FROM proxiima_core.memory
 WHERE (origins && $1::uuid[] OR refs && $1::uuid[])
   AND NOT t = ANY($1::uuid[])";

fn over(offending: Vec<String>) -> SeriesEraseRefusal {
    SeriesEraseRefusal {
        kind: SeriesEraseRefusalKind::OverCap,
        offending,
    }
}

/// The page of `request`'s selection, and whether matching series were left
/// for a later call. `Err` is a refusal.
async fn resolve_footprint(
    tx: &mut Transaction<'_, Postgres>,
    sidecars: &PgSidecarRegistryFrozen,
    pairs: &[ReferencePair],
    owner_id: Uuid,
    request: &SeriesEraseRequest<'_>,
) -> Result<Result<(Footprint, bool), SeriesEraseRefusal>, StorageError> {
    // One past the cap, so a full list of seeds knows whether more wait.
    let limit = i64::try_from(request.max_series.saturating_add(1)).unwrap_or(i64::MAX);
    let seeds: Vec<Uuid> = match request.selection {
        SeriesSelection::Ids(ids) => {
            let seeds: Vec<Uuid> = ids.iter().map(|id| id.into_inner()).collect();
            let footprint = close_footprint(tx, pairs, &seeds).await?;
            let refusal = over_cap(&footprint, request)
                .map(over)
                .or_else(|| scope_refusal(&footprint, owner_id, request.own_schemas));
            return Ok(refusal.map_or(Ok((footprint, false)), Err));
        }
        SeriesSelection::AdmittedBefore { schema, cutoff } => {
            sqlx::query_scalar(ADMITTED_BEFORE_SQL)
                .bind(owner_id)
                .bind(schema.as_str())
                .bind(memory_uuid_floor(*cutoff))
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
    page_footprint(tx, pairs, owner_id, request, seeds).await
}

/// The series of one closure in an order any prefix of which may be erased:
/// a series whose rows reference another series of the closure comes before
/// it, so the seed's own series is last.
///
/// Ranked by the longest chain of referrers above a series: the series
/// nothing references go first, then what only they reference. Series that
/// reference one another (a ring) cannot be ranked; each ring is one unit,
/// and what hangs below it is ranked as usual once the ring is placed. Each
/// unit is a list of series handles, oldest first.
fn erase_order(closure: &Footprint) -> Vec<Vec<Uuid>> {
    let mut references: BTreeMap<Uuid, BTreeSet<Uuid>> = BTreeMap::new();
    // The distinct series that reference each series and are not placed yet.
    let mut pending: BTreeMap<Uuid, usize> =
        closure.handles().into_iter().map(|h| (h, 0)).collect();
    for (referrer, referenced) in &closure.references {
        let series = |t: &Uuid| closure.versions.get(t).map(|version| version.handle);
        if let (Some(from), Some(to)) = (series(referrer), series(referenced))
            && from != to
            && references.entry(from).or_default().insert(to)
        {
            *pending.entry(to).or_default() += 1;
        }
    }
    let mut order: Vec<Vec<Uuid>> = Vec::new();
    while !pending.is_empty() {
        let ready: Vec<Uuid> = pending
            .iter()
            .filter(|(_, referrers)| **referrers == 0)
            .map(|(handle, _)| *handle)
            .collect();
        let units: Vec<Vec<Uuid>> = if ready.is_empty() {
            vec![ring(&pending, &references)]
        } else {
            ready.into_iter().map(|handle| vec![handle]).collect()
        };
        for handle in units.iter().flatten() {
            pending.remove(handle);
        }
        for handle in units.iter().flatten() {
            for to in references.get(handle).into_iter().flatten() {
                if let Some(referrers) = pending.get_mut(to) {
                    *referrers -= 1;
                }
            }
        }
        order.extend(units);
    }
    order
}

/// The series of `pending` that lie on a cycle, or between two cycles: the
/// next unit when none is ready. `pending` holds only rings and what hangs
/// below them, and each of its series has a referrer in it, so a cycle
/// exists and the unit is never empty.
fn ring(pending: &BTreeMap<Uuid, usize>, references: &BTreeMap<Uuid, BTreeSet<Uuid>>) -> Vec<Uuid> {
    let mut ring: BTreeSet<Uuid> = pending.keys().copied().collect();
    // Peel the series that reference nothing still in the ring: they hang
    // below it.
    while let Some(below) = ring
        .iter()
        .find(|handle| {
            references
                .get(handle)
                .is_none_or(|to| to.is_disjoint(&ring))
        })
        .copied()
    {
        ring.remove(&below);
    }
    ring.into_iter().collect()
}

/// The next page of `seeds`' closure: the closures of the oldest seeds in
/// turn, each in [`erase_order`], up to `max_versions` versions, and whether
/// more of the selection waits.
///
/// `seeds` is oldest first, one past `max_series` when more wait. A page ends
/// at the first unit that does not fit, never skipping it for a later one, so
/// a later seed is only reached once an older one is wholly in the page. Each
/// closure is checked for scope whole, not only the part that is erased now:
/// a refusal comes before the first page is deleted, not after it.
async fn page_footprint(
    tx: &mut Transaction<'_, Postgres>,
    pairs: &[ReferencePair],
    owner_id: Uuid,
    request: &SeriesEraseRequest<'_>,
    seeds: Vec<Uuid>,
) -> Result<Result<(Footprint, bool), SeriesEraseRefusal>, StorageError> {
    let more_seeds = seeds.len() > request.max_series;
    let mut page = Footprint::default();
    for seed in seeds.into_iter().take(request.max_series) {
        if page.versions.contains_key(&seed) {
            continue;
        }
        let closure = close_footprint(tx, pairs, &[seed]).await?;
        if let Some(refusal) = scope_refusal(&closure, owner_id, request.own_schemas) {
            return Ok(Err(refusal));
        }
        let series = closure.series();
        let placed = page.handles();
        let units: Vec<(Vec<Uuid>, usize)> = erase_order(&closure)
            .into_iter()
            .map(|unit| {
                unit.into_iter()
                    .filter(|handle| !placed.contains(handle))
                    .collect::<Vec<_>>()
            })
            .map(|unit| {
                let versions = unit.iter().map(|handle| series[handle].len()).sum();
                (unit, versions)
            })
            .collect();
        // A unit over a page can never be erased: refuse it now, before the
        // units ahead of it are, so a refusal never follows a partial erase.
        if let Some((unit, versions)) = units
            .iter()
            .find(|(_, versions)| *versions > request.max_versions)
        {
            return Ok(Err(over(vec![format!(
                "t={seed}: {} series, {versions} versions that cannot be split \
                 (max {} versions)",
                unit.len(),
                request.max_versions
            )])));
        }
        for (unit, versions) in units {
            if page.versions.len() + versions > request.max_versions {
                return Ok(Ok((page, true)));
            }
            for handle in unit {
                for t in &series[&handle] {
                    page.versions.insert(*t, closure.versions[t].clone());
                }
                if let Some(via) = closure.joined_via.get(&handle) {
                    page.joined_via.insert(handle, via.clone());
                }
            }
        }
    }
    Ok(Ok((page, more_seeds)))
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
    owner: &OwnerId,
    request: &SeriesEraseRequest<'_>,
) -> Result<(SeriesEraseOutcome, ColdPurgePlan), StorageError> {
    let owner_id = owner.into_inner();
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
                SELECT 1 FROM proxiima_core.blob k WHERE k.blob_id = b.blob_id)",
    )
    .bind(&cited)
    .fetch_one(tx.as_mut())
    .await
    .map_err(map_err)?;

    let report = SeriesEraseReport {
        versions: ts.iter().copied().map(MemoryId::new).collect(),
        series_erased: count(footprint.handles().len()),
        referencing_series: count(footprint.referencing_series()),
        blobs_removed: int_to_u64(blobs_removed, "blobs removed")?,
        cold_objects_pending: count(cold_purge.entries().len()),
        dangling_pins: int_to_u64(dangling_pins, "dangling pins")?,
        more_remaining,
    };
    Ok((SeriesEraseOutcome::Erased(report), cold_purge))
}

fn count(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use uuid::Uuid;

    use super::{Footprint, Version, erase_order, memory_uuid_floor};

    /// A closure of one-version series `1..=n`; each `(a, b)` is a row of
    /// series `a` that references series `b`.
    fn closure(n: u128, references: &[(u128, u128)]) -> Footprint {
        let id = |n: u128| Uuid::from_u128(n);
        Footprint {
            versions: (1..=n)
                .map(|n| {
                    let version = Version {
                        handle: id(n),
                        owner_id: id(0),
                        schema_id: None,
                    };
                    (id(n), version)
                })
                .collect::<BTreeMap<_, _>>(),
            references: references.iter().map(|(a, b)| (id(*a), id(*b))).collect(),
            ..Footprint::default()
        }
    }

    fn numbers(order: Vec<Vec<Uuid>>) -> Vec<Vec<u128>> {
        order
            .into_iter()
            .map(|unit| unit.into_iter().map(|handle| handle.as_u128()).collect())
            .collect()
    }

    /// Pages 4 and 5 cite attachment 2, page 6 cites attachment 3, both
    /// attachments cite the mail 1: every page goes before any attachment,
    /// and the mail, the seed, is last.
    #[test]
    fn a_referrer_goes_before_what_it_references_and_the_seed_is_last() {
        let order = erase_order(&closure(6, &[(2, 1), (3, 1), (4, 2), (5, 2), (6, 3)]));
        assert_eq!(
            numbers(order),
            vec![vec![4], vec![5], vec![6], vec![2], vec![3], vec![1]]
        );
    }

    /// A series that is referenced by a later rank waits for all of its
    /// referrers, not the first: 3 cites 1 directly and through 2.
    #[test]
    fn a_series_waits_for_its_last_referrer() {
        let order = erase_order(&closure(3, &[(3, 1), (3, 2), (2, 1)]));
        assert_eq!(numbers(order), vec![vec![3], vec![2], vec![1]]);
    }

    /// 2 and 3 cite each other and the seed 1; 4 cites 5, which cites 1. The
    /// ring is one unit, placed when its turn comes, and the seed, which
    /// hangs below it, still goes last.
    #[test]
    fn a_ring_is_one_unit_and_what_hangs_below_it_is_ranked_after() {
        let order = erase_order(&closure(5, &[(2, 3), (3, 2), (3, 1), (4, 5), (5, 1)]));
        assert_eq!(numbers(order), vec![vec![4], vec![5], vec![2, 3], vec![1]]);
    }

    /// A ring that references a large series: the series is not dragged
    /// into the ring's unit, so it can be paged on its own.
    #[test]
    fn a_ring_does_not_absorb_what_it_references() {
        let order = erase_order(&closure(4, &[(1, 2), (2, 1), (1, 3), (2, 4), (3, 4)]));
        assert_eq!(numbers(order), vec![vec![1, 2], vec![3], vec![4]]);
    }

    /// Versions of one series referencing each other are not an edge between
    /// series.
    #[test]
    fn a_series_that_references_itself_is_not_a_ring() {
        let mut closure = closure(2, &[(2, 1)]);
        let second = Uuid::from_u128(3);
        closure.versions.insert(
            second,
            Version {
                handle: Uuid::from_u128(2),
                owner_id: Uuid::from_u128(0),
                schema_id: None,
            },
        );
        closure.references.push((second, Uuid::from_u128(2)));
        assert_eq!(numbers(erase_order(&closure)), vec![vec![2], vec![1]]);
    }

    #[test]
    fn the_floor_orders_below_every_memory_uuid_of_its_millisecond_and_above_the_last() {
        let cutoff = time::OffsetDateTime::from_unix_timestamp(1_790_000_000).expect("valid");
        let floor = memory_uuid_floor(cutoff);
        let millis = u64::try_from(cutoff.unix_timestamp() * 1000).expect("positive");
        let id_at = |millis: u64| {
            let mut bytes = proxiima_core::new_memory_uuid(3).into_bytes();
            bytes[..6].copy_from_slice(&millis.to_be_bytes()[2..]);
            uuid::Uuid::from_bytes(bytes)
        };
        let at = id_at(millis);
        let before = id_at(millis - 1);
        assert!(before < floor, "a t minted a millisecond earlier is before");
        assert!(at >= floor, "a t minted in the cutoff millisecond is not");
        assert_eq!(floor.get_version_num(), 8);
    }
}
