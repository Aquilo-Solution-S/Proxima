//! Recipe-validate subprocess wrapper.
//! Skipped when `goose` is not on PATH.

use std::path::PathBuf;

use proxima_core::inference::recipe_validate::{RecipeValidateError, validate_recipe};

fn goose_on_path() -> bool {
    which::which("goose").is_ok()
}

#[tokio::test]
async fn validate_recipe_returns_ok_for_a_minimal_valid_yaml() {
    if !goose_on_path() {
        eprintln!("skipping: goose not on PATH");
        return;
    }
    let tmp = tempfile::Builder::new().suffix(".yaml").tempfile().unwrap();
    std::fs::write(
        tmp.path(),
        r#"version: 1.0.0
title: smoke
description: smoke
instructions: |
  Just say hi.
"#,
    )
    .unwrap();
    let path: PathBuf = tmp.path().to_path_buf();
    validate_recipe(&path)
        .await
        .expect("a minimal recipe should validate");
}

#[tokio::test]
async fn validate_recipe_returns_invalid_for_broken_yaml() {
    if !goose_on_path() {
        eprintln!("skipping: goose not on PATH");
        return;
    }
    let tmp = tempfile::Builder::new().suffix(".yaml").tempfile().unwrap();
    std::fs::write(tmp.path(), "{ this is not valid yaml }").unwrap();
    let err = validate_recipe(tmp.path())
        .await
        .expect_err("broken recipe must fail");
    assert!(matches!(err, RecipeValidateError::Invalid { .. }));
}

#[tokio::test]
async fn validate_recipe_returns_unavailable_when_goose_absent() {
    if goose_on_path() {
        eprintln!("skipping: goose IS on PATH");
        return;
    }
    let tmp = tempfile::Builder::new().suffix(".yaml").tempfile().unwrap();
    std::fs::write(tmp.path(), "version: 1.0.0").unwrap();
    let err = validate_recipe(tmp.path()).await.expect_err("must error");
    assert!(matches!(err, RecipeValidateError::Unavailable));
}
