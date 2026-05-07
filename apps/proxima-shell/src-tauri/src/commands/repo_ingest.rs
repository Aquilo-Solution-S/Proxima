use std::sync::Arc;

use proxima_core::{Engine, Owner};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

use super::ts_types::{IndexReportTs, IngestProgressTs};

#[allow(clippy::too_many_lines)]
pub(super) fn spawn_run_driver(
    engine: Arc<Engine>,
    pg: Arc<PgStorage>,
    hub: crate::repo_ingest_hub::RepoIngestHub,
    owner: Owner,
    record: proxima_code::RepoRecord,
    run_id: Uuid,
) {
    tokio::spawn(async move {
        let drive = async {
            let Some(run) = proxima_code::begin_run(pg.pool(), run_id)
                .await
                .map_err(|e| e.to_string())?
            else {
                return Ok::<(), String>(());
            };
            hub.publish_snapshot(owner.clone(), run).await;

            let cursor = match record.last_cursor.clone() {
                Some(b) => proxima_core::Cursor::from_bytes(b),
                None => proxima_core::Cursor::empty(),
            };
            let source = proxima_code::LocalGitSource::new(
                record.repo_id,
                std::path::PathBuf::from(record.canonical_path.clone()),
                owner.clone(),
            );

            let owner_for_progress = owner.clone();
            let hub_for_progress = hub.clone();
            let repo_id = record.repo_id;
            let mut sink = move |p: proxima_code::IngestProgress| {
                let owner = owner_for_progress.clone();
                let hub = hub_for_progress.clone();
                tokio::spawn(async move {
                    hub.publish_progress(owner, repo_id, IngestProgressTs::from(p))
                        .await;
                });
            };

            let (report, new_cursor) = source
                .run_poll(pg.pool(), &cursor, &mut sink)
                .await
                .map_err(|e| e.to_string())?;

            let mut counters = proxima_code::StageCounters::zeroed();
            counters.commits_emitted = u32::try_from(report.commits_emitted).unwrap_or(u32::MAX);
            counters.files_emitted =
                u32::try_from(report.files_present_emitted).unwrap_or(u32::MAX);
            counters.chunks_emitted = u32::try_from(report.chunks_emitted).unwrap_or(u32::MAX);
            counters.chunks_reused = u32::try_from(report.chunks_reused).unwrap_or(u32::MAX);
            counters.chunks_tombstoned =
                u32::try_from(report.chunks_tombstoned).unwrap_or(u32::MAX);

            let run = proxima_code::advance_stage(
                pg.pool(),
                run_id,
                proxima_code::RunStage::AstEdges,
                &counters,
            )
            .await
            .map_err(|e| e.to_string())?;
            hub.publish_snapshot(owner.clone(), run).await;

            counters.ast_edges_emitted = count_ast_edges_for_run(pg.pool(), &owner, record.repo_id)
                .await
                .map_err(|e| e.to_string())?;
            let run = proxima_code::advance_stage(
                pg.pool(),
                run_id,
                proxima_code::RunStage::F2a,
                &counters,
            )
            .await
            .map_err(|e| e.to_string())?;
            hub.publish_snapshot(owner.clone(), run).await;

            engine
                .run_dispatcher_tick()
                .await
                .map_err(|e| explain_driver_error("dispatcher", &e.to_string()))?;
            counters.abstractions_emitted =
                count_abstractions_for_run(pg.pool(), &owner, record.repo_id)
                    .await
                    .map_err(|e| e.to_string())?;
            let run = proxima_code::advance_stage(
                pg.pool(),
                run_id,
                proxima_code::RunStage::Embeddings,
                &counters,
            )
            .await
            .map_err(|e| e.to_string())?;
            hub.publish_snapshot(owner.clone(), run).await;

            counters.embeddings_landed = wait_for_embeddings(
                pg.pool(),
                &owner,
                record.repo_id,
                counters.abstractions_emitted,
                std::time::Duration::from_mins(1),
            )
            .await?;
            counters.citations_emitted = count_citations_for_run(pg.pool(), &owner, record.repo_id)
                .await
                .map_err(|e| e.to_string())?;

            proxima_code::update_cursor(
                pg.pool(),
                &owner,
                record.repo_id,
                new_cursor.as_bytes(),
                time::OffsetDateTime::now_utc(),
            )
            .await
            .map_err(|e| e.to_string())?;

            let run = proxima_code::mark_succeeded(pg.pool(), run_id, &counters)
                .await
                .map_err(|e| e.to_string())?;
            hub.publish_snapshot(owner.clone(), run).await;
            hub.publish_done(owner.clone(), record.repo_id, IndexReportTs::from(report))
                .await;
            Ok::<(), String>(())
        };

        if let Err(message) = drive.await {
            tracing::warn!("repo_ingest run {run_id} failed: {message}");
            if let Ok(run) = proxima_code::mark_failed(pg.pool(), run_id, &message).await {
                hub.publish_snapshot(owner.clone(), run).await;
            }
            hub.publish_error(owner, record.repo_id, message).await;
        }
    });
}

fn explain_driver_error(stage: &str, message: &str) -> String {
    if message.contains("HTTP send") && message.contains("timed out") {
        return format!(
            "{stage}: model request timed out. The model endpoint is reachable, \
             but the selected model did not respond before Proxima's timeout. \
             Use a faster model in Settings -> Models or retry after the model \
             is warm."
        );
    }
    if message.contains("localhost:11434") && message.contains("HTTP send") {
        return format!(
            "{stage}: Ollama is not reachable at http://localhost:11434. \
             Start Ollama or update Settings -> Models to a reachable \
             OpenAI-compatible endpoint, then run ingest again."
        );
    }
    if message.contains("chat/completions") && message.contains("HTTP send") {
        return format!(
            "{stage}: LLM endpoint is not reachable. Check Settings -> Models \
             base URL and network access, then run ingest again."
        );
    }
    if message.contains("/embeddings") && message.contains("HTTP send") {
        return format!(
            "{stage}: embedding endpoint is not reachable. Check Settings -> \
             Models embedding configuration, then run ingest again."
        );
    }
    format!("{stage}: {message}")
}

async fn count_ast_edges_for_run(
    pool: &sqlx::PgPool,
    owner: &Owner,
    repo_id: Uuid,
) -> Result<u32, sqlx::Error> {
    let (kind, principal_id, org_id) = proxima_code::repos::owner_columns_pub(owner);
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint \
         FROM proxima_core.edges e \
         JOIN proxima_code.code_calls_v1 s ON s.edge_id = e.edge_id \
         JOIN proxima_code.code_chunk_v1 src ON src.memory_id = e.source_memory_id \
         JOIN proxima_code.code_chunk_v1 tgt ON tgt.memory_id = e.target_memory_id \
         WHERE e.owner_principal_kind = $1 AND e.owner_principal_id = $2 \
           AND e.owner_org_id = $3 AND src.repo_id = $4 AND tgt.repo_id = $4",
    )
    .bind(kind)
    .bind(principal_id)
    .bind(org_id)
    .bind(repo_id)
    .fetch_one(pool)
    .await?;
    Ok(u32::try_from(n).unwrap_or(u32::MAX))
}

async fn count_abstractions_for_run(
    pool: &sqlx::PgPool,
    owner: &Owner,
    repo_id: Uuid,
) -> Result<u32, sqlx::Error> {
    let (kind, principal_id, org_id) = proxima_code::repos::owner_columns_pub(owner);
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint \
         FROM proxima_code.commit_summary_v1 cs \
         JOIN proxima_core.memories m ON m.memory_id = cs.memory_id \
         WHERE m.owner_principal_kind = $1 AND m.owner_principal_id = $2 \
           AND m.owner_org_id = $3 AND cs.repo_id = $4",
    )
    .bind(kind)
    .bind(principal_id)
    .bind(org_id)
    .bind(repo_id)
    .fetch_one(pool)
    .await?;
    Ok(u32::try_from(n).unwrap_or(u32::MAX))
}

async fn count_citations_for_run(
    pool: &sqlx::PgPool,
    owner: &Owner,
    repo_id: Uuid,
) -> Result<u32, sqlx::Error> {
    let (kind, principal_id, org_id) = proxima_code::repos::owner_columns_pub(owner);
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint \
         FROM proxima_core.citation_mappings cm \
         JOIN proxima_core.memories m ON m.memory_id = cm.memory_id \
         WHERE m.owner_principal_kind = $1 AND m.owner_principal_id = $2 \
           AND m.owner_org_id = $3 AND ( \
             cm.memory_id IN (SELECT memory_id FROM proxima_code.commit_v1 WHERE repo_id = $4) OR \
             cm.memory_id IN (SELECT memory_id FROM proxima_code.file_revision_v1 WHERE repo_id = $4) OR \
             cm.memory_id IN (SELECT memory_id FROM proxima_code.code_chunk_v1 WHERE repo_id = $4))",
    )
    .bind(kind)
    .bind(principal_id)
    .bind(org_id)
    .bind(repo_id)
    .fetch_one(pool)
    .await?;
    Ok(u32::try_from(n).unwrap_or(u32::MAX))
}

async fn wait_for_embeddings(
    pool: &sqlx::PgPool,
    owner: &Owner,
    repo_id: Uuid,
    expected: u32,
    timeout: std::time::Duration,
) -> Result<u32, String> {
    if expected == 0 {
        return Ok(0);
    }
    let (kind, principal_id, org_id) = proxima_code::repos::owner_columns_pub(owner);
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let n: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint \
             FROM proxima_core.embeddings e \
             JOIN proxima_code.commit_summary_v1 cs ON cs.memory_id = e.entity_id \
             JOIN proxima_core.memories m ON m.memory_id = cs.memory_id \
             WHERE m.owner_principal_kind = $1 AND m.owner_principal_id = $2 \
               AND m.owner_org_id = $3 AND cs.repo_id = $4",
        )
        .bind(kind)
        .bind(principal_id)
        .bind(org_id)
        .bind(repo_id)
        .fetch_one(pool)
        .await
        .map_err(|e| e.to_string())?;
        let landed = u32::try_from(n).unwrap_or(u32::MAX);
        if landed >= expected {
            return Ok(landed);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "embeddings_timeout: expected={expected} got={landed}"
            ));
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn explain_driver_error_names_unreachable_ollama() {
        let raw = "Internal: operator: LLM call failed: HTTP send: error sending request                    for url (http://localhost:11434/v1/chat/completions)";
        let msg = super::explain_driver_error("f2a", raw);
        assert!(msg.contains("Ollama is not reachable"));
        assert!(msg.contains("run ingest again"));
    }

    #[test]
    fn explain_driver_error_names_model_timeout() {
        let raw = "Internal: operator: LLM call failed: HTTP send: operation timed out";
        let msg = super::explain_driver_error("f2a", raw);
        assert!(msg.contains("model request timed out"));
        assert!(!msg.contains("not reachable"));
    }
}
