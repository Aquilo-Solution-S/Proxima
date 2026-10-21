//! Direct candidate calls must authorize ids, scores, and counts before returning.

use super::*;
use proxima_core::OwnerScope;
use proxima_pg_testkit::DbGuard;
use proxima_storage_pg::begin_owner_transaction;
use proxima_storage_pg::test_fixtures::fresh_pg;
use sqlx::PgConnection;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
type CandidateRow = (Uuid, f32, i64);

#[derive(Clone, Copy)]
enum TextLeg {
    Lexical,
    Substring,
}

impl TextLeg {
    fn sql(self) -> &'static str {
        match self {
            Self::Lexical => {
                "SELECT t, lexical_score, count(*) OVER () FROM proxima_core.search_lexical_candidates($1, $2, NULL, NULL, NULL, NULL, $3, ARRAY['core/agent-note-v1'], true, false, true, 'fact', false)"
            }
            Self::Substring => {
                "SELECT t, lexical_score, count(*) OVER () FROM proxima_core.search_substring_candidates($1, $2, NULL, NULL, NULL, NULL, $3, ARRAY['core/agent-note-v1'], true, false, true, 'fact', false)"
            }
        }
    }

    fn query(self) -> &'static str {
        match self {
            Self::Lexical => "needle",
            Self::Substring => "%needle%",
        }
    }
}

async fn text_candidates(
    connection: &mut PgConnection,
    leg: TextLeg,
    owners: &[Uuid],
    window: Option<i64>,
) -> Result<Vec<CandidateRow>, sqlx::Error> {
    // SQL-POLICY: fixed-fragment — a closed leg chooses one fixed function call.
    sqlx::query_as(sqlx::AssertSqlSafe(leg.sql()))
        .bind(leg.query())
        .bind(window)
        .bind(owners)
        .fetch_all(connection)
        .await
}

async fn vector_candidates(
    connection: &mut PgConnection,
    owners: &[Uuid],
    vector: &pgvector::Vector,
) -> Result<Vec<CandidateRow>, sqlx::Error> {
    sqlx::query_as(
        "SELECT memory_id, similarity_score, count(*) OVER ()
           FROM proxima_core.vector_candidates_d1024($1, 'test-embed', $2, 8)",
    )
    .bind(owners)
    .bind(vector)
    .fetch_all(connection)
    .await
}

fn assert_code(error: &sqlx::Error, expected: &str) {
    assert_eq!(
        error
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .as_deref(),
        Some(expected),
        "{error}"
    );
}

struct Fixture {
    pg: PgStorage,
    owner: OwnerId,
    other: OwnerId,
    ours: Uuid,
    theirs: Uuid,
    _database: DbGuard,
}

impl Fixture {
    async fn new() -> TestResult<Self> {
        let (pg, database) = fresh_pg("direct_search_candidates").await;
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let other = OwnerId::from(UserId::new(Uuid::now_v7()));
        let ours = seed_note(
            pg.pool_for_tests(),
            owner,
            "An observation",
            "A needle surrounded by other words",
        )
        .await?;
        let theirs = seed_note(
            pg.pool_for_tests(),
            other,
            "needle needle needle needle",
            "needle needle needle needle needle needle needle needle",
        )
        .await?;
        seed_embedding(pg.pool_for_tests(), owner, ours, &embed_vector_xy(0.6, 0.8)).await?;
        seed_embedding(pg.pool_for_tests(), other, theirs, &embed_vector()).await?;
        Ok(Self {
            pg,
            owner,
            other,
            ours,
            theirs,
            _database: database,
        })
    }

    fn scope(&self) -> OwnerScope {
        owner_scope_for_tests(&[self.owner])
    }
}

#[tokio::test]
async fn direct_candidates_refuse_missing_forged_and_invalid_inputs() -> TestResult {
    let fixture = Fixture::new().await?;
    let runtime = fixture.pg.clone_pool_for_backend();
    let owners = [fixture.owner.into_inner()];
    let forged = [fixture.owner.into_inner(), fixture.other.into_inner()];
    let scope = fixture.scope();
    for leg in [TextLeg::Lexical, TextLeg::Substring] {
        let mut valid = begin_owner_transaction(&runtime, &scope).await?;
        let rows = text_candidates(valid.as_mut(), leg, &owners, Some(8)).await?;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, fixture.ours);
        assert_eq!(rows[0].2, 1);
        valid.commit().await?;
        let mut missing = runtime.begin().await?;
        let error = text_candidates(missing.as_mut(), leg, &owners, Some(8))
            .await
            .unwrap_err();
        assert_code(&error, "42501");
        missing.rollback().await?;
        let mut scoped = begin_owner_transaction(&runtime, &scope).await?;
        let error = text_candidates(scoped.as_mut(), leg, &forged, Some(8))
            .await
            .unwrap_err();
        assert_code(&error, "42501");
        scoped.rollback().await?;
    }
    for window in [
        None,
        Some(-1),
        Some(0),
        Some(i64::from(note_projection().overfetch_k) + 1),
    ] {
        let mut tx = begin_owner_transaction(&runtime, &scope).await?;
        let error = text_candidates(tx.as_mut(), TextLeg::Lexical, &owners, window)
            .await
            .unwrap_err();
        assert_code(&error, "22023");
        tx.rollback().await?;
    }
    let mut valid = begin_owner_transaction(&runtime, &scope).await?;
    let rows = vector_candidates(valid.as_mut(), &owners, &embed_vector()).await?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, fixture.ours);
    assert_eq!(rows[0].2, 1);
    valid.commit().await?;
    let mut missing = runtime.begin().await?;
    let error = vector_candidates(missing.as_mut(), &owners, &embed_vector())
        .await
        .unwrap_err();
    assert_code(&error, "42501");
    missing.rollback().await?;
    let mut scoped = begin_owner_transaction(&runtime, &scope).await?;
    let error = vector_candidates(scoped.as_mut(), &forged, &embed_vector())
        .await
        .unwrap_err();
    assert_code(&error, "42501");
    scoped.rollback().await?;
    let mut wrong_width = begin_owner_transaction(&runtime, &scope).await?;
    let error = vector_candidates(
        wrong_width.as_mut(),
        &owners,
        &pgvector::Vector::from(vec![1.0; 384]),
    )
    .await
    .unwrap_err();
    assert_code(&error, "22023");
    wrong_width.rollback().await?;
    Ok(())
}

#[tokio::test]
async fn stale_accelerator_owners_never_leak_candidate_ids_scores_or_counts() -> TestResult {
    let fixture = Fixture::new().await?;
    let runtime = fixture.pg.clone_pool_for_backend();
    let both = owner_scope_for_tests(&[fixture.owner, fixture.other]);
    let both_ids = [fixture.owner.into_inner(), fixture.other.into_inner()];
    let mut baseline = begin_owner_transaction(&runtime, &both).await?;
    let lexical = text_candidates(baseline.as_mut(), TextLeg::Lexical, &both_ids, Some(8)).await?;
    assert_eq!(
        lexical[0].0, fixture.theirs,
        "foreign lexical hit ranks first"
    );
    assert!(lexical[0].1 > lexical[1].1);
    let ours_score = lexical.iter().find(|row| row.0 == fixture.ours).unwrap().1;
    let semantic = vector_candidates(baseline.as_mut(), &both_ids, &embed_vector()).await?;
    assert_eq!(
        semantic[0].0, fixture.theirs,
        "foreign vector hit ranks first"
    );
    assert!(semantic[0].1 > semantic[1].1);
    baseline.commit().await?;
    let admin = fixture.pg.pool_for_tests();
    sqlx::query("UPDATE proxima_core.projection SET owner_id = $1 WHERE memory_id = $2")
        .bind(fixture.owner.into_inner())
        .bind(fixture.theirs)
        .execute(admin)
        .await?;
    sqlx::query("UPDATE proxima_core.embeddings SET owner_id = $1 WHERE entity_id = $2")
        .bind(fixture.owner.into_inner())
        .bind(fixture.theirs)
        .execute(admin)
        .await?;
    sqlx::query("UPDATE proxima_core.embedding_heads SET owner_id = $1 WHERE entity_id = $2")
        .bind(fixture.owner.into_inner())
        .bind(fixture.theirs)
        .execute(admin)
        .await?;
    let memory_owner: Uuid =
        sqlx::query_scalar("SELECT owner_id FROM proxima_core.memory WHERE t = $1")
            .bind(fixture.theirs)
            .fetch_one(admin)
            .await?;
    assert_eq!(memory_owner, fixture.other.into_inner());
    let scope = fixture.scope();
    let owners = [fixture.owner.into_inner()];
    // Requested owners also narrow a scope that can read both memories.
    for candidate_scope in [&scope, &both] {
        for leg in [TextLeg::Lexical, TextLeg::Substring] {
            let mut tx = begin_owner_transaction(&runtime, candidate_scope).await?;
            let rows = text_candidates(tx.as_mut(), leg, &owners, Some(8)).await?;
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].0, fixture.ours);
            assert_eq!(rows[0].2, 1, "count contains only authorized candidates");
            if matches!(leg, TextLeg::Lexical) {
                assert_eq!(
                    rows[0].1.to_bits(),
                    ours_score.to_bits(),
                    "foreign score does not enter output"
                );
            }
            tx.commit().await?;
        }
        let mut tx = begin_owner_transaction(&runtime, candidate_scope).await?;
        let rows = vector_candidates(tx.as_mut(), &owners, &embed_vector()).await?;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, fixture.ours);
        assert_eq!(rows[0].2, 1);
        assert!((rows[0].1 - 0.6).abs() < 0.000_001);
        tx.commit().await?;
    }
    let mut req = search_req(fixture.owner, "needle");
    for mode in [
        SearchMode::Lexical,
        SearchMode::Semantic,
        SearchMode::Hybrid,
    ] {
        req.mode = mode;
        req.semantic = Some(test_semantic(embed_vector().to_vec()));
        let page = fixture
            .pg
            .search_memories(Some(&scope), &req, &[note_projection()])
            .await?;
        assert_eq!(page.results.len(), 1);
        assert_eq!(page.results[0].memory_id.into_inner(), fixture.ours);
        assert!(!page.has_more);
        if mode != SearchMode::Semantic {
            assert_eq!(
                page.results[0].lexical_score.to_bits(),
                ours_score.to_bits()
            );
        }
        if mode != SearchMode::Lexical {
            assert!((page.results[0].similarity_score - 0.6).abs() < 0.000_001);
        }
    }
    Ok(())
}

/// The substring function plans only the sidecars a request names; the rows
/// it returns are those of the named schemas whatever the ordering, and a
/// request naming none of them (or no schema at all) returns nothing.
#[tokio::test]
async fn substring_candidates_answer_for_the_requested_schemas_only() -> TestResult {
    let fixture = Fixture::new().await?;
    let runtime = fixture.pg.clone_pool_for_backend();
    let scope = fixture.scope();
    let owners = [fixture.owner.into_inner()];
    let note = "core/agent-note-v1".to_owned();
    let utterance = "core/utterance-v1".to_owned();
    let cases: [(Option<Vec<String>>, usize); 5] = [
        (Some(vec![note.clone()]), 1),
        (Some(vec![utterance.clone(), note.clone()]), 1),
        (Some(vec![utterance]), 0),
        (Some(Vec::new()), 0),
        (None, 0),
    ];
    for recency in [false, true] {
        for (schemas, expected) in &cases {
            let mut tx = begin_owner_transaction(&runtime, &scope).await?;
            let rows: Vec<(Uuid, f32)> = sqlx::query_as(
                "SELECT t, lexical_score FROM proxima_core.search_substring_candidates(
                     '%needle%', 8, NULL, NULL, NULL, NULL, $1, $2, true, $3, true, 'fact', false)",
            )
            .bind(&owners[..])
            .bind(schemas.as_deref())
            .bind(recency)
            .fetch_all(tx.as_mut())
            .await?;
            assert_eq!(rows.len(), *expected, "{schemas:?} recency={recency}");
            if *expected == 1 {
                assert_eq!(rows[0].0, fixture.ours);
            }
            tx.commit().await?;
        }
    }
    Ok(())
}
