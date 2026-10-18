//! Test helpers for the example flavor: plain functions, not tools.
//!
//! Behind `cfg(any(test, feature = "testkit"))`. A test and tutorial
//! fixture, not a product.

use proxima_core::{AuthzContext, Engine, OwnerId, OwnerScope, ProtocolError};
use uuid::Uuid;

use crate::NoteV1;

/// Static SQL: register one notebook for its owner, on an owner-scoped
/// transaction.
const REGISTER_NOTEBOOK_SQL: &str =
    "INSERT INTO proxima_example.notebooks (notebook_id, owner_id, name) VALUES ($1, $2, $3)";

/// Register a notebook named `name` for `owner` and return its id.
///
/// # Errors
///
/// Storage errors from the owner-scoped transaction or the insert.
pub async fn register_notebook(
    pool: &sqlx::PgPool,
    scope: &OwnerScope,
    owner: OwnerId,
    name: &str,
) -> Result<Uuid, Box<dyn std::error::Error + Send + Sync>> {
    let notebook_id = Uuid::now_v7();
    let mut tx = proxima::host::begin_owner_transaction(pool, scope).await?;
    sqlx::query(REGISTER_NOTEBOOK_SQL)
        .bind(notebook_id)
        .bind(owner.into_inner())
        .bind(name)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(notebook_id)
}

/// Remember one note for `owner` through the Engine and return its memory
/// id. The Engine's scope fence and liveness probe run, as on every write.
///
/// # Errors
///
/// The Engine's refusal or storage error.
pub async fn remember_note(
    engine: &Engine,
    authz: &AuthzContext,
    owner: OwnerId,
    note: &NoteV1,
) -> Result<proxima_core::MemoryId, ProtocolError> {
    let outcome = engine
        .ingest_fact(
            authz,
            proxima::FactWrite::new(owner, "proxima-example/testkit", note),
        )
        .await?;
    Ok(outcome.memory_id)
}

/// Boot the example flavor as a single-owner app on a split-role database.
///
/// # Errors
///
/// The boot's config, storage or migration error.
pub async fn build_engine(
    runtime_url: &str,
    platform_url: &str,
    owner: OwnerId,
) -> Result<proxima::BuiltProxima, proxima::ProximaError> {
    proxima::Proxima::<crate::ExampleFlavor>::app()
        .database_url(runtime_url)
        .store_number(0)
        .platform_database_url(platform_url)
        .owner(owner)
        .allow_insecure_single_owner()
        .served_surface(proxima::ServedSurface::All)
        .build()
        .await
}

/// Every example row filed under one notebook, oldest admission first, at
/// most `$2`. Both sidecars carry `notebook_id`; the notebook registry row
/// itself is retired separately, under the scope fence.
const FIND_NOTEBOOK_ROWS_SQL: &str = "\
SELECT t FROM (
    SELECT t FROM proxima_example.note_v1 WHERE notebook_id = $1
    UNION ALL
    SELECT t FROM proxima_example.digest_v1 WHERE notebook_id = $1
) AS filed
 ORDER BY t
 LIMIT $2";

/// The registry row, locked one statement after the fence. A test that
/// holds this row parks the erase exactly where the fence is already held.
const NOTEBOOK_EXISTS_SQL: &str = "\
SELECT notebook_id FROM proxima_example.notebooks
 WHERE owner_id = $1 AND notebook_id = $2
   FOR UPDATE";

/// The registry row, without a lock: the erase's up-front existence check.
const NOTEBOOK_REGISTERED_SQL: &str = "\
SELECT notebook_id FROM proxima_example.notebooks
 WHERE owner_id = $1 AND notebook_id = $2";

/// Retire the registry row.
const DELETE_NOTEBOOK_SQL: &str = "\
DELETE FROM proxima_example.notebooks
 WHERE owner_id = $1 AND notebook_id = $2";

/// Bound every lock wait of the retirement: a stuck lock becomes a
/// retryable conflict instead of a hang.
const ERASE_LOCK_TIMEOUT_SQL: &str = "SET LOCAL lock_timeout = '5s'";

/// Whole attempts the erase allows while it fails transiently.
const ERASE_ATTEMPTS: usize = 3;

/// What [`erase_notebook_with_scope`] erased.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotebookEraseReceipt {
    /// Memory versions the committed pages erased, across attempts.
    pub memories_deleted: u64,
    /// Whether the registry row was deleted.
    pub notebook_record_deleted: bool,
}

/// Why one attempt of [`erase_notebook_with_scope`] stopped.
enum Attempt {
    /// Worth re-running: a serialization conflict, a lock timeout, a
    /// retryable storage refusal, or a row committed after the last page.
    Transient(String),
    /// Final.
    Fatal(String),
}

impl From<sqlx::Error> for Attempt {
    fn from(err: sqlx::Error) -> Self {
        let transient = matches!(
            &err,
            sqlx::Error::Database(db)
                if matches!(db.code().as_deref(), Some("40001" | "40P01" | "55P03"))
        );
        if transient {
            Self::Transient(err.to_string())
        } else {
            Self::Fatal(err.to_string())
        }
    }
}

impl From<ProtocolError> for Attempt {
    fn from(err: ProtocolError) -> Self {
        Self::Fatal(err.to_string())
    }
}

/// Erase one notebook the way a flavor's scope erase runs: the notebook's
/// series through core's [`proxima_core::UnitOfWork::erase_own_series`], a
/// page at a time, then the registry row under the notebook scope fence
/// ([`proxima::flavor::lock_scope_fence_exclusive_tx`]), taken BEFORE the
/// last read so a same-notebook write either committed before it (and is
/// found, and the attempt re-runs) or waits and then finds the scope
/// unregistered.
///
/// `pool` is the runtime pool; the owner scope comes from `authz`.
///
/// # Errors
///
/// A missing owner scope, an unknown notebook, the erase verb's refusal, or
/// a storage error that outlived the retry budget.
pub async fn erase_notebook_with_scope(
    engine: &Engine,
    authz: &AuthzContext,
    pool: &sqlx::PgPool,
    owner: OwnerId,
    notebook_id: Uuid,
) -> Result<NotebookEraseReceipt, Box<dyn std::error::Error + Send + Sync>> {
    let scope = authz
        .owner_scope()
        .ok_or("authenticated owner scope required")?
        .clone();
    let mut memories_deleted = 0_u64;
    let mut attempt = 1;
    loop {
        match erase_notebook_once(engine, authz, pool, &scope, owner, notebook_id).await {
            Ok((erased, deleted)) => {
                return Ok(NotebookEraseReceipt {
                    memories_deleted: memories_deleted + erased,
                    notebook_record_deleted: deleted,
                });
            }
            Err((erased, Attempt::Transient(_))) if attempt < ERASE_ATTEMPTS => {
                memories_deleted += erased;
                attempt += 1;
            }
            Err((_, Attempt::Transient(message) | Attempt::Fatal(message))) => {
                return Err(message.into());
            }
        }
    }
}

/// One attempt: check, erase page by page, retire. Returns the versions
/// erased alongside the outcome, since a committed page stays erased even
/// when a later step fails.
async fn erase_notebook_once(
    engine: &Engine,
    authz: &AuthzContext,
    pool: &sqlx::PgPool,
    scope: &OwnerScope,
    owner: OwnerId,
    notebook_id: Uuid,
) -> Result<(u64, bool), (u64, Attempt)> {
    let mut erased = 0_u64;
    let registered = {
        let mut tx = proxima::host::begin_owner_transaction(pool, scope)
            .await
            .map_err(|err| (0, Attempt::Fatal(err.to_string())))?;
        let row: Option<(Uuid,)> = sqlx::query_as(NOTEBOOK_REGISTERED_SQL)
            .bind(owner.into_inner())
            .bind(notebook_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|err| (0, err.into()))?;
        tx.commit().await.map_err(|err| (0, err.into()))?;
        row.is_some()
    };
    if !registered {
        return Err((
            0,
            Attempt::Fatal(format!("notebook {notebook_id} not found")),
        ));
    }
    let mut page = proxima::flavor::MAX_ERASE_SERIES_PER_CALL;
    loop {
        let found = find_notebook_rows(pool, scope, notebook_id, page)
            .await
            .map_err(|err| (erased, err))?;
        if found.is_empty() {
            break;
        }
        match erase_page(engine, authz, owner, &found).await {
            Ok(0) => {
                return Err((
                    erased,
                    Attempt::Transient(format!("notebook {notebook_id}: the page erased nothing")),
                ));
            }
            Ok(versions) => erased += versions,
            Err(None) if page > 1 => page /= 2,
            Err(None) => {
                return Err((
                    erased,
                    Attempt::Fatal("a single series is over the cap".into()),
                ));
            }
            Err(Some(err)) => return Err((erased, err)),
        }
    }
    let deleted = retire_notebook(pool, scope, owner, notebook_id)
        .await
        .map_err(|err| (erased, err))?;
    Ok((erased, deleted))
}

/// Up to `limit` admissions filed under the notebook, as this owner sees
/// them.
async fn find_notebook_rows(
    pool: &sqlx::PgPool,
    scope: &OwnerScope,
    notebook_id: Uuid,
    limit: usize,
) -> Result<Vec<Uuid>, Attempt> {
    let mut tx = proxima::host::begin_owner_transaction(pool, scope)
        .await
        .map_err(|err| Attempt::Fatal(err.to_string()))?;
    let found: Vec<Uuid> = sqlx::query_scalar(FIND_NOTEBOOK_ROWS_SQL)
        .bind(notebook_id)
        .bind(i64::try_from(limit).unwrap_or(i64::MAX))
        .fetch_all(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(found)
}

/// Erase one page of series in its own unit. `Err(None)` is the verb's
/// over-cap refusal (the unit rolled back with nothing deleted).
async fn erase_page(
    engine: &Engine,
    authz: &AuthzContext,
    owner: OwnerId,
    found: &[Uuid],
) -> Result<u64, Option<Attempt>> {
    use proxima::flavor::{EraseMode, SeriesEraseError, SeriesEraseRefusalKind, SeriesSelection};

    let seeds = found
        .iter()
        .map(|t| proxima_core::MemoryId::new(*t))
        .collect();
    let mut unit = engine
        .unit_of_work(authz)
        .await
        .map_err(|err| Some(err.into()))?;
    let receipt = match unit
        .erase_own_series(
            crate::contract::FLAVOR_ID,
            owner,
            SeriesSelection::Ids(seeds),
            EraseMode::Erase,
        )
        .await
    {
        Ok(receipt) => receipt,
        Err(SeriesEraseError::Refused(refusal))
            if refusal.kind == SeriesEraseRefusalKind::OverCap =>
        {
            return Err(None);
        }
        Err(SeriesEraseError::Retryable(message)) => {
            return Err(Some(Attempt::Transient(message)));
        }
        Err(err) => return Err(Some(Attempt::Fatal(err.to_string()))),
    };
    unit.commit().await.map_err(|err| Some(err.into()))?;
    Ok(receipt.versions_erased)
}

/// Delete the registry row once nothing is filed under it, under the
/// notebook scope fence, exclusively, taken BEFORE the last read.
async fn retire_notebook(
    pool: &sqlx::PgPool,
    scope: &OwnerScope,
    owner: OwnerId,
    notebook_id: Uuid,
) -> Result<bool, Attempt> {
    let mut tx = proxima::host::begin_owner_transaction(pool, scope)
        .await
        .map_err(|err| Attempt::Fatal(err.to_string()))?;
    sqlx::query(ERASE_LOCK_TIMEOUT_SQL)
        .execute(&mut *tx)
        .await?;
    proxima::flavor::lock_scope_fence_exclusive_tx(
        &mut tx,
        crate::payload::NOTEBOOK_SCOPE,
        &owner,
        notebook_id,
    )
    .await
    .map_err(|err| match err {
        proxima_core::StorageError::Retryable(message) => Attempt::Transient(message),
        other => Attempt::Fatal(other.to_string()),
    })?;
    let exists: Option<(Uuid,)> = sqlx::query_as(NOTEBOOK_EXISTS_SQL)
        .bind(owner.into_inner())
        .bind(notebook_id)
        .fetch_optional(&mut *tx)
        .await?;
    if exists.is_none() {
        return Err(Attempt::Fatal(format!("notebook {notebook_id} not found")));
    }
    let left: Vec<Uuid> = sqlx::query_scalar(FIND_NOTEBOOK_ROWS_SQL)
        .bind(notebook_id)
        .bind(1_i64)
        .fetch_all(&mut *tx)
        .await?;
    if let Some(t) = left.first() {
        return Err(Attempt::Transient(format!(
            "notebook {notebook_id}: {t} was committed after the last page"
        )));
    }
    let deleted = sqlx::query(DELETE_NOTEBOOK_SQL)
        .bind(owner.into_inner())
        .bind(notebook_id)
        .execute(&mut *tx)
        .await?
        .rows_affected()
        > 0;
    tx.commit().await?;
    Ok(deleted)
}
