use super::*;

impl DemoWorld {
    pub(super) async fn configure_runtime(&self) -> Result<(), Box<dyn std::error::Error>> {
        self.pg
            .register_inference_target(&RegisterInferenceTargetRequest {
                owner: self.owner.clone(),
                target_ref: TARGET_REF.into(),
                config: InferenceTargetConfig::MistralChat(MistralChatConfig {
                    base_url: self.cfg.base_url.clone(),
                    model_id: MODEL_ID.into(),
                    api_key_env: self.cfg.api_key_env.clone(),
                    temperature: Some(0.2),
                    max_completion_tokens: None,
                    reasoning_effort: None,
                }),
            })
            .await?;
        for tier in [ModelTier::Fast, ModelTier::Standard, ModelTier::Deep] {
            self.pg
                .bind_inference_tier(&BindInferenceTierRequest {
                    owner: self.owner.clone(),
                    tier,
                    target_ref: TARGET_REF.into(),
                })
                .await?;
        }
        self.pg
            .register_embedding_model(EmbeddingModel {
                vendor: EMBED_VENDOR.into(),
                model_id: EMBED_MODEL.into(),
                base_url: "http://localhost:11434".into(),
                caps: EmbedCaps {
                    dim: 4096,
                    matryoshka: false,
                },
                secret_ref: None,
            })
            .await?;
        self.pg
            .set_embedding_active(EMBED_VENDOR, EMBED_MODEL)
            .await?;
        Ok(())
    }

    pub(super) async fn instantiate(
        &mut self,
        role: &str,
        purpose: &str,
    ) -> Result<PersonalityInstanceId, Box<dyn std::error::Error>> {
        let inst = self
            .engine
            .instantiate_personality(InstantiatePersonalityRequest {
                owner: self.owner.clone(),
                display_name: role.into(),
                purpose: purpose.into(),
            })
            .await?;
        self.role_ids.insert(role.into(), inst.instance_id);
        Ok(inst.instance_id)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn set_single_wake(
        &self,
        instance_id: PersonalityInstanceId,
        role: &str,
        trigger_kind: WakeEntryTriggerKind,
        trigger_id: &str,
        execution_mode: WakeExecutionMode,
        substrate_palette: Vec<&str>,
        workspace_palette: Vec<&str>,
        instructions: String,
        options: WakeOptions,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut wake = WakeEntryDraft::new(
            Uuid::now_v7(),
            instance_id,
            trigger_kind,
            trigger_id,
            format!("{role} demo wake"),
            options.authored_by,
            1000,
            ModelTier::Standard,
            Some(TARGET_REF.into()),
            substrate_palette.into_iter().map(str::to_string).collect(),
            options.max_rounds,
        )?;
        wake.execution_mode = execution_mode;
        wake.goal_scope = options.goal_scope;
        wake.instructions = instructions;
        wake.workspace_tool_palette = workspace_palette.into_iter().map(str::to_string).collect();
        wake.workspace_binding = options.workspace_binding;
        wake.intervention_policy = options.intervention_policy;
        self.engine
            .set_wake_entries(
                &Credentials::None,
                &SetWakeEntriesRequest {
                    owner: self.owner.clone(),
                    personality_instance_id: instance_id,
                    entries: vec![wake],
                },
            )
            .await?;
        Ok(())
    }

    pub(super) async fn set_planner_wakes(
        &self,
        planner: PersonalityInstanceId,
        intervention_policy: InterventionPolicy,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let planner_tools = if matches!(
            self.cfg.planner_mode,
            DemoPlannerMode::Real | DemoPlannerMode::VisionDocument
        ) {
            vec![
                "core/walk_lineage".into(),
                "core/fetch_memory".into(),
                "core/search_memories".into(),
                "proxima-code/code_emit_execution_plan".into(),
            ]
        } else {
            vec![
                "core/walk_lineage".into(),
                "core/fetch_memory".into(),
                "core/search_memories".into(),
                "proxima-goal/goal_decompose".into(),
                "proxima-code/code_emit_execution_request".into(),
            ]
        };
        let mut vision_wake = WakeEntryDraft::new(
            Uuid::now_v7(),
            planner,
            WakeEntryTriggerKind::OnMemory,
            VisionBriefV1::SCHEMA_ID,
            "Planner vision-brief demo wake",
            WakeEntryAuthoredBy::Any,
            1000,
            ModelTier::Standard,
            Some(TARGET_REF.into()),
            planner_tools,
            self.cfg.role_max_rounds.planner,
        )?;
        vision_wake.instructions =
            planner_instruction(
                planner,
                self.cfg.challenge,
                self.cfg.planner_mode,
                self.cfg.intervention_mode,
            );
        vision_wake.intervention_policy = Some(intervention_policy.clone());

        let mut child_goal_wake = WakeEntryDraft::new(
            Uuid::now_v7(),
            planner,
            WakeEntryTriggerKind::OnMemory,
            "proxima-goal/goal-activated-v1",
            "Planner child-goal demo wake",
            WakeEntryAuthoredBy::Any,
            1000,
            ModelTier::Standard,
            Some(TARGET_REF.into()),
            vec![
                "proxima-goal/goal_decompose".into(),
                "proxima-code/code_emit_execution_request".into(),
            ],
            self.cfg.role_max_rounds.planner,
        )?;
        child_goal_wake.goal_scope = WakeEntryGoalScope::TriggerGoalAssigned;
        child_goal_wake.instructions =
            planner_instruction(
                planner,
                self.cfg.challenge,
                self.cfg.planner_mode,
                self.cfg.intervention_mode,
            );
        child_goal_wake.intervention_policy = Some(intervention_policy);

        let entries = if self.cfg.planner_mode == DemoPlannerMode::Real {
            vec![vision_wake]
        } else {
            vec![vision_wake, child_goal_wake]
        };
        self.engine
            .set_wake_entries(
                &Credentials::None,
                &SetWakeEntriesRequest {
                    owner: self.owner.clone(),
                    personality_instance_id: planner,
                    entries,
                },
            )
            .await?;
        Ok(())
    }

    pub(super) async fn set_wake_supervisor_wake(
        &self,
        wake_supervisor: PersonalityInstanceId,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut wake = WakeEntryDraft::new(
            Uuid::now_v7(),
            wake_supervisor,
            WakeEntryTriggerKind::OnMemory,
            InterventionRequestedV1::SCHEMA_ID,
            "Wake Supervisor demo wake",
            WakeEntryAuthoredBy::Any,
            1000,
            ModelTier::Standard,
            Some(TARGET_REF.into()),
            vec!["core/emit_intervention_decision".into()],
            self.cfg.role_max_rounds.wake_supervisor,
        )?;
        wake.instructions = wake_supervisor_instruction(self.cfg.intervention_mode);
        self.engine
            .set_wake_entries(
                &Credentials::None,
                &SetWakeEntriesRequest {
                    owner: self.owner.clone(),
                    personality_instance_id: wake_supervisor,
                    entries: vec![wake],
                },
            )
            .await?;
        Ok(())
    }

    pub(super) async fn set_goal_reviewer_wakes(
        &self,
        reviewer: PersonalityInstanceId,
        intervention_policy: InterventionPolicy,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut review_tools = vec![
            "proxima-code/code_goal_completion_status".into(),
            "proxima-goal/goal_mark_achieved".into(),
            "proxima-code/code_emit_correction_execution_request".into(),
        ];
        if self.cfg.intervention_mode == DemoInterventionMode::ForceContinue {
            review_tools.push("core/fetch_memory".into());
        }
        let mut review_wake = WakeEntryDraft::new(
            Uuid::now_v7(),
            reviewer,
            WakeEntryTriggerKind::OnMemory,
            WorkspaceReviewV1::SCHEMA_ID,
            "Goal-Reviewer demo wake",
            WakeEntryAuthoredBy::Any,
            1000,
            ModelTier::Deep,
            Some(TARGET_REF.into()),
            review_tools,
            self.cfg.role_max_rounds.goal_reviewer,
        )?;
        review_wake.instructions = goal_reviewer_instruction(self.cfg.intervention_mode);
        review_wake.intervention_policy = Some(intervention_policy);

        let mut target_validation_wake = WakeEntryDraft::new(
            Uuid::now_v7(),
            reviewer,
            WakeEntryTriggerKind::OnMemory,
            ExecutionRequestV1::SCHEMA_ID,
            "Goal-Reviewer target validation wake",
            WakeEntryAuthoredBy::SelfAuthor,
            1000,
            ModelTier::Standard,
            Some(TARGET_REF.into()),
            Vec::new(),
            1,
        )?;
        target_validation_wake.execution_mode = WakeExecutionMode::Workspace;
        target_validation_wake.instructions = "Target validation only. Stop.".into();

        self.engine
            .set_wake_entries(
                &Credentials::None,
                &SetWakeEntriesRequest {
                    owner: self.owner.clone(),
                    personality_instance_id: reviewer,
                    entries: vec![review_wake, target_validation_wake],
                },
            )
            .await?;
        Ok(())
    }

    pub(super) async fn activate_goal(
        &self,
        planner: PersonalityInstanceId,
    ) -> Result<(MemoryId, GoalId), Box<dyn std::error::Error>> {
        let proposed = self
            .server
            .call_tool(
                "proxima-goal/goal_propose",
                json!({
                    "payload": {
                        "schema_id": "proxima-goal/simple-text-v1",
                        "body": {
                            "title": self.cfg.challenge.goal_title(),
                            "text": self.cfg.challenge.goal_text()
                        }
                    },
                    "target_personality": planner.into_inner().to_string(),
                    "evidence": [],
                    "idempotency_key": format!("demo-{}-propose", self.cfg.challenge.repo_handle())
                }),
                setup_author(),
                None,
            )
            .await?;
        let proposal = proposed
            .get("handle")
            .and_then(serde_json::Value::as_str)
            .ok_or("missing proposal handle")?;
        self.server
            .call_tool(
                "proxima-goal/goal_accept",
                json!({
                    "proposal": proposal,
                    "target_personality": planner.into_inner().to_string(),
                    "idempotency_key": format!("demo-{}-accept", self.cfg.challenge.repo_handle())
                }),
                setup_author(),
                None,
            )
            .await?;
        let row = sqlx::query(
            "SELECT memory_id, goal_id
             FROM proxima_goal.goal_activated_v1
             WHERE title = $1
             ORDER BY accepted_at DESC
             LIMIT 1",
        )
        .bind(self.cfg.challenge.goal_title())
        .fetch_one(self.pg.pool())
        .await?;
        Ok((
            MemoryId::new(row.try_get("memory_id")?),
            GoalId::new(row.try_get("goal_id")?),
        ))
    }

    pub(super) async fn append_goal_assignment(
        &self,
        goal: GoalId,
        instance_id: PersonalityInstanceId,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let root = self
            .pg
            .fetch_personality_runtime(&self.owner, instance_id)
            .await?
            .ok_or("personality runtime missing")?
            .current_root_perspective_memory_id;
        let relation = self
            .engine
            .registry()
            .resolve_relation(CORE_INSPIRES_RELATION)
            .ok_or("core/inspires not registered")?;
        let mut tx = self.pg.pool().begin().await?;
        append_edge_in_tx(
            &mut tx,
            &EdgeDraft {
                edge_id: Uuid::now_v7(),
                relation,
                source_kind: EntityKind::Goal,
                source_memory_id: None,
                source_goal_id: Some(goal.into_inner()),
                target_kind: EntityKind::Perspective,
                target_memory_id: Some(root.into_inner()),
                target_goal_id: None,
                authorship_kind: EdgeAuthorshipKind::User,
                authorship_owner_memory_id: None,
                owner: &self.owner,
            },
            None,
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }
}
