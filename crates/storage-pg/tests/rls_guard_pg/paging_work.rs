//! Work bound for owner-scoped memory pages under the shipped RLS policies.

use proxima_core::verbs::query::{EntityKind, QueryRequest};
use proxima_core::{Credentials, GroupId, OwnerRef, OwnerRoles, Role, UserId};
use proxima_storage_pg::{begin_owner_transaction, verbs::query::memory_page_sql_for_tests};
use sqlx::{PgConnection, PgPool, Row};
use uuid::Uuid;

struct RowSpec {
    handle: Uuid,
    t: Uuid,
    owner: Uuid,
    schema: &'static str,
    current: bool,
}

async fn seed(pool: &PgPool, owners: &[Uuid]) -> Vec<RowSpec> {
    let mut rows = Vec::with_capacity(40_000);
    let mut heads = Vec::with_capacity(19_900);
    let base = Uuid::now_v7().as_u128() & !0xffff;
    let mut n = 0_u128;
    for (owner_index, owner) in owners.iter().copied().enumerate() {
        let count = if owner_index == 0 { 10_000 } else { 100 };
        for index in 0..count {
            let handle = Uuid::from_u128(base + n);
            n += 1;
            let schema = if index % 2 == 0 {
                "paging/fact-a"
            } else {
                "paging/fact-b"
            };
            let (old, current) = if owner_index == 0 && index < 100 {
                let old = RowSpec {
                    handle,
                    t: Uuid::from_u128(base + n),
                    owner,
                    schema,
                    current: false,
                };
                n += 1;
                (Some(old), Uuid::from_u128(base + n))
            } else {
                (None, Uuid::from_u128(base + n))
            };
            n += 1;
            if let Some(old) = old {
                rows.push(old);
            }
            let row = RowSpec {
                handle,
                t: current,
                owner,
                schema,
                current: true,
            };
            heads.push((handle, current, owner, schema));
            rows.push(row);
        }
    }
    for owner in owners {
        sqlx::query("INSERT INTO proxima_core.owners(owner_id, kind) VALUES ($1, 'group')")
            .bind(owner)
            .execute(pool)
            .await
            .unwrap();
    }
    // Newer non-head revisions deliberately exercise the distinction between
    // the ordered head stream and history. The head remains the earlier row.
    for index in 0..20_000_u128 {
        let (handle, _, owner, schema) = heads[usize::try_from(index % 200).unwrap()];
        rows.push(RowSpec {
            handle,
            t: Uuid::from_u128(base + 1_000_000 + index),
            owner,
            schema,
            current: false,
        });
    }
    for chunk in heads.chunks(1000) {
        let handles: Vec<_> = chunk.iter().map(|x| x.0).collect();
        let ts: Vec<_> = chunk.iter().map(|x| x.1).collect();
        let owner_ids: Vec<_> = chunk.iter().map(|x| x.2).collect();
        let schemas: Vec<_> = chunk.iter().map(|x| x.3).collect();
        sqlx::query("INSERT INTO proxima_core.memory_head(handle,t,kind,owner_id,schema_id) SELECT h,t,k::proxima_core.memory_kind,o,s FROM UNNEST($1::uuid[],$2::uuid[],$3::text[],$4::uuid[],$5::text[]) AS x(h,t,k,o,s)")
            .bind(&handles).bind(&ts).bind(vec!["fact"; chunk.len()]).bind(&owner_ids).bind(&schemas)
            .execute(pool).await.unwrap();
    }
    for chunk in rows.chunks(1000) {
        let handles: Vec<_> = chunk.iter().map(|x| x.handle).collect();
        let ts: Vec<_> = chunk.iter().map(|x| x.t).collect();
        let owner_ids: Vec<_> = chunk.iter().map(|x| x.owner).collect();
        let schemas: Vec<_> = chunk.iter().map(|x| x.schema).collect();
        sqlx::query("INSERT INTO proxima_core.memory(handle,t,kind,owner_id,schema_id) SELECT h,t,k::proxima_core.memory_kind,o,s FROM UNNEST($1::uuid[],$2::uuid[],$3::text[],$4::uuid[],$5::text[]) AS x(h,t,k,o,s)")
            .bind(&handles).bind(&ts).bind(vec!["fact"; chunk.len()]).bind(&owner_ids).bind(&schemas)
            .execute(pool).await.unwrap();
    }
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        "ANALYZE proxima_core.owners; ANALYZE proxima_core.memory_head; ANALYZE proxima_core.memory;"
            .to_owned(),
    ))
    .execute(pool)
    .await
    .unwrap();
    rows
}

fn metric(node: &serde_json::Map<String, serde_json::Value>, name: &str) -> f64 {
    node.get(name)
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(0.0)
}

fn work(plan: &serde_json::Value, table: Option<&str>) -> f64 {
    match plan {
        serde_json::Value::Array(xs) => xs.iter().map(|node| work(node, table)).sum(),
        serde_json::Value::Object(node) => {
            let own = node
                .get("Relation Name")
                .and_then(serde_json::Value::as_str)
                .filter(|name| {
                    matches!(*name, "memory" | "memory_head")
                        && table.is_none_or(|table| table == *name)
                })
                .map_or(0.0, |_| {
                    metric(node, "Actual Rows")
                        + metric(node, "Rows Removed by Filter")
                        + metric(node, "Rows Removed by Index Recheck")
                })
                * metric(node, "Actual Loops").max(1.0);
            own + node.values().map(|node| work(node, table)).sum::<f64>()
        }
        _ => 0.0,
    }
}

async fn page_and_work(
    tx: &mut PgConnection,
    owner_ids: &[Uuid],
    schema: Option<&str>,
) -> (Vec<Uuid>, f64, serde_json::Value) {
    let mut req = QueryRequest::readable();
    req.entity_kind = Some(EntityKind::Fact);
    req.limit = 100;
    req.schema_id = schema.map(str::to_owned).map(proxima_core::SchemaId::new);
    let sql = memory_page_sql_for_tests(&req).unwrap();
    let explain = format!("EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) {sql}");
    // SQL-POLICY: fixed-fragment — both statements are generated by the
    // closed production helper; only bind values are caller-controlled.
    let mut q = sqlx::query(sqlx::AssertSqlSafe(sql)).bind(owner_ids);
    let mut e = sqlx::query_scalar(sqlx::AssertSqlSafe(explain)).bind(owner_ids);
    if let Some(schema) = schema {
        q = q.bind(schema);
        e = e.bind(schema);
    }
    q = q.bind("fact");
    e = e.bind("fact");
    let rows = q.fetch_all(&mut *tx).await.unwrap();
    let plan: serde_json::Value = e.fetch_one(&mut *tx).await.unwrap();
    let ids = rows.into_iter().map(|row| row.get("memory_id")).collect();
    (ids, work(&plan, None), plan)
}

#[tokio::test]
async fn owner_paged_memory_work_is_bounded_after_filters() {
    let (database, admin, runtime, platform, platform_role, runtime_role, _) =
        super::setup_full_schema().await;
    let owners: Vec<_> = (0..100).map(|_| Uuid::now_v7()).collect();
    let rows = seed(&admin, &owners).await;
    let roles = owners
        .iter()
        .copied()
        .map(|id| (OwnerRef::Group(GroupId::new(id)), Role::viewer()))
        .collect::<Vec<_>>();
    let verifier = super::SyntheticVerifier {
        roles: OwnerRoles::for_subject(UserId::new(Uuid::now_v7()), roles).unwrap(),
    };
    let authz = proxima_core::authenticate(&verifier, &Credentials::Bearer("paging-work".into()))
        .await
        .unwrap();
    // Freshly inserted heap pages may require sorting heads. Memory hydration
    // must already stop at K. Once vacuum marks this read-mostly fixture
    // all-visible, both head and memory scans must stop at K per owner.
    for settled in [false, true] {
        if settled {
            sqlx::query("VACUUM (ANALYZE) proxima_core.memory_head")
                .execute(&admin)
                .await
                .unwrap();
        }
        let mut tx = begin_owner_transaction(&runtime, authz.owner_scope().unwrap())
            .await
            .unwrap();
        for selected in [vec![owners[0]], owners[..8].to_vec(), owners.clone()] {
            for schema in [None, Some("paging/fact-a")] {
                let (actual, scanned, plan) = page_and_work(&mut tx, &selected, schema).await;
                let mut expected: Vec<_> = rows
                    .iter()
                    .filter(|r| {
                        r.current
                            && selected.contains(&r.owner)
                            && schema.is_none_or(|s| s == r.schema)
                    })
                    .map(|r| r.t)
                    .collect();
                expected.sort_unstable_by(|a, b| b.cmp(a));
                expected.truncate(101);
                assert_eq!(
                    actual,
                    expected,
                    "selected={} schema={schema:?}",
                    selected.len()
                );
                let owner_count = f64::from(u32::try_from(selected.len()).unwrap());
                let memory_work = work(&plan, Some("memory"));
                assert!(
                    memory_work <= owner_count * 101.0,
                    "memory hydration {memory_work} exceeds page bound: {plan}"
                );
                if settled {
                    assert!(
                        scanned <= owner_count * 202.0,
                        "scan work {scanned} exceeds bound for {} owners: {plan}",
                        selected.len()
                    );
                }
            }
        }
        tx.rollback().await.unwrap();
    }
    runtime.close().await;
    platform.close().await;
    super::cleanup(&database, admin, &platform_role, &runtime_role).await;
}
