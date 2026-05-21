use super::*;

impl DemoWorld {
    pub(super) async fn new(cfg: DemoConfig) -> Result<Self, Box<dyn std::error::Error>> {
        std::fs::create_dir_all(&cfg.run_dir)?;
        let db_name = format!("proxima_demo_wheel_{}", Uuid::now_v7().simple());
        create_db(&db_name).await?;
        let pg = PgStorage::connect(&db_url(&db_name)).await?;
        if let Err(err) = async {
            pg.run_migrations().await?;
            proxima_code::migrator().run(pg.pool()).await?;
            proxima_flavor_goal::migrator().run(pg.pool()).await?;
            proxima_flavor_intent::migrator().run(pg.pool()).await?;
            Ok::<(), Box<dyn std::error::Error>>(())
        }
        .await
        {
            drop(pg);
            let _ = drop_db(&db_name).await;
            return Err(err);
        }

        let owner = Owner {
            principal: Principal::User(UserId::new(Uuid::now_v7())),
            org_id: OrgId::new(Uuid::now_v7()),
        };
        let engine = Arc::new(build_demo_engine(&cfg, pg.clone(), owner.clone()));
        engine
            .set_mcp_url("http://127.0.0.1:1/mcp".to_string())
            .await;
        let server = McpToolHost::from_pool(pg.pool().clone(), owner.clone(), registry())
            .with_engine(engine.clone());
        let harness = Arc::new(HarnessLoop::new(engine.clone(), Arc::new(server.clone())));
        engine.set_target_adapter(harness.clone()).await;

        let world = Self {
            cfg,
            db_name,
            pg,
            owner,
            engine,
            server,
            harness,
            role_ids: BTreeMap::new(),
            goal_id: None,
        };
        world.configure_runtime().await?;
        Ok(world)
    }

    pub(super) async fn run(&mut self, started: Instant) -> Result<(), Box<dyn std::error::Error>> {
        let repo_id = prepare_demo_repo(&self.cfg.repo_path, self.cfg.challenge).await?;
        register_repo(
            self.pg.pool(),
            &self.owner,
            repo_id,
            self.cfg.repo_path.to_str().ok_or("repo path is not utf8")?,
            self.cfg.challenge.repo_handle(),
        )
        .await?;
        if self.cfg.planner_mode == DemoPlannerMode::VisionDocument {
            return self.run_vision_document_only(started).await;
        }

        let visionary = self
            .instantiate(
                "Visionary",
                "Interpret ambiguous goals into product intent and quality bars",
            )
            .await?;
        let planner = self
            .instantiate("Planner", "Emit execution requests for active goals")
            .await?;
        let worker = self
            .instantiate("Worker", "Run workspace edits for execution requests")
            .await?;
        let verifier = self
            .instantiate("Verifier", "Review workspace runs against the goal")
            .await?;
        let reviewer = self
            .instantiate(
                "Goal-Reviewer",
                "Close achieved goals or request corrections",
            )
            .await?;
        let wake_supervisor = self
            .instantiate(
                "Wake Supervisor",
                "Decide whether max-round wake truncations need intervention",
            )
            .await?;
        self.set_wake_supervisor_wake(wake_supervisor).await?;
        let intervention_policy = demo_intervention_policy(
            wake_supervisor,
            self.cfg.intervention_mode,
            self.cfg.planner_mode,
        );

        self.set_single_wake(
            visionary,
            "Visionary",
            WakeEntryTriggerKind::OnMemory,
            "proxima-goal/goal-activated-v1",
            WakeExecutionMode::SubstrateOnly,
            vec![
                "core/fetch_memory",
                "core/search_memories",
                "core/list_active_goals",
                "core/emit_abstraction",
            ],
            Vec::new(),
            visionary_instruction(
                self.cfg.challenge,
                self.cfg.intervention_mode,
                self.cfg.planner_mode,
            ),
            WakeOptions {
                goal_scope: WakeEntryGoalScope::TriggerGoalAssigned,
                authored_by: WakeEntryAuthoredBy::Any,
                intervention_policy: Some(intervention_policy.clone()),
                ..WakeOptions::default_with_rounds(self.cfg.role_max_rounds.visionary)
            },
        )
        .await?;
        self.set_planner_wakes(planner, intervention_policy.clone())
            .await?;
        self.pg
            .set_read_scope(&SetReadScopeRequest {
                owner: self.owner.clone(),
                reader_personality_instance_id: planner,
                readable_personality_instance_ids: vec![visionary],
            })
            .await?;
        let worker_substrate_tools = if self.cfg.intervention_mode == DemoInterventionMode::ForceContinue {
            vec!["core/fetch_memory"]
        } else {
            Vec::new()
        };
        self.set_single_wake(
            worker,
            "Worker",
            WakeEntryTriggerKind::OnMemory,
            ExecutionRequestV1::SCHEMA_ID,
            WakeExecutionMode::Workspace,
            worker_substrate_tools,
            vec![
                "proxima-workspace/text_editor",
                "proxima-workspace/shell",
                "proxima-workspace/list_files",
            ],
            worker_instruction(
                self.cfg.challenge,
                self.cfg.planner_mode,
                self.cfg.intervention_mode,
            ),
            WakeOptions {
                intervention_policy: Some(intervention_policy.clone()),
                ..WakeOptions::default_with_rounds(self.cfg.role_max_rounds.worker)
            },
        )
        .await?;
        let mut verifier_substrate_tools = vec![
            "proxima-code/code_emit_verification_evidence",
            "proxima-code/code_emit_workspace_review",
        ];
        if self.cfg.intervention_mode == DemoInterventionMode::ForceContinue {
            verifier_substrate_tools.push("core/fetch_memory");
        }
        let verifier_trigger_schema = if self.cfg.planner_mode == DemoPlannerMode::Real {
            TestRequestV1::SCHEMA_ID
        } else {
            CoreWorkspaceRunV1::SCHEMA_ID
        };
        self.set_single_wake(
            verifier,
            "Verifier",
            WakeEntryTriggerKind::OnMemory,
            verifier_trigger_schema,
            WakeExecutionMode::Workspace,
            verifier_substrate_tools,
            vec![
                "proxima-workspace/text_editor",
                "proxima-workspace/shell",
                "proxima-workspace/list_files",
            ],
            verifier_instruction(
                self.cfg.challenge,
                self.cfg.planner_mode,
                self.cfg.intervention_mode,
            ),
            WakeOptions {
                intervention_policy: Some(intervention_policy.clone()),
                ..WakeOptions::default_with_rounds(self.cfg.role_max_rounds.verifier)
            },
        )
        .await?;

        let (_goal_memory, active_goal) = self.activate_goal(visionary).await?;
        self.goal_id = Some(active_goal);
        if self.cfg.planner_mode == DemoPlannerMode::Real {
            self.append_goal_assignment(active_goal, planner).await?;
        }
        self.append_goal_assignment(active_goal, reviewer).await?;
        self.set_goal_reviewer_wakes(reviewer, intervention_policy)
            .await?;

        let mut ticks = 0_u32;
        while ticks < self.cfg.max_ticks {
            ticks += 1;
            let fired = self.engine.run_dispatcher_tick().await?;
            let correction_loops = self.correction_loop_count().await?;
            if let Some(max_seconds) = self.cfg.max_wall_clock_seconds
                && started.elapsed() >= Duration::from_secs(max_seconds)
            {
                let mut metrics = self.collect_metrics(started, ticks).await?;
                self.write_outputs(&mut metrics).await?;
                return Err(self
                    .failure_report("max wall clock seconds exceeded")
                    .await?
                    .into());
            }
            if self.cfg.intervention_mode == DemoInterventionMode::ForceContinue
                && self
                    .forced_continuation_checks()
                    .await?
                    .values()
                    .all(|value| *value)
            {
                let mut metrics = self.collect_metrics(started, ticks).await?;
                self.write_outputs(&mut metrics).await?;
                return Ok(());
            }
            if self.cfg.intervention_mode == DemoInterventionMode::Normal
                && self.demo_goal_graph_complete().await?
            {
                let mut metrics = self.collect_metrics(started, ticks).await?;
                if !metrics.overall_pass {
                    self.write_outputs(&mut metrics).await?;
                    return Err(self.failure_report("final checks failed").await?.into());
                }
                metrics.auto_merge = Some(self.auto_merge_successful_worktree().await?);
                self.write_outputs(&mut metrics).await?;
                return Ok(());
            }
            if correction_loops > self.cfg.max_correction_loops {
                let mut metrics = self.collect_metrics(started, ticks).await?;
                self.write_outputs(&mut metrics).await?;
                return Err(self
                    .failure_report("max correction loops exceeded")
                    .await?
                    .into());
            }
            if fired == 0 {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }

        let mut metrics = self.collect_metrics(started, ticks).await?;
        self.write_outputs(&mut metrics).await?;
        Err(self
            .failure_report("max dispatcher ticks exceeded")
            .await?
            .into())
    }

    async fn run_vision_document_only(
        &mut self,
        started: Instant,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let visionary = self
            .instantiate(
                "Visionary",
                "Interpret ambiguous goals into a reviewable vision document",
            )
            .await?;
        let planner = self
            .instantiate(
                "Planner",
                "Write a workspace plan artifact and emit execution requests",
            )
            .await?;
        let worker = self
            .instantiate("Worker", "Implement execution requests")
            .await?;
        let tester = self
            .instantiate("Tester", "Verify planned test requests and record evidence")
            .await?;
        let wake_supervisor = self
            .instantiate(
                "Wake Supervisor",
                "Decide whether max-round wake truncations need intervention",
            )
            .await?;
        self.set_wake_supervisor_wake(wake_supervisor).await?;
        let intervention_policy = demo_intervention_policy(
            wake_supervisor,
            self.cfg.intervention_mode,
            self.cfg.planner_mode,
        );
        self.set_single_wake(
            visionary,
            "Visionary",
            WakeEntryTriggerKind::OnMemory,
            "proxima-goal/goal-activated-v1",
            WakeExecutionMode::Workspace,
            if self.cfg.intervention_mode == DemoInterventionMode::ForceContinue {
                vec!["core/emit_abstraction", "core/fetch_memory"]
            } else {
                vec!["core/emit_abstraction"]
            },
            vec!["core-workspace/text_editor", "core-workspace/list_files"],
            visionary_instruction(
                self.cfg.challenge,
                self.cfg.intervention_mode,
                self.cfg.planner_mode,
            ),
            WakeOptions {
                goal_scope: WakeEntryGoalScope::TriggerGoalAssigned,
                authored_by: WakeEntryAuthoredBy::Any,
                intervention_policy: Some(intervention_policy.clone()),
                workspace_binding: Some(WakeWorkspaceBinding::GitWorktree {
                    repo_path: self.cfg.repo_path.to_string_lossy().to_string(),
                    base_ref: "HEAD".into(),
                    finalize: WakeWorkspaceFinalize::CommitAll,
                    worktrees_root: Some(
                        self.cfg
                            .run_dir
                            .join("core-worktrees")
                            .to_string_lossy()
                            .to_string(),
                    ),
                }),
                ..WakeOptions::default_with_rounds(self.cfg.role_max_rounds.visionary)
            },
        )
        .await?;
        self.set_single_wake(
            planner,
            "Planner",
            WakeEntryTriggerKind::OnMemory,
            VisionBriefV1::SCHEMA_ID,
            WakeExecutionMode::Workspace,
            vec![
                "core/walk_lineage",
                "core/fetch_memory",
                "proxima-code/code_emit_execution_plan",
            ],
            vec!["core-workspace/text_editor", "core-workspace/list_files"],
            planner_instruction(
                planner,
                self.cfg.challenge,
                self.cfg.planner_mode,
                self.cfg.intervention_mode,
            ),
            WakeOptions {
                authored_by: WakeEntryAuthoredBy::Any,
                intervention_policy: Some(intervention_policy.clone()),
                workspace_binding: Some(WakeWorkspaceBinding::GitWorktree {
                    repo_path: self.cfg.repo_path.to_string_lossy().to_string(),
                    base_ref: "HEAD".into(),
                    finalize: WakeWorkspaceFinalize::CommitAll,
                    worktrees_root: Some(
                        self.cfg
                            .run_dir
                            .join("core-worktrees")
                            .to_string_lossy()
                            .to_string(),
                    ),
                }),
                ..WakeOptions::default_with_rounds(self.cfg.role_max_rounds.planner)
            },
        )
        .await?;
        let worker_substrate_tools = if self.cfg.intervention_mode == DemoInterventionMode::ForceContinue {
            vec!["core/fetch_memory"]
        } else {
            Vec::new()
        };
        self.set_single_wake(
            worker,
            "Worker",
            WakeEntryTriggerKind::OnMemory,
            ExecutionRequestV1::SCHEMA_ID,
            WakeExecutionMode::Workspace,
            worker_substrate_tools,
            vec![
                "proxima-workspace/text_editor",
                "proxima-workspace/shell",
                "proxima-workspace/list_files",
            ],
            worker_instruction(
                self.cfg.challenge,
                self.cfg.planner_mode,
                self.cfg.intervention_mode,
            ),
            WakeOptions {
                intervention_policy: Some(intervention_policy.clone()),
                ..WakeOptions::default_with_rounds(self.cfg.role_max_rounds.worker)
            },
        )
        .await?;
        let mut tester_substrate_tools = vec!["proxima-code/code_emit_verification_evidence"];
        if self.cfg.intervention_mode == DemoInterventionMode::ForceContinue {
            tester_substrate_tools.push("core/fetch_memory");
        }
        self.set_single_wake(
            tester,
            "Tester",
            WakeEntryTriggerKind::OnMemory,
            TestRequestV1::SCHEMA_ID,
            WakeExecutionMode::Workspace,
            tester_substrate_tools,
            vec!["proxima-workspace/shell", "proxima-workspace/list_files"],
            tester_instruction(self.cfg.intervention_mode),
            WakeOptions {
                intervention_policy: Some(intervention_policy),
                ..WakeOptions::default_with_rounds(self.cfg.role_max_rounds.verifier)
            },
        )
        .await?;
        self.pg
            .set_read_scope(&SetReadScopeRequest {
                owner: self.owner.clone(),
                reader_personality_instance_id: planner,
                readable_personality_instance_ids: vec![visionary],
            })
            .await?;

        let (_goal_memory, active_goal) = self.activate_goal(visionary).await?;
        self.goal_id = Some(active_goal);
        self.append_goal_assignment(active_goal, planner).await?;

        let mut ticks = 0_u32;
        while ticks < self.cfg.max_ticks {
            ticks += 1;
            let fired = self.engine.run_dispatcher_tick().await?;
            if let Some(max_seconds) = self.cfg.max_wall_clock_seconds
                && started.elapsed() >= Duration::from_secs(max_seconds)
            {
                let mut metrics = self.collect_metrics(started, ticks).await?;
                self.write_outputs(&mut metrics).await?;
                return Err(self
                    .failure_report("max wall clock seconds exceeded")
                    .await?
                    .into());
            }
            let mut metrics = self.collect_metrics(started, ticks).await?;
            if metrics.overall_pass {
                self.write_outputs(&mut metrics).await?;
                return Ok(());
            }
            if fired == 0 {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }

        let mut metrics = self.collect_metrics(started, ticks).await?;
        self.write_outputs(&mut metrics).await?;
        Err(self
            .failure_report("max dispatcher ticks exceeded")
            .await?
            .into())
    }

    pub(super) async fn cleanup(self) -> Result<(), sqlx::Error> {
        let DemoWorld {
            db_name,
            pg,
            engine,
            server,
            harness,
            ..
        } = self;
        drop(server);
        drop(engine);
        drop(harness);
        drop(pg);
        drop_db(&db_name).await
    }
}
