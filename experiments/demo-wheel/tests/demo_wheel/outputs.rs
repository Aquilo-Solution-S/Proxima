use super::*;

impl DemoWorld {
    pub(super) async fn write_outputs(
        &self,
        metrics: &mut Metrics,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let metrics_path = self.cfg.run_dir.join("metrics.json");
        let report_path = self.cfg.run_dir.join("report.md");
        let graph_path = self.cfg.run_dir.join("flow_graph.json");
        let mermaid_path = self.cfg.run_dir.join("flow_graph.mmd");
        let flow_graph = self.collect_flow_graph().await?;
        let conversation_index = self.write_conversation_bundle().await?;
        metrics.conversation_index_json = conversation_index.index_path.clone();
        metrics.conversation_invocation_count = conversation_index.invocation_count;
        metrics.conversation_missing_log_count = conversation_index.missing_log_count;
        std::fs::write(&metrics_path, serde_json::to_vec_pretty(metrics)?)?;
        std::fs::write(&graph_path, serde_json::to_vec_pretty(&flow_graph)?)?;
        std::fs::write(&mermaid_path, render_flow_mermaid(&flow_graph))?;
        std::fs::write(&report_path, render_report(metrics, &flow_graph))?;
        eprintln!("demo metrics: {}", metrics_path.display());
        eprintln!("demo report: {}", report_path.display());
        eprintln!("demo flow graph: {}", graph_path.display());
        eprintln!("demo flow mermaid: {}", mermaid_path.display());
        eprintln!("demo conversations: {}", conversation_index.index_path);
        Ok(())
    }

    pub(super) async fn write_conversation_bundle(
        &self,
    ) -> Result<ConversationIndex, Box<dyn std::error::Error>> {
        let conversations_dir = self.cfg.run_dir.join("conversations");
        std::fs::create_dir_all(&conversations_dir)?;
        let index_path = conversations_dir.join("index.json");
        let role_case = self.role_case_sql();
        let sql = format!(
            "SELECT i.invocation_id,
                    {role_case} AS role,
                    i.personality_instance_id,
                    i.wake_entry_id,
                    e.trigger_id,
                    i.change_event_seq,
                    e.execution_mode::text AS execution_mode,
                    i.status::text AS status,
                    artifact.message_tail AS source_jsonl_path
             FROM proxima_core.personality_wake_invocations i
             JOIN proxima_core.personality_wake_entries e
               ON e.owner_principal_kind = i.owner_principal_kind
              AND e.owner_principal_id = i.owner_principal_id
              AND e.owner_org_id = i.owner_org_id
              AND e.personality_instance_id = i.personality_instance_id
              AND e.wake_entry_id = i.wake_entry_id
             LEFT JOIN LATERAL (
                SELECT l.message_tail
                FROM proxima_core.personality_wake_invocation_logs l
                WHERE l.invocation_id = i.invocation_id
                  AND l.phase = 'session_artifact'
                  AND l.status = 'started'
                ORDER BY l.log_seq ASC
                LIMIT 1
             ) artifact ON true
             ORDER BY i.started_at ASC"
        );
        let rows = sqlx::query(&sql).fetch_all(self.pg.pool()).await?;
        let mut invocations = Vec::with_capacity(rows.len());
        for row in rows {
            let invocation_id: Uuid = row.try_get("invocation_id")?;
            let role: String = row.try_get("role")?;
            let source_jsonl_path: Option<String> = row.try_get("source_jsonl_path")?;
            let copied_name = format!(
                "{}-{}.jsonl",
                conversation_file_component(&role),
                invocation_id
            );
            let copied_relative = PathBuf::from("conversations").join(copied_name);
            let copied_absolute = self.cfg.run_dir.join(&copied_relative);
            let mut copied_jsonl_path = None;
            let mut copy_error = None;
            let missing_log = match source_jsonl_path.as_deref() {
                Some(source) if Path::new(source).is_file() => {
                    if let Err(err) = std::fs::copy(source, &copied_absolute) {
                        copy_error = Some(err.to_string());
                        true
                    } else {
                        copied_jsonl_path = Some(copied_relative.display().to_string());
                        false
                    }
                }
                Some(source) => {
                    copy_error = Some(format!("source log not found: {source}"));
                    true
                }
                None => true,
            };
            invocations.push(ConversationInvocationArtifact {
                invocation_id: invocation_id.to_string(),
                role,
                personality_instance_id: row
                    .try_get::<Uuid, _>("personality_instance_id")?
                    .to_string(),
                wake_entry_id: row.try_get::<Uuid, _>("wake_entry_id")?.to_string(),
                trigger_schema_id: row.try_get("trigger_id")?,
                change_event_seq: row.try_get::<Uuid, _>("change_event_seq")?.to_string(),
                execution_mode: row.try_get("execution_mode")?,
                status: row.try_get("status")?,
                source_jsonl_path,
                copied_jsonl_path,
                missing_log,
                copy_error,
            });
        }
        let missing_log_count = invocations
            .iter()
            .filter(|invocation| invocation.missing_log)
            .count();
        let index = ConversationIndex {
            run_dir: self.cfg.run_dir.display().to_string(),
            index_path: index_path.display().to_string(),
            invocation_count: invocations.len(),
            missing_log_count,
            invocations,
        };
        std::fs::write(&index_path, serde_json::to_vec_pretty(&index)?)?;
        Ok(index)
    }

    pub(super) async fn collect_flow_graph(&self) -> Result<FlowGraph, Box<dyn std::error::Error>> {
        let mut nodes = BTreeMap::<String, FlowNode>::new();
        let mut edges = Vec::<FlowEdge>::new();

        for (role, id) in &self.role_ids {
            nodes.insert(
                entity_node_id("personality", id.into_inner()),
                FlowNode {
                    id: entity_node_id("personality", id.into_inner()),
                    kind: "personality".into(),
                    label: role.clone(),
                    role: Some(role.clone()),
                    schema_id: None,
                    state: None,
                    status: Some("active".into()),
                },
            );
        }

        for row in sqlx::query(
            "SELECT goal_id, title, state::text AS state
             FROM proxima_core.goals
             ORDER BY created_at ASC",
        )
        .fetch_all(self.pg.pool())
        .await?
        {
            let goal_id: Uuid = row.try_get("goal_id")?;
            nodes.insert(
                entity_node_id("goal", goal_id),
                FlowNode {
                    id: entity_node_id("goal", goal_id),
                    kind: "goal".into(),
                    label: row.try_get("title")?,
                    role: None,
                    schema_id: Some("proxima-goal".into()),
                    state: Some(row.try_get("state")?),
                    status: None,
                },
            );
        }

        for row in sqlx::query(
            "SELECT m.memory_id, m.schema_id,
                    COALESCE(er.title, wr.branch_name, rv.summary, ga.title, gp.title, gh.title,
                             'Vision: ' || vb.interpreted_outcome,
                             'Intervention request: ' || br.original_invocation_id::text,
                             'Intervention decision: ' || bd.decision::text,
                             m.schema_id) AS label
             FROM proxima_core.memories m
             LEFT JOIN proxima_code.execution_request_v1 er USING (memory_id)
             LEFT JOIN proxima_core.workspace_run_v1 wr USING (memory_id)
             LEFT JOIN proxima_code.workspace_review_v1 rv USING (memory_id)
             LEFT JOIN proxima_code.verification_evidence_v1 ve USING (memory_id)
             LEFT JOIN proxima_goal.goal_activated_v1 ga USING (memory_id)
             LEFT JOIN proxima_goal.goal_proposed_v1 gp USING (memory_id)
             LEFT JOIN proxima_goal.goal_achieved_v1 gh USING (memory_id)
             LEFT JOIN proxima_intent.vision_brief_v1 vb USING (memory_id)
             LEFT JOIN proxima_core.intervention_requested_v1 br USING (memory_id)
             LEFT JOIN proxima_core.intervention_decision_v1 bd USING (memory_id)
             ORDER BY m.created_at ASC",
        )
        .fetch_all(self.pg.pool())
        .await?
        {
            let memory_id: Uuid = row.try_get("memory_id")?;
            let schema_id: String = row.try_get("schema_id")?;
            nodes.insert(
                entity_node_id("memory", memory_id),
                FlowNode {
                    id: entity_node_id("memory", memory_id),
                    kind: "memory".into(),
                    label: row.try_get("label")?,
                    role: None,
                    schema_id: Some(schema_id),
                    state: None,
                    status: None,
                },
            );
        }

        for row in sqlx::query(
            "SELECT goal_id, parent_goal_id
             FROM proxima_core.goal_parents
             ORDER BY parent_goal_id, goal_id",
        )
        .fetch_all(self.pg.pool())
        .await?
        {
            let child: Uuid = row.try_get("goal_id")?;
            let parent: Uuid = row.try_get("parent_goal_id")?;
            edges.push(FlowEdge {
                id: format!("goal-parent:{parent}:{child}"),
                source: entity_node_id("goal", parent),
                target: entity_node_id("goal", child),
                relation: "goal_parent".into(),
                persisted_edge_id: None,
            });
        }

        for row in sqlx::query(
            "SELECT edge_id, relation,
                    source_memory_id, source_goal_id,
                    target_memory_id, target_goal_id
             FROM proxima_core.edges
             ORDER BY created_at ASC",
        )
        .fetch_all(self.pg.pool())
        .await?
        {
            let edge_id: Uuid = row.try_get("edge_id")?;
            let source = flow_endpoint(
                row.try_get::<Option<Uuid>, _>("source_memory_id")?,
                row.try_get::<Option<Uuid>, _>("source_goal_id")?,
            );
            let target = flow_endpoint(
                row.try_get::<Option<Uuid>, _>("target_memory_id")?,
                row.try_get::<Option<Uuid>, _>("target_goal_id")?,
            );
            edges.push(FlowEdge {
                id: format!("edge:{edge_id}"),
                source,
                target,
                relation: row.try_get("relation")?,
                persisted_edge_id: Some(edge_id.to_string()),
            });
        }

        for row in sqlx::query(
            "SELECT i.personality_instance_id, i.wake_entry_id, i.change_event_seq,
                    i.continuation_intervention_decision_memory_id,
                    i.status::text AS status
             FROM proxima_core.personality_wake_invocations i
             ORDER BY i.started_at ASC",
        )
        .fetch_all(self.pg.pool())
        .await?
        {
            let personality_id: Uuid = row.try_get("personality_instance_id")?;
            let change_event_seq: Uuid = row.try_get("change_event_seq")?;
            let wake_entry_id: Uuid = row.try_get("wake_entry_id")?;
            let wake_node_id = format!("wake:{personality_id}:{wake_entry_id}:{change_event_seq}");
            nodes.insert(
                wake_node_id.clone(),
                FlowNode {
                    id: wake_node_id.clone(),
                    kind: "wake_invocation".into(),
                    label: role_for_personality(&self.role_ids, personality_id)
                        .unwrap_or_else(|| "wake".into()),
                    role: role_for_personality(&self.role_ids, personality_id),
                    schema_id: None,
                    state: None,
                    status: Some(row.try_get("status")?),
                },
            );
            edges.push(FlowEdge {
                id: format!("wake-trigger:{personality_id}:{change_event_seq}"),
                source: format!("event:{change_event_seq}"),
                target: wake_node_id.clone(),
                relation: "wake_triggered".into(),
                persisted_edge_id: None,
            });
            edges.push(FlowEdge {
                id: format!("wake-role:{personality_id}:{change_event_seq}"),
                source: entity_node_id("personality", personality_id),
                target: wake_node_id.clone(),
                relation: "wake_executed_by".into(),
                persisted_edge_id: None,
            });
            if let Some(decision_memory_id) =
                row.try_get::<Option<Uuid>, _>("continuation_intervention_decision_memory_id")?
            {
                edges.push(FlowEdge {
                    id: format!("wake-continuation:{decision_memory_id}:{change_event_seq}"),
                    source: entity_node_id("memory", decision_memory_id),
                    target: wake_node_id,
                    relation: "continuation_wake".into(),
                    persisted_edge_id: None,
                });
            }
        }

        let mut events = Vec::new();
        for row in sqlx::query(
            "SELECT seq, kind::text AS kind, entity_kind::text AS entity_kind,
                    entity_memory_id, entity_goal_id, entity_schema_id, edge_relation,
                    entity_personality_instance_id, wake_chain_depth
             FROM proxima_core.change_event
             ORDER BY seq ASC",
        )
        .fetch_all(self.pg.pool())
        .await?
        {
            let seq: Uuid = row.try_get("seq")?;
            let entity_memory_id: Option<Uuid> = row.try_get("entity_memory_id")?;
            let entity_goal_id: Option<Uuid> = row.try_get("entity_goal_id")?;
            let event_node_id = format!("event:{seq}");
            nodes.insert(
                event_node_id.clone(),
                FlowNode {
                    id: event_node_id,
                    kind: "change_event".into(),
                    label: row.try_get("kind")?,
                    role: None,
                    schema_id: row.try_get("entity_schema_id")?,
                    state: None,
                    status: None,
                },
            );
            if let Some(entity_id) = entity_memory_id {
                edges.push(FlowEdge {
                    id: format!("event-entity:{seq}:{entity_id}"),
                    source: format!("event:{seq}"),
                    target: entity_node_id("memory", entity_id),
                    relation: "event_appended".into(),
                    persisted_edge_id: None,
                });
            }
            if let Some(entity_id) = entity_goal_id {
                edges.push(FlowEdge {
                    id: format!("event-goal:{seq}:{entity_id}"),
                    source: format!("event:{seq}"),
                    target: entity_node_id("goal", entity_id),
                    relation: "event_appended".into(),
                    persisted_edge_id: None,
                });
            }
            events.push(FlowEvent {
                seq: seq.to_string(),
                kind: row.try_get("kind")?,
                entity_kind: row.try_get("entity_kind")?,
                entity_id: entity_memory_id.or(entity_goal_id).map(|id| id.to_string()),
                schema_id: row.try_get("entity_schema_id")?,
                edge_relation: row.try_get("edge_relation")?,
                personality_instance_id: row
                    .try_get::<Option<Uuid>, _>("entity_personality_instance_id")?
                    .map(|id| id.to_string()),
                wake_chain_depth: row.try_get("wake_chain_depth")?,
            });
        }

        let unresolved_endpoint_count = edges
            .iter()
            .filter(|edge| !nodes.contains_key(&edge.source) || !nodes.contains_key(&edge.target))
            .count();
        let nodes = nodes.into_values().collect::<Vec<_>>();
        let summary = FlowGraphSummary {
            node_count: nodes.len(),
            edge_count: edges.len(),
            personality_count: nodes.iter().filter(|n| n.kind == "personality").count(),
            goal_count: nodes.iter().filter(|n| n.kind == "goal").count(),
            execution_request_count: nodes
                .iter()
                .filter(|n| {
                    n.kind == "memory"
                        && n.schema_id.as_deref() == Some(ExecutionRequestV1::SCHEMA_ID)
                })
                .count(),
            test_request_count: nodes
                .iter()
                .filter(|n| {
                    n.kind == "memory" && n.schema_id.as_deref() == Some(TestRequestV1::SCHEMA_ID)
                })
                .count(),
            workspace_run_count: nodes
                .iter()
                .filter(|n| {
                    n.kind == "memory"
                        && n.schema_id.as_deref() == Some(CoreWorkspaceRunV1::SCHEMA_ID)
                })
                .count(),
            workspace_review_count: nodes
                .iter()
                .filter(|n| {
                    n.kind == "memory"
                        && n.schema_id.as_deref() == Some(WorkspaceReviewV1::SCHEMA_ID)
                })
                .count(),
            verification_evidence_count: nodes
                .iter()
                .filter(|n| {
                    n.kind == "memory"
                        && n.schema_id.as_deref() == Some("proxima-code/verification-evidence-v1")
                })
                .count(),
            vision_brief_count: nodes
                .iter()
                .filter(|n| {
                    n.kind == "memory" && n.schema_id.as_deref() == Some(VisionBriefV1::SCHEMA_ID)
                })
                .count(),
            intervention_request_count: nodes
                .iter()
                .filter(|n| {
                    n.kind == "memory"
                        && n.schema_id.as_deref() == Some(InterventionRequestedV1::SCHEMA_ID)
                })
                .count(),
            intervention_decision_count: nodes
                .iter()
                .filter(|n| {
                    n.kind == "memory"
                        && n.schema_id.as_deref() == Some(InterventionDecisionV1::SCHEMA_ID)
                })
                .count(),
            wake_invocation_count: nodes.iter().filter(|n| n.kind == "wake_invocation").count(),
            unresolved_endpoint_count,
        };
        Ok(FlowGraph {
            nodes,
            edges,
            events,
            summary,
        })
    }

    pub(super) async fn failure_report(
        &self,
        stage: &str,
    ) -> Result<String, Box<dyn std::error::Error>> {
        let latest = sqlx::query(
            "SELECT i.personality_instance_id, i.status::text, i.failure_reason,
                    i.turn_count, i.stdout_tail, i.stderr_tail
             FROM proxima_core.personality_wake_invocations i
             ORDER BY i.started_at DESC
             LIMIT 1",
        )
        .fetch_optional(self.pg.pool())
        .await?;
        let sidecars = self.output_sidecar_counts().await?;
        let (diff, files) = self.git_diff_metrics().await?;
        let tool_errors = self.first_jsonl_tool_errors().await.unwrap_or_default();
        Ok(format!(
            "demo wheel failed at stage: {stage}\nlatest invocation: {:?}\nfirst JSONL tool errors: {:?}\nsidecar counts: {:?}\ndiff: files={} insertions={} deletions={} changed={:?}",
            latest.map(|row| json!({
                "personality_instance_id": row.try_get::<Uuid, _>("personality_instance_id").ok(),
                "status": row.try_get::<String, _>("status").ok(),
                "failure_reason": row.try_get::<Option<String>, _>("failure_reason").ok().flatten(),
                "turn_count": row.try_get::<i32, _>("turn_count").ok(),
                "stdout_tail": row.try_get::<Option<String>, _>("stdout_tail").ok().flatten(),
                "stderr_tail": row.try_get::<Option<String>, _>("stderr_tail").ok().flatten(),
            })),
            tool_errors,
            sidecars,
            diff.files_changed,
            diff.insertions,
            diff.deletions,
            files
        ))
    }

    pub(super) async fn first_jsonl_tool_errors(
        &self,
    ) -> Result<Vec<serde_json::Value>, Box<dyn std::error::Error>> {
        let rows = sqlx::query(
            "SELECT cj.body
             FROM proxima_core.wake_trace_v1 wt
             JOIN proxima_core.memories m ON m.memory_id = wt.memory_id
             JOIN proxima_core.citation_mappings cm ON cm.memory_id = m.memory_id
             JOIN proxima_core.cited_wake_trace_jsonl_v1 cj
               ON cj.cited_object_id = cm.cited_object_id
             ORDER BY wt.started_at ASC
             LIMIT 10",
        )
        .fetch_all(self.pg.pool())
        .await?;
        let mut errors = Vec::new();
        for row in rows {
            let body: Vec<u8> = row.try_get("body")?;
            for line in String::from_utf8_lossy(&body).lines() {
                let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
                    continue;
                };
                if value.get("record").and_then(serde_json::Value::as_str) == Some("tool_result")
                    && value.get("status").and_then(serde_json::Value::as_str) == Some("error")
                {
                    errors.push(value);
                    if errors.len() >= 5 {
                        return Ok(errors);
                    }
                }
            }
        }
        Ok(errors)
    }
}

pub(super) fn conversation_file_component(raw: &str) -> String {
    let mut out = String::new();
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "unknown".into()
    } else {
        trimmed.into()
    }
}

#[cfg(test)]
mod tests {
    use super::conversation_file_component;

    #[test]
    fn conversation_file_component_is_stable_for_roles() {
        assert_eq!(
            conversation_file_component("Goal Reviewer"),
            "goal-reviewer"
        );
        assert_eq!(
            conversation_file_component("Wake/Supervisor"),
            "wake-supervisor"
        );
        assert_eq!(conversation_file_component("  "), "unknown");
    }
}
