//! Catalog refusals for objects that can change whose RLS/SQL authority applies.

use proxima_core::{Credentials, GroupId, OwnerRef, OwnerRoles, Role, UserId};
use proxima_storage_pg::{PgPlatformScope, assert_runtime_rls, begin_owner_transaction};
use sqlx::{AssertSqlSafe, PgPool};
use uuid::Uuid;

use super::{SyntheticVerifier, cleanup, execute, quoted_identifier, runtime_pool, setup};

async fn assert_guards_refuse_named(
    runtime: &PgPool,
    platform: &PgPool,
    schema: &str,
    object: &str,
    reason: &str,
) {
    let expected_name = format!("{schema}.{object}");
    let runtime_message = assert_runtime_rls(runtime, &[schema])
        .await
        .expect_err("runtime guard must refuse the object")
        .to_string();
    let platform_message = PgPlatformScope::new(platform.clone(), &[schema])
        .await
        .expect_err("platform guard must refuse the object")
        .to_string();
    for message in [runtime_message, platform_message] {
        assert!(message.contains(&expected_name), "{message}");
        assert!(message.contains(reason), "{message}");
    }
}

async fn assert_guards_accept(runtime: &PgPool, platform: &PgPool, schema: &str) {
    assert_runtime_rls(runtime, &[schema]).await.unwrap();
    PgPlatformScope::new(platform.clone(), &[schema])
        .await
        .unwrap();
}

#[tokio::test]
async fn platform_guard_completes_with_one_connection() {
    let (database, admin, runtime, owner, runtime_role, schema, password) = setup().await;
    let options = proxima_pg_testkit::db_url(&database)
        .parse::<sqlx::postgres::PgConnectOptions>()
        .unwrap()
        .username(&owner)
        .password(&password);
    let platform = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        PgPlatformScope::new(platform.clone(), &[&schema]),
    )
    .await
    .expect("platform catalog checks must reuse their single checked-out connection")
    .unwrap();
    runtime.close().await;
    platform.close().await;
    cleanup(&database, admin, &owner, &runtime_role).await;
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one isolated view leak and repair scenario"
)]
async fn platform_definer_view_leaks_forged_scope_but_invoker_view_is_owner_scoped() {
    let (database, admin, runtime, owner, runtime_role, schema, password) = setup().await;
    let platform = runtime_pool(&database, &owner, &password).await;
    let name = quoted_identifier(&schema);
    let owners = [Uuid::now_v7(), Uuid::now_v7()];
    for row_owner in owners {
        // SQL-POLICY: fixed-fragment — fixture-generated, quoted schema identifier.
        sqlx::query(AssertSqlSafe(format!(
            "INSERT INTO {name}.memory(id, owner_id) VALUES ($1, $2)"
        )))
        .bind(row_owner)
        .bind(row_owner)
        .execute(&admin)
        .await
        .unwrap();
    }
    execute(
        &admin,
        // SQL-POLICY: fixed-fragment — quoted fixture identifiers and closed SQL clauses.
        &format!("CREATE VIEW {name}.memory_view AS SELECT owner_id FROM {name}.memory"),
    )
    .await;
    execute(
        &admin,
        &format!(
            "ALTER VIEW {name}.memory_view OWNER TO {}",
            quoted_identifier(&owner)
        ),
    )
    .await;
    execute(
        &admin,
        &format!(
            "GRANT SELECT ON {name}.memory_view TO {}",
            quoted_identifier(&runtime_role)
        ),
    )
    .await;

    let mut forged = runtime.begin().await.unwrap();
    sqlx::query("SET LOCAL app.proxima_scope = 'platform'")
        .execute(&mut *forged)
        .await
        .unwrap();
    // SQL-POLICY: fixed-fragment — fixture-generated, quoted schema identifier.
    let direct: i64 =
        sqlx::query_scalar(AssertSqlSafe(format!("SELECT count(*) FROM {name}.memory")))
            .fetch_one(&mut *forged)
            .await
            .unwrap();
    // SQL-POLICY: fixed-fragment — fixture-generated, quoted schema identifier.
    let through_view: i64 = sqlx::query_scalar(AssertSqlSafe(format!(
        "SELECT count(*) FROM {name}.memory_view"
    )))
    .fetch_one(&mut *forged)
    .await
    .unwrap();
    assert_eq!(
        direct, 0,
        "the runtime cannot use the platform table policy"
    );
    assert_eq!(
        through_view, 2,
        "the definer view supplies the platform owner's identity"
    );
    forged.rollback().await.unwrap();
    assert_guards_refuse_named(
        &runtime,
        &platform,
        &schema,
        "memory_view",
        "security_invoker=true",
    )
    .await;

    execute(
        &admin,
        // SQL-POLICY: fixed-fragment — quoted fixture identifiers and closed SQL clauses.
        &format!("ALTER VIEW {name}.memory_view SET (security_invoker = true)"),
    )
    .await;
    assert_guards_accept(&runtime, &platform, &schema).await;
    let verifier = SyntheticVerifier {
        roles: OwnerRoles::for_subject(
            UserId::new(Uuid::now_v7()),
            [(OwnerRef::Group(GroupId::new(owners[0])), Role::viewer())],
        )
        .unwrap(),
    };
    let authz = proxima_core::authenticate(&verifier, &Credentials::Bearer("view-test".into()))
        .await
        .unwrap();
    let mut scoped = begin_owner_transaction(&runtime, authz.owner_scope().unwrap())
        .await
        .unwrap();
    // SQL-POLICY: fixed-fragment — fixture-generated, quoted schema identifier.
    let visible: Vec<Uuid> = sqlx::query_scalar(AssertSqlSafe(format!(
        "SELECT owner_id FROM {name}.memory_view"
    )))
    .fetch_all(&mut *scoped)
    .await
    .unwrap();
    assert_eq!(visible, vec![owners[0]]);
    scoped.rollback().await.unwrap();

    execute(
        &admin,
        // SQL-POLICY: fixed-fragment — quoted fixture identifiers and closed SQL clauses.
        &format!("ALTER VIEW {name}.memory_view OWNER TO CURRENT_USER"),
    )
    .await;
    let refused = PgPlatformScope::new(platform.clone(), &[&schema])
        .await
        .expect_err("an invoker view left with a legacy owner must refuse platform boot")
        .to_string();
    assert!(
        refused.contains(&format!("{schema}.memory_view")),
        "{refused}"
    );
    assert!(refused.contains("platform role does not own"), "{refused}");
    runtime.close().await;
    platform.close().await;
    cleanup(&database, admin, &owner, &runtime_role).await;
}

#[tokio::test]
async fn catalog_census_refuses_materialized_views_and_foreign_tables_without_runtime_grants() {
    let (database, admin, runtime, owner, runtime_role, schema, password) = setup().await;
    let platform = runtime_pool(&database, &owner, &password).await;
    let name = quoted_identifier(&schema);
    execute(
        &admin,
        // SQL-POLICY: fixed-fragment — quoted fixture identifiers and closed SQL clauses.
        &format!("CREATE MATERIALIZED VIEW {name}.cached_memory AS SELECT owner_id FROM {name}.memory WITH NO DATA"),
    )
    .await;
    execute(
        &admin,
        &format!(
            "ALTER MATERIALIZED VIEW {name}.cached_memory OWNER TO {}",
            quoted_identifier(&owner)
        ),
    )
    .await;
    assert_guards_refuse_named(
        &runtime,
        &platform,
        &schema,
        "cached_memory",
        "unsupported materialized view",
    )
    .await;
    execute(
        &admin,
        // SQL-POLICY: fixed-fragment — quoted fixture identifiers and closed SQL clauses.
        &format!("DROP MATERIALIZED VIEW {name}.cached_memory"),
    )
    .await;

    // No handler is needed: this test inspects the catalog and performs no foreign I/O.
    execute(&admin, "CREATE FOREIGN DATA WRAPPER fixture_fdw").await;
    execute(
        &admin,
        "CREATE SERVER fixture_server FOREIGN DATA WRAPPER fixture_fdw",
    )
    .await;
    execute(
        &admin,
        // SQL-POLICY: fixed-fragment — quoted fixture identifiers and closed SQL clauses.
        &format!("CREATE FOREIGN TABLE {name}.remote_memory(id uuid, owner_id uuid) SERVER fixture_server"),
    )
    .await;
    execute(
        &admin,
        &format!(
            "ALTER FOREIGN TABLE {name}.remote_memory OWNER TO {}",
            quoted_identifier(&owner)
        ),
    )
    .await;
    assert_guards_refuse_named(
        &runtime,
        &platform,
        &schema,
        "remote_memory",
        "unsupported foreign table",
    )
    .await;
    // SQL-POLICY: fixed-fragment — quoted fixture identifiers and closed SQL clauses.
    execute(&admin, &format!("DROP FOREIGN TABLE {name}.remote_memory")).await;
    assert_guards_accept(&runtime, &platform, &schema).await;
    runtime.close().await;
    platform.close().await;
    cleanup(&database, admin, &owner, &runtime_role).await;
}

async fn install_definer(admin: &PgPool, schema: &str, owner: &str, runtime_role: &str) {
    let name = quoted_identifier(schema);
    execute(
        admin,
        // SQL-POLICY: fixed-fragment — quoted fixture identifiers and closed SQL clauses.
        &format!("CREATE FUNCTION {name}.owner_reader() RETURNS integer LANGUAGE sql SECURITY DEFINER SET search_path = pg_catalog, {name}, pg_temp AS 'SELECT 1'"),
    )
    .await;
    execute(
        admin,
        &format!(
            "ALTER FUNCTION {name}.owner_reader() OWNER TO {}",
            quoted_identifier(owner)
        ),
    )
    .await;
    execute(
        admin,
        &format!(
            "REVOKE ALL ON FUNCTION {name}.owner_reader() FROM PUBLIC, {}",
            quoted_identifier(runtime_role)
        ),
    )
    .await;
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "definer refusal matrix shares an isolated database"
)]
async fn definer_function_guards_check_owner_search_path_and_execute_grants() {
    let (database, admin, runtime, owner, runtime_role, schema, password) = setup().await;
    let platform = runtime_pool(&database, &owner, &password).await;
    let name = quoted_identifier(&schema);
    let function = format!("{name}.owner_reader()");
    let bypass = format!("rls_definer_bypass_{}", Uuid::now_v7().simple());
    let inherited = format!("rls_definer_exec_{}", Uuid::now_v7().simple());
    let set_only = format!("rls_definer_set_{}", Uuid::now_v7().simple());
    execute(
        &admin,
        &format!(
            "CREATE ROLE {} NOSUPERUSER BYPASSRLS",
            quoted_identifier(&bypass)
        ),
    )
    .await;
    install_definer(&admin, &schema, &owner, &runtime_role).await;
    assert_guards_accept(&runtime, &platform, &schema).await;

    for unsafe_owner in [
        "CURRENT_USER".to_owned(),
        quoted_identifier(&bypass),
        quoted_identifier(&runtime_role),
    ] {
        execute(
            &admin,
            // SQL-POLICY: fixed-fragment — quoted fixture identifiers and closed SQL clauses.
            &format!("ALTER FUNCTION {function} OWNER TO {unsafe_owner}"),
        )
        .await;
        assert_guards_refuse_named(
            &runtime,
            &platform,
            &schema,
            "owner_reader",
            "platform role",
        )
        .await;
        execute(
            &admin,
            &format!(
                "ALTER FUNCTION {function} OWNER TO {}",
                quoted_identifier(&owner)
            ),
        )
        .await;
    }

    execute(
        &admin,
        // SQL-POLICY: fixed-fragment — quoted fixture identifiers and closed SQL clauses.
        &format!("ALTER FUNCTION {function} RESET search_path"),
    )
    .await;
    assert_guards_refuse_named(&runtime, &platform, &schema, "owner_reader", "search_path").await;
    let writable = format!("writable_{}", Uuid::now_v7().simple());
    let writable_name = quoted_identifier(&writable);
    execute(
        &admin,
        &format!(
            "CREATE SCHEMA {writable_name} AUTHORIZATION {}",
            quoted_identifier(&owner)
        ),
    )
    .await;
    execute(
        &admin,
        &format!(
            "GRANT CREATE ON SCHEMA {writable_name} TO {}",
            quoted_identifier(&runtime_role)
        ),
    )
    .await;
    for unsafe_path in [
        "pg_temp".to_owned(),
        format!("pg_catalog, {name}"),
        format!("pg_temp, pg_catalog, {name}"),
        format!("pg_catalog, {writable_name}, pg_temp"),
    ] {
        execute(
            &admin,
            // SQL-POLICY: fixed-fragment — quoted fixture identifiers and closed SQL clauses.
            &format!("ALTER FUNCTION {function} SET search_path = {unsafe_path}"),
        )
        .await;
        assert_guards_refuse_named(&runtime, &platform, &schema, "owner_reader", "search_path")
            .await;
    }
    execute(
        &admin,
        // SQL-POLICY: fixed-fragment — quoted fixture identifiers and closed SQL clauses.
        &format!("ALTER FUNCTION {function} SET search_path = pg_catalog, {name}, pg_temp"),
    )
    .await;
    assert_guards_accept(&runtime, &platform, &schema).await;

    for grantee in ["PUBLIC".to_owned(), quoted_identifier(&runtime_role)] {
        execute(
            &admin,
            &format!("GRANT EXECUTE ON FUNCTION {function} TO {grantee}"),
        )
        .await;
        assert_guards_refuse_named(&runtime, &platform, &schema, "owner_reader", "EXECUTE").await;
        execute(
            &admin,
            &format!("REVOKE EXECUTE ON FUNCTION {function} FROM {grantee}"),
        )
        .await;
    }
    for (grantee, inherit) in [(&inherited, true), (&set_only, false)] {
        execute(
            &admin,
            // SQL-POLICY: fixed-fragment — quoted fixture identifiers and closed SQL clauses.
            &format!(
                "CREATE ROLE {} NOSUPERUSER NOBYPASSRLS",
                quoted_identifier(grantee)
            ),
        )
        .await;
        let inherit_clause = if inherit { "TRUE" } else { "FALSE" };
        execute(
            &admin,
            &format!(
                "GRANT {} TO {} WITH INHERIT {inherit_clause}, SET TRUE",
                quoted_identifier(grantee),
                quoted_identifier(&runtime_role)
            ),
        )
        .await;
        execute(
            &admin,
            &format!(
                "GRANT EXECUTE ON FUNCTION {function} TO {}",
                quoted_identifier(grantee)
            ),
        )
        .await;
        let inherited_execute: bool =
            sqlx::query_scalar("SELECT has_function_privilege($1, $2, 'EXECUTE')")
                .bind(&runtime_role)
                .bind(format!("{schema}.owner_reader()"))
                .fetch_one(&admin)
                .await
                .unwrap();
        assert_eq!(inherited_execute, inherit);
        assert_guards_refuse_named(&runtime, &platform, &schema, "owner_reader", "EXECUTE").await;
        execute(
            &admin,
            &format!(
                "REVOKE EXECUTE ON FUNCTION {function} FROM {}",
                quoted_identifier(grantee)
            ),
        )
        .await;
        execute(
            &admin,
            &format!(
                "REVOKE {} FROM {}",
                quoted_identifier(grantee),
                quoted_identifier(&runtime_role)
            ),
        )
        .await;
    }
    assert_guards_accept(&runtime, &platform, &schema).await;
    runtime.close().await;
    platform.close().await;
    cleanup(&database, admin, &owner, &runtime_role).await;
    let control = PgPool::connect(&proxima_pg_testkit::admin_url())
        .await
        .unwrap();
    for role in [&bypass, &inherited, &set_only] {
        // SQL-POLICY: fixed-fragment — quoted fixture identifiers and closed SQL clauses.
        execute(&control, &format!("DROP ROLE {}", quoted_identifier(role))).await;
    }
    control.close().await;
}

#[tokio::test]
async fn trigger_definer_executes_without_a_direct_grant_and_exempts_only_execute_acl() {
    let (database, admin, runtime, owner, runtime_role, schema, password) = setup().await;
    let platform = runtime_pool(&database, &owner, &password).await;
    let name = quoted_identifier(&schema);
    let function = format!("{name}.owner_bridge()");
    execute(
        &admin,
        // SQL-POLICY: fixed-fragment — quoted fixture identifiers and closed SQL clauses.
        &format!("CREATE FUNCTION {function} RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, {name}, pg_temp AS $$ BEGIN RETURN NEW; END $$"),
    )
    .await;
    execute(
        &admin,
        &format!(
            "ALTER FUNCTION {function} OWNER TO {}",
            quoted_identifier(&owner)
        ),
    )
    .await;
    // SQL-POLICY: fixed-fragment — quoted fixture identifiers and closed SQL clauses.
    execute(&admin, &format!("CREATE TRIGGER owner_bridge BEFORE INSERT ON {name}.sidecar FOR EACH ROW EXECUTE FUNCTION {function}")).await;
    execute(
        &admin,
        &format!(
            "REVOKE ALL ON FUNCTION {function} FROM PUBLIC, {}",
            quoted_identifier(&runtime_role)
        ),
    )
    .await;
    let runtime_execute: bool =
        sqlx::query_scalar("SELECT has_function_privilege($1, $2, 'EXECUTE')")
            .bind(&runtime_role)
            .bind(format!("{schema}.owner_bridge()"))
            .fetch_one(&admin)
            .await
            .unwrap();
    assert!(!runtime_execute);
    assert_guards_accept(&runtime, &platform, &schema).await;
    let row_owner = Uuid::now_v7();
    let verifier = SyntheticVerifier {
        roles: OwnerRoles::for_subject(
            UserId::new(Uuid::now_v7()),
            [(OwnerRef::Group(GroupId::new(row_owner)), Role::editor())],
        )
        .unwrap(),
    };
    let authz = proxima_core::authenticate(&verifier, &Credentials::Bearer("trigger-test".into()))
        .await
        .unwrap();
    let mut transaction = begin_owner_transaction(&runtime, authz.owner_scope().unwrap())
        .await
        .unwrap();
    // SQL-POLICY: fixed-fragment — fixture-generated, quoted schema identifier.
    sqlx::query(AssertSqlSafe(format!(
        "INSERT INTO {name}.sidecar(id, owner_id) VALUES ($1, $1)"
    )))
    .bind(row_owner)
    .execute(&mut *transaction)
    .await
    .expect("a trigger remains callable after direct EXECUTE is revoked");
    transaction.commit().await.unwrap();
    execute(
        &admin,
        &format!(
            "GRANT EXECUTE ON FUNCTION {function} TO PUBLIC, {}",
            quoted_identifier(&runtime_role)
        ),
    )
    .await;
    assert_guards_accept(&runtime, &platform, &schema).await;
    execute(
        &admin,
        // SQL-POLICY: fixed-fragment — quoted fixture identifiers and closed SQL clauses.
        &format!("ALTER FUNCTION {function} RESET search_path"),
    )
    .await;
    assert_guards_refuse_named(&runtime, &platform, &schema, "owner_bridge", "search_path").await;
    execute(
        &admin,
        // SQL-POLICY: fixed-fragment — quoted fixture identifiers and closed SQL clauses.
        &format!("ALTER FUNCTION {function} SET search_path = pg_catalog, {name}, pg_temp"),
    )
    .await;
    execute(
        &admin,
        &format!(
            "ALTER FUNCTION {function} OWNER TO {}",
            quoted_identifier(&runtime_role)
        ),
    )
    .await;
    assert_guards_refuse_named(
        &runtime,
        &platform,
        &schema,
        "owner_bridge",
        "platform role",
    )
    .await;
    runtime.close().await;
    platform.close().await;
    cleanup(&database, admin, &owner, &runtime_role).await;
}

#[tokio::test]
async fn definer_search_path_refuses_set_only_access_to_a_trusted_schema_owner() {
    let (database, admin, runtime, owner, runtime_role, schema, password) = setup().await;
    let platform = runtime_pool(&database, &owner, &password).await;
    let suffix = Uuid::now_v7().simple().to_string();
    let schema_owner = format!("rls_namespace_owner_{suffix}");
    let external_schema = format!("rls_external_{suffix}");
    let external_name = quoted_identifier(&external_schema);
    let function = format!("{}.path_bridge()", quoted_identifier(&schema));
    execute(
        &admin,
        &format!(
            "CREATE ROLE {} NOSUPERUSER NOBYPASSRLS",
            quoted_identifier(&schema_owner)
        ),
    )
    .await;
    execute(
        &admin,
        &format!(
            "GRANT {} TO {} WITH INHERIT TRUE, SET TRUE",
            quoted_identifier(&schema_owner),
            quoted_identifier(&owner)
        ),
    )
    .await;
    execute(
        &admin,
        &format!(
            "CREATE SCHEMA {external_name} AUTHORIZATION {}",
            quoted_identifier(&schema_owner)
        ),
    )
    .await;
    execute(
        &admin,
        // SQL-POLICY: fixed-fragment — quoted fixture schemas and fixed trigger definition.
        &format!("CREATE FUNCTION {function} RETURNS trigger LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog, {external_name}, pg_temp AS $$ BEGIN RETURN NEW; END $$"),
    )
    .await;
    execute(
        &admin,
        &format!(
            "ALTER FUNCTION {function} OWNER TO {}",
            quoted_identifier(&owner)
        ),
    )
    .await;
    execute(
        &admin,
        &format!(
            "REVOKE ALL ON FUNCTION {function} FROM PUBLIC, {}",
            quoted_identifier(&runtime_role)
        ),
    )
    .await;
    assert_guards_accept(&runtime, &platform, &schema).await;
    execute(
        &admin,
        &format!(
            "GRANT {} TO {} WITH INHERIT FALSE, SET TRUE",
            quoted_identifier(&schema_owner),
            quoted_identifier(&runtime_role)
        ),
    )
    .await;
    let (can_create, can_set_owner): (bool, bool) =
        sqlx::query_as("SELECT has_schema_privilege($1, $2, 'CREATE'), pg_has_role($1, $3, 'SET')")
            .bind(&runtime_role)
            .bind(&external_schema)
            .bind(&schema_owner)
            .fetch_one(&admin)
            .await
            .unwrap();
    assert!(!can_create, "SET-only membership must not inherit CREATE");
    assert!(
        can_set_owner,
        "the runtime can assume the schema owner's CREATE authority"
    );
    assert_guards_refuse_named(
        &runtime,
        &platform,
        &schema,
        "path_bridge",
        "runtime-writable search_path",
    )
    .await;
    runtime.close().await;
    platform.close().await;
    cleanup(&database, admin, &owner, &runtime_role).await;
    let control = PgPool::connect(&proxima_pg_testkit::admin_url())
        .await
        .unwrap();
    execute(
        &control,
        // SQL-POLICY: fixed-fragment — uniquely generated, quoted fixture role.
        &format!("DROP ROLE {}", quoted_identifier(&schema_owner)),
    )
    .await;
    control.close().await;
}
