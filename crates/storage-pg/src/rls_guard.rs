//! Fail-closed boot checks for runtime and platform `PostgreSQL` roles.
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
    relation_kind: String,
    security_invoker: bool,
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
    reason = "catalog projection mirrors independent PostgreSQL routine safety flags"
)]
struct DefinerRow {
    name: String,
    owner_oid: i64,
    owner_name: String,
    owner_superuser: bool,
    owner_bypass_rls: bool,
    owner_usage: bool,
    owner_set: bool,
    platform_owner: bool,
    returns_trigger: bool,
    public_execute: bool,
    runtime_execute: bool,
    non_platform_execute: bool,
    settings: Option<Vec<String>>,
}

#[derive(Debug, sqlx::FromRow)]
struct NamespaceRow {
    name: String,
    platform_owner_oids: Vec<i64>,
    runtime_create: bool,
    non_platform_create: bool,
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
    StorageError::Internal(format!("RLS catalog guard refused pool: {reason}"))
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
    for relation in relations.iter().filter(|relation| relation.is_table()) {
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

impl RelationRow {
    fn is_table(&self) -> bool {
        matches!(self.relation_kind.as_str(), "r" | "p")
    }

    fn name(&self) -> String {
        format!("{}.{}", self.schema_name, self.table_name)
    }

    fn assert_supported(&self) -> Result<(), StorageError> {
        match self.relation_kind.as_str() {
            "m" => Err(refused(format!(
                "{} is an unsupported materialized view",
                self.name()
            ))),
            "f" => Err(refused(format!(
                "{} is an unsupported foreign table",
                self.name()
            ))),
            "v" if !self.security_invoker => Err(refused(format!(
                "{} must declare security_invoker=true",
                self.name()
            ))),
            _ => Ok(()),
        }
    }
}

async fn relations(pool: &PgPool, schemas: &[String]) -> Result<Vec<RelationRow>, StorageError> {
    sqlx::query_as(
        "SELECT c.oid::bigint AS oid,
                n.nspname AS schema_name, c.relname AS table_name,
                c.relkind::text AS relation_kind,
                COALESCE((SELECT option_value::boolean
                            FROM pg_options_to_table(c.reloptions)
                           WHERE option_name = 'security_invoker'), false) AS security_invoker,
                c.relrowsecurity AS row_security,
                c.relforcerowsecurity AS force_row_security,
                has_schema_privilege(current_user, n.oid, 'CREATE') AS schema_create,
                has_table_privilege(current_user, c.oid, 'TRUNCATE') AS table_truncate,
                pg_has_role(current_user, c.relowner, 'USAGE') AS owner_usage,
                pg_has_role(current_user, c.relowner, 'SET') AS owner_set,
                EXISTS (
                    SELECT 1 FROM pg_roles AS target
                     WHERE target.oid <> (SELECT oid FROM pg_roles WHERE rolname = current_user)
                       AND (target.rolsuper OR target.rolbypassrls)
                       AND pg_has_role(current_user, target.oid, 'SET')
                ) AS dangerous_set_role
           FROM pg_class AS c JOIN pg_namespace AS n ON n.oid = c.relnamespace
          WHERE n.nspname = ANY($1::text[]) AND c.relkind IN ('r', 'p', 'v', 'm', 'f')
          ORDER BY n.nspname, c.relname",
    )
    .bind(schemas)
    .fetch_all(pool)
    .await
    .map_err(|error| refused(format!("enumerate schema relations: {error}")))
}

fn assert_inventory(relations: &[RelationRow], schemas: &[String]) -> Result<(), StorageError> {
    for relation in relations {
        relation.assert_supported()?;
    }
    if let Some(schema) = schemas.iter().find(|schema| {
        !relations
            .iter()
            .any(|relation| relation.schema_name == **schema && relation.is_table())
    }) {
        return Err(refused(format!(
            "schema inventory contains an unknown or empty schema: {schema}"
        )));
    }
    Ok(())
}

fn search_path(settings: Option<&[String]>) -> Option<Vec<String>> {
    let paths: Vec<_> = settings?
        .iter()
        .filter_map(|setting| setting.strip_prefix("search_path="))
        .collect();
    let [path] = paths.as_slice() else {
        return None;
    };
    let mut entries = Vec::new();
    let mut entry = String::new();
    let mut quoted = false;
    let mut chars = path.chars().peekable();
    while let Some(character) = chars.next() {
        match character {
            '"' => {
                entry.push(character);
                if quoted && chars.peek() == Some(&'"') {
                    entry.push(chars.next()?);
                } else {
                    quoted = !quoted;
                }
            }
            ',' if !quoted => {
                entries.push(path_identifier(&entry)?);
                entry.clear();
            }
            _ => entry.push(character),
        }
    }
    if quoted {
        return None;
    }
    entries.push(path_identifier(&entry)?);
    Some(entries)
}

fn path_identifier(entry: &str) -> Option<String> {
    let entry = entry.trim();
    if let Some(quoted) = entry
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
    {
        let mut identifier = String::new();
        let mut chars = quoted.chars().peekable();
        while let Some(character) = chars.next() {
            if character == '"' && chars.next()? != '"' {
                return None;
            }
            identifier.push(character);
        }
        return (!identifier.is_empty()).then_some(identifier);
    }
    (!entry.is_empty()
        && entry
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '$')))
    .then(|| entry.to_ascii_lowercase())
}

fn path_is_pinned(path: &[String]) -> bool {
    path.split_last().is_some_and(|(last, schemas)| {
        last == "pg_temp"
            && schemas
                .iter()
                .all(|schema| !matches!(schema.as_str(), "$user" | "pg_temp"))
    })
}

fn assert_definer_path(
    function: &DefinerRow,
    namespaces: &[NamespaceRow],
    platform: bool,
) -> Result<(), StorageError> {
    let invalid = || {
        refused(format!(
            "{} must pin a trusted search_path ending with pg_temp",
            function.name
        ))
    };
    let path = search_path(function.settings.as_deref()).ok_or_else(invalid)?;
    if !path_is_pinned(&path) {
        return Err(invalid());
    }
    for name in &path[..path.len() - 1] {
        let Some(namespace) = namespaces.iter().find(|namespace| namespace.name == *name) else {
            return Err(invalid());
        };
        if (name != "pg_catalog" && !namespace.platform_owner_oids.contains(&function.owner_oid))
            || namespace.non_platform_create
            || (!platform && namespace.runtime_create)
        {
            return Err(refused(format!(
                "{} has an untrusted or runtime-writable search_path schema {name}",
                function.name
            )));
        }
    }
    Ok(())
}

async fn definers(pool: &PgPool, schemas: &[String]) -> Result<Vec<DefinerRow>, StorageError> {
    sqlx::query_as(
        "SELECT format('%I.%I(%s)', n.nspname, p.proname,
                       pg_get_function_identity_arguments(p.oid)) AS name,
                p.proowner::bigint AS owner_oid, owner.rolname AS owner_name,
                owner.rolsuper AS owner_superuser, owner.rolbypassrls AS owner_bypass_rls,
                pg_has_role(current_user, p.proowner, 'USAGE') AS owner_usage,
                pg_has_role(current_user, p.proowner, 'SET') AS owner_set,
                NOT EXISTS (
                    SELECT 1 FROM pg_class c JOIN pg_namespace schema ON schema.oid = c.relnamespace
                     WHERE schema.nspname = ANY($1::text[]) AND c.relkind IN ('r', 'p')
                       AND NOT pg_has_role(p.proowner, c.relowner, 'USAGE')
                ) AS platform_owner,
                p.prorettype = 'pg_catalog.trigger'::regtype AS returns_trigger,
                EXISTS (SELECT 1 FROM aclexplode(COALESCE(p.proacl, acldefault('f', p.proowner))) acl
                         WHERE acl.grantee = 0 AND acl.privilege_type = 'EXECUTE') AS public_execute,
                (has_function_privilege(current_user, p.oid, 'EXECUTE') OR EXISTS (
                    SELECT 1 FROM pg_roles target
                     WHERE pg_has_role(current_user, target.oid, 'SET')
                       AND has_function_privilege(target.oid, p.oid, 'EXECUTE')
                )) AS runtime_execute,
                EXISTS (SELECT 1 FROM aclexplode(COALESCE(p.proacl, acldefault('f', p.proowner))) acl
                         LEFT JOIN pg_roles grantee ON grantee.oid = acl.grantee
                         WHERE acl.privilege_type = 'EXECUTE'
                           AND (acl.grantee = 0 OR (
                               NOT grantee.rolsuper
                               AND NOT pg_has_role(grantee.oid, p.proowner, 'USAGE')
                               AND NOT pg_has_role(grantee.oid, p.proowner, 'SET')
                           ))) AS non_platform_execute,
                p.proconfig AS settings
           FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace
           JOIN pg_roles owner ON owner.oid = p.proowner
          WHERE n.nspname = ANY($1::text[]) AND p.prosecdef
          ORDER BY n.nspname, p.proname, p.oid",
    )
    .bind(schemas)
    .fetch_all(pool)
    .await
    .map_err(|error| refused(format!("enumerate SECURITY DEFINER functions: {error}")))
}

async fn definer_namespaces(
    pool: &PgPool,
    functions: &[DefinerRow],
) -> Result<Vec<NamespaceRow>, StorageError> {
    let owner_oids: Vec<_> = functions
        .iter()
        .filter(|function| {
            function.platform_owner && !function.owner_superuser && !function.owner_bypass_rls
        })
        .map(|function| function.owner_oid)
        .collect();
    sqlx::query_as(
        "SELECT n.nspname AS name,
                ARRAY(SELECT owner.oid::bigint FROM pg_roles owner
                       WHERE owner.oid::bigint = ANY($1::bigint[])
                         AND pg_has_role(owner.oid, n.nspowner, 'USAGE')) AS platform_owner_oids,
                (has_schema_privilege(current_user, n.oid, 'CREATE') OR EXISTS (
                    SELECT 1 FROM pg_roles target
                     WHERE pg_has_role(current_user, target.oid, 'SET')
                       AND has_schema_privilege(target.oid, n.oid, 'CREATE')
                )) AS runtime_create,
                (EXISTS (SELECT 1 FROM aclexplode(COALESCE(n.nspacl, acldefault('n', n.nspowner))) acl
                         LEFT JOIN pg_roles grantee ON grantee.oid = acl.grantee
                         WHERE acl.privilege_type = 'CREATE'
                           AND (acl.grantee = 0 OR (
                               NOT grantee.rolsuper
                               AND NOT pg_has_role(grantee.oid, n.nspowner, 'USAGE')
                               AND NOT pg_has_role(grantee.oid, n.nspowner, 'SET')
                           ))) OR EXISTS (
                    SELECT 1 FROM pg_roles caller
                     WHERE caller.rolcanlogin AND NOT caller.rolsuper
                       AND NOT EXISTS (
                           SELECT 1 FROM pg_roles owner
                            WHERE owner.oid::bigint = ANY($1::bigint[])
                              AND (pg_has_role(caller.oid, owner.oid, 'USAGE')
                                   OR pg_has_role(caller.oid, owner.oid, 'SET'))
                       )
                       AND EXISTS (
                           SELECT 1 FROM pg_roles target
                            WHERE pg_has_role(caller.oid, target.oid, 'SET')
                              AND has_schema_privilege(target.oid, n.oid, 'CREATE')
                       )
                )) AS non_platform_create
           FROM pg_namespace n",
    )
    .bind(&owner_oids)
    .fetch_all(pool)
    .await
    .map_err(|error| refused(format!("inspect definer search_path namespaces: {error}")))
}

/// Non-trigger definers expose only platform-owned authority. The platform
/// capability has no runtime-role binding, so it refuses EXECUTE grants outside
/// that authority and login roles that can assume search-path CREATE authority.
/// The runtime guard also checks the actual effective role.
async fn assert_definers(
    pool: &PgPool,
    schemas: &[String],
    platform: bool,
) -> Result<(), StorageError> {
    let functions = definers(pool, schemas).await?;
    if functions.is_empty() {
        return Ok(());
    }
    let namespaces = definer_namespaces(pool, &functions).await?;
    for function in &functions {
        if function.owner_superuser
            || function.owner_bypass_rls
            || !function.platform_owner
            || (platform && !function.owner_usage)
            || (!platform && (function.owner_usage || function.owner_set))
        {
            return Err(refused(format!(
                "{} must be owned by the safe platform role; owner is {}",
                function.name, function.owner_name
            )));
        }
        assert_definer_path(function, &namespaces, platform)?;
        if !function.returns_trigger {
            if function.public_execute {
                return Err(refused(format!("{} permits PUBLIC EXECUTE", function.name)));
            }
            if (platform && function.non_platform_execute)
                || (!platform && function.runtime_execute)
            {
                return Err(refused(format!(
                    "{} permits runtime EXECUTE outside platform authority",
                    function.name
                )));
            }
        }
    }
    Ok(())
}

/// Assert the structural RLS contract for an owner-backed platform pool.
///
/// This is intentionally separate from [`assert_runtime_rls`]: a platform
/// role must own tables and views, while the runtime role must not. Safe views
/// and definers are required before activation too; base-table policy checks
/// apply only once the database has entered the owner-RLS epoch.
pub(crate) async fn assert_platform_catalog(
    pool: &PgPool,
    schemas: &[&str],
    enforcing: bool,
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
    let relations = relations(pool, &schema_names).await?;
    assert_inventory(&relations, &schema_names)?;
    for relation in &relations {
        if !relation.owner_usage {
            return Err(refused(format!(
                "platform role does not own {}",
                relation.name()
            )));
        }
    }
    assert_definers(pool, &schema_names, true).await?;
    if enforcing {
        assert_policy_structure(pool, &relations, false).await?;
    }
    Ok(())
}

/// Assert that `pool` is safe to use as a runtime (non-migration) pool.
///
/// `schemas` is the complete composed schema set for this host.  The query
/// intentionally discovers tables, views and definers from the catalog, so a
/// newly-added sidecar or privileged object is covered automatically.
/// Empty or unknown schema inventories fail closed.
///
/// # Errors
///
/// Returns [`StorageError::Internal`] when the runtime role or any discovered
/// relation or definer violates the RLS safety contract, or inspection fails.
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

    let relations = relations(pool, &schema_names).await?;
    assert_inventory(&relations, &schema_names)?;

    for relation in &relations {
        let relation_name = format!("{}.{}", relation.schema_name, relation.table_name);
        if relation.schema_create {
            return Err(refused(format!(
                "runtime role can CREATE in {relation_name}"
            )));
        }
        if relation.is_table() && relation.table_truncate {
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
    }
    assert_policy_structure(pool, &relations, true).await?;
    assert_definers(pool, &schema_names, false).await
}

/// Detect whether the database has entered the owner-RLS epoch. This probe is
/// separate from the strict runtime guard so migration tooling can inspect
/// a pre-activation schema before applying the enforcing migration.
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

#[cfg(test)]
mod tests {
    use super::{path_is_pinned, search_path};

    #[test]
    fn search_path_preserves_quoted_identifiers_and_commas() {
        let settings = vec![
            "work_mem=4MB".to_owned(),
            "search_path=pg_catalog, \"Case,Schema\", \"quote\"\"schema\", pg_temp".to_owned(),
        ];
        assert_eq!(
            search_path(Some(&settings)),
            Some(vec![
                "pg_catalog".to_owned(),
                "Case,Schema".to_owned(),
                "quote\"schema".to_owned(),
                "pg_temp".to_owned(),
            ])
        );
    }

    #[test]
    fn search_path_rejects_missing_ambiguous_or_malformed_settings() {
        assert_eq!(search_path(None), None);
        for setting in [
            "work_mem=4MB",
            "search_path=",
            "search_path=pg_catalog,,pg_temp",
            "search_path=\"unclosed,pg_temp",
            "search_path=\"unexpected\"quote\",pg_temp",
        ] {
            assert_eq!(search_path(Some(&[setting.to_owned()])), None, "{setting}");
        }
        assert_eq!(
            search_path(Some(&[
                "search_path=pg_catalog,pg_temp".to_owned(),
                "search_path=public,pg_temp".to_owned(),
            ])),
            None
        );
    }

    #[test]
    fn pinned_path_rejects_dynamic_user_and_early_temp_aliases() {
        for value in [
            "pg_catalog",
            "\"$user\",pg_temp",
            "pg_temp,pg_catalog,pg_temp",
        ] {
            let path = search_path(Some(&[format!("search_path={value}")])).unwrap();
            assert!(!path_is_pinned(&path), "{value}");
        }
        for value in ["pg_temp", "pg_catalog,pg_temp", "\"trusted\",pg_temp"] {
            let path = search_path(Some(&[format!("search_path={value}")])).unwrap();
            assert!(path_is_pinned(&path), "{value}");
        }
        assert!(!path_is_pinned(&[]));
    }
}
