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
    max_commits: Option<usize>,
) {
    tokio::spawn(async move {
        tracing::info!(
            %run_id,
            repo_id = %record.repo_id,
            repo_path = %record.canonical_path,
            max_commits = max_commits.map_or("all".to_string(), |n| n.to_string()),
            "ingest run starting"
        );
        let drive = async {
            let Some(run) = proxima_code::begin_run(pg.pool(), run_id)
                .await
                .map_err(|e| e.to_string())?
            else {
                tracing::info!(%run_id, "ingest run dropped: begin_run returned None");
                return Ok::<(), String>(());
            };
            tracing::info!(%run_id, "ingest stage: facts");
            hub.publish_snapshot(owner.clone(), run).await;
            let baseline = count_repo_ingest_totals(pg.pool(), &owner, record.repo_id)
                .await
                .map_err(|e| e.to_string())?;

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
                .run_poll_limited(pg.pool(), &cursor, max_commits, &mut sink)
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

            tracing::info!(
                %run_id,
                commits = counters.commits_emitted,
                files = counters.files_emitted,
                chunks = counters.chunks_emitted,
                chunks_reused = counters.chunks_reused,
                chunks_tombstoned = counters.chunks_tombstoned,
                "ingest stage: ast_edges"
            );
            let run = proxima_code::advance_stage(
                pg.pool(),
                run_id,
                proxima_code::RunStage::AstEdges,
                &counters,
            )
            .await
            .map_err(|e| e.to_string())?;
            hub.publish_snapshot(owner.clone(), run).await;

            let after_facts = count_repo_ingest_totals(pg.pool(), &owner, record.repo_id)
                .await
                .map_err(|e| e.to_string())?;
            counters.ast_edges_emitted = after_facts.delta_since(baseline).ast_edges;
            tracing::info!(
                %run_id,
                ast_edges = counters.ast_edges_emitted,
                "ingest stage: f2a (dispatcher tick)"
            );
            let run = proxima_code::advance_stage(
                pg.pool(),
                run_id,
                proxima_code::RunStage::F2a,
                &counters,
            )
            .await
            .map_err(|e| e.to_string())?;
            hub.publish_snapshot(owner.clone(), run).await;

            let fired = engine
                .run_dispatcher_tick()
                .await
                .map_err(|e| explain_driver_error("dispatcher", &e.to_string()))?;
            tracing::info!(%run_id, wakes_fired = fired, "dispatcher tick complete");
            let after_f2a = count_repo_ingest_totals(pg.pool(), &owner, record.repo_id)
                .await
                .map_err(|e| e.to_string())?;
            counters.abstractions_emitted = after_f2a.delta_since(baseline).abstractions;
            tracing::info!(
                %run_id,
                abstractions = counters.abstractions_emitted,
                "ingest stage: embeddings"
            );
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
                baseline.embeddings,
                counters.abstractions_emitted,
                std::time::Duration::from_mins(1),
            )
            .await?;
            let final_totals = count_repo_ingest_totals(pg.pool(), &owner, record.repo_id)
                .await
                .map_err(|e| e.to_string())?;
            counters.citations_emitted = final_totals.delta_since(baseline).citations;
            tracing::info!(
                %run_id,
                commits = counters.commits_emitted,
                chunks = counters.chunks_emitted,
                ast_edges = counters.ast_edges_emitted,
                abstractions = counters.abstractions_emitted,
                embeddings = counters.embeddings_landed,
                citations = counters.citations_emitted,
                "ingest run succeeded"
            );

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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct RepoIngestTotals {
    ast_edges: u32,
    abstractions: u32,
    embeddings: u32,
    citations: u32,
}

impl RepoIngestTotals {
    fn delta_since(self, baseline: Self) -> Self {
        Self {
            ast_edges: self.ast_edges.saturating_sub(baseline.ast_edges),
            abstractions: self.abstractions.saturating_sub(baseline.abstractions),
            embeddings: self.embeddings.saturating_sub(baseline.embeddings),
            citations: self.citations.saturating_sub(baseline.citations),
        }
    }
}

async fn count_repo_ingest_totals(
    pool: &sqlx::PgPool,
    owner: &Owner,
    repo_id: Uuid,
) -> Result<RepoIngestTotals, sqlx::Error> {
    let (kind, principal_id, org_id) = proxima_code::repos::owner_columns_pub(owner);
    let ast_edges = sqlx::query_scalar::<_, i64>(
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

    let abstractions = sqlx::query_scalar::<_, i64>(
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

    let embeddings = sqlx::query_scalar::<_, i64>(
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
    .await?;

    let citations = sqlx::query_scalar::<_, i64>(
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

    Ok(RepoIngestTotals {
        ast_edges: u32::try_from(ast_edges).unwrap_or(u32::MAX),
        abstractions: u32::try_from(abstractions).unwrap_or(u32::MAX),
        embeddings: u32::try_from(embeddings).unwrap_or(u32::MAX),
        citations: u32::try_from(citations).unwrap_or(u32::MAX),
    })
}

async fn wait_for_embeddings(
    pool: &sqlx::PgPool,
    owner: &Owner,
    repo_id: Uuid,
    baseline_embeddings: u32,
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
        let total_landed = u32::try_from(n).unwrap_or(u32::MAX);
        let landed = total_landed.saturating_sub(baseline_embeddings);
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
    use super::RepoIngestTotals;

    #[test]
    fn repo_ingest_totals_delta_reports_current_run_only() {
        let baseline = RepoIngestTotals {
            ast_edges: 7_072,
            abstractions: 118,
            embeddings: 118,
            citations: 11_122,
        };
        let after = RepoIngestTotals {
            ast_edges: 7_108,
            abstractions: 119,
            embeddings: 119,
            citations: 11_149,
        };

        assert_eq!(
            after.delta_since(baseline),
            RepoIngestTotals {
                ast_edges: 36,
                abstractions: 1,
                embeddings: 1,
                citations: 27,
            },
        );
    }

    #[test]
    fn repo_ingest_totals_delta_saturates_if_totals_drop() {
        let baseline = RepoIngestTotals {
            ast_edges: 10,
            abstractions: 10,
            embeddings: 10,
            citations: 10,
        };
        let after = RepoIngestTotals {
            ast_edges: 9,
            abstractions: 8,
            embeddings: 7,
            citations: 6,
        };

        assert_eq!(after.delta_since(baseline), RepoIngestTotals::default());
    }

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
