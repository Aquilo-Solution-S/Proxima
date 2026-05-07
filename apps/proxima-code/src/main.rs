//! `proxima-code` — composite binary that links proxima-core,
//! proxima-storage-pg, and the proxima-code flavor.
//!
//! Usage:
//!     proxima-code index --repo-path <path> --repo-id <uuidv7> \
//!         --owner-user <uuidv7> --owner-org <uuidv7> \
//!         [--database-url postgres://...] \
//!         [--watch] [--poll-interval-ms 2000] \
//!         [--llm-model gemma4:31b] [--embed-model qwen3-embedding:8b] \
//!         [--embed-dim 4096] [--no-dispatcher]
//!
//! The CLI is intentionally minimal — no `clap`, no subcommand
//! dispatch framework. Args are positional/keyworded by hand. v1
//! ergonomics are not the goal; we want a thin wrapper that proves
//! the wiring end-to-end.
//!
//! `--watch` enters a polling loop with the given interval; on Ctrl-C
//! the in-flight poll drains before the process exits. The cursor is
//! kept in memory only — restarts begin from an empty cursor.
//! Idempotency on `event_id` makes that safe.
//!
//! After each successful poll, the personality dispatcher walks new
//! change events. Pass `--no-dispatcher` to skip wake processing
//! (e.g. when iterating on the source path without an Ollama running).

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use proxima_code::{IndexReport, LocalGitSource, build_engine, migrator};
use proxima_core::auth::NoAuth;
use proxima_core::{Cursor, OrgId, Owner, Principal, UserId};
use proxima_llm_openai_compat::OllamaEmbeddingClient;
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

const DEFAULT_POLL_INTERVAL_MS: u64 = 2000;
const DEFAULT_LLM_MODEL: &str = "gemma4:31b";
const DEFAULT_EMBED_MODEL: &str = "qwen3-embedding:8b";
const DEFAULT_EMBED_DIM: usize = 4096;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);

    match args.next().as_deref() {
        Some("index") => {}
        Some(other) => return Err(format!("unknown subcommand: {other}").into()),
        None => return Err("usage: proxima-code index --repo-path <p> --repo-id <id> --owner-user <id> --owner-org <id> [--database-url <url>] [--watch] [--poll-interval-ms <n>] [--llm-model <m>] [--embed-model <m>] [--embed-dim <n>] [--no-dispatcher]".into()),
    }

    let mut repo_path: Option<PathBuf> = None;
    let mut repo_id: Option<Uuid> = None;
    let mut owner_user: Option<Uuid> = None;
    let mut owner_org: Option<Uuid> = None;
    let mut database_url: Option<String> = None;
    let mut watch = false;
    let mut poll_interval_ms: u64 = DEFAULT_POLL_INTERVAL_MS;
    let mut llm_model: String = DEFAULT_LLM_MODEL.into();
    let mut embed_model: String = DEFAULT_EMBED_MODEL.into();
    let mut embed_dim: usize = DEFAULT_EMBED_DIM;
    let mut no_dispatcher = false;

    while let Some(flag) = args.next() {
        if flag == "--watch" {
            watch = true;
            continue;
        }
        if flag == "--no-dispatcher" || flag == "--no-f2a" {
            no_dispatcher = true;
            continue;
        }
        let value = args
            .next()
            .ok_or_else(|| format!("flag {flag} expects a value"))?;
        match flag.as_str() {
            "--repo-path" => repo_path = Some(PathBuf::from(value)),
            "--repo-id" => repo_id = Some(Uuid::parse_str(&value)?),
            "--owner-user" => owner_user = Some(Uuid::parse_str(&value)?),
            "--owner-org" => owner_org = Some(Uuid::parse_str(&value)?),
            "--database-url" => database_url = Some(value),
            "--poll-interval-ms" => poll_interval_ms = value.parse()?,
            "--llm-model" => llm_model = value,
            "--embed-model" => embed_model = value,
            "--embed-dim" => embed_dim = value.parse()?,
            other => return Err(format!("unknown flag: {other}").into()),
        }
    }

    let repo_path = repo_path.ok_or("--repo-path required")?;
    let repo_id = repo_id.ok_or("--repo-id required")?;
    let owner_user = owner_user.ok_or("--owner-user required")?;
    let owner_org = owner_org.ok_or("--owner-org required")?;
    let database_url = database_url.unwrap_or_else(PgStorage::url_from_env);

    let owner = Owner {
        principal: Principal::User(UserId::new(owner_user)),
        org_id: OrgId::new(owner_org),
    };

    let pg = PgStorage::connect(&database_url).await?;
    pg.run_migrations().await?;
    migrator().run(pg.pool()).await?;

    let source = LocalGitSource::new(repo_id, repo_path, owner.clone());

    // Dispatcher engine: optional. Without --no-dispatcher we wire Ollama
    // embeddings; the substrate's wake/decide/write loop now talks to
    // Anthropic, which the CLI does not configure yet — the dispatcher
    // skips wakes when the Anthropic client is unwired.
    let _ = llm_model;
    let engine = if no_dispatcher {
        None
    } else {
        let embed = OllamaEmbeddingClient::from_env(&embed_model, embed_dim)?;
        let engine = build_engine(
            pg.clone(),
            Box::new(NoAuth::new(owner.principal.clone(), owner.clone())),
        )
        .with_embed(Arc::new(embed));
        eprintln!(
            "dispatcher enabled - embed={embed_model} (dim={embed_dim}); anthropic unwired (wakes will skip)"
        );
        Some(engine)
    };

    if watch {
        watch_loop(&source, &pg, engine.as_ref(), poll_interval_ms).await
    } else {
        let (report, _cursor) = source
            .run_poll(pg.pool(), &Cursor::empty(), &mut |_| {})
            .await?;
        print_report(&report);
        if let Some(eng) = engine.as_ref() {
            run_dispatcher_pass(eng).await?;
        }
        Ok(())
    }
}

async fn watch_loop(
    source: &LocalGitSource,
    pg: &PgStorage,
    engine: Option<&proxima_core::Engine>,
    poll_interval_ms: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut cursor = Cursor::empty();
    let interval = Duration::from_millis(poll_interval_ms);
    eprintln!(
        "watching repo at {} (poll every {}ms; Ctrl-C to stop)",
        source.repo_path.display(),
        poll_interval_ms
    );

    loop {
        let mut noop_progress = |_| {};
        tokio::select! {
            biased;
            _ = tokio::signal::ctrl_c() => {
                eprintln!("\nstopping (Ctrl-C)");
                return Ok(());
            }
            res = source.run_poll(pg.pool(), &cursor, &mut noop_progress) => {
                let (report, next) = res?;
                cursor = next;
                if poll_emitted_anything(&report) {
                    print_report(&report);
                }
                if let Some(eng) = engine
                    && let Err(e) = run_dispatcher_pass(eng).await {
                        eprintln!("dispatcher pass failed: {e}");
                    }
            }
        }
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                eprintln!("\nstopping (Ctrl-C)");
                return Ok(());
            }
            () = tokio::time::sleep(interval) => {}
        }
    }
}

async fn run_dispatcher_pass(
    engine: &proxima_core::Engine,
) -> Result<(), Box<dyn std::error::Error>> {
    let fired = engine.run_dispatcher_tick().await?;
    if fired > 0 {
        eprintln!("dispatcher fired {fired} wake(s)");
    }
    Ok(())
}

fn poll_emitted_anything(r: &IndexReport) -> bool {
    r.commits_emitted
        + r.files_present_emitted
        + r.files_tombstoned
        + r.chunks_emitted
        + r.chunks_reused
        + r.chunks_tombstoned
        > 0
}

fn print_report(r: &IndexReport) {
    println!(
        "indexed: {} commits ({} replayed), {} files (+{} tombstoned), {} chunks (+{} reused, +{} tombstoned)",
        r.commits_emitted,
        r.commits_replayed,
        r.files_present_emitted,
        r.files_tombstoned,
        r.chunks_emitted,
        r.chunks_reused,
        r.chunks_tombstoned,
    );
}
