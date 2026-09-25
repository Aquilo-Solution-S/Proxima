//! `EmbeddingRecipe::Never` is a claim about work that must not happen.

mod common;

/// The enqueue lane binds one list of schema ids for the linked flavors, and
/// this is what it holds. Stated as its complement because the embedding set
/// is the short one and the one a reader can check: eight schemas across both
/// flavors carry text worth a vector, and every other declaration is a
/// `Never` the lane must skip whatever kind it was registered under.
#[test]
fn only_the_text_schemas_are_embeddable() {
    let registry = common::code_registry_with_test_citations();
    let mut embeds: Vec<String> = registry
        .contracts()
        .iter()
        .flat_map(|contract| contract.schemas.iter())
        .map(|schema| schema.schema_id().as_str().to_owned())
        .filter(|schema_id| registry.schema_is_embeddable(schema_id))
        .collect();
    embeds.sort();
    embeds.dedup();
    assert_eq!(
        embeds,
        [
            "core/agent-derivation-v1",
            "core/agent-note-v1",
            "core/interpretation-v1",
            "core/utterance-v1",
            "proxima-code/code-chunk-v1",
            "proxima-code/commit-summary-v1",
            "proxima-code/commit-v1",
            "proxima-code/file-revision-v1",
        ]
    );
}
