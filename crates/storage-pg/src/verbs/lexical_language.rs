//! Per-row lexical language support for memory writes.
//!
//! The language a row is stamped with must (a) exist as a text-search
//! configuration — the shape was validated core-side, existence only
//! the catalog can answer — and (b) be recorded in
//! `proxima_core.lexical_languages`, because membership there is what
//! makes a language *searchable*: the query builder ORs one tsquery per
//! entry (see migration 0014). Both happen inside the write
//! transaction, so a stamped row and its language's searchability
//! commit together.

use proxima_core::StorageError;
use sqlx::{Postgres, Transaction};

use crate::error::map_err;

/// Verify `language` names an existing text-search configuration and
/// record it in the active-language set.
///
/// # Errors
///
/// `ConstraintViolation` when the configuration does not exist in this
/// database's catalog.
#[allow(dead_code)]
pub(crate) async fn register_lexical_language_in_tx(
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
