use proxima_storage_pg::test_fixtures::fresh_pg;

async fn analyzed(pool: &sqlx::PgPool, config: &str, query: &str) -> Result<String, sqlx::Error> {
    sqlx::query_scalar("SELECT proxima_core.lexical_query_text($1::regconfig, $2)")
        .bind(config)
        .bind(query)
        .fetch_one(pool)
        .await
}

#[tokio::test]
async fn query_stopwords_choose_dominant_language_and_preserve_source_syntax()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, _guard) = fresh_pg("dominant_stopwords").await;
    let pool = pg.pool_for_tests();
    sqlx::query("INSERT INTO proxima_core.lexical_languages VALUES ('german')")
        .execute(pool)
        .await?;
    for (query, expected) in [
        (
            "How do I use the man page, Red Hat, and war crimes?",
            "   use  man page, Red Hat,  war crimes?",
        ),
        (
            "Wie funktioniert der \"checkpoint recovery\" OR -protocol und der checkpoint?",
            " funktioniert  \"checkpoint recovery\" OR -protocol   checkpoint?",
        ),
        ("die the checkpoint", "  checkpoint"),
        ("die die the checkpoint", "  the checkpoint"),
        ("flurble checkpoint", "flurble checkpoint"),
        ("the OR checkpoint", "  checkpoint"),
        ("alpha OR beta", "alpha OR beta"),
        ("alpha or beta", "alpha or beta"),
        ("alpha -OR beta", "alpha  beta"),
        ("or checkpoint", " checkpoint"),
        ("checkpoint or", "checkpoint "),
        ("foo-or-bar", "foo--bar"),
        ("alpha OR OR beta", "alpha OR  beta"),
        ("OR OR checkpoint", "  checkpoint"),
        ("alpha OR OR OR OR OR beta", "alpha OR     beta"),
        ("or-checkpoint", "checkpoint"),
        ("the-checkpoint", "checkpoint"),
        ("alpha OR- beta", "alpha  beta"),
        ("the -checkpoint", " -checkpoint"),
        ("\"or or or die war crimes\"", "\"   die war crimes\""),
        ("-the cat", " cat"),
        (
            "alpha \"the beta\" OR -the gamma",
            "alpha \" beta\" OR  gamma",
        ),
        ("", ""),
        // The specified maximum-count rule is ambiguous for bare English phrases.
        ("man page", " page"),
        ("Red Hat", "Red "),
        ("war crimes", " crimes"),
    ] {
        for config in ["english", "german"] {
            assert_eq!(
                analyzed(pool, config, query).await?,
                expected,
                "{config}: {query}"
            );
        }
    }
    let negation: String = sqlx::query_scalar(
        "SELECT websearch_to_tsquery('english',
                    proxima_core.lexical_query_text('english', '-the cat'))::text",
    )
    .fetch_one(pool)
    .await?;
    assert_eq!(
        negation, "'cat'",
        "a removed stopword's minus must not negate the next term"
    );
    sqlx::query("INSERT INTO proxima_core.lexical_languages VALUES ('french')")
        .execute(pool)
        .await?;
    assert_eq!(
        analyzed(
            pool,
            "english",
            "Comment fonctionne le protocole de checkpoint?"
        )
        .await?,
        "Comment fonctionne  protocole  checkpoint?",
        "a third registered language uses its configured dictionary"
    );
    Ok(())
}

#[tokio::test]
async fn query_stopwords_use_configured_dictionaries_and_ignore_unknown_words()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, _guard) = fresh_pg("dictionary_stopwords").await;
    let pool = pg.pool_for_tests();
    sqlx::raw_sql(
        "CREATE TEXT SEARCH DICTIONARY proxima_core.query_stop_only (
             TEMPLATE = pg_catalog.simple, STOPWORDS = english, ACCEPT = false
         );
         CREATE TEXT SEARCH CONFIGURATION proxima_core.query_custom (PARSER = pg_catalog.default);
         ALTER TEXT SEARCH CONFIGURATION proxima_core.query_custom
             ADD MAPPING FOR asciiword, word WITH proxima_core.query_stop_only;
         DELETE FROM proxima_core.lexical_default;
         DELETE FROM proxima_core.lexical_languages;
         INSERT INTO proxima_core.lexical_languages VALUES ('proxima_core.query_custom');",
    )
    .execute(pool)
    .await?;
    let dictionary: (Option<Vec<String>>, Option<Vec<String>>) = sqlx::query_as(
        "SELECT ts_lexize('proxima_core.query_stop_only', 'the'),
                ts_lexize('proxima_core.query_stop_only', 'checkpoint')",
    )
    .fetch_one(pool)
    .await?;
    assert_eq!(dictionary, (Some(Vec::new()), None));
    assert_eq!(
        analyzed(pool, "german", "the checkpoint").await?,
        " checkpoint"
    );
    assert_eq!(
        analyzed(pool, "german", "unknown checkpoint").await?,
        "unknown checkpoint"
    );

    sqlx::query("DELETE FROM proxima_core.lexical_languages")
        .execute(pool)
        .await?;
    assert_eq!(
        analyzed(pool, "english", "the checkpoint").await?,
        " checkpoint",
        "an empty inventory falls back to the supplied configuration"
    );
    Ok(())
}

#[tokio::test]
async fn lexical_search_uses_dominant_german_stopwords_for_english_rows()
-> Result<(), Box<dyn std::error::Error>> {
    use proxima_core::storage_ports::MemoryReadPort;
    use proxima_core::{OwnerRef, UserId};
    use uuid::Uuid;

    let (pg, _guard) = fresh_pg("german_english_query").await;
    let pool = pg.pool_for_tests();
    sqlx::query("INSERT INTO proxima_core.lexical_languages VALUES ('german')")
        .execute(pool)
        .await?;
    let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let wanted = super::seed_note(
        pool,
        owner,
        "checkpoint recovery",
        "checkpoint recovery commit protocol funktioniert",
    )
    .await?;
    let distractor = super::seed_note(pool, owner, "function words", "der die das und").await?;
    let page = pg
        .search_memories(
            None,
            &super::search_req(
                owner,
                "Wie funktioniert der checkpoint recovery und das commit protocol?",
            ),
            &[super::note_projection()],
        )
        .await?;
    assert_eq!(page.results[0].memory_id.into_inner(), wanted);
    assert!(
        page.results
            .iter()
            .all(|hit| hit.memory_id.into_inner() != distractor)
    );
    Ok(())
}
