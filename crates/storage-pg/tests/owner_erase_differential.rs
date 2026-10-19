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
//! Two deliberate exclusions, held out of BOTH sides: four relations the
//! baseline has and this schema does not (`compliance_audit_log`, `cooled`,
//! `owner_fact_retention`, `owner_legal_holds`) and one column
//! (`cold_purge_pending.compliance_operation_id`). A surface that does not exist on both
//! sides cannot be compared. Everything else is compared, including every row
//! of the neighbour owner that both erases must leave untouched.
//!
//! Determinism comes from two choices. `ORDER BY ctid` is physical insertion
//! order, which is identical across two runs executing the same statements
//! in the same order; and every uuid and timestamp is replaced by an
//! order-of-first-appearance token, so values that are freshly generated on
//! every run still compare.
//!
//! ## The one regeneration, and what was checked before it
//!
//! The corpus used to admit with `ingest_fact_atomic` and then INSERT its
//! sidecar rows by hand. `assert_memory_declares_sidecar` makes that corpus
//! unwritable — a sidecar row whose memory does not declare the table is
//! refused — so the four admissions that carry sidecar rows now go through
//! `ingest_fact_with_typed_sidecar`, which writes the rows and the stamp as
//! one act. A typed payload carries its own `note_id`, so four uuids that
//! used to equal their memory's `t` are now distinct, and every
//! order-of-first-appearance token after them shifts.
//!
//! ## The second regeneration (issue #305)
//!
//! The corpus produced no `publication_outbox` rows, so the section compared
//! equal at `(0)` whatever an erase did to it — the outbox was inside the
//! harness's blind spot described below. Two admissions now carry the
//! publication plan core resolves for a listenable schema: the target's and
//! the neighbour's. The owner-scope golden therefore witnesses
//! `EraseRule::ByOwner` destroying one record and leaving the other, and the
//! source-scope golden witnesses that a source-scope erase leaves both.
//!
//! Checked, not assumed: strip the new `publication_outbox` rows from the new
//! files, re-canonicalise the order-of-first-appearance tokens, and the result
//! is byte-identical to the previous goldens. Nothing else moved; the new rows
//! shifted the token numbering after them and that is the whole of the diff.
//!
//! `envelope` and `envelope_digest` are dropped from the dump. The sealed
//! `CloudEvents` document renders as a hex `bytea` carrying a wall-clock
//! `time` INSIDE the blob, where the uuid/timestamp tokenizer cannot reach it,
//! so two runs of the same corpus would differ in those bytes. What the bytes
//! say is pinned by `publication_outbox_pg_tests`; what this file asks is
//! whether the ROW survives, which `t`, `owner_id`, `event_id` and `state`
//! answer.
//!
//! The goldens were regenerated for that and nothing else, and the claim was
//! checked rather than assumed. Against the pre-trigger baseline: the
//! section list and every per-section row count are identical (no row
//! appeared, none vanished), and once `note_id` is removed and
//! `sidecar_tables` blanked, re-canonicalising the tokens reproduces the old
//! file byte for byte — so every value and every id-ALIASING (which rows
//! share a content row, which share a handle) is unchanged. The equivalence
//! this file pins is therefore the same equivalence; only the corpus's
//! spelling moved onto the port.
//!
//! Issue #496 changes the surviving publication's `event_id` from a
//! kind-prefixed handle to its bare UUID. The two snapshot lines change only
//! that field's spelling; their row counts, owner scope, and ID aliasing stay
//! pinned to the same corpus. No other snapshot content was regenerated.
//!
//! Issue #488 makes forget keep the memory row instead of writing a cooled
//! stub, and erase and export stop reading `cooled`. The corpus's two cooled
//! admissions (`k4`, `k5`) are now forgotten through the real verb. Checked,
//! not assumed: with every token replaced by one placeholder, the only diff
//! is each cooled line becoming a `memory` line with `forgotten_at` set, and
//! the section counts moving with them. Nothing else moved.
//!
//! Issue #482 names the object store on every upload row
//! (`blob_uploads.object_store`). Checked, not assumed: with every token
//! replaced by one placeholder and that one field removed, both files are
//! line-identical to the previous goldens.
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
//! and the cross-flavor half in the facade's `owner_inverse_reach_pg`, which
//! composes core with the example flavor.
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
use std::sync::Arc;

use proxima_core::publication::{
    PublicationDraft, PublicationExtensions, PublicationLimits, PublicationPlan, PublicationSource,
};
use proxima_core::storage_ports::FactIngestPort;
use proxima_core::storage_ports::MemoryAuthoringPort;
use proxima_core::storage_ports::OwnerWritePermit;
use proxima_core::verbs::fact_ingest::{AuthorizedFactWrite, FactWriteCommand};
use proxima_core::{
    AccessKind, AgentNoteV1, ColdObjectStore, OwnerId, SchemaId, SchemaVersion, SidecarPayload,
    StorageError, UserId,
};
use proxima_pg_testkit::{db_url, drop_db};
use proxima_storage_pg::PgStorage;
use proxima_storage_pg::test_fixtures::create_core_db;
use proxima_storage_pg::verbs::forget::MemoryColdStore;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

/// The pinned corpus names every cited upload's synthetic store `s3:test/bucket`.
/// Model that store explicitly while retaining its in-memory test transport.
#[derive(Default)]
struct CorpusColdStore(MemoryColdStore);

#[async_trait::async_trait]
impl ColdObjectStore for CorpusColdStore {
    fn store_id(&self) -> &proxima_core::ObjectStoreId {
        static ID: std::sync::LazyLock<proxima_core::ObjectStoreId> =
            std::sync::LazyLock::new(|| {
                proxima_core::ObjectStoreId::parse("s3:test/bucket").expect("test store id")
            });
        &ID
    }

    async fn delete(&self, key: &str) -> Result<(), StorageError> {
        self.0.delete(key).await
    }
}

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
        occurred_at: None,
        citation: None,
        additional_references: Vec::new(),
        refs: Vec::new(),
        blob_id,
        kind: "fact".into(),
        request: None,
    }
}

// Keep these direct-SQL goals after the runtime Memory UUIDv7 values: a
// PostgreSQL/Rust same-millisecond ordering can otherwise flake the golden
// export while the corpus still has the same logical insertion order.
const FIXTURE_GOAL_HANDLES: [Uuid; 3] = [
    Uuid::from_u128(0xffff_ffff_ffff_7000_8000_0000_0000_0001),
    Uuid::from_u128(0xffff_ffff_ffff_7000_8000_0000_0000_0002),
    Uuid::from_u128(0xffff_ffff_ffff_7000_8000_0000_0000_0003),
];

/// The corpus's admissions, as the write port takes them.
///
/// `lexical_language` is set here rather than in [`draft`]: `agent-note-v1`
/// declares `LanguagePolicy::PerRow`, so a write carrying that payload has
/// to name a configuration or the port refuses it. Nothing in this baseline
/// reads it — the corpus writes under an unregistered `schema_id` and so
/// projects nowhere (see the module docs) — but the port is entitled to ask.
fn authorized(owner: OwnerId, mut draft: FactWriteCommand) -> AuthorizedFactWrite {
    draft.lexical_language =
        Some(proxima_core::lexical_language::LEXICAL_LANGUAGE_DEPLOYMENT_DEFAULT.to_owned());
    AuthorizedFactWrite::new_for_tests(
        OwnerWritePermit::new_for_tests(owner, AccessKind::Fact),
        draft,
        None,
        Vec::new(),
    )
}

fn authorized_with_sidecars(
    owner: OwnerId,
    draft: FactWriteCommand,
    sidecars: &[SidecarPayload],
) -> AuthorizedFactWrite {
    authorized(owner, draft).with_sidecar_payloads_for_tests(sidecars.to_vec())
}

/// Attach the capture instruction core resolves for a LISTENABLE schema.
///
/// The corpus's own `core/test-fact-v1` stands in for one. Storage never
/// consults a registry — listenability reaches it as the plan travelling on
/// the witness, exactly as in production — so a corpus that carries the plan
/// carries the real thing. Two admissions get one: the target's, which an
/// owner erase must destroy, and the neighbour's, which it must not touch.
fn published(owner: OwnerId, note: &str, write: AuthorizedFactWrite) -> AuthorizedFactWrite {
    let source = PublicationSource::new("urn:proxima:owner-differential")
        .expect("a URN is an absolute source");
    write.with_publication_for_tests(PublicationPlan::new(
        PublicationDraft::new(
            SchemaId::new("core/test-fact-v1".to_owned()),
            SchemaVersion::new(1),
            source,
            owner,
            Some("trusted/runner".to_owned()),
            PublicationExtensions::new(),
            serde_json::json!({ "note": note }),
        ),
        PublicationLimits::default(),
    ))
}

/// The memory-keyed sidecar row: one per admission that carries a note.
fn note(title: &str) -> SidecarPayload {
    SidecarPayload::fact(AgentNoteV1 {
        note_id: Uuid::now_v7(),
        title: title.to_owned(),
        body: "body".to_owned(),
        tags: Vec::new(),
        idempotency_key: None,
    })
}

async fn owner_row(pool: &PgPool, owner: OwnerId) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO proxima_core.owners (owner_id, kind)
         VALUES ($1, 'personal'::proxima_core.owner_kind) ON CONFLICT (owner_id) DO NOTHING",
    )
    .bind(owner.into_inner())
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
    let owner = OwnerId::from(target);
    let other = OwnerId::from(neighbour);
    owner_row(pool, owner).await?;
    owner_row(pool, other).await?;

    let permit = OwnerWritePermit::new_for_tests(owner, AccessKind::Fact);

    // Blobs: one cited by an admission, one mounted — the same object key
    // named by an upload row of each owner.
    let blob: Uuid = sqlx::query_scalar(
        "INSERT INTO proxima_core.blob (owner_id, schema_id, content_hash)
         VALUES ($1, 'core/bytes-v1', $2) RETURNING blob_id",
    )
    .bind(owner.into_inner())
    .bind(vec![21u8; 32])
    .fetch_one(pool)
    .await?;
    let mounted: Uuid = sqlx::query_scalar(
        "INSERT INTO proxima_core.blob (owner_id, schema_id, content_hash)
         VALUES ($1, 'core/bytes-v1', $2) RETURNING blob_id",
    )
    .bind(owner.into_inner())
    .bind(vec![22u8; 32])
    .fetch_one(pool)
    .await?;
    let neighbour_blob: Uuid = sqlx::query_scalar(
        "INSERT INTO proxima_core.blob (owner_id, schema_id, content_hash)
         VALUES ($1, 'core/bytes-v1', $2) RETURNING blob_id",
    )
    .bind(other.into_inner())
    .bind(vec![22u8; 32])
    .fetch_one(pool)
    .await?;
    for (blob_id, owner_id, key, status, has_blob) in [
        (
            Some(blob),
            owner.into_inner(),
            "objects/cited",
            "completed",
            true,
        ),
        (
            Some(mounted),
            owner.into_inner(),
            "objects/mounted",
            "completed",
            true,
        ),
        (
            Some(neighbour_blob),
            other.into_inner(),
            "objects/mounted",
            "completed",
            true,
        ),
        (
            None,
            owner.into_inner(),
            "objects/pending",
            "pending",
            false,
        ),
    ] {
        sqlx::query(
            "INSERT INTO proxima_core.blob_uploads
                 (owner_id, bucket, object_store, object_key, filename, mime, expected_byte_len,
                  status, blob_id, sha256, expires_at, completed_at)
             VALUES ($1, 'bucket', 's3:test/bucket', $2, 'f.pdf', 'application/pdf', 1,
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
    //
    // Through the WRITE PORT, with the sidecar rows as typed payloads. The
    // corpus used to admit with `ingest_fact_atomic` and then INSERT its
    // sidecar rows by hand, which declares nothing: those rows are ones the
    // sidecar sweep, forget and export all walk past, and
    // `assert_memory_declares_sidecar` now refuses them outright. The port
    // stamps `memory.sidecar_tables` from the payloads it routes, so the
    // stamp and the rows are one statement of one fact rather than two.
    let first = pg
        .ingest_fact_with_typed_sidecar(
            &published(
                owner,
                "one",
                authorized_with_sidecars(
                    owner,
                    draft(Some(("src-a", "k1")), None, None),
                    &[note("one")],
                ),
            ),
            &[],
        )
        .await?;
    let handle = first.handle;
    let second = pg
        .ingest_fact_with_typed_sidecar(
            &authorized_with_sidecars(
                owner,
                draft(Some(("src-a", "k2")), Some(handle), Some(blob)),
                &[note("two")],
            ),
            &[],
        )
        .await?;
    let third = pg
        .ingest_fact_with_typed_sidecar(
            &authorized_with_sidecars(
                owner,
                draft(Some(("src-b", "k3")), None, None),
                &[note("three")],
            ),
            &[],
        )
        .await?;
    let neighbour_memory = pg
        .ingest_fact_with_typed_sidecar(
            &published(
                other,
                "neighbour",
                authorized_with_sidecars(
                    other,
                    draft(Some(("src-a", "n1")), None, None),
                    &[note("neighbour")],
                ),
            ),
            &[],
        )
        .await?;

    let fourth = pg
        .ingest_fact_atomic(&permit, &draft(Some(("src-a", "k4")), None, None), None)
        .await?;
    let fifth = pg
        .ingest_fact_atomic(&permit, &draft(Some(("src-b", "k5")), None, None), None)
        .await?;

    let t1 = first.memory_id.into_inner();
    let t2 = second.memory_id.into_inner();
    let t3 = third.memory_id.into_inner();
    let tn = neighbour_memory.memory_id.into_inner();

    // The write port mints one Content row per write, hashed over the
    // payloads it just routed. That is correct and is not what this baseline
    // is about: the corpus declares its OWN content graph below — one body
    // shared by two admissions, one shared across sources, one held by the
    // neighbour and named by nothing — because that graph is what the
    // content-GC legs of an erase are tested against. Remember what the port
    // minted so those four rows can be dropped once the memories are
    // repointed off them; leaving them would compare the write path's
    // bookkeeping instead of the corpus.
    let minted_content: Vec<Uuid> = sqlx::query_scalar(
        "SELECT content_id FROM proxima_core.memory
          WHERE t = ANY($1::uuid[]) AND content_id IS NOT NULL",
    )
    .bind(vec![t1, t2, t3, tn])
    .fetch_all(pool)
    .await?;

    // The neighbour's admission names no content: its row exists and nothing
    // points at it, which is the leg that proves an owner erase does not
    // reach across owners to GC content it did not write.
    //
    // BEFORE the repointing below, deliberately. The dump is `ORDER BY
    // ctid`, an UPDATE writes a new tuple at the end of the heap, and this
    // row and `k3`'s are the two that survive a `src-a` source-scope erase —
    // so which of them is updated first IS the order the baseline pins.
    sqlx::query("UPDATE proxima_core.memory SET content_id = NULL WHERE t = $1")
        .bind(tn)
        .execute(pool)
        .await?;

    // Content: one body shared by both `src-a` versions (owner erase GCs it,
    // source erase of `src-a` GCs it too), one shared across sources (source
    // erase must NOT GC it), one held by the neighbour.
    let shared_a: Uuid = sqlx::query_scalar(
        "INSERT INTO proxima_core.content (owner_id, schema_id, content_hash)
         VALUES ($1, 'core/test-fact-v1', $2) RETURNING content_id",
    )
    .bind(owner.into_inner())
    .bind(vec![11u8; 32])
    .fetch_one(pool)
    .await?;
    let cross_source: Uuid = sqlx::query_scalar(
        "INSERT INTO proxima_core.content (owner_id, schema_id, content_hash)
         VALUES ($1, 'core/test-fact-v1', $2) RETURNING content_id",
    )
    .bind(owner.into_inner())
    .bind(vec![12u8; 32])
    .fetch_one(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.content (owner_id, schema_id, content_hash)
         VALUES ($1, 'core/test-fact-v1', $2)",
    )
    .bind(other.into_inner())
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
    sqlx::query("DELETE FROM proxima_core.content WHERE content_id = ANY($1::uuid[])")
        .bind(&minted_content)
        .execute(pool)
        .await?;

    // Two forgotten admissions — one per source — so both erase scopes
    // carry a forgotten row into the bundle and erase it with the rest.
    // Before #488 these were cooled stubs; forget now keeps the row.
    for t in [fourth.memory_id, fifth.memory_id] {
        pg.forget_memory(&permit, t).await?;
    }

    // Embedding infrastructure.
    for t in [t1, t2, tn] {
        sqlx::query(
            "INSERT INTO proxima_core.embeddings
                 (entity_id, model_id, dim, embedding_version, vec, owner_id)
             VALUES ($1, 'test-embed', 1024, 1, $2, $3)",
        )
        .bind(t)
        .bind(embed_vector())
        .bind(if t == tn {
            other.into_inner()
        } else {
            owner.into_inner()
        })
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.embedding_heads
                 (entity_id, model_id, dim, embedding_version, owner_id)
             VALUES ($1, 'test-embed', 1024, 1, $2)",
        )
        .bind(t)
        .bind(if t == tn {
            other.into_inner()
        } else {
            owner.into_inner()
        })
        .execute(pool)
        .await?;
    }

    // Goals and goal heads.
    for ((owner_ref, request, _prompt), goal_handle) in [
        (owner, "req-1", "target prompt"),
        (owner, "req-2", "target prompt two"),
        (other, "req-n", "neighbour prompt"),
    ]
    .into_iter()
    .zip(FIXTURE_GOAL_HANDLES)
    {
        sqlx::query(
            "INSERT INTO proxima_core.goal_head (handle, schema_id, owner_id, t)
             VALUES ($1, 'core/task-goal-v1', $2, $1)",
        )
        .bind(goal_handle)
        .bind(owner_ref.into_inner())
        .execute(pool)
        .await?;
        let goal_t: Uuid = sqlx::query_scalar(
            "INSERT INTO proxima_core.goal
                 (handle, t, owner_id, title, state, request_id, \"by\", via)
             VALUES ($1, $1, $2, 'a goal', 'Active', $3, '00000000-0000-0000-0000-000000000509'::uuid, NULL) RETURNING t",
        )
        .bind(goal_handle)
        .bind(owner_ref.into_inner())
        .bind(request)
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
        .bind(owner_ref.into_inner())
        .execute(pool)
        .await
        .ok();
    }

    // Source cursors and delegated authority.
    for (owner_ref, source) in [(owner, "src-a"), (owner, "src-b"), (other, "src-a")] {
        sqlx::query(
            "INSERT INTO proxima_core.source_cursors
                 (owner_id, source, cursor, updated_at)
             VALUES ($1, $2, $3,
                     TIMESTAMPTZ '2026-03-03 00:00:00Z')",
        )
        .bind(owner_ref.into_inner())
        .bind(source)
        .bind(vec![51u8; 8])
        .execute(pool)
        .await?;
    }
    for (subject, holder) in [(target, owner), (target, other), (neighbour, other)] {
        sqlx::query(
            "INSERT INTO proxima_core.delegated_authority_grants
                 (delegation_id, subject_user_id, owner_id, tool_name,
                  read_ceiling, write_ceiling, expires_at, auth_epoch, issued_at, \"by\", via)
             VALUES ($1, $2, $3, 'core_remember',
                     'fact', 'fact', TIMESTAMPTZ '2030-01-01 00:00:00Z', 1,
                     TIMESTAMPTZ '2026-01-01 00:00:00Z', '00000000-0000-0000-0000-000000000509'::uuid, NULL)",
        )
        .bind(Uuid::now_v7())
        .bind(subject.into_inner())
        .bind(holder.into_inner())
        .execute(pool)
        .await?;
    }

    Ok(Corpus { target, neighbour })
}

fn embed_vector() -> pgvector::Vector {
    let mut values = vec![0.0; 1024];
    values[0] = 1.0;
    pgvector::Vector::from(values)
}

/// Relations and columns present on only one side of the pinned comparison.
/// They are held out of BOTH sides of the differential: the question this
/// harness answers is whether the surviving rows are the same, and a surface
/// that does not exist on both sides cannot be compared. Every other
/// difference is a failure.
pub const DROPPED_TABLES: &[&str] = &[
    "compliance_audit_log",
    // Dropped by 0040 (#488): forget keeps the memory row, so the corpus's
    // forgotten admissions are in the `memory` section.
    "cooled",
    "owner_fact_retention",
    "owner_legal_holds",
    // Added after the pinned fd362509 baseline. Its capture/revoke/backfill
    // semantics have focused publication-origin PG oracles; this differential
    // continues to compare only relations available to both implementations.
    "publication_origin",
    // Added after the baseline by 0026: the consumer registry and a record's
    // per-consumer delivery lanes. `publication_outbox_pg_tests` pins them; a
    // lane is deleted with its record by cascade, which the outbox section
    // here already witnesses.
    "publication_consumer",
    "publication_delivery",
    // Added after the baseline by 0029: other-store origins of a derived
    // memory, keyed on its `t`. Skipped here only because the pinned
    // baseline lacks it; it is a flavor #0 surface (#478), so its erase leg
    // is generated, live and forgotten, and `external_origin_reach` pins it.
    "memory_external_origin",
    // Added after the baseline by 0034: a series' publication position,
    // deleted by owner erase once nothing of its series is left.
    // `publication_outbox_pg_tests` pins it.
    "publication_counter",
    // Added after the baseline by 0042: which flavor tables are Follows
    // sidecars, host-written schema metadata about no owner.
    "owner_follows",
];
pub const UNPAIRED_COLUMNS: &[(&str, &str)] = &[
    // 0007 added staged BLAKE3 identity after this frozen baseline;
    // migration and transfer differentials assert the live value directly.
    ("blob_uploads", "content_hash"),
    ("cold_purge_pending", "compliance_operation_id"),
    // 0025 added the Goal prompt text after this frozen baseline; the
    // export and erase of it are asserted directly in `goal_text.rs`.
    ("goal", "text"),
    // 0026 added each series' publication position after this frozen
    // baseline; `publication_outbox_pg_tests` asserts it directly.
    ("memory_head", "publication_sequence"),
];

fn names_relation(table: &str, relation: &str) -> bool {
    table == relation || table.strip_prefix("proxima_core.") == Some(relation)
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
/// have put two dynamic-SQL sites in the tree for a harness, and the whole
/// point of a harness is that it costs nothing to keep.
pub async fn dump_database(pool: &PgPool) -> Result<String, Box<dyn std::error::Error>> {
    // Permanent owner-free relations are excluded so this harness compares
    // mutable owner-scoped state: `erased_pin_target` gains erase witnesses;
    // `installation` and `store_identity` hold immutable deployment identity
    // that no owner lifecycle operation can reach.
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
            AND t.table_name NOT IN ('erased_pin_target', 'installation', 'store_identity')
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
            for (unpaired_table, column) in UNPAIRED_COLUMNS {
                if names_relation(&table, unpaired_table)
                    && let Some(object) = row.as_object_mut()
                {
                    object.remove(*column);
                }
            }
            if table == "publication_outbox"
                && let Some(object) = row.as_object_mut()
            {
                // The captured `CloudEvents` document renders as a hex
                // bytea and carries a wall-clock `time` INSIDE the blob,
                // where the uuid/timestamp tokenizer cannot reach it — two
                // runs of the same corpus would differ inside those bytes.
                // What the bytes say is pinned by the capture tests
                // (`publication_outbox_pg_tests`); what THIS file asks is
                // whether the row survives an erase, and `t`, `owner_id`,
                // `event_id` and `state` answer that.
                object.remove("envelope");
                object.remove("envelope_digest");
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
    create_core_db(&db_name)
        .await
        .expect("PG required: admin connect failed");
    let url = db_url(&db_name);
    (db_name, url)
}

pub async fn boot(url: &str) -> Result<PgStorage, Box<dyn std::error::Error>> {
    let pg = PgStorage::connect_for_tests(url.to_owned())
        .await?
        .with_cold(Arc::new(CorpusColdStore::default()));
    // The corpus writes its sidecar rows through the port, which routes each
    // payload by its own `(kind, schema_id, version)` through this registry.
    // Erase and export are unaffected: both are driven by `OwnerSurfaces`
    // off the contract, and neither reads the PG sidecar registry.
    Ok(pg.with_sidecars(proxima_storage_pg::core_pg_sidecars()))
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
        // #478 began exporting these (allowlisted); the pinned baseline did
        // not. Their erase is still compared through the database dump;
        // `tests/owner_export.rs` pins their exported field lists.
        if names_relation(table, "content") || names_relation(table, "blob_uploads") {
            continue;
        }
        sections.insert(table.clone(), rows.clone());
    }
    sections.insert("edges".into(), bundle.edges.clone());
    let mut out = String::new();
    for (table, rows) in sections {
        if rows.is_empty() {
            continue;
        }
        out.push_str(&format!("## export {table} ({})\n", rows.len()));
        for mut row in rows {
            for (unpaired_table, column) in UNPAIRED_COLUMNS {
                if names_relation(&table, unpaired_table)
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

async fn scenario(source_scope: Option<&str>, out_key: &str) {
    let (db_name, url) = fresh_db("proxima_diff").await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = boot(&url).await?;
        let pool = pg.pool_for_tests();
        let corpus = seed(&pg).await?;
        let mut text = export_dump(&pg, corpus.target).await?;
        match source_scope {
            None => {
                // Every target series carries a publication position, so the
                // erase has counter rows to remove. The dump skips the table,
                // which keeps the golden file as pinned.
                let target_owner = Uuid::from(OwnerId::from(corpus.target));
                sqlx::query(
                    "INSERT INTO proxima_core.publication_counter (handle, owner_id, sequence)
                     SELECT h.handle, h.owner_id, 1
                       FROM proxima_core.memory_head h
                      WHERE h.owner_id = $1
                     ON CONFLICT (handle) DO NOTHING",
                )
                .bind(target_owner)
                .execute(pool)
                .await?;
                let before = counter_rows(pool, target_owner).await?;
                assert!(before > 0, "the corpus seeds counter rows for the target");
                let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalOwner {
                    user_id: corpus.target,
                    drop_event_id: "diff".into(),
                });
                let outcome = pg
                    .erase_personal_owner(&auth, corpus.target, &tables())
                    .await?;
                text.push_str(&format!("## outcome {}\n", outcome_shape(&outcome)));
                assert_eq!(
                    counter_rows(pool, target_owner).await?,
                    0,
                    "owner erase leaves no publication_counter row for the erased owner"
                );
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

async fn counter_rows(pool: &PgPool, owner: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT count(*) FROM proxima_core.publication_counter WHERE owner_id = $1")
        .bind(owner)
        .fetch_one(pool)
        .await
}

fn outcome_shape(outcome: &proxima_core::owner_inverse::OwnerEraseOutcome) -> String {
    match outcome {
        proxima_core::owner_inverse::OwnerEraseOutcome::Completed {
            cold_object_purge_pending,
            ..
        } => format!("Completed cold_pending={cold_object_purge_pending}"),
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
