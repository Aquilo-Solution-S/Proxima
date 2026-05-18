use std::fs;

use proxima_code::VerificationEvidenceStatus;
use proxima_code::verification::{DeterministicCheck, run_check};

#[tokio::test]
async fn deterministic_checks_return_pass_and_fail_evidence()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    fs::write(temp.path().join("index.html"), "<h1>Signal Match</h1>\n")?;

    let pass = run_check(
        temp.path(),
        "static_entrypoint",
        &DeterministicCheck::FileExists {
            path: "index.html".into(),
        },
    )
    .await;
    assert_eq!(pass.status, VerificationEvidenceStatus::Passed);
    assert!(pass.summary.contains("index.html"));

    let fail = run_check(
        temp.path(),
        "missing_file",
        &DeterministicCheck::FileExists {
            path: "missing.html".into(),
        },
    )
    .await;
    assert_eq!(fail.status, VerificationEvidenceStatus::Failed);
    assert!(fail.summary.contains("missing.html"));

    let command = run_check(
        temp.path(),
        "grep_signal",
        &DeterministicCheck::Command {
            command: vec![
                "grep".into(),
                "-E".into(),
                "Signal Match".into(),
                "index.html".into(),
            ],
            timeout_ms: 2_000,
        },
    )
    .await;
    assert_eq!(command.status, VerificationEvidenceStatus::Passed);
    Ok(())
}
