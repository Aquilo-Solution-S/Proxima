//! Per-wake observation sandbox: the disposable Docker container the whole
//! workspace wake runs inside.
//!
//! The sandbox is an observation instrument, not an adversarial jail. One
//! container per wake, running as the host uid, bind-mounting a fresh clone
//! at `/workspace` and a persistent build cache at `/cache`. The container
//! idles on `sleep infinity`; `workspace_shell` enters it via `docker exec`.
//! When the wake ends, the container and its per-wake network are discarded.
//!
//! Egress traverses a per-wake logging proxy. The proxy container is
//! dual-homed — on the default bridge (internet) and on the per-wake
//! `--internal` network — while the workspace container sits on the
//! internal network only, so every egress attempt must go through the
//! proxy. On teardown the proxy's `docker logs` output is captured as the
//! wake's network log (CONNECT-level metadata, no TLS interception).

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
    /// `proxima-wake-proxy-<invocation_id>` — the per-wake logging proxy.
    pub proxy_name: String,
    /// Sandbox image; surfaced in `ShellResult.sandbox` as `docker:<image>`.
    pub image: String,
    /// `proxima.wake=<invocation_id>` label, for startup orphan reaping.
    pub label: String,
}

/// Result of tearing a per-wake sandbox down.
#[derive(Debug, Clone)]
pub struct SandboxStopOutcome {
    /// The per-wake proxy's egress log (`docker logs`): CONNECT-level
    /// metadata for the wake. Empty when the proxy log could not be read.
    pub network_log: String,
    /// First teardown failure, if any. The wake outcome is preserved
    /// regardless; the startup reaper sweeps whatever was left behind.
    pub teardown_error: Option<String>,
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

/// `docker network connect <network> <container>`.
#[must_use]
pub fn network_connect_args(network: &str, container: &str) -> Vec<String> {
    vec![
        "network".into(),
        "connect".into(),
        network.into(),
        container.into(),
    ]
}

/// `docker run -d ...` args for the per-wake logging proxy.
///
/// The proxy starts on the default bridge network (its route to the
/// internet); `start` then dual-homes it onto the per-wake internal
/// network. It carries no mounts, no secrets, and no host uid.
#[must_use]
pub fn proxy_run_args(spec: &WorkspaceSandboxSpec, proxy: &str) -> Vec<String> {
    vec![
        "run".into(),
        "-d".into(),
        "--init".into(),
        "--name".into(),
        proxy.into(),
        "--label".into(),
        spec.label.clone(),
        spec.proxy_image.clone(),
    ]
}

/// `docker logs <proxy>` — reads the proxy's captured egress log.
#[must_use]
pub fn proxy_logs_args(proxy: &str) -> Vec<String> {
    vec!["logs".into(), proxy.into()]
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

/// `docker logs <proxy>` — the proxy's stdout is the egress log; any
/// stderr is appended so a misconfigured proxy is visible in the record.
async fn capture_docker_logs(proxy: &str) -> Result<String, String> {
    let output = Command::new(docker_bin())
        .args(proxy_logs_args(proxy))
        .output()
        .await
        .map_err(|err| format!("docker logs: spawn docker: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "docker logs: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let mut log = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.trim().is_empty() {
        log.push_str(&stderr);
    }
    Ok(log)
}

/// Best-effort teardown of whatever a partial `start` left running.
async fn force_remove(container: Option<&str>, proxy: Option<&str>, network: &str) {
    if let Some(container) = container {
        let _ = run_docker("container rm", &container_rm_args(container)).await;
    }
    if let Some(proxy) = proxy {
        let _ = run_docker("proxy rm", &container_rm_args(proxy)).await;
    }
    let _ = run_docker("network rm", &network_rm_args(network)).await;
}

/// Start the per-wake observation sandbox: the internal network, the
/// logging proxy dual-homed onto it, then the idle observation container
/// the shell tool will `docker exec` into.
///
/// # Errors
///
/// Returns the docker failure detail when any step fails; partial state is
/// torn down first. The caller must fail the wake — there is no host
/// fallback.
pub async fn start(
    spec: &WorkspaceSandboxSpec,
    invocation_id: Uuid,
    workspace_root: &Path,
) -> Result<WorkspaceSandboxSession, String> {
    let container = container_name(invocation_id);
    let network = network_name(invocation_id);
    let proxy = proxy_name(invocation_id);
    let staging = workspace_root
        .canonicalize()
        .map_err(|err| format!("canonicalize workspace root: {err}"))?;
    let staging_mount = staging.to_string_lossy().to_string();

    // 1. The per-wake internal network.
    run_docker("network create", &network_create_args(&network)).await?;

    // 2. The logging proxy — started on the default bridge (its route to
    //    the internet), then connected to the internal network so the
    //    workspace container can reach it by name.
    if let Err(err) = run_docker("proxy run", &proxy_run_args(spec, &proxy)).await {
        force_remove(None, None, &network).await;
        return Err(err);
    }
    if let Err(err) = run_docker("network connect", &network_connect_args(&network, &proxy)).await {
        force_remove(None, Some(&proxy), &network).await;
        return Err(err);
    }

    // 3. The observation container, on the internal network only — every
    //    egress attempt must traverse the proxy.
    if let Err(err) = run_docker(
        "container run",
        &container_run_args(spec, &container, &network, &proxy, &staging_mount),
    )
    .await
    {
        force_remove(None, Some(&proxy), &network).await;
        return Err(err);
    }

    Ok(WorkspaceSandboxSession {
        container_name: container,
        network_name: network,
        proxy_name: proxy,
        image: spec.image.clone(),
        label: spec.label.clone(),
    })
}

/// Tear down the per-wake sandbox: capture the proxy's egress log, then
/// remove the observation container, the proxy, and the network.
///
/// Infallible — it always returns the captured log (empty if unreadable);
/// teardown failures surface in `teardown_error` and are swept by the
/// startup reaper rather than failing the wake.
pub async fn stop(session: &WorkspaceSandboxSession) -> SandboxStopOutcome {
    // Capture the egress log before anything is removed.
    let network_log = capture_docker_logs(&session.proxy_name)
        .await
        .unwrap_or_default();

    let mut teardown_error = None;
    for (step, args) in [
        ("container rm", container_rm_args(&session.container_name)),
        ("proxy rm", container_rm_args(&session.proxy_name)),
        ("network rm", network_rm_args(&session.network_name)),
    ] {
        if let Err(err) = run_docker(step, &args).await {
            teardown_error.get_or_insert(err);
        }
    }

    SandboxStopOutcome {
        network_log,
        teardown_error,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        WorkspaceSandboxSpec, container_name, container_run_args, network_connect_args,
        network_create_args, network_name, proxy_logs_args, proxy_name, proxy_run_args,
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
    fn proxy_run_args_carry_proxy_image_and_no_mounts() {
        let args = proxy_run_args(&spec(), "proxima-wake-proxy-x");
        assert!(
            args.windows(2)
                .any(|p| p == ["--name", "proxima-wake-proxy-x"])
        );
        assert_eq!(args.last().unwrap(), "proxima-workspace-proxy:local");
        // The proxy carries no clone/cache mounts and no host uid.
        assert!(!args.iter().any(|a| a == "-v"));
        assert!(!args.iter().any(|a| a == "--user"));
    }

    #[test]
    fn network_connect_dual_homes_the_proxy() {
        assert_eq!(
            network_connect_args("proxima-wake-net-x", "proxima-wake-proxy-x"),
            ["network", "connect", "proxima-wake-net-x", "proxima-wake-proxy-x"]
        );
    }

    #[test]
    fn proxy_logs_reads_the_proxy_container() {
        assert_eq!(
            proxy_logs_args("proxima-wake-proxy-x"),
            ["logs", "proxima-wake-proxy-x"]
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
