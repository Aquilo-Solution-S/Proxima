use std::collections::HashMap;

use proxima_core::{
    AbstractionPayload, EdgeEndpoint, EntityKind, FactPayload, MemoryId, Tool, ToolCtx, ToolError,
};

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
use super::ingest::{
    FactProvenance, ingest_acceptance_criteria, ingest_execution_request, ingest_test_request,
};
use super::input_validation::{normalize_text, resolve_evidence, validate_plan_items};
use super::plan_persistence::{append_execution_plan, default_plan_key};
use super::types::{
    CodeEmitExecutionPlanArgs, CodeEmitExecutionPlanOutput, ExecutionPlanItemKind,
    ExecutionPlanItemOutput,
};

#[derive(Debug)]
pub struct CodeEmitExecutionPlanTool;

impl Tool for CodeEmitExecutionPlanTool {
    const NAME: &'static str = "proxima-code_emit_execution_plan";
    const DESCRIPTION: &'static str = "Atomically emit repo-scoped implementation/test request Facts and the execution-plan Abstraction that references them.";
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

            // Every item declares the same thing it was made from: the
            // activation Fact and the plan's evidence. One list, built once.
            let mut item_origins = Vec::with_capacity(1 + evidence.len());
            item_origins.push(EdgeEndpoint::memory(
                EntityKind::Fact,
                goal_activated_memory_id,
            ));
            item_origins.extend(
                evidence
                    .iter()
                    .map(|memory_id| EdgeEndpoint::memory(EntityKind::Fact, *memory_id)),
            );

            // Items first, plan last. The plan's payload names the request
            // Fact behind each item, and an index row cannot point at a node
            // that does not exist yet.
            let mut emitted: HashMap<String, MemoryId> = HashMap::new();
            let mut outputs = Vec::with_capacity(plan_items.len());
            let mut plan_payload_items = Vec::with_capacity(plan_items.len());
            for item in plan_items {
                let kind = item.kind;
                // The plan records the item's title and key alongside the
                // request Fact it became; the payloads below consume the
                // originals.
                let plan_title = item.title.clone();
                let plan_request_key = item.idempotency_key.clone();
                let plan_depends_on = item.depends_on.clone();
                // A dependency is a property of the depending row, so it
                // rides in the item's own payload and becomes a `reference`
                // at ingest. Ordering is already enforced by
                // `validate_plan_items`: a key may only depend on an
                // earlier one.
                let mut depends_on_memory_ids = Vec::with_capacity(item.depends_on.len());
                for dependency_key in &item.depends_on {
                    let dependency_memory_id =
                        emitted.get(dependency_key).copied().ok_or_else(|| {
                            ToolError::InvalidInput(format!(
                                "depends_on references unavailable item key: {dependency_key}"
                            ))
                        })?;
                    depends_on_memory_ids.push(dependency_memory_id.into_inner());
                }
                let provenance = FactProvenance {
                    derived_from: &item_origins,
                    authoring_perspective_id: Some(planner_root),
                };
                let outcome = match kind {
                    ExecutionPlanItemKind::Implementation => {
                        let payload = ExecutionRequestV1 {
                            repo_id,
                            title: item.title,
                            instructions: item.instructions,
                            request_key: item.idempotency_key,
                            depends_on_memory_ids,
                        };
                        let outcome =
                            ingest_execution_request(&mut tx, &ctx, &payload, provenance).await?;
                        if !outcome.idempotent_replay && !item.acceptance_criteria.is_empty() {
                            let criteria_payload = AcceptanceCriteriaV1 {
                                work_item_memory_id: outcome.memory_id.into_inner(),
                                criteria: item.acceptance_criteria,
                            };
                            ingest_acceptance_criteria(
                                &mut tx,
                                &ctx,
                                &criteria_payload,
                                FactProvenance {
                                    derived_from: &[],
                                    authoring_perspective_id: Some(planner_root),
                                },
                            )
                            .await?;
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
                            depends_on_memory_ids,
                        };
                        ingest_test_request(&mut tx, &ctx, &payload, provenance).await?
                    }
                };
                emitted.insert(item.key.clone(), outcome.memory_id);
                plan_payload_items.push(CodeExecutionPlanItemV1 {
                    key: item.key.clone(),
                    kind: match kind {
                        ExecutionPlanItemKind::Implementation => CodeExecutionPlanItemKind::Work,
                        ExecutionPlanItemKind::Test => CodeExecutionPlanItemKind::Test,
                    },
                    title: plan_title,
                    depends_on: plan_depends_on,
                    request_key: plan_request_key,
                    request_memory_id: outcome.memory_id.into_inner(),
                });
                outputs.push(ExecutionPlanItemOutput {
                    key: item.key,
                    kind,
                    handle: ctx.format_fact_memory(outcome.memory_id),
                    idempotent_replay: outcome.idempotent_replay,
                });
            }

            let plan_payload = CodeExecutionPlanV1 {
                repo_id,
                plan_key: plan_key.clone(),
                goal_activated_memory_id: goal_activated_memory_id.into_inner(),
                summary: plan_summary.clone(),
                items: plan_payload_items,
                evidence_memory_ids: evidence.iter().map(|id| id.into_inner()).collect(),
            };
            let plan_outcome = append_execution_plan(
                &mut tx,
                &ctx,
                planner_root,
                plan_source_memory_id,
                &plan_key,
                &plan_summary,
                &plan_payload,
            )
            .await?;
            tx.commit().await.map_err(map_storage)?;
            Ok(CodeEmitExecutionPlanOutput {
                plan_handle: ctx.format_abstraction_memory(plan_outcome.memory_id),
                plan_edge_count: plan_outcome.edge_count,
                plan_idempotent_replay: plan_outcome.idempotent_replay,
                items: outputs,
            })
        })
    }
}
