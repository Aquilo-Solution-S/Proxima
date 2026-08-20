//! The identity re-proof: the projection must not move a single result.
//!
//! With flavor #0's weights all `WEIGHT_UNIFORM` there is ONE distinct
//! level, so every lexeme lands in `tsvector` class `D` — which is what
//! `to_tsvector` produces unweighted — and no weight array is passed to
//! `ts_rank_cd`. The stored vector is therefore byte-identical to the one
//! the per-sidecar `search_tsv` generated column carried, and the score
//! arithmetic is unchanged. This test is what turns "therefore" into
//! evidence.
//!
//! **The expected values are literals captured from `b5fe11ad`** — the
//! commit before the projection — by running this same corpus and these
//! same requests against the pre-projection code and printing the results.
//! They are deliberately NOT derived from anything in the current tree: a
//! pin computed by the code under test proves only that it agrees with
//! itself.
//!
//! Capturing them is the harness in reverse: a worktree at `b5fe11ad`, a
//! copy of this file with `project()` removed (there the sidecar's
//! generated `search_tsv` did the work), and the printed `CASE` blocks
//! pasted into [`EXPECTED`]. The first capture used the pre-existing
//! `search_sidecar.rs` fixture's hand-restated projection, which omitted
//! `agent_note_v1.tags` from `fields` — so it pinned snippets production
//! never produced, and this test failed against a baseline that was
//! itself wrong. Read the pins off the shipped declaration, never off a
//! fixture's copy of it.
//!
//! Four owners, because a shared index is exactly where a multi-owner
//! deployment can leak: the corpus is built so every query has hits under
//! more than one owner, and the requests below read one owner and then
//! two, so a projection row whose `owner_id` drifted would show up as an
//! extra result rather than as a missing one.
//!
//! Scale: [`corpus`] takes the per-owner note count. CI runs it at
//! [`CI_NOTES_PER_OWNER`]; the 500k re-proof on the bench cluster calls the
//! same function with a larger number and compares two builds against each
//! other rather than against these literals.
#![allow(clippy::doc_markdown, clippy::too_many_lines)]

use proxima_core::storage_ports::MemoryReadPort;
use proxima_core::verbs::query::{
    EntityKind, MemorySearchPage, MemorySearchRequest, SearchMode, SearchOrder, SupersessionStatus,
    TagMatch,
};
use proxima_core::verbs::schema::MemorySearchProjection;
use proxima_core::{OwnerRef, UserId};
use proxima_pg_testkit::{create_db, db_url, drop_db};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

/// Small enough for CI, large enough that the overfetch window, the tie
/// break and the rescue band all have something to do.
const CI_NOTES_PER_OWNER: usize = 6;

const OWNERS: usize = 4;

/// A deterministic UUIDv7. The timestamp half is a fixed epoch plus the
/// sequence number, so `uuid_extract_timestamp` ordering, the `t DESC` tie
/// break and the recency cursor are all reproducible across runs and
/// across builds — without which "identical results" would be unfalsifiable.
fn det_uuid(seq: u64) -> Uuid {
    let ms = 1_700_000_000_000_u64 + seq;
    let mut bytes = [0_u8; 16];
    bytes[0..6].copy_from_slice(&ms.to_be_bytes()[2..8]);
    bytes[6] = 0x70 | u8::try_from((seq >> 8) & 0x0f).expect("nibble");
    bytes[7] = u8::try_from(seq & 0xff).expect("byte");
    bytes[8] = 0x80;
    for (index, byte) in bytes[9..].iter_mut().enumerate() {
        *byte = u8::try_from(seq & 0xff)
            .expect("byte")
            .wrapping_add(u8::try_from(index).expect("index"));
    }
    Uuid::from_bytes(bytes)
}

fn owner_at(index: usize) -> OwnerRef {
    OwnerRef::Personal(UserId::new(det_uuid(9_000 + index as u64)))
}

/// Word pool chosen so term frequency, document length and stop words all
/// vary: `ts_rank_cd` is sensitive to all three, so a corpus where every
/// document scored the same would prove nothing.
const WORDS: [&str; 12] = [
    "atlas",
    "edges",
    "retrieval",
    "substrate",
    "keyword",
    "needle",
    "the",
    "cartography",
    "index",
    "vector",
    "owner",
    "projection",
];

/// One note, deterministically. `seq` is global across owners so no two
/// notes share a `t`.
struct Note {
    t: Uuid,
    title: String,
    body: String,
    tags: Vec<String>,
}

/// `owners * notes_per_owner` notes, deterministic in every field.
fn corpus(notes_per_owner: usize) -> Vec<(OwnerRef, Note)> {
    let mut out = Vec::with_capacity(OWNERS * notes_per_owner);
    let mut seq = 0_u64;
    for note_index in 0..notes_per_owner {
        for owner_index in 0..OWNERS {
            seq += 1;
            let owner = owner_at(owner_index);
            let title = format!(
                "{} {}",
                WORDS[(note_index + owner_index) % WORDS.len()],
                WORDS[(note_index * 2 + owner_index) % WORDS.len()]
            );
            let body_len = 3 + (note_index % 5);
            let body = (0..body_len)
                .map(|word| WORDS[(note_index + owner_index + word * 3) % WORDS.len()])
                .collect::<Vec<_>>()
                .join(" ");
            let tags = vec![format!("bucket-{}", note_index % 3)];
            out.push((
                owner,
                Note {
                    t: det_uuid(seq),
                    title,
                    body,
                    tags,
                },
            ));
        }
    }
    out
}

fn note_projection() -> MemorySearchProjection {
    proxima_core::FlavorRegistry::new()
        .freeze_or_panic_for_tests()
        .search_projections()
        .iter()
        .find(|projection| projection.schema_id.as_str() == "core/agent-note-v1")
        .expect("core/agent-note-v1 is a search surface")
        .clone()
}

/// The production maintenance statement, over a hand-seeded row.
async fn project(pool: &sqlx::PgPool, t: Uuid) -> Result<(), sqlx::Error> {
    let spec = proxima_core::FLAVOR_0
        .projection
        .spec()
        .expect("flavor #0 declares a projection");
    let schema = proxima_core::FLAVOR_0
        .schemas
        .iter()
        .find(|schema| schema.schema_id().as_str() == "core/agent-note-v1")
        .expect("core/agent-note-v1 is declared");
    let sql = proxima_storage_pg::projection::projection_insert_sql(spec, schema)
        .expect("the generator emits a valid statement");
    // SQL-POLICY: generated
    sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(t)
        .bind(None::<&str>)
        .bind("core/agent-note-v1")
        .execute(pool)
        .await?;
    Ok(())
}

async fn seed(pool: &sqlx::PgPool, notes_per_owner: usize) -> Result<(), sqlx::Error> {
    for owner_index in 0..OWNERS {
        sqlx::query(
            "INSERT INTO proxima_core.owners (owner_id, kind)
             VALUES ($1, 'personal') ON CONFLICT DO NOTHING",
        )
        .bind(owner_at(owner_index).stored_owner_id())
        .execute(pool)
        .await?;
    }
    for (owner, note) in corpus(notes_per_owner) {
        let owner_id = owner.stored_owner_id();
        let handle = det_uuid(u64::from_be_bytes(
            note.t.as_bytes()[8..16].try_into().expect("8 bytes"),
        ));
        sqlx::query(
            "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
             VALUES ($1, 'fact', 'core/agent-note-v1', $2, $3)",
        )
        .bind(handle)
        .bind(owner_id)
        .bind(note.t)
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.memory (handle, t, kind, owner_id, schema_id)
             VALUES ($1, $2, 'fact', $3, 'core/agent-note-v1')",
        )
        .bind(handle)
        .bind(note.t)
        .bind(owner_id)
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body, tags)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(note.t)
        .bind(note.t)
        .bind(&note.title)
        .bind(&note.body)
        .bind(&note.tags)
        .execute(pool)
        .await?;
        project(pool, note.t).await?;
    }
    Ok(())
}

fn request(read_owners: Vec<OwnerRef>, query: &str, order: SearchOrder) -> MemorySearchRequest {
    MemorySearchRequest {
        owner: read_owners[0],
        read_owners,
        query: query.into(),
        mode: SearchMode::Lexical,
        supersession: SupersessionStatus::HeadsOnly,
        limit: 8,
        kind: Some(EntityKind::Fact),
        schema_id: None,
        tags: Vec::new(),
        tag_match: TagMatch::Any,
        since: None,
        until: None,
        order,
        min_score: None,
        semantic_weight: None,
        after: None,
        query_embedding: None,
        embedding_model_id: None,
    }
}

/// One line per result: id, score at six decimals, snippet. Everything a
/// caller can observe, in the order the caller observes it.
fn render(page: &MemorySearchPage) -> Vec<String> {
    let mut lines: Vec<String> = page
        .results
        .iter()
        .map(|result| {
            format!(
                "{} {:.6} {}",
                result.memory_id.into_inner(),
                result.score,
                result.snippet
            )
        })
        .collect();
    lines.push(format!("has_more={}", page.has_more));
    lines
}

/// The cases. Named so a failure says which request moved.
fn cases() -> Vec<(&'static str, MemorySearchRequest)> {
    vec![
        (
            "one-owner/relevance/atlas",
            request(vec![owner_at(0)], "atlas", SearchOrder::Relevance),
        ),
        (
            "one-owner/recency/atlas",
            request(vec![owner_at(0)], "atlas", SearchOrder::Recency),
        ),
        (
            "two-owners/relevance/atlas edges",
            request(
                vec![owner_at(0), owner_at(2)],
                "atlas edges",
                SearchOrder::Relevance,
            ),
        ),
        (
            "two-owners/recency/atlas edges",
            request(
                vec![owner_at(0), owner_at(2)],
                "atlas edges",
                SearchOrder::Recency,
            ),
        ),
        (
            "all-owners/relevance/retrieval substrate",
            request(
                (0..OWNERS).map(owner_at).collect(),
                "retrieval substrate",
                SearchOrder::Relevance,
            ),
        ),
        (
            "all-owners/relevance/the",
            request(
                (0..OWNERS).map(owner_at).collect(),
                "the",
                SearchOrder::Relevance,
            ),
        ),
        (
            // No lexeme match: the substring arm has to fire, on the
            // sidecar, exactly as it did before.
            "one-owner/relevance/substring",
            request(vec![owner_at(1)], "trieval", SearchOrder::Relevance),
        ),
        (
            "one-owner/relevance/miss",
            request(
                vec![owner_at(3)],
                "no-such-lexeme-xyzzy",
                SearchOrder::Recency,
            ),
        ),
    ]
}

/// Captured from `b5fe11ad` — the pre-projection tree — with this exact
/// corpus and these exact requests. See the module doc.
const EXPECTED: &[(&str, &[&str])] = &[
    (
        "one-owner/relevance/atlas",
        &[
            "018bcfe5-6801-7001-8001-020304050607 0.615385 atlas atlas atlas substrate the bucket-0",
            "018bcfe5-680d-700d-800d-0e0f10111213 0.545455 substrate the substrate the vector atlas substrate the bucket-0",
            "has_more=false",
        ],
    ),
    (
        "one-owner/recency/atlas",
        &[
            "018bcfe5-680d-700d-800d-0e0f10111213 0.545455 substrate the substrate the vector atlas substrate the bucket-0",
            "018bcfe5-6801-7001-8001-020304050607 0.615385 atlas atlas atlas substrate the bucket-0",
            "has_more=false",
        ],
    ),
    (
        "two-owners/relevance/atlas edges",
        &[
            "018bcfe5-6817-7017-8017-18191a1b1c1d 0.516129 cartography atlas cartography owner edges bucket-2",
            "018bcfe5-6813-7013-8013-141516171819 0.450000 the owner the vector atlas substrate the vector atlas bucket-1",
            "018bcfe5-680d-700d-800d-0e0f10111213 0.450000 substrate the substrate the vector atlas substrate the bucket-0",
            "018bcfe5-6807-7007-8007-08090a0b0c0d 0.450000 substrate keyword substrate the vector atlas bucket-1",
            "018bcfe5-6805-7005-8005-060708090a0b 0.450000 edges retrieval edges keyword cartography owner bucket-1",
            "018bcfe5-6801-7001-8001-020304050607 0.450000 atlas atlas atlas substrate the bucket-0",
            "018bcfe5-680b-700b-800b-0c0d0e0f1011 0.439958 keyword the keyword cartography owner edges keyword bucket-2",
            "018bcfe5-6811-7011-8011-121314151617 0.418151 keyword index keyword cartography owner edges keyword cartography owner bucket-1",
            "has_more=false",
        ],
    ),
    (
        "two-owners/recency/atlas edges",
        &[
            "018bcfe5-6817-7017-8017-18191a1b1c1d 0.516129 cartography atlas cartography owner edges bucket-2",
            "018bcfe5-6813-7013-8013-141516171819 0.450000 the owner the vector atlas substrate the vector atlas bucket-1",
            "018bcfe5-6811-7011-8011-121314151617 0.418151 keyword index keyword cartography owner edges keyword cartography owner bucket-1",
            "018bcfe5-680d-700d-800d-0e0f10111213 0.450000 substrate the substrate the vector atlas substrate the bucket-0",
            "018bcfe5-680b-700b-800b-0c0d0e0f1011 0.439958 keyword the keyword cartography owner edges keyword bucket-2",
            "018bcfe5-6807-7007-8007-08090a0b0c0d 0.450000 substrate keyword substrate the vector atlas bucket-1",
            "018bcfe5-6805-7005-8005-060708090a0b 0.450000 edges retrieval edges keyword cartography owner bucket-1",
            "018bcfe5-6801-7001-8001-020304050607 0.450000 atlas atlas atlas substrate the bucket-0",
            "has_more=false",
        ],
    ),
    (
        "all-owners/relevance/retrieval substrate",
        &[
            "018bcfe5-6806-7006-8006-0708090a0b0c 0.583333 retrieval substrate retrieval needle index projection bucket-1",
            "018bcfe5-6818-7018-8018-191a1b1c1d1e 0.450000 index edges index projection retrieval bucket-2",
            "018bcfe5-6810-7010-8010-111213141516 0.450000 the vector the vector atlas substrate the vector bucket-0",
            "018bcfe5-680d-700d-800d-0e0f10111213 0.450000 substrate the substrate the vector atlas substrate the bucket-0",
            "018bcfe5-680a-700a-800a-0b0c0d0e0f10 0.450000 substrate needle substrate the vector atlas substrate bucket-2",
            "018bcfe5-6809-7009-8009-0a0b0c0d0e0f 0.450000 retrieval keyword retrieval needle index projection retrieval bucket-2",
            "018bcfe5-6807-7007-8007-08090a0b0c0d 0.450000 substrate keyword substrate the vector atlas bucket-1",
            "018bcfe5-6804-7004-8004-05060708090a 0.450000 substrate substrate substrate the vector bucket-0",
            "has_more=true",
        ],
    ),
    (
        "all-owners/relevance/the",
        &[
            "018bcfe5-6816-7016-8016-1718191a1b1c 0.250000 the projection the vector atlas bucket-2",
            "018bcfe5-6813-7013-8013-141516171819 0.250000 the owner the vector atlas substrate the vector atlas bucket-1",
            "018bcfe5-6810-7010-8010-111213141516 0.250000 the vector the vector atlas substrate the vector bucket-0",
            "018bcfe5-680d-700d-800d-0e0f10111213 0.250000 substrate the substrate the vector atlas substrate the bucket-0",
            "018bcfe5-680b-700b-800b-0c0d0e0f1011 0.250000 keyword the keyword cartography owner edges keyword bucket-2",
            "018bcfe5-680a-700a-800a-0b0c0d0e0f10 0.250000 substrate needle substrate the vector atlas substrate bucket-2",
            "018bcfe5-6807-7007-8007-08090a0b0c0d 0.250000 substrate keyword substrate the vector atlas bucket-1",
            "018bcfe5-6804-7004-8004-05060708090a 0.250000 substrate substrate substrate the vector bucket-0",
            "has_more=true",
        ],
    ),
    (
        "one-owner/relevance/substring",
        &[
            "018bcfe5-6812-7012-8012-131415161718 0.250000 needle vector needle index projection retrieval needle index projection bucket-1",
            "018bcfe5-6806-7006-8006-0708090a0b0c 0.250000 retrieval substrate retrieval needle index projection bucket-1",
            "has_more=false",
        ],
    ),
    ("one-owner/relevance/miss", &["has_more=false"]),
];

#[tokio::test]
async fn the_projection_returns_the_results_the_sidecar_vectors_did() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        seed(pg.pool_for_tests(), CI_NOTES_PER_OWNER).await?;

        let projections = [note_projection()];
        let mut actual = Vec::new();
        for (name, req) in cases() {
            let page = pg.search_memories(&req, &projections).await?;
            actual.push((name, render(&page)));
        }

        // Print before asserting: capturing the pins for a NEW case, or
        // re-capturing them on the pre-projection tree, is a matter of
        // reading this output rather than of instrumenting the test.
        for (name, lines) in &actual {
            println!("CASE {name}");
            for line in lines {
                println!("  {line}");
            }
        }

        for ((name, lines), (expected_name, expected)) in actual.iter().zip(EXPECTED) {
            assert_eq!(name, expected_name, "case order drifted");
            assert_eq!(
                lines.as_slice(),
                *expected,
                "{name}: the projection moved a result"
            );
        }
        assert_eq!(actual.len(), EXPECTED.len(), "a case lost its pin");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("projection identity re-proof failed");
}
