//! Tree-sitter call-graph extraction (definitions + call sites).
//!
//! Field-name captures (`function:`, `name:`, `field:`, `property:`) read
//! directly from the grammar's named fields.
//!
//! Languages and compiled `Query` patterns are cached in `OnceLock`s —
//! query compilation is the expensive part (rule compilation + state
//! machine), so it is paid once per language for the lifetime of the
//! process. A `Parser` is built per extraction.
//!
//! [`extract_blob_callgraph`] parses a blob once and runs both queries
//! against the same `Tree`. The single-fn `extract_definitions` and
//! `extract_calls` wrappers exist for tests and any caller that only
//! needs one side. Ingestion uses `analyze_blob` to share the chunker's
//! tree with both queries as well.

use std::sync::OnceLock;

use tree_sitter::{Language, Parser, Query, QueryCursor, StreamingIterator, Tree};

/// A call site extracted from a code blob.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtractedCall {
    /// Byte range of the entire `call_expression` within the blob.
    pub byte_start: u32,
    pub byte_end: u32,
    /// Identifier of the function being called. For method calls
    /// (`obj.method(...)`) this is the rightmost name; for scoped
    /// paths (`a::b::c(...)`) it's the final segment.
    pub callee_name: String,
    /// True iff the syntactic call form is method-style
    /// (`obj.method(...)`) rather than free or path-style.
    pub is_dynamic: bool,
}

/// A named, callable-by-name definition discovered in a blob.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtractedDefinition {
    pub name: String,
    /// Byte range of the entire definition node (signature + body).
    pub byte_start: u32,
    pub byte_end: u32,
}

/// Single parse, two queries. Returns `(definitions, calls)`.
/// Both vecs are empty for unsupported languages or invalid utf-8.
#[must_use]
pub fn extract_blob_callgraph(
    language: Option<&'static str>,
    blob: &[u8],
) -> (Vec<ExtractedDefinition>, Vec<ExtractedCall>) {
    extract_blob_callgraph_with_tree(language, blob, None)
}

/// Chunk and extract the callgraph with one parse on the supported AST path.
/// If the chunker rejected the blob, retain the standalone callgraph's
/// behavior rather than widening its binary/size rejection policy.
pub(crate) fn analyze_blob(
    file_path: &str,
    blob: &[u8],
) -> (
    Vec<crate::chunker::Chunk>,
    Vec<ExtractedDefinition>,
    Vec<ExtractedCall>,
) {
    let (chunks, tree) = crate::chunker::chunk_blob_with_tree(file_path, blob);
    let (definitions, calls) =
        extract_blob_callgraph_with_tree(crate::chunker::detect_language(file_path), blob, tree);
    (chunks, definitions, calls)
}

fn extract_blob_callgraph_with_tree(
    language: Option<&'static str>,
    blob: &[u8],
    tree: Option<Tree>,
) -> (Vec<ExtractedDefinition>, Vec<ExtractedCall>) {
    let Ok(text) = std::str::from_utf8(blob) else {
        return (Vec::new(), Vec::new());
    };
    let Some(kind) = LangKind::from_tag(language) else {
        return (Vec::new(), Vec::new());
    };

    let tree = if let Some(tree) = tree {
        tree
    } else {
        let mut parser = Parser::new();
        if parser.set_language(kind.language()).is_err() {
            return (Vec::new(), Vec::new());
        }
        let Some(tree) = parser.parse(text, None) else {
            return (Vec::new(), Vec::new());
        };
        tree
    };

    let defs = run_defs(&tree, kind, text);
    let calls = run_calls(&tree, kind, text);
    (defs, calls)
}

/// Convenience wrapper — parses once, discards calls.
#[must_use]
pub fn extract_definitions(
    language: Option<&'static str>,
    blob: &[u8],
) -> Vec<ExtractedDefinition> {
    extract_blob_callgraph(language, blob).0
}

/// Convenience wrapper — parses once, discards definitions.
#[must_use]
pub fn extract_calls(language: Option<&'static str>, blob: &[u8]) -> Vec<ExtractedCall> {
    extract_blob_callgraph(language, blob).1
}

// ---------------------------------------------------------------------
// Language + query cache.
// ---------------------------------------------------------------------

#[derive(Copy, Clone)]
enum LangKind {
    Rust,
    Typescript,
    Tsx,
    Python,
    Go,
}

impl LangKind {
    fn from_tag(tag: Option<&str>) -> Option<Self> {
        match tag {
            Some("rust") => Some(Self::Rust),
            Some("typescript") => Some(Self::Typescript),
            Some("tsx") => Some(Self::Tsx),
            Some("python") => Some(Self::Python),
            Some("go") => Some(Self::Go),
            _ => None,
        }
    }
    fn language(self) -> &'static Language {
        match self {
            Self::Rust => rust_lang(),
            Self::Typescript => ts_lang(),
            Self::Tsx => tsx_lang(),
            Self::Python => python_lang(),
            Self::Go => go_lang(),
        }
    }
    fn defs_query(self) -> &'static Query {
        match self {
            Self::Rust => rust_defs_query(),
            Self::Typescript => ts_defs_query(),
            Self::Tsx => tsx_defs_query(),
            Self::Python => python_defs_query(),
            Self::Go => go_defs_query(),
        }
    }
    fn calls_query(self) -> &'static Query {
        match self {
            Self::Rust => rust_calls_query(),
            Self::Typescript => ts_calls_query(),
            Self::Tsx => tsx_calls_query(),
            Self::Python => python_calls_query(),
            Self::Go => go_calls_query(),
        }
    }
}

fn rust_lang() -> &'static Language {
    static L: OnceLock<Language> = OnceLock::new();
    L.get_or_init(|| tree_sitter_rust::LANGUAGE.into())
}
fn ts_lang() -> &'static Language {
    static L: OnceLock<Language> = OnceLock::new();
    L.get_or_init(|| tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
}
fn tsx_lang() -> &'static Language {
    static L: OnceLock<Language> = OnceLock::new();
    L.get_or_init(|| tree_sitter_typescript::LANGUAGE_TSX.into())
}
fn python_lang() -> &'static Language {
    static L: OnceLock<Language> = OnceLock::new();
    L.get_or_init(|| tree_sitter_python::LANGUAGE.into())
}
fn go_lang() -> &'static Language {
    static L: OnceLock<Language> = OnceLock::new();
    L.get_or_init(|| tree_sitter_go::LANGUAGE.into())
}

// Rust grammar:
//   `function_item` covers free fns and impl methods both — the
//   grammar uses the same node kind for either context.
const RUST_DEFS_SRC: &str = r"
(function_item name: (identifier) @def.name) @def
";

// Rust grammar fields:
//   call_expression.function: <expr>
//     (identifier)             — free-fn / local fn  (e.g. `foo()`)
//     (field_expression .field) — method call         (e.g. `x.foo()`)
//     (scoped_identifier .name) — path call           (e.g. `a::b::foo()`)
// `scoped_identifier.name` is the rightmost segment per the grammar,
// so deeply-nested paths (`a::b::c::foo`) collapse to `foo` cleanly.
const RUST_CALLS_SRC: &str = r"
(call_expression
  function: (identifier) @free.name) @call.free
(call_expression
  function: (field_expression field: (field_identifier) @method.name)) @call.method
(call_expression
  function: (scoped_identifier name: (identifier) @scoped.name)) @call.scoped
";

// TS/TSX grammar:
//   function_declaration.name        — top-level `function foo() {}`
//   method_definition.name           — class methods
//   variable_declarator.name with
//     value being arrow_function /
//     function_expression / function — `const foo = () => {}` etc.
// Interface/ambient signatures (`function_signature`,
// `method_signature`) are intentionally excluded — they declare a
// type but don't carry a body and would shadow real implementations
// at call-resolution time.
const TS_DEFS_SRC: &str = r"
(function_declaration name: (identifier) @def.name) @def
(method_definition name: (property_identifier) @def.name) @def
(variable_declarator
  name: (identifier) @def.name
  value: [(arrow_function) (function_expression)]) @def
";

// TS/TSX grammar:
//   call_expression.function: <expr>
//     (identifier)              — free call            (`foo()`)
//     (member_expression .property) — method/property  (`o.foo()`, `a.b.foo()`)
// `subscript_expression` (`o[k]()`) is intentionally not captured —
// the callee is a runtime value, not a syntactic identifier.
const TS_CALLS_SRC: &str = r"
(call_expression
  function: (identifier) @free.name) @call.free
(call_expression
  function: (member_expression property: (property_identifier) @method.name)) @call.method
";

// Python grammar:
//   `function_definition` covers free functions and methods both; a
//   decorated one sits inside `decorated_definition`, and the definition
//   range here excludes its decorators.
const PYTHON_DEFS_SRC: &str = r"
(function_definition name: (identifier) @def.name) @def
";

// Python grammar fields:
//   call.function: <expr>
//     (identifier)            — free call    (`foo()`)
//     (attribute .attribute)  — method call  (`o.foo()`, `a.b.foo()`)
// A subscripted callee (`handlers[k]()`) is a runtime value and is not
// captured.
const PYTHON_CALLS_SRC: &str = r"
(call
  function: (identifier) @free.name) @call.free
(call
  function: (attribute attribute: (identifier) @method.name)) @call.method
";

// Go grammar:
//   function_declaration.name — `func foo()`
//   method_declaration.name   — `func (r *T) foo()`, a field_identifier
const GO_DEFS_SRC: &str = r"
(function_declaration name: (identifier) @def.name) @def
(method_declaration name: (field_identifier) @def.name) @def
";

// Go grammar fields:
//   call_expression.function: <expr>
//     (identifier)                  — free call               (`foo()`)
//     (selector_expression .field)  — method or package call  (`r.foo()`, `pkg.Foo()`)
// Go spells a method call and a package-qualified call alike, so both are
// method-style (`is_dynamic`).
const GO_CALLS_SRC: &str = r"
(call_expression
  function: (identifier) @free.name) @call.free
(call_expression
  function: (selector_expression field: (field_identifier) @method.name)) @call.method
";

fn rust_defs_query() -> &'static Query {
    static Q: OnceLock<Query> = OnceLock::new();
    Q.get_or_init(|| Query::new(rust_lang(), RUST_DEFS_SRC).expect("rust defs query"))
}
fn rust_calls_query() -> &'static Query {
    static Q: OnceLock<Query> = OnceLock::new();
    Q.get_or_init(|| Query::new(rust_lang(), RUST_CALLS_SRC).expect("rust calls query"))
}
fn ts_defs_query() -> &'static Query {
    static Q: OnceLock<Query> = OnceLock::new();
    Q.get_or_init(|| Query::new(ts_lang(), TS_DEFS_SRC).expect("ts defs query"))
}
fn ts_calls_query() -> &'static Query {
    static Q: OnceLock<Query> = OnceLock::new();
    Q.get_or_init(|| Query::new(ts_lang(), TS_CALLS_SRC).expect("ts calls query"))
}
fn tsx_defs_query() -> &'static Query {
    static Q: OnceLock<Query> = OnceLock::new();
    Q.get_or_init(|| Query::new(tsx_lang(), TS_DEFS_SRC).expect("tsx defs query"))
}
fn tsx_calls_query() -> &'static Query {
    static Q: OnceLock<Query> = OnceLock::new();
    Q.get_or_init(|| Query::new(tsx_lang(), TS_CALLS_SRC).expect("tsx calls query"))
}
fn python_defs_query() -> &'static Query {
    static Q: OnceLock<Query> = OnceLock::new();
    Q.get_or_init(|| Query::new(python_lang(), PYTHON_DEFS_SRC).expect("python defs query"))
}
fn python_calls_query() -> &'static Query {
    static Q: OnceLock<Query> = OnceLock::new();
    Q.get_or_init(|| Query::new(python_lang(), PYTHON_CALLS_SRC).expect("python calls query"))
}
fn go_defs_query() -> &'static Query {
    static Q: OnceLock<Query> = OnceLock::new();
    Q.get_or_init(|| Query::new(go_lang(), GO_DEFS_SRC).expect("go defs query"))
}
fn go_calls_query() -> &'static Query {
    static Q: OnceLock<Query> = OnceLock::new();
    Q.get_or_init(|| Query::new(go_lang(), GO_CALLS_SRC).expect("go calls query"))
}

// ---------------------------------------------------------------------
// Query execution.
// ---------------------------------------------------------------------

fn run_defs(tree: &Tree, kind: LangKind, src: &str) -> Vec<ExtractedDefinition> {
    let q = kind.defs_query();
    let bytes = src.as_bytes();
    let cap_def = q.capture_index_for_name("def");
    let cap_name = q.capture_index_for_name("def.name");

    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(q, tree.root_node(), bytes);
    let mut out = Vec::new();
    while let Some(m) = matches.next() {
        let mut def_node = None;
        let mut name_text: Option<&str> = None;
        for cap in m.captures() {
            let idx = Some(cap.index);
            if idx == cap_def {
                def_node = Some(cap.node);
            } else if idx == cap_name
                && let Ok(t) = cap.node.utf8_text(bytes)
            {
                name_text = Some(t);
            }
        }
        if let (Some(node), Some(name)) = (def_node, name_text)
            && !name.is_empty()
        {
            out.push(ExtractedDefinition {
                name: name.to_string(),
                byte_start: u32::try_from(node.start_byte()).unwrap_or(0),
                byte_end: u32::try_from(node.end_byte()).unwrap_or(0),
            });
        }
    }
    out.sort_by_key(|d| d.byte_start);
    out
}

fn run_calls(tree: &Tree, kind: LangKind, src: &str) -> Vec<ExtractedCall> {
    let q = kind.calls_query();
    let bytes = src.as_bytes();
    let cap_call_free = q.capture_index_for_name("call.free");
    let cap_call_method = q.capture_index_for_name("call.method");
    let cap_call_scoped = q.capture_index_for_name("call.scoped");
    let cap_free_name = q.capture_index_for_name("free.name");
    let cap_method_name = q.capture_index_for_name("method.name");
    let cap_scoped_name = q.capture_index_for_name("scoped.name");

    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(q, tree.root_node(), bytes);
    let mut out = Vec::new();
    while let Some(m) = matches.next() {
        let mut call_node = None;
        let mut name_text: Option<&str> = None;
        let mut is_dynamic = false;
        for cap in m.captures() {
            let idx = Some(cap.index);
            if idx == cap_call_free || idx == cap_call_method || idx == cap_call_scoped {
                call_node = Some(cap.node);
                if idx == cap_call_method {
                    is_dynamic = true;
                }
            } else if (idx == cap_free_name || idx == cap_method_name || idx == cap_scoped_name)
                && let Ok(t) = cap.node.utf8_text(bytes)
            {
                name_text = Some(t);
            }
        }
        if let (Some(node), Some(name)) = (call_node, name_text)
            && !name.is_empty()
        {
            out.push(ExtractedCall {
                byte_start: u32::try_from(node.start_byte()).unwrap_or(0),
                byte_end: u32::try_from(node.end_byte()).unwrap_or(0),
                callee_name: name.to_string(),
                is_dynamic,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_same_analysis(path: &str, blob: &[u8]) {
        let expected_chunks = crate::chunker::chunk_blob(path, blob);
        let (expected_definitions, expected_calls) =
            extract_blob_callgraph(crate::chunker::detect_language(path), blob);
        assert_eq!(
            analyze_blob(path, blob),
            (expected_chunks, expected_definitions, expected_calls),
            "shared parse changed analysis for {path}",
        );
    }

    #[test]
    fn shared_tree_preserves_the_callgraph_for_chunker_rejected_blobs() {
        assert_same_analysis("invalid.rs", b"fn valid() {}\xff");
        let oversized = format!(
            "fn caller() {{ callee(); }}\n//{}",
            "x".repeat(crate::chunker::MAX_BLOB_BYTES)
        );
        for blob in [
            b"fn caller() { callee(); }\n\0".as_slice(),
            oversized.as_bytes(),
        ] {
            let (chunks, definitions, calls) = analyze_blob("rejected.rs", blob);
            assert!(chunks.is_empty());
            assert!(!definitions.is_empty(), "standalone extractor admits UTF-8");
            assert!(!calls.is_empty());
            assert_same_analysis("rejected.rs", blob);
        }
    }

    #[test]
    fn extract_rust_scoped_call_takes_rightmost() {
        let code = b"fn main() { a::b::c::baz(); }";
        let calls = extract_calls(Some("rust"), code);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].callee_name, "baz");
        assert!(!calls[0].is_dynamic);
    }

    #[test]
    fn python_definitions_and_calls_are_extracted() {
        let code = b"@cache\ndef load(path):\n    return parse(read(path))\n\nclass Client:\n    def send(self, request):\n        self.transport.handle(request)\n        handlers[request.kind]()\n";
        let (defs, calls) = extract_blob_callgraph(Some("python"), code);
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["load", "send"]);
        let text = std::str::from_utf8(code).expect("utf8");
        assert!(
            text[defs[0].byte_start as usize..].starts_with("def load"),
            "a definition's range starts at `def`, after its decorators"
        );
        let calls: Vec<(&str, bool)> = calls
            .iter()
            .map(|c| (c.callee_name.as_str(), c.is_dynamic))
            .collect();
        for expected in [("parse", false), ("read", false), ("handle", true)] {
            assert!(calls.contains(&expected), "{expected:?} missing: {calls:?}");
        }
        assert_eq!(
            calls.len(),
            3,
            "a subscripted callee is not captured: {calls:?}"
        );
    }

    #[test]
    fn go_definitions_and_calls_are_extracted() {
        let code = b"package main\n\ntype Server struct{}\n\nfunc (s *Server) Serve() error {\n\treturn listen(s.addr())\n}\n\nfunc main() {\n\tfmt.Println(run())\n}\n";
        let (defs, calls) = extract_blob_callgraph(Some("go"), code);
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["Serve", "main"]);
        let mut calls: Vec<(&str, bool)> = calls
            .iter()
            .map(|c| (c.callee_name.as_str(), c.is_dynamic))
            .collect();
        calls.sort_unstable();
        assert_eq!(
            calls,
            [
                ("Println", true),
                ("addr", true),
                ("listen", false),
                ("run", false)
            ]
        );
    }

    #[test]
    fn python_and_go_share_the_chunkers_parse() {
        assert_same_analysis(
            "pkg/client.py",
            b"def a():\n    b()\n\ndef b():\n    pass\n",
        );
        assert_same_analysis(
            "cmd/main.go",
            b"package main\n\nfunc a() { b() }\n\nfunc b() {}\n",
        );
    }

    #[test]
    fn ts_interface_signature_is_not_a_definition() {
        // An ambient interface signature shouldn't be treated as a
        // definition — there's no body to point an edge at, and it
        // would shadow the real implementing class's method.
        let code = b"interface I { run(): void; }\nclass C implements I { run() {} }\n";
        let defs = extract_definitions(Some("typescript"), code);
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names.iter().filter(|n| **n == "run").count(), 1);
    }
}
