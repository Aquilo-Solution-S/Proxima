//! Code-aware chunker — cAST split-merge.
//!
//! Tree-sitter parses each blob; we walk the AST top-down. A node that fits
//! inside `MAX_CHUNK_CHARS` is emitted as one chunk; a node larger than that
//! has its children processed instead, with consecutive small children
//! greedy-merged up to `TARGET_CHUNK_CHARS`.
//!
//! Sizes are measured in **non-whitespace characters** rather than raw
//! bytes (cAST: Zhang et al., arXiv:2506.15655). NWS keeps the budget
//! tied to actual content density across languages and indent styles.
//! O(1) range lookups via a precomputed prefix sum.
//!
//! Grammars: Rust, TypeScript, TSX, Python and Go. Languages without a
//! vendored tree-sitter grammar fall back to whole-file (when small) or
//! non-overlapping line windows and emit `chunk_type="file"`.
//!
//! Pure module: parses bytes, returns chunks. No I/O, no async.

use tree_sitter::{Language, Node, Parser, Tree};

/// Greedy-merge target, in non-whitespace characters. ~500 tokens of
/// typical code under cl100k/o200k tokenizers — comfortably within
/// the context window of common code embedders.
pub const TARGET_CHUNK_CHARS: usize = 1500;

/// Hard upper bound on the Unicode character count of every emitted chunk.
/// AST node selection still uses the non-whitespace cAST budget above this
/// layer (paper Table 4: Pass@1 peaks at 2000 NWS chars, 2000–2500 is the
/// sweet spot). An oversized node or fallback window is split at UTF-8
/// character boundaries before emission.
pub const MAX_CHUNK_CHARS: usize = 2500;

/// Line-window stride for the no-AST fallback.
pub const FALLBACK_LINE_WINDOW: usize = 80;

/// Hard cap on blob size we'll chunk at all. 1 MiB.
pub const MAX_BLOB_BYTES: usize = 1024 * 1024;

/// A code chunk produced by the cAST chunker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chunk {
    pub file_path: String,
    pub text: String,
    pub language: Option<&'static str>,
    pub chunk_type: &'static str,
    pub byte_range_start: u32,
    pub byte_range_end: u32,
    pub line_range_start: u32,
    pub line_range_end: u32,
}

/// Slice a single blob into chunks. Returns empty when:
/// - blob exceeds `MAX_BLOB_BYTES`,
/// - blob contains a `NUL` byte (heuristic for "binary"),
/// - blob isn't valid UTF-8 (also "binary"),
/// - blob is empty.
///
/// `NUL` is checked separately from the UTF-8 test because `U+0000` *is*
/// valid UTF-8. Postgres cannot store `NUL` in a `text` column and fails the
/// statement:
///
/// ```text
/// invalid byte sequence for encoding "UTF8": 0x00
/// ```
///
/// That failure takes the **whole HEAD snapshot**, not just the offending
/// file. Treating a `NUL`-bearing blob as binary is the rule `git` itself
/// uses, and it keeps a value Postgres cannot store from being constructed
/// at all.
#[must_use]
pub fn chunk_blob(file_path: &str, content: &[u8]) -> Vec<Chunk> {
    chunk_blob_with_tree(file_path, content).0
}

/// Retain the successful parse for ingestion's callgraph queries. Rejected
/// blobs and parser failures return no tree; chunk/fallback policy is shared
/// with the public chunk-only entrypoint.
pub(crate) fn chunk_blob_with_tree(file_path: &str, content: &[u8]) -> (Vec<Chunk>, Option<Tree>) {
    if content.len() > MAX_BLOB_BYTES {
        return (Vec::new(), None);
    }
    if content.contains(&0) {
        return (Vec::new(), None);
    }
    let Ok(text) = std::str::from_utf8(content) else {
        return (Vec::new(), None);
    };
    if text.is_empty() {
        return (Vec::new(), None);
    }

    let language = detect_language(file_path);
    let mut parsed_tree = None;

    if let Some(ts_lang) = ts_language_for(language)
        && let Some((chunks, tree)) = ast_chunks(file_path, text, language, &ts_lang)
    {
        if !chunks.is_empty() {
            return (chunks, Some(tree));
        }
        parsed_tree = Some(tree);
    }

    let fallback_lang = fallback_language(file_path);
    (fallback_chunks(file_path, text, fallback_lang), parsed_tree)
}

/// AST path. Returns `None` only when tree-sitter fails entirely; an empty
/// `Vec` means the parser produced no spans and the caller should fall back.
fn ast_chunks(
    file_path: &str,
    source: &str,
    language: Option<&'static str>,
    ts_lang: &Language,
) -> Option<(Vec<Chunk>, Tree)> {
    let mut parser = Parser::new();
    parser.set_language(ts_lang).ok()?;
    let tree = parser.parse(source, None)?;
    let root = tree.root_node();

    let bytes = source.as_bytes();
    let nws_cumsum = build_nws_cumsum(bytes);
    let mut spans: Vec<Span> = Vec::new();
    cast_split_merge(root, &nws_cumsum, &mut spans);
    if spans.is_empty() {
        return Some((Vec::new(), tree));
    }

    let mut out = Vec::with_capacity(spans.len());
    for s in spans {
        if s.start_byte > s.end_byte || s.end_byte > bytes.len() {
            continue;
        }
        let Ok(snippet) = std::str::from_utf8(&bytes[s.start_byte..s.end_byte]) else {
            continue;
        };
        let trimmed = snippet.trim_end_matches(['\n', '\r']);
        if trimmed.trim().is_empty() {
            continue;
        }
        // The cAST walk uses non-whitespace characters to choose nodes, but
        // the emitted contract has a hard Unicode-character bound. Most
        // spans fit and retain the historical trailing-newline range. A
        // whitespace-heavy span or an oversized leaf is split at UTF-8
        // character boundaries with exact metadata for each piece.
        if trimmed.chars().count() <= MAX_CHUNK_CHARS {
            // Bounded by MAX_BLOB_BYTES (1 MiB) at the entry of chunk_blob;
            // u32 fits all byte and row offsets by construction.
            out.push(Chunk {
                file_path: file_path.to_string(),
                text: trimmed.to_string(),
                language,
                chunk_type: s.chunk_type,
                byte_range_start: u32::try_from(s.start_byte).unwrap_or(u32::MAX),
                byte_range_end: u32::try_from(s.end_byte).unwrap_or(u32::MAX),
                line_range_start: u32::try_from(s.start_row + 1).unwrap_or(u32::MAX),
                line_range_end: u32::try_from(s.end_row + 1).unwrap_or(u32::MAX),
            });
        } else {
            push_bounded_chunks(
                &mut out,
                file_path,
                trimmed,
                s.start_byte,
                s.start_row + 1,
                language,
                s.chunk_type,
            );
        }
    }
    Some((out, tree))
}

#[derive(Debug, Clone)]
struct Span {
    start_byte: usize,
    end_byte: usize,
    start_row: usize,
    end_row: usize,
    chunk_type: &'static str,
}

/// cAST split-merge: walk the AST top-down. A node that already fits within
/// `MAX_CHUNK_CHARS` and is a chunk-candidate gets emitted whole. A node
/// larger than budget has its children processed: each child's text is
/// greedy-merged into a running buffer up to `TARGET_CHUNK_CHARS`;
/// oversized children recurse. All sizes are non-whitespace-character
/// counts looked up against the precomputed prefix sum.
fn cast_split_merge(node: Node, nws: &[u32], out: &mut Vec<Span>) {
    let node_size = nws_count(nws, node.start_byte(), node.end_byte());

    if node_size <= MAX_CHUNK_CHARS && is_chunk_candidate(&node) {
        out.push(span_of(node, node_kind_label(&node)));
        return;
    }

    let mut cursor = node.walk();
    let children: Vec<Node> = node.children(&mut cursor).collect();

    if children.is_empty() {
        if node_size >= MIN_FRAGMENT_CHARS {
            out.push(span_of(node, "fragment"));
        }
        return;
    }

    let mut buffer: Option<Span> = None;

    for child in children {
        let child_size = nws_count(nws, child.start_byte(), child.end_byte());

        if child_size > MAX_CHUNK_CHARS {
            if let Some(b) = buffer.take() {
                out.push(b);
            }
            cast_split_merge(child, nws, out);
            continue;
        }

        if !is_substantive(&child) {
            continue;
        }

        match buffer.as_mut() {
            Some(b) => {
                let merged_size = nws_count(nws, b.start_byte, child.end_byte());
                if merged_size <= TARGET_CHUNK_CHARS {
                    b.end_byte = child.end_byte();
                    b.end_row = child.end_position().row;
                    b.chunk_type = "block";
                } else {
                    out.push(buffer.take().unwrap());
                    buffer = Some(span_of(child, node_kind_label(&child)));
                }
            }
            None => buffer = Some(span_of(child, node_kind_label(&child))),
        }
    }

    if let Some(b) = buffer {
        out.push(b);
    }
}

/// Prefix sum of non-whitespace bytes. `cumsum[i]` = NWS bytes in `bytes[..i]`;
/// length is `bytes.len() + 1`. Whitespace is detected at the byte level —
/// safe for UTF-8 because every continuation byte has the high bit set
/// and is therefore non-whitespace.
fn build_nws_cumsum(bytes: &[u8]) -> Vec<u32> {
    let mut cumsum = Vec::with_capacity(bytes.len() + 1);
    cumsum.push(0);
    let mut count: u32 = 0;
    for &b in bytes {
        if !b.is_ascii_whitespace() {
            count += 1;
        }
        cumsum.push(count);
    }
    cumsum
}

/// Non-whitespace count over `[start, end)`. Caller guarantees both
/// indices are within the prefix sum's range (tree-sitter byte offsets
/// are bounded by the source length).
fn nws_count(cumsum: &[u32], start: usize, end: usize) -> usize {
    (cumsum[end] - cumsum[start]) as usize
}

/// Minimum fragment size to emit as a standalone chunk (leaf path),
/// in non-whitespace characters.
const MIN_FRAGMENT_CHARS: usize = 50;

fn span_of(node: Node, label: &'static str) -> Span {
    Span {
        start_byte: node.start_byte(),
        end_byte: node.end_byte(),
        start_row: node.start_position().row,
        end_row: node.end_position().row,
        chunk_type: label,
    }
}

/// Whether the node should be considered as a possible whole-chunk emit.
/// We exclude the outermost container nodes — `source_file`, `module`,
/// `program`, etc. — because emitting them whole when the whole file fits
/// would lose all per-function granularity.
fn is_chunk_candidate(node: &Node) -> bool {
    !matches!(
        node.kind(),
        "source_file" | "translation_unit" | "module" | "program" | "compilation_unit" | "ERROR"
    )
}

/// Skip insubstantial leaves at merge time: anonymous punctuation tokens
/// (`//`, `/*`, `{`, `;`) that carry no retrievable content.
///
/// Comments are *not* skipped. In tree-sitter-rust a doc comment is a
/// sibling of the item it documents rather than part of it, so excluding
/// comment nodes here drops every `///` and `//!` block. Letting comments
/// merge normally puts a doc comment in the same greedy buffer as the
/// item that follows it.
///
/// `node.is_named()` already excludes the anonymous `//` / `/*` / `*/`
/// tokens; the named `comment`, `line_comment` and `block_comment` kinds
/// are content and are kept.
fn is_substantive(node: &Node) -> bool {
    if node.kind().is_empty() {
        return false;
    }
    node.is_named()
}

fn node_kind_label(node: &Node) -> &'static str {
    match node.kind() {
        // Python keeps a decorator outside the definition it decorates, in a
        // wrapper whose `definition` field is the function or class.
        "decorated_definition" => node
            .child_by_field_name("definition")
            .map_or("block", |definition| node_kind_label(&definition)),
        "function_declaration"
        | "function_definition"
        | "function_item"
        | "function_signature_item"
        | "method_declaration"
        | "method_definition"
        | "constructor_declaration"
        | "destructor_declaration"
        | "arrow_function"
        | "local_function_statement" => "function",
        "class_declaration"
        | "class_definition"
        | "interface_declaration"
        | "struct_item"
        | "struct_specifier"
        | "enum_item"
        | "enum_declaration"
        | "trait_item"
        | "impl_item"
        | "type_declaration"
        | "namespace_declaration" => "class",
        _ => "block",
    }
}

fn ts_language_for(language: Option<&'static str>) -> Option<Language> {
    match language {
        Some("rust") => Some(tree_sitter_rust::LANGUAGE.into()),
        Some("typescript") => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
        Some("tsx") => Some(tree_sitter_typescript::LANGUAGE_TSX.into()),
        Some("python") => Some(tree_sitter_python::LANGUAGE.into()),
        Some("go") => Some(tree_sitter_go::LANGUAGE.into()),
        _ => None,
    }
}

/// File-window fallback for grammar-less languages and parser misses.
/// Non-overlapping line windows; `chunk_type="file"`. Text is an exact
/// source slice with the window's final line terminator excluded; interior
/// line endings remain unchanged and byte ranges are file-relative.
fn fallback_chunks(file_path: &str, text: &str, language: Option<&'static str>) -> Vec<Chunk> {
    let mut out = Vec::new();
    let mut start_byte = 0;
    let mut end_byte = 0;
    let mut start_line = 1;
    for (index, line) in text.split_inclusive('\n').enumerate() {
        end_byte += line.len();
        let end_line = index + 1;
        if end_line % FALLBACK_LINE_WINDOW != 0 && end_byte < text.len() {
            continue;
        }

        let window = &text[start_byte..end_byte];
        let chunk_text = window
            .strip_suffix('\n')
            .map_or(window, |line| line.strip_suffix('\r').unwrap_or(line));
        if !chunk_text.trim().is_empty() {
            if chunk_text.chars().count() <= MAX_CHUNK_CHARS {
                out.push(Chunk {
                    file_path: file_path.to_string(),
                    text: chunk_text.to_string(),
                    language,
                    chunk_type: "file",
                    byte_range_start: u32::try_from(start_byte).unwrap_or(u32::MAX),
                    byte_range_end: u32::try_from(start_byte + chunk_text.len())
                        .unwrap_or(u32::MAX),
                    line_range_start: u32::try_from(start_line).unwrap_or(u32::MAX),
                    line_range_end: u32::try_from(end_line).unwrap_or(u32::MAX),
                });
            } else {
                push_bounded_chunks(
                    &mut out, file_path, chunk_text, start_byte, start_line, language, "file",
                );
            }
        }
        start_byte = end_byte;
        start_line = end_line + 1;
    }
    out
}

/// Emit contiguous UTF-8-safe pieces whose text is at most the hard Unicode
/// character bound. Ranges describe the actual piece, and line numbers are
/// derived from its embedded newlines rather than from byte offsets.
fn push_bounded_chunks(
    out: &mut Vec<Chunk>,
    file_path: &str,
    text: &str,
    source_start_byte: usize,
    source_start_line: usize,
    language: Option<&'static str>,
    chunk_type: &'static str,
) {
    let mut context = BoundedChunkContext {
        out,
        file_path,
        text,
        source_start_byte,
        language,
        chunk_type,
    };
    let mut piece_start = 0;
    let mut piece_chars = 0;
    let mut piece_line_start = source_start_line;
    let mut piece_newlines = 0;

    for (byte, character) in context.text.char_indices() {
        if piece_chars == MAX_CHUNK_CHARS {
            push_bounded_piece(
                &mut context,
                piece_start,
                byte,
                piece_line_start,
                piece_newlines,
            );
            piece_line_start += piece_newlines;
            piece_start = byte;
            piece_chars = 0;
            piece_newlines = 0;
        }
        piece_chars += 1;
        piece_newlines += usize::from(character == '\n');
    }

    if piece_start < context.text.len() {
        let text_len = context.text.len();
        push_bounded_piece(
            &mut context,
            piece_start,
            text_len,
            piece_line_start,
            piece_newlines,
        );
    }
}

struct BoundedChunkContext<'a> {
    out: &'a mut Vec<Chunk>,
    file_path: &'a str,
    text: &'a str,
    source_start_byte: usize,
    language: Option<&'static str>,
    chunk_type: &'static str,
}

fn push_bounded_piece(
    context: &mut BoundedChunkContext<'_>,
    start: usize,
    end: usize,
    source_start_line: usize,
    newline_count: usize,
) {
    let piece = &context.text[start..end];
    if !piece.trim().is_empty() {
        let line_end = source_start_line + newline_count - usize::from(piece.ends_with('\n'));
        context.out.push(Chunk {
            file_path: context.file_path.to_string(),
            text: piece.to_string(),
            language: context.language,
            chunk_type: context.chunk_type,
            byte_range_start: u32::try_from(context.source_start_byte + start).unwrap_or(u32::MAX),
            byte_range_end: u32::try_from(context.source_start_byte + end).unwrap_or(u32::MAX),
            line_range_start: u32::try_from(source_start_line).unwrap_or(u32::MAX),
            line_range_end: u32::try_from(line_end).unwrap_or(u32::MAX),
        });
    }
}

/// Every language label [`detect_language`] or [`fallback_language`] can
/// emit, which is the closed set a chunk's `language` takes. The
/// `search_chunks` language filter accepts exactly these.
pub const LANGUAGE_LABELS: &[&str] = &[
    "rust",
    "typescript",
    "tsx",
    "javascript",
    "python",
    "go",
    "markdown",
    "toml",
    "json",
    "yaml",
    "sql",
    "text",
];

/// File-extension -> language string for AST path.
/// Returns None for unknown extensions (fallback path uses extension
/// for language label separately).
#[must_use]
pub fn detect_language(file_path: &str) -> Option<&'static str> {
    let ext = file_path
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "rs" => Some("rust"),
        "ts" | "mts" | "cts" => Some("typescript"),
        "tsx" => Some("tsx"),
        "py" | "pyi" => Some("python"),
        "go" => Some("go"),
        _ => None,
    }
}

/// Fallback language detection for non-AST path.
/// Used for `chunk_type = "file"` chunks to provide a language label.
#[must_use]
pub fn fallback_language(file_path: &str) -> Option<&'static str> {
    let ext = file_path
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "rs" => Some("rust"),
        "ts" | "mts" | "cts" => Some("typescript"),
        "tsx" => Some("tsx"),
        "js" | "jsx" | "mjs" | "cjs" => Some("javascript"),
        "py" | "pyi" => Some("python"),
        "go" => Some("go"),
        "md" | "markdown" => Some("markdown"),
        "toml" => Some("toml"),
        "json" => Some("json"),
        "yaml" | "yml" => Some("yaml"),
        "sql" => Some("sql"),
        "txt" => Some("text"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write;

    /// A label outside [`LANGUAGE_LABELS`] would be stored on a chunk that
    /// the `search_chunks` language filter then refuses to name.
    #[test]
    fn every_emitted_language_label_is_listed() {
        let paths = [
            "a.rs",
            "a.ts",
            "a.mts",
            "a.cts",
            "a.tsx",
            "a.js",
            "a.jsx",
            "a.mjs",
            "a.cjs",
            "a.py",
            "a.pyi",
            "a.go",
            "a.md",
            "a.markdown",
            "a.toml",
            "a.json",
            "a.yaml",
            "a.yml",
            "a.sql",
            "a.txt",
            "A.PY",
            "Makefile",
            "a.unknown",
        ];
        for path in paths {
            for label in [detect_language(path), fallback_language(path)]
                .into_iter()
                .flatten()
            {
                assert!(
                    LANGUAGE_LABELS.contains(&label),
                    "{path}: label {label} missing from LANGUAGE_LABELS"
                );
            }
        }
    }

    #[test]
    fn python_go_and_javascript_get_a_language_label() {
        for (path, label, grammar) in [
            ("httpx/_client.py", "python", true),
            ("stubs/api.pyi", "python", true),
            ("cmd/server/main.go", "go", true),
            ("src/index.js", "javascript", false),
            ("src/App.jsx", "javascript", false),
            ("scripts/build.mjs", "javascript", false),
            ("config/jest.cjs", "javascript", false),
        ] {
            assert_eq!(fallback_language(path), Some(label), "{path}");
            assert_eq!(
                detect_language(path),
                grammar.then_some(label),
                "{path}: grammar {grammar}"
            );
        }
    }

    /// A Python function of about `lines` padded statements, prefixed by
    /// `decorator` when given.
    fn python_function(name: &str, lines: usize, decorator: Option<&str>) -> String {
        let mut out = decorator.map_or_else(String::new, |d| format!("@{d}\n"));
        let _ = writeln!(out, "def {name}(value):");
        for line in 0..lines {
            let _ = writeln!(
                out,
                "    total_{line} = value + {line}  # padding statement"
            );
        }
        out.push_str("    return value\n\n");
        out
    }

    /// Every definition sits whole inside one chunk: a syntax-aware cut never
    /// falls inside a function that fits the budget.
    fn assert_definitions_whole(path: &str, source: &str, chunks: &[Chunk]) {
        let language = detect_language(path);
        let definitions = crate::calls::extract_definitions(language, source.as_bytes());
        assert!(!definitions.is_empty(), "{path}: no definitions found");
        for definition in definitions {
            assert!(
                chunks
                    .iter()
                    .any(|chunk| chunk.byte_range_start <= definition.byte_start
                        && definition.byte_end <= chunk.byte_range_end),
                "{path}: {} is split across chunks",
                definition.name
            );
        }
    }

    #[test]
    fn python_chunks_on_definitions_with_their_decorators() {
        let source = [
            python_function("first", 40, None),
            python_function("second", 40, Some("retry(times=3)")),
            python_function("third", 40, None),
        ]
        .concat();
        let chunks = chunk_blob("pkg/jobs.py", source.as_bytes());
        assert!(chunks.len() >= 3, "{chunks:#?}");
        for chunk in &chunks {
            assert_eq!(chunk.language, Some("python"));
            assert_eq!(chunk.chunk_type, "function", "{}", chunk.text);
            assert!(chunk.text.chars().count() <= MAX_CHUNK_CHARS);
        }
        assert!(
            chunks
                .iter()
                .any(|chunk| chunk.text.starts_with("@retry(times=3)\ndef second")),
            "the decorator stays with its function"
        );
        assert_definitions_whole("pkg/jobs.py", &source, &chunks);
    }

    #[test]
    fn a_python_class_over_budget_splits_between_its_methods() {
        let mut source = String::from("class Client:\n    \"\"\"Sends requests.\"\"\"\n\n");
        for method in 0..6 {
            for line in python_function(&format!("method_{method}"), 20, None).lines() {
                if line.is_empty() {
                    source.push('\n');
                } else {
                    let _ = writeln!(source, "    {line}");
                }
            }
        }
        let chunks = chunk_blob("pkg/client.py", source.as_bytes());
        assert!(chunks.len() > 1, "a class over budget is not one chunk");
        assert!(chunks.iter().all(|chunk| chunk.chunk_type != "file"));
        assert_definitions_whole("pkg/client.py", &source, &chunks);
    }

    #[test]
    fn go_chunks_label_types_and_functions() {
        let mut source = String::from("package server\n\n");
        // Over `TARGET_CHUNK_CHARS`, so the struct does not merge with the
        // package clause into a `block`.
        source.push_str("type Server struct {\n");
        for field in 0..60 {
            let _ = writeln!(source, "\tField{field} string // padding field comment");
        }
        source.push_str("}\n\nfunc (s *Server) Serve() error {\n");
        for line in 0..40 {
            let _ = writeln!(source, "\ts.Field{line} = listen(\"padding statement\")");
        }
        source.push_str("\treturn nil\n}\n");
        let chunks = chunk_blob("server/server.go", source.as_bytes());
        let kinds: Vec<(&str, bool)> = chunks
            .iter()
            .map(|chunk| (chunk.chunk_type, chunk.language == Some("go")))
            .collect();
        assert!(kinds.contains(&("class", true)), "{kinds:?}");
        assert!(kinds.contains(&("function", true)), "{kinds:?}");
        assert_definitions_whole("server/server.go", &source, &chunks);
    }

    /// `U+0000` is valid UTF-8, so "is it UTF-8" does not classify these as
    /// binary — and the chunk text would reach a Postgres `text` column,
    /// which cannot hold a NUL and fails the whole snapshot with
    /// `invalid byte sequence for encoding "UTF8": 0x00`.
    ///
    /// Each case is valid UTF-8 and named after how it arises in a real
    /// tree: a source file with a stray NUL, a UTF-16 file `git` did not
    /// mark binary, and a fixture whose NUL sits past any fixed-size sniff
    /// window.
    #[test]
    fn a_nul_makes_a_blob_binary_even_though_it_is_valid_utf8() {
        for (label, bytes) in [
            (
                "a source file with a stray NUL",
                b"pub fn main() {\0}\n".to_vec(),
            ),
            (
                "UTF-16LE ASCII, alternating NULs",
                b"p\0u\0b\0 \0f\0n\0".to_vec(),
            ),
            ("a NUL past a fixed sniff window", {
                let mut bytes = vec![b'x'; 9000];
                bytes.push(0);
                bytes
            }),
        ] {
            assert!(
                std::str::from_utf8(&bytes).is_ok(),
                "{label}: the premise is that this is valid UTF-8"
            );
            assert!(
                chunk_blob("a.rs", &bytes).is_empty(),
                "{label}: must be treated as binary"
            );
        }
    }

    #[test]
    fn oversized_typescript_leaf_is_split_at_unicode_boundaries() {
        let source = format!("const encoded = '{}';\n", "ä0123456789".repeat(300));
        let chunks = chunk_blob("payload.ts", source.as_bytes());

        assert!(chunks.len() > 1);
        assert!(chunks.iter().all(|chunk| chunk.chunk_type != "file"));
        for chunk in &chunks {
            assert!(chunk.text.chars().count() <= MAX_CHUNK_CHARS);
            assert!(std::str::from_utf8(chunk.text.as_bytes()).is_ok());
            assert_eq!(
                &source[chunk.byte_range_start as usize..chunk.byte_range_end as usize],
                chunk.text,
            );
        }
        assert!(chunks.iter().any(|chunk| chunk.text.contains('ä')));
    }

    #[test]
    fn fallback_splits_long_line_and_preserves_multiline_metadata() {
        let long_line = "ß".repeat(MAX_CHUNK_CHARS + 17);
        let mut source = long_line.clone();
        for line in 2..=80 {
            source.push('\n');
            let _ = write!(source, "line {line}");
        }
        source.push('\n');
        source.push_str("tail");

        let chunks = chunk_blob("payload.txt", source.as_bytes());
        assert!(chunks.len() > 2);
        for chunk in &chunks {
            assert!(chunk.text.chars().count() <= MAX_CHUNK_CHARS);
            assert_eq!(
                &source[chunk.byte_range_start as usize..chunk.byte_range_end as usize],
                chunk.text,
            );
            assert!(chunk.line_range_start <= chunk.line_range_end);
        }
        assert_eq!(chunks[0].line_range_start, 1);
        assert_eq!(chunks[0].line_range_end, 1);
        assert!(chunks.iter().any(|chunk| chunk.line_range_end >= 80));
        assert_eq!(chunks.last().unwrap().text, "tail");
        assert_eq!(chunks.last().unwrap().line_range_start, 81);
    }

    #[test]
    fn fallback_splits_an_overlong_eighty_line_window() {
        let mut source = String::new();
        for line in 1..=80 {
            if line > 1 {
                source.push('\n');
            }
            let _ = write!(source, "line {line} {}", "x".repeat(36));
        }

        let chunks = chunk_blob("payload.txt", source.as_bytes());
        assert!(chunks.len() >= 2);
        for chunk in &chunks {
            assert!(chunk.text.chars().count() <= MAX_CHUNK_CHARS);
            assert_eq!(
                &source[chunk.byte_range_start as usize..chunk.byte_range_end as usize],
                chunk.text,
            );
        }
        assert_eq!(chunks.first().unwrap().line_range_start, 1);
        assert!(chunks.last().unwrap().line_range_end <= 80);
    }
}
