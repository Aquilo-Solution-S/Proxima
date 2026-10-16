#!/usr/bin/env python3
"""Check that environment settings read by the source have one home.

The scanner deliberately understands the small set of access shapes used by
Proxima.  A new access shape must be taught here (or is rejected as dynamic);
it must not disappear into a broad regex.
"""

from __future__ import annotations

import argparse
import ast
import re
import sys
import tempfile
from pathlib import Path

NAME = r"PROXIMA_[A-Z0-9_]+"
SOURCE_DIRS = ("apps", "crates", "flavors", "tools")
SOURCE_SUFFIXES = {".rs", ".py", ".sh", ".yml", ".yaml"}


def source_files(root: Path) -> list[Path]:
    paths: list[Path] = []
    for directory in SOURCE_DIRS:
        base = root / directory
        if not base.exists():
            continue
        paths.extend(
            path
            for path in base.rglob("*")
            if path.is_file()
            and (path.suffix in SOURCE_SUFFIXES or path.name.startswith("compose"))
        )
    paths.extend(path for path in (root / "scripts").iterdir()
                 if path.is_file() and path.suffix in SOURCE_SUFFIXES)
    paths.extend(
        path for path in root.glob("*compose*.yml") if path.is_file()
    )
    return sorted(paths)


def without_comments(text: str, *, hash_comments: bool = False) -> str:
    """Keep strings and code, removing comments in Rust/shell/YAML/Python."""
    out: list[str] = []
    quote: str | None = None
    escaped = False
    line_comment = False
    block_depth = 0
    i = 0
    while i < len(text):
        char = text[i]
        nxt = text[i + 1] if i + 1 < len(text) else ""
        if line_comment:
            if char == "\n":
                line_comment = False
                out.append(char)
            else:
                out.append(" ")
            i += 1
            continue
        if block_depth:
            if char == "/" and nxt == "*":
                block_depth += 1
                out.extend("  ")
                i += 2
            elif char == "*" and nxt == "/":
                block_depth -= 1
                out.extend("  ")
                i += 2
            else:
                out.append("\n" if char == "\n" else " ")
                i += 1
            continue
        if quote:
            out.append(char)
            if escaped:
                escaped = False
            elif char == "\\":
                escaped = True
            elif char == quote:
                quote = None
            i += 1
            continue
        # Rust lifetimes (`'static`) are identifiers, not string literals.
        # Keep apostrophe quoting for shell/YAML, while avoiding a lifetime
        # swallowing the rest of a Rust line and hiding a following comment.
        if char == '"' or (char == "'" and not (nxt.isalpha() or nxt == "_")):
            quote = char
            out.append(char)
            i += 1
        elif char == "/" and nxt == "/":
            line_comment = True
            out.extend("  ")
            i += 2
        elif char == "#" and (hash_comments or i == 0 or text[i - 1] == "\n"):
            line_comment = True
            out.append(" ")
            i += 1
        elif char == "/" and nxt == "*":
            block_depth = 1
            out.extend("  ")
            i += 2
        else:
            out.append(char)
            i += 1
    return "".join(out)


def string_value(match: re.Match[str]) -> str:
    return match.group(2)


def string_spans(text: str) -> list[tuple[int, int]]:
    """Return ordinary quoted-string spans in comment-free source."""
    spans: list[tuple[int, int]] = []
    start: int | None = None
    quote: str | None = None
    escaped = False
    for index, char in enumerate(text):
        next_char = text[index + 1] if index + 1 < len(text) else ""
        if quote is None:
            if char == '"' or (char == "'" and not (next_char.isalpha() or next_char == "_")):
                quote = char
                start = index
        elif escaped:
            escaped = False
        elif char == "\\":
            escaped = True
        elif char == quote:
            spans.append((start or index, index + 1))
            quote = None
            start = None
    return spans


def outside_strings(position: int, spans: list[tuple[int, int]]) -> bool:
    return all(not start <= position < end for start, end in spans)


def scan_file(path: Path) -> tuple[set[str], list[str]]:
    raw = path.read_text(encoding="utf-8", errors="replace")
    if path.suffix == ".py":
        return scan_python(path, raw)
    code = without_comments(
        raw, hash_comments=path.suffix in {".sh", ".yml", ".yaml"} or path.name.startswith("compose")
    )
    spans = string_spans(code)
    found: set[str] = set()
    unsupported: list[str] = []

    # Shell and Compose interpolation are direct reads. Rust strings can
    # contain `${...}` as examples or payload data and are not process reads.
    if path.suffix in {".sh", ".yml", ".yaml"} or path.name.startswith("compose"):
        found.update(re.findall(r"\$\{(" + NAME + r")(?:[:-][^}]*)?\}", code))

    # Literal calls.  Keep the call names narrow: error/help text is not a read.
    literal_call = re.compile(
        r"(?:std::env::|\benv::)?var(?:_os)?\s*\(\s*([\"'])(" + NAME + r")\1"
        r"|(?:env_value|lookup_non_empty|lookup)\s*\(\s*(?:[^\n;(),]+\s*,\s*)?([\"'])(" + NAME + r")\3"
    )
    for match in literal_call.finditer(code):
        if outside_strings(match.start(), spans):
            found.add(match.group(2) or match.group(4))

    # Named constants are the normal way runtime modules share keys.
    constants = {
        match.group(1): match.group(3)
        for match in re.finditer(
            r"\b(?:pub(?:\([^)]*\))?\s+)?const\s+([A-Z][A-Z0-9_]*)\s*:[^=;]+"
            r"=\s*([\"'])(" + NAME + r")\2",
            code,
        )
    }
    helper_reads_key = bool(
        re.search(
            r"\b(?:lookup|lookup_non_empty|env_value|var|var_os)\s*\([^\n;]*(?:\bkey\b|\bname\b)",
            code,
        )
    )
    for identifier, setting in constants.items():
        direct = re.search(
            r"\b(?:lookup|lookup_non_empty|env_value|var(?:_os)?)\s*\([^\n;]*\b"
            + re.escape(identifier)
            + r"\b",
            code,
        )
        helper_argument = helper_reads_key and re.search(
            r"\b[A-Za-z_][A-Za-z0-9_]*\s*\(\s*(?:&?\s*)?lookup\s*,\s*[^;()]*\b"
            + re.escape(identifier)
            + r"\b",
            code,
        )
        if (direct and outside_strings(direct.start(), spans)) or (
            helper_argument and outside_strings(helper_argument.start(), spans)
        ):
            found.add(setting)

    # Arrays passed through a lookup helper (OIDC and transport settings use
    # this shape).  Tuple fixtures are intentionally excluded.
    arrays = re.finditer(
        r"(?:const|let)\s+([A-Za-z_][A-Za-z0-9_]*)[^=;]*=\s*(?:&\s*)?\[([^\]]*)\]",
        code,
        re.DOTALL,
    )
    for array in arrays:
        body = array.group(2)
        if re.search(r"\b" + re.escape(array.group(1)) + r"\b\s*\.?(?:iter|into_iter)", code) and re.search(r"\b(?:lookup|var|var_os)(?:_non_empty)?\s*\(\s*(?:key|name)\b", code):
            found.update(re.findall(NAME, body))
            found.update(value for identifier, value in constants.items()
                         if re.search(r"\b" + re.escape(identifier) + r"\b", body))

    # Inline arrays are used for groups of keys (`[A, B].iter()`).  Resolve
    # only constants that are actually members of such a read path.
    if helper_reads_key:
        for body in re.findall(r"\[([^\]]*)\]\s*\.?(?:iter|into_iter)", code, re.DOTALL):
            for identifier, value in constants.items():
                if re.search(r"\b" + re.escape(identifier) + r"\b", body):
                    found.add(value)

    # Injected lookup helpers commonly receive the key after the lookup
    # closure: `non_empty(lookup, "NAME")`, `env_int_or(lookup, ...)`.
    found.update(
        match.group(2)
        for match in re.finditer(
            r"\b[A-Za-z_][A-Za-z0-9_]*\s*\(\s*(?:&?\s*)?lookup\s*,\s*([\"'])(" + NAME + r")\1",
            code,
        )
        if outside_strings(match.start(), spans)
    )
    found.update(
        match.group(2)
        for match in re.finditer(
            r"\b(?:require_env_or_skip|require_env_or_skip_with)\s*\(\s*([\"'])(" + NAME + r")\1",
            code,
        )
        if outside_strings(match.start(), spans)
    )

    # Dynamic process reads cannot silently escape the inventory. Framework
    # callbacks forward a key/name; their concrete keys are resolved above.
    for match in re.finditer(r"\b(?:std::env::|env::)var(?:_os)?\s*\(\s*([^\n,;)]+)", code):
        if not outside_strings(match.start(), spans):
            continue
        argument = match.group(1).strip()
        if argument.startswith(('"', "'")) or argument in constants or argument in {"key", "name"}:
            continue
        # Non-Proxima constants (HOME, CI, test modes) are outside this table.
        if re.fullmatch(r"[A-Z][A-Z0-9_]*", argument):
            continue
        line = code.count("\n", 0, match.start()) + 1
        unsupported.append(f"{path}:{line}: dynamic process environment key")
    return found, unsupported


def scan_python(path: Path, source: str) -> tuple[set[str], list[str]]:
    """Read Python syntax, so code inside strings and environment writes vanish."""
    tree = ast.parse(source, filename=str(path))
    constants = {node.targets[0].id: node.value.value for node in ast.walk(tree)
                 if isinstance(node, ast.Assign) and len(node.targets) == 1
                 and isinstance(node.targets[0], ast.Name)
                 and isinstance(node.value, ast.Constant) and isinstance(node.value.value, str)}
    aliases = {"os": "os"}
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            for item in node.names:
                if item.name == "os":
                    aliases[item.asname or "os"] = "os"
        if isinstance(node, ast.ImportFrom) and node.module == "os":
            for item in node.names:
                aliases[item.asname or item.name] = "os." + item.name

    def name(node: ast.AST) -> str:
        if isinstance(node, ast.Name):
            return aliases.get(node.id, node.id)
        if isinstance(node, ast.Attribute):
            return name(node.value) + "." + node.attr
        return ""

    found: set[str] = set()
    unsupported: list[str] = []
    for node in ast.walk(tree):
        argument = None
        if isinstance(node, ast.Call) and name(node.func) in {"os.getenv", "os.environ.get"}:
            argument = node.args[0] if node.args else next(
                (item.value for item in node.keywords if item.arg == "key"), None)
        elif isinstance(node, ast.Subscript) and isinstance(node.ctx, ast.Load) and name(node.value) == "os.environ":
            argument = node.slice
        if argument is None:
            continue
        value = argument.value if isinstance(argument, ast.Constant) else constants.get(argument.id) if isinstance(argument, ast.Name) else None
        if not isinstance(value, str):
            unsupported.append(f"{path}:{node.lineno}: dynamic Python environment key")
        elif re.fullmatch(NAME, value):
            found.add(value)
    return found, unsupported


def documented(root: Path) -> set[str]:
    page = root / "docs" / "configuration.md"
    if not page.exists():
        raise ValueError(f"missing canonical page: {page}")
    names: set[str] = set()
    for line in page.read_text(encoding="utf-8").splitlines():
        if not line.startswith("|"):
            continue
        first = line.split("|", 2)[1].strip()
        if re.fullmatch(NAME, first):
            names.add(first)
        elif first == "DATABASE_URL":
            continue
        elif first.startswith("PROXIMA_") or first.startswith("DATABASE_URL"):
            raise ValueError(f"invalid setting name in first table column: {first!r}")
    return names


def check(root: Path) -> int:
    actual: set[str] = set()
    gaps: list[str] = []
    for path in source_files(root):
        names, errors = scan_file(path)
        actual.update(names)
        gaps.extend(errors)
    docs = documented(root)
    for gap in gaps:
        print(f"unsupported read: {gap}", file=sys.stderr)
    for name in sorted(actual - docs):
        print(f"undocumented read: {name}", file=sys.stderr)
    for name in sorted(docs - actual):
        print(f"unread documented setting: {name}", file=sys.stderr)
    if gaps or actual != docs:
        return 1
    print(f"settings: {len(actual)} names checked")
    return 0


def self_test() -> int:
    with tempfile.TemporaryDirectory(prefix="proxima-settings-") as directory:
        root = Path(directory)
        (root / "apps").mkdir()
        (root / "scripts").mkdir()
        (root / "docs").mkdir()
        (root / "docker-compose.dev.yml").write_text(
            'ports: ["${PROXIMA_COMPOSE_PORT:-5432}:5432"]\n', encoding="utf-8"
        )
        fixture = '''// lookup("PROXIMA_COMMENT")
const REAL: &str = "PROXIMA_CONST";
const UNUSED: &str = "PROXIMA_UNUSED";
const KEYS: &[&str] = &["PROXIMA_ARRAY"];
fn read(lookup: impl Fn(&str) -> Option<String>) {
  lookup(REAL);
  KEYS.iter().filter_map(|key| lookup(key));
  non_empty(lookup, "PROXIMA_HELPER");
  std::env::var("PROXIMA_LITERAL");
  println!("std::env::var(\\\"PROXIMA_MEREDATA\\\") ${PROXIMA_MEREDATA}");
  println!("{UNUSED}");
  let _ = [("PROXIMA_FIXTURE", "value")];
}
'''
        (root / "apps" / "fixture.rs").write_text(fixture, encoding="utf-8")
        (root / "scripts" / "compose.sh").write_text(
            '  echo "${PROXIMA_SHELL:-x}" # ${PROXIMA_SHELL_COMMENT}\n', encoding="utf-8"
        )
        names = ["PROXIMA_ARRAY", "PROXIMA_COMPOSE_PORT", "PROXIMA_CONST", "PROXIMA_HELPER", "PROXIMA_LITERAL", "PROXIMA_SHELL"]
        table = "# Configuration\n> Status: current\n\n| Setting | Purpose | When needed | Reader |\n|---|---|---|---|\n"
        doc = table + "".join(f"| {name} | test | test | source |\n" for name in names)
        (root / "docs" / "configuration.md").write_text(
            doc,
            encoding="utf-8",
        )
        if check(root) != 0:
            return 1
        (root / "apps" / "fixture.rs").write_text(
            fixture + 'std::env::var("PROXIMA_UNDOCUMENTED");\n', encoding="utf-8"
        )
        if check(root) == 0:
            return 1
        (root / "apps" / "fixture.rs").write_text(fixture, encoding="utf-8")
        (root / "docs" / "configuration.md").write_text(
            doc + "| PROXIMA_MISSING | test | test | source |\n", encoding="utf-8"
        )
        if check(root) == 0:
            return 1
        (root / "docs" / "configuration.md").write_text(doc, encoding="utf-8")
        dynamic = fixture + "fn dynamic(lookup: impl Fn(&str) -> Option<String>, setting: &str) { std::env::var(setting); }\n"
        (root / "apps" / "fixture.rs").write_text(dynamic, encoding="utf-8")
        if check(root) == 0:
            return 1
        (root / "apps" / "fixture.rs").write_text(fixture, encoding="utf-8")
        (root / "scripts" / "fixture.py").write_text(
            'import os\nKEY = "PROXIMA_PYTHON"\nos.getenv(KEY)\nos.environ["PROXIMA_PYTHON_WRITE"] = "fixture"\n',
            encoding="utf-8",
        )
        (root / "docs" / "configuration.md").write_text(
            doc + "| PROXIMA_PYTHON | test | test | source |\n", encoding="utf-8"
        )
        if check(root) != 0:
            return 1
        (root / "scripts" / "fixture.py").write_text(
            'import os\nos.getenv("PROXIMA_PY_UNDOCUMENTED")\n', encoding="utf-8"
        )
        if check(root) == 0:
            return 1
        (root / "scripts" / "fixture.py").write_text(
            'import os\nkey = input()\nos.getenv(key)\n', encoding="utf-8"
        )
        if check(root) == 0:
            return 1
        (root / "scripts" / "fixture.py").unlink()
        (root / "docs" / "configuration.md").write_text(doc, encoding="utf-8")
        (root / "docs" / "configuration.md").write_text(
            doc + "| PROXIMA_BAD-NAME | test | test | source |\n", encoding="utf-8"
        )
        try:
            documented(root)
        except ValueError:
            pass
        else:
            return 1
    print("self-test: ok")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    return self_test() if args.self_test else check(args.root.resolve())


if __name__ == "__main__":
    raise SystemExit(main())
