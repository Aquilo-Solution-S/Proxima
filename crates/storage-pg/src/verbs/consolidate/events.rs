use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use proxima_core::read_models::ChangeEventForWake;
use proxima_core::{Owner, OwnerRef, StorageError};
use sqlx::Row;
use sqlx::{PgConnection, PgPool};

use crate::error::map_err;

/// Env var (milliseconds) enabling the commit-safety grace window on the
/// forward change-event cursor. Unset/`0` = disabled (default).
const COMMIT_GRACE_ENV: &str = "PROXIMA_CHANGE_EVENT_COMMIT_GRACE_MS";

/// Configured commit-safety grace, read once from [`COMMIT_GRACE_ENV`].
///
/// `announce.seq` is a `UUIDv7` stamped at INSERT time, so a slow
/// writer can commit a *smaller* seq after a faster writer's larger seq is
/// already visible. A forward cursor (`seq > after`) that advanced past the
/// larger seq would then skip the smaller one forever. When a positive grace
/// is configured, the read withholds events whose seq timestamp is newer than
/// `now - grace`, so a slow committer whose commit latency is below the grace
/// is not skipped once the cursor advances.
///
/// Default off: consumers rely on immediate write-then-read visibility.
/// Enabling it trades up to `grace` of wake latency for skip-safety, and is
/// correct only while commit latency stays below `grace`.
///
/// # Errors
///
/// [`StorageError::Unavailable`] when the variable is set to something that
/// is not a whole number of milliseconds. Deliberately not read as "unset":
/// unset and `50ms` both used to resolve to `Duration::ZERO`, so a typo
/// silently disabled the very skip-safety it was written to turn on — and
/// because off IS the default, nothing downstream ever looked wrong.
fn configured_commit_grace() -> Result<Duration, StorageError> {
    static GRACE: OnceLock<Result<Duration, String>> = OnceLock::new();
    GRACE
        .get_or_init(|| {
            // `env_value` is the workspace's one rule for "set to
            // something": trimmed, and blank reads as unset.
            let Some(raw) = proxima_core::env_value(&proxima_core::process_env, COMMIT_GRACE_ENV)
            else {
                return Ok(Duration::ZERO);
            };
            raw.parse::<u64>().map(Duration::from_millis).map_err(|_| {
                format!("{COMMIT_GRACE_ENV}={raw} is not a whole number of milliseconds")
            })
        })
        .as_ref()
        .copied()
        .map_err(|message| StorageError::Unavailable(message.clone()))
}

/// `UUIDv7` low-water horizon for a wall-clock `grace`: the smallest v7 uuid
/// stamped at `now_ms - grace`. Any `announce.seq` strictly below it was
/// stamped more than `grace` ago; comparing `seq < horizon` therefore
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
    let mut connection = pool.acquire().await.map_err(crate::error::map_err)?;
    list_change_events_after_on_connection(&mut connection, read_owners, after, limit).await
}

/// # Errors
/// Returns storage errors from query execution or invalid stored data.
pub async fn list_change_events_after_on_connection(
    connection: &mut PgConnection,
    read_owners: &[OwnerRef],
    after: uuid::Uuid,
    limit: usize,
) -> Result<Vec<ChangeEventForWake>, StorageError> {
    if read_owners.is_empty() {
        return Ok(Vec::new());
    }
    let owners: Vec<uuid::Uuid> = read_owners
        .iter()
        .copied()
        .map(OwnerRef::stored_owner_id)
        .collect();
    let horizon = commit_horizon_seq(now_unix_ms(), configured_commit_grace()?);
    let clause = if horizon.is_some() {
        "AND seq < $4"
    } else {
        ""
    };
    let sql = format!(
        "SELECT seq FROM proxima_core.announce WHERE owner_id = ANY($1::uuid[]) AND seq > $2 {clause} ORDER BY seq ASC LIMIT $3"
    );
    // SQL-POLICY: fixed-fragment
    let mut q = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(&owners)
        .bind(after)
        .bind(i64::try_from(limit).unwrap_or(i64::MAX));
    if let Some(h) = horizon {
        q = q.bind(h);
    }
    let rows = q.fetch_all(&mut *connection).await.map_err(map_err)?;
    let seqs: Vec<uuid::Uuid> = rows
        .iter()
        .map(|r| r.try_get("seq"))
        .collect::<Result<_, _>>()
        .map_err(map_err)?;
    let mut events = crate::change_event::hydrate_change_events_batch_on_connection(
        connection,
        read_owners,
        &seqs,
    )
    .await?;
    events.sort_by_key(|e| e.seq);
    Ok(events
        .into_iter()
        .map(|event| ChangeEventForWake { event })
        .collect())
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
    let mut connection = pool.acquire().await.map_err(crate::error::map_err)?;
    list_change_events_for_replay_on_connection(&mut connection, owner, after, until, limit).await
}

/// # Errors
/// Returns storage errors from query execution or invalid stored data.
pub async fn list_change_events_for_replay_on_connection(
    connection: &mut PgConnection,
    owner: &Owner,
    after: uuid::Uuid,
    until: Option<uuid::Uuid>,
    limit: usize,
) -> Result<Vec<ChangeEventForWake>, StorageError> {
    let rows = sqlx::query("SELECT seq FROM proxima_core.announce WHERE owner_id = $1 AND seq > $2 AND ($3::uuid IS NULL OR seq <= $3) ORDER BY seq ASC LIMIT $4").bind(owner.stored_owner_id()).bind(after).bind(until).bind(i64::try_from(limit).unwrap_or(i64::MAX)).fetch_all(&mut *connection).await.map_err(map_err)?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let seq: uuid::Uuid = row.try_get("seq").map_err(map_err)?;
        if let Some(event) = crate::change_event::hydrate_change_event_on_connection(
            connection,
            std::slice::from_ref(owner),
            seq,
        )
        .await?
        {
            out.push(ChangeEventForWake { event });
        }
    }
    Ok(out)
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
