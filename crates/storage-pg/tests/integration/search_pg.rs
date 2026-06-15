use crate::common::{drop_db, fresh_pg, owner_fixture};

use proxima_core::llm::EMBEDDING_DIM;
use proxima_core::verbs::query::{EntityKind, MemorySearchRequest, SearchMode};
use proxima_core::verbs::schema::{
    MemorySearchProjection, MemorySearchProjectionField, PayloadKind,
};
use proxima_core::{
    MemoryId, OrgId, Owner, OwnerPrincipalKind, PersonalityInstanceId, Principal, SchemaId,
    SchemaVersion, SearchProjectionColumnKind, SourceBatchId, SourceId, Storage, UserId,
};
use uuid::Uuid;

#[tokio::test]
async fn semantic_search_ranks_nearest_vector_and_isolates_owner()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };
    pg.run_migrations().await?;

    let owner = owner_fixture();
    let other_owner = Owner {
        principal: Principal::User(UserId::new(Uuid::from_u128(7))),
        org_id: OrgId::new(Uuid::nil()),
    };

    let near = insert_embedded_memory(&pg, &owner, "nearest", [0.99, 0.01, 0.0]).await?;
    let far = insert_embedded_memory(&pg, &owner, "orthogonal", [0.0, 1.0, 0.0]).await?;
    let other = insert_embedded_memory(&pg, &other_owner, "other owner", [1.0, 0.0, 0.0]).await?;

    let rows = pg
        .search_memories(
            &MemorySearchRequest {
                principal: owner.principal.clone(),
                query: "semantic query".into(),
                mode: SearchMode::Semantic,
                limit: 10,
                kind: Some(EntityKind::Abstraction),
                schema_id: Some(SchemaId::new("test/search-abstraction-v1".into())),
                query_embedding: Some(padded_embedding([1.0, 0.0, 0.0])),
                embedding_model_id: Some("test-embed".into()),
                reader_personality_instance_id: None,
            },
            &[],
        )
        .await?;

    assert_eq!(
        rows.first().map(|row| row.memory_id.into_inner()),
        Some(near)
    );
    assert!(rows.iter().any(|row| row.memory_id.into_inner() == far));
    assert!(!rows.iter().any(|row| row.memory_id.into_inner() == other));

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn semantic_search_matches_pgvector_cosine_and_clamps_zero_query()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };
    pg.run_migrations().await?;

    let owner = owner_fixture();
    let east_vec = padded_embedding([1.0, 0.0, 0.0]);
    let north_vec = padded_embedding([0.0, 1.0, 0.0]);
    let diagonal_vec = padded_embedding([0.5, 0.5, 0.0]);
    let query_vec = padded_embedding([1.0, 0.2, 0.0]);

    let east = insert_embedded_memory_with_vec(&pg, &owner, "east", &east_vec).await?;
    let north = insert_embedded_memory_with_vec(&pg, &owner, "north", &north_vec).await?;
    let diagonal = insert_embedded_memory_with_vec(&pg, &owner, "diagonal", &diagonal_vec).await?;

    let indexdef: Option<String> = sqlx::query_scalar(
        "SELECT indexdef
           FROM pg_indexes
          WHERE schemaname = 'proxima_core'
            AND tablename = 'embeddings'
            AND indexname = 'idx_embeddings_vec_hnsw'",
    )
    .fetch_optional(pg.pool())
    .await?;
    let indexdef = indexdef.expect("HNSW index exists");
    assert!(indexdef.contains("USING hnsw"), "{indexdef}");

    let rows = pg
        .search_memories(&semantic_request(&owner, query_vec.clone()), &[])
        .await?;

    let mut expected = [
        (east, brute_cosine(&east_vec, &query_vec)),
        (north, brute_cosine(&north_vec, &query_vec)),
        (diagonal, brute_cosine(&diagonal_vec, &query_vec)),
    ];
    expected.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| b.0.cmp(&a.0)));

    let actual: Vec<_> = rows
        .iter()
        .map(|row| (row.memory_id.into_inner(), row.similarity_score))
        .collect();
    assert_eq!(
        actual.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        expected.iter().map(|(id, _)| *id).collect::<Vec<_>>()
    );
    for ((_, actual_score), (_, expected_score)) in actual.iter().zip(expected.iter()) {
        assert!(
            (actual_score - expected_score).abs() <= 1.0e-4,
            "actual {actual_score} expected {expected_score}"
        );
    }

    let zero_rows = pg
        .search_memories(&semantic_request(&owner, vec![0.0; EMBEDDING_DIM]), &[])
        .await?;
    assert!(
        zero_rows
            .iter()
            .all(|row| row.similarity_score.abs() <= f32::EPSILON),
        "{zero_rows:#?}"
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn lexical_search_ignores_unprojected_code_chunk_text()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };
    pg.run_migrations().await?;
    sqlx::query("CREATE SCHEMA IF NOT EXISTS proxima_code")
        .execute(pg.pool())
        .await?;
    sqlx::query(
        "CREATE TABLE proxima_code.code_chunk_v1 (
             memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
             file_path text NOT NULL,
             language text,
             chunk_type text NOT NULL,
             text text NOT NULL
         )",
    )
    .execute(pg.pool())
    .await?;

    let owner = owner_fixture();
    let chunk_id =
        ingest_fact_memory(&pg, &owner, "proxima-code/code-chunk-v1", b"code-chunk").await?;
    let long_token = format!("{}rawonlyneedle", "a".repeat(300));
    sqlx::query(
        "INSERT INTO proxima_code.code_chunk_v1
            (memory_id, file_path, language, chunk_type, text)
         VALUES ($1, 'src/search.rs', 'rust', 'function', $2)",
    )
    .bind(chunk_id.into_inner())
    .bind(long_token)
    .execute(pg.pool())
    .await?;

    let projections = vec![code_chunk_projection()];
    let rows = pg
        .search_memories(&lexical_request(&owner, "rawonlyneedle"), &projections)
        .await?;
    assert!(rows.is_empty(), "{rows:#?}");

    let rows = pg
        .search_memories(&lexical_request(&owner, "src search"), &projections)
        .await?;
    assert_eq!(rows.first().map(|row| row.memory_id), Some(chunk_id));
    assert!(!rows[0].snippet.contains("rawonlyneedle"));

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn search_projects_authoring_personality_and_nil_as_none()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };
    pg.run_migrations().await?;

    let owner = owner_fixture();
    let author = PersonalityInstanceId::new(Uuid::now_v7());
    let authored = insert_text_memory(
        &pg,
        &owner,
        "authored attribution needle",
        Some(author.into_inner()),
    )
    .await?;
    let nil_authored = insert_text_memory(&pg, &owner, "nil attribution needle", None).await?;

    let authored_rows = pg
        .search_memories(
            &MemorySearchRequest {
                principal: owner.principal.clone(),
                query: "authored attribution".into(),
                mode: SearchMode::Lexical,
                limit: 10,
                kind: Some(EntityKind::Abstraction),
                schema_id: Some(SchemaId::new("test/search-attribution-v1".into())),
                query_embedding: None,
                embedding_model_id: None,
                reader_personality_instance_id: None,
            },
            &[],
        )
        .await?;
    let authored_row = authored_rows
        .iter()
        .find(|row| row.memory_id.into_inner() == authored)
        .expect("authored row");
    assert_eq!(authored_row.authoring_personality_instance_id, Some(author));

    let nil_rows = pg
        .search_memories(
            &MemorySearchRequest {
                principal: owner.principal.clone(),
                query: "nil attribution".into(),
                mode: SearchMode::Lexical,
                limit: 10,
                kind: Some(EntityKind::Abstraction),
                schema_id: Some(SchemaId::new("test/search-attribution-v1".into())),
                query_embedding: None,
                embedding_model_id: None,
                reader_personality_instance_id: None,
            },
            &[],
        )
        .await?;
    let nil_row = nil_rows
        .iter()
        .find(|row| row.memory_id.into_inner() == nil_authored)
        .expect("nil-author row");
    assert_eq!(nil_row.authoring_personality_instance_id, None);

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn lexical_search_ignores_sidecar_without_projection()
-> Result<(), Box<dyn std::error::Error>> {
    let Some((pg, db_name)) = fresh_pg().await else {
        return Ok(());
    };
    pg.run_migrations().await?;
    sqlx::query("CREATE SCHEMA IF NOT EXISTS proxima_test")
        .execute(pg.pool())
        .await?;
    sqlx::query(
        "CREATE TABLE proxima_test.unprojected_v1 (
             memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
             secret text NOT NULL
         )",
    )
    .execute(pg.pool())
    .await?;

    let owner = owner_fixture();
    let memory_id =
        ingest_fact_memory(&pg, &owner, "proxima-test/unprojected-v1", b"unprojected").await?;
    sqlx::query("INSERT INTO proxima_test.unprojected_v1 (memory_id, secret) VALUES ($1, $2)")
        .bind(memory_id.into_inner())
        .bind("hidden payload phrase")
        .execute(pg.pool())
        .await?;

    let rows = pg
        .search_memories(&lexical_request(&owner, "hidden payload"), &[])
        .await?;
    assert!(rows.is_empty(), "{rows:#?}");

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

async fn insert_embedded_memory(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    text: &str,
    embedding: [f32; 3],
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let embedding = padded_embedding(embedding);
    insert_embedded_memory_with_vec(pg, owner, text, &embedding).await
}

async fn insert_embedded_memory_with_vec(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    text: &str,
    embedding: &[f32],
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let memory_id = Uuid::now_v7();
    let owner_kind = OwnerPrincipalKind::of(&owner.principal);
    let owner_principal_id = match &owner.principal {
        Principal::User(user) => user.into_inner(),
        Principal::Group(group) => group.into_inner(),
    };
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_principal_kind, owner_principal_id, owner_org_id,
             schema_id, schema_version, kind, text, operator_kind, model_id,
             prompt_version, personality_instance_id, wake_chain_depth)
         VALUES ($1, $2, $3, $4, 'test/search-abstraction-v1', 1,
                 'Abstraction', $5, 'Wake', 'test-model', 'test-v1',
                 '00000000-0000-0000-0000-000000000000'::uuid, 2)",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner.org_id.into_inner())
    .bind(text)
    .execute(pg.pool())
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.embeddings
            (entity_kind, entity_id, embedding_version, model_id, vec,
             owner_principal_kind, owner_principal_id, owner_org_id)
         VALUES ('Abstraction', $1, 1, 'test-embed', $2::vector, $3, $4, $5)",
    )
    .bind(memory_id)
    .bind(vector_literal(embedding))
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner.org_id.into_inner())
    .execute(pg.pool())
    .await?;
    Ok(memory_id)
}

async fn insert_text_memory(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    text: &str,
    personality_instance_id: Option<Uuid>,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let memory_id = Uuid::now_v7();
    let owner_kind = OwnerPrincipalKind::of(&owner.principal);
    let owner_principal_id = match &owner.principal {
        Principal::User(user) => user.into_inner(),
        Principal::Group(group) => group.into_inner(),
    };
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_principal_kind, owner_principal_id, owner_org_id,
             schema_id, schema_version, kind, text, operator_kind, model_id,
             prompt_version, personality_instance_id, wake_chain_depth)
         VALUES ($1, $2, $3, $4, 'test/search-attribution-v1', 1,
                 'Abstraction', $5, 'Wake', 'test-model', 'test-v1',
                 COALESCE($6, '00000000-0000-0000-0000-000000000000'::uuid), 2)",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner.org_id.into_inner())
    .bind(text)
    .bind(personality_instance_id)
    .execute(pg.pool())
    .await?;
    Ok(memory_id)
}

async fn ingest_fact_memory(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    schema_id: &str,
    payload: &[u8],
) -> Result<MemoryId, Box<dyn std::error::Error>> {
    use proxima_core::verbs::event_ingest::{
        Citation, CitationMappingHint, CitedObjectHint, EventDraft,
    };

    let now = time::OffsetDateTime::now_utc();
    let outcome = pg
        .ingest_event_atomic(
            &EventDraft {
                source_id: SourceId::new("test/search"),
                source_batch_id: SourceBatchId::new(Uuid::now_v7()),
                principal: owner.principal.clone(),
                org_id: Some(owner.org_id),
                author_personality_instance_id: None,
                schema_id: SchemaId::new(schema_id.to_string()),
                schema_version: SchemaVersion::new(1),
                payload: payload.to_vec(),
                rendered_text: None,
                observed_at: now,
                occurred_at: now,
                citation: Some(Citation {
                    object: CitedObjectHint {
                        schema_id: SchemaId::new("test/search-object-v1".into()),
                        schema_version: SchemaVersion::new(1),
                        content_hash: *blake3::hash(payload).as_bytes(),
                    },
                    mapping: CitationMappingHint {
                        schema_id: SchemaId::new("test/search-whole-v1".into()),
                        schema_version: SchemaVersion::new(1),
                    },
                }),
            },
            None,
        )
        .await?;
    Ok(outcome.memory_id)
}

fn lexical_request(owner: &Owner, query: &str) -> MemorySearchRequest {
    MemorySearchRequest {
        principal: owner.principal.clone(),
        query: query.into(),
        mode: SearchMode::Lexical,
        limit: 10,
        kind: Some(EntityKind::Fact),
        schema_id: None,
        query_embedding: None,
        embedding_model_id: None,
        reader_personality_instance_id: None,
    }
}

fn semantic_request(owner: &Owner, query_embedding: Vec<f32>) -> MemorySearchRequest {
    MemorySearchRequest {
        principal: owner.principal.clone(),
        query: "semantic query".into(),
        mode: SearchMode::Semantic,
        limit: 10,
        kind: Some(EntityKind::Abstraction),
        schema_id: Some(SchemaId::new("test/search-abstraction-v1".into())),
        query_embedding: Some(query_embedding),
        embedding_model_id: Some("test-embed".into()),
        reader_personality_instance_id: None,
    }
}

fn padded_embedding(prefix: [f32; 3]) -> Vec<f32> {
    let mut embedding = vec![0.0; EMBEDDING_DIM];
    embedding[..prefix.len()].copy_from_slice(&prefix);
    embedding
}

fn vector_literal(vec: &[f32]) -> String {
    let mut out = String::with_capacity(vec.len().saturating_mul(8).saturating_add(2));
    out.push('[');
    for (idx, value) in vec.iter().enumerate() {
        if idx > 0 {
            out.push(',');
        }
        out.push_str(&value.to_string());
    }
    out.push(']');
    out
}

fn brute_cosine(stored: &[f32], query: &[f32]) -> f32 {
    let dot: f32 = stored
        .iter()
        .zip(query.iter())
        .map(|(stored, query)| stored * query)
        .sum();
    let stored_norm = stored.iter().map(|value| value * value).sum::<f32>().sqrt();
    let query_norm = query.iter().map(|value| value * value).sum::<f32>().sqrt();
    if stored_norm <= f32::EPSILON || query_norm <= f32::EPSILON {
        0.0
    } else {
        (dot / (stored_norm * query_norm)).max(0.0)
    }
}

fn code_chunk_projection() -> MemorySearchProjection {
    MemorySearchProjection {
        schema_id: SchemaId::new("proxima-code/code-chunk-v1".into()),
        schema_version: SchemaVersion::new(1),
        kind: PayloadKind::Fact,
        sidecar_table: "proxima_code.code_chunk_v1".into(),
        fields: vec![
            MemorySearchProjectionField {
                column: "file_path".into(),
                kind: SearchProjectionColumnKind::Text,
            },
            MemorySearchProjectionField {
                column: "language".into(),
                kind: SearchProjectionColumnKind::Text,
            },
            MemorySearchProjectionField {
                column: "chunk_type".into(),
                kind: SearchProjectionColumnKind::Text,
            },
        ],
    }
}
