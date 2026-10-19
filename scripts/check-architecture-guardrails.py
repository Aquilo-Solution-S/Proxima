#!/usr/bin/env python3
"""Architecture guardrails.

These checks intentionally scan source text rather than relying on compile-time
reachability. A stale public vocabulary or bypass surface should be removed, not
left around behind an adapter.
"""
from __future__ import annotations

import argparse
import re
import subprocess
import sys
from dataclasses import dataclass
from datetime import date
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
ALLOW = "ARCH-GUARDRAIL-ALLOW"
OWNER_WRITE_PERMIT = "OwnerWritePermit"

# Dated exemptions for flavor code that still reads `proxima_core.*` tables
# directly instead of going through an authorized flavor-read facade. Every
# entry needs an ISO `expires` date and a `reason`; once an entry expires it
# stops suppressing findings and the failure renders the expiration date so
# reviewers see why it started failing.
#
# A flavor under flavors/ holds zero raw `proxima_core.*` SQL in its `src` —
# every flavor read goes through `proxima::flavor::authorized_*` — so the allowlist
# is empty.
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
    """Line numbers carrying an ARCH-GUARDRAIL-ALLOW marker outside every
    string literal.

    Marker text INSIDE a literal (e.g. a `-- ARCH-GUARDRAIL-ALLOW` SQL comment
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
        "crates/storage-pg/migrations/0033_baseline.sql",
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
        "public forgeable abandonment witness export",
    )
    inverse = ROOT / "crates/core/src/owner_inverse.rs"
    if inverse.exists():
        lines = inverse.read_text(encoding="utf-8").splitlines()
        public_ctor = re.compile(r"\bpub\s+fn\s+\w*Abandoned\w*\b")
        public_observation = re.compile(r"\bpub\s+enum\s+AbandonmentObservation\b")
        for line_no, line in enumerate(lines, 1):
            if ALLOW in line or "pub(crate)" in line:
                continue
            if public_ctor.search(line) or public_observation.search(line):
                findings.append(Finding(inverse, line_no, "public forgeable abandonment witness constructor", line))


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
    inverse = ROOT / "crates/core/src/owner_inverse.rs"
    if not inverse.exists():
        return
    text = inverse.read_text(encoding="utf-8")
    request = extract_struct_block(text, "OwnerEraseRequest")
    forbidden_fields = re.compile(r"\b(operation_id|requester|auth_path|requested_at|audit(?:_context)?)\b")
    for line_no, line in enumerate(request.splitlines(), 1):
        if ALLOW in line:
            continue
        if forbidden_fields.search(line):
            findings.append(Finding(inverse, line_no, "caller-supplied erase audit metadata", line))
    for line_no, line in enumerate(text.splitlines(), 1):
        if "OwnerEraseContext" not in text:
            break
        if re.search(r"\bpub\s+fn\s+new\s*\(", line) and "OwnerEraseContext" not in line:
            # Only lines inside the impl are interesting; a small window keeps
            # false positives out without writing a Rust parser.
            window = "\n".join(text.splitlines()[max(0, line_no - 5) : line_no + 5])
            if "impl OwnerEraseContext" in window and "pub(crate)" not in line:
                findings.append(Finding(inverse, line_no, "public OwnerEraseContext constructor", line))


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
        # Membership evidence contains the sealed owner-write capability plus
        # endpoint authority; other writes still require OwnerWritePermit.
        required = (
            "MembershipWritePermit"
            if rel_path == "crates/core/src/storage_ports/access.rs"
            and name in {
                "add_group_member", "remove_group_member",
                "remove_group_member_relation", "replace_group_member_relation",
            }
            else OWNER_WRITE_PERMIT
        )
        if required not in signature:
            first = signature.splitlines()[0]
            findings.append(Finding(path, line_no, rule, f"{name} lacks &{required}: {first}"))


def rust_function_signatures(text: str, names: set[str]):
    """Yield ``(name, line, start, signature)`` for selected Rust functions."""
    if not names:
        return
    name_pattern = "|".join(re.escape(name) for name in sorted(names))
    rx = re.compile(
        rf"(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+(?P<name>{name_pattern})"
        rf"(?:<[^>{{}}]*>)?\s*\("
    )
    for match in rx.finditer(text):
        brace = text.find("{", match.end())
        semi = text.find(";", match.end())
        ends = [idx for idx in (brace, semi) if idx != -1]
        end = min(ends) if ends else len(text)
        yield (
            match.group("name"),
            text.count("\n", 0, match.start()) + 1,
            match.start(),
            text[match.start() : end],
        )


def directly_fixture_gated(text: str, start: int) -> bool:
    """Whether a function has the fixture cfg attribute immediately above it."""
    lines = text[:start].splitlines()
    index = len(lines) - 1
    while index >= 0 and (not lines[index].strip() or lines[index].lstrip().startswith("//")):
        index -= 1
    return index >= 0 and lines[index].strip() == '#[cfg(any(test, feature = "test-fixtures"))]'


def check_authorized_fact_surface(findings: list[Finding]) -> None:
    """Keep the production Fact door behind Engine's authorized carrier.

    The legacy raw helper may exist only as the directly cfg-gated fixture
    adapter in the core port. The PG verb has no raw compatibility alias.
    """
    positive = {
        "crates/core/src/storage_ports/fact.rs": "ingest_authorized_fact_atomic",
        "crates/storage-pg/src/verbs/fact_ingest.rs": "ingest_authorized_fact_atomic",
    }
    for rel_path, name in positive.items():
        path = ROOT / rel_path
        if not path.exists():
            findings.append(Finding(path, 1, "authorized Fact write surface missing", name))
            continue
        text = path.read_text(encoding="utf-8")
        found = rust_signature_for(text, name)
        if found is None:
            findings.append(Finding(path, 1, "authorized Fact write surface missing", name))
        elif "AuthorizedFactWrite" not in found[1]:
            findings.append(
                Finding(
                    path,
                    found[0],
                    "authorized Fact write surface lacks AuthorizedFactWrite",
                    found[1].splitlines()[0],
                )
            )

    raw_paths = [
        ROOT / "crates/core/src/storage_ports/fact.rs",
        ROOT / "crates/storage-pg/src/verbs/fact_ingest.rs",
    ]
    raw_names = {"ingest_fact_atomic", "ingest_authorized_fact_atomic"}
    for path in raw_paths:
        if not path.exists():
            continue
        text = path.read_text(encoding="utf-8")
        for name, line_no, start, signature in rust_function_signatures(text, raw_names):
            if "OwnerWritePermit" not in signature or "FactWriteCommand" not in signature:
                continue
            if not directly_fixture_gated(text, start):
                findings.append(
                    Finding(
                        path,
                        line_no,
                        "raw Fact write surface outside fixture cfg",
                        f"{name} accepts OwnerWritePermit and FactWriteCommand",
                    )
                )


def check_owner_write_permit_surfaces(findings: list[Finding]) -> None:
    storage_port_methods = {
        # There is no MCP-call write surface to guard: the engine logs a call
        # through the governed typed-Fact path, so `storage_ports/mcp.rs` is a
        # read port only.
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
            "transfer_to_owner",
            "add_group_member",
            "remove_group_member",
            "remove_group_member_relation",
            "replace_group_member_relation",
        ],
        "crates/core/src/storage_ports/cursors.rs": ["store_source_cursor"],
    }
    storage_pg_verbs = {
        "crates/storage-pg/src/verbs/derive_append.rs": [
            "append_derived_with_content_payloads_in_tx",
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
    check_authorized_fact_surface(findings)


# Acceptance 1 of #473: the code flavor left the repository and stays in git
# history only. This pattern names it, so the scan below skips this file (and
# only this file) besides the generated release notes.
REMOVED_FLAVOR_PATTERN = r"proxima[-_]code|flavors/code"
REMOVED_FLAVOR_EXEMPT = {"CHANGELOG.md", "scripts/check-architecture-guardrails.py"}


def git_lines(*args: str) -> list[str]:
    result = subprocess.run(
        ["git", *args], cwd=ROOT, capture_output=True, text=True, check=False
    )
    if result.returncode not in (0, 1):
        raise SystemExit(f"git {' '.join(args)} failed: {result.stderr.strip()}")
    return result.stdout.splitlines()


def removed_flavor_hits(path: str, text: str) -> list[tuple[int, str]]:
    """Lines of `text` naming a removed flavor, unless `path` is exempt."""
    if path in REMOVED_FLAVOR_EXEMPT:
        return []
    rx = re.compile(REMOVED_FLAVOR_PATTERN, re.IGNORECASE)
    return [(n, line) for n, line in enumerate(text.splitlines(), 1) if rx.search(line)]


def flavor_vocabulary(package_names: list[str]) -> list[str]:
    """The PG schema, flavor id and tool prefix each in-tree flavor owns.

    A flavor's package name `proxima-x` is its flavor id; its schema is
    `proxima_x` and its tools are named `proxima-x_*`.
    """
    words: list[str] = []
    for name in package_names:
        words.extend([name.replace("-", "_"), name, f"{name}_"])
    return words


def crate_src_flavor_hits(text: str, vocabulary: list[str]) -> list[tuple[int, str]]:
    if not vocabulary:
        return []
    rx = re.compile("|".join(re.escape(word) for word in vocabulary))
    return [(n, line) for n, line in enumerate(text.splitlines(), 1) if rx.search(line)]


def in_tree_flavor_packages() -> list[str]:
    names: list[str] = []
    for manifest in sorted((ROOT / "flavors").glob("*/Cargo.toml")):
        match = re.search(r'(?m)^\[package\][^\[]*?^name\s*=\s*"([^"]+)"', manifest.read_text(encoding="utf-8"))
        if match:
            names.append(match.group(1))
    return names


def check_removed_flavor_vocabulary(findings: list[Finding]) -> None:
    # Rule 1: no tracked file names the removed code flavor.
    for rel in git_lines("grep", "-l", "-i", "-I", "-E", REMOVED_FLAVOR_PATTERN):
        path = ROOT / rel
        for line_no, line in removed_flavor_hits(rel, path.read_text(encoding="utf-8")):
            findings.append(Finding(path, line_no, "removed flavor named", line))
    # Rule 2: crate sources never name an in-tree flavor's schema, flavor id
    # or tool prefix. Crate tests may name the test flavor.
    vocabulary = flavor_vocabulary(in_tree_flavor_packages())
    for rel in git_lines("ls-files", "--", "crates/*/src/**"):
        path = ROOT / rel
        try:
            text = path.read_text(encoding="utf-8")
        except (UnicodeDecodeError, FileNotFoundError):
            continue
        for line_no, line in crate_src_flavor_hits(text, vocabulary):
            findings.append(Finding(path, line_no, "crate source names an in-tree flavor", line))


# Census of reads of `proxima_core.memory` that do not go through the owner
# read policy (#488). Owner-scope SQL is decided by `proxima_owner_read`;
# platform-scope SQL bypasses it, so every such read states whether it wants
# live memories only or forgotten ones too. The counts are pinned so a new
# platform read is classified on purpose.
FORGET_POLICY_MARKER = re.compile(r"FORGET-POLICY: (live-only|includes-forgotten) \(([^)]*\S[^)]*)\)")
MEMORY_READ = re.compile(
    r"\b(?:FROM|JOIN|UPDATE|INTO)\s+(?:[\w.]+(?:\s+\w+)?\s*,\s*)*proxima_core\.memory\b"
)
PLATFORM_SCOPE_BINDING = re.compile(
    r"PgPlatformScope|platform_scope|platform_transaction\(|set_config\('app\.proxima_scope', 'platform'"
)
FUNCTION_START = re.compile(r"CREATE (?:OR REPLACE )?FUNCTION ")
MEMORY_CENSUS_ALWAYS = (
    "crates/storage-pg/definitions/functions.sql",
    "crates/storage-pg/definitions/triggers.sql",
)
# Verb modules whose callers bind platform scope (ports/embeddings.rs) rather
# than the module itself, so the file-level binding test cannot see them.
MEMORY_CENSUS_PLATFORM_CALLEES = (
    "crates/storage-pg/src/verbs/fact_embeddings/",
    # Content GC runs only inside transfer, forget and erase transactions.
    "crates/storage-pg/src/verbs/content.rs",
)
# A live-only read filters forgotten rows itself (#488 S6): the read's line or
# the statement lines right after it say so. Reads inside generated owner-RLS
# policy text are hidden by the policy and say that in their reason instead.
LIVE_ONLY_FILTER = re.compile(r"\bforgotten_at IS NULL\b")
LIVE_ONLY_WINDOW = 8
LIVE_ONLY_RLS_REASON = "under owner RLS"
EXPECTED_MEMORY_READS = {"live-only": 15, "includes-forgotten": 63}


def in_memory_census(text: str, suffix: str, rel: str = "") -> bool:
    if suffix not in {".rs", ".sql"}:
        return False
    return rel.startswith(MEMORY_CENSUS_PLATFORM_CALLEES) or PLATFORM_SCOPE_BINDING.search(text) is not None


def live_only_statement(lines: list[str], index: int, suffix: str) -> str:
    """The rest of the statement a read starts: up to the literal's closing
    quote (Rust) or the semicolon (SQL), at most LIVE_ONLY_WINDOW lines."""
    end = '"' if suffix == ".rs" else ";"
    match = MEMORY_READ.search(lines[index])
    parts: list[str] = []
    rest = lines[index][match.end() if match else 0 :]
    for offset in range(LIVE_ONLY_WINDOW):
        if offset:
            if index + offset >= len(lines):
                break
            rest = lines[index + offset]
        stop = rest.find(end)
        if stop >= 0:
            parts.append(rest[:stop])
            break
        parts.append(rest)
    return "\n".join(parts)


def unfiltered_live_only(text: str, suffix: str) -> list[int]:
    """Live-only reads whose statement does not filter forgotten rows."""
    lines = text.splitlines()
    unfiltered: list[int] = []
    for line_no, kind in memory_read_hits(text, suffix):
        if kind != "live-only":
            continue
        if LIVE_ONLY_FILTER.search(live_only_statement(lines, line_no - 1, suffix)):
            continue
        if suffix == ".sql" and LIVE_ONLY_RLS_REASON in text:
            reasons = [m.group(2) for m in FORGET_POLICY_MARKER.finditer(text) if m.group(1) == "live-only"]
            if reasons and all(LIVE_ONLY_RLS_REASON in reason for reason in reasons):
                continue
        unfiltered.append(line_no)
    return unfiltered


def memory_read_hits(text: str, suffix: str) -> list[tuple[int, str | None]]:
    """Each read of the memory spine, with its marker class (None: unmarked)."""
    lines = text.splitlines()
    comment = "//" if suffix == ".rs" else "--"
    hits: list[tuple[int, str | None]] = []
    for index, line in enumerate(lines):
        # An inline test module ends the census: tests read the spine freely.
        following = lines[index + 1] if index + 1 < len(lines) else ""
        if suffix == ".rs" and line == "#[cfg(test)]" and re.match(r"mod \w+ \{", following):
            break
        if line.strip().startswith(comment) or not MEMORY_READ.search(line):
            continue
        kind = None
        candidates = [line, lines[index - 1] if index else ""]
        if suffix == ".sql":
            # A function body is pinned verbatim elsewhere (migrations,
            # generated triggers), so a definitions function carries one
            # marker, on the line above its CREATE, for every read inside it.
            for back in range(index, -1, -1):
                if FUNCTION_START.match(lines[back]):
                    candidates.append(lines[back - 1] if back else "")
                    break
        for candidate in candidates:
            marker = FORGET_POLICY_MARKER.search(candidate)
            if marker:
                kind = marker.group(1)
                break
        hits.append((index + 1, kind))
    return hits


def memory_census_mismatch(expected: dict[str, int], actual: dict[str, int]) -> str | None:
    if expected == actual:
        return None
    return f"EXPECTED_MEMORY_READS = {expected}, found {actual}"


def memory_census_files() -> list[Path]:
    files = [ROOT / rel for rel in MEMORY_CENSUS_ALWAYS]
    for path in iter_files("crates/storage-pg/src"):
        if path.suffix not in {".rs", ".sql"} or path.stem == "tests" or path.stem.endswith("_tests"):
            continue
        if in_memory_census(path.read_text(encoding="utf-8"), path.suffix, path.relative_to(ROOT).as_posix()):
            files.append(path)
    return files


def check_memory_reads(findings: list[Finding]) -> None:
    counts = {"live-only": 0, "includes-forgotten": 0}
    for path in memory_census_files():
        text = path.read_text(encoding="utf-8")
        lines = text.splitlines()
        for line_no, kind in memory_read_hits(text, path.suffix):
            if kind is None:
                findings.append(
                    Finding(path, line_no, "platform-scope memory read without FORGET-POLICY marker", lines[line_no - 1])
                )
            else:
                counts[kind] += 1
        for line_no in unfiltered_live_only(text, path.suffix):
            findings.append(
                Finding(path, line_no, "live-only memory read without forgotten_at IS NULL", lines[line_no - 1])
            )
    mismatch = memory_census_mismatch(EXPECTED_MEMORY_READS, counts)
    if mismatch:
        findings.append(Finding(ROOT / "scripts/check-architecture-guardrails.py", 1, "memory read census", mismatch))


# #488: the owner read policy shows forgotten memories only when the
# transaction-local GUC below is on. One module sets it (PgLifecycleScope);
# a second writer would be a way around the policy nobody reviewed.
LIFECYCLE_GUC_WRITE = re.compile(
    r"set_config\(\s*'app\.proxima_lifecycle'|\bSET\s+(?:LOCAL\s+)?(?:SESSION\s+)?app\.proxima_lifecycle\b",
    re.IGNORECASE,
)
LIFECYCLE_GUC_MODULE = "crates/storage-pg/src/lifecycle_scope.rs"
LIFECYCLE_GUC_ROOTS = ("crates", "apps", "tools", "flavors")


def lifecycle_guc_writers(files: dict[str, str]) -> list[tuple[str, int]]:
    """Every place outside the lifecycle module that sets the lifecycle GUC."""
    writers: list[tuple[str, int]] = []
    for rel, text in sorted(files.items()):
        if rel == LIFECYCLE_GUC_MODULE:
            continue
        for index, line in enumerate(text.splitlines(), start=1):
            if LIFECYCLE_GUC_WRITE.search(line):
                writers.append((rel, index))
    return writers


def check_lifecycle_guc(findings: list[Finding]) -> None:
    files: dict[str, str] = {}
    for root in LIFECYCLE_GUC_ROOTS:
        base = ROOT / root
        if not base.is_dir():
            continue
        for path in base.rglob("*"):
            if path.suffix not in {".rs", ".sql"} or "target" in path.parts or not path.is_file():
                continue
            files[path.relative_to(ROOT).as_posix()] = path.read_text(encoding="utf-8")
    module = files.get(LIFECYCLE_GUC_MODULE, "")
    if not LIFECYCLE_GUC_WRITE.search(module):
        findings.append(Finding(ROOT / LIFECYCLE_GUC_MODULE, 1, "lifecycle scope", "the lifecycle module no longer sets app.proxima_lifecycle"))
    for rel, line_no in lifecycle_guc_writers(files):
        line = files[rel].splitlines()[line_no - 1]
        findings.append(Finding(ROOT / rel, line_no, "app.proxima_lifecycle set outside PgLifecycleScope", line))


COLD_VOCABULARY = re.compile(
    r"cooled|hydrate_memor|COLD_FORMAT|cold_object_key|\"cold/",
    re.IGNORECASE,
)


def cold_vocabulary_hits(text: str) -> list[tuple[int, str]]:
    """Lines naming the removed cold path (#488): the cooled table, cold
    hydration, the cold object format, its key function and `cold/` keys.
    The purge port keeps its names (`ColdObjectStore`, `cold_store`,
    `cold_purge_pending`, `retry_cold_object_purges`), which this does not
    match."""
    return [
        (index, line)
        for index, line in enumerate(text.splitlines(), start=1)
        if ALLOW not in line and COLD_VOCABULARY.search(line)
    ]


def check_cold_vocabulary(findings: list[Finding]) -> None:
    for path in production_src_files():
        for line_no, line in cold_vocabulary_hits(path.read_text(encoding="utf-8")):
            findings.append(Finding(path, line_no, "removed cold-path vocabulary (#488)", line))


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
    gated_raw = '''#[cfg(any(test, feature = "test-fixtures"))]
async fn ingest_fact_atomic(
    permit: &OwnerWritePermit,
    draft: &FactWriteCommand,
) -> Result<(), StorageError> { Ok(()) }
'''
    ungated_raw = '''async fn ingest_fact_atomic(
    permit: &OwnerWritePermit,
    draft: &FactWriteCommand,
) -> Result<(), StorageError> { Ok(()) }
'''
    gated = list(rust_function_signatures(gated_raw, {"ingest_fact_atomic"}))
    ungated = list(rust_function_signatures(ungated_raw, {"ingest_fact_atomic"}))
    if not gated or not directly_fixture_gated(gated_raw, gated[0][2]):
        failures.append("fixture-gated raw Fact adapter was not recognized")
    if not ungated or directly_fixture_gated(ungated_raw, ungated[0][2]):
        failures.append("ungated raw Fact adapter was not detected")
    if not removed_flavor_hits("docs/x.md", "see flavors/code\nproxima_code.chunk\nPROXIMA-CODE\nproxima_core"):
        failures.append("removed flavor name not detected")
    elif len(removed_flavor_hits("docs/x.md", "flavors/code\nproxima_code\nPROXIMA-CODE\nproxima_core")) != 3:
        failures.append("removed flavor rule must flag exactly the three spellings, not proxima_core")
    if removed_flavor_hits("CHANGELOG.md", "proxima-code"):
        failures.append("CHANGELOG.md must stay exempt from the removed flavor rule")
    vocabulary = flavor_vocabulary(["proxima-example"])
    if vocabulary != ["proxima_example", "proxima-example", "proxima-example_"]:
        failures.append(f"flavor vocabulary derivation changed: {vocabulary}")
    if len(crate_src_flavor_hits("// proxima_example\nproxima-example_note\nproxima_core", vocabulary)) != 2:
        failures.append("crate source flavor vocabulary not detected")
    memory_fixture = ROOT / "scripts/fixtures/architecture-guardrails/memory_reads.rs"
    memory_hits = memory_read_hits(memory_fixture.read_text(encoding="utf-8"), ".rs")
    unmarked = [line for line, kind in memory_hits if kind is None]
    marked = sorted(kind for _line, kind in memory_hits if kind is not None)
    if len(unmarked) != 2:
        failures.append(f"memory census: expected the unmarked and the empty-reason read flagged, got {unmarked}")
    if marked != ["includes-forgotten", "live-only"]:
        failures.append(f"memory census: marked reads not classified (same line, line above): {marked}")
    sql_fixture = (
        "-- FORGET-POLICY: live-only (fixture) applies to every read in this function\n"
        "CREATE OR REPLACE FUNCTION proxima_core.f() RETURNS void AS $$\n"
        "  SELECT 1 FROM proxima_core.memory m;\n$$;\n"
        "CREATE OR REPLACE FUNCTION proxima_core.g() RETURNS void AS $$\n"
        "  SELECT 1 FROM proxima_core.memory m;\n$$;\n"
    )
    if [kind for _line, kind in memory_read_hits(sql_fixture, ".sql")] != ["live-only", None]:
        failures.append("memory census: a function-level marker must cover its own function only")
    if memory_census_mismatch({"live-only": 1, "includes-forgotten": 1}, {"live-only": 1, "includes-forgotten": 2}) is None:
        failures.append("memory census: a count mismatch was not detected")
    if not in_memory_census("let scope = PgPlatformScope::begin(&pool).await?;", ".rs"):
        failures.append("memory census: a platform-scope file was not recognized")
    if in_memory_census("let scope = owner_scope(&pool).await?;", ".rs"):
        failures.append("memory census: an owner-scope file was put in the census")
    guc_files = {
        LIFECYCLE_GUC_MODULE: "sqlx::query(\"SELECT set_config('app.proxima_lifecycle', 'on', true)\")",
        "crates/storage-pg/src/other.rs": "sqlx::query(\"SELECT set_config( 'app.proxima_lifecycle', 'on', true)\")",
        "crates/x/src/y.sql": "SET LOCAL app.proxima_lifecycle = 'on';",
        "crates/storage-pg/definitions/policies.sql": "current_setting('app.proxima_lifecycle'::text, true) = 'on'::text",
    }
    if [rel for rel, _line in lifecycle_guc_writers(guc_files)] != ["crates/storage-pg/src/other.rs", "crates/x/src/y.sql"]:
        failures.append("lifecycle scope: a second writer of app.proxima_lifecycle was not detected (or a reader was)")
    live_fixture = (
        'sqlx::query("SELECT t FROM proxima_core.memory /* FORGET-POLICY: live-only (fixture) */ WHERE t = $1")\n'
        'sqlx::query("SELECT t FROM proxima_core.memory /* FORGET-POLICY: live-only (fixture) */\n'
        '    WHERE t = $1 AND forgotten_at IS NULL")\n'
        'sqlx::query("UPDATE j FROM claimed, proxima_core.memory m /* FORGET-POLICY: includes-forgotten (fixture) */")\n'
    )
    if unfiltered_live_only(live_fixture, ".rs") != [1]:
        failures.append("memory census: a live-only read without the forgotten filter was not flagged (or a filtered one was)")
    if [kind for _line, kind in memory_read_hits(live_fixture, ".rs")] != ["live-only", "live-only", "includes-forgotten"]:
        failures.append("memory census: a comma join to proxima_core.memory was not recognized as a read")
    if not in_memory_census("fn f() {}", ".rs", "crates/storage-pg/src/verbs/fact_embeddings/write.rs"):
        failures.append("memory census: a platform callee module was not put in the census")
    cold_fixture = ROOT / "scripts/fixtures/architecture-guardrails/cold_vocabulary.rs"
    cold_hits = [line for line, _text in cold_vocabulary_hits(cold_fixture.read_text(encoding="utf-8"))]
    if cold_hits != [3, 4, 5, 6, 7]:
        failures.append(f"cold vocabulary: expected lines 3-7 flagged and the purge port names allowed, got {cold_hits}")
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
    check_removed_flavor_vocabulary(findings)
    check_memory_reads(findings)
    check_lifecycle_guc(findings)
    check_cold_vocabulary(findings)

    if findings:
        print("architecture guardrail violations:", file=sys.stderr)
        for finding in findings:
            print(f"  {finding.render()}", file=sys.stderr)
        return 1
    print("architecture guardrails passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
