//! The stored lexical vectors must equal the expression they replaced.
//!
//! Migration 0011 moved `to_tsvector` off the read path and into generated
//! columns. The definition now lives in two places that cannot see each
//! other: SQL (the generated column) and the Rust query builder (the
//! fallback for sidecars with no stored column). If they ever disagree, a
//! memory silently scores differently depending on which table it lives
//! in — no error, just wrong results. These tests pin both against the
//! literal expression the builder computed before 0011.

use crate::common::{drop_db, fresh_pg, owner_fixture};

use super::{insert_search_abstraction, insert_text_memory};

/// The exact tsvector expression the lexical branch inlined before 0011,
/// as a SQL fragment over `$1`.
const LEGACY_TSV_SQL: &str = "to_tsvector(
     'english',
     regexp_replace(
         regexp_replace($1, '[[:punct:]]+', ' ', 'g'),
         '\\m[[:alnum:]]{255}[[:alnum:]]+\\M',
         ' ',
         'g'
     )
 )";

/// Inputs chosen to hit every branch of the scrub: punctuation runs, the
/// over-long-token cut, stopwords, stemming, unicode, and the empty and
/// whitespace-only cases that decide NULL-vs-empty-vector.
fn adversarial_texts() -> Vec<String> {
    vec![
        String::new(),
        "   ".to_string(),
        "plain text".to_string(),
        "Hello, World! -- it's a test...".to_string(),
        "adopted adopting adopts".to_string(),
        "what is my the a of".to_string(),
        format!("prefix {} suffix", "x".repeat(300)),
        format!("edge {} edge", "y".repeat(255)),
        "e-mail user@example.com http://host/path?q=1".to_string(),
        "Grüße Straßberger naïve".to_string(),
        "tabs\tand\nnewlines".to_string(),
        "127.0.0.1 3.14 -42".to_string(),
    ]
}

#[tokio::test]
async fn lexical_tsv_function_matches_the_inlined_expression()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;

    for text in adversarial_texts() {
        // SQL-POLICY: fixed-fragment
        let matches: bool = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT proxima_core.lexical_tsv($1) IS NOT DISTINCT FROM {LEGACY_TSV_SQL}"
        )))
        .bind(&text)
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert!(
            matches,
            "lexical_tsv diverged from the pre-0011 expression for {text:?}"
        );
    }

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn stored_memory_tsv_matches_the_projected_search_text()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;

    let owner = owner_fixture();
    for text in adversarial_texts() {
        if text.trim().is_empty() {
            // The base branch filters empty text out before it can be a
            // candidate; the column is still generated, just unreachable.
            continue;
        }
        insert_text_memory(&pg, &owner, &text).await?;
        insert_search_abstraction(&pg, &owner, &text, None).await?;
    }

    // memories.search_tsv is generated from COALESCE(text, ''), which is
    // exactly what the base candidate branch projects as search_text.
    let mismatches: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM proxima_core.memories m
          WHERE m.search_tsv IS DISTINCT FROM
                proxima_core.lexical_tsv(COALESCE(m.text, ''))",
    )
    .fetch_one(pg.pool_for_tests())
    .await?;
    assert_eq!(mismatches, 0, "stored memories.search_tsv drifted");

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn stored_sidecar_tsv_matches_the_projection_concatenation()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;

    let owner = owner_fixture();
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(&owner);
    for (idx, text) in adversarial_texts().into_iter().enumerate() {
        let memory_id = uuid::Uuid::now_v7();
        sqlx::query(
            "INSERT INTO proxima_core.memories
                (memory_id, owner_kind, owner_id, schema_id, schema_version, kind, text,
                 operator_kind, operator_id, input_contract_id, source_batch_id,
                 model_id, prompt_version)
             VALUES ($1, $2, $3, 'proxima/agent-derivation-v1', 1, 'Abstraction', $4, 'AtoA',
                     '00000000-0000-0000-0000-000000000331'::uuid,
                     '00000000-0000-0000-0000-000000000332'::uuid, NULL,
                     'test-model', 'test-v1')",
        )
        .bind(memory_id)
        .bind(owner_kind)
        .bind(owner_id)
        .bind(format!("body {idx}"))
        .execute(pg.pool_for_tests())
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.agent_derivation_v1
                (memory_id, title, body, tags, source_memory_ids, model_id,
                 client_name, client_version)
             VALUES ($1, $2, $3, $4, ARRAY[]::uuid[], 'test-model', 'test', '1')",
        )
        .bind(memory_id)
        .bind(format!("title {idx}"))
        // body carries the adversarial text; title and tags stay well-formed
        // so the row still satisfies the nonempty check constraints.
        .bind(format!("body {idx} {text}"))
        .bind(vec![format!("tag{idx}"), "shared, tag".to_string()])
        .execute(pg.pool_for_tests())
        .await?;
    }

    // The generated column must equal lexical_tsv over the same
    // concat_ws the sidecar candidate branch emits as search_text.
    let mismatches: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM proxima_core.agent_derivation_v1 d
          WHERE d.search_tsv IS DISTINCT FROM proxima_core.lexical_tsv(
                NULLIF(concat_ws(' ',
                    NULLIF(d.title, ''),
                    NULLIF(d.body, ''),
                    NULLIF(array_to_string(d.tags, ' '), '')), ''))",
    )
    .fetch_one(pg.pool_for_tests())
    .await?;
    assert_eq!(
        mismatches, 0,
        "stored agent_derivation_v1.search_tsv drifted"
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}
