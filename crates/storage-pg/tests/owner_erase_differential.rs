//! The equivalence gate for the owner inverse: erase and export, against a
//! pinned baseline.
//!
//! What this pins is not "the erase works" — the lanes beside it do that. It
//! pins that the declaration-driven erase and export agree with the pinned
//! baseline, on a corpus that touches all twelve genuinely owner-scope legs
//! plus a neighbour owner and a cross-owner mounted object.
//!
//! The goldens were produced by running the shared half of this file verbatim
//! against a worktree at `fd362509`, with an adapter that differs only in that
//! tree's vocabulary. So a failure here is not "the golden is stale": it is a
//! statement that this implementation and the baseline disagree about what an
//! owner erase leaves behind, or about what an owner's bundle contains.
//!
//! Two deliberate exclusions, held out of BOTH sides: three relations the
//! baseline has and this schema does not (`compliance_audit_log`,
//! `owner_fact_retention`, `owner_legal_holds`) and one column
//! (`cold_purge_pending.compliance_operation_id`). A table that does not exist
//! cannot be compared to one that does. Everything else is compared, including
//! every row of the neighbour owner that both erases must leave untouched.
//!
//! Determinism comes from two choices. `ORDER BY ctid` is physical insertion
//! order, which is identical across two runs executing the same statements
//! in the same order; and every uuid and timestamp is replaced by an
//! order-of-first-appearance token, so values that are freshly generated on
//! every run still compare.
//!
//! ## What this does NOT cover
//!
//! Read the claim narrowly. This is an EQUIVALENCE gate over a corpus, and a
//! corpus writes what it writes.
//!
//! The dump skips empty sections, so a surface with no rows in the corpus is
//! absent from both sides and compares equal whatever happens to it. Four
//! exportable core surfaces are in that position, and that is why the
//! bundle's key set is pinned separately, against the DECLARATIONS rather
//! than against a corpus:
//! `owner_export::the_bundle_carries_every_exportable_surface_even_when_empty`
//! and the cross-flavor half in `proxima-code`'s `owner_inverse_reach_pg`.
//! Neither belongs here — regenerating these goldens would destroy their
//! provenance, which is the whole of this file's argument.
//!
//! The corpus also produces no projection rows: it writes under
//! `core/test-fact-v1`, which is unregistered and therefore projects
//! nowhere. So the `Cascade` leg — a projection row leaving with the memory
//! it derives from — is NOT witnessed here. It is carried by
//! `projection_maintenance::an_owner_erase_takes_the_owners_projection_rows_by_cascade`,
//! which erases an owner that does have projection rows, and by
//! `erase_repo_pg::every_cascade_the_contract_declares_is_a_cascade_the_schema_enforces`,
//! which asks `pg_constraint` whether every declared cascade is really
//! `confdeltype = 'c'`. Those two are the projection leg's evidence; this
//! file is silent about it.
#![allow(
    clippy::doc_markdown,
    clippy::format_push_string,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::too_many_lines
)]

use std::collections::BTreeMap;

use proxima_core::storage_ports::FactIngestPort;
use proxima_core::storage_ports::OwnerWritePermit;
use proxima_core::verbs::fact_ingest::FactWriteCommand;
use proxima_core::{AccessKind, OwnerRef, SchemaId, SchemaVersion, UserId};
use proxima_pg_testkit::{create_db, db_url, drop_db};
use proxima_storage_pg::PgStorage;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug)]
pub struct Corpus {
    pub target: UserId,
    pub neighbour: UserId,
}

fn draft(
    source: Option<(&str, &str)>,
    handle: Option<Uuid>,
    blob_id: Option<Uuid>,
) -> FactWriteCommand {
    FactWriteCommand {
        schema_id: SchemaId::new("core/test-fact-v1".to_string()),
        schema_version: SchemaVersion::new(1),
        handle,
        source_id: source.map(|(s, _)| s.to_owned()),
        ingest_key: source.map(|(_, k)| k.to_owned()),
        payload: Vec::new(),
        rendered_text: None,
        lexical_language: None,
        receipt: None,
        citation: None,
        derived_from: Vec::new(),
        refs: Vec::new(),
        blob_id,
        kind: "fact".into(),
    }
}

async fn owner_row(pool: &PgPool, owner: OwnerRef) -> Result<(), sqlx::Error> {
    let kind = match owner {
        OwnerRef::Personal(_) => "personal",
        OwnerRef::Group(_) => "group",
    };
    sqlx::query(
        "INSERT INTO proxima_core.owners (owner_id, kind)
         VALUES ($1, $2::proxima_core.owner_kind) ON CONFLICT (owner_id) DO NOTHING",
    )
    .bind(owner.stored_owner_id())
    .bind(kind)
    .execute(pool)
    .await?;
    Ok(())
}

/// Seed the twelve genuinely owner-scope legs for a target owner, plus a
/// neighbour owner whose rows every erase must leave alone, plus one shared
/// object key both owners' upload rows name (the cross-owner mount).
pub async fn seed(pg: &PgStorage) -> Result<Corpus, Box<dyn std::error::Error>> {
    let pool = pg.pool_for_tests();
    let target = UserId::new(Uuid::now_v7());
    let neighbour = UserId::new(Uuid::now_v7());
    let owner = OwnerRef::Personal(target);
    let other = OwnerRef::Personal(neighbour);
    owner_row(pool, owner).await?;
    owner_row(pool, other).await?;

    let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);
    let other_permit = OwnerWritePermit::new_for_tests(other, AccessKind::Fact);

    // Blobs: one cited by an admission, one mounted — the same object key
    // named by an upload row of each owner.
    let blob: Uuid = sqlx::query_scalar(
        "INSERT INTO proxima_core.blob (owner_id, schema_id, content_hash)
         VALUES ($1, 'core/bytes-v1', $2) RETURNING blob_id",
    )
    .bind(owner.stored_owner_id())
    .bind(vec![21u8; 32])
    .fetch_one(pool)
    .await?;
    let mounted: Uuid = sqlx::query_scalar(
        "INSERT INTO proxima_core.blob (owner_id, schema_id, content_hash)
         VALUES ($1, 'core/bytes-v1', $2) RETURNING blob_id",
    )
    .bind(owner.stored_owner_id())
    .bind(vec![22u8; 32])
    .fetch_one(pool)
    .await?;
    let neighbour_blob: Uuid = sqlx::query_scalar(
        "INSERT INTO proxima_core.blob (owner_id, schema_id, content_hash)
         VALUES ($1, 'core/bytes-v1', $2) RETURNING blob_id",
    )
    .bind(other.stored_owner_id())
    .bind(vec![22u8; 32])
    .fetch_one(pool)
    .await?;
    for (blob_id, owner_id, key, status, has_blob) in [
        (
            Some(blob),
            owner.stored_owner_id(),
            "objects/cited",
            "completed",
            true,
        ),
        (
            Some(mounted),
            owner.stored_owner_id(),
            "objects/mounted",
            "completed",
            true,
        ),
        (
            Some(neighbour_blob),
            other.stored_owner_id(),
            "objects/mounted",
            "completed",
            true,
        ),
        (
            None,
            owner.stored_owner_id(),
            "objects/pending",
            "pending",
            false,
        ),
    ] {
        sqlx::query(
            "INSERT INTO proxima_core.blob_uploads
                 (owner_id, bucket, object_key, filename, mime, expected_byte_len,
                  status, blob_id, sha256, expires_at, completed_at)
             VALUES ($1, 'bucket', $2, 'f.pdf', 'application/pdf', 1,
                     $3::proxima_core.blob_upload_status, $4, $5,
                     TIMESTAMPTZ '2030-01-01 00:00:00Z',
                     CASE WHEN $6 THEN TIMESTAMPTZ '2026-01-01 00:00:00Z' ELSE NULL END)",
        )
        .bind(owner_id)
        .bind(key)
        .bind(status)
        .bind(blob_id)
        .bind(vec![31u8; 32])
        .bind(has_blob)
        .execute(pool)
        .await?;
    }
    // A two-version series on source `src-a`, a single admission on `src-b`,
    // and one neighbour admission on `src-a`.
    let first = pg
        .ingest_fact_atomic(&permit, &draft(Some(("src-a", "k1")), None, None), None)
        .await?;
    let handle = first.handle;
    let second = pg
        .ingest_fact_atomic(
            &permit,
            &draft(Some(("src-a", "k2")), Some(handle), Some(blob)),
            None,
        )
        .await?;
    let third = pg
        .ingest_fact_atomic(&permit, &draft(Some(("src-b", "k3")), None, None), None)
        .await?;
    let neighbour_memory = pg
        .ingest_fact_atomic(
            &other_permit,
            &draft(Some(("src-a", "n1")), None, None),
            None,
        )
        .await?;

    let fourth = pg
        .ingest_fact_atomic(&permit, &draft(Some(("src-a", "k4")), None, None), None)
        .await?;
    let fifth = pg
        .ingest_fact_atomic(&permit, &draft(Some(("src-b", "k5")), None, None), None)
        .await?;

    let t4 = fourth.memory_id.into_inner();
    let t5 = fifth.memory_id.into_inner();
    let t1 = first.memory_id.into_inner();
    let t2 = second.memory_id.into_inner();
    let t3 = third.memory_id.into_inner();
    let tn = neighbour_memory.memory_id.into_inner();

    // Content: one body shared by both `src-a` versions (owner erase GCs it,
    // source erase of `src-a` GCs it too), one shared across sources (source
    // erase must NOT GC it), one held by the neighbour.
    let shared_a: Uuid = sqlx::query_scalar(
        "INSERT INTO proxima_core.content (owner_id, schema_id, content_hash)
         VALUES ($1, 'core/test-fact-v1', $2) RETURNING content_id",
    )
    .bind(owner.stored_owner_id())
    .bind(vec![11u8; 32])
    .fetch_one(pool)
    .await?;
    let cross_source: Uuid = sqlx::query_scalar(
        "INSERT INTO proxima_core.content (owner_id, schema_id, content_hash)
         VALUES ($1, 'core/test-fact-v1', $2) RETURNING content_id",
    )
    .bind(owner.stored_owner_id())
    .bind(vec![12u8; 32])
    .fetch_one(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.content (owner_id, schema_id, content_hash)
         VALUES ($1, 'core/test-fact-v1', $2)",
    )
    .bind(other.stored_owner_id())
    .bind(vec![13u8; 32])
    .execute(pool)
    .await?;
    sqlx::query("UPDATE proxima_core.memory SET content_id = $2 WHERE t = ANY($1::uuid[])")
        .bind(vec![t1, t2])
        .bind(shared_a)
        .execute(pool)
        .await?;
    sqlx::query("UPDATE proxima_core.memory SET content_id = $2 WHERE t = $1")
        .bind(t3)
        .bind(cross_source)
        .execute(pool)
        .await?;
    // `cross_source` is also named by a `src-a` admission, so a source-scope
    // erase of `src-a` must leave it: the anti-join, not the selection set,
    // is what decides.
    sqlx::query("UPDATE proxima_core.memory SET content_id = $2 WHERE t = $1")
        .bind(t1)
        .bind(cross_source)
        .execute(pool)
        .await?;

    // Memory-keyed sidecar rows (the registry sweep) and owner-pinned rows
    // (the sidecar's own owner_id).
    for (t, note) in [(t1, "one"), (t2, "two"), (t3, "three")] {
        sqlx::query(
            "INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body)
             VALUES ($1, $1, $2, 'body')",
        )
        .bind(t)
        .bind(note)
        .execute(pool)
        .await?;
    }
    sqlx::query(
        "INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body)
         VALUES ($1, $1, 'neighbour', 'body')",
    )
    .bind(tn)
    .execute(pool)
    .await?;
    for (t, owner_id, upn) in [
        (t1, owner.stored_owner_id(), "target@example.test"),
        (t3, owner.stored_owner_id(), "target@example.test"),
        (tn, other.stored_owner_id(), "neighbour@example.test"),
    ] {
        sqlx::query(
            "INSERT INTO proxima_core.mcp_call_logged_v1
                 (t, owner_id, tool_name, actor_oid, actor_upn, ok, latency_ms,
                  io_byte_len, io_truncated, io_content_hash)
             VALUES ($1, $2, 'core_remember', 'oid', $3, true, 7, 11, false, $4)",
        )
        .bind(t)
        .bind(owner_id)
        .bind(upn)
        .bind(vec![41u8; 32])
        .execute(pool)
        .await?;
    }

    // Two cooled admissions — one per source — so both erase scopes carry the
    // locator manifest into the bundle and the cold-purge debt into the queue.
    // Cooling is what `forget` does: the cooled stub replaces the hot row.
    for (t, at) in [(t4, "2026-02-02 00:00:00Z"), (t5, "2026-02-03 00:00:00Z")] {
        sqlx::query(
            "INSERT INTO proxima_core.cooled
                 (t, handle, owner_id, kind, object_key, source_id, ingest_key, cooled_at)
             SELECT m.t, m.handle, m.owner_id, m.kind, 'cold/' || m.t::text, m.source_id,
                    m.ingest_key, $2::timestamptz
               FROM proxima_core.memory m WHERE m.t = $1",
        )
        .bind(t)
        .bind(at)
        .execute(pool)
        .await?;
        sqlx::query("DELETE FROM proxima_core.memory WHERE t = $1")
            .bind(t)
            .execute(pool)
            .await?;
    }

    // Embedding infrastructure.
    for t in [t1, t2, tn] {
        sqlx::query(
            "INSERT INTO proxima_core.embeddings
                 (entity_id, model_id, embedding_version, vec, owner_id)
             VALUES ($1, 'test-embed', 1, $2::vector, $3)",
        )
        .bind(t)
        .bind(embed_literal())
        .bind(if t == tn {
            other.stored_owner_id()
        } else {
            owner.stored_owner_id()
        })
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.embedding_heads
                 (entity_id, model_id, embedding_version, owner_id)
             VALUES ($1, 'test-embed', 1, $2)",
        )
        .bind(t)
        .bind(if t == tn {
            other.stored_owner_id()
        } else {
            owner.stored_owner_id()
        })
        .execute(pool)
        .await?;
    }

    // Goals, goal heads, wake configs.
    for (owner_ref, request, prompt) in [
        (owner, "req-1", "target prompt"),
        (owner, "req-2", "target prompt two"),
        (other, "req-n", "neighbour prompt"),
    ] {
        let wake_id: Uuid = sqlx::query_scalar(
            "INSERT INTO proxima_core.wake_config
                 (owner_id, trigger_kind, trigger_schema_id, tool_ids, prompt)
             VALUES ($1, 'fact_schema', 'core/test-fact-v1', ARRAY['core.remember'], $2)
             RETURNING wake_id",
        )
        .bind(owner_ref.stored_owner_id())
        .bind(prompt)
        .fetch_one(pool)
        .await?;
        let goal_handle = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO proxima_core.goal_head (handle, schema_id, owner_id, t)
             VALUES ($1, 'core/task-goal-v1', $2, $1)",
        )
        .bind(goal_handle)
        .bind(owner_ref.stored_owner_id())
        .execute(pool)
        .await?;
        let goal_t: Uuid = sqlx::query_scalar(
            "INSERT INTO proxima_core.goal
                 (handle, t, owner_id, title, state, request_id, wake_id)
             VALUES ($1, $1, $2, 'a goal', 'Active', $3, $4) RETURNING t",
        )
        .bind(goal_handle)
        .bind(owner_ref.stored_owner_id())
        .bind(request)
        .bind(wake_id)
        .fetch_one(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.task_goal_v1 (t, due_at)
             VALUES ($1, TIMESTAMPTZ '2027-01-01 00:00:00Z')",
        )
        .bind(goal_t)
        .execute(pool)
        .await
        .ok();
        sqlx::query(
            "INSERT INTO proxima_core.sketch (t, owner_id, kind, text)
             VALUES ($1, $2, 'goal', 'goal sketch')",
        )
        .bind(goal_t)
        .bind(owner_ref.stored_owner_id())
        .execute(pool)
        .await
        .ok();
    }

    // Source cursors and delegated authority.
    for (owner_ref, source) in [(owner, "src-a"), (owner, "src-b"), (other, "src-a")] {
        sqlx::query(
            "INSERT INTO proxima_core.source_cursors
                 (owner_kind, owner_id, source, cursor, updated_at)
             VALUES ($1::proxima_core.owner_kind, $2, $3, $4,
                     TIMESTAMPTZ '2026-03-03 00:00:00Z')",
        )
        .bind(match owner_ref {
            OwnerRef::Personal(_) => "personal",
            OwnerRef::Group(_) => "group",
        })
        .bind(owner_ref.stored_owner_id())
        .bind(source)
        .bind(vec![51u8; 8])
        .execute(pool)
        .await?;
    }
    for (subject, holder) in [(target, owner), (target, other), (neighbour, other)] {
        sqlx::query(
            "INSERT INTO proxima_core.delegated_authority_grants
                 (delegation_id, subject_user_id, owner_kind, owner_id, tool_name,
                  read_ceiling, write_ceiling, expires_at, auth_epoch, issued_at)
             VALUES ($1, $2, $3::proxima_core.owner_kind, $4, 'core_remember',
                     'fact', 'fact', TIMESTAMPTZ '2030-01-01 00:00:00Z', 1,
                     TIMESTAMPTZ '2026-01-01 00:00:00Z')",
        )
        .bind(Uuid::now_v7())
        .bind(subject.into_inner())
        .bind(match holder {
            OwnerRef::Personal(_) => "personal",
            OwnerRef::Group(_) => "group",
        })
        .bind(holder.stored_owner_id())
        .execute(pool)
        .await?;
    }

    Ok(Corpus { target, neighbour })
}

fn embed_literal() -> String {
    format!(
        "[{}]",
        std::iter::once("1")
            .chain(std::iter::repeat_n("0", 1023))
            .collect::<Vec<_>>()
            .join(",")
    )
}

/// Relations and columns the baseline has and this schema does not. They are
/// held out of BOTH sides of the differential: the question this harness
/// answers is whether the surviving rows are the same, and a table that does
/// not exist cannot be compared to one that does. Every other difference is a
/// failure.
pub const DROPPED_TABLES: &[&str] = &[
    "compliance_audit_log",
    "owner_fact_retention",
    "owner_legal_holds",
];
pub const DROPPED_COLUMNS: &[(&str, &str)] = &[("cold_purge_pending", "compliance_operation_id")];

/// Every base relation of `proxima_core`, dumped in physical insertion order
/// and normalized: identifiers and timestamps become order-of-appearance
/// tokens, so two runs that did the same thing to the same logical corpus
/// produce the same bytes.
///
/// ONE statement, and no SQL built in Rust. The per-relation query is
/// assembled by `format(..., %I)` INSIDE Postgres, where `%I` is the
/// server's own identifier quoting, and `query_to_xml` runs it; `xpath`
/// then pulls the `to_jsonb` text back out in row order. The obvious
/// spelling — a `format!` per table name from `information_schema` — would
/// have put two dynamic-SQL sites in the tree for a harness, and the whole
/// point of a harness is that it costs nothing to keep.
pub async fn dump_database(pool: &PgPool) -> Result<String, Box<dyn std::error::Error>> {
    let relations: Vec<(String, Vec<String>)> = sqlx::query_as(
        "SELECT t.table_name::text,
                (xpath(
                    '/table/row/to_jsonb/text()',
                    query_to_xml(
                        format(
                            'SELECT to_jsonb(x) FROM proxima_core.%I x ORDER BY x.ctid',
                            t.table_name
                        ),
                        false, false, ''
                    )
                ))::text[]
           FROM information_schema.tables t
          WHERE t.table_schema = 'proxima_core' AND t.table_type = 'BASE TABLE'
          ORDER BY t.table_name",
    )
    .fetch_all(pool)
    .await?;
    let mut out = String::new();
    for (table, rows) in relations {
        if DROPPED_TABLES.contains(&table.as_str()) {
            continue;
        }
        out.push_str(&format!("## proxima_core.{table} ({})\n", rows.len()));
        for row in rows {
            let mut row: Value = serde_json::from_str(&row)?;
            for (dropped_table, column) in DROPPED_COLUMNS {
                if *dropped_table == table
                    && let Some(object) = row.as_object_mut()
                {
                    object.remove(*column);
                }
            }
            out.push_str(&canonical(&row));
            out.push('\n');
        }
    }
    Ok(out)
}

pub fn canonical(value: &Value) -> String {
    String::from_utf8(proxima_core::canonical_json_bytes(value)).expect("utf8")
}

/// Replace every uuid and timestamp with an order-of-appearance token.
pub fn normalize(text: &str) -> String {
    let mut ids: BTreeMap<String, String> = BTreeMap::new();
    let mut order: Vec<(String, String)> = Vec::new();
    let mut out = String::with_capacity(text.len());
    let bytes: Vec<char> = text.chars().collect();
    let mut i = 0usize;
    while i < bytes.len() {
        if let Some(len) = uuid_at(&bytes, i) {
            let raw: String = bytes[i..i + len].iter().collect();
            let token = ids.entry(raw.clone()).or_insert_with(|| {
                let token = format!("<id{:03}>", order.len() + 1);
                order.push((raw.clone(), token.clone()));
                token
            });
            out.push_str(token);
            i += len;
            continue;
        }
        if let Some(len) = timestamp_at(&bytes, i) {
            let raw: String = bytes[i..i + len].iter().collect();
            let token = ids.entry(raw.clone()).or_insert_with(|| {
                let token = format!("<ts{:03}>", order.len() + 1);
                order.push((raw.clone(), token.clone()));
                token
            });
            out.push_str(token);
            i += len;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

fn uuid_at(chars: &[char], at: usize) -> Option<usize> {
    const SHAPE: [usize; 5] = [8, 4, 4, 4, 12];
    let mut i = at;
    for (n, group) in SHAPE.iter().enumerate() {
        if n > 0 {
            if chars.get(i) != Some(&'-') {
                return None;
            }
            i += 1;
        }
        for _ in 0..*group {
            match chars.get(i) {
                Some(c) if c.is_ascii_hexdigit() => i += 1,
                _ => return None,
            }
        }
    }
    if chars.get(i).is_some_and(char::is_ascii_hexdigit) {
        return None;
    }
    Some(i - at)
}

fn timestamp_at(chars: &[char], at: usize) -> Option<usize> {
    // 2026-01-01T00:00:00...  up to the closing quote.
    let digits =
        |i: usize, n: usize| (0..n).all(|k| chars.get(i + k).is_some_and(char::is_ascii_digit));
    if !(digits(at, 4) && chars.get(at + 4) == Some(&'-') && digits(at + 5, 2)) {
        return None;
    }
    let mut i = at;
    while let Some(c) = chars.get(i) {
        if *c == '"' {
            break;
        }
        i += 1;
    }
    Some(i - at)
}

pub async fn fresh_db(prefix: &str) -> (String, String) {
    let db_name = format!("{prefix}_{}", Uuid::now_v7().simple());
    create_db(&db_name)
        .await
        .expect("PG required: admin connect failed");
    let url = db_url(&db_name);
    (db_name, url)
}

pub async fn boot(url: &str) -> Result<PgStorage, Box<dyn std::error::Error>> {
    let pg = PgStorage::connect(url).await?;
    pg.run_migrations().await?;
    Ok(pg)
}

pub async fn teardown(db_name: &str) {
    let _ = drop_db(db_name).await;
}

// ── ADAPTER (the only half that may differ from the baseline) ───────────
//
// The only text that differs from the baseline worktree's copy of this
// file. That is the point of the split: if the shared half were allowed to
// drift, the two sides would stop being a comparison.

use proxima_core::owner_inverse::{
    EraseAuthorization, ExportAuthorization, OwnerEraseTarget, OwnerExportTarget, OwnerSurfaces,
};
use proxima_core::storage_ports::OwnerInversePort;

fn tables() -> OwnerSurfaces {
    OwnerSurfaces::for_registry(&proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests())
}

async fn export_dump(pg: &PgStorage, user: UserId) -> Result<String, Box<dyn std::error::Error>> {
    let auth =
        ExportAuthorization::new_for_tests(OwnerExportTarget::PersonalOwner { user_id: user });
    let bundle = pg.export_owner_bundle(&auth, &tables()).await?;
    let mut sections: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for (table, rows) in &bundle.tables {
        sections.insert(table.clone(), rows.clone());
    }
    sections.insert("edges".into(), bundle.edges.clone());
    let mut out = String::new();
    for (table, rows) in sections {
        if rows.is_empty() {
            continue;
        }
        out.push_str(&format!("## export {table} ({})\n", rows.len()));
        for row in rows {
            out.push_str(&canonical(&row));
            out.push('\n');
        }
    }
    Ok(out)
}

async fn scenario(source_scope: Option<&str>, out_key: &str) {
    let (db_name, url) = fresh_db("proxima_diff").await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = boot(&url).await?;
        let pool = pg.pool_for_tests();
        let corpus = seed(&pg).await?;
        let mut text = export_dump(&pg, corpus.target).await?;
        match source_scope {
            None => {
                let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalOwner {
                    user_id: corpus.target,
                    drop_event_id: "diff".into(),
                });
                let outcome = pg
                    .erase_personal_owner(&auth, corpus.target, false, &tables())
                    .await?;
                text.push_str(&format!("## outcome {}\n", outcome_shape(&outcome)));
            }
            Some(source) => {
                let source_id = proxima_core::SourceId::new(source.to_owned());
                let auth =
                    EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalSourceScope {
                        user_id: corpus.target,
                        source_id: source_id.clone(),
                        drop_event_id: "diff".into(),
                    });
                let outcome = pg
                    .erase_personal_source_scope(&auth, corpus.target, &source_id, &tables())
                    .await?;
                text.push_str(&format!("## outcome {}\n", outcome_shape(&outcome)));
            }
        }
        text.push_str(&dump_database(pool).await?);
        let actual = normalize(&text);
        if let Ok(dir) = std::env::var("PROXIMA_DIFFERENTIAL_DIR") {
            std::fs::write(format!("{dir}/{out_key}.txt"), &actual)?;
            return Ok(());
        }
        let golden = match out_key {
            "owner_scope" => include_str!("golden/owner_erase_owner_scope.txt"),
            _ => include_str!("golden/owner_erase_source_scope.txt"),
        };
        assert_eq!(
            actual, golden,
            "the {out_key} inverse diverged from the pinned fd362509 baseline"
        );
        Ok(())
    }
    .await;
    teardown(&db_name).await;
    result.expect("differential scenario failed");
}

fn outcome_shape(outcome: &proxima_core::owner_inverse::OwnerEraseOutcome) -> String {
    match outcome {
        proxima_core::owner_inverse::OwnerEraseOutcome::Completed {
            cited_object_purge_pending,
            cold_object_purge_pending,
            ..
        } => format!(
            "Completed cited_pending={cited_object_purge_pending} cold_pending={cold_object_purge_pending}"
        ),
        other => format!("{other:?}"),
    }
}

#[tokio::test]
async fn owner_scope_differential() {
    scenario(None, "owner_scope").await;
}

#[tokio::test]
async fn source_scope_differential() {
    scenario(Some("src-a"), "source_scope").await;
}
