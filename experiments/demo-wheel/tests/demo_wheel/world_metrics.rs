use super::*;

impl DemoWorld {
    pub(super) async fn correction_loop_count(&self) -> Result<u32, sqlx::Error> {
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*)
             FROM proxima_code.execution_request_v1
             WHERE request_key LIKE 'demo-signal-match-correction-%'",
        )
        .fetch_one(self.pg.pool())
        .await?;
        Ok(u32::try_from(count).unwrap_or(u32::MAX))
    }

    pub(super) async fn goal_achieved_fact_exists(&self) -> Result<bool, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT EXISTS(
                SELECT 1 FROM proxima_goal.goal_achieved_v1 WHERE title = $1
             )",
        )
        .bind(self.cfg.challenge.goal_title())
        .fetch_one(self.pg.pool())
        .await
    }

    pub(super) async fn demo_goal_graph_complete(&self) -> Result<bool, sqlx::Error> {
        let parent_achieved = self.goal_achieved_fact_exists().await?;
        let graph = self.goal_graph_metrics().await?;
        Ok(graph.complete(parent_achieved, self.cfg.required_child_goal_count()))
    }

    pub(super) async fn goal_graph_metrics(&self) -> Result<GoalGraphMetrics, sqlx::Error> {
        let Some(parent_goal) = self.goal_id else {
            return Ok(GoalGraphMetrics::default());
        };
        let row = sqlx::query(
            "WITH RECURSIVE child_roots AS (
                 SELECT gp.goal_id AS root_goal_id
                   FROM proxima_core.goal_parents gp
                  WHERE gp.parent_goal_id = $1
             ),
             child_lineage(root_goal_id, goal_id, depth, path) AS (
                 SELECT root_goal_id, root_goal_id, 0, ARRAY[root_goal_id]
                   FROM child_roots
                 UNION ALL
                 SELECT l.root_goal_id, child.goal_id, l.depth + 1, l.path || child.goal_id
                   FROM child_lineage l
                   JOIN proxima_core.goals child
                     ON child.supersedes = l.goal_id
                  WHERE NOT child.goal_id = ANY(l.path)
             ),
             child_heads AS (
                 SELECT DISTINCT ON (l.root_goal_id)
                        l.root_goal_id,
                        g.goal_id AS head_goal_id,
                        g.state
                   FROM child_lineage l
                   JOIN proxima_core.goals g ON g.goal_id = l.goal_id
                  ORDER BY l.root_goal_id, l.depth DESC, g.created_at DESC
             ),
             child_activations AS (
                 SELECT ga.memory_id, ga.goal_id
                   FROM proxima_goal.goal_activated_v1 ga
                   JOIN child_roots cr ON cr.root_goal_id = ga.goal_id
             ),
             child_requests AS (
                 SELECT DISTINCT er.memory_id
                   FROM proxima_code.execution_request_v1 er
                   JOIN proxima_core.edges e
                     ON e.source_kind = 'Fact'
                    AND e.source_memory_id = er.memory_id
                    AND e.target_kind = 'Fact'
                    AND e.target_memory_id IN (SELECT memory_id FROM child_activations)
                    AND e.relation = 'core/derived-from'
             ),
             child_runs AS (
                 SELECT DISTINCT wr.memory_id
                   FROM proxima_code.workspace_run_v1 wr
                   JOIN proxima_core.edges e
                     ON e.source_kind = 'Fact'
                    AND e.source_memory_id = wr.memory_id
                    AND e.target_kind = 'Fact'
                    AND e.target_memory_id IN (SELECT memory_id FROM child_requests)
                    AND e.relation = 'core/derived-from'
             )
             SELECT
                (SELECT count(*) FROM child_roots) AS child_goal_count,
                (SELECT count(*) FROM child_heads WHERE state = 'Achieved') AS achieved_child_goal_count,
                (SELECT count(*) FROM child_requests) AS child_execution_request_count,
                (SELECT count(*) FROM child_runs) AS child_workspace_run_count,
                (SELECT count(*) FROM proxima_code.workspace_review_v1
                  WHERE execution_request_memory_id IN (SELECT memory_id FROM child_requests)) AS child_workspace_review_count,
                (SELECT count(*) FROM proxima_code.verification_evidence_v1
                  WHERE execution_request_memory_id IN (SELECT memory_id FROM child_requests)) AS verification_evidence_count",
        )
        .bind(parent_goal.into_inner())
        .fetch_one(self.pg.pool())
        .await?;
        Ok(GoalGraphMetrics {
            child_goal_count: row.try_get("child_goal_count")?,
            achieved_child_goal_count: row.try_get("achieved_child_goal_count")?,
            child_execution_request_count: row.try_get("child_execution_request_count")?,
            child_workspace_run_count: row.try_get("child_workspace_run_count")?,
            child_workspace_review_count: row.try_get("child_workspace_review_count")?,
            verification_evidence_count: row.try_get("verification_evidence_count")?,
        })
    }

    pub(super) async fn collect_metrics(
        &self,
        started: Instant,
        ticks: u32,
    ) -> Result<Metrics, Box<dyn std::error::Error>> {
        let wake_invocations = self.wake_invocation_metrics().await?;
        let wake_invocation_count_by_role =
            wake_invocations
                .iter()
                .fold(BTreeMap::<String, u32>::new(), |mut acc, row| {
                    *acc.entry(row.role.clone()).or_default() += 1;
                    acc
                });
        let output_sidecar_counts_by_schema = self.output_sidecar_counts().await?;
        let vision_brief_count = *output_sidecar_counts_by_schema
            .get(VisionBriefV1::SCHEMA_ID)
            .unwrap_or(&0);
        let review_verdicts = self.review_verdicts().await?;
        let request_flow_counts = self.request_flow_counts().await?;
        let workspace_run_count = *output_sidecar_counts_by_schema
            .get(WorkspaceRunV1::SCHEMA_ID)
            .unwrap_or(&0);
        let goal_achieved_fact_exists = self.goal_achieved_fact_exists().await?;
        let goal_graph = self.goal_graph_metrics().await?;
        let final_goal_state = self.final_goal_state().await?;
        let (git_diff_stats, final_changed_files) = self.git_diff_metrics().await?;
        let deterministic_checks = deterministic_checks(
            self.cfg.challenge,
            self.cfg.required_child_goal_count(),
            goal_achieved_fact_exists,
            &goal_graph,
            vision_brief_count,
            &git_diff_stats,
            &final_changed_files,
        );
        let deterministic_pass = deterministic_checks.values().all(|value| *value);
        let forced_continuation_checks = self.forced_continuation_checks().await?;
        let forced_continuation_pass = forced_continuation_checks.values().all(|value| *value);
        let real_planner_checks = self.real_planner_checks().await?;
        let real_planner_pass = real_planner_checks.values().all(|value| *value);
        let intervention_decision_count = *output_sidecar_counts_by_schema
            .get(InterventionDecisionV1::SCHEMA_ID)
            .unwrap_or(&0);
        let wake_failures = wake_invocations.iter().any(|row| {
            if row.status == "succeeded" || row.status == "skipped" {
                return false;
            }
            row.status != "truncated"
                || row.failure_reason.as_deref() != Some("max_rounds_reached")
                || intervention_decision_count == 0
        });
        let correction_loop_count = self.correction_loop_count().await?;
        let intervention_pass = ticks <= self.cfg.max_ticks
            && correction_loop_count <= self.cfg.max_correction_loops
            && !wake_failures;
        let (reviewer_score, reviewer_score_error) =
            if self.cfg.intervention_mode == DemoInterventionMode::ForceContinue {
                (None, None)
            } else {
                match self
                    .run_read_only_reviewer(&git_diff_stats, &final_changed_files)
                    .await
                {
                    Ok(score) => (Some(score), None),
                    Err(err) => {
                        eprintln!("read-only reviewer failed: {err}");
                        (None, Some(err.to_string()))
                    }
                }
            };
        let functional_pass = match self.cfg.intervention_mode {
            DemoInterventionMode::Normal => {
                let reviewer_raw = reviewer_score
                    .as_ref()
                    .map_or(0, |score| score.score.min(100));
                deterministic_pass
                    && reviewer_raw >= 70
                    && (self.cfg.planner_mode == DemoPlannerMode::Scripted || real_planner_pass)
            }
            DemoInterventionMode::ForceContinue => forced_continuation_pass,
        };
        let reviewer_raw = reviewer_score
            .as_ref()
            .map_or(0, |score| score.score.min(100));
        let overall_score = match self.cfg.intervention_mode {
            DemoInterventionMode::Normal => {
                if deterministic_pass {
                    reviewer_raw
                } else {
                    reviewer_raw.min(49)
                }
            }
            DemoInterventionMode::ForceContinue => u32::from(forced_continuation_pass) * 100,
        };
        let total_model_rounds: u32 = wake_invocations
            .iter()
            .filter_map(|row| row.rounds_or_turns)
            .filter_map(|value| u32::try_from(value).ok())
            .sum();
        let wall_clock_seconds = started.elapsed().as_secs_f64();
        let wall_clock_timed_out = self
            .cfg
            .max_wall_clock_seconds
            .is_some_and(|max| wall_clock_seconds >= max as f64);
        let overall_pass = match self.cfg.intervention_mode {
            DemoInterventionMode::Normal => {
                functional_pass && intervention_pass && !wall_clock_timed_out
            }
            DemoInterventionMode::ForceContinue => forced_continuation_pass && intervention_pass,
        };
        Ok(Metrics {
            intervention_mode: self.cfg.intervention_mode,
            planner_mode: self.cfg.planner_mode,
            run_dir: self.cfg.run_dir.display().to_string(),
            repo_path: self.cfg.repo_path.display().to_string(),
            db_name: self.db_name.clone(),
            max_ticks: self.cfg.max_ticks,
            max_wall_clock_seconds: self.cfg.max_wall_clock_seconds,
            max_correction_loops: self.cfg.max_correction_loops,
            role_max_rounds: self.cfg.role_max_rounds,
            dispatcher_tick_count: ticks,
            wake_invocation_count_by_role,
            terminal_guard_hits: terminal_guard_hits(&wake_invocations),
            wake_invocations,
            correction_loop_count,
            output_sidecar_counts_by_schema,
            workspace_run_count,
            request_flow_counts,
            review_verdicts,
            final_goal_state,
            goal_achieved_fact_exists,
            goal_graph,
            git_diff_stats,
            final_changed_files,
            deterministic_checks,
            deterministic_pass,
            forced_continuation_checks,
            forced_continuation_pass,
            real_planner_checks,
            real_planner_pass,
            functional_pass,
            flow_graph_json: self
                .cfg
                .run_dir
                .join("flow_graph.json")
                .display()
                .to_string(),
            flow_graph_mermaid: self
                .cfg
                .run_dir
                .join("flow_graph.mmd")
                .display()
                .to_string(),
            flow_graph_summary: self.collect_flow_graph().await?.summary,
            conversation_index_json: self
                .cfg
                .run_dir
                .join("conversations/index.json")
                .display()
                .to_string(),
            conversation_invocation_count: 0,
            conversation_missing_log_count: 0,
            reviewer_score,
            reviewer_score_error,
            auto_merge: None,
            overall_score,
            total_model_rounds,
            wall_clock_seconds,
            wall_clock_timed_out,
            score_per_model_round: if total_model_rounds == 0 {
                None
            } else {
                Some(f64::from(overall_score) / f64::from(total_model_rounds))
            },
            score_per_wall_clock_second: f64::from(overall_score) / wall_clock_seconds.max(0.001),
            intervention_pass,
            overall_pass,
        })
    }

    pub(super) async fn forced_continuation_checks(
        &self,
    ) -> Result<BTreeMap<String, bool>, Box<dyn std::error::Error>> {
        let continue_decision_count: i64 = sqlx::query_scalar(
            "SELECT count(*)
             FROM proxima_core.intervention_decision_v1
             WHERE decision = 'continue'",
        )
        .fetch_one(self.pg.pool())
        .await?;
        let decision_request_edge_count: i64 = sqlx::query_scalar(
            "SELECT count(*)
             FROM proxima_core.intervention_decision_v1 d
             JOIN proxima_core.edges e
               ON e.source_kind = 'Fact'
              AND e.target_kind = 'Fact'
              AND e.source_memory_id = d.memory_id
              AND e.target_memory_id = d.intervention_request_memory_id
              AND e.relation = $1
             WHERE d.decision = 'continue'",
        )
        .bind(CORE_DERIVED_FROM_RELATION)
        .fetch_one(self.pg.pool())
        .await?;
        let continuation_invocation_count: i64 = sqlx::query_scalar(
            "SELECT count(*)
             FROM proxima_core.personality_wake_invocations
             WHERE continuation_intervention_decision_memory_id IS NOT NULL",
        )
        .fetch_one(self.pg.pool())
        .await?;
        let continuation_row_links_decision_count: i64 = sqlx::query_scalar(
            "SELECT count(*)
             FROM proxima_core.personality_wake_invocations i
             JOIN proxima_core.intervention_decision_v1 d
               ON d.memory_id = i.continuation_intervention_decision_memory_id
             WHERE d.decision = 'continue'",
        )
        .fetch_one(self.pg.pool())
        .await?;
        let continuation_uses_decision_event_count: i64 = sqlx::query_scalar(
            "SELECT count(*)
             FROM proxima_core.personality_wake_invocations i
             JOIN proxima_core.change_event ce
               ON ce.seq = i.change_event_seq
              AND ce.entity_memory_id = i.continuation_intervention_decision_memory_id
             JOIN proxima_core.intervention_decision_v1 d
               ON d.memory_id = i.continuation_intervention_decision_memory_id
             WHERE d.decision = 'continue'",
        )
        .fetch_one(self.pg.pool())
        .await?;

        let fetch_handles = self.continuation_fetch_memory_handles().await?;
        let decision_handle = fetch_handles.iter().any(|handle| handle == "N1");
        let request_handle = fetch_handles.iter().any(|handle| handle == "N3");
        let prior_trace_handle = fetch_handles.iter().any(|handle| handle == "N4");
        let original_trigger_handle = fetch_handles.iter().any(|handle| handle == "N5");

        let mut checks = BTreeMap::new();
        checks.insert("one_continue_decision".into(), continue_decision_count == 1);
        checks.insert(
            "decision_has_core_derived_from_request".into(),
            decision_request_edge_count == 1,
        );
        checks.insert(
            "one_continuation_invocation".into(),
            continuation_invocation_count == 1,
        );
        checks.insert(
            "continuation_invocation_uses_decision_change_event".into(),
            continuation_uses_decision_event_count == 1,
        );
        checks.insert(
            "continuation_row_links_intervention_decision".into(),
            continuation_row_links_decision_count == 1,
        );
        checks.insert(
            "continuation_trace_fetches_intervention_decision_handle".into(),
            decision_handle,
        );
        checks.insert(
            "continuation_trace_fetches_intervention_request_handle".into(),
            request_handle,
        );
        checks.insert(
            "continuation_trace_fetches_prior_wake_trace_handle".into(),
            prior_trace_handle,
        );
        checks.insert(
            "continuation_trace_fetches_original_triggering_handle".into(),
            original_trigger_handle,
        );
        checks.insert(
            "continuation_trace_fetches_seeded_context_handles".into(),
            decision_handle && request_handle && prior_trace_handle && original_trigger_handle,
        );
        Ok(checks)
    }

    pub(super) async fn real_planner_checks(
        &self,
    ) -> Result<BTreeMap<String, bool>, Box<dyn std::error::Error>> {
        let mut checks = BTreeMap::new();
        if self.cfg.planner_mode != DemoPlannerMode::Real {
            return Ok(checks);
        }
        let Some(parent_goal) = self.goal_id else {
            checks.insert("planner_created_child_goals".into(), false);
            checks.insert("child_goal_titles_model_authored".into(), false);
            checks.insert("execution_requests_from_child_goals".into(), false);
            checks.insert("execution_requests_have_model_text".into(), false);
            checks.insert("execution_request_keys_not_scripted".into(), false);
            checks.insert(
                "execution_requests_include_acceptance_criteria".into(),
                false,
            );
            checks.insert("workspace_run_and_review_from_real_requests".into(), false);
            return Ok(checks);
        };
        let row = sqlx::query(
            "WITH child_roots AS (
                 SELECT gp.goal_id AS root_goal_id
                   FROM proxima_core.goal_parents gp
                  WHERE gp.parent_goal_id = $1
             ),
             child_activations AS (
                 SELECT ga.memory_id, ga.goal_id, ga.title
                   FROM proxima_goal.goal_activated_v1 ga
                   JOIN child_roots cr ON cr.root_goal_id = ga.goal_id
             ),
             child_requests AS (
                 SELECT DISTINCT er.memory_id, er.title, er.instructions, er.request_key
                   FROM proxima_code.execution_request_v1 er
                   JOIN proxima_core.edges e
                     ON e.source_kind = 'Fact'
                    AND e.source_memory_id = er.memory_id
                    AND e.target_kind = 'Fact'
                    AND e.target_memory_id IN (SELECT memory_id FROM child_activations)
                    AND e.relation = 'core/derived-from'
             ),
             child_runs AS (
                 SELECT DISTINCT wr.memory_id
                   FROM proxima_code.workspace_run_v1 wr
                   JOIN proxima_core.edges e
                     ON e.source_kind = 'Fact'
                    AND e.source_memory_id = wr.memory_id
                    AND e.target_kind = 'Fact'
                    AND e.target_memory_id IN (SELECT memory_id FROM child_requests)
                    AND e.relation = 'core/derived-from'
             )
             SELECT
                (SELECT count(*) FROM child_activations) AS child_goal_count,
                (SELECT count(*) FROM child_activations
                  WHERE title = ANY($2)) AS scripted_child_title_count,
                (SELECT count(*) FROM child_requests) AS execution_request_count,
                (SELECT count(*) FROM child_requests
                  WHERE length(btrim(title)) > 0
                    AND length(btrim(instructions)) > 0) AS request_with_text_count,
                (SELECT count(*) FROM child_requests
                  WHERE request_key = ANY($3)) AS scripted_request_key_count,
                (SELECT count(DISTINCT cr.memory_id)
                   FROM child_requests cr
                   JOIN proxima_code.acceptance_criteria_v1 ac
                     ON ac.execution_request_memory_id = cr.memory_id) AS request_with_acceptance_count,
                (SELECT count(*) FROM child_runs) AS workspace_run_count,
                (SELECT count(*)
                   FROM proxima_code.workspace_review_v1
                  WHERE execution_request_memory_id IN (SELECT memory_id FROM child_requests)) AS workspace_review_count",
        )
        .bind(parent_goal.into_inner())
        .bind(scripted_child_titles())
        .bind(scripted_request_keys())
        .fetch_one(self.pg.pool())
        .await?;
        let required = self.cfg.required_child_goal_count();
        let child_goal_count: i64 = row.try_get("child_goal_count")?;
        let scripted_child_title_count: i64 = row.try_get("scripted_child_title_count")?;
        let execution_request_count: i64 = row.try_get("execution_request_count")?;
        let request_with_text_count: i64 = row.try_get("request_with_text_count")?;
        let scripted_request_key_count: i64 = row.try_get("scripted_request_key_count")?;
        let request_with_acceptance_count: i64 = row.try_get("request_with_acceptance_count")?;
        let workspace_run_count: i64 = row.try_get("workspace_run_count")?;
        let workspace_review_count: i64 = row.try_get("workspace_review_count")?;

        checks.insert(
            "planner_created_child_goals".into(),
            child_goal_count >= required,
        );
        checks.insert(
            "child_goal_titles_model_authored".into(),
            child_goal_count >= required && scripted_child_title_count == 0,
        );
        checks.insert(
            "execution_requests_from_child_goals".into(),
            execution_request_count >= required,
        );
        checks.insert(
            "execution_requests_have_model_text".into(),
            request_with_text_count >= required,
        );
        checks.insert(
            "execution_request_keys_not_scripted".into(),
            scripted_request_key_count == 0,
        );
        checks.insert(
            "execution_requests_include_acceptance_criteria".into(),
            request_with_acceptance_count >= required,
        );
        checks.insert(
            "workspace_run_and_review_from_real_requests".into(),
            workspace_run_count >= required && workspace_review_count >= required,
        );
        Ok(checks)
    }

    async fn continuation_fetch_memory_handles(
        &self,
    ) -> Result<Vec<String>, Box<dyn std::error::Error>> {
        let Some(invocation_id) = sqlx::query_scalar::<_, Uuid>(
            "SELECT invocation_id
             FROM proxima_core.personality_wake_invocations
             WHERE continuation_intervention_decision_memory_id IS NOT NULL
             ORDER BY started_at ASC
             LIMIT 1",
        )
        .fetch_optional(self.pg.pool())
        .await?
        else {
            return Ok(Vec::new());
        };
        let rows = sqlx::query(
            "SELECT cj.body
             FROM proxima_core.wake_trace_v1 wt
             JOIN proxima_core.citation_mappings cm ON cm.memory_id = wt.memory_id
             JOIN proxima_core.cited_wake_trace_jsonl_v1 cj
               ON cj.cited_object_id = cm.cited_object_id
             WHERE wt.invocation_id = $1
             ORDER BY wt.started_at ASC",
        )
        .bind(invocation_id)
        .fetch_all(self.pg.pool())
        .await?;
        let mut handles = Vec::new();
        for row in rows {
            let body: Vec<u8> = row.try_get("body")?;
            for line in String::from_utf8_lossy(&body).lines() {
                let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
                    continue;
                };
                if value.get("record").and_then(serde_json::Value::as_str) != Some("tool_call")
                    || value.get("tool_name").and_then(serde_json::Value::as_str)
                        != Some("core/fetch_memory")
                {
                    continue;
                }
                if let Some(memory) = value
                    .get("args")
                    .and_then(|args| args.get("memory"))
                    .and_then(serde_json::Value::as_str)
                {
                    handles.push(memory.to_string());
                }
            }
        }
        Ok(handles)
    }

    pub(super) async fn request_flow_counts(&self) -> Result<Vec<RequestFlowCount>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT er.memory_id,
                    er.title,
                    count(DISTINCT wr.memory_id) AS workspace_run_count,
                    count(DISTINCT rv.memory_id) AS workspace_review_count,
                    count(DISTINCT rv.memory_id)
                      FILTER (WHERE rv.verdict IN ('approved', 'needs_user')) AS terminal_review_count
             FROM proxima_code.execution_request_v1 er
             LEFT JOIN proxima_core.edges e
               ON e.source_kind = 'Fact'
              AND e.target_kind = 'Fact'
              AND e.target_memory_id = er.memory_id
              AND e.relation = 'core/derived-from'
             LEFT JOIN proxima_code.workspace_run_v1 wr
               ON wr.memory_id = e.source_memory_id
             LEFT JOIN proxima_code.workspace_review_v1 rv
               ON rv.execution_request_memory_id = er.memory_id
             GROUP BY er.memory_id, er.title
             ORDER BY er.title, er.memory_id",
        )
        .fetch_all(self.pg.pool())
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(RequestFlowCount {
                    request_memory_id: row.try_get::<Uuid, _>("memory_id")?.to_string(),
                    title: row.try_get("title")?,
                    workspace_run_count: row.try_get("workspace_run_count")?,
                    workspace_review_count: row.try_get("workspace_review_count")?,
                    terminal_review_count: row.try_get("terminal_review_count")?,
                })
            })
            .collect()
    }

    pub(super) async fn wake_invocation_metrics(
        &self,
    ) -> Result<Vec<WakeInvocationMetric>, Box<dyn std::error::Error>> {
        let role_case = self.role_case_sql();
        let sql = format!(
            "SELECT {role_case} AS role,
                    i.status::text AS status,
                    i.duration_ms,
                    i.turn_count,
                    i.cost_usd::text AS cost_usd,
                    i.resolved_inference_target_ref,
                    i.failure_reason,
                    wt.model_id,
                    wt.rounds_used,
                    wt.tool_call_count,
                    wt.total_prompt_tokens,
                    wt.total_completion_tokens
             FROM proxima_core.personality_wake_invocations i
             LEFT JOIN LATERAL (
                SELECT *
                FROM proxima_core.wake_trace_v1 wt
                WHERE wt.personality_instance_id = i.personality_instance_id
                  AND wt.wake_entry_id = i.wake_entry_id
                ORDER BY wt.started_at DESC
                LIMIT 1
             ) wt ON true
             ORDER BY i.started_at ASC"
        );
        let rows = sqlx::query(&sql).fetch_all(self.pg.pool()).await?;
        rows.into_iter()
            .map(|row| {
                Ok(WakeInvocationMetric {
                    role: row.try_get("role")?,
                    status: row.try_get("status")?,
                    duration_ms: row.try_get("duration_ms")?,
                    rounds_or_turns: row
                        .try_get::<Option<i32>, _>("rounds_used")?
                        .or(row.try_get("turn_count")?),
                    tool_calls: row.try_get("tool_call_count")?,
                    target_ref: row.try_get("resolved_inference_target_ref")?,
                    model_id: row.try_get("model_id")?,
                    prompt_tokens: row.try_get("total_prompt_tokens")?,
                    completion_tokens: row.try_get("total_completion_tokens")?,
                    cost_usd: row.try_get("cost_usd")?,
                    failure_reason: row.try_get("failure_reason")?,
                })
            })
            .collect()
    }

    pub(super) fn role_case_sql(&self) -> String {
        let mut arms = String::from("CASE");
        for (role, id) in &self.role_ids {
            arms.push_str(&format!(
                " WHEN i.personality_instance_id = '{}' THEN '{}'",
                id.into_inner(),
                role.replace('\'', "''")
            ));
        }
        arms.push_str(" ELSE 'unknown' END");
        arms
    }

    pub(super) async fn output_sidecar_counts(&self) -> Result<BTreeMap<String, i64>, sqlx::Error> {
        let mut out = BTreeMap::new();
        for (schema, table) in [
            (
                ExecutionRequestV1::SCHEMA_ID,
                "proxima_code.execution_request_v1",
            ),
            (WorkspaceRunV1::SCHEMA_ID, "proxima_code.workspace_run_v1"),
            (
                WorkspaceReviewV1::SCHEMA_ID,
                "proxima_code.workspace_review_v1",
            ),
            (
                "proxima-goal/goal-achieved-v1",
                "proxima_goal.goal_achieved_v1",
            ),
            (VisionBriefV1::SCHEMA_ID, "proxima_intent.vision_brief_v1"),
            (
                InterventionRequestedV1::SCHEMA_ID,
                "proxima_core.intervention_requested_v1",
            ),
            (
                InterventionDecisionV1::SCHEMA_ID,
                "proxima_core.intervention_decision_v1",
            ),
        ] {
            let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
                .fetch_one(self.pg.pool())
                .await?;
            out.insert(schema.into(), count);
        }
        Ok(out)
    }

    pub(super) async fn review_verdicts(&self) -> Result<BTreeMap<String, i64>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT verdict::text, count(*) AS count
             FROM proxima_code.workspace_review_v1
             GROUP BY verdict
             ORDER BY verdict",
        )
        .fetch_all(self.pg.pool())
        .await?;
        rows.into_iter()
            .map(|row| Ok((row.try_get("verdict")?, row.try_get("count")?)))
            .collect()
    }

    pub(super) async fn final_goal_state(&self) -> Result<String, sqlx::Error> {
        let Some(goal_id) = self.goal_id else {
            return Ok("not_created".into());
        };
        let state: Option<String> = sqlx::query_scalar(
            "SELECT state::text
             FROM proxima_core.goals
             WHERE supersedes = $1 OR goal_id = $1
             ORDER BY created_at DESC
             LIMIT 1",
        )
        .bind(goal_id.into_inner())
        .fetch_optional(self.pg.pool())
        .await?;
        Ok(state.unwrap_or_else(|| "missing".into()))
    }

    pub(super) async fn git_diff_metrics(
        &self,
    ) -> Result<(GitDiffStats, Vec<String>), Box<dyn std::error::Error>> {
        let Some(worktree) = self.latest_worktree().await? else {
            return Ok((GitDiffStats::default(), Vec::new()));
        };
        let path = worktree.path;
        let base = "main";
        let numstat = git_output(&path, &["diff", "--numstat", base])?;
        let mut stats = GitDiffStats::default();
        let mut files = Vec::new();
        for line in numstat.lines() {
            let mut parts = line.split('\t');
            let insertions = parts.next().unwrap_or("0").parse::<u32>().unwrap_or(0);
            let deletions = parts.next().unwrap_or("0").parse::<u32>().unwrap_or(0);
            if let Some(file) = parts.next() {
                stats.files_changed += 1;
                stats.insertions = stats.insertions.saturating_add(insertions);
                stats.deletions = stats.deletions.saturating_add(deletions);
                files.push(file.to_string());
            }
        }
        for file in git_output(&path, &["ls-files", "--others", "--exclude-standard"])?
            .lines()
            .filter(|line| !line.is_empty())
        {
            stats.files_changed += 1;
            stats.insertions = stats
                .insertions
                .saturating_add(count_file_lines(&path.join(file))?);
            files.push(file.to_string());
        }
        files.sort();
        files.dedup();
        Ok((stats, files))
    }

    pub(super) async fn latest_worktree(&self) -> Result<Option<WorktreeInfo>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT worktree_path, branch_name
             FROM proxima_code.workspace_run_v1
             ORDER BY memory_id DESC",
        )
        .fetch_all(self.pg.pool())
        .await?;
        for row in rows {
            let path = PathBuf::from(row.try_get::<String, _>("worktree_path")?);
            if self.cfg.challenge.worktree_has_primary_output(&path) {
                return Ok(Some(WorktreeInfo {
                    path,
                    branch_name: row.try_get("branch_name")?,
                }));
            }
        }
        Ok(None)
    }

    pub(super) async fn run_read_only_reviewer(
        &self,
        diff_stats: &GitDiffStats,
        changed_files: &[String],
    ) -> Result<ReviewerScore, Box<dyn std::error::Error>> {
        let prompt = self.cfg.challenge.reviewer_prompt(
            self.latest_worktree().await?,
            diff_stats,
            changed_files,
        )?;
        let outcome = self
            .harness
            .run(
                HarnessProgram {
                    system_prompt: "You are a strict read-only product evaluator.".into(),
                    instructions: prompt,
                    context_params: BTreeMap::new().into_iter().collect(),
                    tool_projection: Vec::new(),
                    substrate_tool_palette: Vec::new(),
                    workspace_root: None,
                    max_rounds: 1,
                    provider: ProviderTarget::MistralChat {
                        base_url: self.cfg.base_url.clone(),
                        model_id: MODEL_ID.into(),
                        api_key: std::env::var(&self.cfg.api_key_env)?,
                        temperature: Some(0.0),
                        max_completion_tokens: Some(1024),
                    },
                },
                HarnessContext {
                    owner: self.owner.clone(),
                    invocation_id: Uuid::now_v7(),
                    wake_entry_id: Uuid::now_v7(),
                    personality_instance_id: *self
                        .role_ids
                        .get("Goal-Reviewer")
                        .ok_or("Goal-Reviewer missing")?,
                    change_event_seq: Uuid::now_v7(),
                    root_perspective_memory_id: MemoryId::new(Uuid::now_v7()),
                    wake_token: Uuid::now_v7(),
                    invocation_timeout: Duration::from_secs(120),
                },
            )
            .await?;
        let text =
            assistant_text_from_jsonl(&outcome.jsonl_bytes).ok_or("evaluator returned no text")?;
        let json_text = extract_json_object(&text).ok_or("evaluator returned no JSON object")?;
        parse_reviewer_score(json_text)
    }

    pub(super) async fn auto_merge_successful_worktree(
        &self,
    ) -> Result<AutoMergeMetric, Box<dyn std::error::Error>> {
        let worktree = self
            .latest_worktree()
            .await?
            .ok_or("no generated worktree found for auto merge")?;
        let repo_status = git_output(&self.cfg.repo_path, &["status", "--porcelain"])?;
        if !repo_status.trim().is_empty() {
            return Err(format!(
                "demo repo has uncommitted changes before auto merge: {repo_status}"
            )
            .into());
        }
        let worktree_status = git_output(&worktree.path, &["status", "--porcelain"])?;
        if worktree_status.trim().is_empty() {
            return Err("generated worktree has no changes to auto merge".into());
        }
        git(&worktree.path, &["add", "-A"])?;
        git(
            &worktree.path,
            &[
                "-c",
                "user.name=Proxima Demo",
                "-c",
                "user.email=demo@example.test",
                "commit",
                "-m",
                match self.cfg.challenge {
                    DemoChallenge::SignalMatch => "feat: auto merge signal match demo result",
                    DemoChallenge::TodoCli => "feat: auto merge todo audit demo result",
                    DemoChallenge::KanbanBoard => "feat: auto merge kanban board demo result",
                },
            ],
        )?;
        let commit_sha = git_output(&worktree.path, &["rev-parse", "HEAD"])?;
        git(&self.cfg.repo_path, &["merge", "--ff-only", &commit_sha])?;
        Ok(AutoMergeMetric {
            worktree_path: worktree.path.display().to_string(),
            branch_name: worktree.branch_name,
            commit_sha,
            merged_to_repo: self.cfg.repo_path.display().to_string(),
            merged_to_branch: "main".into(),
        })
    }
}
