#!/usr/bin/env python3
"""Architecture guardrails.

These checks intentionally scan source text rather than relying on compile-time
reachability. A stale public vocabulary or bypass surface should be removed, not
left around behind an adapter.
"""
from __future__ import annotations

import argparse
import re
import sys
from dataclasses import dataclass
from datetime import date
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
ALLOW = "PR9-RATCHET-ALLOW"
OWNER_WRITE_PERMIT = "OwnerWritePermit"

# Dated exemptions for flavor code that still reads `proxima_core.*` tables
# directly instead of going through an authorized flavor-read facade. Every
# entry needs an ISO `expires` date and a `reason`; once an entry expires it
# stops suppressing findings and the failure renders the expiration date so
# reviewers see why it started failing.
#
# The authorized-read helpers migrated the three prior entries here (search_chunks.rs,
# open_file_revision.rs, local_git_source.rs) onto
# `proxima::flavor::authorized_*` — production `flavors/code/src` now holds
# zero raw `proxima_core.*` SQL, so this allowlist is empty until the next
# dated exemption is actually needed.
ALLOWLISTED_FLAVOR_CORE_SQL: dict[str, dict[str, str]] = {}


@dataclass(frozen=True)
class Finding:
    path: Path
    line: int
    rule: str
    text: str

    def render(self) -> str:
        rel = self.path.relative_to(ROOT)
        return f"{rel}:{self.line}: {self.rule}: {self.text.strip()}"


def is_text_file(path: Path) -> bool:
    return path.suffix in {".rs", ".sql", ".toml", ".yaml", ".yml"}


def iter_files(*roots: str) -> list[Path]:
    files: list[Path] = []
    for root in roots:
        base = ROOT / root
        if not base.exists():
            continue
        if base.is_file():
            files.append(base)
            continue
        files.extend(
            p
            for p in base.rglob("*")
            if p.is_file()
            and is_text_file(p)
            and "/target/" not in p.as_posix()
            and "/.git/" not in p.as_posix()
        )
    return sorted(files)


def source_files() -> list[Path]:
    return iter_files("crates", "apps", "flavors", "examples")


def production_src_files() -> list[Path]:
    files: list[Path] = []
    for root in ("crates", "apps", "flavors", "examples"):
        base = ROOT / root
        if not base.exists():
            continue
        files.extend(
            p
            for p in base.rglob("src/**/*.rs")
            if p.is_file() and "/target/" not in p.as_posix()
        )
    return sorted(files)


def add_regex_findings(
    findings: list[Finding],
    paths: list[Path],
    pattern: str,
    rule: str,
    *,
    flags: int = 0,
) -> None:
    rx = re.compile(pattern, flags)
    for path in paths:
        try:
            lines = path.read_text(encoding="utf-8").splitlines()
        except UnicodeDecodeError:
            continue
        for line_no, line in enumerate(lines, 1):
            if ALLOW in line:
                continue
            if rx.search(line):
                findings.append(Finding(path, line_no, rule, line))


def check_stale_access(findings: list[Finding]) -> None:
    add_regex_findings(
        findings,
        production_src_files(),
        r"\b(ReadScope|OwnerPrincipalKind|Principal)\b",
        "stale access/authz carrier",
    )


def authz_bearing_files() -> list[Path]:
    paths: list[Path] = []
    direct = [
        ROOT / "crates/core/src/authz.rs",
        ROOT / "crates/core/src/access.rs",
    ]
    paths.extend(p for p in direct if p.exists())
    for root in [
        ROOT / "crates/core/src/engine",
        ROOT / "crates/storage-pg/src/verbs",
        ROOT / "crates/mcp-server/src",
    ]:
        if root.exists():
            paths.extend(p for p in root.rglob("*.rs") if p.is_file())
    return sorted(set(paths))


def check_personality_authz(findings: list[Finding]) -> None:
    add_regex_findings(
        findings,
        authz_bearing_files(),
        r"\b[Pp]ersonality\b|\bpersonality_\w+|\w+_personality\b",
        "personality authz carrier in authorization-bearing module",
    )


def check_storage_resurrection(findings: list[Finding]) -> None:
    add_regex_findings(
        findings,
        source_files(),
        r"\bpub\s+(trait\s+Storage|struct\s+StorageHandle|type\s+StorageHandle)\b",
        "public aggregate Storage resurrection",
    )


def check_runtime_registration(findings: list[Finding]) -> None:
    add_regex_findings(
        findings,
        production_src_files(),
        r"\bpub\s+(?:async\s+)?fn\s+(?:register_.*(?:runtime|dynamic)|install_.*plugin|upload_.*manifest)\b",
        "runtime/dynamic/plugin registration surface",
    )
    add_regex_findings(
        findings,
        iter_files("crates", "apps", "flavors", "examples"),
        r"proxima_core\.(?:tools|tool_invocations)\b|CREATE\s+TABLE\s+proxima_core\.(?:tools|tool_invocations)\b",
        "runtime tool table surface",
        flags=re.IGNORECASE,
    )


# `entity_owner_union()` is the entity-ownership model written in SQL: the
# set every read joins against to ask who owns a memory or a goal. It is
# defined once, in the module below, and interpolated by every caller.
ENTITY_OWNER_UNION_DEF = "crates/storage-pg/src/verbs/query/mod.rs"
ENTITY_OWNER_UNION_TEXT = (
    r"SELECT\s+memory_id\s+AS\s+entity_id,\s*owner_kind,\s*owner_id\s+"
    r"FROM\s+proxima_core\.memories\s+UNION\s+ALL\s+"
    r"SELECT\s+goal_id\s+AS\s+entity_id,\s*owner_kind,\s*owner_id\s+"
    r"FROM\s+proxima_core\.goals"
)


def check_entity_owner_union(findings: list[Finding]) -> None:
    """Restating the union is how the ownership model silently forks.

    A copy is not wrong on the day it is written — it is wrong on the day
    the union gains a third source and only some readers learn about it.
    Interpolate `entity_owner_union()` instead; the SQL is byte-identical,
    and a `SQL-POLICY: fixed-fragment` proof covers the interpolation.
    """
    paths = [
        p
        for p in production_src_files()
        if p.relative_to(ROOT).as_posix() != ENTITY_OWNER_UNION_DEF
    ]
    add_regex_findings(
        findings,
        paths,
        ENTITY_OWNER_UNION_TEXT,
        "restated entity-owner union (interpolate entity_owner_union())",
    )


def iter_rust_string_literals(text: str):
    """Yield ``(start_line, start_offset, literal_text)`` for each Rust string /
    raw-string / byte-string literal in ``text``, in source order.

    A regex applied line-by-line cannot see a raw string whose opening
    delimiter and offending text land on different lines (multi-line SQL
    literals are the norm in this codebase). This walks the source once,
    skipping line comments, block comments, and char literals so their quote
    characters cannot desynchronize the scan, and yields the full span of
    every string literal regardless of how many lines it covers.
    """
    i = 0
    n = len(text)
    line = 1
    while i < n:
        ch = text[i]
        if ch == "\n":
            line += 1
            i += 1
            continue
        if text.startswith("//", i):
            nl = text.find("\n", i)
            i = n if nl == -1 else nl
            continue
        if text.startswith("/*", i):
            end = text.find("*/", i + 2)
            if end == -1:
                break
            line += text.count("\n", i, end)
            i = end + 2
            continue
        raw_match = re.match(r'[bB]?r(#*)"', text[i:])
        if raw_match:
            hashes = raw_match.group(1)
            start = i
            start_line = line
            body_start = i + raw_match.end()
            closer = '"' + hashes
            end = text.find(closer, body_start)
            literal = text[start:] if end == -1 else text[start : end + len(closer)]
            line += literal.count("\n")
            i = n if end == -1 else end + len(closer)
            yield start_line, start, literal
            continue
        str_match = re.match(r'[bB]?"', text[i:])
        if str_match:
            start = i
            start_line = line
            j = i + str_match.end()
            while j < n:
                if text[j] == "\\":
                    j += 2
                    continue
                if text[j] == '"':
                    j += 1
                    break
                j += 1
            literal = text[start:j]
            line += literal.count("\n")
            i = j
            yield start_line, start, literal
            continue
        if ch == "'":
            char_match = re.match(r"'(?:\\.|[^'\\])'", text[i:])
            if char_match:
                i += char_match.end()
                continue
        i += 1


#: Pure `proxima_core` SQL functions a flavor may call from its own query.
#:
#: The rule this supports is that a flavor must not reach core *data* — it
#: goes through an authorized port instead. These are not data: each is
#: IMMUTABLE, STRICT and PARALLEL SAFE, reads no row, enforces no
#: authorization, and returns a pure function of its arguments. They exist to
#: be shared, and a flavor that could not call them would have to restate the
#: definition — which is the drift they were introduced to prevent
#: (`0011_v007.sql`). Anything with a table or view behind it stays denied.
PURE_CORE_SQL_FUNCTIONS = (
    "lexical_scrub",
    "lexical_tsv",
    "lexical_config",
    "lexical_join",
    "lexical_text_array",
    "memory_entity_kind",
)

_PURE_CORE_SQL_CALL = re.compile(
    r"proxima_core\.(?:" + "|".join(PURE_CORE_SQL_FUNCTIONS) + r")\s*\("
)


def flavor_core_sql_hits(text: str) -> list[tuple[int, int, str]]:
    """Return ``(start_line, end_line, literal)`` for every string literal
    containing a schema-qualified ``proxima_core.`` reference.

    A literal dot after `proxima_core` is a schema-qualified SQL identifier
    (`proxima_core.memories`); a colon there is Rust path syntax
    (`proxima_core::verbs`) and must stay allowed everywhere.

    Calls to `PURE_CORE_SQL_FUNCTIONS` are masked out before the check, so a
    literal that references core only to call one of them is not a finding
    while a literal that also names a table still is.
    """
    hits: list[tuple[int, int, str]] = []
    for start_line, _start_offset, literal in iter_rust_string_literals(text):
        if "proxima_core." not in literal:
            continue
        if "proxima_core." not in _PURE_CORE_SQL_CALL.sub("", literal):
            continue
        hits.append((start_line, start_line + literal.count("\n"), literal))
    return hits


def allow_marker_lines(text: str) -> set[int]:
    """Line numbers carrying a PR9-RATCHET-ALLOW marker outside every string
    literal.

    Marker text INSIDE a literal (e.g. a `-- PR9-RATCHET-ALLOW` SQL comment
    within a flagged raw string) must never suppress the finding on that
    literal, otherwise the scanned content could self-authorize.
    """
    literal_spans = [
        (start_offset, start_offset + len(literal))
        for _start_line, start_offset, literal in iter_rust_string_literals(text)
    ]
    marker_lines: set[int] = set()
    for match in re.finditer(re.escape(ALLOW), text):
        if any(start <= match.start() < end for start, end in literal_spans):
            continue
        marker_lines.add(text.count("\n", 0, match.start()) + 1)
    return marker_lines


def check_flavor_core_sql(findings: list[Finding]) -> None:
    flavors = ROOT / "flavors"
    if not flavors.exists():
        return
    paths = [p for p in flavors.rglob("src/**/*.rs") if p.is_file()]
    for path in sorted(paths):
        rel = path.relative_to(ROOT).as_posix()
        text = path.read_text(encoding="utf-8")
        lines = text.splitlines()
        allow_lines = allow_marker_lines(text)
        allowance = ALLOWLISTED_FLAVOR_CORE_SQL.get(rel)
        for start_line, end_line, literal in flavor_core_sql_hits(text):
            if allow_lines & set(range(start_line, end_line + 1)):
                continue
            snippet = lines[start_line - 1].strip() if start_line - 1 < len(lines) else literal.strip()
            if allowance is not None:
                if date.today() <= date.fromisoformat(allowance["expires"]):
                    continue
                findings.append(
                    Finding(
                        path,
                        start_line,
                        "flavor raw proxima_core SQL",
                        f"allowlist expired {allowance['expires']} ({allowance['reason']}): {snippet}",
                    )
                )
                continue
            findings.append(Finding(path, start_line, "flavor raw proxima_core SQL", snippet))


def check_event_source_vocabulary(findings: list[Finding]) -> None:
    module_roots = [
        "crates/core/src",
        "crates/storage-pg/src",
        "crates/storage-pg/migrations/0001_init.sql",
        "crates/mcp-server/src",
        "crates/proxima/src",
        "apps/proxima-mcp/src",
    ]
    add_regex_findings(
        findings,
        iter_files(*module_roots),
        r"\bEventSource\b|\bevent_source\b|\bsource_event\b|\bcore_event\b",
        "EventSource-as-core vocabulary",
    )


def check_tombstone_tool(findings: list[Finding]) -> None:
    module_roots = [
        "crates/core/src/mcp",
        "crates/proxima/src",
        "crates/mcp-server/src",
        "apps/proxima-mcp/src",
    ]
    add_regex_findings(
        findings,
        iter_files(*module_roots),
        r"core_fact:tombstone",
        "legacy core_fact:tombstone production surface",
    )


def check_public_witnesses(findings: list[Finding]) -> None:
    witness_names = (
        "AbandonedOwner",
        "AbandonedSourceScope",
        "AbandonmentObservation",
        "PersonalOwnerDropped",
        "GroupRosterEmpty",
    )
    export_files = [
        ROOT / "crates/core/src/lib.rs",
        ROOT / "crates/proxima/src/lib.rs",
        ROOT / "crates/proxima/src/host.rs",
    ]
    add_regex_findings(
        findings,
        [p for p in export_files if p.exists()],
        r"\bpub\s+use\b.*\b(?:" + "|".join(witness_names) + r")\b",
        "public forgeable compliance witness export",
    )
    compliance = ROOT / "crates/core/src/compliance.rs"
    if compliance.exists():
        lines = compliance.read_text(encoding="utf-8").splitlines()
        public_ctor = re.compile(r"\bpub\s+fn\s+\w*Abandoned\w*\b")
        public_observation = re.compile(r"\bpub\s+enum\s+AbandonmentObservation\b")
        for line_no, line in enumerate(lines, 1):
            if ALLOW in line or "pub(crate)" in line:
                continue
            if public_ctor.search(line) or public_observation.search(line):
                findings.append(Finding(compliance, line_no, "public forgeable compliance witness constructor", line))


def extract_struct_block(text: str, name: str) -> str:
    m = re.search(rf"pub\s+struct\s+{re.escape(name)}\s*\{{", text)
    if not m:
        return ""
    depth = 0
    for idx in range(m.end() - 1, len(text)):
        ch = text[idx]
        if ch == "{":
            depth += 1
        elif ch == "}":
            depth -= 1
            if depth == 0:
                return text[m.start() : idx + 1]
    return text[m.start() :]


def check_caller_supplied_audit(findings: list[Finding]) -> None:
    compliance = ROOT / "crates/core/src/compliance.rs"
    if not compliance.exists():
        return
    text = compliance.read_text(encoding="utf-8")
    request = extract_struct_block(text, "ComplianceEraseRequest")
    forbidden_fields = re.compile(r"\b(operation_id|requester|auth_path|requested_at|audit(?:_context)?)\b")
    for line_no, line in enumerate(request.splitlines(), 1):
        if ALLOW in line:
            continue
        if forbidden_fields.search(line):
            findings.append(Finding(compliance, line_no, "caller-supplied compliance audit metadata", line))
    for line_no, line in enumerate(text.splitlines(), 1):
        if "ComplianceAuditContext" not in text:
            break
        if re.search(r"\bpub\s+fn\s+new\s*\(", line) and "ComplianceAuditContext" not in line:
            # Only lines inside the impl are interesting; a small window keeps
            # false positives out without writing a Rust parser.
            window = "\n".join(text.splitlines()[max(0, line_no - 5) : line_no + 5])
            if "impl ComplianceAuditContext" in window and "pub(crate)" not in line:
                findings.append(Finding(compliance, line_no, "public ComplianceAuditContext constructor", line))


def rust_signature_for(text: str, name: str) -> tuple[int, str] | None:
    match = re.search(
        rf"(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+{re.escape(name)}(?:<[^>{{}}]*>)?\s*\(",
        text,
    )
    if match is None:
        return None
    start = match.start()
    brace = text.find("{", match.end())
    semi = text.find(";", match.end())
    ends = [idx for idx in (brace, semi) if idx != -1]
    if not ends:
        end = len(text)
    else:
        end = min(ends)
    return text.count("\n", 0, start) + 1, text[start:end]


def require_owner_write_permit(
    findings: list[Finding],
    rel_path: str,
    names: list[str],
    *,
    rule: str,
) -> None:
    path = ROOT / rel_path
    if not path.exists():
        findings.append(Finding(path, 1, rule, "expected file missing"))
        return
    text = path.read_text(encoding="utf-8")
    for name in names:
        found = rust_signature_for(text, name)
        if found is None:
            findings.append(Finding(path, 1, rule, f"expected write surface `{name}` missing"))
            continue
        line_no, signature = found
        if OWNER_WRITE_PERMIT not in signature:
            first = signature.splitlines()[0]
            findings.append(Finding(path, line_no, rule, f"{name} lacks &OwnerWritePermit: {first}"))


def check_owner_write_permit_surfaces(findings: list[Finding]) -> None:
    storage_port_methods = {
        "crates/core/src/storage_ports/fact.rs": [
            "ingest_fact_atomic",
            "close_batch",
        ],
        "crates/core/src/storage_ports/mcp.rs": ["persist_mcp_call_atomic"],
        # There is no edge-write surface to guard. An edge is not a thing a
        # caller appends; it is the index row a node write leaves behind, so
        # the permit that guards the node write is the only one there is.
        "crates/core/src/storage_ports/memory.rs": ["author_derived"],
        "crates/core/src/storage_ports/embeddings.rs": ["enqueue_missing_embedding_jobs"],
        "crates/core/src/storage_ports/goals.rs": [
            "create_goal_atomic",
            "transition_goal_atomic",
            "achieve_goal_atomic",
            "modify_goal_atomic",
            "decompose_goal_atomic",
        ],
        "crates/core/src/storage_ports/access.rs": [
            "transfer_to_world",
            "add_group_member",
            "remove_group_member",
        ],
        "crates/core/src/storage_ports/cursors.rs": ["store_source_cursor"],
        "crates/core/src/storage_ports/compliance.rs": [
            "upsert_fact_retention",
            "clear_fact_retention",
            "set_legal_hold",
            "clear_legal_hold",
        ],
    }
    storage_pg_verbs = {
        "crates/storage-pg/src/verbs/fact_ingest.rs": [
            "ingest_fact_atomic",
            "ingest_fact_in_tx",
            "ingest_fact_for_owner_in_tx",
            "ingest_fact",
            "ingest_fact_for_owner",
        ],
        "crates/storage-pg/src/verbs/derive_append.rs": [
            "append_derived_in_tx",
            "append_derived_with_edges_in_tx",
        ],
        "crates/storage-pg/src/verbs/close_batch.rs": ["close_batch"],
        "crates/storage-pg/src/verbs/persist_mcp_call.rs": [
            "persist_mcp_call_atomic",
            "persist_mcp_call_in_tx",
        ],
        "crates/storage-pg/src/verbs/fact_retention.rs": [
            "upsert_fact_retention",
            "clear_fact_retention",
            "set_legal_hold",
            "clear_legal_hold",
        ],
        "crates/storage-pg/src/verbs/fact_embeddings/jobs.rs": ["enqueue_missing_embedding_jobs"],
        "crates/storage-pg/src/verbs/source_cursors.rs": ["store_source_cursor"],
        "crates/storage-pg/src/verbs/goal_write/commands.rs": [
            "create_goal_atomic",
            "transition_goal_atomic",
            "achieve_goal_atomic",
            "modify_goal_atomic",
            "decompose_goal_atomic",
        ],
    }
    for rel_path, names in storage_port_methods.items():
        require_owner_write_permit(
            findings,
            rel_path,
            names,
            rule="storage port write method lacks OwnerWritePermit",
        )
    for rel_path, names in storage_pg_verbs.items():
        require_owner_write_permit(
            findings,
            rel_path,
            names,
            rule="storage-pg write verb lacks OwnerWritePermit",
        )


def run_self_test() -> int:
    """Assert the flavor core SQL detector fires on its committed fixture.

    Runs the raw detection logic (no allowlist, no ALLOW markers) so a
    regression in the whole-literal tokenizer cannot hide behind either
    suppression path. Green requires: the multi-line raw-string evasion case
    detected, the single-line case detected, and `proxima_core::` Rust paths
    not flagged.
    """
    fixture = ROOT / "scripts/fixtures/architecture-guardrails/flavor_core_sql.rs"
    rel = fixture.relative_to(ROOT)
    text = fixture.read_text(encoding="utf-8")
    lines = text.splitlines()
    hits = flavor_core_sql_hits(text)
    for start_line, _end_line, _literal in hits:
        print(f"{rel}:{start_line}: flavor raw proxima_core SQL: {lines[start_line - 1].strip()}")
    failures: list[str] = []
    if not any(
        end_line > start_line and "proxima_core." not in literal.splitlines()[0]
        for start_line, end_line, literal in hits
    ):
        failures.append("multi-line raw-string evasion case not detected")
    if not any(start_line == end_line for start_line, end_line, _literal in hits):
        failures.append("single-line literal case not detected")
    if len(hits) != 2:
        failures.append(
            f"expected exactly 2 flagged literals (`proxima_core::` Rust paths must stay allowed), got {len(hits)}"
        )
    good = "pub async fn write(\n    permit: &OwnerWritePermit,\n) -> Result<(), StorageError> { Ok(()) }"
    bad = "pub async fn write(\n    owner: &Owner,\n) -> Result<(), StorageError> { Ok(()) }"
    good_sig = rust_signature_for(good, "write")
    bad_sig = rust_signature_for(bad, "write")
    if good_sig is None or OWNER_WRITE_PERMIT not in good_sig[1]:
        failures.append("OwnerWritePermit positive signature fixture not recognized")
    if bad_sig is None or OWNER_WRITE_PERMIT in bad_sig[1]:
        failures.append("OwnerWritePermit missing-signature fixture not detected")
    if failures:
        for failure in failures:
            print(f"self-test failed: {failure}: {rel}", file=sys.stderr)
        return 1
    print(f"self-test detected {len(hits)} flavor raw proxima_core SQL literals", file=sys.stderr)
    return 0


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="assert the flavor core SQL detector fires on its fixture; exits zero when it does",
    )
    args = parser.parse_args()

    if args.self_test:
        return run_self_test()

    findings: list[Finding] = []
    check_stale_access(findings)
    check_personality_authz(findings)
    check_storage_resurrection(findings)
    check_runtime_registration(findings)
    check_entity_owner_union(findings)
    check_flavor_core_sql(findings)
    check_event_source_vocabulary(findings)
    check_tombstone_tool(findings)
    check_public_witnesses(findings)
    check_caller_supplied_audit(findings)
    check_owner_write_permit_surfaces(findings)

    if findings:
        print("architecture guardrail violations:", file=sys.stderr)
        for finding in findings:
            print(f"  {finding.render()}", file=sys.stderr)
        return 1
    print("architecture guardrails passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
