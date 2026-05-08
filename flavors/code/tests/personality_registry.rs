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

    assert_eq!(
        frozen.bundled_recipes_for("proxima-code"),
        vec!["proxima-code/commit_summary", "proxima-code/engineer"],
    );
}
