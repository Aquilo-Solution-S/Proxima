use std::path::PathBuf;
use std::time::Duration;

use proxima_core::error::ProtocolError;
use tokio::process::Command;
use tokio::time::timeout;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct DetectedHarnessTs {
    pub path: String,
    pub version: String,
}

fn finder_command() -> (&'static str, &'static str) {
    if cfg!(windows) {
        ("where.exe", "where")
    } else {
        ("which", "which")
    }
}

async fn locate_harness(name: &str) -> Result<Option<PathBuf>, ProtocolError> {
    if name.trim().is_empty() {
        return Ok(None);
    }

    let (program, label) = finder_command();
    let output = Command::new(program)
        .arg(name)
        .output()
        .await
        .map_err(|err| ProtocolError::internal(format!("{label} {name}: {err}")))?;

    if !output.status.success() {
        return Ok(None);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| PathBuf::from(line.trim())))
}

fn parse_version_output(name: &str, bytes: &[u8]) -> String {
    let output = String::from_utf8_lossy(bytes);
    let mut parts = output.split_whitespace();
    match parts.next() {
        Some(program) if program == name || program.ends_with(name) => {
            parts.next().unwrap_or_default().to_string()
        }
        Some(_) => parts.next().unwrap_or_default().to_string(),
        None => String::new(),
    }
}

async fn detect_version(name: &str, path: &PathBuf) -> String {
    let output = timeout(
        Duration::from_secs(2),
        Command::new(path).arg("--version").output(),
    )
    .await;

    let Ok(Ok(output)) = output else {
        return String::new();
    };

    if output.stdout.is_empty() {
        parse_version_output(name, &output.stderr)
    } else {
        parse_version_output(name, &output.stdout)
    }
}

/// # Errors
/// Returns `ProtocolError::Internal` when the platform lookup command cannot
/// be executed. Missing harness binaries return `Ok(None)`.
#[tauri::command]
#[specta::specta]
pub async fn detect_local_harness(
    name: String,
) -> Result<Option<DetectedHarnessTs>, ProtocolError> {
    let Some(path) = locate_harness(&name).await? else {
        return Ok(None);
    };
    let version = detect_version(&name, &path).await;
    Ok(Some(DetectedHarnessTs {
        path: path.display().to_string(),
        version,
    }))
}
