//! Git subprocess wrappers used by `LocalGitSource`.
//!
//! Pure I/O over `git` on PATH; no DB, no schema awareness. The
//! orchestration in the parent module owns the policy (which commits,
//! which diffs, what to ingest); this module only knows how to extract
//! shapes from a working tree via `std::process::Command`.

use std::path::Path;
use std::process::Command;

use super::IndexError;
use crate::chunker::MAX_BLOB_BYTES;

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

fn blob_size(repo: &Path, spec: &str) -> Result<u64, IndexError> {
    let bytes = run_git(repo, &["cat-file", "-s", spec])?;
    let text = String::from_utf8(bytes).map_err(|_| IndexError::Utf8)?;
    text.trim()
        .parse::<u64>()
        .map_err(|e| IndexError::Git(format!("git cat-file -s {spec:?}: {e}")))
}

fn blob_within_cap(repo: &Path, spec: &str) -> Result<bool, IndexError> {
    Ok(blob_size(repo, spec)? <= MAX_BLOB_BYTES as u64)
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

/// One blob in a tree listing: enough to decide whether to read it, without
/// reading it.
#[derive(Debug, Clone)]
pub(super) struct TreeEntry {
    pub path: String,
    pub oid: String,
    pub size: u64,
}

/// Every blob reachable from `rev`, with sizes, in one `git` invocation.
///
/// `-l` puts the object size in the listing. Without it the only way to learn
/// a blob's size was `git cat-file -s` per blob, which — together with the
/// `git cat-file blob` that followed — meant two process spawns for every
/// file in the tree.
pub(super) fn ls_tree(repo: &Path, rev: &str) -> Result<Vec<TreeEntry>, IndexError> {
    let listing = run_git(repo, &["ls-tree", "-r", "-l", "-z", rev])?;
    let listing_str = String::from_utf8(listing).map_err(|_| IndexError::Utf8)?;

    let mut out = Vec::new();
    for entry in listing_str.split('\0') {
        if entry.is_empty() {
            continue;
        }
        let (header, path) = entry
            .split_once('\t')
            .ok_or_else(|| IndexError::Git(format!("malformed ls-tree entry: {entry:?}")))?;
        // `<mode> SP <type> SP <oid> SP<pad> <size>`; the size column is
        // right-aligned, so split on whitespace rather than a single space.
        let cols: Vec<&str> = header.split_whitespace().collect();
        if cols.len() != 4 || cols[1] != "blob" {
            continue;
        }
        let size = cols[3]
            .parse::<u64>()
            .map_err(|e| IndexError::Git(format!("git ls-tree size {:?}: {e}", cols[3])))?;
        out.push(TreeEntry {
            path: path.to_string(),
            oid: cols[2].to_string(),
            size,
        });
    }
    Ok(out)
}

/// Contents of `oids`, in order, from a single `git cat-file --batch`.
///
/// One process for the whole batch instead of one per blob. stdin is written
/// from a worker thread: `--batch` streams a reply per request, so writing the
/// whole request list before reading deadlocks once either pipe buffer fills.
pub(super) fn cat_blobs(repo: &Path, oids: &[String]) -> Result<Vec<Vec<u8>>, IndexError> {
    use std::io::{Read, Write};

    if oids.is_empty() {
        return Ok(Vec::new());
    }
    let mut child = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["cat-file", "--batch"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| IndexError::Git("git cat-file --batch: stdin unavailable".to_string()))?;
    let request: Vec<u8> = oids.join("\n").into_bytes();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&request);
        let _ = stdin.write_all(b"\n");
        let _ = stdin.flush();
        drop(stdin);
    });

    let mut stdout = Vec::new();
    child
        .stdout
        .take()
        .ok_or_else(|| IndexError::Git("git cat-file --batch: stdout unavailable".to_string()))?
        .read_to_end(&mut stdout)?;
    let _ = writer.join();
    if !child.wait()?.success() {
        return Err(IndexError::Git("git cat-file --batch failed".to_string()));
    }

    // Each reply is `<oid> SP <type> SP <size> LF <contents> LF`.
    let mut out = Vec::with_capacity(oids.len());
    let mut pos = 0usize;
    while out.len() < oids.len() {
        let Some(nl) = stdout[pos..].iter().position(|b| *b == b'\n') else {
            break;
        };
        let header = String::from_utf8_lossy(&stdout[pos..pos + nl]).to_string();
        pos += nl + 1;
        let cols: Vec<&str> = header.split_whitespace().collect();
        if cols.len() != 3 {
            // `<oid> missing`. The caller listed these from a tree, so this
            // should not happen — and guessing would shift every later reply
            // onto the wrong path.
            return Err(IndexError::Git(format!(
                "git cat-file --batch: unexpected reply {header:?}"
            )));
        }
        let size: usize = cols[2]
            .parse()
            .map_err(|e| IndexError::Git(format!("git cat-file --batch size: {e}")))?;
        if pos + size > stdout.len() {
            return Err(IndexError::Git(
                "git cat-file --batch: truncated payload".to_string(),
            ));
        }
        out.push(stdout[pos..pos + size].to_vec());
        pos += size + 1; // payload plus its trailing LF
    }
    if out.len() != oids.len() {
        return Err(IndexError::Git(format!(
            "git cat-file --batch: got {} replies for {} requests",
            out.len(),
            oids.len()
        )));
    }
    Ok(out)
}

pub(super) fn cat_blob(repo: &Path, rev: &str, path: &str) -> Result<Option<Vec<u8>>, IndexError> {
    let spec = format!("{rev}:{path}");
    if !blob_within_cap(repo, &spec)? {
        return Ok(None);
    }
    run_git(repo, &["show", &spec]).map(Some)
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;

    use tempfile::TempDir;

    use super::*;

    fn run(repo: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed: {status}");
    }

    fn fixture_repo() -> TempDir {
        let dir = TempDir::new().expect("tempdir");
        run(dir.path(), &["init", "-q", "-b", "main"]);
        run(dir.path(), &["config", "user.email", "test@example.com"]);
        run(dir.path(), &["config", "user.name", "Test"]);
        run(dir.path(), &["config", "commit.gpgsign", "false"]);
        fs::write(dir.path().join("small.txt"), b"small\n").expect("write small");
        fs::write(dir.path().join("large.txt"), vec![b'x'; MAX_BLOB_BYTES + 1])
            .expect("write large");
        run(dir.path(), &["add", "."]);
        run(dir.path(), &["commit", "-q", "-m", "initial"]);
        dir
    }

    /// The listing carries sizes, so the blob cap is applied without reading.
    #[test]
    fn ls_tree_reports_sizes_so_the_cap_needs_no_blob_read() {
        let repo = fixture_repo();

        let entries = ls_tree(repo.path(), "HEAD").expect("list tree");

        let small = entries
            .iter()
            .find(|e| e.path == "small.txt")
            .expect("small.txt listed");
        let large = entries
            .iter()
            .find(|e| e.path == "large.txt")
            .expect("large.txt listed");
        assert_eq!(small.size, b"small\n".len() as u64);
        assert!(small.size <= MAX_BLOB_BYTES as u64);
        assert!(large.size > MAX_BLOB_BYTES as u64);
    }

    /// One process returns every requested blob, in request order.
    #[test]
    fn cat_blobs_returns_every_blob_in_request_order() {
        let repo = fixture_repo();
        let entries = ls_tree(repo.path(), "HEAD").expect("list tree");
        let small = entries
            .iter()
            .find(|e| e.path == "small.txt")
            .expect("small.txt");
        let large = entries
            .iter()
            .find(|e| e.path == "large.txt")
            .expect("large.txt");

        // Deliberately large-then-small: a reply misparse would shift the
        // payloads onto the wrong requests, which order pins.
        let oids = vec![large.oid.clone(), small.oid.clone(), small.oid.clone()];
        let blobs = cat_blobs(repo.path(), &oids).expect("cat blobs");

        assert_eq!(blobs.len(), 3);
        assert_eq!(blobs[0].len(), MAX_BLOB_BYTES + 1);
        assert_eq!(blobs[1], b"small\n");
        assert_eq!(blobs[2], b"small\n");
    }

    #[test]
    fn cat_blobs_of_nothing_spawns_nothing() {
        let repo = fixture_repo();
        assert!(cat_blobs(repo.path(), &[]).expect("empty batch").is_empty());
    }

    #[test]
    fn cat_blob_returns_none_for_blob_larger_than_cap() {
        let repo = fixture_repo();

        let small = cat_blob(repo.path(), "HEAD", "small.txt").expect("small blob");
        let large = cat_blob(repo.path(), "HEAD", "large.txt").expect("large blob");

        assert_eq!(small.as_deref(), Some(&b"small\n"[..]));
        assert!(large.is_none());
    }
}
