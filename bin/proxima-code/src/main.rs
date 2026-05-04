//! `proxima-code` — composite binary that links proxima-core,
//! proxima-storage-pg, and the proxima-code flavor.
//!
//! Usage:
//!     proxima-code index --repo-path <path> --repo-id <uuidv7> \
//!         --owner-user <uuidv7> --owner-org <uuidv7> \
//!         [--database-url postgres://...] \
//!         [--watch] [--poll-interval-ms 2000]
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

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use proxima_code::{IndexReport, LocalGitSource, migrator};
use proxima_core::{Cursor, OrgId, Owner, Principal, UserId};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

const DEFAULT_POLL_INTERVAL_MS: u64 = 2000;

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
        None => return Err("usage: proxima-code index --repo-path <p> --repo-id <id> --owner-user <id> --owner-org <id> [--database-url <url>] [--watch] [--poll-interval-ms <n>]".into()),
    }

    let mut repo_path: Option<PathBuf> = None;
    let mut repo_id: Option<Uuid> = None;
    let mut owner_user: Option<Uuid> = None;
    let mut owner_org: Option<Uuid> = None;
    let mut database_url: Option<String> = None;
    let mut watch = false;
    let mut poll_interval_ms: u64 = DEFAULT_POLL_INTERVAL_MS;

    while let Some(flag) = args.next() {
        if flag == "--watch" {
            watch = true;
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

    let source = LocalGitSource::new(repo_id, repo_path, owner);

    if watch {
        watch_loop(&source, &pg, poll_interval_ms).await
    } else {
        let (report, _cursor) = source.run_poll(pg.pool(), &Cursor::empty()).await?;
        print_report(&report);
        Ok(())
    }
}

async fn watch_loop(
    source: &LocalGitSource,
    pg: &PgStorage,
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
        tokio::select! {
            biased;
            _ = tokio::signal::ctrl_c() => {
                eprintln!("\nstopping (Ctrl-C)");
                return Ok(());
            }
            res = source.run_poll(pg.pool(), &cursor) => {
                let (report, next) = res?;
                cursor = next;
                if poll_emitted_anything(&report) {
                    print_report(&report);
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

fn poll_emitted_anything(r: &IndexReport) -> bool {
    r.commits_emitted + r.files_present_emitted + r.files_tombstoned + r.chunks_emitted
        + r.chunks_tombstoned
        > 0
}

fn print_report(r: &IndexReport) {
    println!(
        "indexed: {} commits ({} replayed), {} files (+{} tombstoned), {} chunks (+{} tombstoned)",
        r.commits_emitted,
        r.commits_replayed,
        r.files_present_emitted,
        r.files_tombstoned,
        r.chunks_emitted,
        r.chunks_tombstoned,
    );
}
