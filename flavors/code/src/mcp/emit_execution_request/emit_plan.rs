use std::collections::HashMap;

use proxima_core::access::AccessKind;
use proxima_core::{AbstractionPayload, EdgeId, FactPayload, MemoryId, Tool, ToolCtx, ToolError};

use crate::payloads::{
    AcceptanceCriteriaV1, CodeExecutionPlanItemKind, CodeExecutionPlanItemV1, CodeExecutionPlanV1,
    ExecutionRequestV1, TestRequestV1,
};

use super::super::sql::{map_storage, resolve_repo_identifier};
use super::super::{CodeToolCtxExt, code_store};
use super::context_validation::{
    validate_active_goal_context, validate_evidence_in_owner, validate_goal_activated_fact,
    validate_plan_source_abstraction_in_owner, validate_repo,
};
use super::edges::{
    append_acceptance_criteria_edge, append_authored_edge, append_dependency_edge,
    append_derived_edge,
};
use super::ingest::{ingest_acceptance_criteria, ingest_execution_request, ingest_test_request};
use super::input_validation::{normalize_text, resolve_evidence, validate_plan_items};
use super::plan_persistence::{
    append_execution_plan, append_plan_fact_evidence_edge, default_plan_key,
};
use super::types::{
    CodeEmitExecutionPlanArgs, CodeEmitExecutionPlanOutput, ExecutionPlanItemKind,
    ExecutionPlanItemOutput,
};

#[derive(Debug)]
pub struct CodeEmitExecutionPlanTool;

impl Tool for CodeEmitExecutionPlanTool {
    const NAME: &'static str = "proxima-code_emit_execution_plan";
    const DESCRIPTION: &'static str = "Atomically emit a repo-scoped execution-plan Abstraction plus implementation/test request Facts and core/depends-on edges.";
    const ANNOTATIONS: Option<proxima_core::mcp::McpToolAnnotations> =
        Some(crate::mcp::WRITE_NON_IDEMPOTENT);
    const PRODUCES_SCHEMA_IDS: &'static [&'static str] = &[
        CodeExecutionPlanV1::SCHEMA_ID,
        ExecutionRequestV1::SCHEMA_ID,
        TestRequestV1::SCHEMA_ID,
    ];

    type Args = CodeEmitExecutionPlanArgs;
    type Output = CodeEmitExecutionPlanOutput;

    #[allow(clippy::too_many_lines)]
    fn call(
        ctx: ToolCtx,
        args: CodeEmitExecutionPlanArgs,
    ) -> futures::future::BoxFuture<'static, Result<CodeEmitExecutionPlanOutput, ToolError>> {
        Box::pin(async move {
            let repo_id = resolve_repo_identifier(&ctx, &args.repo_handle).await?;
            validate_repo(&ctx, repo_id).await?;
            let plan_items = validate_plan_items(args.items)?;

            let planner_root = ctx.caller_self_perspective().ok_or_else(|| {
                ToolError::InvalidInput(
                    "caller_self_perspective is required to author an execution plan".into(),
                )
            })?;
            let goal_activated_memory_id = ctx.resolve_fact_memory(&args.goal_activated_memory)?;
            let plan_source_memory_id = ctx.resolve_abstraction_memory(&args.plan_source_memory)?;
            let evidence = resolve_evidence(&ctx, &args.evidence)?;

            let pool = code_store(&ctx)?;
            let mut tx = pool.pool().begin().await.map_err(map_storage)?;
            let goal_id =
                validate_goal_activated_fact(&mut tx, &ctx, goal_activated_memory_id).await?;
            validate_active_goal_context(&mut tx, &ctx, goal_id, planner_root).await?;
            validate_plan_source_abstraction_in_owner(&mut tx, &ctx, plan_source_memory_id).await?;
            validate_evidence_in_owner(&mut tx, &ctx, &evidence).await?;

            let plan_key = match args.plan_key {
                Some(value) => normalize_text("plan_key", &value, 240)?,
                None => default_plan_key(goal_activated_memory_id, &plan_items),
            };
            let plan_summary = match args.plan_summary {
                Some(value) => normalize_text("plan_summary", &value, 4_000)?,
                None => format!("Plan with {} work/test item(s)", plan_items.len()),
            };
            let plan_payload = CodeExecutionPlanV1 {
                repo_id,
                plan_key: plan_key.clone(),
                goal_activated_memory_id: goal_activated_memory_id.into_inner(),
                summary: plan_summary.clone(),
                items: plan_items
                    .iter()
                    .map(|item| CodeExecutionPlanItemV1 {
                        key: item.key.clone(),
                        kind: match item.kind {
                            ExecutionPlanItemKind::Implementation => {
                                CodeExecutionPlanItemKind::Work
                            }
                            ExecutionPlanItemKind::Test => CodeExecutionPlanItemKind::Test,
                        },
                        title: item.title.clone(),
                        depends_on: item.depends_on.clone(),
                        request_key: item.idempotency_key.clone(),
                    })
                    .collect(),
                evidence_memory_ids: evidence.iter().map(|id| id.into_inner()).collect(),
            };
            let plan_outcome = append_execution_plan(
                &mut tx,
                &ctx,
                planner_root,
                goal_activated_memory_id,
                plan_source_memory_id,
                &evidence,
                &plan_key,
                &plan_summary,
                &plan_payload,
            )
            .await?;
            let plan_memory_id = plan_outcome.memory_id;
            let mut plan_edge_ids = plan_outcome.edge_ids;

            let mut emitted: HashMap<String, MemoryId> = HashMap::new();
            let mut outputs = Vec::with_capacity(plan_items.len());
            for item in plan_items {
                let kind = item.kind;
                let outcome = match kind {
                    ExecutionPlanItemKind::Implementation => {
                        let payload = ExecutionRequestV1 {
                            repo_id,
                            title: item.title,
                            instructions: item.instructions,
                            request_key: item.idempotency_key,
                        };
                        let outcome = ingest_execution_request(&mut tx, &ctx, &payload).await?;
                        if !outcome.idempotent_replay {
                            append_authored_edge(&mut tx, &ctx, planner_root, outcome.memory_id)
                                .await?;
                            append_derived_edge(
                                &mut tx,
                                &ctx,
                                outcome.memory_id,
                                goal_activated_memory_id,
                            )
                            .await?;
                            for memory_id in &evidence {
                                append_derived_edge(&mut tx, &ctx, outcome.memory_id, *memory_id)
                                    .await?;
                            }
                            if !item.acceptance_criteria.is_empty() {
                                let criteria_payload = AcceptanceCriteriaV1 {
                                    work_item_memory_id: outcome.memory_id.into_inner(),
                                    criteria: item.acceptance_criteria,
                                };
                                let criteria_outcome =
                                    ingest_acceptance_criteria(&mut tx, &ctx, &criteria_payload)
                                        .await?;
                                if !criteria_outcome.idempotent_replay {
                                    append_acceptance_criteria_edge(
                                        &mut tx,
                                        &ctx,
                                        outcome.memory_id,
                                        criteria_outcome.memory_id,
                                    )
                                    .await?;
                                }
                            }
                        }
                        outcome
                    }
                    ExecutionPlanItemKind::Test => {
                        let payload = TestRequestV1 {
                            repo_id,
                            title: item.title,
                            instructions: item.instructions,
                            test_key: item.idempotency_key,
                            criteria: item.test_criteria,
                        };
                        let outcome = ingest_test_request(&mut tx, &ctx, &payload).await?;
                        if !outcome.idempotent_replay {
                            append_authored_edge(&mut tx, &ctx, planner_root, outcome.memory_id)
                                .await?;
                            append_derived_edge(
                                &mut tx,
                                &ctx,
                                outcome.memory_id,
                                goal_activated_memory_id,
                            )
                            .await?;
                            for memory_id in &evidence {
                                append_derived_edge(&mut tx, &ctx, outcome.memory_id, *memory_id)
                                    .await?;
                            }
                        }
                        outcome
                    }
                };
                let edge_permit = ctx.owner_write_permit(AccessKind::Perspective).await?;
                plan_edge_ids.push(
                    append_plan_fact_evidence_edge(
                        &mut tx,
                        &ctx,
                        &edge_permit,
                        plan_memory_id,
                        outcome.memory_id,
                    )
                    .await?,
                );
                let mut dependency_edges = Vec::new();
                for dependency_key in &item.depends_on {
                    let dependency_memory_id =
                        emitted.get(dependency_key).copied().ok_or_else(|| {
                            ToolError::InvalidInput(format!(
                                "depends_on references unavailable item key: {dependency_key}"
                            ))
                        })?;
                    let edge_id = append_dependency_edge(
                        &mut tx,
                        &ctx,
                        outcome.memory_id,
                        dependency_memory_id,
                    )
                    .await?;
                    dependency_edges.push(ctx.format_edge(EdgeId::new(edge_id)));
                }
                emitted.insert(item.key.clone(), outcome.memory_id);
                outputs.push(ExecutionPlanItemOutput {
                    key: item.key,
                    kind,
                    handle: ctx.format_fact_memory(outcome.memory_id),
                    dependency_edge_handles: dependency_edges,
                    idempotent_replay: outcome.idempotent_replay,
                });
            }
            tx.commit().await.map_err(map_storage)?;
            Ok(CodeEmitExecutionPlanOutput {
                plan_handle: ctx.format_abstraction_memory(plan_memory_id),
                plan_derived_edge_handles: plan_edge_ids
                    .into_iter()
                    .map(|edge_id| ctx.format_edge(EdgeId::new(edge_id)))
                    .collect(),
                plan_idempotent_replay: plan_outcome.idempotent_replay,
                items: outputs,
            })
        })
    }
}
