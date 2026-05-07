use proxima_core::wake::boot_check::{verify_goose_on_path, BootCheckError};
use std::path::PathBuf;

fn goose_on_path() -> Option<PathBuf> {
    which::which("goose").ok()
}

#[tokio::test]
async fn verify_goose_succeeds_when_binary_present() {
    let Some(goose) = goose_on_path() else {
        eprintln!("skipping: goose not on PATH");
        return;
    };
    let info = verify_goose_on_path(&goose).await.expect("goose ok");
    assert!(!info.version.is_empty());
}

#[tokio::test]
async fn verify_goose_fails_when_binary_absent() {
    let bogus = PathBuf::from("/nonexistent/goose");
    let err = verify_goose_on_path(&bogus).await.expect_err("must fail");
    assert!(matches!(err, BootCheckError::BinaryMissing { .. }));
}
