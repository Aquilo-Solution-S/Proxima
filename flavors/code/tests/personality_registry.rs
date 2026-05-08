use proxima_code::{
    CodeCommitSummarizerSelfV1, CodeEngineerPersonality, CodeEngineerSelfV1,
    CommitSummaryPersonality,
};
use proxima_core::{FlavorRegistry, PersonalityFlavor, PerspectivePayload, SchemaId};

#[test]
fn code_flavor_registers_personalities_and_self_schemas() {
    let mut registry = FlavorRegistry::new();
    proxima_code::register(&mut registry);
    let frozen = registry.freeze();

    let personalities: Vec<_> = frozen
        .list_personalities()
        .iter()
        .map(|p| p.personality_type_id())
        .collect();
    assert_eq!(
        personalities,
        vec!["proxima-code/commit-summary-v1", "proxima-code/engineer-v1",]
    );

    let schemas: std::collections::HashSet<_> = frozen
        .list()
        .into_iter()
        .map(|schema| schema.schema_id)
        .collect();
    assert!(schemas.contains(&SchemaId::new(CodeCommitSummarizerSelfV1::SCHEMA_ID.into(),)));
    assert!(schemas.contains(&SchemaId::new(CodeEngineerSelfV1::SCHEMA_ID.into())));
}

#[test]
fn commit_summary_personality_exposes_default_metadata() {
    let personality = CommitSummaryPersonality;
    assert_eq!(personality.default_display_name(), "Commit Summarizer");
    assert_eq!(
        personality.default_purpose(),
        "Summarize commits as Abstractions"
    );
}

#[test]
fn engineer_personality_exposes_default_metadata() {
    let personality = CodeEngineerPersonality;
    assert_eq!(personality.default_display_name(), "Engineer");
    assert_eq!(
        personality.default_purpose(),
        "Develop perspectives on code changes"
    );
}

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
