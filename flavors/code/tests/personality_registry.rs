use proxima_core::FlavorRegistry;

#[test]
fn registry_resolves_bundled_recipes() {
    let mut registry = FlavorRegistry::new();
    proxima_code::register(&mut registry);
    let frozen = registry.freeze();

    let summary_path = frozen
        .bundled_recipe_path("proxima-code/commit_summary")
        .expect("commit_summary recipe must be registered");
    assert!(
        summary_path.exists(),
        "commit_summary recipe path {summary_path:?} must exist on disk",
    );
    assert!(
        summary_path.ends_with("recipes/commit_summary.yaml"),
        "unexpected commit_summary recipe path: {summary_path:?}",
    );

    let engineer_path = frozen
        .bundled_recipe_path("proxima-code/engineer")
        .expect("engineer recipe must be registered");
    assert!(
        engineer_path.exists(),
        "engineer recipe path {engineer_path:?} must exist on disk",
    );
    assert!(
        engineer_path.ends_with("recipes/engineer.yaml"),
        "unexpected engineer recipe path: {engineer_path:?}",
    );

    let execution_worker_path = frozen
        .bundled_recipe_path("proxima-code/execution_worker")
        .expect("execution_worker recipe must be registered");
    assert!(
        execution_worker_path.exists(),
        "execution_worker recipe path {execution_worker_path:?} must exist on disk",
    );
    assert!(
        execution_worker_path.ends_with("recipes/execution_worker.yaml"),
        "unexpected execution_worker recipe path: {execution_worker_path:?}",
    );

    let planner_path = frozen
        .bundled_recipe_path("proxima-code/plan_execution_requests")
        .expect("plan_execution_requests recipe must be registered");
    assert!(
        planner_path.exists(),
        "plan_execution_requests recipe path {planner_path:?} must exist on disk",
    );
    assert!(
        planner_path.ends_with("recipes/plan_execution_requests.yaml"),
        "unexpected plan_execution_requests recipe path: {planner_path:?}",
    );

    let verifier_path = frozen
        .bundled_recipe_path("proxima-code/verify_workspace_run")
        .expect("verify_workspace_run recipe must be registered");
    assert!(
        verifier_path.exists(),
        "verify_workspace_run recipe path {verifier_path:?} must exist on disk",
    );
    assert!(
        verifier_path.ends_with("recipes/verify_workspace_run.yaml"),
        "unexpected verify_workspace_run recipe path: {verifier_path:?}",
    );

    let correction_path = frozen
        .bundled_recipe_path("proxima-code/plan_workspace_correction")
        .expect("plan_workspace_correction recipe must be registered");
    assert!(
        correction_path.exists(),
        "plan_workspace_correction recipe path {correction_path:?} must exist on disk",
    );
    assert!(
        correction_path.ends_with("recipes/plan_workspace_correction.yaml"),
        "unexpected plan_workspace_correction recipe path: {correction_path:?}",
    );

    let close_goal_path = frozen
        .bundled_recipe_path("proxima-code/close_goal_after_merge")
        .expect("close_goal_after_merge recipe must be registered");
    assert!(
        close_goal_path.exists(),
        "close_goal_after_merge recipe path {close_goal_path:?} must exist on disk",
    );
    assert!(
        close_goal_path.ends_with("recipes/close_goal_after_merge.yaml"),
        "unexpected close_goal_after_merge recipe path: {close_goal_path:?}",
    );

    assert_eq!(
        frozen.bundled_recipes_for("proxima-code"),
        vec![
            "proxima-code/commit_summary",
            "proxima-code/engineer",
            "proxima-code/execution_worker",
            "proxima-code/verify_workspace_run",
            "proxima-code/plan_workspace_correction",
            "proxima-code/close_goal_after_merge",
            "proxima-code/plan_execution_requests",
        ],
    );
}
