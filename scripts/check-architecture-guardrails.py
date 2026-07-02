#!/usr/bin/env python3
"""Architecture guardrails.

These checks intentionally scan source text rather than relying on compile-time
reachability. A stale public vocabulary or bypass surface should be removed, not
left around behind an adapter.
"""
from __future__ import annotations

import re
import sys
from dataclasses import dataclass
from datetime import date
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
ALLOW = "PR9-RATCHET-ALLOW"

# Dated exemptions for flavor code that still reads `proxima_core.*` tables
# directly instead of going through an authorized flavor-read facade. Every
# entry needs an ISO `expires` date and a `reason`; once an entry expires it
# stops suppressing findings and the failure renders the expiration date so
# reviewers see why it started failing.
ALLOWLISTED_FLAVOR_CORE_SQL: dict[str, dict[str, str]] = {
    "flavors/code/src/mcp/search_chunks.rs": {
        "expires": "2026-07-31",
        "reason": "until Task 5 lands the authorized flavor-read facade",
    },
    "flavors/code/src/mcp/open_file_revision.rs": {
        "expires": "2026-07-31",
        "reason": "until Task 5 lands the authorized flavor-read facade",
    },
    "flavors/code/src/local_git_source.rs": {
        "expires": "2026-07-31",
        "reason": "until Task 5 lands the authorized flavor-read facade",
    },
}


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


def iter_rust_string_literals(text: str):
    """Yield ``(start_line, literal_text)`` for each Rust string / raw-string /
    byte-string literal in ``text``, in source order.

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
            yield start_line, literal
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
            yield start_line, literal
            continue
        if ch == "'":
            char_match = re.match(r"'(?:\\.|[^'\\])'", text[i:])
            if char_match:
                i += char_match.end()
                continue
        i += 1


def check_flavor_core_sql(findings: list[Finding]) -> None:
    flavors = ROOT / "flavors"
    if not flavors.exists():
        return
    paths = [p for p in flavors.rglob("src/**/*.rs") if p.is_file()]
    for path in sorted(paths):
        rel = path.relative_to(ROOT).as_posix()
        text = path.read_text(encoding="utf-8")
        lines = text.splitlines()
        allow_lines = {line_no for line_no, line in enumerate(lines, 1) if ALLOW in line}
        allowance = ALLOWLISTED_FLAVOR_CORE_SQL.get(rel)
        for start_line, literal in iter_rust_string_literals(text):
            # A literal dot after `proxima_core` is a schema-qualified SQL
            # identifier (`proxima_core.memories`); a colon there is Rust path
            # syntax (`proxima_core::verbs`) and must stay allowed everywhere.
            if "proxima_core." not in literal:
                continue
            end_line = start_line + literal.count("\n")
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


def main() -> int:
    findings: list[Finding] = []
    check_stale_access(findings)
    check_personality_authz(findings)
    check_storage_resurrection(findings)
    check_runtime_registration(findings)
    check_flavor_core_sql(findings)
    check_event_source_vocabulary(findings)
    check_tombstone_tool(findings)
    check_public_witnesses(findings)
    check_caller_supplied_audit(findings)

    if findings:
        print("architecture guardrail violations:", file=sys.stderr)
        for finding in findings:
            print(f"  {finding.render()}", file=sys.stderr)
        return 1
    print("architecture guardrails passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
