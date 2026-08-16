use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use proxima_core::read_models::ChangeEventForWake;
use proxima_core::{Owner, OwnerRef, OwnerRefKind, StorageError};
use sqlx::PgPool;
use sqlx::Row;

use crate::change_event::{hydrate_change_event, hydrate_change_events_batch};
use crate::error::map_err;

/// Env var (milliseconds) enabling the commit-safety grace window on the
/// forward change-event cursor. Unset/`0` = disabled (default).
const COMMIT_GRACE_ENV: &str = "PROXIMA_CHANGE_EVENT_COMMIT_GRACE_MS";

/// Configured commit-safety grace, read once from [`COMMIT_GRACE_ENV`].
///
/// `change_event.seq` is a `UUIDv7` stamped at INSERT time, so a slow
/// writer can commit a *smaller* seq after a faster writer's larger seq is
/// already visible. A forward cursor (`ce.seq > after`) that advanced past the
/// larger seq would then skip the smaller one forever. When a positive grace
/// is configured, the read withholds events whose seq timestamp is newer than
/// `now - grace`, so a slow committer whose commit latency is below the grace
/// is not skipped once the cursor advances.
///
/// Default off because current consumers (and their tests) rely on immediate
/// write-then-read visibility; enabling it trades up to `grace` of wake
/// latency for skip-safety. See the module NOTE for the correctness bound.
fn configured_commit_grace() -> Duration {
    static GRACE: OnceLock<Duration> = OnceLock::new();
    *GRACE.get_or_init(|| {
        std::env::var(COMMIT_GRACE_ENV)
            .ok()
            .and_then(|raw| raw.trim().parse::<u64>().ok())
            .map_or(Duration::ZERO, Duration::from_millis)
    })
}

/// `UUIDv7` low-water horizon for a wall-clock `grace`: the smallest v7 uuid
/// stamped at `now_ms - grace`. Any `change_event.seq` strictly below it was
/// stamped more than `grace` ago; comparing `ce.seq < horizon` therefore
/// withholds too-recent (possibly still-in-flight) events. `None` disables the
/// bound (zero grace), which is the default.
fn commit_horizon_seq(now_ms: u64, grace: Duration) -> Option<uuid::Uuid> {
    let grace_ms = u64::try_from(grace.as_millis()).unwrap_or(u64::MAX);
    if grace_ms == 0 {
        return None;
    }
    let horizon_ms = now_ms.saturating_sub(grace_ms);
    let ms_be = horizon_ms.to_be_bytes();
    let mut bytes = [0u8; 16];
    // UUIDv7: 48-bit big-endian unix_ms in bytes 0..6, then version nibble.
    bytes[0..6].copy_from_slice(&ms_be[2..8]);
    bytes[6] = 0x70;
    Some(uuid::Uuid::from_bytes(bytes))
}

fn now_unix_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

/// Change events visible to `read_owners`, after `after`, oldest first.
///
/// # Errors
///
/// Returns [`StorageError`] when the change-event read fails.
pub async fn list_change_events_after(
    pool: &PgPool,
    read_owners: &[OwnerRef],
    after: uuid::Uuid,
    limit: usize,
) -> Result<Vec<ChangeEventForWake>, StorageError> {
    if read_owners.is_empty() {
        return Ok(Vec::new());
    }
    let (read_owner_kinds, read_owner_ids) = read_owner_columns(read_owners);
    let (world_kind, world_id) =
        crate::access::owner_columns::owner_binds(&proxima_core::access::world());
    let edge_visibility = edge_event_visibility_predicate(1, 2, 5, 6);
    // Plain `=` on ce.owner_id here and in the replay read below: the
    // change_event CHECKs prove the column is never NULL, so `=` selects
    // exactly what IS NOT DISTINCT FROM did while staying an index
    // condition.
    // Commit-safety horizon (opt-in via COMMIT_GRACE_ENV). When set, bind it
    // as $7 and only return events stamped before `now - grace`.
    let horizon = commit_horizon_seq(now_unix_ms(), configured_commit_grace());
    let horizon_clause = if horizon.is_some() {
        "AND ce.seq < $7"
    } else {
        ""
    };
    let sql = format!(
        "SELECT ce.seq
             FROM proxima_core.change_event ce
             WHERE EXISTS (
                SELECT 1
                  FROM unnest($1::proxima_core.owner_kind[], $2::uuid[]) AS s(kind, id)
                 WHERE ce.owner_kind = s.kind
                   AND ce.owner_id = s.id
             )
               AND ce.seq > $3
               {horizon_clause}
               AND {edge_visibility}
             ORDER BY ce.seq ASC
             LIMIT $4"
    );
    // SQL-POLICY: fixed-fragment (horizon_clause / edge_visibility are fixed
    // literal fragments; every value — owners, cursor, limit, horizon — is bound)
    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(&read_owner_kinds)
        .bind(&read_owner_ids)
        .bind(after)
        .bind(i64::try_from(limit).unwrap_or(i64::MAX))
        .bind(world_kind)
        .bind(world_id);
    if let Some(horizon) = horizon {
        query = query.bind(horizon);
    }
    let rows = query.fetch_all(pool).await.map_err(map_err)?;

    let seqs: Vec<uuid::Uuid> = rows
        .iter()
        .map(|r| r.try_get("seq"))
        .collect::<Result<_, _>>()
        .map_err(map_err)?;

    // `hydrate_change_events_batch` always returns `seq DESC`; wake
    // consumers rely on the forward chronological order the query above
    // already established (`ORDER BY ce.seq ASC`), so restore it here.
    let mut events = hydrate_change_events_batch(pool, read_owners, &seqs).await?;
    events.sort_by_key(|event| event.seq);

    Ok(events
        .into_iter()
        .map(|event| ChangeEventForWake { event })
        .collect())
}

fn read_owner_columns(read_owners: &[OwnerRef]) -> (Vec<OwnerRefKind>, Vec<Option<uuid::Uuid>>) {
    crate::access::owner_columns::owner_arrays(read_owners)
}

/// One owner's change events in `(after, until]` for replay, oldest first.
///
/// # Errors
///
/// Returns [`StorageError`] when the change-event read fails.
pub async fn list_change_events_for_replay(
    pool: &PgPool,
    owner: &Owner,
    after: uuid::Uuid,
    until: Option<uuid::Uuid>,
    limit: usize,
) -> Result<Vec<ChangeEventForWake>, StorageError> {
    let (read_owner_kinds, read_owner_ids) = read_owner_columns(std::slice::from_ref(owner));
    let (world_kind, world_id) =
        crate::access::owner_columns::owner_binds(&proxima_core::access::world());
    let edge_visibility = edge_event_visibility_predicate(1, 2, 6, 7);
    let sql = format!(
        "SELECT ce.seq
             FROM proxima_core.change_event ce
             WHERE EXISTS (
                SELECT 1
                  FROM unnest($1::proxima_core.owner_kind[], $2::uuid[]) AS s(kind, id)
                 WHERE ce.owner_kind = s.kind
                   AND ce.owner_id = s.id
             )
               AND ce.seq > $3
               AND ($4::uuid IS NULL OR ce.seq <= $4)
               AND {edge_visibility}
             ORDER BY ce.seq ASC
             LIMIT $5"
    );
    // SQL-POLICY: fixed-fragment — `sql` only interpolates the fixed
    // `edge_event_visibility_predicate` fragment (positional `$N` binds
    // only); explicit proof since this call's line shifts with edits above.
    let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(&read_owner_kinds)
        .bind(&read_owner_ids)
        .bind(after)
        .bind(until)
        .bind(i64::try_from(limit).unwrap_or(i64::MAX))
        .bind(world_kind)
        .bind(world_id)
        .fetch_all(pool)
        .await
        .map_err(map_err)?;

    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let seq: uuid::Uuid = r.try_get("seq").map_err(map_err)?;
        if let Some(event) = hydrate_change_event(pool, std::slice::from_ref(owner), seq).await? {
            out.push(ChangeEventForWake { event });
        }
    }
    Ok(out)
}

/// Source-owned edge visibility for `change_event` edge rows, matching
/// `read_edges`: source readable and not (source World-readable and target
/// unreadable). Fact-entity endpoints resolve through their current head.
#[must_use]
pub(crate) fn edge_event_visibility_predicate(
    read_kinds_param: usize,
    read_ids_param: usize,
    world_kind_param: usize,
    world_id_param: usize,
) -> String {
    // A Fact-entity endpoint is an address that follows the head, so its
    // visibility is the visibility of whatever observation is current.
    let source_entity = "COALESCE(
        (SELECT fe.current_memory_id FROM proxima_core.fact_entities fe
          WHERE fe.fact_entity_id = ce.edge_source_id),
        ce.edge_source_id)";
    let target_entity = "COALESCE(
        (SELECT fe.current_memory_id FROM proxima_core.fact_entities fe
          WHERE fe.fact_entity_id = ce.edge_target_id),
        ce.edge_target_id)";
    format!(
        "(
                    ce.edge_kind IS NULL
                    OR (
                        EXISTS (
                            SELECT 1
                              FROM {eo_union} seo
                              JOIN unnest(${read_kinds_param}::proxima_core.owner_kind[], ${read_ids_param}::uuid[]) AS rs(kind, id)
                                ON seo.owner_kind = rs.kind
                               AND seo.owner_id IS NOT DISTINCT FROM rs.id
                             WHERE seo.entity_id = {source_entity}
                        )
                        AND NOT (
                            EXISTS (
                                SELECT 1
                                  FROM {eo_union} weo
                                 WHERE weo.entity_id = {source_entity}
                                   AND weo.owner_kind = ${world_kind_param}
                                   AND weo.owner_id IS NOT DISTINCT FROM ${world_id_param}
                            )
                            AND NOT EXISTS (
                                SELECT 1
                                  FROM {eo_union} teo
                                  JOIN unnest(${read_kinds_param}::proxima_core.owner_kind[], ${read_ids_param}::uuid[]) AS rt(kind, id)
                                    ON teo.owner_kind = rt.kind
                                   AND teo.owner_id IS NOT DISTINCT FROM rt.id
                                 WHERE teo.entity_id = {target_entity}
                            )
                        )
                    )
               )",
        eo_union = crate::verbs::query::entity_owner_union(),
    )
}

#[cfg(test)]
mod commit_horizon_tests {
    use super::commit_horizon_seq;
    use std::time::Duration;

    /// Build a `UUIDv7` stamped at `ms` with maximal (adversarial) low bytes,
    /// so the timestamp prefix alone orders it against the horizon.
    fn uuidv7_at_ms(ms: u64) -> uuid::Uuid {
        let ms_be = ms.to_be_bytes();
        let mut bytes = [0xFFu8; 16];
        bytes[0..6].copy_from_slice(&ms_be[2..8]);
        bytes[6] = 0x7F;
        uuid::Uuid::from_bytes(bytes)
    }

    #[test]
    fn zero_grace_disables_horizon() {
        assert!(commit_horizon_seq(1_000_000_000_000, Duration::ZERO).is_none());
    }

    #[test]
    fn horizon_withholds_recent_and_delivers_aged_seqs() {
        let now_ms = 1_000_000_000_000u64;
        let grace = Duration::from_secs(1);
        let horizon = commit_horizon_seq(now_ms, grace).expect("nonzero grace yields a horizon");

        // Stamped 5s ago (older than now-grace): below the horizon → delivered,
        // even with maximal low bytes.
        let aged = uuidv7_at_ms(now_ms - 5_000);
        assert!(
            aged < horizon,
            "aged seq must be below the low-water horizon"
        );

        // Stamped 100ms ago (newer than now-grace): at/above the horizon →
        // withheld until it ages past the grace window.
        let recent = uuidv7_at_ms(now_ms - 100);
        assert!(
            recent >= horizon,
            "too-recent seq must be withheld by the horizon"
        );
    }

    #[test]
    fn horizon_boundary_is_grace_milliseconds_back() {
        let now_ms = 2_000_000u64;
        let grace = Duration::from_millis(250);
        let horizon = commit_horizon_seq(now_ms, grace).unwrap();
        // Exactly at the boundary ms is withheld (>= horizon); one ms older is
        // delivered (< horizon).
        assert!(uuidv7_at_ms(now_ms - 250) >= horizon);
        assert!(uuidv7_at_ms(now_ms - 251) < horizon);
    }
}
