//! Per-wake observation sandbox: the disposable Docker container the whole
//! workspace wake runs inside.
//!
//! The sandbox is an observation instrument, not an adversarial jail. One
//! container per wake, running as the host uid, bind-mounting a fresh clone
//! at `/workspace` and a persistent build cache at `/cache`. The container
//! idles on `sleep infinity`; `workspace_shell` enters it via `docker exec`.
//! When the wake ends, the container and its per-wake network are discarded.
//!
//! Egress is pointed at a per-wake logging proxy. The proxy container and
//! the network log capture are wired in Phase C; this module already places
//! the workspace container on the per-wake `--internal` network and exports
//! `HTTP(S)_PROXY` so that wiring is purely additive.

use std::path::Path;

use proxima_core::harness::WorkspaceSandboxSpec;
use tokio::process::Command;
use uuid::Uuid;

/// Port the per-wake logging proxy listens on (Phase C).
const PROXY_PORT: u16 = 8888;

/// Handle to a running per-wake sandbox, threaded into `WorkspaceCtx` so
/// `workspace_shell` can `docker exec` into the container.
#[derive(Debug, Clone)]
pub struct WorkspaceSandboxSession {
    /// `proxima-wake-<invocation_id>` — the running observation container.
    pub container_name: String,
    /// `proxima-wake-net-<invocation_id>` — the per-wake docker network.
    pub network_name: String,
    /// Sandbox image; surfaced in `ShellResult.sandbox` as `docker:<image>`.
    pub image: String,
    /// `proxima.wake=<invocation_id>` label, for startup orphan reaping.
    pub label: String,
}

/// Per-wake container name.
#[must_use]
pub fn container_name(invocation_id: Uuid) -> String {
    format!("proxima-wake-{invocation_id}")
}

/// Per-wake docker network name.
#[must_use]
pub fn network_name(invocation_id: Uuid) -> String {
    format!("proxima-wake-net-{invocation_id}")
}

/// Per-wake logging-proxy container name (Phase C).
#[must_use]
pub fn proxy_name(invocation_id: Uuid) -> String {
    format!("proxima-wake-proxy-{invocation_id}")
}

/// The `docker` executable. Overridable so Docker Desktop or a remote
/// daemon can be selected on the host.
#[must_use]
pub fn docker_bin() -> String {
    std::env::var("PROXIMA_WORKSPACE_SANDBOX_DOCKER_BIN").unwrap_or_else(|_| "docker".into())
}

/// `docker network create --internal <network>`.
#[must_use]
pub fn network_create_args(network: &str) -> Vec<String> {
    vec![
        "network".into(),
        "create".into(),
        "--internal".into(),
        network.into(),
    ]
}

/// `docker network rm <network>`.
#[must_use]
pub fn network_rm_args(network: &str) -> Vec<String> {
    vec!["network".into(), "rm".into(), network.into()]
}

/// `docker rm -f <container>`.
#[must_use]
pub fn container_rm_args(container: &str) -> Vec<String> {
    vec!["rm".into(), "-f".into(), container.into()]
}

/// `docker run -d ...` args for the per-wake observation container.
///
/// The container runs detached as the host uid, sits on the per-wake
/// internal network, and idles on `sleep infinity` — shell commands enter
/// it later via `docker exec`. Only proxy + benign build env reach the
/// container; no provider secrets are forwarded.
#[must_use]
pub fn container_run_args(
    spec: &WorkspaceSandboxSpec,
    container: &str,
    network: &str,
    proxy_host: &str,
    staging_mount: &str,
) -> Vec<String> {
    let mut args = vec![
        "run".into(),
        "-d".into(),
        "--init".into(),
        "--name".into(),
        container.into(),
        "--label".into(),
        spec.label.clone(),
        "--user".into(),
        format!("{}:{}", spec.uid, spec.gid),
        "--network".into(),
        network.into(),
        "-e".into(),
        format!("HTTP_PROXY=http://{proxy_host}:{PROXY_PORT}"),
        "-e".into(),
        format!("HTTPS_PROXY=http://{proxy_host}:{PROXY_PORT}"),
        "-e".into(),
        "HOME=/tmp".into(),
        "-e".into(),
        "CI=true".into(),
    ];
    if let Some(memory) = &spec.memory {
        args.push("--memory".into());
        args.push(memory.clone());
    }
    args.extend([
        "-v".into(),
        format!("{staging_mount}:/workspace:rw"),
        "-v".into(),
        format!("{}:/cache:rw", spec.cache_volume),
        "-w".into(),
        "/workspace".into(),
        spec.image.clone(),
        "sleep".into(),
        "infinity".into(),
    ]);
    args
}

async fn run_docker(step: &str, args: &[String]) -> Result<String, String> {
    let output = Command::new(docker_bin())
        .args(args)
        .output()
        .await
        .map_err(|err| format!("{step}: spawn docker: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "{step}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Start the per-wake observation sandbox: create the per-wake network,
/// then launch the idle container the shell tool will `docker exec` into.
///
/// # Errors
///
/// Returns the docker failure detail when the network or container cannot
/// be created. The caller must fail the wake — there is no host fallback.
pub async fn start(
    spec: &WorkspaceSandboxSpec,
    invocation_id: Uuid,
    workspace_root: &Path,
) -> Result<WorkspaceSandboxSession, String> {
    let container = container_name(invocation_id);
    let network = network_name(invocation_id);
    let proxy_host = proxy_name(invocation_id);
    let staging = workspace_root
        .canonicalize()
        .map_err(|err| format!("canonicalize workspace root: {err}"))?;
    let staging_mount = staging.to_string_lossy().to_string();

    run_docker("network create", &network_create_args(&network)).await?;

    if let Err(err) = run_docker(
        "container run",
        &container_run_args(spec, &container, &network, &proxy_host, &staging_mount),
    )
    .await
    {
        // The network is already up; do not leak it on a failed start.
        let _ = run_docker("network rm", &network_rm_args(&network)).await;
        return Err(err);
    }

    Ok(WorkspaceSandboxSession {
        container_name: container,
        network_name: network,
        image: spec.image.clone(),
        label: spec.label.clone(),
    })
}

/// Tear down the per-wake sandbox: remove the container, then its network.
///
/// # Errors
///
/// Returns the first docker failure detail. The caller logs it but keeps
/// the model outcome — a leaked container is swept by the startup reaper.
pub async fn stop(session: &WorkspaceSandboxSession) -> Result<(), String> {
    run_docker("container rm", &container_rm_args(&session.container_name)).await?;
    run_docker("network rm", &network_rm_args(&session.network_name)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        WorkspaceSandboxSpec, container_name, container_run_args, network_create_args,
        network_name, proxy_name,
    };
    use uuid::Uuid;

    fn spec() -> WorkspaceSandboxSpec {
        WorkspaceSandboxSpec {
            image: "proxima-workspace-sandbox:local".into(),
            proxy_image: "proxima-workspace-proxy:local".into(),
            uid: 501,
            gid: 20,
            cache_volume: "proxima-wake-cache".into(),
            memory: Some("4g".into()),
            label: "proxima.wake=abc".into(),
        }
    }

    #[test]
    fn run_args_carry_network_user_mounts_and_cache() {
        let args = container_run_args(
            &spec(),
            "proxima-wake-x",
            "proxima-wake-net-x",
            "proxima-wake-proxy-x",
            "/clone",
        );
        assert!(
            args.windows(2)
                .any(|p| p == ["--network", "proxima-wake-net-x"])
        );
        assert!(args.windows(2).any(|p| p == ["--user", "501:20"]));
        assert!(
            args.windows(2)
                .any(|p| p == ["-v", "/clone:/workspace:rw"])
        );
        assert!(
            args.windows(2)
                .any(|p| p == ["-v", "proxima-wake-cache:/cache:rw"])
        );
        assert!(args.windows(2).any(|p| p == ["--memory", "4g"]));
        assert_eq!(args.last().unwrap(), "infinity");
    }

    #[test]
    fn run_args_forward_no_secret_env() {
        let args = container_run_args(&spec(), "c", "n", "p", "/clone");
        let forwarded: Vec<&str> = args
            .iter()
            .zip(args.iter().skip(1))
            .filter(|(flag, _)| flag.as_str() == "-e")
            .map(|(_, value)| value.split('=').next().unwrap_or(""))
            .collect();
        for key in &forwarded {
            assert!(
                matches!(*key, "HTTP_PROXY" | "HTTPS_PROXY" | "HOME" | "CI"),
                "unexpected env forwarded to sandbox container: {key}"
            );
        }
    }

    #[test]
    fn memory_omitted_when_unbounded() {
        let mut unbounded = spec();
        unbounded.memory = None;
        let args = container_run_args(&unbounded, "c", "n", "p", "/clone");
        assert!(!args.iter().any(|arg| arg == "--memory"));
    }

    #[test]
    fn network_create_is_internal() {
        assert_eq!(
            network_create_args("proxima-wake-net-x"),
            ["network", "create", "--internal", "proxima-wake-net-x"]
        );
    }

    #[test]
    fn names_are_deterministic_per_invocation() {
        let id = Uuid::nil();
        assert_eq!(container_name(id), format!("proxima-wake-{id}"));
        assert_eq!(network_name(id), format!("proxima-wake-net-{id}"));
        assert_eq!(proxy_name(id), format!("proxima-wake-proxy-{id}"));
    }
}
