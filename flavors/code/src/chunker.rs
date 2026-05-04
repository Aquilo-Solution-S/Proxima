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
/// - blob isn't valid UTF-8 (heuristic for "binary"),
/// - blob is empty.
#[must_use]
pub fn chunk_blob(file_path: &str, content: &[u8]) -> Vec<Chunk> {
    if content.len() > MAX_BLOB_BYTES {
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
        "source_file"
            | "translation_unit"
            | "module"
            | "program"
            | "compilation_unit"
            | "ERROR"
    )
}

/// Skip insubstantial leaves at merge time: top-level comments, raw
/// whitespace tokens, semicolons.
fn is_substantive(node: &Node) -> bool {
    let kind = node.kind();
    if kind == "comment"
        || kind == "line_comment"
        || kind == "block_comment"
        || kind == "//"
        || kind == "/*"
        || kind == "*/"
    {
        return false;
    }
    if kind.is_empty() {
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
    let ext = file_path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
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
    let ext = file_path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
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
