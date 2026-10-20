//! Serving code runs no session-scoped SQL (#493).
//!
//! Behind a transaction-mode pooler a server connection is shared between
//! clients transaction by transaction. A session advisory lock or a session
//! `SET` outlives the transaction that took it and lands on whichever client
//! is handed that connection next. Serving code therefore uses
//! transaction-scoped forms only: `pg_advisory_xact_lock`, `SET LOCAL`,
//! `set_config(.., true)`, table locks, lease rows. Flagged: session
//! advisory locks; a `SET` that is not `LOCAL`, `TRANSACTION` or
//! `CONSTRAINTS` (`SET ROLE` and `SET SESSION ..` included); `RESET`;
//! `DISCARD`; `LISTEN` and `UNLISTEN`; `PREPARE` (a named prepared
//! statement lives in the session); and a `set_config` whose third argument
//! is anything but the literal `true`. Keywords and function names match in
//! any case, and a call may have whitespace before its `(`. A statement may sit anywhere
//! in a multi-statement string or a PL/pgSQL body, after any whitespace.
//!
//! The migration runners are the exception: migrations run on a direct (or
//! session-pooled) connection (docs/operate.md), and the test kit is not
//! serving code. This scans the string literals of the non-test Rust sources
//! of the workspace, and the SQL definitions the storage adapter installs
//! (`crates/storage-pg/definitions/*.sql`: functions run on serving
//! connections).

use std::path::{Path, PathBuf};

/// Files allowed to hold session-scoped SQL, with the snippet each may hold
/// (`None`: any). Each entry must still match something, so the list cannot
/// go stale.
const ALLOWED: &[(&str, Option<&str>)] = &[
    // The runtime migration orchestrator: whole-run lock and session setup on
    // its direct connection.
    ("crates/proxima/src/migrations.rs", None),
    // The storage migration runner's direct connection.
    (
        "crates/storage-pg/src/lib.rs",
        Some("SET statement_timeout = 0"),
    ),
];

/// Session-level advisory lock functions, matched as whole names followed by
/// `(`, whitespace allowed between.
const SESSION_LOCKS: &[&str] = &[
    "pg_advisory_lock",
    "pg_advisory_lock_shared",
    "pg_try_advisory_lock",
    "pg_try_advisory_lock_shared",
    "pg_advisory_unlock",
    "pg_advisory_unlock_shared",
    "pg_advisory_unlock_all",
];

/// The word after `SET` that makes it transaction-scoped.
const TRANSACTION_SETS: &[&str] = &["LOCAL", "TRANSACTION", "CONSTRAINTS"];

/// Words after which a new statement starts inside a PL/pgSQL body.
const BLOCK_WORDS: &[&str] = &["BEGIN", "THEN", "ELSE", "LOOP", "DECLARE"];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/storage-pg sits two levels below the workspace root")
        .to_path_buf()
}

fn is_test_source(relative: &str) -> bool {
    relative.contains("/tests/")
        || relative.ends_with("_tests.rs")
        || relative.ends_with("/tests.rs")
        || relative.contains("test_fixtures")
        || relative.contains("testkit")
}

fn sources(dir: &Path, extension: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if name != "target" && !name.starts_with('.') {
                sources(&path, extension, out);
            }
        } else if name.ends_with(extension) {
            out.push(path);
        }
    }
}

/// The string literals of Rust source `text`: the byte offset each body
/// starts at, and the body as written (escapes are not decoded). Comments
/// and character literals are skipped, so a `'"'` or a commented-out query
/// opens no string.
fn string_literals(text: &str) -> Vec<(usize, &str)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'/' if bytes.get(at + 1) == Some(&b'/') => {
                at = text[at..].find('\n').map_or(bytes.len(), |end| at + end);
            }
            b'/' if bytes.get(at + 1) == Some(&b'*') => {
                at = text[at + 2..]
                    .find("*/")
                    .map_or(bytes.len(), |end| at + 2 + end + 2);
            }
            b'\'' => {
                // A char literal ('x', '\n', '"'); otherwise a lifetime.
                if bytes.get(at + 1) == Some(&b'\\') {
                    at = text[at + 2..]
                        .find('\'')
                        .map_or(bytes.len(), |end| at + 2 + end + 1);
                } else if bytes.get(at + 2) == Some(&b'\'') {
                    at += 3;
                } else {
                    at += 1;
                }
            }
            b'r' if matches!(bytes.get(at + 1), Some(b'"' | b'#'))
                && (at == 0
                    || !(bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'_')) =>
            {
                let hashes = bytes[at + 1..].iter().take_while(|&&b| b == b'#').count();
                let open = at + 1 + hashes;
                if bytes.get(open) != Some(&b'"') {
                    at += 1;
                    continue;
                }
                let close = format!("\"{}", "#".repeat(hashes));
                let end = text[open + 1..]
                    .find(&close)
                    .map_or(bytes.len(), |end| open + 1 + end);
                out.push((open + 1, &text[open + 1..end]));
                at = end + close.len();
            }
            b'"' => {
                let mut end = at + 1;
                while end < bytes.len() && bytes[end] != b'"' {
                    end += if bytes[end] == b'\\' { 2 } else { 1 };
                }
                let end = end.min(bytes.len());
                out.push((at + 1, &text[at + 1..end]));
                at = end + 1;
            }
            _ => at += 1,
        }
    }
    out
}

/// `sql` with `--` comments blanked (same length, so offsets hold) and the
/// header of every `CREATE FUNCTION`/`PROCEDURE` up to its body blanked: a
/// `SET` clause there is scoped to the function's calls.
fn without_comments_and_headers(sql: &str) -> String {
    let mut out: String = sql
        .split_inclusive('\n')
        .map(|line| match line.find("--") {
            Some(at) => {
                let blank: String = line[at..]
                    .chars()
                    .map(|c| if c == '\n' { '\n' } else { ' ' })
                    .collect();
                format!("{}{blank}", &line[..at])
            }
            None => line.to_owned(),
        })
        .collect();
    let upper = out.to_ascii_uppercase();
    let mut headers = Vec::new();
    let mut from = 0;
    while let Some(found) = upper[from..].find("CREATE ") {
        let start = from + found;
        let rest = &upper[start..];
        let words: Vec<&str> = rest.split_ascii_whitespace().take(4).collect();
        let is_routine = words
            .iter()
            .any(|word| *word == "FUNCTION" || *word == "PROCEDURE");
        let body = body_opener(rest);
        match (is_routine, body) {
            (true, Some(body)) => {
                headers.push((start, start + body));
                from = start + body;
            }
            _ => from = start + "CREATE ".len(),
        }
    }
    for (start, end) in headers {
        out.replace_range(start..end, &" ".repeat(end - start));
    }
    out
}

/// The offset of the `AS` that opens a routine body (`AS $$`, `AS $tag$`,
/// `AS '..'`) in `upper`, the routine's header.
fn body_opener(upper: &str) -> Option<usize> {
    let bytes = upper.as_bytes();
    upper.match_indices("AS").map(|(at, _)| at).find(|&at| {
        let before = at == 0 || !is_word_byte(bytes[at - 1]);
        let after = upper[at + 2..].trim_start();
        before
            && bytes.get(at + 2).is_some_and(u8::is_ascii_whitespace)
            && after.starts_with(['$', '\''])
    })
}

/// Session-scoped SQL in one SQL text (a string literal's body or a `.sql`
/// file): each hit with the byte offset it starts at.
fn session_sql(sql: &str) -> Vec<(usize, String)> {
    let cleaned = without_comments_and_headers(sql);
    let upper = cleaned.to_ascii_uppercase();
    let mut hits = Vec::new();
    let lower = cleaned.to_ascii_lowercase();
    for lock in SESSION_LOCKS {
        for (at, _) in calls(&lower, lock) {
            hits.push((at, format!("{lock}(")));
        }
    }
    for (at, args) in calls(&lower, "set_config") {
        if !set_config_is_local(&cleaned[args..]) {
            hits.push((at, "set_config(.., not true)".to_owned()));
        }
    }
    for start in statement_starts(&upper) {
        if let Some(hit) = session_statement(&cleaned[start..]) {
            let text = &cleaned[start..];
            let leading = text.len() - text.trim_start().len();
            hits.push((start + leading, hit));
        }
    }
    hits.sort();
    hits.dedup();
    hits
}

/// The length of the whitespace `text` opens with, a Rust string body's
/// escapes (`\n`, `\t`, `\r`) and line continuations included.
fn space_len(text: &str) -> usize {
    let mut rest = text;
    loop {
        let trimmed = rest.trim_start_matches(|c: char| c.is_ascii_whitespace());
        let after = ["\\n", "\\t", "\\r", "\\\n"]
            .iter()
            .find_map(|escape| trimmed.strip_prefix(escape));
        match after {
            Some(after) => rest = after,
            None => return text.len() - trimmed.len(),
        }
    }
}

/// Calls of the function `name` in `lower`: where the name starts, and where
/// its arguments start (after the `(`). The name must stand alone (`.` may
/// qualify it) and only whitespace may separate it from the `(`.
fn calls(lower: &str, name: &str) -> Vec<(usize, usize)> {
    let bytes = lower.as_bytes();
    lower
        .match_indices(name)
        .filter_map(|(at, _)| {
            if at > 0 && is_word_byte(bytes[at - 1]) {
                return None;
            }
            let after = &lower[at + name.len()..];
            let open = space_len(after);
            after[open..]
                .starts_with('(')
                .then(|| (at, at + name.len() + open + 1))
        })
        .collect()
}

/// Where a statement can start: the text's start, after each `;`, after a
/// PL/pgSQL block word, and after a dollar-quote opener.
fn statement_starts(upper: &str) -> Vec<usize> {
    let bytes = upper.as_bytes();
    let mut starts = vec![0];
    for (at, c) in upper.char_indices() {
        if c == ';' {
            starts.push(at + 1);
        }
        if c == '$' {
            // `$$` or `$tag$`: the position after the closing `$`.
            if let Some(len) = upper[at + 1..].find('$')
                && upper[at + 1..at + 1 + len]
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !upper[at + 1..at + 1 + len].starts_with(|c: char| c.is_ascii_digit())
            {
                starts.push(at + 1 + len + 1);
            }
        }
    }
    for word in BLOCK_WORDS {
        for (at, _) in upper.match_indices(word) {
            let end = at + word.len();
            let before_ok = at == 0 || !is_word_byte(bytes[at - 1]);
            let after_ok = bytes
                .get(end)
                .is_none_or(|&b| b.is_ascii_whitespace() || b == b'\\');
            if before_ok && after_ok {
                starts.push(end);
            }
        }
    }
    starts.sort_unstable();
    starts.dedup();
    starts
}

const fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// The words of `statement`. Whitespace in a Rust string body includes its
/// escapes (`\n`, `\t`, `\r`) and line continuations (`\` at a line end).
fn tokens(statement: &str) -> Vec<&str> {
    let mut words = Vec::new();
    let mut rest = statement;
    loop {
        rest = rest.trim_start_matches(|c: char| c.is_ascii_whitespace());
        let escape = rest
            .strip_prefix("\\n")
            .or_else(|| rest.strip_prefix("\\t"))
            .or_else(|| rest.strip_prefix("\\r"))
            .or_else(|| rest.strip_prefix('\\'));
        if let Some(after) = escape {
            rest = after;
            continue;
        }
        if rest.is_empty() {
            return words;
        }
        let end = rest
            .find(|c: char| c.is_ascii_whitespace() || c == '\\')
            .unwrap_or(rest.len());
        words.push(&rest[..end]);
        rest = &rest[end..];
    }
}

/// The session-scoped statement `text` opens with, if any.
///
/// Any `SET` that is not `SET LOCAL`, `SET TRANSACTION` or `SET CONSTRAINTS`
/// is session-scoped. Prose in a message string can open with the same words
/// ("set 0 to keep", "reset marker"), so a keyword written in lower case
/// counts only in a form prose does not take: `SET` with a session form
/// (`role`, `session`, `time zone`, `names`, `schema`) or an assignment to a
/// lower-case or quoted setting name; `RESET` of a setting-like name;
/// `DISCARD` of what it discards; `LISTEN`/`UNLISTEN` of one channel as the
/// whole statement; `PREPARE name AS` or `PREPARE name(`. Written in upper
/// case, `SET` is always a statement.
fn session_statement(text: &str) -> Option<String> {
    let statement = text.split(';').next().unwrap_or_default();
    let words = tokens(statement);
    let (first, rest) = words.split_first()?;
    let keyword = first.to_ascii_uppercase();
    let upper_rest: Vec<String> = rest.iter().map(|word| word.to_ascii_uppercase()).collect();
    let setting_like = |word: &str| {
        word.eq_ignore_ascii_case("all")
            || word == "*"
            || (word.contains(['_', '.'])
                && word
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.'))
    };
    let identifier = |word: &str| {
        !word.is_empty()
            && word
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '*' || c == '"')
    };
    let shouted = |word: &str| word.chars().all(|c| c.is_ascii_uppercase());
    let hit = match keyword.as_str() {
        "SET" => {
            let second = upper_rest.first()?;
            if TRANSACTION_SETS.contains(&second.as_str()) {
                return None;
            }
            let session_form = ["ROLE", "SESSION", "NAMES", "SCHEMA"].contains(&second.as_str())
                || (second == "TIME" && upper_rest.get(1).is_some_and(|zone| zone == "ZONE"));
            shouted(first) || session_form || assigns_a_setting(rest)
        }
        "RESET" => match upper_rest.as_slice() {
            [role] if role == "ROLE" => true,
            [session, authorization] => session == "SESSION" && authorization == "AUTHORIZATION",
            [name] => shouted(first) || (setting_like(rest[0]) && name != "ROLE"),
            _ => false,
        },
        "DISCARD" => {
            upper_rest.len() == 1
                && ["ALL", "PLANS", "SEQUENCES", "TEMP", "TEMPORARY"]
                    .contains(&upper_rest[0].as_str())
        }
        "LISTEN" | "UNLISTEN" => rest.len() == 1 && identifier(rest[0]),
        "PREPARE" => match rest {
            [name, ..] if name.contains('(') => {
                identifier(name.split('(').next().unwrap_or_default()) && !name.starts_with('(')
            }
            [name, next, ..] => {
                identifier(name)
                    && !name.eq_ignore_ascii_case("transaction")
                    && (next.eq_ignore_ascii_case("as") || next.starts_with('('))
            }
            _ => false,
        },
        _ => false,
    };
    hit.then(|| words.iter().take(4).copied().collect::<Vec<_>>().join(" "))
}

/// `name = ..`, `name=..` or `name TO ..` after `SET`, the name a lower-case
/// setting (`search_path`, `app.owner`) or a quoted one (`"search_path"`): a
/// `SET` statement, not prose that happens to open with the word.
fn assigns_a_setting(after_set: &[&str]) -> bool {
    let Some((first, rest)) = after_set.split_first() else {
        return false;
    };
    let (name, inline_value) = match first.split_once('=') {
        Some((name, _)) => (name, true),
        None => (*first, false),
    };
    let quoted = name.len() > 2 && name.starts_with('"') && name.ends_with('"');
    let is_setting = quoted
        || (name.starts_with(|c: char| c.is_ascii_lowercase() || c == '_')
            && name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '.'));
    is_setting
        && (inline_value
            || rest
                .first()
                .is_some_and(|next| next.starts_with('=') || next.eq_ignore_ascii_case("to")))
}

/// Whether the `set_config(` call whose arguments start `args` passes the
/// literal `true` as its third argument.
fn set_config_is_local(args: &str) -> bool {
    let mut depth = 0usize;
    let mut quoted = false;
    let mut commas = Vec::new();
    let mut close = None;
    for (at, c) in args.char_indices() {
        match c {
            '\'' => quoted = !quoted,
            '(' if !quoted => depth += 1,
            ')' if !quoted && depth == 0 => {
                close = Some(at);
                break;
            }
            ')' if !quoted => depth -= 1,
            ',' if !quoted && depth == 0 => commas.push(at),
            _ => {}
        }
    }
    let (Some(close), [_, second]) = (close, commas.as_slice()) else {
        return false;
    };
    tokens(&args[second + 1..close]) == ["true"] || tokens(&args[second + 1..close]) == ["TRUE"]
}

fn line_of(text: &str, offset: usize) -> usize {
    text[..offset].matches('\n').count() + 1
}

#[test]
fn serving_code_runs_no_session_scoped_sql() {
    let root = workspace_root();
    let mut files = Vec::new();
    for top in ["apps", "crates", "flavors", "tools"] {
        sources(&root.join(top), ".rs", &mut files);
    }
    assert!(files.len() > 100, "the scan found the workspace sources");
    let mut definitions = Vec::new();
    sources(
        &root.join("crates/storage-pg/definitions"),
        ".sql",
        &mut definitions,
    );
    assert!(definitions.len() >= 3, "the scan found the SQL definitions");
    let mut found = Vec::new();
    let mut used = vec![false; ALLOWED.len()];
    for path in files.into_iter().chain(definitions) {
        let relative = path
            .strip_prefix(&root)
            .expect("under the root")
            .to_string_lossy()
            .replace('\\', "/");
        let is_sql = path.extension().is_some_and(|extension| extension == "sql");
        if !is_sql && (!relative.contains("/src/") || is_test_source(&relative)) {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("readable source");
        let bodies = if is_sql {
            vec![(0, text.as_str())]
        } else {
            string_literals(&text)
        };
        let lines: Vec<&str> = text.lines().collect();
        for (start, body) in bodies {
            for (offset, hit) in session_sql(body) {
                let number = line_of(&text, start + offset);
                let line = lines.get(number - 1).copied().unwrap_or_default();
                let allowed = ALLOWED.iter().position(|(file, snippet)| {
                    *file == relative && snippet.is_none_or(|snippet| line.contains(snippet))
                });
                match allowed {
                    Some(entry) => used[entry] = true,
                    None => found.push(format!("{relative}:{number}: {hit}")),
                }
            }
        }
    }
    found.sort();
    found.dedup();
    assert!(
        found.is_empty(),
        "session-scoped SQL in serving code; use the transaction-scoped form:\n{}",
        found.join("\n")
    );
    let stale: Vec<_> = ALLOWED
        .iter()
        .zip(used)
        .filter(|(_, used)| !used)
        .map(|((file, snippet), _)| format!("{file} {snippet:?}"))
        .collect();
    assert!(
        stale.is_empty(),
        "allowlist entries match nothing: {stale:?}"
    );
}

/// The session SQL `source` (Rust) holds in its string literals.
fn rust_hits(source: &str) -> Vec<String> {
    string_literals(source)
        .into_iter()
        .flat_map(|(_, body)| session_sql(body))
        .map(|(_, hit)| hit)
        .collect()
}

#[test]
fn the_scan_recognizes_session_and_transaction_forms() {
    for session in [
        r#"sqlx::query("SELECT pg_advisory_lock($1)")"#,
        r#"sqlx::query("SELECT pg_try_advisory_lock($1)")"#,
        r#"sqlx::query("SELECT pg_advisory_unlock_all()")"#,
        r#"sqlx::query("SET statement_timeout = 0")"#,
        r#"sqlx::query("set search_path to x")"#,
        r#"sqlx::query("SELECT set_config('app.proxima_scope', 'platform', false)")"#,
        // Session forms other than an assignment.
        r#"sqlx::query("SET ROLE proxima_runtime")"#,
        r#"sqlx::query("SET SESSION AUTHORIZATION proxima_runtime")"#,
        r#"sqlx::query("SET SESSION statement_timeout = 0")"#,
        r#"sqlx::query("SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY")"#,
        r#"sqlx::query("RESET ALL")"#,
        r#"sqlx::query("RESET statement_timeout")"#,
        r#"sqlx::query("DISCARD ALL")"#,
        r#"sqlx::query("LISTEN proxima_writes")"#,
        r#"sqlx::query("UNLISTEN *")"#,
        // SET followed by a tab or a line break.
        r#"sqlx::query("SET\tstatement_timeout = 0")"#,
        r#"sqlx::query("SET
                       search_path TO public")"#,
        r#"sqlx::query("SET\n search_path TO public")"#,
        r#"sqlx::query("SET \
                       statement_timeout = 0")"#,
        // A later statement of a multi-statement string.
        r#"sqlx::raw_sql("SELECT 1; SET statement_timeout = 0")"#,
        r#"sqlx::raw_sql("BEGIN; SET lock_timeout = '1s'; COMMIT")"#,
        // set_config not passed a literal true: bound, or false on a later line.
        r#"sqlx::query("SELECT set_config('app.owner', $1, $2)")"#,
        r#"sqlx::query("SELECT set_config(
                           'app.owner',
                           $1,
                           false
                       )")"#,
        r#"sqlx::query(r"SELECT set_config('app.owner', $1, false)")"#,
    ] {
        assert!(!rust_hits(session).is_empty(), "missed: {session}");
    }
    for sql in [
        "SET search_path TO public;",
        "SELECT 1;\nSET statement_timeout = 0;",
        "CREATE FUNCTION f() RETURNS void LANGUAGE plpgsql AS $$\nBEGIN\n    SET search_path = x;\nEND;\n$$;",
        "DO $$ BEGIN IF true THEN RESET ALL; END IF; END $$;",
        // The first header ends at its own body, not at a later ` AS `.
        "CREATE FUNCTION f() RETURNS int LANGUAGE sql\nAS $$ SELECT 1 $$;\nCREATE FUNCTION g() RETURNS void LANGUAGE plpgsql\nAS $$\nBEGIN\n    SET search_path = x;\n    PERFORM 1 AS y;\nEND;\n$$;",
        "CREATE FUNCTION f() RETURNS void LANGUAGE plpgsql AS $$\nBEGIN\n    PERFORM set_config('app.owner', '', false);\nEND;\n$$;",
    ] {
        assert!(!session_sql(sql).is_empty(), "missed SQL: {sql}");
    }
    for transaction in [
        r#"sqlx::query("SELECT pg_advisory_xact_lock($1)")"#,
        r#"sqlx::query("SET LOCAL lock_timeout = '5s'")"#,
        r#"sqlx::query("SET\tLOCAL lock_timeout = '5s'")"#,
        r#"sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")"#,
        r#"sqlx::query("SET CONSTRAINTS ALL DEFERRED")"#,
        r#"sqlx::query("SELECT set_config('app.proxima_scope', 'platform', true)")"#,
        r#"sqlx::query("SELECT set_config(
                           'app.owner',
                           $1,
                           true
                       )")"#,
        r#"sqlx::query("UPDATE t\n   SET expires_at = now()")"#,
        r#"sqlx::query("INSERT INTO t VALUES (1) ON CONFLICT (k) DO UPDATE SET v = 1")"#,
        r#"// sqlx::query("SELECT pg_advisory_lock(1)") once did this"#,
        r#"format!("set embed_client or embedding_router")"#,
        r#"format!("reset the drain before {}", x)"#,
        r#"format!("listen on {bind}")"#,
        r#"let quote = '"'; let s = "plain text";"#,
    ] {
        assert!(rust_hits(transaction).is_empty(), "flagged: {transaction}");
    }
    for sql in [
        "CREATE OR REPLACE FUNCTION f()\n RETURNS void\n LANGUAGE plpgsql\n SECURITY DEFINER\n SET search_path TO 'pg_catalog'\nAS $function$\nBEGIN\n    PERFORM set_config('app.proxima_scope', 'platform', true);\nEND;\n$function$;",
        "-- SET search_path TO public;\nSELECT 1;",
        "UPDATE t\n   SET v = 1;",
    ] {
        assert!(session_sql(sql).is_empty(), "flagged SQL: {sql}");
    }
}

/// Any `SET` but `LOCAL`/`TRANSACTION`/`CONSTRAINTS`, keywords in any case,
/// `PREPARE`, and calls with whitespace before their `(`.
#[test]
fn the_scan_recognizes_any_set_any_case_and_spaced_calls() {
    for session in [
        r#"sqlx::query("SET STATEMENT_TIMEOUT = 0")"#,
        r#"sqlx::query("SET TIME ZONE 'UTC'")"#,
        r#"sqlx::query("set time zone 'UTC'")"#,
        r#"sqlx::query("SET NAMES 'UTF8'")"#,
        r#"sqlx::query("SET SCHEMA 'proxima_core'")"#,
        r#"sqlx::query("SET \"search_path\" = x")"#,
        r##"sqlx::query(r#"set "search_path" = x"#)"##,
        r#"sqlx::query("discard all")"#,
        r#"sqlx::query("listen x")"#,
        r#"sqlx::query("unlisten *")"#,
        r#"sqlx::query("PREPARE fetch_owner AS SELECT 1")"#,
        r#"sqlx::query("prepare fetch_owner(uuid) AS SELECT $1")"#,
        r#"sqlx::query("SELECT pg_advisory_lock ($1)")"#,
        r#"sqlx::query("SELECT pg_try_advisory_lock_shared
($1)")"#,
        r#"sqlx::query("SELECT pg_advisory_unlock_all ()")"#,
        r#"sqlx::query("SELECT set_config ('app.owner', $1, false)")"#,
        r#"sqlx::query("SELECT pg_catalog.set_config
('app.owner', $1, $2)")"#,
    ] {
        assert!(!rust_hits(session).is_empty(), "missed: {session}");
    }
    for not_session in [
        r#"sqlx::query("SELECT set_config ('app.owner', $1, true)")"#,
        r#"sqlx::query("SELECT pg_advisory_xact_lock ($1)")"#,
        r#"sqlx::query("PREPARE TRANSACTION 'x'")"#,
        r#"format!("prepare the upload for {owner}")"#,
        r#"format!("discard the draft")"#,
        r#"format!("listen on {bind}")"#,
        r#"format!("set PROXIMA_TOOL_PROFILE=memory for agent-facing hosts")"#,
        r#"format!("is {seconds}s, under the floor; set 0 to keep records")"#,
        r#"let name = "my_pg_advisory_lock_helper";"#,
    ] {
        assert!(rust_hits(not_session).is_empty(), "flagged: {not_session}");
    }
}
