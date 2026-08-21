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

/// Whether `source` declares `symbol`, a Rust path like
/// `Engine::transfer_to_owner` or a bare `transfer_to_owner`.
///
/// Two bounds carry the weight. The `(` in the needle is what stops a
/// SUFFIXED rename from answering for the old name: without it,
/// `fn transfer_to_owner_v2` contains `fn transfer_to_owner`, so a
/// tree-wide rename passes the very test that exists to catch it. And a
/// qualified citation has to land under a matching `impl`, so a free
/// function of the same name elsewhere in the file does not answer for an
/// `Engine::` citation.
///
/// LIMITS, both deliberate. The impl tracker does not count braces: it
/// means "the nearest preceding `impl` header", not "inside that block", so
/// an item after a block closes is still attributed to it. And the generic
/// list is skipped to the first `>`, which a nested generic would defeat.
/// Both are enough to separate a free function from an inherent method,
/// which is the confusion worth guarding against here.
fn declares(source: &str, symbol: &str) -> bool {
    let mut segments = symbol.rsplit("::");
    let Some(item) = segments.next() else {
        return false;
    };
    let qualifier = segments.next();
    let needle = format!("fn {item}(");
    let mut nearest_impl: Option<&str> = None;
    for line in source.lines() {
        if let Some(target) = impl_target(line) {
            nearest_impl = Some(target);
        }
        if line.contains(&needle) && (qualifier.is_none() || nearest_impl == qualifier) {
            return true;
        }
    }
    false
}

/// The type an `impl` header hangs its items on: `Engine` for
/// `impl Engine {`, `PgStore` for `impl<S> Store for PgStore<S> {`.
fn impl_target(line: &str) -> Option<&str> {
    let rest = line.trim_start().strip_prefix("impl")?;
    if !rest.starts_with([' ', '<']) {
        return None;
    }
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('<').map_or(rest, |generics| {
        generics.split_once('>').map_or(generics, |(_, tail)| tail)
    });
    let target = rest.rsplit(" for ").next().unwrap_or(rest).trim_start();
    let ident = target
        .split(|ch: char| !(ch.is_alphanumeric() || ch == '_'))
        .next()?;
    (!ident.is_empty()).then_some(ident)
}

/// Whether the migrated catalog carries a non-internal trigger `name` on
/// `relation`.
///
/// Not `tgisinternal`: a constraint's own internal trigger is the
/// constraint's enforcement, and a `Trigger` citation claims a trigger of
/// its own.
async fn trigger_exists(
    pool: &sqlx::PgPool,
    relation: &str,
    name: &str,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT EXISTS (
           SELECT 1
             FROM pg_trigger g
             JOIN pg_class t ON t.oid = g.tgrelid
            WHERE NOT g.tgisinternal
              AND g.tgname = $2
              AND (t.relnamespace::regnamespace)::text || '.' || t.relname = $1
         )",
    )
    .bind(relation)
    .bind(name)
    .fetch_one(pool)
    .await
}

/// Whether the migrated catalog carries constraint `name` on `relation`.
///
/// A separate statement rather than a union with the trigger one: the two
/// catalogs have different columns, and a citation of a constraint must not
/// be satisfied by a trigger that happens to share its name.
async fn constraint_exists(
    pool: &sqlx::PgPool,
    relation: &str,
    name: &str,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT EXISTS (
           SELECT 1
             FROM pg_constraint c
             JOIN pg_class t ON t.oid = c.conrelid
            WHERE c.conname = $2
              AND (t.relnamespace::regnamespace)::text || '.' || t.relname = $1
         )",
    )
    .bind(relation)
    .bind(name)
    .fetch_one(pool)
    .await
}

/// Every cited enforcement site resolves to something that exists.
///
/// `Enforcement::EngineRefusal`/`StorageBackstop` carry a free-form
/// `&'static str`, which the compiler cannot check: rename the function and
/// the contract goes on claiming a refusal at an address nothing answers.
/// The citation format is `<crate-dir>/<path>::<symbol path>`, so both
/// halves are resolvable — the file relative to the workspace root, and the
/// symbol as an item declared in it under the right `impl`.
///
/// The two DDL arms are resolved against the CATALOG of a migrated
/// database, not against the migration's source text. What a `CREATE
/// TRIGGER` line says and what the server ended up with are different
/// claims: a later `DROP TRIGGER`, a rename, a relation that the statement
/// names through a search path, or a trigger created inside a `DO` block
/// all separate them, and only one of the two is what a transfer attempt
/// actually meets. `pg_trigger`/`pg_constraint` answer the question the
/// declaration is making — *this relation is guarded by this thing* —
/// including the relation, which the source-text version could only check
/// for `Trigger` and not for `Constraint`.
#[tokio::test]
async fn every_cited_enforcement_site_resolves() {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("crates/storage-pg sits two levels under the workspace root")
        .to_owned();

    let mut ddl: Vec<Enforcement> = Vec::new();
    let mut checked = 0_usize;
    let rules = FLAVOR_0
        .schemas
        .iter()
        .map(|schema| schema.transfer)
        .chain(FLAVOR_0.all_surfaces().map(|surface| surface.transfer));
    for rule in rules {
        let TransferRule::NotTransferable { enforced_by, .. } = rule else {
            continue;
        };
        for site in enforced_by {
            checked += 1;
            match site {
                Enforcement::EngineRefusal { at } | Enforcement::StorageBackstop { at } => {
                    let (path, symbol) = at
                        .split_once("::")
                        .unwrap_or_else(|| panic!("{at} must cite <path>::<symbol>"));
                    let file = workspace.join("crates").join(path);
                    let source = std::fs::read_to_string(&file).unwrap_or_else(|err| {
                        panic!("{at} cites {} which does not read: {err}", file.display())
                    });
                    assert!(
                        declares(&source, symbol),
                        "{at} cites `{symbol}`, which {} does not declare",
                        file.display()
                    );
                }
                Enforcement::Trigger(_) | Enforcement::Constraint(_) => ddl.push(*site),
            }
        }
    }
    assert!(
        checked >= 3,
        "goals alone cite three sites; found {checked}"
    );
    assert!(
        !ddl.is_empty(),
        "goals cite a DDL backstop; the catalog half of this test must have something to ask about"
    );

    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        for site in ddl {
            let pool = pg.pool_for_tests();
            let (kind, relation, name, exists) = match site {
                Enforcement::Trigger(t) => (
                    "trigger",
                    t.relation,
                    t.name,
                    trigger_exists(pool, t.relation, t.name).await?,
                ),
                Enforcement::Constraint(c) => (
                    "constraint",
                    c.relation,
                    c.name,
                    constraint_exists(pool, c.relation, c.name).await?,
                ),
                Enforcement::EngineRefusal { .. } | Enforcement::StorageBackstop { .. } => {
                    unreachable!("the source-text arms were resolved above")
                }
            };
            assert!(
                exists,
                "the contract cites {kind} {name} on {relation}, and the migrated catalog has \
                 no such {kind} on that relation"
            );
        }
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("every_cited_enforcement_site_resolves failed");
}

// ── §2.5 (2): declared absence ──────────────────────────────────────────

/// A non-surface is a value, not an omission.
///
/// Before the contract, `search_projection() -> None` was reachable three
/// ways — no projection, empty fields, no sidecar table — so a schema that
/// deliberately does not search was indistinguishable from one whose
/// declaration was forgotten. The plan's acceptance case is spelled
/// "ChatTurn-style"; utterances are a search surface in this tree — they
/// carry their own score band — so the declared-absence exemplars are the
/// schemas that really do decline.
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

/// Every declared non-count states a reason, in every registered flavor.
///
/// `Surface::counter` was `Option<&'static str>` and `None` was the last
/// declared absence in the contract with nothing attached — "feeds no
/// counter" and "nobody said" were the same value. The seven `None`s in the
/// shipped tree turned out to have six DIFFERENT reasons: a pointer into a
/// counted table, a refcounted shared row, a work queue counted after the
/// commit, a derived index with no `rows_affected`, a detail table already
/// counted under its parent, and two surfaces the erase never touches at
/// all. None of that was recoverable from the word `None`.
#[test]
fn every_declared_non_count_says_why() {
    let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
    let mut uncounted = 0;
    let mut counted = 0;
    for contract in registry.contracts() {
        for surface in contract.all_surfaces() {
            match surface.counter {
                proxima_core::flavor::CounterRule::Counted(key) => {
                    assert!(
                        !key.is_empty(),
                        "{} names an empty counter key",
                        surface.table
                    );
                    counted += 1;
                }
                proxima_core::flavor::CounterRule::Uncounted { why } => {
                    assert!(
                        why.len() > 40,
                        "{} contributes to no count and must say why, not \
                         gesture at it: {why:?}",
                        surface.table
                    );
                    uncounted += 1;
                }
            }
        }
    }
    assert!(
        counted > 10 && uncounted >= 7,
        "{counted} counted, {uncounted} uncounted"
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
/// `(table, column)` pair the shipped embed-text drain reads.
///
/// `EmbeddingRecipe` generalizes `EMBEDDABLE: bool` + the old
/// `embed_text_column`. A generalization that quietly changed which column
/// is embedded would re-embed the whole corpus, so the pairs are compared
/// both ways: no schema gains a unit it did not have, and none loses one.
#[test]
fn every_recipe_resolves_to_the_pair_the_shipped_drain_reads() {
    let registry = FlavorRegistry::new().freeze_or_panic_for_tests();

    for schema in FLAVOR_0.schemas {
        let schema_id = schema.schema_id();
        let shipped = registry
            .embed_units()
            .iter()
            .find(|unit| unit.schema_id == schema_id)
            .map(|unit| unit.column.clone());

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
                registry.embed_units(),
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

// ── Phase C: a declared cascade is a cascade the catalog enforces ────────

/// `EraseRule::Cascade { via }` is a claim about the database, and the
/// consolidated owner erase acts on it: a cascading surface gets no
/// statement, because the constraint is the proof. Until this test, nothing
/// in core checked the claim, and two of flavor #0's three cascades were
/// false — `memory_head` named a `NO ACTION` foreign key that runs the other
/// way, and `content` named a Rust function. Both were corrected in the same
/// commit that added this gate; generating from the declarations first would
/// have left every erased owner's head rows behind.
///
/// Lifted from `flavors/code/tests/erase_repo_pg.rs`, which has asked
/// `pg_constraint` this question since Phase 2. The core copy is the one
/// that matters: flavor #0 speaks for the kernel spine.
#[tokio::test]
async fn every_cascade_flavor_zero_declares_is_a_cascade_the_schema_enforces() {
    let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
    let mut relations: Vec<String> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    for contract in registry.contracts() {
        for surface in contract.all_surfaces() {
            if let proxima_core::flavor::EraseRule::Cascade { via } = surface.erase {
                relations.push(via.relation.to_owned());
                names.push(via.name.to_owned());
            }
        }
    }
    assert!(
        relations
            .iter()
            .any(|relation| relation == "proxima_core.projection"),
        "the projection is the surface owner erase reaches on its cascade declaration \
         alone, so it is the one this gate may not be blind to; found {relations:?}"
    );

    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let unenforced: Vec<(String, String)> = sqlx::query_as(
            "SELECT d.relation, d.name
               FROM unnest($1::text[], $2::text[]) AS d(relation, name)
              WHERE NOT EXISTS (
                    SELECT 1
                      FROM pg_constraint c
                      JOIN pg_class src ON src.oid = c.conrelid
                     WHERE c.conname = d.name
                       AND c.contype = 'f'
                       AND c.confdeltype = 'c'
                       AND (src.relnamespace::regnamespace)::text || '.' || src.relname
                           = d.relation
                )",
        )
        .bind(&relations)
        .bind(&names)
        .fetch_all(pg.pool_for_tests())
        .await?;
        assert!(
            unenforced.is_empty(),
            "these surfaces declare EraseRule::Cascade, and the owner erase emits no \
             statement for them on that declaration, but no ON DELETE CASCADE foreign \
             key of that name backs it: {unenforced:?}"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("every_cascade_flavor_zero_declares_is_a_cascade_the_schema_enforces failed");
}

/// Every column a declaration NAMES is a column the catalog HAS.
///
/// `KeyShape` and `FollowOrDedupe { dedupe_key, remaps }` spell column
/// names as `&'static str`, which the compiler cannot check against a
/// table it has never seen. Three phases of evidence say the compiler not
/// checking them is not a theoretical worry:
///
/// - four code-flavor detail tables declared `Custom(&["memory_id"])`, a
///   column none of them has, and it went unnoticed for as long as nothing
///   read the field for a `Cascade` surface;
/// - `proxima_core.projection` declared its memory key as `t` until Phase
///   4, and the column is `memory_id` — again unread, again untrue;
/// - `blob`'s `remaps` named three columns where the shipped SQL touched
///   two.
///
/// All three are the same defect: a string nothing resolves. This resolves
/// every one of them against `information_schema`, so the next one fails a
/// test instead of reaching a generator.
///
/// `owner_columns` is deliberately in scope too. It is genuinely consumed
/// by the export generator, but only for surfaces the bundle carries — an
/// `Excluded` surface's owner column has the same unread-string shape as
/// the two above.
#[tokio::test]
async fn every_column_a_declaration_names_is_a_column_the_catalog_has() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;

        // (schema-qualified table, column, what named it) — flattened so
        // one round trip answers for the whole contract.
        let mut cited: Vec<(String, String, String)> = Vec::new();
        for surface in FLAVOR_0.all_surfaces() {
            for column in surface.key.columns() {
                cited.push((surface.table.to_owned(), column.to_owned(), "key".into()));
            }
            for column in surface.owner_columns {
                cited.push((
                    surface.table.to_owned(),
                    (*column).to_owned(),
                    "owner_columns".into(),
                ));
            }
            if let Some(column) = surface.lexical_language_column {
                cited.push((
                    surface.table.to_owned(),
                    column.to_owned(),
                    "lexical_language_column".into(),
                ));
            }
            if let TransferRule::FollowOrDedupe { dedupe_key, remaps } = surface.transfer {
                for column in dedupe_key {
                    cited.push((
                        surface.table.to_owned(),
                        (*column).to_owned(),
                        "dedupe_key".into(),
                    ));
                }
                // A remap names the REFERRING table, not this surface:
                // `blob`'s remaps are columns on `memory` and `cooled`.
                for entry in remaps {
                    let (table, column) = entry
                        .split_once('.')
                        .unwrap_or_else(|| panic!("{entry} must be <table>.<column>"));
                    cited.push((
                        format!("proxima_core.{table}"),
                        column.to_owned(),
                        "remaps".into(),
                    ));
                }
            }
        }
        assert!(
            cited.len() > 40,
            "flavor #0 names columns on the kernel spine as well as its own sidecars; \
             found {}",
            cited.len()
        );

        let tables: Vec<String> = cited.iter().map(|(table, ..)| table.clone()).collect();
        let columns: Vec<String> = cited.iter().map(|(_, column, _)| column.clone()).collect();
        let sources: Vec<String> = cited.iter().map(|(.., source)| source.clone()).collect();
        let missing: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT d.table_name, d.column_name, d.source
               FROM unnest($1::text[], $2::text[], $3::text[])
                    AS d(table_name, column_name, source)
              WHERE NOT EXISTS (
                    SELECT 1
                      FROM information_schema.columns c
                     WHERE c.table_schema || '.' || c.table_name = d.table_name
                       AND c.column_name = d.column_name
              )",
        )
        .bind(&tables)
        .bind(&columns)
        .bind(&sources)
        .fetch_all(pg.pool_for_tests())
        .await?;
        assert!(
            missing.is_empty(),
            "these declarations name columns no relation has, which is a string the \
             compiler cannot check and the catalog can: {missing:?}"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("every_column_a_declaration_names_is_a_column_the_catalog_has failed");
}

/// Every `dedupe_key` is a key the schema actually enforces as unique.
///
/// `FollowOrDedupe`'s generated half asks the destination owner "do you
/// already hold this row?" and repoints the referring columns at whatever
/// comes back. That question has ONE answer only if the declared columns
/// are unique on the destination. If they are not, the lookup picks an
/// arbitrary row out of several and the transfer silently rehomes a
/// memory onto a body it did not have — a corruption no error reports.
///
/// So the declaration is not free to name any tuple it likes: it must name
/// a tuple `pg_constraint` agrees is unique. This is the `dedupe_key`
/// counterpart of the `Cascade`/`confdeltype` gate above, and the reason it
/// is a catalog question rather than a code review one is that the SQL is
/// generated from the string — nothing in Rust can see the constraint.
#[tokio::test]
async fn every_dedupe_key_is_a_uniqueness_the_schema_enforces() {
    let declared: Vec<(&'static str, &'static [&'static str])> = FLAVOR_0
        .all_surfaces()
        .filter_map(|surface| match surface.transfer {
            TransferRule::FollowOrDedupe { dedupe_key, .. } => Some((surface.table, dedupe_key)),
            _ => None,
        })
        .collect();
    assert_eq!(
        declared.len(),
        2,
        "blob and content are flavor #0's dedupe surfaces; found {declared:?}"
    );

    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        for (table, dedupe_key) in declared {
            let columns: Vec<String> = dedupe_key.iter().map(|c| (*c).to_owned()).collect();
            // Both a UNIQUE constraint and a bare unique index enforce the
            // same thing; `pg_index` sees both, so ask it rather than
            // `pg_constraint`, and compare the column SET (order is the
            // index's business, not the declaration's).
            let enforced: bool = sqlx::query_scalar(
                "SELECT EXISTS (
                   SELECT 1
                     FROM pg_index i
                     JOIN pg_class t ON t.oid = i.indrelid
                    WHERE i.indisunique
                      AND NOT i.indisexclusion
                      AND i.indpred IS NULL
                      AND (t.relnamespace::regnamespace)::text || '.' || t.relname = $1
                      AND (
                            SELECT array_agg(a.attname::text ORDER BY a.attname::text)
                              FROM unnest(i.indkey::int[]) AS k(attnum)
                              JOIN pg_attribute a
                                ON a.attrelid = i.indrelid AND a.attnum = k.attnum
                          ) = (SELECT array_agg(x ORDER BY x) FROM unnest($2::text[]) AS x)
                 )",
            )
            .bind(table)
            .bind(&columns)
            .fetch_one(pg.pool_for_tests())
            .await?;
            assert!(
                enforced,
                "{table} declares TransferRule::FollowOrDedupe with dedupe_key \
                 {dedupe_key:?}, and the transfer generator trusts that tuple to \
                 identify at most one destination-owned row, but no unique index \
                 over exactly those columns exists"
            );
        }
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("every_dedupe_key_is_a_uniqueness_the_schema_enforces failed");
}
