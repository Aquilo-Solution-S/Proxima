//! Lexical text-search configurations: the per-row languages writes stamp,
//! and the database default reads fall back to.
//!
//! The language a row is stamped with must (a) exist as a text-search
//! configuration — the shape was validated core-side, existence only
//! the catalog can answer — and (b) be recorded in
//! `proxima_core.lexical_languages`, because membership there is what
//! makes a language *searchable*: the query builder ORs one tsquery per
//! entry (see migration 0014). Both happen inside the write
//! transaction, so a stamped row and its language's searchability
//! commit together.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::{Mutex, OnceLock};

use proxima_core::StorageError;
use sqlx::{PgPool, Postgres, Transaction};
use tokio::sync::OnceCell;

use crate::PgTuning;
use crate::error::map_err;
use crate::pg_ident::PgIdent;

/// Verify `language` names an existing text-search configuration and
/// record it in the active-language set.
///
/// # Errors
///
/// `ConstraintViolation` when the configuration does not exist in this
/// database's catalog.
pub(crate) async fn register_lexical_language_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    language: &str,
) -> Result<(), StorageError> {
    verify_lexical_config_in_tx(tx, language).await?;
    hold_lexical_language_in_tx(tx, language).await
}

/// Configurations this process has already found in the catalog.
///
/// Membership only ever grows: the catalog probe below asks whether a
/// text-search configuration exists, and no Proxima verb creates or drops
/// one, so within a process run there is nothing to invalidate. What the
/// cache gives up is the *sentence*, not the safety — a configuration an
/// operator DROPs after this process first verified it fails on the
/// `::regconfig` cast in the registration insert instead of on the guard,
/// so the error reads as a cast failure rather than as the catalog
/// explanation below.
static VERIFIED_LEXICAL_CONFIGS: OnceLock<Mutex<BTreeSet<String>>> = OnceLock::new();

/// [`register_lexical_language_in_tx`] with the catalog probe answered from
/// the process cache after its first success.
///
/// The registration insert and its `FOR KEY SHARE` hold are NOT cached and
/// never can be: the lock is what serializes this write against
/// `lexical_language_forget`, so it has to be taken inside every
/// transaction that stamps a row.
///
/// # Errors
///
/// Same as [`register_lexical_language_in_tx`].
pub(crate) async fn register_lexical_language_cached_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    language: &str,
) -> Result<(), StorageError> {
    let cache = VERIFIED_LEXICAL_CONFIGS.get_or_init(|| Mutex::new(BTreeSet::new()));
    let verified = cache
        .lock()
        .map_err(|_| StorageError::Internal("lexical configuration cache poisoned".into()))?
        .contains(language);
    if !verified {
        verify_lexical_config_in_tx(tx, language).await?;
        cache
            .lock()
            .map_err(|_| StorageError::Internal("lexical configuration cache poisoned".into()))?
            .insert(language.to_string());
    }
    hold_lexical_language_in_tx(tx, language).await
}

async fn verify_lexical_config_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    language: &str,
) -> Result<(), StorageError> {
    // No `to_regconfig` exists (the `to_reg*` helpers skip text-search
    // configurations), so existence is answered from the catalog directly.
    // Unqualified names resolve through the search_path EXCLUDING the
    // session's temp schema — PostgreSQL's text-search lookup (and thus
    // the `::regconfig` cast in the INSERT below) skips pg_temp, so the
    // guard must too or a temp-schema configuration passes the check and
    // then fails the cast as an unclassified internal error. Qualified
    // names resolve against their named schema.
    let known: bool = sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1
               FROM pg_ts_config c
               JOIN pg_namespace n ON n.oid = c.cfgnamespace
              WHERE (position('.' in $1) = 0
                     AND c.cfgname = $1
                     AND n.nspname = ANY (current_schemas(true))
                     AND n.oid <> pg_my_temp_schema())
                 OR (position('.' in $1) > 0
                     AND n.nspname = split_part($1, '.', 1)
                     AND c.cfgname = split_part($1, '.', 2)))",
    )
    .bind(language)
    .fetch_one(tx.as_mut())
    .await
    .map_err(map_err)?;
    if !known {
        return Err(StorageError::ConstraintViolation(format!(
            "unknown text-search configuration {language:?}: it is not in this database's \
             catalog (PostgreSQL ships e.g. 'english', 'german', 'simple'; custom \
             configurations must be CREATEd before rows can be stamped with them)"
        )));
    }
    Ok(())
}

async fn hold_lexical_language_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    language: &str,
) -> Result<(), StorageError> {
    // Register AND hold FOR KEY SHARE until this write transaction ends.
    // `lexical_language_forget` takes FOR UPDATE on this row before its
    // referencing-rows scan; the conflicting locks serialize the two, so
    // forget cannot certify a language unreferenced while a write stamping
    // it is still in flight. The loop covers the one gap: a forget that
    // committed between our upsert (no lock when the row already exists)
    // and our lock attempt deletes the row — re-registering then succeeds
    // because the deleter is gone.
    for _ in 0..3 {
        sqlx::query(
            "INSERT INTO proxima_core.lexical_languages (config)
             VALUES ($1::regconfig)
             ON CONFLICT (config) DO NOTHING",
        )
        .bind(language)
        .execute(tx.as_mut())
        .await
        .map_err(map_err)?;
        let locked: Option<i32> = sqlx::query_scalar(
            "SELECT 1 FROM proxima_core.lexical_languages
              WHERE config = $1::regconfig
                FOR KEY SHARE",
        )
        .bind(language)
        .fetch_optional(tx.as_mut())
        .await
        .map_err(map_err)?;
        if locked.is_some() {
            return Ok(());
        }
    }
    Err(StorageError::Retryable(format!(
        "could not register lexical language {language:?}: concurrent \
         lexical_language_forget kept removing it"
    )))
}

/// How the lexical branch names the default text-search configuration when
/// nothing is cached: the function itself, resolved by the server on every
/// query. This is the shipped spelling.
pub(crate) const LEXICAL_CONFIG_CALL: &str = "proxima_core.lexical_config()";

/// `proxima_core.lexical_config()` resolved once per storage handle.
///
/// The function is `IMMUTABLE` and returns a literal, so within one process
/// its answer changes only when an operator runs
/// `proxima_core.set_lexical_config()` — which rewrites every stored
/// `search_tsv` and is a maintenance act, not a request. A handle that
/// resolved before such a change keeps emitting the old configuration until
/// the process restarts; that is what this cache trades away.
///
/// Per handle rather than per process: one test binary connects to many
/// databases, and the default configuration is a property of the database,
/// not of the binary reading it.
#[derive(Debug, Clone, Default)]
pub(crate) struct LexicalConfigCache(Arc<OnceCell<String>>);

impl LexicalConfigCache {
    /// The text-search configuration expression the lexical branch emits.
    ///
    /// With [`PgTuning::static_lookup_cache`] off this is
    /// [`LEXICAL_CONFIG_CALL`] and no query is issued, so the branch text is
    /// byte-for-byte the shipped one. With it on the configuration is read
    /// once and emitted as a literal, which is what removes the function
    /// call from the two fallback tsqueries.
    ///
    /// # Errors
    ///
    /// `Unavailable` when the configuration cannot be read, or when the
    /// catalog answers a name that is not a plain identifier.
    pub(crate) async fn text_search_config(
        &self,
        pool: &PgPool,
        tuning: &PgTuning,
    ) -> Result<&str, StorageError> {
        if !tuning.static_lookup_cache {
            return Ok(LEXICAL_CONFIG_CALL);
        }
        self.0
            .get_or_try_init(|| resolve_lexical_config(pool))
            .await
            .map(String::as_str)
    }
}

/// Read the default configuration and spell it as a `regconfig` literal.
///
/// The name comes from the catalog, not from a caller, and is validated
/// anyway: [`PgIdent::table`] admits exactly the unquoted, optionally
/// schema-qualified identifiers, so nothing that reaches the literal can
/// close it. A name that needs quoting refuses rather than being escaped —
/// the flag is a measurement arm, and an arm that silently falls back to the
/// shipped spelling reports "no effect" for "never applied".
async fn resolve_lexical_config(pool: &PgPool) -> Result<String, StorageError> {
    let resolved: String = sqlx::query_scalar("SELECT proxima_core.lexical_config()::text")
        .fetch_one(pool)
        .await
        .map_err(map_err)?;
    let ident = PgIdent::table(&resolved).map_err(|_| {
        StorageError::Unavailable(format!(
            "PROXIMA_PG_STATIC_LOOKUP_CACHE cannot inline the default text-search \
             configuration {resolved:?}: it is not a plain identifier"
        ))
    })?;
    Ok(format!("'{}'::regconfig", ident.as_str()))
}

#[cfg(test)]
mod tests {
    use super::{LEXICAL_CONFIG_CALL, LexicalConfigCache};
    use crate::PgTuning;

    fn unreachable_pool() -> sqlx::PgPool {
        sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(std::time::Duration::from_millis(1))
            .connect_lazy("postgres://proxima-lexical-config-test.invalid/none")
            .expect("a lazy pool does not connect")
    }

    /// Flag off is the shipped spelling and costs nothing: the pool below
    /// never resolves, so a read that went to the catalog would fail here.
    #[tokio::test]
    async fn the_uncached_arm_names_the_function_without_reading_it() {
        let cache = LexicalConfigCache::default();

        let config = cache
            .text_search_config(&unreachable_pool(), &PgTuning::default())
            .await
            .expect("the uncached arm issues no query");

        assert_eq!(config, LEXICAL_CONFIG_CALL);
        assert_eq!(config, "proxima_core.lexical_config()");
    }

    /// Flag on does read, and says so when it cannot.
    #[tokio::test]
    async fn the_cached_arm_reads_the_catalog() {
        let cache = LexicalConfigCache::default();
        let tuning = PgTuning {
            static_lookup_cache: true,
            ..PgTuning::default()
        };

        assert!(
            cache
                .text_search_config(&unreachable_pool(), &tuning)
                .await
                .is_err(),
            "the cached arm must resolve the configuration, not assume it"
        );
    }
}
