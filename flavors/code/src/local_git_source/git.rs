//! Git subprocess wrappers used by `LocalGitSource`.
//!
//! Pure I/O over `git` on PATH; no DB, no schema awareness. The
//! orchestration in the parent module owns the policy (which commits,
//! which diffs, what to ingest); this module only knows how to extract
//! shapes from a working tree via `std::process::Command`.

use std::path::Path;
use std::process::Command;

use super::IndexError;

/// Walk plan returned by the cursor-aware git pre-pass.
#[derive(Debug)]
pub(super) struct WalkPlan {
    pub head_sha: String,
    pub head_tree_sha: String,
    pub commits: Vec<CommitInfo>,
}

/// One commit's metadata as parsed from `git log --format=...`.
#[derive(Debug, Clone)]
pub(super) struct CommitInfo {
    pub sha: String,
    pub parents: Vec<String>,
    pub author_name: String,
    pub author_email: String,
    pub author_time: time::OffsetDateTime,
    pub committer_name: String,
    pub committer_email: String,
    pub committer_time: time::OffsetDateTime,
    pub message: String,
}

fn run_git(repo: &Path, args: &[&str]) -> Result<Vec<u8>, IndexError> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()?;
    if !out.status.success() {
        return Err(IndexError::Git(format!(
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(out.stdout)
}

pub(super) fn head_sha(repo: &Path) -> Result<String, IndexError> {
    let bytes = run_git(repo, &["rev-parse", "HEAD"])?;
    String::from_utf8(bytes)
        .map(|s| s.trim().to_string())
        .map_err(|_| IndexError::Utf8)
}

pub(super) fn tree_sha(repo: &Path, rev: &str) -> Result<String, IndexError> {
    let bytes = run_git(repo, &["rev-parse", &format!("{rev}^{{tree}}")])?;
    String::from_utf8(bytes)
        .map(|s| s.trim().to_string())
        .map_err(|_| IndexError::Utf8)
}

pub(super) fn log(repo: &Path) -> Result<Vec<CommitInfo>, IndexError> {
    log_args(repo, &["log", "--first-parent"])
}

pub(super) fn log_range(repo: &Path, from: &str, to: &str) -> Result<Vec<CommitInfo>, IndexError> {
    log_args(repo, &["log", "--first-parent", &format!("{from}..{to}")])
}

fn log_args(repo: &Path, base_args: &[&str]) -> Result<Vec<CommitInfo>, IndexError> {
    let fmt = "%H%x1f%P%x1f%an%x1f%ae%x1f%aI%x1f%cn%x1f%ce%x1f%cI%x1f%B%x1e";
    let fmt_arg = format!("--format={fmt}");
    let mut args: Vec<&str> = base_args.to_vec();
    args.push(&fmt_arg);
    let bytes = run_git(repo, &args)?;
    let text = String::from_utf8(bytes).map_err(|_| IndexError::Utf8)?;
    let mut out = Vec::new();
    for record in text.split('\x1e') {
        let r = record.trim_start_matches('\n');
        if r.is_empty() {
            continue;
        }
        let parts: Vec<&str> = r.splitn(9, '\x1f').collect();
        if parts.len() != 9 {
            return Err(IndexError::Git(format!("malformed log record: {r:?}")));
        }
        let parents: Vec<String> = parts[1].split_whitespace().map(str::to_string).collect();
        let author_time = time::OffsetDateTime::parse(
            parts[4],
            &time::format_description::well_known::Iso8601::DEFAULT,
        )
        .map_err(|e| IndexError::Git(format!("author_time {:?}: {e}", parts[4])))?;
        let committer_time = time::OffsetDateTime::parse(
            parts[7],
            &time::format_description::well_known::Iso8601::DEFAULT,
        )
        .map_err(|e| IndexError::Git(format!("committer_time {:?}: {e}", parts[7])))?;
        out.push(CommitInfo {
            sha: parts[0].to_string(),
            parents,
            author_name: parts[2].to_string(),
            author_email: parts[3].to_string(),
            author_time,
            committer_name: parts[5].to_string(),
            committer_email: parts[6].to_string(),
            committer_time,
            message: parts[8].to_string(),
        });
    }
    Ok(out)
}

pub(super) fn ls_files(repo: &Path, rev: &str) -> Result<Vec<(String, Vec<u8>)>, IndexError> {
    let listing = run_git(repo, &["ls-tree", "-r", "-z", rev])?;
    let listing_str = String::from_utf8(listing).map_err(|_| IndexError::Utf8)?;

    let mut paths = Vec::new();
    for entry in listing_str.split('\0') {
        if entry.is_empty() {
            continue;
        }
        let (header, path) = entry
            .split_once('\t')
            .ok_or_else(|| IndexError::Git(format!("malformed ls-tree entry: {entry:?}")))?;
        let cols: Vec<&str> = header.split_whitespace().collect();
        if cols.len() != 3 || cols[1] != "blob" {
            continue;
        }
        paths.push((path.to_string(), cols[2].to_string()));
    }

    let mut out = Vec::with_capacity(paths.len());
    for (path, oid) in paths {
        let bytes = run_git(repo, &["cat-file", "blob", &oid])?;
        out.push((path, bytes));
    }
    Ok(out)
}

pub(super) fn cat_blob(repo: &Path, rev: &str, path: &str) -> Result<Vec<u8>, IndexError> {
    run_git(repo, &["show", &format!("{rev}:{path}")])
}

/// Returns `(changed_or_added_paths, deleted_paths)` between two
/// tree shas. Uses `--name-status -z`. Renames are reported as
/// delete + add.
pub(super) fn diff_paths(
    repo: &Path,
    from: &str,
    to: &str,
) -> Result<(Vec<String>, Vec<String>), IndexError> {
    let bytes = run_git(repo, &["diff", "--name-status", "-z", from, to])?;
    let text = String::from_utf8(bytes).map_err(|_| IndexError::Utf8)?;
    // -z emits: "<status>\0<path>\0[<path2>\0 if rename/copy]"
    // Status codes: A/M/D for add/modify/delete; R<num>/C<num> for
    // rename/copy with two paths.
    let mut tokens = text.split('\0').filter(|s| !s.is_empty());
    let mut changed = Vec::new();
    let mut deleted = Vec::new();
    while let Some(status) = tokens.next() {
        let primary = tokens
            .next()
            .ok_or_else(|| IndexError::Git(format!("diff missing path after status {status:?}")))?;
        let first_char = status.chars().next().unwrap_or(' ');
        match first_char {
            'A' | 'M' | 'T' => changed.push(primary.to_string()),
            'D' => deleted.push(primary.to_string()),
            'R' | 'C' => {
                let dest = tokens.next().ok_or_else(|| {
                    IndexError::Git(format!("rename {status:?} missing dest path"))
                })?;
                deleted.push(primary.to_string());
                changed.push(dest.to_string());
            }
            _ => {}
        }
    }
    Ok((changed, deleted))
}
