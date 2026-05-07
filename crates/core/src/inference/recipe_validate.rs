//! Wraps `goose recipe validate <path>` as a tokio subprocess.

use std::path::Path;
use std::time::Duration;

use thiserror::Error;
use tokio::process::Command;
use tokio::time::timeout;

#[derive(Debug, Error)]
pub enum RecipeValidateError {
    #[error("goose CLI not on PATH")]
    Unavailable,
    #[error("goose recipe validate timed out after {0:?}")]
    Timeout(Duration),
    #[error("goose recipe validate failed: {stderr}")]
    Invalid { stderr: String },
    #[error("io error invoking goose: {0}")]
    Io(#[from] std::io::Error),
}

const VALIDATE_TIMEOUT: Duration = Duration::from_secs(5);

pub async fn validate_recipe(path: &Path) -> Result<(), RecipeValidateError> {
    let mut cmd = Command::new("goose");
    cmd.arg("recipe")
        .arg("validate")
        .arg(path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());

    let spawn = match cmd.spawn() {
        Ok(child) => child,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(RecipeValidateError::Unavailable);
        }
        Err(e) => return Err(RecipeValidateError::Io(e)),
    };

    let output = match timeout(VALIDATE_TIMEOUT, spawn.wait_with_output()).await {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => return Err(RecipeValidateError::Io(e)),
        Err(_) => return Err(RecipeValidateError::Timeout(VALIDATE_TIMEOUT)),
    };

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        Err(RecipeValidateError::Invalid { stderr })
    }
}
