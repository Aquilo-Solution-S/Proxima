//! Exact paging semantics under the production owner policies. Expected rows
//! are computed from the seeded records, independently of the SQL builder.

use proxima_core::verbs::query::{EntityKind, QueryCursor, QueryRequest, SupersessionStatus};
use proxima_core::{Credentials, GroupId, MemoryId, OwnerRef, OwnerRoles, Role, SchemaId, UserId};
use proxima_storage_pg::{begin_owner_transaction, verbs::query::memory_page_sql_for_tests};
use sqlx::{PgConnection, PgPool, Row};
use uuid::Uuid;

struct Record {
    t: Uuid,
    owner: Uuid,
    kind: &'static str,
    schema: String,
    current: bool,
}

async fn insert_record(pool: &PgPool, handle: Uuid, row: &Record, anchor: Option<Uuid>) {
    let content = if row.kind == "fact" {
        None
    } else {
        let id = Uuid::now_v7();
        sqlx::query("INSERT INTO proxima_core.content(content_id, owner_id, schema_id, content_hash) VALUES ($1,$2,$3,$4)")
            .bind(id).bind(row.owner).bind(&row.schema).bind(row.t.as_bytes().repeat(2))
            .execute(pool).await.unwrap();
        Some(id)
    };
    sqlx::query("INSERT INTO proxima_core.memory(handle,t,kind,owner_id,schema_id,content_id,refs) VALUES ($1,$2,$3::text::proxima_core.memory_kind,$4,$5,$6,$7)")
        .bind(handle).bind(row.t).bind(row.kind).bind(row.owner).bind(&row.schema)
        .bind(content).bind(anchor.into_iter().collect::<Vec<_>>())
        .execute(pool).await.unwrap();
}

async fn insert_head(pool: &PgPool, handle: Uuid, row: &Record, t: Uuid) {
    sqlx::query("INSERT INTO proxima_core.memory_head(handle,t,kind,owner_id,schema_id) VALUES ($1,$2,$3::text::proxima_core.memory_kind,$4,$5)")
        .bind(handle).bind(t).bind(row.kind).bind(row.owner).bind(&row.schema)
        .execute(pool).await.unwrap();
}

async fn seed(pool: &PgPool, owners: &[Uuid; 3]) -> Vec<Record> {
    // Equal UUIDv7 timestamp, distinct ordered low bits: pagination must use
    // the complete t rather than lose rows at a timestamp boundary.
    let base = Uuid::now_v7().as_u128() & !0xffff;
    let mut rows = Vec::new();
    for (index, owner) in owners.iter().copied().enumerate() {
        sqlx::query("INSERT INTO proxima_core.owners(owner_id, kind) VALUES ($1, 'group')")
            .bind(owner)
            .execute(pool)
            .await
            .unwrap();
        let row = Record {
            t: Uuid::from_u128(base + u128::try_from(index).unwrap()),
            owner,
            kind: "fact",
            schema: "paging/anchor".into(),
            current: true,
        };
        insert_head(pool, row.t, &row, row.t).await;
        insert_record(pool, row.t, &row, None).await;
        rows.push(row);
    }
    for series in 0..24_u128 {
        for (index, owner) in owners.iter().copied().enumerate() {
            let kind = ["fact", "abstraction", "perspective"][usize::try_from(series % 3).unwrap()];
            let first = base + 3 + series * 6 + u128::try_from(index).unwrap() * 2;
            let handle = Uuid::now_v7();
            for revision in 0..2 {
                let row = Record {
                    t: Uuid::from_u128(first + revision),
                    owner,
                    kind,
                    schema: format!("paging/{kind}-{}", series % 2),
                    current: revision == 1,
                };
                if revision == 0 {
                    insert_head(pool, handle, &row, Uuid::from_u128(first + 1)).await;
                }
                let anchor = (kind != "fact").then_some(rows[index].t);
                insert_record(pool, handle, &row, anchor).await;
                rows.push(row);
            }
        }
    }
    // A head can exist before its memory version. These newest candidates
    // must not consume page slots ahead of the RLS-checked memory lookup.
    for index in 0..10_u128 {
        let row = Record {
            t: Uuid::from_u128(base + 5_000 + index),
            owner: owners[0],
            kind: "fact",
            schema: "paging/fact-0".into(),
            current: true,
        };
        insert_head(pool, row.t, &row, row.t).await;
    }
    rows
}

fn kind_filter(kind: Option<EntityKind>) -> Option<&'static str> {
    match kind {
        Some(EntityKind::Fact) => Some("fact"),
        Some(EntityKind::Abstraction) => Some("abstraction"),
        Some(EntityKind::Perspective) => Some("perspective"),
        Some(EntityKind::Goal) => unreachable!("separate Goal query"),
        None => None,
    }
}

fn expected(
    req: &QueryRequest,
    requested: &[Uuid],
    owners: &[Uuid; 3],
    rows: &[Record],
) -> Vec<Uuid> {
    let after = match req.page.after {
        Some(QueryCursor::Memory { memory_id, .. }) => Some(memory_id.into_inner()),
        _ => None,
    };
    let mut ids: Vec<_> = rows
        .iter()
        .filter(|row| {
            requested.contains(&row.owner)
                && (row.owner == owners[0] || (row.owner == owners[1] && row.kind == "fact"))
                && (req.supersession == SupersessionStatus::IncludeSuperseded || row.current)
                && req
                    .schema_id
                    .as_ref()
                    .is_none_or(|schema| schema.as_str() == row.schema)
                && kind_filter(req.entity_kind).is_none_or(|kind| kind == row.kind)
                && (req.memory_ids.is_empty() || req.memory_ids.contains(&MemoryId::new(row.t)))
                && after.is_none_or(|cursor| row.t < cursor)
        })
        .map(|row| row.t)
        .collect();
    ids.sort_unstable_by(|a, b| b.cmp(a));
    ids.truncate(usize::try_from(req.limit).unwrap() + usize::from(req.entity_kind.is_some()));
    ids
}

async fn page(connection: &mut PgConnection, req: &QueryRequest, owners: &[Uuid]) -> Vec<Uuid> {
    let sql = memory_page_sql_for_tests(req).unwrap();
    // SQL-POLICY: fixed-fragment — production builder uses only closed predicates and bind numbers.
    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql)).bind(owners);
    if let Some(schema) = &req.schema_id {
        query = query.bind(schema.as_str());
    }
    if let Some(kind) = kind_filter(req.entity_kind) {
        query = query.bind(kind);
    }
    if !req.memory_ids.is_empty() {
        query = query.bind(
            req.memory_ids
                .iter()
                .map(|id| id.into_inner())
                .collect::<Vec<_>>(),
        );
    }
    if let Some(QueryCursor::Memory { memory_id, .. }) = req.page.after {
        query = query.bind(memory_id.into_inner());
    }
    query
        .fetch_all(connection)
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.get("memory_id"))
        .collect()
}

async fn assert_filter_matrix(connection: &mut PgConnection, owners: &[Uuid; 3], rows: &[Record]) {
    for mask in 0..32 {
        let mut req = QueryRequest::readable();
        req.limit = 7;
        if mask & 1 != 0 {
            req.supersession = SupersessionStatus::IncludeSuperseded;
        }
        if mask & 2 != 0 {
            req.schema_id = Some(SchemaId::new("paging/fact-0".into()));
        }
        if mask & 4 != 0 {
            req.memory_ids = rows
                .iter()
                .step_by(5)
                .map(|row| MemoryId::new(row.t))
                .collect();
        }
        if mask & 8 != 0 {
            req.page.after = Some(QueryCursor::Memory {
                created_at: time::OffsetDateTime::UNIX_EPOCH,
                memory_id: MemoryId::new(rows[rows.len() / 2].t),
            });
        }
        let requested = if mask & 16 != 0 {
            vec![owners[0], owners[0], owners[1], owners[2]]
        } else {
            vec![owners[1]]
        };
        for kind in [
            None,
            Some(EntityKind::Fact),
            Some(EntityKind::Abstraction),
            Some(EntityKind::Perspective),
        ] {
            req.entity_kind = kind;
            assert_eq!(
                page(connection, &req, &requested).await,
                expected(&req, &requested, owners, rows),
                "mask={mask}, kind={kind:?}"
            );
        }
    }
    for requested in [vec![], vec![owners[2]]] {
        assert!(
            page(connection, &QueryRequest::readable(), &requested)
                .await
                .is_empty()
        );
    }
}

#[tokio::test]
async fn paged_query_preserves_filters_history_and_scope_before_limit() {
    let (database, admin, runtime, platform, platform_role, runtime_role, _) =
        super::setup_full_schema().await;
    let owners = [Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7()];
    let rows = seed(&admin, &owners).await;
    let verifier = super::SyntheticVerifier {
        roles: OwnerRoles::for_subject(
            UserId::new(Uuid::now_v7()),
            [
                (OwnerRef::Group(GroupId::new(owners[0])), Role::viewer()),
                (OwnerRef::Group(GroupId::new(owners[1])), Role::ingest()),
            ],
        )
        .unwrap(),
    };
    let authz = proxima_core::authenticate(&verifier, &Credentials::Bearer("paging-test".into()))
        .await
        .unwrap();
    let mut tx = begin_owner_transaction(&runtime, authz.owner_scope().unwrap())
        .await
        .unwrap();
    assert_filter_matrix(&mut tx, &owners, &rows).await;
    tx.rollback().await.unwrap();
    runtime.close().await;
    platform.close().await;
    super::cleanup(&database, admin, &platform_role, &runtime_role).await;
}
