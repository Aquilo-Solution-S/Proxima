//! Canonical expressions preserve independently seeded owner/kind outcomes.
//! Raw transaction settings deliberately cover asymmetric SQL-boundary scopes.

use sqlx::{PgConnection, PgPool, Row};
use uuid::Uuid;

const SETTINGS: [&str; 10] = [
    "app.proxima_scope",
    "app.owner",
    "app.read_abstraction",
    "app.read_perspective",
    "app.read_goal",
    "app.write_owner",
    "app.write_abstraction",
    "app.write_perspective",
    "app.write_goal",
    "app.manage_owner",
];
const HELPER: &str = "proxima_core.owner_rls_class_predicates(text,text,text,text,proxima_core.owner_rls_class,text)";
const MEMORY_TABLE: &str = "memory\"rows;--";

struct Fixture {
    database: String,
    admin: PgPool,
    runtime: PgPool,
    platform: PgPool,
    platform_role: String,
    runtime_role: String,
}

impl Fixture {
    async fn new() -> Self {
        let (database, admin, runtime, platform, platform_role, runtime_role, _) =
            super::setup_full_schema().await;
        Self {
            database,
            admin,
            runtime,
            platform,
            platform_role,
            runtime_role,
        }
    }

    async fn close(self) {
        self.runtime.close().await;
        self.platform.close().await;
        super::cleanup(
            &self.database,
            self.admin,
            &self.platform_role,
            &self.runtime_role,
        )
        .await;
    }
}

struct Entity {
    id: Uuid,
    owner: Uuid,
    memory: bool,
}

struct Seed {
    owners: [Uuid; 2],
    // Each owner's independent oracle is fact, abstraction, perspective, Goal.
    entities: Vec<Entity>,
}

async fn seed_memory(admin: &PgPool, owner: Uuid, kind: &str, anchor: Option<Uuid>) -> Uuid {
    let id = Uuid::now_v7();
    let content = if kind == "fact" {
        None
    } else {
        let content = Uuid::now_v7();
        sqlx::query("INSERT INTO proxima_core.content(content_id,owner_id,schema_id,content_hash) VALUES ($1,$2,'predicate/memory',$3)")
            .bind(content).bind(owner).bind(id.as_bytes().repeat(2)).execute(admin).await.unwrap();
        Some(content)
    };
    sqlx::query("INSERT INTO proxima_core.memory_head(handle,t,kind,owner_id,schema_id) VALUES ($1,$1,$2::text::proxima_core.memory_kind,$3,'predicate/memory')")
        .bind(id).bind(kind).bind(owner).execute(admin).await.unwrap();
    sqlx::query("INSERT INTO proxima_core.memory(handle,t,kind,owner_id,schema_id,content_id,refs,occurred_at, \"by\", via) VALUES ($1,$1,$2::text::proxima_core.memory_kind,$3,'predicate/memory',$4,$5,CASE WHEN $2::text='fact' THEN uuid_extract_timestamp($1) END, '00000000-0000-0000-0000-000000000509'::uuid, NULL)")
        .bind(id).bind(kind).bind(owner).bind(content).bind(anchor.into_iter().collect::<Vec<_>>())
        .execute(admin).await.unwrap();
    id
}

async fn seed_goal(admin: &PgPool, owner: Uuid) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO proxima_core.goal_head(handle,t,owner_id,schema_id) VALUES ($1,$1,$2,'core/task-goal-v1')")
        .bind(id).bind(owner).execute(admin).await.unwrap();
    sqlx::query("INSERT INTO proxima_core.goal(handle,t,owner_id,title,state,request_id, \"by\", via) VALUES ($1,$1,$2,'predicate goal','Active',$3, '00000000-0000-0000-0000-000000000509'::uuid, NULL)")
        .bind(id).bind(owner).bind(id.to_string()).execute(admin).await.unwrap();
    id
}

async fn seed_entities(admin: &PgPool) -> Seed {
    let owners = [Uuid::now_v7(), Uuid::now_v7()];
    let mut entities = Vec::new();
    for owner in owners {
        sqlx::query("INSERT INTO proxima_core.owners(owner_id,kind) VALUES ($1,'group')")
            .bind(owner)
            .execute(admin)
            .await
            .unwrap();
        let fact = seed_memory(admin, owner, "fact", None).await;
        entities.push(Entity {
            id: fact,
            owner,
            memory: true,
        });
        for kind in ["abstraction", "perspective"] {
            let id = seed_memory(admin, owner, kind, Some(fact)).await;
            entities.push(Entity {
                id,
                owner,
                memory: true,
            });
        }
        let id = seed_goal(admin, owner).await;
        entities.push(Entity {
            id,
            owner,
            memory: false,
        });
    }
    let goal_memory_rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM proxima_core.memory WHERE t=ANY($1)")
            .bind(vec![entities[3].id, entities[7].id])
            .fetch_one(admin)
            .await
            .unwrap();
    assert_eq!(goal_memory_rows, 0, "Goals have no parent memory row");
    Seed { owners, entities }
}

async fn settings(connection: &mut PgConnection) -> Vec<Option<String>> {
    sqlx::query_scalar("SELECT current_setting(setting,true) FROM unnest($1::text[]) WITH ORDINALITY AS names(setting,position) ORDER BY position")
        .bind(&SETTINGS[..]).fetch_all(connection).await.unwrap()
}

async fn set_scope(connection: &mut PgConnection, scope: &str, grants: &[(&str, Uuid)]) {
    let mut values = vec!["{}".to_owned(); SETTINGS.len()];
    scope.clone_into(&mut values[0]);
    for (setting, owner) in grants {
        let index = SETTINGS.iter().position(|name| name == setting).unwrap();
        values[index] = format!("{{{owner}}}");
    }
    sqlx::query("SELECT set_config(setting,value,true) FROM unnest($1::text[],$2::text[]) AS settings(setting,value)")
        .bind(&SETTINGS[..]).bind(values).execute(connection).await.unwrap();
}

#[derive(Debug)]
struct Predicates {
    read: String,
    write: String,
    select: String,
}

async fn predicates(
    connection: &mut PgConnection,
    schema: &str,
    table: &str,
    class: Option<&str>,
    owner_column: Option<&str>,
) -> Result<Predicates, sqlx::Error> {
    let rows: Vec<(String, String, String)> = sqlx::query_as("SELECT read_expr,write_expr,select_expr FROM proxima_core.owner_rls_class_predicates('predicate test','test classification',$1,$2,$3::text::proxima_core.owner_rls_class,$4)")
        .bind(schema).bind(table).bind(class).bind(owner_column).fetch_all(connection).await?;
    assert_eq!(rows.len(), 1, "one expression record per classification");
    let (read, write, select) = rows.into_iter().next().unwrap();
    Ok(Predicates {
        read,
        write,
        select,
    })
}

fn relation(schema: &str, table: &str) -> String {
    format!(
        "{}.{}",
        super::quoted_identifier(schema),
        super::quoted_identifier(table)
    )
}

async fn assert_envelope(fixture: &Fixture, schema: &str, table: &str) {
    let target = relation(schema, table);
    let metadata: (bool, bool, String) = sqlx::query_as("SELECT relrowsecurity,relforcerowsecurity,pg_get_userbyid(relowner)::text FROM pg_class WHERE oid=$1::regclass")
        .bind(&target).fetch_one(&fixture.admin).await.unwrap();
    assert_eq!(metadata, (true, true, fixture.platform_role.clone()));
    let policies: Vec<(String, String, bool, bool, bool, bool, bool)> = sqlx::query_as(
        "SELECT p.polname::text,p.polcmd::text,p.polpermissive,p.polroles=ARRAY[0::oid],p.polroles=ARRAY[c.relowner],p.polwithcheck IS NOT NULL,pg_get_expr(p.polqual,p.polrelid) IS NOT DISTINCT FROM pg_get_expr(p.polwithcheck,p.polrelid) FROM pg_policy p JOIN pg_class c ON c.oid=p.polrelid WHERE p.polrelid=$1::regclass ORDER BY p.polname"
    ).bind(&target).fetch_all(&fixture.admin).await.unwrap();
    assert_eq!(
        policies,
        vec![
            (
                "proxima_owner_read".into(),
                "r".into(),
                true,
                true,
                false,
                false,
                false
            ),
            (
                "proxima_owner_write".into(),
                "*".into(),
                true,
                true,
                false,
                true,
                true
            ),
            (
                "proxima_platform".into(),
                "*".into(),
                true,
                false,
                true,
                true,
                true
            ),
        ],
        "{target}"
    );
}

async fn assert_helper_acl(fixture: &Fixture) {
    let metadata = sqlx::query("SELECT prosecdef,proisstrict,provolatile<>'i' AS catalog_dependent,proconfig,pg_get_userbyid(proowner)::text AS owner,has_function_privilege($2,p.oid,'EXECUTE') AS runtime_execute,EXISTS(SELECT 1 FROM aclexplode(COALESCE(proacl,acldefault('f',proowner))) a WHERE a.grantee=0 AND a.privilege_type='EXECUTE') AS public_execute FROM pg_proc p WHERE oid=$1::regprocedure")
        .bind(HELPER).bind(&fixture.runtime_role).fetch_one(&fixture.admin).await.unwrap();
    for field in [
        "prosecdef",
        "proisstrict",
        "runtime_execute",
        "public_execute",
    ] {
        assert!(!metadata.get::<bool, _>(field), "{field}");
    }
    assert!(metadata.get::<bool, _>("catalog_dependent"));
    assert_eq!(
        metadata.get::<Vec<String>, _>("proconfig"),
        ["search_path=pg_catalog".to_owned()]
    );
    assert_eq!(metadata.get::<String, _>("owner"), fixture.platform_role);
    let mut transaction = fixture.runtime.begin().await.unwrap();
    let error = predicates(
        &mut transaction,
        "proxima_core",
        "projection",
        Some("owner_id"),
        Some("owner_id"),
    )
    .await
    .expect_err("runtime cannot generate privileged DDL expressions");
    assert_eq!(
        error.as_database_error().unwrap().code().as_deref(),
        Some("42501")
    );
    transaction.rollback().await.unwrap();
}

struct CoreTable {
    table: &'static str,
    key: &'static str,
    insert: &'static str,
}

const CORE_TABLES: [CoreTable; 4] = [
    CoreTable {
        table: "projection",
        key: "memory_id",
        insert: "INSERT INTO proxima_core.projection(memory_id,owner_id,schema_id,lexical_language,search_tsv) VALUES ($1,$2,$3,'simple'::regconfig,to_tsvector('simple','predicate'))",
    },
    CoreTable {
        table: "embeddings",
        key: "entity_id",
        insert: "INSERT INTO proxima_core.embeddings(entity_id,owner_id,model_id,dim,vec) VALUES ($1,$2,$3,384,array_fill(0.25::real,ARRAY[384])::public.vector)",
    },
    CoreTable {
        table: "embedding_heads",
        key: "entity_id",
        insert: "INSERT INTO proxima_core.embedding_heads(entity_id,owner_id,model_id,embedding_version,dim) VALUES ($1,$2,$3,1,384)",
    },
    CoreTable {
        table: "embedding_jobs",
        key: "entity_id",
        insert: "INSERT INTO proxima_core.embedding_jobs(entity_id,owner_id,model_id,dim) VALUES ($1,$2,$3,384)",
    },
];

async fn insert_core_row(
    connection: &mut PgConnection,
    table: &CoreTable,
    id: Uuid,
    owner: Uuid,
    label: &str,
) -> Result<(), sqlx::Error> {
    // SQL-POLICY: fixed-fragment — the four insert templates above are closed test fixtures.
    sqlx::query(sqlx::AssertSqlSafe(table.insert))
        .bind(id)
        .bind(owner)
        .bind(label)
        .execute(connection)
        .await
        .map(|_| ())
}

async fn seed_core(fixture: &Fixture, seed: &Seed) {
    let mut admin = fixture.admin.acquire().await.unwrap();
    for table in &CORE_TABLES {
        for entity in &seed.entities {
            // Projection's real FK cannot hold a Goal without a memory row.
            if table.table != "projection" || entity.memory {
                insert_core_row(&mut admin, table, entity.id, entity.owner, "predicate/core")
                    .await
                    .unwrap();
            }
        }
        // Existing constraints permit a child owned by B with A's memory key.
        insert_core_row(
            &mut admin,
            table,
            seed.entities[0].id,
            seed.owners[1],
            "predicate/foreign",
        )
        .await
        .unwrap();
    }
}

async fn core_rows(
    connection: &mut PgConnection,
    table: &CoreTable,
    expression: &str,
) -> Vec<(Uuid, Uuid)> {
    let target = relation("proxima_core", table.table);
    // SQL-POLICY: fixed-fragment — closed table keys and the trusted catalog generator's expression.
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {target}.{}, {target}.owner_id FROM {target} WHERE {expression} ORDER BY 1,2",
        super::quoted_identifier(table.key)
    )))
    .fetch_all(connection)
    .await
    .unwrap()
}

struct ScopeCase {
    name: &'static str,
    grants: Vec<(&'static str, Uuid)>,
    read: Vec<usize>,
    write: Vec<usize>,
}

fn core_cases(owners: [Uuid; 2]) -> Vec<ScopeCase> {
    let [a, b] = owners;
    vec![
        ScopeCase {
            name: "fact only and foreign child owner",
            grants: vec![("app.owner", a)],
            read: vec![0],
            write: vec![],
        },
        ScopeCase {
            name: "all memory kinds",
            grants: vec![
                ("app.owner", a),
                ("app.read_abstraction", a),
                ("app.read_perspective", a),
            ],
            read: vec![0, 1, 2],
            write: vec![],
        },
        ScopeCase {
            name: "Goal shortcut without F owner",
            grants: vec![("app.read_goal", a)],
            read: vec![0, 1, 2, 3],
            write: vec![],
        },
        ScopeCase {
            name: "different runtime owner",
            grants: vec![
                ("app.owner", b),
                ("app.read_abstraction", b),
                ("app.read_perspective", b),
            ],
            read: vec![4, 5, 6],
            write: vec![],
        },
        ScopeCase {
            name: "write-only SELECT",
            grants: vec![
                ("app.write_owner", a),
                ("app.write_abstraction", a),
                ("app.write_perspective", a),
            ],
            read: vec![],
            write: vec![0, 1, 2],
        },
        ScopeCase {
            name: "Goal write-only shortcut",
            grants: vec![("app.write_goal", a)],
            read: vec![],
            write: vec![0, 1, 2, 3],
        },
        ScopeCase {
            name: "empty",
            grants: vec![],
            read: vec![],
            write: vec![],
        },
    ]
}

fn expected_core(seed: &Seed, table: &CoreTable, indices: &[usize]) -> Vec<(Uuid, Uuid)> {
    let mut rows: Vec<_> = indices
        .iter()
        .map(|index| &seed.entities[*index])
        .filter(|entity| table.table != "projection" || entity.memory)
        .map(|entity| (entity.id, entity.owner))
        .collect();
    rows.sort_unstable();
    rows.dedup();
    rows
}

async fn assert_core_case(
    fixture: &Fixture,
    seed: &Seed,
    expressions: &[Predicates],
    case: &ScopeCase,
) {
    let mut transaction = fixture.runtime.begin().await.unwrap();
    set_scope(&mut transaction, "owner", &case.grants).await;
    let before = settings(&mut transaction).await;
    for (table, predicates) in CORE_TABLES.iter().zip(expressions) {
        let selected: Vec<_> = case.read.iter().chain(&case.write).copied().collect();
        for (expression, indices) in [
            (&predicates.read, &case.read),
            (&predicates.write, &case.write),
            (&predicates.select, &selected),
        ] {
            assert_eq!(
                core_rows(&mut transaction, table, expression).await,
                expected_core(seed, table, indices),
                "{} / {}",
                case.name,
                table.table
            );
        }
        assert_eq!(
            core_rows(&mut transaction, table, "true").await,
            expected_core(seed, table, &selected),
            "ordinary policy / {} / {}",
            case.name,
            table.table
        );
    }
    assert_eq!(settings(&mut transaction).await, before);
    transaction.rollback().await.unwrap();
}

async fn assert_core_writes(fixture: &Fixture, seed: &Seed, table: &CoreTable) {
    let mut transaction = fixture.runtime.begin().await.unwrap();
    set_scope(
        &mut transaction,
        "owner",
        &[("app.write_owner", seed.owners[0])],
    )
    .await;
    let before = settings(&mut transaction).await;
    insert_core_row(
        &mut transaction,
        table,
        seed.entities[0].id,
        seed.owners[0],
        "predicate/write",
    )
    .await
    .unwrap();
    let target = relation("proxima_core", table.table);
    // SQL-POLICY: fixed-fragment — closed relation/key, bound owner values.
    let update = format!(
        "UPDATE {target} SET owner_id=$1 WHERE {}=$2",
        super::quoted_identifier(table.key)
    );
    assert_eq!(
        // SQL-POLICY: fixed-fragment — the closed table/key template above; values stay bound.
        sqlx::query(sqlx::AssertSqlSafe(update.clone()))
            .bind(seed.owners[1])
            .bind(seed.entities[4].id)
            .execute(&mut *transaction)
            .await
            .unwrap()
            .rows_affected(),
        0,
        "USING hides another owner's row"
    );
    sqlx::query("SAVEPOINT refused_write")
        .execute(&mut *transaction)
        .await
        .unwrap();
    // SQL-POLICY: fixed-fragment — the same closed table/key template with bound owner values.
    let error = sqlx::query(sqlx::AssertSqlSafe(update))
        .bind(seed.owners[1])
        .bind(seed.entities[0].id)
        .execute(&mut *transaction)
        .await
        .expect_err("WITH CHECK refuses owner reassignment");
    super::assert_rls_refusal(&error);
    sqlx::query("ROLLBACK TO SAVEPOINT refused_write")
        .execute(&mut *transaction)
        .await
        .unwrap();
    let error = insert_core_row(
        &mut transaction,
        table,
        seed.entities[4].id,
        seed.owners[1],
        "predicate/refused",
    )
    .await
    .expect_err("WITH CHECK refuses another owner's insert");
    super::assert_rls_refusal(&error);
    sqlx::query("ROLLBACK TO SAVEPOINT refused_write")
        .execute(&mut *transaction)
        .await
        .unwrap();
    assert_eq!(settings(&mut transaction).await, before);
    transaction.rollback().await.unwrap();
}

#[tokio::test]
async fn core_predicates_preserve_kinds_goals_effective_select_and_write_checks() {
    let fixture = Fixture::new().await;
    let seed = seed_entities(&fixture.admin).await;
    seed_core(&fixture, &seed).await;
    assert_helper_acl(&fixture).await;
    let mut generation = fixture.platform.begin().await.unwrap();
    set_scope(&mut generation, "", &[]).await;
    let before = settings(&mut generation).await;
    let mut expressions = Vec::new();
    for table in &CORE_TABLES {
        expressions.push(
            predicates(
                &mut generation,
                "proxima_core",
                table.table,
                Some("owner_id"),
                Some("owner_id"),
            )
            .await
            .unwrap(),
        );
        assert_envelope(&fixture, "proxima_core", table.table).await;
    }
    assert_eq!(settings(&mut generation).await, before);
    generation.rollback().await.unwrap();
    for case in core_cases(seed.owners) {
        assert_core_case(&fixture, &seed, &expressions, &case).await;
    }
    for table in &CORE_TABLES {
        assert_core_writes(&fixture, &seed, table).await;
    }
    fixture.close().await;
}

async fn create_flavor_tables(fixture: &Fixture, schema: &str) {
    let schema_name = super::quoted_identifier(schema);
    super::execute(
        &fixture.admin,
        &format!(
            "CREATE SCHEMA {schema_name} AUTHORIZATION {}",
            super::quoted_identifier(&fixture.platform_role)
        ),
    )
    .await;
    let mut platform = fixture.platform.acquire().await.unwrap();
    // SQL-POLICY: fixed-fragment — identifiers are quoted; no supplied value is executable SQL.
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "CREATE TABLE {} (id uuid PRIMARY KEY REFERENCES proxima_core.memory(t),owner_id uuid NOT NULL REFERENCES proxima_core.owners(owner_id)); CREATE TABLE {} (id uuid PRIMARY KEY,owner_id uuid NOT NULL REFERENCES proxima_core.owners(owner_id))",
        relation(schema, MEMORY_TABLE), relation(schema, "projection")
    ))).execute(&mut *platform).await.unwrap();
    super::execute(
        &fixture.admin,
        &format!(
            "GRANT USAGE ON SCHEMA {schema_name} TO {}",
            super::quoted_identifier(&fixture.runtime_role)
        ),
    )
    .await;
    super::execute(
        &fixture.admin,
        &format!(
            "GRANT SELECT,INSERT,UPDATE,DELETE ON ALL TABLES IN SCHEMA {schema_name} TO {}",
            super::quoted_identifier(&fixture.runtime_role)
        ),
    )
    .await;
}

async fn install_flavor_tables(connection: &mut PgConnection, schema: &str, whole_schema: bool) {
    if whole_schema {
        sqlx::query("SELECT proxima_core.install_owner_rls($1,ARRAY['projection']::text[],'{}'::text[],'{}'::text[],$2::text[])")
            .bind(schema).bind(vec![MEMORY_TABLE]).execute(&mut *connection).await.unwrap();
    } else {
        for (table, class) in [(MEMORY_TABLE, "memory_owner"), ("projection", "owner_id")] {
            sqlx::query("SELECT proxima_core.install_owner_rls_table($1,$2,$3::text::proxima_core.owner_rls_class)")
                .bind(schema).bind(table).bind(class).execute(&mut *connection).await.unwrap();
        }
    }
}

async fn seed_flavor(fixture: &Fixture, schema: &str, seed: &Seed) {
    for entity in seed.entities.iter().filter(|entity| entity.memory) {
        // This class intentionally derives ownership from memory, not the child owner.
        // SQL-POLICY: fixed-fragment — quoted fixture relation, bound data.
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "INSERT INTO {}(id,owner_id) VALUES ($1,$2)",
            relation(schema, MEMORY_TABLE)
        )))
        .bind(entity.id)
        .bind(seed.owners[1])
        .execute(&fixture.admin)
        .await
        .unwrap();
    }
    for index in [0, 4] {
        let entity = &seed.entities[index];
        // SQL-POLICY: fixed-fragment — quoted fixture relation, bound data.
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "INSERT INTO {}(id,owner_id) VALUES ($1,$2)",
            relation(schema, "projection")
        )))
        .bind(entity.id)
        .bind(entity.owner)
        .execute(&fixture.admin)
        .await
        .unwrap();
    }
}

async fn ids(
    connection: &mut PgConnection,
    schema: &str,
    table: &str,
    key: &str,
    expression: &str,
) -> Vec<Uuid> {
    let target = relation(schema, table);
    // The extra owner_id makes an accidentally unqualified owner column ambiguous.
    // SQL-POLICY: fixed-fragment — quoted identifiers and trusted generated expression.
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT {target}.{} FROM {target} CROSS JOIN (SELECT NULL::uuid AS owner_id) other WHERE {expression} ORDER BY 1", super::quoted_identifier(key))))
        .fetch_all(connection).await.unwrap()
}

fn expected_ids(seed: &Seed, indices: &[usize]) -> Vec<Uuid> {
    let mut expected: Vec<_> = indices
        .iter()
        .map(|index| seed.entities[*index].id)
        .collect();
    expected.sort_unstable();
    expected
}

struct FlavorCase {
    name: &'static str,
    grants: Vec<(&'static str, Uuid)>,
    memory: Vec<usize>,
    owner: Vec<usize>,
}

fn flavor_cases(owners: [Uuid; 2]) -> Vec<FlavorCase> {
    let [a, b] = owners;
    vec![
        FlavorCase {
            name: "same owner",
            grants: vec![
                ("app.owner", a),
                ("app.read_abstraction", a),
                ("app.read_perspective", a),
            ],
            memory: vec![0, 1, 2],
            owner: vec![0],
        },
        FlavorCase {
            name: "parent RLS hides foreign abstraction",
            grants: vec![("app.owner", a), ("app.read_abstraction", b)],
            memory: vec![0],
            owner: vec![0],
        },
        FlavorCase {
            name: "empty global owner",
            grants: vec![("app.read_abstraction", a)],
            memory: vec![],
            owner: vec![],
        },
        FlavorCase {
            name: "all empty",
            grants: vec![],
            memory: vec![],
            owner: vec![],
        },
        FlavorCase {
            name: "write-only effective SELECT",
            grants: vec![
                ("app.write_owner", a),
                ("app.write_abstraction", a),
                ("app.write_perspective", a),
            ],
            memory: vec![0, 1, 2],
            owner: vec![0],
        },
        FlavorCase {
            name: "flavor projection has no Goal shortcut",
            grants: vec![("app.read_goal", a)],
            memory: vec![],
            owner: vec![],
        },
        FlavorCase {
            name: "second runtime owner",
            grants: vec![
                ("app.owner", b),
                ("app.read_abstraction", b),
                ("app.read_perspective", b),
            ],
            memory: vec![4, 5, 6],
            owner: vec![4],
        },
    ]
}

async fn assert_flavor_cases(
    fixture: &Fixture,
    seed: &Seed,
    schema: &str,
    expressions: &[Predicates],
) {
    for case in flavor_cases(seed.owners) {
        let mut transaction = fixture.runtime.begin().await.unwrap();
        set_scope(&mut transaction, "owner", &case.grants).await;
        let before = settings(&mut transaction).await;
        for ((table, expected), predicates) in
            [(MEMORY_TABLE, &case.memory), ("projection", &case.owner)]
                .into_iter()
                .zip(expressions)
        {
            for expression in [&predicates.select[..], "true"] {
                assert_eq!(
                    ids(&mut transaction, schema, table, "id", expression).await,
                    expected_ids(seed, expected),
                    "{schema} / {table} / {}",
                    case.name
                );
            }
        }
        assert_eq!(settings(&mut transaction).await, before);
        transaction.rollback().await.unwrap();
    }
}

#[tokio::test]
async fn flavor_predicates_keep_parent_rls_through_both_installers_and_quoted_names() {
    let fixture = Fixture::new().await;
    let seed = seed_entities(&fixture.admin).await;
    for (schema, whole_schema) in [("predicate bulk", true), ("predicate \"per table", false)] {
        create_flavor_tables(&fixture, schema).await;
        let mut generation = fixture.platform.begin().await.unwrap();
        set_scope(&mut generation, "", &[]).await;
        let before = settings(&mut generation).await;
        install_flavor_tables(&mut generation, schema, whole_schema).await;
        let mut expressions = Vec::new();
        for (table, class, column) in [
            (MEMORY_TABLE, "memory_owner", None),
            ("projection", "owner_id", Some("owner_id")),
        ] {
            expressions.push(
                predicates(&mut generation, schema, table, Some(class), column)
                    .await
                    .unwrap(),
            );
        }
        assert_eq!(settings(&mut generation).await, before);
        generation.commit().await.unwrap();
        seed_flavor(&fixture, schema, &seed).await;
        for table in [MEMORY_TABLE, "projection"] {
            assert_envelope(&fixture, schema, table).await;
        }
        assert_flavor_cases(&fixture, &seed, schema, &expressions).await;
    }
    fixture.close().await;
}

async fn create_fk_tables(fixture: &Fixture) {
    super::execute(
        &fixture.admin,
        &format!(
            "CREATE SCHEMA predicate_fk AUTHORIZATION {}",
            super::quoted_identifier(&fixture.platform_role)
        ),
    )
    .await;
    let mut platform = fixture.platform.acquire().await.unwrap();
    sqlx::raw_sql(
        "CREATE TABLE predicate_fk.leading_memory(memory_key uuid PRIMARY KEY REFERENCES proxima_core.memory(t),t uuid REFERENCES proxima_core.memory(t));
         CREATE TABLE predicate_fk.sole_memory(row_no integer PRIMARY KEY,memory_key uuid REFERENCES proxima_core.memory(t));
         CREATE TABLE predicate_fk.fallback(t uuid PRIMARY KEY);
         CREATE TABLE predicate_fk.ambiguous(id integer PRIMARY KEY,one uuid REFERENCES proxima_core.memory(t),two uuid REFERENCES proxima_core.memory(t));
         CREATE TABLE predicate_fk.competing_t(t uuid PRIMARY KEY,memory_key uuid REFERENCES proxima_core.memory(t));
         CREATE TABLE predicate_fk.missing_key(id integer PRIMARY KEY);
         CREATE TABLE predicate_fk.text_key(t text PRIMARY KEY);
         CREATE TABLE predicate_fk.cycle_a(t uuid PRIMARY KEY,parent uuid);
         CREATE TABLE predicate_fk.cycle_b(t uuid PRIMARY KEY,parent uuid REFERENCES predicate_fk.cycle_a(t));
         ALTER TABLE predicate_fk.cycle_a ADD FOREIGN KEY(parent) REFERENCES predicate_fk.cycle_b(t);
         CREATE TABLE predicate_fk.quoted_owner(id uuid PRIMARY KEY,\"owner\"\"key;--\" uuid NOT NULL)"
    ).execute(&mut *platform).await.unwrap();
    super::execute(
        &fixture.admin,
        &format!(
            "GRANT USAGE ON SCHEMA predicate_fk TO {}",
            super::quoted_identifier(&fixture.runtime_role)
        ),
    )
    .await;
    super::execute(
        &fixture.admin,
        &format!(
            "GRANT SELECT,INSERT,UPDATE,DELETE ON ALL TABLES IN SCHEMA predicate_fk TO {}",
            super::quoted_identifier(&fixture.runtime_role)
        ),
    )
    .await;
}

async fn assert_generation_refusal(
    fixture: &Fixture,
    schema: &str,
    table: &str,
    class: Option<&str>,
    column: Option<&str>,
    message: &str,
) {
    let mut transaction = fixture.platform.begin().await.unwrap();
    let error = predicates(&mut transaction, schema, table, class, column)
        .await
        .expect_err("invalid catalog input must refuse generation");
    assert!(error.as_database_error().is_some(), "{error}");
    assert!(
        error.to_string().contains(message),
        "{schema}.{table}: {error}"
    );
    transaction.rollback().await.unwrap();
}

async fn assert_installer_refusal(
    fixture: &Fixture,
    table: &str,
    class: &str,
    column: &str,
    message: &str,
) {
    let mut transaction = fixture.platform.begin().await.unwrap();
    let error = sqlx::query("SELECT proxima_core.install_owner_rls_table('predicate_fk',$1,$2::text::proxima_core.owner_rls_class,$3)")
        .bind(table).bind(class).bind(column).execute(&mut *transaction).await.expect_err("invalid installation must refuse");
    assert!(error.to_string().contains(message), "{error}");
    transaction.rollback().await.unwrap();
}

async fn assert_fk_resolution(fixture: &Fixture, seed: &Seed) {
    let [a, b] = [seed.entities[0].id, seed.entities[4].id];
    sqlx::query("INSERT INTO predicate_fk.leading_memory(memory_key,t) VALUES ($1,$2),($2,$1)")
        .bind(a)
        .bind(b)
        .execute(&fixture.admin)
        .await
        .unwrap();
    sqlx::query("INSERT INTO predicate_fk.sole_memory(row_no,memory_key) VALUES (1,$1),(2,$2)")
        .bind(a)
        .bind(b)
        .execute(&fixture.admin)
        .await
        .unwrap();
    sqlx::query("INSERT INTO predicate_fk.fallback(t) VALUES ($1),($2)")
        .bind(a)
        .bind(b)
        .execute(&fixture.admin)
        .await
        .unwrap();
    for (table, key) in [
        ("leading_memory", "memory_key"),
        ("sole_memory", "memory_key"),
        ("fallback", "t"),
    ] {
        let mut generation = fixture.platform.begin().await.unwrap();
        set_scope(&mut generation, "", &[]).await;
        let before = settings(&mut generation).await;
        let expressions = predicates(
            &mut generation,
            "predicate_fk",
            table,
            Some("memory_owner"),
            None,
        )
        .await
        .unwrap();
        sqlx::query(
            "SELECT proxima_core.install_owner_rls_table('predicate_fk',$1,'memory_owner')",
        )
        .bind(table)
        .execute(&mut *generation)
        .await
        .unwrap();
        assert_eq!(settings(&mut generation).await, before);
        generation.commit().await.unwrap();
        assert_envelope(fixture, "predicate_fk", table).await;
        for (owner, expected) in [(seed.owners[0], a), (seed.owners[1], b)] {
            let mut transaction = fixture.runtime.begin().await.unwrap();
            set_scope(&mut transaction, "owner", &[("app.owner", owner)]).await;
            assert_eq!(
                ids(
                    &mut transaction,
                    "predicate_fk",
                    table,
                    key,
                    &expressions.select
                )
                .await,
                [expected]
            );
            transaction.rollback().await.unwrap();
        }
    }
}

async fn assert_quoted_owner_column(fixture: &Fixture, seed: &Seed) {
    let column = "owner\"key;--";
    sqlx::query(
        "INSERT INTO predicate_fk.quoted_owner(id,\"owner\"\"key;--\") VALUES ($1,$2),($3,$4)",
    )
    .bind(seed.entities[0].id)
    .bind(seed.owners[0])
    .bind(seed.entities[4].id)
    .bind(seed.owners[1])
    .execute(&fixture.admin)
    .await
    .unwrap();
    let mut generation = fixture.platform.begin().await.unwrap();
    set_scope(&mut generation, "", &[]).await;
    let before = settings(&mut generation).await;
    let expressions = predicates(
        &mut generation,
        "predicate_fk",
        "quoted_owner",
        Some("owner_id"),
        Some(column),
    )
    .await
    .unwrap();
    sqlx::query(
        "SELECT proxima_core.install_owner_rls_table('predicate_fk','quoted_owner','owner_id',$1)",
    )
    .bind(column)
    .execute(&mut *generation)
    .await
    .unwrap();
    assert_eq!(settings(&mut generation).await, before);
    generation.commit().await.unwrap();
    let mut transaction = fixture.runtime.begin().await.unwrap();
    set_scope(&mut transaction, "owner", &[("app.owner", seed.owners[0])]).await;
    assert_eq!(
        ids(
            &mut transaction,
            "predicate_fk",
            "quoted_owner",
            "id",
            &expressions.select
        )
        .await,
        [seed.entities[0].id]
    );
    transaction.rollback().await.unwrap();
    assert_envelope(fixture, "predicate_fk", "quoted_owner").await;
    assert_installer_refusal(
        fixture,
        "quoted_owner",
        "owner_id",
        "owner_id) OR true --",
        "has no",
    )
    .await;
}

#[tokio::test]
async fn predicate_generation_preserves_fk_springs_and_rejects_invalid_core_inputs() {
    let fixture = Fixture::new().await;
    let seed = seed_entities(&fixture.admin).await;
    create_fk_tables(&fixture).await;
    assert_fk_resolution(&fixture, &seed).await;
    assert_quoted_owner_column(&fixture, &seed).await;
    for (table, message) in [
        ("ambiguous", "its memory is ambiguous"),
        ("competing_t", "its memory is ambiguous"),
        ("missing_key", "has no uuid t column"),
        ("text_key", "has no uuid t column"),
    ] {
        assert_generation_refusal(
            &fixture,
            "predicate_fk",
            table,
            Some("memory_owner"),
            None,
            message,
        )
        .await;
    }
    assert_generation_refusal(
        &fixture,
        "predicate_fk",
        "ambiguous",
        Some("fk_parent"),
        None,
        "its owner is ambiguous",
    )
    .await;
    assert_installer_refusal(
        &fixture,
        "cycle_a",
        "fk_parent",
        "owner_id",
        "reaches itself",
    )
    .await;
    for table in &CORE_TABLES {
        for column in [Some("another_owner"), None] {
            assert_generation_refusal(
                &fixture,
                "proxima_core",
                table.table,
                Some("owner_id"),
                column,
                "requires owner column owner_id",
            )
            .await;
        }
    }
    assert_generation_refusal(
        &fixture,
        "proxima_core",
        "projection",
        None,
        Some("owner_id"),
        "case not found",
    )
    .await;
    fixture.close().await;
}
