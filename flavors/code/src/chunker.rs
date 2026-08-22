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
//! Languages without a vendored tree-sitter grammar fall back to
//! whole-file (when small) or non-overlapping line windows and emit
//! `chunk_type="file"`.
//!
//! Pure module: parses bytes, returns chunks. No I/O, no async.

use tree_sitter::{Language, Node, Parser};

/// Greedy-merge target, in non-whitespace characters. ~500 tokens of
/// typical code under cl100k/o200k tokenizers — comfortably within
/// the context window of common code embedders.
pub const TARGET_CHUNK_CHARS: usize = 1500;

/// Hard upper bound on a single emitted chunk, in non-whitespace
/// characters. Upper end of cAST's recommended budget (paper Table 4:
/// Pass@1 peaks at 2000 NWS chars, 2000–2500 is the sweet spot). Set
/// above the peak so one large function passes through whole rather
/// than being over-split. A single AST node larger than this is split
/// into its children, or emitted as an oversize 'fragment' if it has
/// no children.
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
    if content.len() > MAX_BLOB_BYTES {
        return Vec::new();
    }
    if content.contains(&0) {
        return Vec::new();
    }
    let Ok(text) = std::str::from_utf8(content) else {
        return Vec::new();
    };
    if text.is_empty() {
        return Vec::new();
    }

    let language = detect_language(file_path);

    if let Some(ts_lang) = ts_language_for(language)
        && let Some(chunks) = ast_chunks(file_path, text, language, &ts_lang)
        && !chunks.is_empty()
    {
        return chunks;
    }

    let fallback_lang = fallback_language(file_path);
    fallback_chunks(file_path, text, fallback_lang)
}

/// AST path. Returns `None` only when tree-sitter fails entirely; an empty
/// `Vec` means the parser produced no spans and the caller should fall back.
fn ast_chunks(
    file_path: &str,
    source: &str,
    language: Option<&'static str>,
    ts_lang: &Language,
) -> Option<Vec<Chunk>> {
    let mut parser = Parser::new();
    parser.set_language(ts_lang).ok()?;
    let tree = parser.parse(source, None)?;
    let root = tree.root_node();

    let bytes = source.as_bytes();
    let nws_cumsum = build_nws_cumsum(bytes);
    let mut spans: Vec<Span> = Vec::new();
    cast_split_merge(root, &nws_cumsum, &mut spans);
    if spans.is_empty() {
        return Some(Vec::new());
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
    }
    Some(out)
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
        _ => None,
    }
}

/// File-window fallback for grammar-less languages and parser misses.
/// Non-overlapping line windows; `chunk_type="file"`.
fn fallback_chunks(file_path: &str, text: &str, language: Option<&'static str>) -> Vec<Chunk> {
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::new();
    let mut start = 0usize;
    while start < lines.len() {
        let end = (start + FALLBACK_LINE_WINDOW).min(lines.len());
        let chunk_text = lines[start..end].join("\n");
        if !chunk_text.trim().is_empty() {
            let text_len = u32::try_from(chunk_text.len()).unwrap_or(u32::MAX);
            out.push(Chunk {
                file_path: file_path.to_string(),
                text: chunk_text,
                language,
                chunk_type: "file",
                byte_range_start: 0,
                byte_range_end: text_len,
                line_range_start: u32::try_from(start + 1).unwrap_or(u32::MAX),
                line_range_end: u32::try_from(end).unwrap_or(u32::MAX),
            });
        }
        if end == lines.len() {
            break;
        }
        start += FALLBACK_LINE_WINDOW;
    }
    out
}

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

    #[test]
    fn empty_input_produces_no_chunks() {
        assert!(chunk_blob("a.rs", b"").is_empty());
    }

    #[test]
    fn binary_input_skipped() {
        let bytes = vec![0xff, 0xfe, 0x00, 0x01, 0x02];
        assert!(chunk_blob("a.bin", &bytes).is_empty());
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

    /// The check must not reject ordinary control characters. Tabs, CRs and
    /// form feeds are common in real source and Postgres stores them fine.
    #[test]
    fn other_control_characters_are_still_chunked() {
        let text = b"pub fn main() {\r\n\t\x0clet x = 1;\r\n}\n".to_vec();
        assert!(!chunk_blob("a.rs", &text).is_empty());
    }

    #[test]
    fn oversize_input_skipped() {
        let big = vec![b'x'; MAX_BLOB_BYTES + 1];
        assert!(chunk_blob("big.rs", &big).is_empty());
    }

    #[test]
    fn rust_single_function_one_chunk() {
        let src = "fn main() {\n    println!(\"hi\");\n}\n";
        let chunks = chunk_blob("a.rs", src.as_bytes());
        assert_eq!(chunks.len(), 1);
        let c = &chunks[0];
        assert_eq!(c.language, Some("rust"));
        assert_eq!(c.chunk_type, "function");
        assert_eq!(c.line_range_start, 1);
        assert_eq!(c.line_range_end, 3);
        assert!(c.text.contains("fn main"));
    }

    #[test]
    fn rust_two_functions_chunks_or_merged() {
        let src = "fn a() {}\nfn b() {}\n";
        let chunks = chunk_blob("a.rs", src.as_bytes());
        assert!(chunks.len() <= 2);
        let combined: String = chunks.iter().map(|c| c.text.as_str()).collect();
        assert!(combined.contains("fn a") && combined.contains("fn b"));
    }

    #[test]
    fn typescript_class_chunked() {
        let src = "class Foo {\n  bar() {}\n}\n";
        let chunks = chunk_blob("a.ts", src.as_bytes());
        assert!(!chunks.is_empty());
        let c = &chunks[0];
        assert_eq!(c.language, Some("typescript"));
        assert!(matches!(c.chunk_type, "class" | "block"));
    }

    #[test]
    fn tsx_file_recognized() {
        let src = "export default function App() { return null; }\n";
        let chunks = chunk_blob("a.tsx", src.as_bytes());
        assert!(!chunks.is_empty());
        assert_eq!(chunks[0].language, Some("tsx"));
    }

    #[test]
    fn fallback_for_unknown_language() {
        let src = "SELECT 1;\nSELECT 2;\n";
        let chunks = chunk_blob("a.sql", src.as_bytes());
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].chunk_type, "file");
        assert_eq!(chunks[0].language, fallback_language("a.sql"));
    }

    #[test]
    fn fallback_line_windowing_for_large_unknown_blob() {
        use std::fmt::Write as _;
        let mut src = String::new();
        for i in 0..(FALLBACK_LINE_WINDOW * 2 + 5) {
            writeln!(src, "line {i}").unwrap();
        }
        let chunks = chunk_blob("notes.md", src.as_bytes());
        assert_eq!(chunks.len(), 3);
        for c in &chunks {
            assert_eq!(c.chunk_type, "file");
            assert_eq!(c.language, fallback_language("notes.md"));
        }
        assert_eq!(chunks[0].line_range_start, 1);
        assert_eq!(
            chunks[0].line_range_end,
            u32::try_from(FALLBACK_LINE_WINDOW).unwrap()
        );
    }

    #[test]
    fn no_chunk_text_exceeds_max_chars() {
        use std::fmt::Write as _;
        let mut src = String::new();
        for i in 0..30 {
            writeln!(src, "fn f{i}() {{ let x = {i}; }}").unwrap();
        }
        let chunks = chunk_blob("a.rs", src.as_bytes());
        assert!(!chunks.is_empty());
        for c in &chunks {
            let nws = c.text.bytes().filter(|b| !b.is_ascii_whitespace()).count();
            assert!(
                nws <= MAX_CHUNK_CHARS,
                "chunk NWS size {nws} exceeds MAX_CHUNK_CHARS {MAX_CHUNK_CHARS}"
            );
        }
    }

    /// Byte-span coverage of a source, as a fraction of its length.
    /// Overlapping spans are merged so a file cannot score above 1.0.
    fn coverage(src: &str, chunks: &[Chunk]) -> f64 {
        let mut spans: Vec<(usize, usize)> = chunks
            .iter()
            .map(|c| (c.byte_range_start as usize, c.byte_range_end as usize))
            .collect();
        spans.sort_unstable();
        let mut covered = 0usize;
        let mut reach = 0usize;
        for (start, end) in spans {
            let start = start.max(reach);
            if end > start {
                covered += end - start;
                reach = end;
            }
        }
        f64::from(u32::try_from(covered).unwrap_or(u32::MAX))
            / f64::from(u32::try_from(src.len()).unwrap_or(u32::MAX))
    }

    /// A doc comment is a *sibling* of the item it documents in the Rust
    /// grammar, so the merge loop is the only thing that can keep it.
    #[test]
    fn rust_doc_comments_are_indexed() {
        let src = "\
//! Crate-level prose explaining the module's contract.

/// Returns the answer, having consulted the oracle about parity.
pub fn answer() -> u32 {
    42
}
";
        let chunks = chunk_blob("a.rs", src.as_bytes());
        let combined: String = chunks
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            combined.contains("consulted the oracle about parity"),
            "item doc comment missing from every chunk: {combined:?}"
        );
        assert!(
            combined.contains("Crate-level prose"),
            "module doc comment missing from every chunk: {combined:?}"
        );
    }

    /// The greedy merge should put a doc comment and the item it documents
    /// in one chunk, which is the grouping a reader would choose too.
    #[test]
    fn doc_comment_merges_with_the_item_it_documents() {
        let src = "\
/// Parses the manifest and yields one record per stanza.
pub fn parse() {}
";
        let chunks = chunk_blob("a.rs", src.as_bytes());
        assert_eq!(chunks.len(), 1, "expected one merged chunk: {chunks:?}");
        assert!(chunks[0].text.contains("one record per stanza"));
        assert!(chunks[0].text.contains("pub fn parse"));
    }

    /// Comment-dense sources are the worst case for coverage: comments must
    /// count toward the covered span, not be skipped.
    #[test]
    fn comment_dense_source_is_almost_fully_covered() {
        let src = "\
//! Module prose that carries the design rationale for this file.
//!
//! It runs several lines, because that is how the reasoning is recorded.

use std::fmt;

/// Why this constant has the value it has, at length, because the number
/// is not self-explanatory and the next reader will want the argument.
pub const LIMIT: usize = 64;

/// What this function guarantees to its caller, and what it deliberately
/// does not guarantee, spelled out so nobody has to re-derive it.
pub fn run(input: &str) -> Result<(), fmt::Error> {
    let _ = input;
    Ok(())
}
";
        let chunks = chunk_blob("a.rs", src.as_bytes());
        let covered = coverage(src, &chunks);
        assert!(
            covered > 0.95,
            "coverage {covered:.3} — comment-dense source is losing bytes"
        );
    }

    #[test]
    fn language_detection_table() {
        assert_eq!(detect_language("foo.rs"), Some("rust"));
        assert_eq!(detect_language("foo.ts"), Some("typescript"));
        assert_eq!(detect_language("foo.tsx"), Some("tsx"));
        assert_eq!(detect_language("foo.md"), None);
        assert_eq!(detect_language("Cargo.toml"), None);
        assert_eq!(detect_language("foo.bin"), None);

        assert_eq!(fallback_language("README.md"), Some("markdown"));
        assert_eq!(fallback_language("Cargo.toml"), Some("toml"));
        assert_eq!(fallback_language("foo.sql"), Some("sql"));
        assert_eq!(fallback_language("foo.unknown"), None);
    }
}
