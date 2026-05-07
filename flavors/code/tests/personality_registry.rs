use proxima_code::{
    CodeCommitSummarizerSelfV1, CodeEngineerPersonality, CodeEngineerSelfV1,
    CommitSummaryPersonality, CommitSummaryV1,
};
use proxima_core::{
    AbstractionPayload, FlavorRegistry, ModelTier, OrgId, Owner, PersonalityFlavor,
    PerspectivePayload, Principal, SchemaId, UserId,
};
use uuid::Uuid;

fn owner_fixture() -> Owner {
    Owner {
        principal: Principal::User(UserId::new(Uuid::now_v7())),
        org_id: OrgId::new(Uuid::now_v7()),
    }
}

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
fn commit_summary_personality_preserves_operator_surface() {
    let personality = CommitSummaryPersonality;
    assert_eq!(
        personality.writeable_schemas(),
        &[CommitSummaryV1::SCHEMA_ID]
    );
    assert_eq!(personality.writeable_relations(), &[] as &[&str]);
    assert_eq!(personality.tier(), ModelTier::Fast);
    let draft = personality
        .default_self_payload(&owner_fixture(), None)
        .expect("default self payload");
    assert_eq!(
        draft.schema_id.as_str(),
        CodeCommitSummarizerSelfV1::SCHEMA_ID
    );
    assert_eq!(draft.text, "Commit Summarizer");
}

#[test]
fn engineer_personality_preserves_operator_surface() {
    let personality = CodeEngineerPersonality;
    assert_eq!(
        personality.writeable_schemas(),
        &[proxima_code::CodeDevelopmentPerspectiveV1::SCHEMA_ID]
    );
    assert_eq!(personality.writeable_relations(), &[] as &[&str]);
    assert_eq!(personality.tier(), ModelTier::Standard);
    let draft = personality
        .default_self_payload(
            &owner_fixture(),
            Some(&serde_json::json!({
                "display_name": "Engineer B",
                "purpose": "review risky changes",
            })),
        )
        .expect("default self payload");
    assert_eq!(draft.schema_id.as_str(), CodeEngineerSelfV1::SCHEMA_ID);
    assert_eq!(draft.text, "Engineer B");
}
