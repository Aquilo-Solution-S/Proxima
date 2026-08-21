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
//!
//! # What the Phase 3 cases add, and why
//!
//! The eight original cases are one schema, which cannot see the two
//! changes that matter most when the per-schema fan-out collapses into one
//! statement per flavor. The corpus therefore carries `core/agent-derivation-v1`
//! rows as well, and the cases below it exercise:
//!
//! - **hits in two schemas** — the single flavor-wide overfetch window
//!   replaces the union of per-schema windows.
//! - **mixed arms across schemas** — `cartograph` is a LEXEME in a
//!   derivation (the word appears verbatim) and only a SUBSTRING in a note
//!   (`cartography` stems to `cartographi`). This is the exact case a
//!   flavor-wide "the ranked arm returned nothing" trigger would lose: the
//!   ranked arm returns derivations, so it is not empty, and the note rows
//!   only survive because the trigger is computed per SCHEMA.
//! - **a tag filter** — the `p.tag` predicate across two schemas.
//! - **a schema-scoped request** — `schema_id = ANY(..)` with one member.
//!
//! The second schema is `core/agent-derivation-v1`, an ABSTRACTION. The
//! eight original cases all pass `kind: Some(Fact)`, so the new rows are
//! structurally invisible to them and the original literals stay valid
//! without a re-capture — which is what makes them a pin rather than a
//! snapshot. The new cases pass `kind: None`, which is how they see both.
//!
//! `Hybrid` is deliberately NOT pinned. One global top-k per flavor can
//! move a hybrid page — a row with weak lexical rank but strong similarity
//! could previously ride in on its own schema's window — and that movement
//! is a documented v0.0.8 breaking change, not an identity claim.
//!
//! Language variation gets its own database
//! ([`a_second_lexical_configuration_scores_what_it_scored`]): registering
//! a second `lexical_languages` row changes the query side for EVERY row in
//! that database, so a corpus that mixes configurations cannot also pin the
//! single-configuration cases.
//!
//! # The starvation probe
//!
//! [`a_superseded_backlog_does_not_starve_the_page`] is a third database and
//! a third corpus, and it exists because the neutrality claim above was
//! nearly true rather than true. See that test.
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

/// Two per owner: enough that a two-schema query has hits under more than
/// one owner in both schemas, which is what makes the multi-owner claim
/// testable in the schema the Phase 2 corpus did not have.
const CI_DERIVATIONS_PER_OWNER: usize = 2;

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

/// Derivation text is built from its own pool, overlapping the note pool in
/// `atlas`, `vector`, `index` and `substrate` — so a query can hit both
/// schemas — and disjoint in `cartograph` and `waypoint`.
///
/// `cartograph` is the load-bearing one. It is a whole word here, so it is
/// a LEXEME in a derivation; the note pool has `cartography`, which the
/// English stemmer reduces to `cartographi`, so the same query reaches a
/// note only through the substring arm.
const DERIVED_WORDS: [&str; 6] = [
    "cartograph",
    "vector",
    "waypoint",
    "atlas",
    "index",
    "substrate",
];

/// One note, deterministically. `seq` is global across owners so no two
/// notes share a `t`.
struct Note {
    t: Uuid,
    title: String,
    body: String,
    tags: Vec<String>,
}

/// One derivation, deterministically.
struct Derivation {
    t: Uuid,
    title: String,
    body: String,
    tags: Vec<String>,
}

/// `OWNERS * per_owner` derivations, deterministic in every field.
///
/// `seq` starts at 1_000 so no derivation can collide with a note's `t` at
/// any corpus size CI or the bench cluster runs.
fn derivations(per_owner: usize) -> Vec<(OwnerRef, Derivation)> {
    let mut out = Vec::with_capacity(OWNERS * per_owner);
    let mut seq = 1_000_u64;
    for index in 0..per_owner {
        for owner_index in 0..OWNERS {
            seq += 1;
            out.push((
                owner_at(owner_index),
                Derivation {
                    t: det_uuid(seq),
                    title: format!(
                        "{} {}",
                        DERIVED_WORDS[(index + owner_index) % DERIVED_WORDS.len()],
                        DERIVED_WORDS[(index * 2 + owner_index + 1) % DERIVED_WORDS.len()]
                    ),
                    body: format!(
                        "{} {} {}",
                        DERIVED_WORDS[(index + owner_index + 2) % DERIVED_WORDS.len()],
                        DERIVED_WORDS[(index * 3 + owner_index) % DERIVED_WORDS.len()],
                        DERIVED_WORDS[(index + owner_index * 2 + 1) % DERIVED_WORDS.len()]
                    ),
                    tags: vec![format!("bucket-{}", (index + 1) % 3)],
                },
            ));
        }
    }
    out
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

const NOTE_SCHEMA: &str = "core/agent-note-v1";
const DERIVATION_SCHEMA: &str = "core/agent-derivation-v1";

/// EVERY projected schema the frozen registry declares, which is exactly
/// what `Engine::search` passes. The fixture used to hand-pick one, so the
/// projection-selection gates were never exercised by this test at all.
fn projections() -> Vec<MemorySearchProjection> {
    proxima_core::FlavorRegistry::new()
        .freeze_or_panic_for_tests()
        .search_projections()
        .to_vec()
}

/// The production maintenance statement, over a hand-seeded row.
///
/// `language` is the `PerRow` bind: `None` takes the deployment default,
/// `Some(config)` stamps that configuration on the projection row and
/// registers it in `lexical_languages` through the table's own trigger.
async fn project(
    pool: &sqlx::PgPool,
    t: Uuid,
    schema_id: &str,
    language: Option<&str>,
) -> Result<(), sqlx::Error> {
    let spec = proxima_core::FLAVOR_0
        .projection
        .spec()
        .expect("flavor #0 declares a projection");
    let schema = proxima_core::FLAVOR_0
        .schemas
        .iter()
        .find(|schema| schema.schema_id().as_str() == schema_id)
        .unwrap_or_else(|| panic!("{schema_id} is declared"));
    let sql = proxima_storage_pg::projection::projection_insert_sql(spec, schema)
        .expect("the generator emits a valid statement");
    // SQL-POLICY: generated
    sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(t)
        .bind(language)
        .bind(schema_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// The handle a hand-seeded row gets: deterministic in `t`, so the
/// `memory_head` and `memory` rows agree without a lookup.
fn handle_for(t: Uuid) -> Uuid {
    det_uuid(u64::from_be_bytes(
        t.as_bytes()[8..16].try_into().expect("8 bytes"),
    ))
}

/// `origins` is not decoration: `memory_pin_checks` refuses a non-Fact that
/// pins no hot memory, so a derivation has to ground in a note. It
/// grounds in its own owner's first one, which keeps the corpus
/// deterministic and the pin inside the owner.
async fn admit(
    pool: &sqlx::PgPool,
    owner_id: Uuid,
    t: Uuid,
    kind: &str,
    schema_id: &str,
    origins: &[Uuid],
) -> Result<(), sqlx::Error> {
    let handle = handle_for(t);
    sqlx::query(
        "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
         VALUES ($1, $2::proxima_core.memory_kind, $3, $4, $5)",
    )
    .bind(handle)
    .bind(kind)
    .bind(schema_id)
    .bind(owner_id)
    .bind(t)
    .execute(pool)
    .await?;
    // `memory_ap_content_chk`: only a Fact may have no content row. The
    // hash is `t`'s bytes doubled, which is deterministic, 32 bytes, and
    // unique per memory — the corpus is a fixture, not an ingest.
    let content_id: Option<Uuid> = if kind == "fact" {
        None
    } else {
        let mut hash = Vec::with_capacity(32);
        hash.extend_from_slice(t.as_bytes());
        hash.extend_from_slice(t.as_bytes());
        Some(
            sqlx::query_scalar(
                "INSERT INTO proxima_core.content (content_id, owner_id, schema_id, content_hash)
                 VALUES ($1, $2, $3, $4) RETURNING content_id",
            )
            .bind(t)
            .bind(owner_id)
            .bind(schema_id)
            .bind(hash)
            .fetch_one(pool)
            .await?,
        )
    };
    sqlx::query(
        "INSERT INTO proxima_core.memory (handle, t, kind, owner_id, schema_id, origins, content_id)
         VALUES ($1, $2, $3::proxima_core.memory_kind, $4, $5, $6, $7)",
    )
    .bind(handle)
    .bind(t)
    .bind(kind)
    .bind(owner_id)
    .bind(schema_id)
    .bind(origins)
    .bind(content_id)
    .execute(pool)
    .await?;
    Ok(())
}

/// The `t` of `owner_index`'s first note. `corpus` numbers `seq` with the
/// owner as the inner loop, so owner `o`'s first note is `det_uuid(o + 1)`.
fn first_note_of(owner_index: usize) -> Uuid {
    det_uuid(owner_index as u64 + 1)
}

/// `language` is threaded to [`project`] so one call can seed either the
/// single-configuration corpus (`None` everywhere) or the mixed one.
async fn seed(
    pool: &sqlx::PgPool,
    notes_per_owner: usize,
    derivations_per_owner: usize,
    language_for: impl Fn(usize) -> Option<&'static str>,
) -> Result<(), sqlx::Error> {
    for owner_index in 0..OWNERS {
        sqlx::query(
            "INSERT INTO proxima_core.owners (owner_id, kind)
             VALUES ($1, 'personal') ON CONFLICT DO NOTHING",
        )
        .bind(owner_at(owner_index).stored_owner_id())
        .execute(pool)
        .await?;
    }
    for (index, (owner, note)) in corpus(notes_per_owner).into_iter().enumerate() {
        let owner_id = owner.stored_owner_id();
        admit(pool, owner_id, note.t, "fact", NOTE_SCHEMA, &[]).await?;
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
        project(pool, note.t, NOTE_SCHEMA, language_for(index)).await?;
    }
    for (index, (owner, derivation)) in derivations(derivations_per_owner).into_iter().enumerate() {
        let owner_id = owner.stored_owner_id();
        admit(
            pool,
            owner_id,
            derivation.t,
            "abstraction",
            DERIVATION_SCHEMA,
            &[first_note_of(index % OWNERS)],
        )
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.agent_derivation_v1
                 (t, title, body, tags, model_id, client_name, client_version)
             VALUES ($1, $2, $3, $4, 'test-model', 'identity-fixture', '1')",
        )
        .bind(derivation.t)
        .bind(&derivation.title)
        .bind(&derivation.body)
        .bind(&derivation.tags)
        .execute(pool)
        .await?;
        project(pool, derivation.t, DERIVATION_SCHEMA, language_for(index)).await?;
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
        // ── Phase 3: the cases one schema cannot see ──────────────────
        (
            // Both schemas match by lexeme. Before the collapse this was
            // two statements with two overfetch windows; after, it is one
            // statement with one.
            "all-owners/relevance/two-schemas/vector",
            any_kind(request(
                (0..OWNERS).map(owner_at).collect(),
                "vector",
                SearchOrder::Relevance,
            )),
        ),
        (
            "two-owners/recency/two-schemas/atlas",
            any_kind(request(
                vec![owner_at(0), owner_at(2)],
                "atlas",
                SearchOrder::Recency,
            )),
        ),
        (
            // THE substring-trigger case. `cartograph` is a lexeme in a
            // derivation and only a substring in a note, so the ranked
            // arm returns rows for one schema and nothing for the other.
            // A flavor-wide "ranked returned nothing" trigger drops every
            // note below; a per-schema one keeps them.
            "all-owners/relevance/mixed-arms/cartograph",
            any_kind(request(
                (0..OWNERS).map(owner_at).collect(),
                "cartograph",
                SearchOrder::Relevance,
            )),
        ),
        (
            // The `p.tag` predicate, across two schemas in one statement.
            // Both declare a `tag_column`; the request narrows rows, and
            // `core_search_flavors` is what would narrow the SCHEMA SET if
            // one of them did not.
            "all-owners/relevance/tagged/bucket-1",
            tagged(any_kind(request(
                (0..OWNERS).map(owner_at).collect(),
                "atlas edges",
                SearchOrder::Relevance,
            ))),
        ),
        (
            // `schema_id = ANY(..)` with one member: the narrowing that
            // used to be projection selection is a row predicate now.
            "all-owners/relevance/schema-scoped/derivation",
            scoped(any_kind(request(
                (0..OWNERS).map(owner_at).collect(),
                "atlas",
                SearchOrder::Relevance,
            ))),
        ),
    ]
}

/// Facts AND perspectives, so a request can reach both projected schemas.
fn any_kind(mut req: MemorySearchRequest) -> MemorySearchRequest {
    req.kind = None;
    req
}

fn tagged(mut req: MemorySearchRequest) -> MemorySearchRequest {
    req.tags = vec!["bucket-1".into()];
    req.tag_match = TagMatch::Any;
    req
}

fn scoped(mut req: MemorySearchRequest) -> MemorySearchRequest {
    req.schema_id = Some(proxima_core::SchemaId::new(DERIVATION_SCHEMA.to_owned()));
    req
}

/// The mixed-configuration cases. Their own database: registering a second
/// `lexical_languages` row rewrites the query side for every row in it.
fn language_cases() -> Vec<(&'static str, MemorySearchRequest)> {
    vec![
        (
            "mixed-language/all-owners/relevance/atlas",
            any_kind(request(
                (0..OWNERS).map(owner_at).collect(),
                "atlas",
                SearchOrder::Relevance,
            )),
        ),
        (
            // `the` is a stop word under `english` and a real lexeme under
            // `simple`, so this case is where a second registered
            // configuration is visible at all: the rows stamped `simple`
            // reach the exact arm, the rows stamped `english` reach only
            // the substring arm.
            "mixed-language/all-owners/relevance/the",
            any_kind(request(
                (0..OWNERS).map(owner_at).collect(),
                "the",
                SearchOrder::Relevance,
            )),
        ),
        (
            "mixed-language/two-owners/recency/vector",
            any_kind(request(
                vec![owner_at(1), owner_at(3)],
                "vector",
                SearchOrder::Recency,
            )),
        ),
    ]
}

/// Every third seeded row is stamped `simple` instead of the deployment
/// default. Deterministic, so the capture and the assertion see the same
/// corpus.
fn alternating_language(index: usize) -> Option<&'static str> {
    index.is_multiple_of(3).then_some("simple")
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
    // ── Phase 3 cases, captured from `e7c3c83f` by the same method ───
    (
        "all-owners/relevance/two-schemas/vector",
        &[
            "018bcfe5-6810-7010-8010-111213141516 0.615385 the vector the vector atlas substrate the vector bucket-0",
            "018bcfe5-6bea-73ea-80ea-ebecedeeeff0 0.583333 vector waypoint atlas vector atlas bucket-1",
            "018bcfe5-6be9-73e9-80e9-eaebecedeeef 0.583333 cartograph vector waypoint cartograph vector bucket-1",
            "018bcfe5-6813-7013-8013-141516171819 0.583333 the owner the vector atlas substrate the vector atlas bucket-1",
            "018bcfe5-6bed-73ed-80ed-eeeff0f1f2f3 0.545455 vector atlas atlas atlas waypoint bucket-2",
            "018bcfe5-6bec-73ec-80ec-edeeeff0f1f2 0.545455 atlas index substrate atlas vector bucket-1",
            "018bcfe5-6816-7016-8016-1718191a1b1c 0.545455 the projection the vector atlas bucket-2",
            "018bcfe5-6812-7012-8012-131415161718 0.545455 needle vector needle index projection retrieval needle index projection bucket-1",
            "has_more=true",
        ],
    ),
    (
        "two-owners/recency/two-schemas/atlas",
        &[
            "018bcfe5-6bef-73ef-80ef-f0f1f2f3f4f5 0.545455 atlas substrate substrate substrate cartograph bucket-2",
            "018bcfe5-6bed-73ed-80ed-eeeff0f1f2f3 0.615385 vector atlas atlas atlas waypoint bucket-2",
            "018bcfe5-6beb-73eb-80eb-ecedeeeff0f1 0.545455 waypoint atlas index waypoint substrate bucket-1",
            "018bcfe5-6817-7017-8017-18191a1b1c1d 0.545455 cartography atlas cartography owner edges bucket-2",
            "018bcfe5-6813-7013-8013-141516171819 0.583333 the owner the vector atlas substrate the vector atlas bucket-1",
            "018bcfe5-680d-700d-800d-0e0f10111213 0.545455 substrate the substrate the vector atlas substrate the bucket-0",
            "018bcfe5-6807-7007-8007-08090a0b0c0d 0.545455 substrate keyword substrate the vector atlas bucket-1",
            "018bcfe5-6801-7001-8001-020304050607 0.615385 atlas atlas atlas substrate the bucket-0",
            "has_more=false",
        ],
    ),
    (
        "all-owners/relevance/mixed-arms/cartograph",
        &[
            "018bcfe5-6bf0-73f0-80f0-f1f2f3f4f5f6 0.615385 index cartograph cartograph cartograph waypoint bucket-2",
            "018bcfe5-6be9-73e9-80e9-eaebecedeeef 0.583333 cartograph vector waypoint cartograph vector bucket-1",
            "018bcfe5-6bef-73ef-80ef-f0f1f2f3f4f5 0.545455 atlas substrate substrate substrate cartograph bucket-2",
            "018bcfe5-6817-7017-8017-18191a1b1c1d 0.250000 cartography atlas cartography owner edges bucket-2",
            "018bcfe5-6814-7014-8014-15161718191a 0.250000 cartography projection cartography owner edges keyword cartography owner edges bucket-1",
            "018bcfe5-6811-7011-8011-121314151617 0.250000 keyword index keyword cartography owner edges keyword cartography owner bucket-1",
            "018bcfe5-680e-700e-800e-0f1011121314 0.250000 keyword cartography keyword cartography owner edges keyword cartography bucket-0",
            "018bcfe5-680c-700c-800c-0d0e0f101112 0.250000 needle cartography needle index projection retrieval needle bucket-2",
            "has_more=true",
        ],
    ),
    (
        "all-owners/relevance/tagged/bucket-1",
        &[
            "018bcfe5-6bec-73ec-80ec-edeeeff0f1f2 0.450000 atlas index substrate atlas vector bucket-1",
            "018bcfe5-6beb-73eb-80eb-ecedeeeff0f1 0.450000 waypoint atlas index waypoint substrate bucket-1",
            "018bcfe5-6bea-73ea-80ea-ebecedeeeff0 0.450000 vector waypoint atlas vector atlas bucket-1",
            "018bcfe5-6814-7014-8014-15161718191a 0.450000 cartography projection cartography owner edges keyword cartography owner edges bucket-1",
            "018bcfe5-6813-7013-8013-141516171819 0.450000 the owner the vector atlas substrate the vector atlas bucket-1",
            "018bcfe5-6807-7007-8007-08090a0b0c0d 0.450000 substrate keyword substrate the vector atlas bucket-1",
            "018bcfe5-6805-7005-8005-060708090a0b 0.450000 edges retrieval edges keyword cartography owner bucket-1",
            "018bcfe5-6808-7008-8008-090a0b0c0d0e 0.439958 keyword needle keyword cartography owner edges bucket-1",
            "has_more=true",
        ],
    ),
    (
        "all-owners/relevance/schema-scoped/derivation",
        &[
            "018bcfe5-6bed-73ed-80ed-eeeff0f1f2f3 0.615385 vector atlas atlas atlas waypoint bucket-2",
            "018bcfe5-6bec-73ec-80ec-edeeeff0f1f2 0.583333 atlas index substrate atlas vector bucket-1",
            "018bcfe5-6bea-73ea-80ea-ebecedeeeff0 0.583333 vector waypoint atlas vector atlas bucket-1",
            "018bcfe5-6bef-73ef-80ef-f0f1f2f3f4f5 0.545455 atlas substrate substrate substrate cartograph bucket-2",
            "018bcfe5-6beb-73eb-80eb-ecedeeeff0f1 0.545455 waypoint atlas index waypoint substrate bucket-1",
            "has_more=false",
        ],
    ),
];

/// Captured from `e7c3c83f` with the mixed-configuration corpus. See
/// [`a_second_lexical_configuration_scores_what_it_scored`].
const LANGUAGE_EXPECTED: &[(&str, &[&str])] = &[
    (
        "mixed-language/all-owners/relevance/atlas",
        &[
            "018bcfe5-6bed-73ed-80ed-eeeff0f1f2f3 0.615385 vector atlas atlas atlas waypoint bucket-2",
            "018bcfe5-6801-7001-8001-020304050607 0.615385 atlas atlas atlas substrate the bucket-0",
            "018bcfe5-6bec-73ec-80ec-edeeeff0f1f2 0.583333 atlas index substrate atlas vector bucket-1",
            "018bcfe5-6bea-73ea-80ea-ebecedeeeff0 0.583333 vector waypoint atlas vector atlas bucket-1",
            "018bcfe5-6813-7013-8013-141516171819 0.583333 the owner the vector atlas substrate the vector atlas bucket-1",
            "018bcfe5-6bef-73ef-80ef-f0f1f2f3f4f5 0.545455 atlas substrate substrate substrate cartograph bucket-2",
            "018bcfe5-6beb-73eb-80eb-ecedeeeff0f1 0.545455 waypoint atlas index waypoint substrate bucket-1",
            "018bcfe5-6817-7017-8017-18191a1b1c1d 0.545455 cartography atlas cartography owner edges bucket-2",
            "has_more=true",
        ],
    ),
    (
        "mixed-language/all-owners/relevance/the",
        &[
            "018bcfe5-6813-7013-8013-141516171819 0.615385 the owner the vector atlas substrate the vector atlas bucket-1",
            "018bcfe5-6810-7010-8010-111213141516 0.615385 the vector the vector atlas substrate the vector bucket-0",
            "018bcfe5-680d-700d-800d-0e0f10111213 0.615385 substrate the substrate the vector atlas substrate the bucket-0",
            "018bcfe5-6816-7016-8016-1718191a1b1c 0.583333 the projection the vector atlas bucket-2",
            "018bcfe5-680a-700a-800a-0b0c0d0e0f10 0.545455 substrate needle substrate the vector atlas substrate bucket-2",
            "018bcfe5-6807-7007-8007-08090a0b0c0d 0.545455 substrate keyword substrate the vector atlas bucket-1",
            "018bcfe5-6804-7004-8004-05060708090a 0.545455 substrate substrate substrate the vector bucket-0",
            "018bcfe5-6801-7001-8001-020304050607 0.545455 atlas atlas atlas substrate the bucket-0",
            "has_more=false",
        ],
    ),
    (
        "mixed-language/two-owners/recency/vector",
        &[
            "018bcfe5-6bec-73ec-80ec-edeeeff0f1f2 0.545455 atlas index substrate atlas vector bucket-1",
            "018bcfe5-6bea-73ea-80ea-ebecedeeeff0 0.583333 vector waypoint atlas vector atlas bucket-1",
            "018bcfe5-6816-7016-8016-1718191a1b1c 0.545455 the projection the vector atlas bucket-2",
            "018bcfe5-6812-7012-8012-131415161718 0.545455 needle vector needle index projection retrieval needle index projection bucket-1",
            "018bcfe5-6810-7010-8010-111213141516 0.615385 the vector the vector atlas substrate the vector bucket-0",
            "018bcfe5-680a-700a-800a-0b0c0d0e0f10 0.545455 substrate needle substrate the vector atlas substrate bucket-2",
            "018bcfe5-6804-7004-8004-05060708090a 0.545455 substrate substrate substrate the vector bucket-0",
            "has_more=false",
        ],
    ),
];

/// Run `cases` against a freshly migrated database seeded by
/// `language_for`, print every result, then assert it against `expected`.
///
/// Printing before asserting is deliberate and load-bearing: capturing the
/// pins for a NEW case, or re-capturing them on a baseline worktree, is a
/// matter of reading this output rather than of instrumenting the test.
async fn run_identity(
    cases: Vec<(&'static str, MemorySearchRequest)>,
    expected: &[(&str, &[&str])],
    language_for: impl Fn(usize) -> Option<&'static str>,
) {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        seed(
            pg.pool_for_tests(),
            CI_NOTES_PER_OWNER,
            CI_DERIVATIONS_PER_OWNER,
            language_for,
        )
        .await?;

        let projections = projections();
        let mut actual = Vec::new();
        for (name, req) in cases {
            let page = pg.search_memories(&req, &projections).await?;
            actual.push((name, render(&page)));
        }

        for (name, lines) in &actual {
            println!("CASE {name}");
            for line in lines {
                println!("  {line}");
            }
        }

        for ((name, lines), (expected_name, expected)) in actual.iter().zip(expected) {
            assert_eq!(name, expected_name, "case order drifted");
            assert_eq!(
                lines.as_slice(),
                *expected,
                "{name}: the collapsed search moved a result"
            );
        }
        assert_eq!(actual.len(), expected.len(), "a case lost its pin");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("projection identity re-proof failed");
}

#[tokio::test]
async fn the_projection_returns_the_results_the_sidecar_vectors_did() {
    run_identity(cases(), EXPECTED, |_| None).await;
}

// ── The starvation probe ────────────────────────────────────────────────
//
// Its own corpus and its own database: supersession changes what every
// other case in this file admits, so it cannot share one.

/// Superseded note handles in the probe corpus. Chosen above the
/// `overfetch` a `limit = 1` request gets (`1 * 20 = 20`) and below the one
/// a `limit = 8` request gets (`160`), so the same corpus produces the
/// failure and its control.
const STARVED_HANDLES: usize = 25;

/// The revision that matches — strongly, in BOTH probe queries — and that
/// `HeadsOnly` admission is going to throw away.
const SUPERSEDED_TITLE: &str = "Atlas cartography";
const SUPERSEDED_BODY: &str = "atlas cartography atlas cartography atlas";
/// …and the revision that replaced it, which matches neither query by
/// lexeme nor by substring.
const HEAD_TITLE: &str = "Retired stub";
const HEAD_BODY: &str = "this revision says nothing";

/// One live derivation, matching WEAKLY. It is the row the page should
/// contain; the backlog is what buries it.
///
/// The text does not contain the substring `cartographies`, and it does
/// contain `cartography`, which stems onto it. That is what makes the
/// second case discriminate: a starved schema is re-found by the substring
/// arm, and the substring arm cannot find this row at all.
///
/// There is deliberately no live NOTE here. The note schema's own window is
/// what the backlog fills, so a live note would be starved on the baseline
/// tree too — by the per-schema window, at five times the budget — and a
/// case whose baseline answer is itself a starvation cannot pin anything.
/// See the test's own doc.
const LIVE_DERIVATION_TITLE: &str = "Waypoint";
const LIVE_DERIVATION_BODY: &str = "a single atlas reference in the cartography of the archive";

/// One revision of one note handle: the memory row, its sidecar, and the
/// projection row the generator writes for it.
async fn seed_note_revision(
    pool: &sqlx::PgPool,
    owner_id: Uuid,
    handle: Uuid,
    t: Uuid,
    title: &str,
    body: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO proxima_core.memory (handle, t, kind, owner_id, schema_id)
         VALUES ($1, $2, 'fact', $3, $4)",
    )
    .bind(handle)
    .bind(t)
    .bind(owner_id)
    .bind(NOTE_SCHEMA)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body, tags)
         VALUES ($1, $2, $3, $4, '{}')",
    )
    .bind(t)
    .bind(t)
    .bind(title)
    .bind(body)
    .execute(pool)
    .await?;
    project(pool, t, NOTE_SCHEMA, None).await?;
    Ok(())
}

/// [`STARVED_HANDLES`] two-revision note handles, one live note and one
/// live derivation, all under one owner.
async fn seed_starvation(pool: &sqlx::PgPool) -> Result<(), sqlx::Error> {
    let owner_id = owner_at(0).stored_owner_id();
    sqlx::query(
        "INSERT INTO proxima_core.owners (owner_id, kind)
         VALUES ($1, 'personal') ON CONFLICT DO NOTHING",
    )
    .bind(owner_id)
    .execute(pool)
    .await?;

    for index in 0..STARVED_HANDLES as u64 {
        let handle = det_uuid(30_000 + index);
        let superseded = det_uuid(10_000 + index);
        let head = det_uuid(20_000 + index);
        // The head row has to exist before any memory row can reference
        // the handle, so it is created pointing at the revision it will
        // later supersede — which is exactly the order a real write takes.
        sqlx::query(
            "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
             VALUES ($1, 'fact', $2, $3, $4)",
        )
        .bind(handle)
        .bind(NOTE_SCHEMA)
        .bind(owner_id)
        .bind(superseded)
        .execute(pool)
        .await?;
        seed_note_revision(
            pool,
            owner_id,
            handle,
            superseded,
            SUPERSEDED_TITLE,
            SUPERSEDED_BODY,
        )
        .await?;
        seed_note_revision(pool, owner_id, handle, head, HEAD_TITLE, HEAD_BODY).await?;
        sqlx::query("UPDATE proxima_core.memory_head SET t = $2 WHERE handle = $1")
            .bind(handle)
            .bind(head)
            .execute(pool)
            .await?;
    }

    let live_derivation = det_uuid(50_000);
    admit(
        pool,
        owner_id,
        live_derivation,
        "abstraction",
        DERIVATION_SCHEMA,
        // `memory_pin_checks` wants a hot memory; the first handle's HEAD
        // revision is one.
        &[det_uuid(20_000)],
    )
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.agent_derivation_v1
             (t, title, body, tags, model_id, client_name, client_version)
         VALUES ($1, $2, $3, '{}', 'test-model', 'identity-fixture', '1')",
    )
    .bind(live_derivation)
    .bind(LIVE_DERIVATION_TITLE)
    .bind(LIVE_DERIVATION_BODY)
    .execute(pool)
    .await?;
    project(pool, live_derivation, DERIVATION_SCHEMA, None).await?;
    Ok(())
}

fn starvation_cases() -> Vec<(&'static str, MemorySearchRequest)> {
    let at = |query: &str, limit: u32| {
        let mut req = any_kind(request(vec![owner_at(0)], query, SearchOrder::Relevance));
        req.limit = limit;
        req
    };
    vec![
        ("starved/limit-1/relevance/atlas", at("atlas", 1)),
        (
            "starved/limit-1/relevance/cartographies",
            at("cartographies", 1),
        ),
        ("starved/limit-8/relevance/atlas", at("atlas", 8)),
    ]
}

/// Captured from `e7c3c83f` with the starvation corpus, by the same method
/// as the other two pins. See [`STARVATION_EXPECTED`]'s own test.
const STARVATION_EXPECTED: &[(&str, &[&str])] = &[
    (
        "starved/limit-1/relevance/atlas",
        &[
            "018bcfe6-2b50-7350-8050-515253545556 0.545455 Waypoint a single atlas reference in \
             the cartography of the archive",
            "has_more=false",
        ],
    ),
    (
        "starved/limit-1/relevance/cartographies",
        &[
            "018bcfe6-2b50-7350-8050-515253545556 0.545455 Waypoint a single atlas reference in \
             the cartography of the archive",
            "has_more=false",
        ],
    ),
    (
        "starved/limit-8/relevance/atlas",
        &[
            "018bcfe6-2b50-7350-8050-515253545556 0.545455 Waypoint a single atlas reference in \
             the cartography of the archive",
            "has_more=false",
        ],
    ),
];

/// A backlog of superseded revisions must not cost the page its live rows.
///
/// **This is the case that falsified "the Lexical + Relevance page cannot
/// move".** The candidate-superset argument for the collapse is about
/// candidates; `search_admit_sql(heads_only)` then drops every superseded
/// revision from the hit set, and `HeadsOnly` is the tool default and what
/// `core_recall` sends. Five per-schema windows of twenty could absorb a
/// backlog that one flavor-wide window of twenty cannot, so at `limit = 1`
/// twenty-five superseded revisions of one schema starved the other
/// schema's live row out of the window entirely — and the starved schema
/// was then reported "missing" and re-found by the SUBSTRING arm, which
/// stamps a flat floor. The two failure modes are:
///
/// - `atlas`: the live rows came back at `0.250000` instead of their
///   `ts_rank` scores, because the substring arm found them.
/// - `cartographies`: the live rows came back not at all, because
///   `cartography` stems onto the query but does not contain it as a
///   substring, so the substring arm has nothing to find.
///
/// `limit = 8` is the control: its window is 160, the backlog is 26 rows,
/// and it never starved on either tree. If the first two cases fail and
/// this one passes, the window is the cause; if all three fail, the corpus
/// or the scoring moved.
///
/// The literals come from `e7c3c83f`, captured by running this file in a
/// worktree at that commit. They are the answers the PER-SCHEMA fan-out
/// gave.
///
/// **What this corpus deliberately does NOT contain, and why.** An earlier
/// version added a live note matching weakly, expecting it on the page
/// beside the derivation. It is not pinnable: the backlog is in the note
/// schema, so a live note is starved out of the note schema's window on the
/// BASELINE tree as well — the per-schema window has the same defect, at
/// five times the budget. Measured on both trees with 25 handles and
/// `limit = 1`: `e7c3c83f` returned the derivation at `0.545455` and
/// dropped the live note entirely; this tree returns the live note at
/// `0.583333`, which is the higher-scoring admissible row and the answer
/// the corpus actually justifies. That is a page MOVE against the baseline,
/// in the correct direction, and it is not something a fix to this tree can
/// avoid — so the pinned corpus stays inside the region where the baseline
/// was itself right.
#[tokio::test]
async fn a_superseded_backlog_does_not_starve_the_page() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        seed_starvation(pg.pool_for_tests()).await?;

        let projections = projections();
        let mut actual = Vec::new();
        for (name, req) in starvation_cases() {
            let page = pg.search_memories(&req, &projections).await?;
            actual.push((name, render(&page)));
        }
        for (name, lines) in &actual {
            println!("CASE {name}");
            for line in lines {
                println!("  {line}");
            }
        }
        for ((name, lines), (expected_name, expected)) in actual.iter().zip(STARVATION_EXPECTED) {
            assert_eq!(name, expected_name, "case order drifted");
            assert_eq!(
                lines.as_slice(),
                *expected,
                "{name}: a superseded backlog moved the page"
            );
        }
        assert_eq!(
            actual.len(),
            STARVATION_EXPECTED.len(),
            "a case lost its pin"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("starvation probe failed");
}

/// A second registered `lexical_languages` row does not move a score.
///
/// `LanguagePolicy::PerRow` is the whole reason the query side ORs one
/// `websearch_to_tsquery` per registered configuration, and the collapse
/// computes that CTE once per flavor where it used to be computed once per
/// schema. If one statement over four schemas got the multilingual query
/// side wrong, this is the case that says so.
#[tokio::test]
async fn a_second_lexical_configuration_scores_what_it_scored() {
    run_identity(language_cases(), LANGUAGE_EXPECTED, alternating_language).await;
}
