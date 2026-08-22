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
//! needs one side; the indexer uses the combined entry point.

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
    let Ok(text) = std::str::from_utf8(blob) else {
        return (Vec::new(), Vec::new());
    };
    let Some(kind) = LangKind::from_tag(language) else {
        return (Vec::new(), Vec::new());
    };

    let mut parser = Parser::new();
    if parser.set_language(kind.language()).is_err() {
        return (Vec::new(), Vec::new());
    }
    let Some(tree) = parser.parse(text, None) else {
        return (Vec::new(), Vec::new());
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
}

impl LangKind {
    fn from_tag(tag: Option<&str>) -> Option<Self> {
        match tag {
            Some("rust") => Some(Self::Rust),
            Some("typescript") => Some(Self::Typescript),
            Some("tsx") => Some(Self::Tsx),
            _ => None,
        }
    }
    fn language(self) -> &'static Language {
        match self {
            Self::Rust => rust_lang(),
            Self::Typescript => ts_lang(),
            Self::Tsx => tsx_lang(),
        }
    }
    fn defs_query(self) -> &'static Query {
        match self {
            Self::Rust => rust_defs_query(),
            Self::Typescript => ts_defs_query(),
            Self::Tsx => tsx_defs_query(),
        }
    }
    fn calls_query(self) -> &'static Query {
        match self {
            Self::Rust => rust_calls_query(),
            Self::Typescript => ts_calls_query(),
            Self::Tsx => tsx_calls_query(),
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
        for cap in m.captures {
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
        for cap in m.captures {
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

    #[test]
    fn extract_rust_free_function() {
        let code = b"fn main() { greet(\"world\"); }\nfn greet(name: &str) {}";
        let calls = extract_calls(Some("rust"), code);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].callee_name, "greet");
        assert!(!calls[0].is_dynamic);
    }

    #[test]
    fn extract_rust_method_call() {
        let code =
            b"struct Foo; impl Foo { fn bar(&self) {} }\nfn main() { let f = Foo; f.bar(); }";
        let calls = extract_calls(Some("rust"), code);
        assert!(calls.iter().any(|c| c.callee_name == "bar" && c.is_dynamic));
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
    fn extract_ts_free_function() {
        let code = b"function greet(name: string) {}\ngreet(\"world\");";
        let calls = extract_calls(Some("typescript"), code);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].callee_name, "greet");
        assert!(!calls[0].is_dynamic);
    }

    #[test]
    fn extract_ts_method_call() {
        let code = b"class Foo { bar() {} }\nconst f = new Foo(); f.bar();";
        let calls = extract_calls(Some("typescript"), code);
        assert!(calls.iter().any(|c| c.callee_name == "bar" && c.is_dynamic));
    }

    #[test]
    fn extract_ts_chained_member_call() {
        let code = b"obj.a.b.greet();";
        let calls = extract_calls(Some("typescript"), code);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].callee_name, "greet");
        assert!(calls[0].is_dynamic);
    }

    #[test]
    fn unknown_language_returns_empty() {
        let code = b"greet(\"world\");";
        let calls = extract_calls(Some("python"), code);
        assert!(calls.is_empty());
    }

    #[test]
    fn binary_input_returns_empty() {
        let code = b"\x80\x81\x82";
        let calls = extract_calls(Some("rust"), code);
        assert!(calls.is_empty());
    }

    #[test]
    fn extract_definitions_rust_basic() {
        let code = b"pub fn alpha() {}\nfn beta(x: i32) -> i32 { x + 1 }\nasync fn gamma() {}\n// fn ignored_in_comment\n";
        let defs = extract_definitions(Some("rust"), code);
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["alpha", "beta", "gamma"]);
    }

    #[test]
    fn extract_definitions_rust_methods() {
        let code = b"struct S; impl S { pub fn foo(&self) {} fn bar() {} }\n";
        let defs = extract_definitions(Some("rust"), code);
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"foo"));
        assert!(names.contains(&"bar"));
    }

    #[test]
    fn extract_definitions_typescript() {
        let code =
            b"function alpha() {}\nconst beta = (x: number) => x + 1;\nclass C { gamma() {} }\n";
        let defs = extract_definitions(Some("typescript"), code);
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"alpha"));
        assert!(names.contains(&"beta"));
        assert!(names.contains(&"gamma"));
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

    #[test]
    fn combined_blob_callgraph_single_pass() {
        let code = b"fn caller() { callee(); }\nfn callee() {}\n";
        let (defs, calls) = extract_blob_callgraph(Some("rust"), code);
        let def_names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(def_names, ["caller", "callee"]);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].callee_name, "callee");
    }
}
