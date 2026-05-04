//! `proxima-code` — composite binary that links proxima-core,
//! proxima-storage-pg, and the proxima-code flavor.
//!
//! Usage:
//!     proxima-code index --repo-path <path> --repo-id <uuidv7> \
//!         --owner-user <uuidv7> --owner-org <uuidv7> \
//!         [--database-url postgres://...]
//!
//! The CLI is intentionally minimal — no `clap`, no subcommand
//! dispatch framework. Args are positional/keyworded by hand. v1
//! ergonomics are not the goal; we want a thin wrapper that proves
//! the wiring end-to-end.

use std::path::PathBuf;
use std::process::ExitCode;

use proxima_code::{LocalGitSource, migrator};
use proxima_core::{OrgId, Owner, Principal, UserId};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

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
        None => return Err("usage: proxima-code index --repo-path <p> --repo-id <id> --owner-user <id> --owner-org <id> [--database-url <url>]".into()),
    }

    let mut repo_path: Option<PathBuf> = None;
    let mut repo_id: Option<Uuid> = None;
    let mut owner_user: Option<Uuid> = None;
    let mut owner_org: Option<Uuid> = None;
    let mut database_url: Option<String> = None;

    while let Some(flag) = args.next() {
        let value = args
            .next()
            .ok_or_else(|| format!("flag {flag} expects a value"))?;
        match flag.as_str() {
            "--repo-path" => repo_path = Some(PathBuf::from(value)),
            "--repo-id" => repo_id = Some(Uuid::parse_str(&value)?),
            "--owner-user" => owner_user = Some(Uuid::parse_str(&value)?),
            "--owner-org" => owner_org = Some(Uuid::parse_str(&value)?),
            "--database-url" => database_url = Some(value),
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
    let report = source.index(pg.pool()).await?;

    println!(
        "indexed: {} commits ({} replayed), {} files (+{} unchanged, +{} tombstoned), {} chunks (+{} tombstoned)",
        report.commits_emitted,
        report.commits_replayed,
        report.files_present_emitted,
        report.files_unchanged,
        report.files_tombstoned,
        report.chunks_emitted,
        report.chunks_tombstoned,
    );

    Ok(())
}
