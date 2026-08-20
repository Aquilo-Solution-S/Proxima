//! The plan's §2.5 acceptance criteria, and the embedding byte-parity gate.
//!
//! Each case is the same shape: something that used to be true only because
//! several places happened to agree is now a declaration plus a test that
//! the declaration matches the behaviour. Requires local PG.

use proxima_core::flavor::{
    EmbedText, EmbeddingRecipe, Enforcement, SLOT_DEFAULT, SearchProjectionDecl, TransferRule,
};
use proxima_core::verbs::schema::PayloadKind;
use proxima_core::{FLAVOR_0, FlavorRegistry};
use proxima_pg_testkit::{create_db, db_url, drop_db};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

// ── §2.5 (1): goals don't transfer ──────────────────────────────────────

/// The refusal is a declaration, and the declaration names the sites that
/// actually enforce it.
///
/// This is the case the map (RA-6) flagged: after #229 there is no CHECK
/// constraint to point at, so a contract that claimed one would be a lie.
/// What exists is an engine refusal, a storage backstop and a DDL trigger,
/// and `try_freeze` rejects a `NotTransferable` that names none of them.
#[test]
fn goals_are_not_transferable_and_the_declaration_names_its_enforcement() {
    let goals = FLAVOR_0
        .schemas
        .iter()
        .filter(|schema| schema.kind == PayloadKind::Goal)
        .collect::<Vec<_>>();

    assert_eq!(goals.len(), 2, "core declares simple-text and task goals");

    for schema in goals {
        let TransferRule::NotTransferable { why, enforced_by } = schema.transfer else {
            panic!(
                "{} must declare NotTransferable, not {:?}",
                schema.id.render(),
                schema.transfer
            );
        };
        assert!(!why.is_empty());
        assert!(
            enforced_by
                .iter()
                .any(|site| matches!(site, Enforcement::EngineRefusal { .. })),
            "{}: the engine refuses the transfer verb",
            schema.id.render()
        );
        assert!(
            enforced_by
                .iter()
                .any(|site| matches!(site, Enforcement::StorageBackstop { .. })),
            "{}: storage refuses a goal owner move even if the engine is bypassed",
            schema.id.render()
        );
        assert!(
            enforced_by
                .iter()
                .any(|site| matches!(site, Enforcement::Trigger(_))),
            "{}: goal_head_t_only is the DDL backstop that survives both",
            schema.id.render()
        );
        assert!(
            !enforced_by
                .iter()
                .any(|site| matches!(site, Enforcement::Constraint(_))),
            "{}: there is no CHECK constraint to cite (map RA-6); claiming one \
             would make the contract a comment",
            schema.id.render()
        );
    }
}

// ── §2.5 (2): declared absence ──────────────────────────────────────────

/// A non-surface is a value, not an omission.
///
/// Before the contract, `search_projection() -> None` was reachable three
/// ways — no projection, empty fields, no sidecar table — so a schema that
/// deliberately does not search was indistinguishable from one whose
/// declaration was forgotten. The plan's acceptance case is spelled
/// "ChatTurn-style"; utterances are searchable in this tree (operator
/// ruling §4.3), so the declared-absence exemplars are the schemas that
/// really do decline.
#[test]
fn declared_absence_is_a_value_with_a_reason() {
    let declining = FLAVOR_0
        .schemas
        .iter()
        .filter(|schema| !schema.search.is_projected())
        .collect::<Vec<_>>();

    assert!(
        declining.len() >= 2,
        "core has schemas that decline to search"
    );
    for schema in &declining {
        let SearchProjectionDecl::None { why } = schema.search else {
            unreachable!("filtered to the absent arm")
        };
        assert!(
            !why.is_empty(),
            "{} declines to search and must say why",
            schema.id.render()
        );
    }

    let utterance = FLAVOR_0
        .schemas
        .iter()
        .find(|schema| schema.id.render() == "core/utterance-v1")
        .expect("core declares utterance-v1");
    assert!(
        utterance.search.is_projected(),
        "utterances ARE searchable here (§4.3); the tested value is that \
         absence is declarable, not that utterances declare it"
    );

    let call_log = FLAVOR_0
        .schemas
        .iter()
        .find(|schema| schema.id.render() == "core/mcp-call-logged-v1")
        .expect("core declares mcp-call-logged-v1");
    assert!(
        matches!(call_log.search, SearchProjectionDecl::None { .. }),
        "call telemetry is not retrievable content"
    );
    assert!(
        matches!(call_log.embedding, EmbeddingRecipe::Never { .. }),
        "and it is not embeddable either — both absences are declared"
    );
}

// ── §2.5 (3): forget touches everything ─────────────────────────────────

/// Every declared surface says what forget does to it, non-optionally.
///
/// The bug class this closes is a surface reached by six independent acts
/// of remembering: a table added to the migration and to one sweep is
/// invisible to the other five. A `Surface` cannot be constructed without
/// a `forget` rule, so the compiler asks the question.
#[test]
fn every_declared_surface_states_what_forget_does() {
    let surfaces = FLAVOR_0.all_surfaces().collect::<Vec<_>>();

    assert!(
        surfaces.len() > 20,
        "flavor #0 speaks for the kernel spine as well as its own sidecars"
    );

    for surface in &surfaces {
        // The rule is an enum with no default and no `Option`, so reaching
        // this line at all is the structural half of the claim. The
        // behavioural half: a surface that keeps its rows has to say why.
        if let proxima_core::flavor::ForgetRule::Keep { why } = surface.forget {
            assert!(
                !why.is_empty(),
                "{} survives forget and must say why",
                surface.table
            );
        }
    }

    // And the same for erase and export, which is the asymmetry the plan
    // (§2.5 item 6) calls out: exclusions are declared per table.
    for surface in &surfaces {
        if let proxima_core::flavor::ExportRule::Excluded { why } = surface.export {
            assert!(
                !why.is_empty(),
                "{} is held out of the export bundle and must say why",
                surface.table
            );
        }
        if let proxima_core::flavor::EraseRule::Never { why } = surface.erase {
            assert!(
                !why.is_empty(),
                "{} outlives an owner erase and must say why",
                surface.table
            );
        }
    }
}

// ── §2.5 (4): transfers announce everywhere ─────────────────────────────

/// `proxima_core.announce` is a declared surface with a stated transfer
/// rule, so a transfer cannot move a memory and leave its change log
/// behind — nor take another owner's log with it.
#[tokio::test]
async fn transfer_is_announced_and_the_announce_surface_is_declared() {
    let announce = FLAVOR_0
        .all_surfaces()
        .find(|surface| surface.table == "proxima_core.announce")
        .expect("flavor #0 declares the announce log");

    assert!(
        matches!(announce.transfer, TransferRule::RetainAtSource { .. }),
        "an announce row records what happened to an owner, not to a memory: \
         it stays where it was written, so a transfer's own row survives at \
         the source. Declared, not inferred: {:?}",
        announce.transfer
    );

    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;

        // `transfer` is one of the four announce ops, in the enum, in this
        // order. The guardrail asserts the order; this asserts the lane
        // exists at all, which is what "announced everywhere" rests on.
        let ops: Vec<String> = sqlx::query_scalar(
            "SELECT e.enumlabel::text
               FROM pg_enum e
               JOIN pg_type t ON t.oid = e.enumtypid
               JOIN pg_namespace n ON n.oid = t.typnamespace
              WHERE n.nspname = 'proxima_core' AND t.typname = 'announce_op'
              ORDER BY e.enumsortorder",
        )
        .fetch_all(pg.pool_for_tests())
        .await?;
        assert_eq!(ops, vec!["append", "forget", "erase", "transfer"]);
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("announce lane test failed");
}

// ── The embedding byte-parity gate ──────────────────────────────────────

/// Structural half: every flavor #0 recipe resolves to exactly the
/// `(table, column)` pair the shipped `embed_text_column` path reads.
///
/// `EmbeddingRecipe` generalizes `EMBEDDABLE: bool` + `embed_text_column`.
/// A generalization that quietly changed which column is embedded would
/// re-embed the whole corpus, so the pairs are compared both ways: no
/// schema gains a unit it did not have, and none loses one.
#[test]
fn every_recipe_resolves_to_the_pair_the_shipped_drain_reads() {
    let registry = FlavorRegistry::new().freeze_or_panic_for_tests();

    for schema in FLAVOR_0.schemas {
        let schema_id = schema.schema_id();
        let projection = registry
            .search_projections()
            .iter()
            .find(|projection| projection.schema_id == schema_id);
        let shipped = projection.and_then(|projection| projection.embed_text_column.clone());

        let resolved = schema.embedding.resolve(schema.sidecar_table);
        if let Some(column) = shipped {
            assert_eq!(resolved.len(), 1, "{}", schema_id.as_str());
            assert_eq!(resolved[0].table, schema.sidecar_table);
            assert_eq!(
                resolved[0].column,
                Some(column.as_str()),
                "{}: the recipe must resolve to the shipped column",
                schema_id.as_str()
            );
            assert_eq!(resolved[0].slot, SLOT_DEFAULT);
        } else {
            assert!(
                resolved.is_empty(),
                "{}: nothing embeds this schema today, so the recipe must \
                 produce no units",
                schema_id.as_str()
            );
            assert!(
                schema.embedding.is_never(),
                "{}: and it must say so as Never, with a reason",
                schema_id.as_str()
            );
        }
    }
}

/// Byte-parity half: the text a recipe-resolved column yields is byte-for-byte
/// the text the shipped path embeds.
///
/// The structural test above proves the recipe names the same column. This
/// one reads it. `embed_text` is a generated column in every case, so the
/// value is the migration's expression applied to real row data — the thing
/// a resolved unit has to reproduce, not a string the test chose.
#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn each_recipe_reproduces_the_bytes_the_shipped_path_embeds() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);

    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let pool = pg.pool_for_tests();

        let owner_id = Uuid::now_v7();
        sqlx::query("INSERT INTO proxima_core.owners (owner_id, kind) VALUES ($1, 'personal')")
            .bind(owner_id)
            .execute(pool)
            .await?;

        // Text with a mix of whitespace, punctuation and non-ASCII, so a
        // recipe that trimmed, normalized or re-joined differently from the
        // generated column would not survive the comparison.
        let title = "  Ferner liefen: Größe & Maß  ";
        let body = "zwei\nZeilen — mit\tTabs";
        let claim = "  Der Anspruch ist mehrdeutig  ";
        let utterance = "  hallo, Welt!  ";

        let seeds: Vec<(&str, &str, Uuid)> = vec![
            ("core/agent-note-v1", "fact", Uuid::now_v7()),
            ("core/utterance-v1", "fact", Uuid::now_v7()),
            ("core/agent-derivation-v1", "abstraction", Uuid::now_v7()),
            ("core/interpretation-v1", "perspective", Uuid::now_v7()),
        ];

        for (schema_id, kind, t) in &seeds {
            let handle = Uuid::now_v7();
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
            let content_id: Option<Uuid> = if *kind == "fact" {
                None
            } else {
                let content_id = Uuid::now_v7();
                sqlx::query(
                    "INSERT INTO proxima_core.content
                         (content_id, owner_id, schema_id, content_hash)
                     VALUES ($1, $2, $3, sha256($4::bytea))",
                )
                .bind(content_id)
                .bind(owner_id)
                .bind(schema_id)
                .bind(content_id.as_bytes().as_slice())
                .execute(pool)
                .await?;
                Some(content_id)
            };
            // `memory_pin_checks`: an abstraction pins a Fact, a perspective
            // pins an Abstraction. The seed order below is that chain.
            let origins: Vec<Uuid> = match *kind {
                "abstraction" => vec![seeds[0].2],
                "perspective" => vec![seeds[2].2],
                _ => Vec::new(),
            };
            sqlx::query(
                "INSERT INTO proxima_core.memory
                     (handle, t, kind, owner_id, schema_id, content_id, origins, sidecar_tables)
                 VALUES ($1, $2, $3::proxima_core.memory_kind, $4, $5, $6, $7, '{}')",
            )
            .bind(handle)
            .bind(t)
            .bind(kind)
            .bind(owner_id)
            .bind(schema_id)
            .bind(content_id)
            .bind(&origins)
            .execute(pool)
            .await?;
        }

        sqlx::query(
            "INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body, tags)
             VALUES ($1, $2, $3, $4, ARRAY['ümlaut', 'zwei worte'])",
        )
        .bind(seeds[0].2)
        .bind(Uuid::now_v7())
        .bind(title)
        .bind(body)
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.utterance_v1 (t, speaker, conversation_id, text)
             VALUES ($1, 'agent', 'conv-1', $2)",
        )
        .bind(seeds[1].2)
        .bind(utterance)
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.agent_derivation_v1
                 (t, title, body, tags, model_id, client_name, client_version)
             VALUES ($1, $2, $3, ARRAY['abgeleitet'], 'm', 'c', 'v')",
        )
        .bind(seeds[2].2)
        .bind(title)
        .bind(body)
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.interpretation_v1
                 (t, claim, confidence, model_id, client_name, client_version)
             VALUES ($1, $2, 70, 'm', 'c', 'v')",
        )
        .bind(seeds[3].2)
        .bind(claim)
        .execute(pool)
        .await?;

        let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
        let owner = proxima_core::Owner::Personal(proxima_core::UserId::new(owner_id));

        let mut compared = 0_usize;
        for (schema_id, _, t) in &seeds {
            let schema = FLAVOR_0
                .schemas
                .iter()
                .find(|candidate| candidate.schema_id().as_str() == *schema_id)
                .expect("seeded schema is declared by flavor #0");

            let units = schema.embedding.resolve(schema.sidecar_table);
            assert_eq!(units.len(), 1, "{schema_id} embeds one unit in v0.0.8");
            let unit = units[0];
            assert_eq!(
                unit.slot, SLOT_DEFAULT,
                "{schema_id}: only `default` is wired"
            );
            let (Some(table), Some(column)) = (unit.table, unit.column) else {
                panic!("{schema_id}: the recipe must resolve to a stored column")
            };
            assert!(
                matches!(schema.embedding.units()[0].text, EmbedText::StoredColumn(_)),
                "{schema_id}: the shipped idiom is a stored column"
            );

            // What the recipe says to read, read literally.
            // SQL-POLICY: fixed-fragment — `table` and `column` are
            // `&'static str` off the compiled-in contract, not runtime input.
            let recipe_text: Option<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "SELECT c.{column} FROM {table} c WHERE c.t = $1"
            )))
            .bind(t)
            .fetch_one(pool)
            .await?;

            // What the drain reads today.
            let shipped_text = proxima_storage_pg::verbs::fact_embeddings::load_embedding_text(
                pool,
                &owner,
                entity_kind_of(schema.kind),
                proxima_core::MemoryId::new(*t),
                registry.non_embeddable_schema_ids(),
                registry.search_projections(),
            )
            .await?;

            assert!(
                recipe_text.is_some(),
                "{schema_id}: the seeded row has embed text"
            );
            assert_eq!(
                recipe_text.as_deref().map(str::as_bytes),
                shipped_text.as_deref().map(str::as_bytes),
                "{schema_id}: byte parity between the recipe and the shipped path"
            );
            compared += 1;
        }

        assert_eq!(
            compared, 4,
            "all four embeddable core schemas were compared"
        );
        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("embedding byte-parity test failed");
}

fn entity_kind_of(kind: PayloadKind) -> proxima_core::EntityKind {
    match kind {
        PayloadKind::Fact => proxima_core::EntityKind::Fact,
        PayloadKind::Abstraction => proxima_core::EntityKind::Abstraction,
        PayloadKind::Perspective => proxima_core::EntityKind::Perspective,
        other => panic!("{other:?} has no embedding lane"),
    }
}
