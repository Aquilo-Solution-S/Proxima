//! Fail-closed boot checks for the runtime `PostgreSQL` role.
//!
//! This module checks the structural part of the Proxima RLS contract.  It is
//! deliberately catalog-only: scope construction and the semantics of the
//! owner predicates belong to the request transaction and migration tests.

use proxima_core::StorageError;
use sqlx::PgPool;

const REQUIRED_POLICIES: [&str; 3] = [
    "proxima_owner_read",
    "proxima_owner_write",
    "proxima_platform",
];

#[derive(Debug, sqlx::FromRow)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "catalog projection mirrors independent PostgreSQL safety flags"
)]
struct RelationRow {
    oid: i64,
    schema_name: String,
    table_name: String,
    row_security: bool,
    force_row_security: bool,
    schema_create: bool,
    table_truncate: bool,
    owner_usage: bool,
    owner_set: bool,
    dangerous_set_role: bool,
}

#[derive(Debug, sqlx::FromRow)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "catalog projection mirrors independent PostgreSQL policy flags"
)]
struct PolicyRow {
    name: String,
    command: String,
    permissive: bool,
    using_expr: Option<String>,
    check_expr: Option<String>,
    platform_role_is_owner: bool,
    platform_role_is_public: bool,
    platform_targets_runtime: bool,
}

fn refused(reason: impl std::fmt::Display) -> StorageError {
    StorageError::Internal(format!("runtime RLS guard refused pool: {reason}"))
}

fn predicate_is_nontrivial(predicate: Option<&str>) -> bool {
    predicate.is_some_and(|value| {
        let normalized = value.trim().to_ascii_lowercase();
        !normalized.is_empty() && normalized != "true" && normalized != "(true)"
    })
}

async fn assert_policy_structure(
    pool: &PgPool,
    relations: &[RelationRow],
    reject_current_user_platform_policy: bool,
) -> Result<(), StorageError> {
    for relation in relations {
        let relation_name = format!("{}.{}", relation.schema_name, relation.table_name);
        if !relation.row_security || !relation.force_row_security {
            return Err(refused(format!(
                "{relation_name} does not have ENABLE and FORCE ROW LEVEL SECURITY"
            )));
        }
        let policies: Vec<PolicyRow> = sqlx::query_as(
            "SELECT p.polname AS name, p.polcmd::text AS command,
                    p.polpermissive AS permissive,
                    pg_get_expr(p.polqual, p.polrelid) AS using_expr,
                    pg_get_expr(p.polwithcheck, p.polrelid) AS check_expr,
                    (p.polroles = ARRAY[c.relowner]) AS platform_role_is_owner,
                    (0 = ANY(p.polroles)) AS platform_role_is_public,
                    ((SELECT oid FROM pg_roles WHERE rolname = current_user) = ANY(p.polroles))
                        AS platform_targets_runtime
               FROM pg_policy AS p JOIN pg_class AS c ON c.oid = p.polrelid
              WHERE p.polrelid::bigint = $1",
        )
        .bind(relation.oid)
        .fetch_all(pool)
        .await
        .map_err(|error| refused(format!("inspect policies on {relation_name}: {error}")))?;
        if policies.len() != REQUIRED_POLICIES.len()
            || policies.iter().any(|policy| {
                !REQUIRED_POLICIES.contains(&policy.name.as_str()) || !policy.permissive
            })
        {
            return Err(refused(format!(
                "{relation_name} has missing or extra RLS policies"
            )));
        }
        for policy in &policies {
            match policy.name.as_str() {
                "proxima_owner_read"
                    if policy.command == "r"
                        && predicate_is_nontrivial(policy.using_expr.as_deref()) => {}
                "proxima_owner_write"
                    if policy.command == "*"
                        && predicate_is_nontrivial(policy.using_expr.as_deref())
                        && predicate_is_nontrivial(policy.check_expr.as_deref()) => {}
                "proxima_platform"
                    if policy.command == "*"
                        && predicate_is_nontrivial(policy.using_expr.as_deref())
                        && predicate_is_nontrivial(policy.check_expr.as_deref())
                        && policy.platform_role_is_owner
                        && !policy.platform_role_is_public
                        && (!reject_current_user_platform_policy
                            || !policy.platform_targets_runtime) => {}
                _ => {
                    return Err(refused(format!(
                        "{relation_name} has an invalid {} policy",
                        policy.name
                    )));
                }
            }
        }
    }
    Ok(())
}

/// Assert the structural RLS contract for an owner-backed platform pool.
///
/// This is intentionally separate from [`assert_runtime_rls`]: a platform
/// role must own the tables so it can run maintenance, while the runtime role
/// must not. The check is used only after owner RLS activation.
pub(crate) async fn assert_platform_rls(
    pool: &PgPool,
    schemas: &[&str],
) -> Result<(), StorageError> {
    if schemas.is_empty() || schemas.iter().any(|schema| schema.trim().is_empty()) {
        return Err(refused("schema inventory is empty"));
    }
    let schema_names: Vec<String> = schemas.iter().map(|schema| (*schema).to_owned()).collect();
    let (_, superuser, bypass): (String, bool, bool) = sqlx::query_as(
        "SELECT current_user::text, r.rolsuper, r.rolbypassrls
           FROM pg_roles AS r WHERE r.rolname = current_user",
    )
    .fetch_one(pool)
    .await
    .map_err(|error| refused(format!("read platform role: {error}")))?;
    if superuser || bypass {
        return Err(refused("platform role must obey FORCE ROW LEVEL SECURITY"));
    }
    let relations: Vec<RelationRow> = sqlx::query_as(
        "SELECT c.oid::bigint AS oid,
                n.nspname AS schema_name, c.relname AS table_name,
                c.relrowsecurity AS row_security,
                c.relforcerowsecurity AS force_row_security,
                false AS schema_create, false AS table_truncate,
                pg_has_role(current_user, c.relowner, 'USAGE') AS owner_usage,
                false AS owner_set, false AS dangerous_set_role
           FROM pg_class AS c JOIN pg_namespace AS n ON n.oid = c.relnamespace
          WHERE n.nspname = ANY($1::text[]) AND c.relkind IN ('r', 'p')
          ORDER BY n.nspname, c.relname",
    )
    .bind(&schema_names)
    .fetch_all(pool)
    .await
    .map_err(|error| refused(format!("enumerate platform tables: {error}")))?;
    if relations.is_empty()
        || schema_names.iter().any(|schema| {
            !relations
                .iter()
                .any(|relation| relation.schema_name == *schema)
        })
        || relations.iter().any(|relation| !relation.owner_usage)
    {
        return Err(refused(
            "platform schema inventory or ownership is incomplete",
        ));
    }
    assert_policy_structure(pool, &relations, false).await
}

/// Assert that `pool` is safe to use as a runtime (non-migration) pool.
///
/// `schemas` is the complete composed schema set for this host.  The query
/// intentionally discovers tables from the catalog instead of accepting an
/// expected table list, so a newly-added sidecar is covered automatically.
/// Empty or unknown schema inventories fail closed.
///
/// # Errors
///
/// Returns [`StorageError::Internal`] when the runtime role or any discovered
/// table violates the RLS safety contract, or when catalog inspection fails.
pub async fn assert_runtime_rls(pool: &PgPool, schemas: &[&str]) -> Result<(), StorageError> {
    if schemas.is_empty() || schemas.iter().any(|schema| schema.trim().is_empty()) {
        return Err(refused("schema inventory is empty"));
    }
    let schema_names: Vec<String> = schemas.iter().map(|schema| (*schema).to_owned()).collect();

    let (role_name, is_superuser, bypass_rls): (String, bool, bool) = sqlx::query_as(
        "SELECT current_user::text, r.rolsuper, r.rolbypassrls
           FROM pg_roles AS r
          WHERE r.rolname = current_user",
    )
    .fetch_one(pool)
    .await
    .map_err(|error| refused(format!("read runtime role: {error}")))?;

    if is_superuser {
        return Err(refused(format!("runtime role {role_name} is a superuser")));
    }
    if bypass_rls {
        return Err(refused(format!("runtime role {role_name} has BYPASSRLS")));
    }

    let relations: Vec<RelationRow> = sqlx::query_as(
        "SELECT c.oid::bigint AS oid,
                n.nspname AS schema_name,
                c.relname AS table_name,
                c.relrowsecurity AS row_security,
                c.relforcerowsecurity AS force_row_security,
                has_schema_privilege(current_user, n.oid, 'CREATE') AS schema_create,
                has_table_privilege(current_user, c.oid, 'TRUNCATE') AS table_truncate,
                pg_has_role(current_user, c.relowner, 'USAGE') AS owner_usage,
                pg_has_role(current_user, c.relowner, 'SET') AS owner_set,
                EXISTS (
                    SELECT 1
                      FROM pg_roles AS target
                     WHERE target.oid <> (SELECT oid FROM pg_roles WHERE rolname = current_user)
                       AND (target.rolsuper OR target.rolbypassrls)
                       AND pg_has_role(current_user, target.oid, 'SET')
                ) AS dangerous_set_role
           FROM pg_class AS c
           JOIN pg_namespace AS n ON n.oid = c.relnamespace
          WHERE n.nspname = ANY($1::text[])
            AND c.relkind IN ('r', 'p')
          ORDER BY n.nspname, c.relname",
    )
    .bind(&schema_names)
    .fetch_all(pool)
    .await
    .map_err(|error| refused(format!("enumerate schema tables: {error}")))?;

    let discovered_schemas: Vec<&str> = relations
        .iter()
        .map(|relation| relation.schema_name.as_str())
        .collect();
    if schema_names
        .iter()
        .any(|schema| !discovered_schemas.contains(&schema.as_str()))
    {
        return Err(refused(
            "schema inventory contains an unknown or empty schema",
        ));
    }
    if relations.is_empty() {
        return Err(refused("schema inventory contains no base tables"));
    }

    for relation in &relations {
        let relation_name = format!("{}.{}", relation.schema_name, relation.table_name);
        if relation.schema_create {
            return Err(refused(format!(
                "runtime role can CREATE in {relation_name}"
            )));
        }
        if relation.table_truncate {
            return Err(refused(format!(
                "runtime role can TRUNCATE {relation_name}"
            )));
        }
        if relation.owner_usage || relation.owner_set {
            return Err(refused(format!(
                "runtime role has effective ownership of {relation_name}"
            )));
        }
        if relation.dangerous_set_role {
            return Err(refused(format!(
                "runtime role can SET ROLE to a superuser or BYPASSRLS role for {relation_name}"
            )));
        }
        if !relation.row_security || !relation.force_row_security {
            return Err(refused(format!(
                "{relation_name} does not have ENABLE and FORCE ROW LEVEL SECURITY"
            )));
        }
    }
    assert_policy_structure(pool, &relations, true).await
}

/// Detect whether the database has entered the owner-RLS epoch. This probe is
/// deliberately separate from the strict runtime guard so pre-RLS databases
/// retain the compatibility bridge.
///
/// # Errors
///
/// Returns [`StorageError::Internal`] when the catalog probe fails.
pub async fn owner_rls_enforced(pool: &PgPool, schemas: &[&str]) -> Result<bool, StorageError> {
    let epoch: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = 'proxima_core' AND c.relname = 'owner_rls_epoch')",
    ).fetch_one(pool).await.map_err(|error| StorageError::Internal(error.to_string()))?;
    if epoch {
        return Ok(true);
    }
    let flags: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = ANY($1::text[]) AND c.relkind IN ('r','p') AND (c.relrowsecurity OR c.relforcerowsecurity))",
    ).bind(schemas).fetch_one(pool).await.map_err(|error| StorageError::Internal(error.to_string()))?;
    Ok(flags)
}
