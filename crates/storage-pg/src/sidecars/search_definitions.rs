use std::sync::Arc;

use proxiima_core::StorageError;
use sha2::{Digest, Sha384};
use sqlx::{PgConnection, Postgres, Row, Transaction};

use super::PgSidecarRegistryFrozen;

use crate::error::map_err as catalog_error;
use crate::search_candidates::{self, CandidateDefinition};

/// The generated candidate definitions for one frozen spiice composition.
///
/// The artifact list is created only from the frozen registry. Callers cannot
/// inject SQL into this value; migration and preflight consume the same list.
#[derive(Debug, Clone, Default)]
pub(super) struct SearchDefinitions {
    definitions: Arc<[CandidateDefinition]>,
    schemas: Arc<[String]>,
}

impl SearchDefinitions {
    pub(crate) fn from_contracts(
        contracts: &[&'static proxiima_core::spiice::SpiiceContract],
        schemas: Arc<[String]>,
    ) -> Result<Self, StorageError> {
        let mut definitions = Vec::new();
        for contract in contracts {
            if contract.ordinal == 0 {
                continue;
            }
            definitions.extend(search_candidates::candidate_definitions(contract)?);
        }
        definitions.sort_by(|left, right| left.signature.cmp(&right.signature));
        Ok(Self {
            definitions: definitions.into(),
            schemas,
        })
    }

    pub(crate) fn definitions(&self) -> &[CandidateDefinition] {
        &self.definitions
    }

    /// Apply the current generated candidate functions on the held migration
    /// connection. The caller owns the transaction and migration lock.
    pub async fn apply_on(&self, connection: &mut PgConnection) -> Result<(), StorageError> {
        self.ensure_no_unexpected_functions(connection).await?;
        for definition in self.definitions() {
            sqlx::raw_sql(sqlx::AssertSqlSafe(definition.artifact.forward.as_str()))
                .execute(&mut *connection)
                .await
                .map_err(catalog_error)?;
        }
        self.ensure_on(connection).await
    }

    /// Verify that every generated candidate function exists with its current
    /// stable signature and security-definer property. This issues no DDL.
    pub async fn ensure_on(&self, connection: &mut PgConnection) -> Result<(), StorageError> {
        self.ensure_no_unexpected_functions(connection).await?;
        for definition in self.definitions() {
            let row = sqlx::query(
                "SELECT p.prosrc, p.prosecdef, l.lanname, p.proconfig,
                        p.proisstrict, p.provolatile::text, p.proparallel::text,
                        r.rolname AS owner_name,
                        EXISTS (
                            SELECT 1
                              FROM aclexplode(COALESCE(p.proacl, acldefault('f', p.proowner))) acl
                             WHERE acl.grantee = 0 AND acl.privilege_type = 'EXECUTE'
                        ) AS public_execute,
                        pg_get_function_result(p.oid) AS result_shape
                   FROM pg_proc p
                   JOIN pg_namespace n ON n.oid = p.pronamespace
                   JOIN pg_language l ON l.oid = p.prolang
                   JOIN pg_roles r ON r.oid = p.proowner
                  WHERE p.oid = to_regprocedure($1)",
            )
            .bind(&definition.signature)
            .fetch_optional(&mut *connection)
            .await
            .map_err(catalog_error)?;
            let Some(row) = row else {
                return Err(StorageError::ConstraintViolation(format!(
                    "generated candidate function {} is missing",
                    definition.signature
                )));
            };
            let body: String = row.try_get("prosrc").map_err(catalog_error)?;
            let definer: bool = row.try_get("prosecdef").map_err(catalog_error)?;
            let language: String = row.try_get("lanname").map_err(catalog_error)?;
            let config: Option<Vec<String>> = row.try_get("proconfig").map_err(catalog_error)?;
            let owner_name: String = row.try_get("owner_name").map_err(catalog_error)?;
            let public_execute: bool = row.try_get("public_execute").map_err(catalog_error)?;
            let result_shape: String = row.try_get("result_shape").map_err(catalog_error)?;
            let expected_result = format!("TABLE({})", definition.kind.result_columns());
            let strict: bool = row.try_get("proisstrict").map_err(catalog_error)?;
            let volatility: String = row.try_get("provolatile").map_err(catalog_error)?;
            let parallel: String = row.try_get("proparallel").map_err(catalog_error)?;
            if !definer
                || strict
                || volatility != "v"
                || parallel != "u"
                || language != "plpgsql"
                || owner_name != "proxiima_search"
                || public_execute
                || !has_safe_search_path(config.as_deref())
                || result_shape != expected_result
            {
                return Err(StorageError::ConstraintViolation(format!(
                    "generated candidate function {} has an unexpected security configuration",
                    definition.signature
                )));
            }
            if body != definition.body {
                return Err(StorageError::ConstraintViolation(format!(
                    "generated candidate function {} has a stale body",
                    definition.signature
                )));
            }
        }
        Ok(())
    }

    async fn ensure_no_unexpected_functions(
        &self,
        connection: &mut PgConnection,
    ) -> Result<(), StorageError> {
        let expected: Vec<&str> = self
            .definitions()
            .iter()
            .map(|definition| definition.signature.as_str())
            .collect();
        let unexpected: Option<String> = sqlx::query_scalar(
            "SELECT format('%I.%I(%s)', n.nspname, p.proname, oidvectortypes(p.proargtypes))
               FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
              WHERE n.nspname = ANY($1::text[])
                AND p.proname IN ('search_lexical_candidates', 'search_substring_candidates')
                AND NOT EXISTS (SELECT 1 FROM unnest($2::text[]) expected(signature)
                                 WHERE to_regprocedure(expected.signature) = p.oid)
              ORDER BY n.nspname, p.proname, p.oid LIMIT 1",
        )
        .bind(self.schemas.as_ref())
        .bind(&expected)
        .fetch_optional(&mut *connection)
        .await
        .map_err(catalog_error)?;
        if let Some(signature) = unexpected {
            return Err(StorageError::ConstraintViolation(format!(
                "unexpected generated candidate overload {signature}; an explicit schema migration is required"
            )));
        }

        Ok(())
    }

    #[must_use]
    pub(crate) fn fingerprint(&self) -> [u8; 48] {
        let mut hasher = Sha384::new();
        for schema in self.schemas.iter() {
            hasher.update(schema.as_bytes());
            hasher.update([0]);
        }
        for definition in self.definitions() {
            hasher.update(definition.artifact.forward.as_bytes());
            hasher.update([0]);
        }
        hasher.finalize().into()
    }
}

fn has_safe_search_path(config: Option<&[String]>) -> bool {
    config.is_some_and(|values| {
        values.len() == 1
            && matches!(
                values[0].as_str(),
                "search_path=pg_catalog, pg_temp" | "search_path=pg_catalog,pg_temp"
            )
    })
}

impl PgSidecarRegistryFrozen {
    /// Install generated spiice candidate functions in the caller's migration transaction.
    ///
    /// # Errors
    /// Refuses unexpected signatures and propagates catalog or installation failures.
    pub async fn apply_search_definitions(
        &self,
        transaction: &mut Transaction<'_, Postgres>,
    ) -> Result<(), StorageError> {
        self.search_definitions.apply_on(transaction.as_mut()).await
    }

    /// Validate the current composed search program without changing the database.
    ///
    /// # Errors
    /// Refuses missing, stale or incompatible function definitions and privileges.
    pub async fn ensure_search_definitions(
        &self,
        connection: &mut PgConnection,
    ) -> Result<(), StorageError> {
        self.search_definitions.ensure_on(connection).await
    }

    /// Fingerprint of generated definitions for database test-template identity.
    #[must_use]
    pub fn search_definitions_fingerprint(&self) -> [u8; 48] {
        self.search_definitions.fingerprint()
    }
}
