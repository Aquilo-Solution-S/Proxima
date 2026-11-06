//! Offline retrieval evaluation over authored text and explicit judgments.
//! Fixed feature hashes are fixture encoders, not production embedding models.

#[path = "retrieval_quality/corpus.rs"]
mod corpus;

use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use proxiima_core::storage_ports::MemoryReadPort;
use proxiima_core::test_fixtures::owner_scope_for_tests;
use proxiima_core::verbs::query::{
    EntityKind, MemorySearchRequest, MemorySearchScope, SearchMode, SearchOrder,
    SupersessionStatus, TagMatch,
};
use proxiima_core::{EmbeddingDim, EmbeddingSpace, OwnerId, SpaceVector, UserId};
use proxiima_storage_pg::test_fixtures::fresh_pg;
use uuid::Uuid;

use corpus::{NOTES, QUERIES};

type TestResult = Result<(), Box<dyn std::error::Error>>;
const K: usize = 5;

fn owner() -> OwnerId {
    OwnerId::from(UserId::new(Uuid::now_v7()))
}
fn count(value: usize) -> f64 {
    f64::from(u32::try_from(value).expect("bounded fixture count"))
}

#[derive(Clone, Copy, serde::Serialize)]
struct Metrics {
    hits: usize,
    recall: f64,
    reciprocal_rank: f64,
    ndcg: f64,
    covered: bool,
}

fn metrics(relevant: &[Uuid], ranked: &[Uuid], k: usize) -> Metrics {
    let relevant: BTreeSet<_> = relevant.iter().copied().collect();
    let ranks: Vec<_> = ranked
        .iter()
        .take(k)
        .enumerate()
        .filter_map(|(rank, id)| relevant.contains(id).then_some(rank + 1))
        .collect();
    let dcg: f64 = ranks
        .iter()
        .map(|rank| 1.0 / (count(*rank) + 1.0).log2())
        .sum();
    let ideal: f64 = (1..=relevant.len().min(k))
        .map(|rank| 1.0 / (count(rank) + 1.0).log2())
        .sum();
    Metrics {
        hits: ranks.len(),
        recall: if relevant.is_empty() {
            0.0
        } else {
            count(ranks.len()) / count(relevant.len())
        },
        reciprocal_rank: ranks.first().map_or(0.0, |rank| 1.0 / count(*rank)),
        ndcg: if ideal == 0.0 { 0.0 } else { dcg / ideal },
        covered: !ranks.is_empty(),
    }
}

#[test]
fn relevance_and_rank_determine_distinct_metrics() {
    let ids = Uuid::from_u128;
    let actual = metrics(&[ids(1), ids(3)], &[ids(9), ids(1), ids(2), ids(3)], 3);
    assert_eq!(actual.hits, 1);
    assert!((actual.recall - 0.5).abs() < 1e-12);
    assert!((actual.reciprocal_rank - 0.5).abs() < 1e-12);
    let expected = (1.0 / 3.0_f64.log2()) / (1.0 + 1.0 / 3.0_f64.log2());
    assert!((actual.ndcg - expected).abs() < 1e-12);
    assert!(actual.covered);
    let miss = metrics(&[ids(1)], &[ids(9)], 5);
    assert_eq!(
        (miss.hits, miss.recall, miss.reciprocal_rank, miss.ndcg),
        (0, 0.0, 0.0, 0.0)
    );
    assert!(!miss.covered);
}

#[derive(Clone, Copy)]
enum Encoder {
    Words,
    Trigrams,
}
impl Encoder {
    fn space(self) -> EmbeddingSpace {
        match self {
            Self::Words => EmbeddingSpace::new("fixture/fnv-word-v1", EmbeddingDim::D384),
            Self::Trigrams => EmbeddingSpace::new("fixture/fnv-trigram-v1", EmbeddingDim::D768),
        }
    }
    fn encode(self, text: &str) -> SpaceVector {
        let space = self.space();
        let mut values = vec![0.0; space.dim().width()];
        let lowered = text.to_ascii_lowercase();
        let mut add = |unit: &[u8]| {
            let hash = unit
                .iter()
                .fold(14_695_981_039_346_656_037_u64, |hash, byte| {
                    (hash ^ u64::from(*byte)).wrapping_mul(1_099_511_628_211)
                });
            let width = u64::try_from(values.len()).expect("supported width");
            let index = usize::try_from(hash % width).expect("index is below vector width");
            values[index] += 1.0;
        };
        match self {
            Self::Words => {
                for word in lowered
                    .split(|c: char| !c.is_ascii_alphanumeric())
                    .filter(|s| !s.is_empty())
                {
                    add(word.as_bytes());
                }
            }
            Self::Trigrams => {
                for unit in lowered.as_bytes().windows(3) {
                    add(unit);
                }
            }
        }
        SpaceVector::new(space, values).expect("fixture encoder supplies declared width")
    }
}

async fn seed_note(
    pool: &sqlx::PgPool,
    owner: OwnerId,
    note: &corpus::Note,
) -> Result<Uuid, sqlx::Error> {
    sqlx::query("INSERT INTO proxiima_core.owners(owner_id,kind) VALUES($1,'personal') ON CONFLICT DO NOTHING")
        .bind(owner.into_inner()).execute(pool).await?;
    let handle = Uuid::now_v7();
    let t = Uuid::now_v7();
    let mut tx = pool.begin().await?;
    sqlx::query("INSERT INTO proxiima_core.memory_head(handle,kind,schema_id,owner_id,t) VALUES($1,'fact','core/agent-note-v1',$2,$3)")
        .bind(handle).bind(owner.into_inner()).bind(t).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO proxiima_core.memory(handle,t,kind,owner_id,schema_id,sidecar_tables,occurred_at,\"by\",via) VALUES($1,$2,'fact',$3,'core/agent-note-v1',ARRAY['proxiima_core.agent_note_v1'],uuid_extract_timestamp($2),'00000000-0000-0000-0000-000000000509'::uuid,NULL)")
        .bind(handle).bind(t).bind(owner.into_inner()).execute(&mut *tx).await?;
    sqlx::query(
        "INSERT INTO proxiima_core.agent_note_v1(t,note_id,title,body,tags) VALUES($1,$2,$3,$4,$5)",
    )
    .bind(t)
    .bind(Uuid::now_v7())
    .bind(note.key)
    .bind(note.text)
    .bind(vec![note.tag])
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    let schema = proxiima_core::SPIICE_0
        .schemas
        .iter()
        .find(|schema| schema.schema_id().as_str() == "core/agent-note-v1")
        .expect("declared note schema");
    let sql =
        proxiima_storage_pg::projection::projection_insert_sql(&proxiima_core::SPIICE_0, schema)
            .expect("generated projection SQL");
    // SQL-POLICY: generated
    sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(t)
        .bind(None::<&str>)
        .bind("core/agent-note-v1")
        .execute(pool)
        .await?;
    Ok(t)
}

async fn seed_embedding(
    pool: &sqlx::PgPool,
    owner: OwnerId,
    entity: Uuid,
    vector: &SpaceVector,
) -> Result<(), sqlx::Error> {
    let width = i16::try_from(vector.space().dim().width()).expect("supported dimension fits i16");
    sqlx::query("INSERT INTO proxiima_core.embeddings(entity_id,model_id,dim,embedding_version,chunk_ordinal,vec,owner_id) VALUES($1,$2,$3,1,0,$4,$5)")
        .bind(entity).bind(vector.space().model_id()).bind(width).bind(pgvector::Vector::from(vector.values().to_vec())).bind(owner.into_inner()).execute(pool).await?;
    sqlx::query("INSERT INTO proxiima_core.embedding_heads(entity_id,model_id,dim,embedding_version,owner_id) VALUES($1,$2,$3,1,$4)")
        .bind(entity).bind(vector.space().model_id()).bind(width).bind(owner.into_inner()).execute(pool).await?;
    Ok(())
}

fn request(
    owner: OwnerId,
    query: &str,
    mode: SearchMode,
    semantic: SpaceVector,
    tags: Vec<String>,
) -> MemorySearchRequest {
    MemorySearchRequest {
        scope: MemorySearchScope::Owner(owner),
        read_owners: vec![owner],
        query: query.into(),
        mode,
        supersession: SupersessionStatus::HeadsOnly,
        limit: 5,
        kind: Some(EntityKind::Fact),
        schema_id: None,
        tags,
        tag_match: TagMatch::All,
        since: None,
        until: None,
        order: SearchOrder::Relevance,
        min_score: None,
        semantic_weight: (mode == SearchMode::Hybrid).then_some(0.5),
        after: None,
        semantic: Some(semantic),
    }
}

#[tokio::test]
async fn pg_queries_keep_owner_model_and_tag_admission_separate() -> TestResult {
    let (pg, _guard) = fresh_pg("retrieval_scope").await;
    let a = owner();
    let b = owner();
    let a_words = seed_note(pg.pool_for_tests(), a, &NOTES[0][0]).await?;
    let a_trigrams = seed_note(pg.pool_for_tests(), a, &NOTES[0][3]).await?;
    let foreign = seed_note(pg.pool_for_tests(), b, &NOTES[1][0]).await?;
    let query = "supplier invoice";
    seed_embedding(
        pg.pool_for_tests(),
        a,
        a_words,
        &Encoder::Words.encode(query),
    )
    .await?;
    seed_embedding(
        pg.pool_for_tests(),
        a,
        a_trigrams,
        &Encoder::Trigrams.encode(query),
    )
    .await?;
    seed_embedding(
        pg.pool_for_tests(),
        b,
        foreign,
        &Encoder::Words.encode(query),
    )
    .await?;
    let registry = proxiima_core::SpiiceRegistry::new().freeze_or_panic_for_tests();
    for (encoder, expected, tag) in [
        (Encoder::Words, a_words, "finance"),
        (Encoder::Trigrams, a_trigrams, "ops"),
    ] {
        let mut req = request(
            a,
            query,
            SearchMode::Semantic,
            encoder.encode(query),
            vec![tag.into()],
        );
        let page = pg
            .search_memories(
                Some(&owner_scope_for_tests(&[a])),
                &req,
                registry.search_projections(),
            )
            .await?;
        assert_eq!(
            page.results
                .iter()
                .map(|hit| hit.memory_id.into_inner())
                .collect::<Vec<_>>(),
            vec![expected]
        );
        req.tags = vec!["no-such-tag".into()];
        assert!(
            pg.search_memories(
                Some(&owner_scope_for_tests(&[a])),
                &req,
                registry.search_projections()
            )
            .await?
            .results
            .is_empty()
        );
    }
    Ok(())
}

#[derive(Default, serde::Serialize)]
struct Aggregate {
    queries: usize,
    judged_relevant: usize,
    retrieved: usize,
    hits: usize,
    recall_at_5: f64,
    mrr: f64,
    ndcg_at_5: f64,
    coverage: f64,
    latency_ms: f64,
}
impl Aggregate {
    fn add(&mut self, measured: Metrics, relevant: usize, retrieved: usize, latency_ms: f64) {
        self.queries += 1;
        self.judged_relevant += relevant;
        self.retrieved += retrieved;
        self.hits += measured.hits;
        self.recall_at_5 += measured.recall;
        self.mrr += measured.reciprocal_rank;
        self.ndcg_at_5 += measured.ndcg;
        self.coverage += f64::from(u8::from(measured.covered));
        self.latency_ms += latency_ms;
    }
    fn finish(&mut self) {
        let divisor = count(self.queries);
        self.recall_at_5 /= divisor;
        self.mrr /= divisor;
        self.ndcg_at_5 /= divisor;
        self.coverage /= divisor;
        self.latency_ms /= divisor;
    }
}

type Corpus = BTreeMap<Uuid, (usize, &'static corpus::Note)>;

async fn seed_corpus(pool: &sqlx::PgPool, owners: &[OwnerId; 2]) -> Result<Corpus, sqlx::Error> {
    let mut documents = BTreeMap::new();
    for (owner_index, notes) in NOTES.iter().enumerate() {
        for note in notes {
            let id = seed_note(pool, owners[owner_index], note).await?;
            for encoder in [Encoder::Words, Encoder::Trigrams] {
                seed_embedding(pool, owners[owner_index], id, &encoder.encode(note.text)).await?;
            }
            documents.insert(id, (owner_index, note));
        }
    }
    Ok(documents)
}

#[tokio::test]
async fn held_out_authored_text_reports_metrics_by_stratum() -> TestResult {
    let (pg, _guard) = fresh_pg("retrieval_quality").await;
    let owners = [owner(), owner()];
    let documents = seed_corpus(pg.pool_for_tests(), &owners).await?;
    let registry = proxiima_core::SpiiceRegistry::new().freeze_or_panic_for_tests();
    let mut strata = BTreeMap::<String, Aggregate>::new();
    let mut observations = Vec::new();
    for (query_index, query) in QUERIES.iter().enumerate() {
        for encoder in [Encoder::Words, Encoder::Trigrams] {
            for (mode, mode_name) in [
                (SearchMode::Semantic, "semantic"),
                (SearchMode::Hybrid, "hybrid"),
            ] {
                for filtered in [false, true] {
                    let tags = if filtered {
                        vec![query.tag.into()]
                    } else {
                        Vec::new()
                    };
                    let req = request(
                        owners[query.owner],
                        query.text,
                        mode,
                        encoder.encode(query.text),
                        tags,
                    );
                    let relevant: Vec<_> = documents
                        .iter()
                        .filter_map(|(id, (owner_index, note))| {
                            (*owner_index == query.owner
                                && query.relevant.contains(&note.key)
                                && (!filtered || note.tag == query.tag))
                                .then_some(*id)
                        })
                        .collect();
                    let started = Instant::now();
                    let page = pg
                        .search_memories(
                            Some(&owner_scope_for_tests(&[owners[query.owner]])),
                            &req,
                            registry.search_projections(),
                        )
                        .await?;
                    let latency_ms = started.elapsed().as_secs_f64() * 1000.0;
                    let ranked: Vec<_> = page
                        .results
                        .iter()
                        .map(|result| result.memory_id.into_inner())
                        .collect();
                    for id in &ranked {
                        let (owner_index, note) =
                            documents.get(id).expect("returned a fixture memory");
                        assert_eq!(
                            *owner_index, query.owner,
                            "owner admission for query {query_index}"
                        );
                        assert!(
                            !filtered || note.tag == query.tag,
                            "tag admission for query {query_index}"
                        );
                    }
                    let measured = metrics(&relevant, &ranked, K);
                    let space = encoder.space();
                    let filter_name = if filtered {
                        "tag-selective"
                    } else {
                        "unfiltered"
                    };
                    let key = format!(
                        "owner-{}/{}/{}/{mode_name}/{filter_name}",
                        query.owner,
                        space.model_id(),
                        space.dim().width()
                    );
                    strata.entry(key).or_default().add(
                        measured,
                        relevant.len(),
                        ranked.len(),
                        latency_ms,
                    );
                    observations.push(serde_json::json!({"query":query_index,"owner":query.owner,"model":space.model_id(),"dim":space.dim().width(),"mode":mode_name,"filter":filter_name,"judged_relevant":relevant.len(),"retrieved":ranked.len(),"metrics":measured,"latency_ms":latency_ms}));
                }
            }
        }
    }
    for aggregate in strata.values_mut() {
        aggregate.finish();
    }
    println!(
        "RETRIEVAL_BASELINE {}",
        serde_json::json!({
            "fixture":"authored-text-v1", "corpus_documents":documents.len(), "held_out_queries":QUERIES.len(),
            "executed_queries":observations.len(), "k":K, "strata":strata, "observations":observations,
            "encoder":"fixed offline word/trigram feature hashes; no training", "production_quality_claim":false,
        })
    );
    Ok(())
}

#[tokio::test]
async fn pg_canary_reports_multiple_spaces_and_partial_sampling() -> TestResult {
    let (pg, _guard) = fresh_pg("retrieval_canary_strata").await;
    assert!(
        pg.embedding_ann_observability()
            .await?
            .recall_canary
            .is_none()
    );
    for number in 1..=34_u128 {
        let selected = OwnerId::from(UserId::new(Uuid::from_u128(number)));
        let id = seed_note(pg.pool_for_tests(), selected, &NOTES[0][0]).await?;
        seed_embedding(
            pg.pool_for_tests(),
            selected,
            id,
            &Encoder::Words.encode(NOTES[0][0].text),
        )
        .await?;
        if number <= 2 {
            seed_embedding(
                pg.pool_for_tests(),
                selected,
                id,
                &Encoder::Trigrams.encode(NOTES[0][0].text),
            )
            .await?;
        }
    }
    let canary = pg
        .embedding_ann_observability()
        .await?
        .recall_canary
        .expect("nonempty strata");
    assert_eq!(canary.available_strata, 36);
    assert_eq!(
        canary.sample_count, 32,
        "comparison work stops at its declared cap"
    );
    assert_eq!(canary.strata.len(), 2);
    assert_eq!(
        canary
            .strata
            .iter()
            .map(|stratum| stratum.sample_count)
            .sum::<u64>(),
        32
    );
    assert!(canary.strata.iter().any(|stratum| stratum.model_id
        == Encoder::Words.space().model_id()
        && stratum.dim == 384));
    assert!(canary.strata.iter().any(|stratum| stratum.model_id
        == Encoder::Trigrams.space().model_id()
        && stratum.dim == 768));
    Ok(())
}
