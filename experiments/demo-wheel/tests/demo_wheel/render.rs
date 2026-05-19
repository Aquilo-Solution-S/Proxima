use super::*;

pub(super) fn render_report(metrics: &Metrics, flow_graph: &FlowGraph) -> String {
    format!(
        "# Proxima Demo Wheel Report\n\n- run_dir: `{}`\n- repo_path: `{}`\n- db_name: `{}`\n- ticks: `{}`\n- corrections: `{}`\n- goal_state: `{}`\n- deterministic_pass: `{}`\n- functional_pass: `{}`\n- intervention_pass: `{}`\n- overall_pass: `{}`\n- reviewer_score: `{}`\n- overall_score: `{}`\n- score_per_model_round: `{:?}`\n- score_per_wall_clock_second: `{:.4}`\n\n## Role Round Caps\n\n```json\n{}\n```\n\n## Goal Graph\n\n```json\n{}\n```\n\n## Request Flow Counts\n\n```json\n{}\n```\n\n## Terminal Guard Hits\n\n```json\n{}\n```\n\n## Flow Graph\n\n- graph_json: `{}`\n- graph_mermaid: `{}`\n- nodes: `{}`\n- edges: `{}`\n- intervention_requests: `{}`\n- intervention_decisions: `{}`\n- unresolved_endpoints: `{}`\n\n```mermaid\n{}\n```\n\n## Auto Merge\n\n```json\n{}\n```\n\n## Diff\n\n- files_changed: `{}`\n- insertions: `{}`\n- deletions: `{}`\n- files: `{:?}`\n\n## Wake Invocations\n\n```json\n{}\n```\n\n## Checks\n\n```json\n{}\n```\n",
        metrics.run_dir,
        metrics.repo_path,
        metrics.db_name,
        metrics.dispatcher_tick_count,
        metrics.correction_loop_count,
        metrics.final_goal_state,
        metrics.deterministic_pass,
        metrics.functional_pass,
        metrics.intervention_pass,
        metrics.overall_pass,
        metrics
            .reviewer_score
            .as_ref()
            .map(|s| s.score.to_string())
            .unwrap_or_else(|| "null".into()),
        metrics.overall_score,
        metrics.score_per_model_round,
        metrics.score_per_wall_clock_second,
        serde_json::to_string_pretty(&metrics.role_max_rounds).unwrap_or_default(),
        serde_json::to_string_pretty(&metrics.goal_graph).unwrap_or_default(),
        serde_json::to_string_pretty(&metrics.request_flow_counts).unwrap_or_default(),
        serde_json::to_string_pretty(&metrics.terminal_guard_hits).unwrap_or_default(),
        metrics.flow_graph_json,
        metrics.flow_graph_mermaid,
        flow_graph.summary.node_count,
        flow_graph.summary.edge_count,
        flow_graph.summary.intervention_request_count,
        flow_graph.summary.intervention_decision_count,
        flow_graph.summary.unresolved_endpoint_count,
        render_flow_mermaid(flow_graph),
        serde_json::to_string_pretty(&metrics.auto_merge).unwrap_or_default(),
        metrics.git_diff_stats.files_changed,
        metrics.git_diff_stats.insertions,
        metrics.git_diff_stats.deletions,
        metrics.final_changed_files,
        serde_json::to_string_pretty(&metrics.wake_invocations).unwrap_or_default(),
        serde_json::to_string_pretty(&metrics.deterministic_checks).unwrap_or_default()
    )
}

pub(super) fn render_flow_mermaid(graph: &FlowGraph) -> String {
    let mut out = String::from("graph TD\n");
    for node in &graph.nodes {
        out.push_str(&format!(
            "  {}[\"{}\"]\n",
            mermaid_id(&node.id),
            mermaid_label(&node.label)
        ));
    }
    for edge in &graph.edges {
        out.push_str(&format!(
            "  {} -->|{}| {}\n",
            mermaid_id(&edge.source),
            mermaid_label(&edge.relation),
            mermaid_id(&edge.target)
        ));
    }
    out
}

pub(super) fn entity_node_id(kind: &str, id: Uuid) -> String {
    format!("{kind}:{id}")
}

pub(super) fn flow_endpoint(memory_id: Option<Uuid>, goal_id: Option<Uuid>) -> String {
    if let Some(memory_id) = memory_id {
        entity_node_id("memory", memory_id)
    } else if let Some(goal_id) = goal_id {
        entity_node_id("goal", goal_id)
    } else {
        "missing:endpoint".into()
    }
}

pub(super) fn role_for_personality(
    role_ids: &BTreeMap<String, PersonalityInstanceId>,
    personality_id: Uuid,
) -> Option<String> {
    role_ids
        .iter()
        .find_map(|(role, id)| (id.into_inner() == personality_id).then(|| role.clone()))
}

pub(super) fn mermaid_id(raw: &str) -> String {
    let mut id = String::from("n");
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            id.push(ch);
        } else {
            id.push('_');
        }
    }
    id
}

pub(super) fn mermaid_label(raw: &str) -> String {
    raw.replace('\\', "\\\\").replace('"', "\\\"")
}
