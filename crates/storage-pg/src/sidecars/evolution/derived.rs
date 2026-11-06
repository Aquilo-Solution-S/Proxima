use std::collections::HashMap;

use proxiima_core::StorageError;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use super::{MigrationInput, MigrationPlan, PgSchemaEvolution};
use crate::error::map_err;

/// Existing embedding spaces follow the explicit mapping, but vectors never
/// survive a payload rewrite. Resetting jobs also invalidates in-flight claims.
pub(super) async fn rebuild(
    tx: &mut Transaction<'_, Postgres>,
    input: &MigrationInput,
    plan: &MigrationPlan,
    program: &PgSchemaEvolution,
) -> Result<(), StorageError> {
    let sources: Vec<_> = input
        .memories
        .iter()
        .map(|m| m.id().into_inner())
        .chain(input.goals.iter().map(|g| g.id().into_inner()))
        .collect();
    let spaces: Vec<(Uuid, String, i16)> = sqlx::query_as(
        "SELECT entity_id,model_id,dim FROM proxiima_core.embedding_heads WHERE entity_id=ANY($1::uuid[]) AND owner_id=$2
         UNION SELECT entity_id,model_id,dim FROM proxiima_core.embedding_jobs WHERE entity_id=ANY($1::uuid[]) AND owner_id=$2")
        .bind(&sources).bind(input.owner.into_inner()).fetch_all(&mut **tx).await.map_err(map_err)?;
    let mut target_spaces = HashMap::<Uuid, Vec<proxiima_core::llm::EmbeddingSpace>>::new();
    for (source, model, dim) in spaces {
        let space = crate::pgvector::stored_space(model, dim)?;
        for target in plan.mappings[&source].targets() {
            target_spaces.entry(target).or_default().push(space.clone());
        }
    }
    sqlx::query(
        "DELETE FROM proxiima_core.embedding_jobs WHERE entity_id=ANY($1::uuid[]) AND owner_id=$2",
    )
    .bind(&sources)
    .bind(input.owner.into_inner())
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    sqlx::query(
        "DELETE FROM proxiima_core.embedding_heads WHERE entity_id=ANY($1::uuid[]) AND owner_id=$2",
    )
    .bind(&sources)
    .bind(input.owner.into_inner())
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    sqlx::query(
        "DELETE FROM proxiima_core.embeddings WHERE entity_id=ANY($1::uuid[]) AND owner_id=$2",
    )
    .bind(&sources)
    .bind(input.owner.into_inner())
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;
    for (target, spaces) in target_spaces {
        let payload = plan
            .memories
            .iter()
            .find(|m| m.id == target)
            .map(|m| &m.payload)
            .or_else(|| {
                plan.goals
                    .iter()
                    .find(|g| g.id == target)
                    .and_then(|g| g.payload.sidecar_payload.as_ref())
            });
        let embeddable = payload.is_some_and(|payload| {
            program.declaration.targets.iter().any(|schema| {
                schema.kind == payload.kind
                    && schema.schema_id() == payload.schema_id
                    && matches!(
                        schema.embedding,
                        proxiima_core::spiice::EmbeddingRecipe::Units(_)
                    )
            })
        });
        if !embeddable {
            continue;
        }
        if plan
            .memories
            .iter()
            .any(|m| m.id == target && m.source.row.forgotten_at.is_some())
        {
            continue;
        }
        crate::verbs::fact_embeddings::enqueue_embedding_jobs_in_tx(
            tx,
            input.owner.into_inner(),
            target,
            &spaces,
        )
        .await?;
    }
    // External provenance is a captured witness. Every declared successor
    // retains the contributor's witness, without turning it into a live pin.
    for source in &input.memories {
        for target in plan.mappings[&source.id().into_inner()].targets() {
            sqlx::query("INSERT INTO proxiima_core.memory_external_origin(t,memory,edge_kind)
                SELECT $1,memory,edge_kind FROM proxiima_core.memory_external_origin WHERE t=$2 ON CONFLICT DO NOTHING")
                .bind(target).bind(source.id().into_inner()).execute(&mut **tx).await.map_err(map_err)?;
        }
    }
    Ok(())
}
