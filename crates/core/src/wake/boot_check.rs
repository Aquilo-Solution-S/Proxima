use std::path::{Path, PathBuf};

use thiserror::Error;
use tokio::process::Command;

#[derive(Debug, Clone)]
pub struct GooseInfo {
    pub binary: PathBuf,
    pub version: String,
}

#[derive(Debug, Error)]
pub enum BootCheckError {
    #[error("goose binary not executable at {path:?}: {source}")]
    BinaryMissing {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("goose --version exited non-zero: {stderr}")]
    VersionFailed { stderr: String },
    #[error("goose --version stdout was not utf8")]
    BadStdout,
}

pub async fn verify_goose_on_path(binary: &Path) -> Result<GooseInfo, BootCheckError> {
    let output = Command::new(binary)
        .arg("--version")
        .output()
        .await
        .map_err(|e| BootCheckError::BinaryMissing {
            path: binary.to_path_buf(),
            source: e,
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        return Err(BootCheckError::VersionFailed { stderr });
    }
    let version = String::from_utf8(output.stdout)
        .map_err(|_| BootCheckError::BadStdout)?
        .trim()
        .to_string();
    Ok(GooseInfo {
        binary: binary.to_path_buf(),
        version,
    })
}
