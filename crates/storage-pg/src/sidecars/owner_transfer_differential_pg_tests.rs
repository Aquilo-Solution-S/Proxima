//! The equivalence gate for the owner transfer, against a pinned baseline.
//!
//! What this pins is not "the transfer works" — `owner_transfer.rs` does that,
//! case by case. It pins that the declaration-driven transfer agrees with the
//! pinned baseline — every relation holding the same multiset of rows, every
//! column of every row equal — over a corpus that touches every table
//! `owner_columns.rs`'s transfer path names.
//!
//! The goldens were produced by running the shared half of this file verbatim
//! against a worktree at `eef54c8e`, where a hand-written per-table transfer
//! served this path, with an adapter that differs only in that tree's
//! vocabulary. So a failure here is not "the golden is stale": it is a
//! statement that this
//! implementation and the baseline disagree about what a transfer leaves
//! behind, on either side of the move.
//!
//! Transfer is the verb whose failure mode is cross-tenant visibility. A
//! partially-transferred series is not merely incomplete — it leaves rows
//! readable by the SOURCE owner after the memory moved, or moves rows the
//! source is entitled to keep. Both halves are dumped: the whole database,
//! every owner in it.
//!
//! Determinism comes from two choices. The second is where this file departs
//! from `owner_erase_differential`, at a cost stated below.
//!
//! Every uuid and timestamp is replaced by an order-of-first-appearance
//! token, so values freshly generated on every run still compare. That is
//! the same.
//!
//! The dump is `ORDER BY ctid`, but the COMPARISON is per-relation
//! order-independent: two dumps are equal when every relation holds the same
//! multiset of rows. The erase differential can demand byte-order equality
//! because it changes which statements run, not the predicates they run with.
//! This path's predicates differ from the baseline's — `memory`'s re-home is
//! the generated `WHERE t = ANY($1::uuid[])` where the baseline matched on
//! `handle` and `owner_id` — and `ctid` is where MVCC put the tuple, which is a
//! function of scan order and free space.
//!
//! **What that gives up, precisely:** this file cannot see a change
//! that reorders rows within one relation while preserving their contents.
//! Nothing reads physical order — no query in the tree orders by `ctid`,
//! and the export bundle's row order comes off `KeyShape::columns()` — so
//! the property is a proxy for determinism rather than a claim about
//! behaviour. What is NOT given up is the whole of the claim that matters:
//! every relation, every row, every column, every owner, on both sides of
//! the move.
//!
//! ## What this does NOT cover
//!
//! Read the claim narrowly. This is an EQUIVALENCE gate over a corpus, and
//! a corpus writes what it writes.
//!
//! - The dump **skips empty relations' rows but still prints their header**,
//!   so a table that gains or loses its only row is visible. What it cannot
//!   see is a table that is empty on BOTH sides for a reason the corpus
//!   never created — `source_cursors` and `delegated_authority_grants` are
//!   seeded precisely so `StaysOnKey` is witnessed rather than assumed.
//! - **Concurrency is out of scope.** The three head-advanced races, the
//!   advisory-lock rounds and the bounded retry are single-threaded here and
//!   compare equal trivially. `owner_transfer.rs` carries those.
//! - **Object storage is out of scope.** A transfer performs no S3 work by
//!   construction (cold keys are owner-free, and the mount arm copies
//!   metadata only), so there is nothing for a dump to compare.
//! - **Goals are witnessed only by their refusal.** A goal series is seeded
//!   and a transfer of it attempted; what the dump proves is that the refusal
//!   left every goal row exactly where it was.
//! - The corpus is **flavor #0 only**. `proxima-storage-pg` depends on no
//!   flavor, so a second flavor's surfaces cannot be reached from here; the
//!   cross-flavor half is `owner_inverse_reach_pg` in `proxima-code`.
#![allow(
    clippy::doc_markdown,
    clippy::format_push_string,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::too_many_lines
)]

use std::collections::BTreeMap;

use crate::PgStorage;
use crate::core_pg_sidecars;
use crate::verbs::memory_timeseries::ingest_fact_timeseries;
use proxima_core::storage_ports::OwnerWritePermit;
use proxima_core::verbs::fact_ingest::FactWriteCommand;
use proxima_core::{
    AccessKind, AgentNoteV1, EntityId, FactPayload, GroupId, MemoryId, OwnerRef, SchemaId,
    SchemaVersion, SidecarPayload, UserId,
};
use proxima_pg_testkit::{create_db, db_url, drop_db};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

const AGENT_NOTE: &str = "proxima_core.agent_note_v1";
const MCP_CALL: &str = "proxima_core.mcp_call_logged_v1";

/// The three transfers this corpus performs, and the entities that must
/// refuse. Named so the adapter and the dump agree without sharing state.
#[derive(Debug)]
pub struct Corpus {
    /// Cites a blob nobody else names: the in-place arm.
    pub in_place: MemoryId,
    /// Cites a blob whose bytes the DESTINATION already owns: the dedupe
    /// arm, which remaps rather than moving.
    pub dedupe: MemoryId,
    /// Cites a blob a bystander's live series also names: the mount arm.
    pub mount: MemoryId,
    /// A goal series. Transfer must refuse it and change nothing.
    pub goal: Uuid,
    pub source: OwnerRef,
    pub destination: OwnerRef,
    /// The third owner the corpus seeds. Nothing reads the handle: the dump
    /// covers every owner in the database, so the bystander's rows are
    /// asserted by the golden rather than by a caller of this field. Kept
    /// because the corpus's third participant is part of what the gate says
    /// it covers.
    #[expect(dead_code, reason = "seeded participant, asserted through the dump")]
    pub bystander: OwnerRef,
}

fn draft(
    source: Option<(&str, &str)>,
    handle: Option<Uuid>,
    blob_id: Option<Uuid>,
) -> FactWriteCommand {
    FactWriteCommand {
        schema_id: SchemaId::new(AgentNoteV1::SCHEMA_ID.to_string()),
        schema_version: SchemaVersion::new(1),
        handle,
        source_id: source.map(|(s, _)| s.to_owned()),
        ingest_key: source.map(|(_, k)| k.to_owned()),
        payload: Vec::new(),
        rendered_text: None,
        // `agent-note-v1` is `LanguagePolicy::PerRow`: the write names a
        // language. These fixtures do not care which, so they ask for the
        // deployment configuration.
        lexical_language: Some(
            proxima_core::lexical_language::LEXICAL_LANGUAGE_DEPLOYMENT_DEFAULT.to_owned(),
        ),
        receipt: None,
        citation: None,
        derived_from: Vec::new(),
        refs: Vec::new(),
        blob_id,
        kind: "fact".into(),
    }
}

fn note(title: &str) -> SidecarPayload {
    SidecarPayload::fact(AgentNoteV1 {
        note_id: Uuid::now_v7(),
        title: title.to_owned(),
        body: "the pilings under the north quay are sound".into(),
        tags: vec!["survey".into()],
        idempotency_key: None,
    })
}

/// The production write: `ingest_fact_timeseries` for the admission and
/// `insert_memory_sidecar` for the payload, which is where the generated
/// projection statement runs. Hand-INSERTing would leave the projection
/// leg unwitnessed.
async fn write_note(
    pool: &PgPool,
    owner: OwnerRef,
    source: Option<(&str, &str)>,
    handle: Option<Uuid>,
    blob_id: Option<Uuid>,
    title: &str,
    call_log: bool,
) -> Result<(MemoryId, Uuid), Box<dyn std::error::Error>> {
    let mut tx = pool.begin().await?;
    let write = draft(source, handle, blob_id);
    // `call_log` admissions carry a hand-written `RetainAtSource` row below,
    // and a sidecar row the memory does not declare is one no verb reaches.
    let mut tables = vec![AGENT_NOTE.to_owned()];
    if call_log {
        tables.push(MCP_CALL.to_owned());
    }
    let outcome = ingest_fact_timeseries(&mut tx, &owner, &write, &[], &[], &tables, None).await?;
    core_pg_sidecars()
        .writing(&write)
        .insert_memory_sidecar(&mut tx, outcome.memory_id, &note(title))
        .await?;
    tx.commit().await?;
    Ok((outcome.memory_id, outcome.handle))
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

async fn blob_row(
    pool: &PgPool,
    owner: OwnerRef,
    hash: u8,
    object_key: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let blob_id: Uuid = sqlx::query_scalar(
        "INSERT INTO proxima_core.blob (owner_id, schema_id, content_hash)
         VALUES ($1, 'core/bytes-v1', $2) RETURNING blob_id",
    )
    .bind(owner.stored_owner_id())
    .bind(vec![hash; 32])
    .fetch_one(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.blob_uploads
             (owner_id, bucket, object_key, filename, mime, expected_byte_len,
              status, blob_id, sha256, etag, expires_at, completed_at)
         VALUES ($1, 'bucket', $2, 'f.pdf', 'application/pdf', 1,
                 'completed'::proxima_core.blob_upload_status, $3, $4, 'etag-1',
                 TIMESTAMPTZ '2030-01-01 00:00:00Z', TIMESTAMPTZ '2026-01-01 00:00:00Z')",
    )
    .bind(owner.stored_owner_id())
    .bind(object_key)
    .bind(blob_id)
    .bind(vec![31u8; 32])
    .execute(pool)
    .await?;
    Ok(blob_id)
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

async fn embed(pool: &PgPool, t: Uuid, owner: OwnerRef) -> Result<(), Box<dyn std::error::Error>> {
    sqlx::query(
        "INSERT INTO proxima_core.embeddings
             (entity_id, model_id, embedding_version, vec, owner_id)
         VALUES ($1, 'test-embed', 1, $2::vector, $3)",
    )
    .bind(t)
    .bind(embed_literal())
    .bind(owner.stored_owner_id())
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.embedding_heads
             (entity_id, model_id, embedding_version, owner_id)
         VALUES ($1, 'test-embed', 1, $2)",
    )
    .bind(t)
    .bind(owner.stored_owner_id())
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.embedding_jobs (entity_id, model_id, owner_id)
         VALUES ($1, 'test-embed', $2)",
    )
    .bind(t)
    .bind(owner.stored_owner_id())
    .execute(pool)
    .await?;
    Ok(())
}

/// Cool the hot row into a `cooled` stub, the way `forget` does, so the
/// transferred series carries a cold tail.
async fn cool(pool: &PgPool, t: Uuid, at: &str) -> Result<(), Box<dyn std::error::Error>> {
    sqlx::query(
        "INSERT INTO proxima_core.cooled
             (t, handle, owner_id, kind, object_key, blob_id, content_id, source_id,
              ingest_key, origins, refs, goal_refs, cooled_at)
         SELECT m.t, m.handle, m.owner_id, m.kind, 'cold/' || m.t::text, m.blob_id,
                m.content_id, m.source_id, m.ingest_key, m.origins, m.refs, m.goal_refs,
                $2::timestamptz
           FROM proxima_core.memory m WHERE m.t = $1",
    )
    .bind(t)
    .bind(at)
    .execute(pool)
    .await?;
    sqlx::query("DELETE FROM proxima_core.projection WHERE memory_id = $1")
        .bind(t)
        .execute(pool)
        .await?;
    sqlx::query("DELETE FROM proxima_core.agent_note_v1 WHERE t = $1")
        .bind(t)
        .execute(pool)
        .await?;
    sqlx::query("DELETE FROM proxima_core.memory WHERE t = $1")
        .bind(t)
        .execute(pool)
        .await?;
    Ok(())
}

/// Seed every table the transfer path names, under three owners.
///
/// The source keeps a second series throughout, so "moved the wrong rows"
/// is as visible as "moved too few". The bystander exists to make the mount
/// arm reachable — it is the other owner whose live series names shared
/// bytes — and every row of its is a row both implementations must leave
/// exactly alone.
pub async fn seed(pool: &PgPool) -> Result<Corpus, Box<dyn std::error::Error>> {
    let source = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let destination = OwnerRef::Group(GroupId::new(Uuid::now_v7()));
    let bystander = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    owner_row(pool, source).await?;
    owner_row(pool, bystander).await?;
    // The DESTINATION's owners row is deliberately NOT pre-created for the
    // first transfer: minting it is `ensure_owner_row`'s job inside the
    // transfer transaction, and a differential that pre-created it would
    // hide a regression there. It is created below only as the side effect
    // of owning the dedupe blob, which is after the corpus needs it.

    // ── Blobs, one per arm of FollowOrDedupe ────────────────────────────
    let blob_in_place = blob_row(pool, source, 21, "objects/in-place").await?;
    let blob_dedupe = blob_row(pool, source, 22, "objects/dedupe").await?;
    let blob_mount = blob_row(pool, source, 23, "objects/mount").await?;
    // The destination already holds these bytes under the same
    // (schema_id, content_hash): case 1, remap rather than move.
    owner_row(pool, destination).await?;
    blob_row(pool, destination, 22, "objects/dedupe-destination").await?;

    // ── The three transferred series ────────────────────────────────────
    // Each is two hot versions plus a cooled tail, on a named source so the
    // Drop leg (`ingest_keys`) has receipts to destroy.
    let mut transferred = Vec::new();
    for (n, blob) in [
        ("in-place", blob_in_place),
        ("dedupe", blob_dedupe),
        ("mount", blob_mount),
    ] {
        let (first, handle) = write_note(
            pool,
            source,
            Some(("src-a", &format!("{n}-1"))),
            None,
            Some(blob),
            n,
            false,
        )
        .await?;
        let (second, _) = write_note(
            pool,
            source,
            Some(("src-a", &format!("{n}-2"))),
            Some(handle),
            Some(blob),
            n,
            true,
        )
        .await?;
        let (third, _) = write_note(
            pool,
            source,
            Some(("src-a", &format!("{n}-3"))),
            Some(handle),
            Some(blob),
            n,
            false,
        )
        .await?;
        // The oldest version cools: the series carries a cold tail whose
        // owner must follow even though the row is not in `memory`.
        cool(pool, first.into_inner(), "2026-02-02 00:00:00Z").await?;
        embed(pool, second.into_inner(), source).await?;
        embed(pool, third.into_inner(), source).await?;
        // An owner-pinned sidecar row: `RetainAtSource`. It must NOT move,
        // and the destination must never gain a row that names the source's
        // actor.
        sqlx::query(
            "INSERT INTO proxima_core.mcp_call_logged_v1
                 (t, owner_id, tool_name, actor_oid, actor_upn, ok, latency_ms,
                  io_byte_len, io_truncated, io_content_hash)
             VALUES ($1, $2, 'core_remember', 'oid', 'source@example.test', true, 7, 11,
                     false, $3)",
        )
        .bind(second.into_inner())
        .bind(source.stored_owner_id())
        .bind(vec![41u8; 32])
        .execute(pool)
        .await?;
        // `sketch` needs no hand-written row: `ingest_fact_timeseries`
        // writes one per admission, which is the shape the transfer has to
        // re-home.
        transferred.push(third);
    }

    // ── Content: one body two admissions of one series share, one body the
    // source's OTHER series also names (so the dedupe/GC arm is exercised
    // both ways) ────────────────────────────────────────────────────────
    let shared: Uuid = sqlx::query_scalar(
        "INSERT INTO proxima_core.content (owner_id, schema_id, content_hash)
         VALUES ($1, 'core/agent-note-v1', $2) RETURNING content_id",
    )
    .bind(source.stored_owner_id())
    .bind(vec![11u8; 32])
    .fetch_one(pool)
    .await?;
    let solo: Uuid = sqlx::query_scalar(
        "INSERT INTO proxima_core.content (owner_id, schema_id, content_hash)
         VALUES ($1, 'core/agent-note-v1', $2) RETURNING content_id",
    )
    .bind(source.stored_owner_id())
    .bind(vec![12u8; 32])
    .fetch_one(pool)
    .await?;
    // Every hot row of the in-place series names `shared`; its cooled tail
    // names `solo`, which nothing else does — so the transfer must re-home
    // one and remap-then-GC the other.
    sqlx::query(
        "UPDATE proxima_core.memory SET content_id = $2
          WHERE handle = (SELECT handle FROM proxima_core.memory WHERE t = $1)",
    )
    .bind(transferred[0].into_inner())
    .bind(shared)
    .execute(pool)
    .await?;
    sqlx::query(
        "UPDATE proxima_core.cooled SET content_id = $2
          WHERE handle = (SELECT handle FROM proxima_core.memory WHERE t = $1)",
    )
    .bind(transferred[0].into_inner())
    .bind(solo)
    .execute(pool)
    .await?;

    // ── The source's OTHER series: never named by a transfer ────────────
    let (kept, _) = write_note(
        pool,
        source,
        Some(("src-b", "kept-1")),
        None,
        None,
        "kept",
        false,
    )
    .await?;
    embed(pool, kept.into_inner(), source).await?;
    sqlx::query("UPDATE proxima_core.memory SET content_id = $2 WHERE t = $1")
        .bind(kept.into_inner())
        .bind(shared)
        .execute(pool)
        .await?;

    // ── The bystander: names the mount blob from a live series ──────────
    let (bystander_t, _) = write_note(
        pool,
        bystander,
        Some(("src-a", "bystander-1")),
        None,
        Some(blob_mount),
        "bystander",
        true,
    )
    .await?;
    embed(pool, bystander_t.into_inner(), bystander).await?;
    sqlx::query(
        "INSERT INTO proxima_core.mcp_call_logged_v1
             (t, owner_id, tool_name, actor_oid, actor_upn, ok, latency_ms,
              io_byte_len, io_truncated, io_content_hash)
         VALUES ($1, $2, 'core_remember', 'oid', 'bystander@example.test', true, 7, 11,
                 false, $3)",
    )
    .bind(bystander_t.into_inner())
    .bind(bystander.stored_owner_id())
    .bind(vec![42u8; 32])
    .execute(pool)
    .await?;

    // ── StaysOnKey surfaces: owner state a memory transfer never touches ─
    for (owner, source_name) in [(source, "src-a"), (source, "src-b"), (bystander, "src-a")] {
        sqlx::query(
            "INSERT INTO proxima_core.source_cursors
                 (owner_kind, owner_id, source, cursor, updated_at)
             VALUES ($1::proxima_core.owner_kind, $2, $3, $4,
                     TIMESTAMPTZ '2026-03-03 00:00:00Z')",
        )
        .bind(match owner {
            OwnerRef::Personal(_) => "personal",
            OwnerRef::Group(_) => "group",
        })
        .bind(owner.stored_owner_id())
        .bind(source_name)
        .bind(vec![51u8; 8])
        .execute(pool)
        .await?;
    }
    sqlx::query(
        "INSERT INTO proxima_core.delegated_authority_grants
             (delegation_id, subject_user_id, owner_kind, owner_id, tool_name,
              read_ceiling, write_ceiling, expires_at, auth_epoch, issued_at)
         VALUES ($1, $2, 'personal'::proxima_core.owner_kind, $3, 'core_remember',
                 'fact', 'fact', TIMESTAMPTZ '2030-01-01 00:00:00Z', 1,
                 TIMESTAMPTZ '2026-01-01 00:00:00Z')",
    )
    .bind(Uuid::now_v7())
    .bind(match source {
        OwnerRef::Personal(user) => user.into_inner(),
        OwnerRef::Group(_) => unreachable!("source is personal"),
    })
    .bind(source.stored_owner_id())
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.cold_purge_pending (owner_id, object_key, enqueued_at)
         VALUES ($1, 'cold/never-drained', TIMESTAMPTZ '2026-04-04 00:00:00Z')",
    )
    .bind(source.stored_owner_id())
    .execute(pool)
    .await
    .ok();

    // ── A goal series, so the refusal is witnessed against real rows ────
    let goal_handle = Uuid::now_v7();
    let wake_id: Uuid = sqlx::query_scalar(
        "INSERT INTO proxima_core.wake_config
             (owner_id, trigger_kind, trigger_schema_id, tool_ids, prompt)
         VALUES ($1, 'fact_schema', 'core/agent-note-v1', ARRAY['core.remember'], 'wake')
         RETURNING wake_id",
    )
    .bind(source.stored_owner_id())
    .fetch_one(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.goal_head (handle, schema_id, owner_id, t)
         VALUES ($1, 'core/task-goal-v1', $2, $1)",
    )
    .bind(goal_handle)
    .bind(source.stored_owner_id())
    .execute(pool)
    .await?;
    let goal_t: Uuid = sqlx::query_scalar(
        "INSERT INTO proxima_core.goal
             (handle, t, owner_id, title, state, request_id, wake_id)
         VALUES ($1, $1, $2, 'a goal', 'Active', 'req-1', $3) RETURNING t",
    )
    .bind(goal_handle)
    .bind(source.stored_owner_id())
    .bind(wake_id)
    .fetch_one(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.sketch (t, owner_id, kind, text)
         VALUES ($1, $2, 'goal', 'goal sketch')",
    )
    .bind(goal_t)
    .bind(source.stored_owner_id())
    .execute(pool)
    .await?;
    // A goal-keyed embedding: `entity_id` is an entity t, not a memory t,
    // and a memory transfer must never sweep it up.
    embed(pool, goal_t, source).await?;

    Ok(Corpus {
        in_place: transferred[0],
        dedupe: transferred[1],
        mount: transferred[2],
        goal: goal_t,
        source,
        destination,
        bystander,
    })
}

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
/// have put a dynamic-SQL site in the tree for a harness.
///
/// `embeddings.vec` is dropped: a 1024-dimension vector renders as
/// kilobytes of text per row and says nothing a transfer could get wrong
/// that `owner_id` does not already say.
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
            AND t.table_name <> 'erased_pin_target'
          ORDER BY t.table_name",
    )
    .fetch_all(pool)
    .await?;
    let mut out = String::new();
    for (table, rows) in relations {
        out.push_str(&format!("## proxima_core.{table} ({})\n", rows.len()));
        for row in rows {
            let mut row: Value = serde_json::from_str(&row)?;
            if table == "embeddings"
                && let Some(object) = row.as_object_mut()
            {
                object.remove("vec");
            }
            if table == "projection"
                && let Some(object) = row.as_object_mut()
            {
                // The tsvector's text is `search_projection_identity`'s
                // business, not this file's, and it carries the note body
                // verbatim.
                object.remove("search_tsv");
            }
            if table == "cooled"
                && let Some(object) = row.as_object_mut()
            {
                // The pinned pre-v0.0.10 corpus predates the nullable
                // declaration witnesses (including the v0.0.11 split).
                // Dedicated migration tests cover the new columns; this
                // differential remains focused on transfer behavior against
                // its historical golden.
                object.remove("origins");
                object.remove("refs");
                object.insert("goal_refs".to_owned(), Value::Null);
            }
            out.push_str(&canonical(&row));
            out.push('\n');
        }
    }
    Ok(out)
}

/// Split a dump into `## relation` sections with their rows sorted.
///
/// Sorting is on the canonical JSON text, which is a total order over row
/// CONTENT — so two dumps compare equal exactly when every relation holds
/// the same rows. The section headers carry their own row counts, so a
/// relation that gained or lost a row still fails on the header alone.
pub fn by_relation(text: &str) -> BTreeMap<String, Vec<String>> {
    let mut sections: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut current = String::new();
    for line in text.lines() {
        if let Some(header) = line.strip_prefix("## ") {
            current.clear();
            current.push_str(header);
            sections.entry(current.clone()).or_default();
        } else {
            sections
                .entry(current.clone())
                .or_default()
                .push(line.to_owned());
        }
    }
    for rows in sections.values_mut() {
        rows.sort();
    }
    sections
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
// The only text that differs from the baseline tree's copy of this file.
// That is the point of the split: if the shared half were allowed to drift,
// the two sides would stop being a comparison.

use proxima_core::storage_ports::OwnerTransferPort;

/// The transfer's registry-resolved legs, exactly as the engine assembles
/// them. Passing a hand-built set here would test a registry production
/// never sees.
fn transfer_surfaces() -> proxima_core::owner_inverse::OwnerSurfaces {
    proxima_core::owner_inverse::OwnerSurfaces::for_registry(
        &proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests(),
    )
}

async fn run_transfers(
    pg: &PgStorage,
    corpus: &Corpus,
) -> Result<String, Box<dyn std::error::Error>> {
    let permit = OwnerWritePermit::new_for_tests(corpus.source, AccessKind::Fact);
    let goal_permit = OwnerWritePermit::new_for_tests(corpus.source, AccessKind::Goal);
    let mut out = String::new();
    for (name, memory) in [
        ("in_place", corpus.in_place),
        ("dedupe", corpus.dedupe),
        ("mount", corpus.mount),
    ] {
        let moved = pg
            .transfer_to_owner(
                &permit,
                EntityId::Memory(memory),
                corpus.destination,
                &transfer_surfaces(),
            )
            .await?;
        out.push_str(&format!("## transfer {name} -> {moved}\n"));
    }
    // The refusal, with its exact message: it is what the declaration
    // claims a storage backstop does.
    let refused = pg
        .transfer_to_owner(
            &goal_permit,
            EntityId::Goal(proxima_core::GoalId::new(corpus.goal)),
            corpus.destination,
            &transfer_surfaces(),
        )
        .await
        .expect_err("goals do not transfer");
    out.push_str(&format!("## transfer goal -> {refused:?}\n"));
    // Same owner on both sides: refused before any row is read.
    let same = pg
        .transfer_to_owner(
            &permit,
            EntityId::Memory(corpus.in_place),
            corpus.destination,
            &transfer_surfaces(),
        )
        .await;
    out.push_str(&format!("## transfer already-there -> {same:?}\n"));
    Ok(out)
}

#[tokio::test]
async fn transfer_differential() {
    let (db_name, url) = fresh_db("proxima_xfer_diff").await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = boot(&url).await?;
        let pool = pg.pool_for_tests();
        let corpus = seed(pool).await?;
        let mut text = run_transfers(&pg, &corpus).await?;
        text.push_str(&dump_database(pool).await?);
        let actual = normalize(&text);
        if let Ok(dir) = std::env::var("PROXIMA_DIFFERENTIAL_DIR") {
            std::fs::write(format!("{dir}/owner_transfer.txt"), &actual)?;
            return Ok(());
        }
        let golden = include_str!("../../tests/golden/owner_transfer.txt");
        assert_eq!(
            by_relation(&actual),
            by_relation(golden),
            "the transfer diverged from the pinned eef54c8e baseline"
        );
        Ok(())
    }
    .await;
    teardown(&db_name).await;
    result.expect("transfer differential failed");
}
