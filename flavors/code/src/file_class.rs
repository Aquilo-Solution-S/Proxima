//! Which kind of file a chunk was cut from.
//!
//! Every class stays indexed and searchable. The class decides what ingest
//! derives from a file and where search ranks it:
//!
//! | Class | Calls extracted | Vector over | Hybrid rank |
//! |---|---|---|---|
//! | `source` | yes | header + body | first |
//! | `generated` | no | header + body | after every source match |
//! | `vendored` | no | header + body | after every source match |
//! | `lockfile` | no | header only | after every source match |
//!
//! First rule that decides wins:
//!
//! 1. lockfile by file name ([`LOCKFILE_NAMES`], `*.lock`, `*.lockfile`);
//! 2. `.gitattributes` at the ingested revision: `linguist-vendored` set →
//!    vendored; `linguist-generated` set, `-diff` or `binary` → generated;
//! 3. built-in vendored paths ([`VENDORED_GLOBS`]) unless the path's
//!    `linguist-vendored` is explicitly false;
//! 4. built-in generated paths ([`GENERATED_GLOBS`]) or a leading
//!    `// Code generated … DO NOT EDIT.` line, unless the path's
//!    `linguist-generated` is explicitly false;
//! 5. source.
//!
//! `.gitattributes` is read from the tree, not through `git check-attr`:
//! the ingested revision is often not the checkout, and `check-attr
//! --source` needs a newer git than the runtime images carry. Only
//! in-tree files count — `$GIT_DIR/info/attributes` and
//! `core.attributesFile` are host state, and a class must not depend on
//! which host ran the ingest.

use std::sync::LazyLock;

use globset::{Glob, GlobBuilder, GlobSet, GlobSetBuilder};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The class of the file a chunk was cut from. Stored as
/// `proxima_code.file_class`; a chunk written before v0.0.24 stores NULL
/// and reads as [`FileClass::Source`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum FileClass {
    #[default]
    Source,
    Generated,
    Vendored,
    Lockfile,
}

impl FileClass {
    pub const ALL: [Self; 4] = [
        Self::Source,
        Self::Generated,
        Self::Vendored,
        Self::Lockfile,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Generated => "generated",
            Self::Vendored => "vendored",
            Self::Lockfile => "lockfile",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|class| class.as_str() == value)
    }

    #[must_use]
    pub const fn is_source(self) -> bool {
        matches!(self, Self::Source)
    }
}

/// One counter per class, as an ingest report carries them.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
pub struct FileClassCounts {
    pub source: usize,
    pub generated: usize,
    pub vendored: usize,
    pub lockfile: usize,
}

impl FileClassCounts {
    pub fn add(&mut self, class: FileClass, count: usize) {
        let slot = match class {
            FileClass::Source => &mut self.source,
            FileClass::Generated => &mut self.generated,
            FileClass::Vendored => &mut self.vendored,
            FileClass::Lockfile => &mut self.lockfile,
        };
        *slot = slot.saturating_add(count);
    }
}

/// Lockfiles whose names do not end in `.lock` or `.lockfile`.
pub const LOCKFILE_NAMES: &[&str] = &[
    "package-lock.json",
    "npm-shrinkwrap.json",
    "pnpm-lock.yaml",
    "go.sum",
    "Package.resolved",
    "packages.lock.json",
];

/// Paths classed vendored unless `.gitattributes` says otherwise.
pub const VENDORED_GLOBS: &[&str] = &["**/vendor/**", "**/node_modules/**"];

/// Paths classed generated unless `.gitattributes` says otherwise.
pub const GENERATED_GLOBS: &[&str] = &[
    "**/__snapshots__/**",
    "**/*.snap",
    "**/*.min.js",
    "**/*.min.css",
    "**/*.map",
];

/// How far into a file the generated-code header is looked for. The marker
/// sits in the leading comment block; a license block ahead of it is the
/// only thing that pushes it down.
const GENERATED_HEADER_SCAN_BYTES: usize = 64 * 1024;

static BUILTIN_VENDORED: LazyLock<GlobSet> = LazyLock::new(|| builtin_set(VENDORED_GLOBS));
static BUILTIN_GENERATED: LazyLock<GlobSet> = LazyLock::new(|| builtin_set(GENERATED_GLOBS));

fn builtin_set(patterns: &[&str]) -> GlobSet {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(path_glob(pattern).expect("built-in class glob compiles"));
    }
    builder.build().expect("built-in class globs compile")
}

/// `*` stops at `/`, as it does in `.gitignore` and `.gitattributes`.
fn path_glob(pattern: &str) -> Result<Glob, globset::Error> {
    GlobBuilder::new(pattern).literal_separator(true).build()
}

/// Classifies files at one revision: the built-in rules plus that
/// revision's `.gitattributes` files. Build once per revision, not per path.
#[derive(Debug, Clone, Default)]
pub struct FileClassifier {
    /// Root first, then by depth: a deeper file overrides a shallower one,
    /// so applying them in this order leaves the right state standing.
    attributes: Vec<AttributesFile>,
}

impl FileClassifier {
    /// A classifier over `.gitattributes` files, each given as its
    /// repo-relative path and contents. Other paths are ignored.
    #[must_use]
    pub fn from_gitattributes<'a>(files: impl IntoIterator<Item = (&'a str, &'a [u8])>) -> Self {
        let mut attributes: Vec<AttributesFile> = files
            .into_iter()
            .filter_map(|(path, contents)| AttributesFile::parse(path, contents))
            .collect();
        attributes.sort_by(|a, b| {
            a.depth
                .cmp(&b.depth)
                .then_with(|| a.dir.as_str().cmp(b.dir.as_str()))
        });
        Self { attributes }
    }

    /// Whether `path` is a `.gitattributes` file this classifier reads.
    #[must_use]
    pub fn is_attributes_path(path: &str) -> bool {
        base_name(path) == ".gitattributes"
    }

    /// Directory a `.gitattributes` file classes. The root file's directory
    /// is empty, and an empty directory covers every path.
    #[must_use]
    pub fn attributes_directory(path: &str) -> &str {
        path.rsplit_once('/').map_or("", |(dir, _)| dir)
    }

    /// Whether `path` is strictly inside `dir`. An empty `dir` is the root
    /// file, which covers every path.
    #[must_use]
    pub fn path_is_under_attributes_directory(path: &str, dir: &str) -> bool {
        dir.is_empty()
            || path.len() > dir.len()
                && path.as_bytes().get(..dir.len()) == Some(dir.as_bytes())
                && path.as_bytes().get(dir.len()) == Some(&b'/')
    }

    /// Directories whose files a change to these paths can reclass.
    ///
    /// Empty when none of `paths` is a `.gitattributes` file. A root file
    /// yields one empty directory, which covers the tree. A directory that
    /// sits inside another is dropped: the ancestor already covers it.
    #[must_use]
    pub fn affected_attribute_directories<'a>(
        paths: impl IntoIterator<Item = &'a str>,
    ) -> Vec<&'a str> {
        let mut dirs: Vec<&str> = paths
            .into_iter()
            .filter(|path| Self::is_attributes_path(path))
            .map(Self::attributes_directory)
            .collect();
        if dirs.iter().any(|dir| dir.is_empty()) {
            return vec![""];
        }
        dirs.sort_unstable();
        dirs.dedup();
        let mut kept: Vec<&'a str> = Vec::new();
        for dir in dirs {
            if kept
                .iter()
                .any(|parent| Self::path_is_under_attributes_directory(dir, parent))
            {
                continue;
            }
            kept.push(dir);
        }
        kept
    }

    /// The class of `path` (repo-relative, `/`-separated) with contents
    /// `blob`.
    #[must_use]
    pub fn classify(&self, path: &str, blob: &[u8]) -> FileClass {
        if is_lockfile_name(base_name(path)) {
            return FileClass::Lockfile;
        }
        let attrs = self.attributes_of(path);
        if attrs.vendored == Some(true) {
            return FileClass::Vendored;
        }
        if attrs.generated == Some(true) || attrs.diff_unset {
            return FileClass::Generated;
        }
        if attrs.vendored.is_none() && BUILTIN_VENDORED.is_match(path) {
            return FileClass::Vendored;
        }
        if attrs.generated.is_none()
            && (BUILTIN_GENERATED.is_match(path) || has_generated_header(blob))
        {
            return FileClass::Generated;
        }
        FileClass::Source
    }

    fn attributes_of(&self, path: &str) -> PathAttributes {
        let mut state = PathAttributes::default();
        for file in &self.attributes {
            let rel = if file.dir.is_empty() {
                path
            } else {
                match path
                    .strip_prefix(file.dir.as_str())
                    .and_then(|rest| rest.strip_prefix('/'))
                {
                    Some(rel) => rel,
                    None => continue,
                }
            };
            file.apply(rel, &mut state);
        }
        state
    }
}

fn base_name(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

fn is_lockfile_name(name: &str) -> bool {
    LOCKFILE_NAMES.contains(&name)
        || name
            .strip_suffix(".lock")
            .or_else(|| name.strip_suffix(".lockfile"))
            .is_some_and(|stem| !stem.is_empty())
}

/// Go's generated-code marker (`^// Code generated .* DO NOT EDIT\.$`) on a
/// line of the file's leading comment block. Other generators emit the same
/// line for other languages, so the check is not limited to `.go`.
fn has_generated_header(blob: &[u8]) -> bool {
    let head = &blob[..blob.len().min(GENERATED_HEADER_SCAN_BYTES)];
    let text = String::from_utf8_lossy(head);
    let mut in_block = false;
    for line in text.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if in_block {
            in_block = !line.contains("*/");
            continue;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(comment) = trimmed.strip_prefix("/*") {
            in_block = !comment.contains("*/");
            continue;
        }
        if !trimmed.starts_with("//") {
            return false;
        }
        if line
            .strip_prefix("// Code generated ")
            .is_some_and(|rest| rest.ends_with(" DO NOT EDIT."))
        {
            return true;
        }
    }
    false
}

/// The three attributes classification reads, as the rules leave them for
/// one path. `None` is unspecified.
#[derive(Debug, Default, Clone, Copy)]
struct PathAttributes {
    generated: Option<bool>,
    vendored: Option<bool>,
    diff_unset: bool,
}

#[derive(Debug, Clone, Copy)]
enum Attr {
    Generated,
    Vendored,
    Diff,
}

/// `attr` / `-attr` / `attr=value` / `!attr`, reduced to what
/// classification needs.
#[derive(Debug, Clone, Copy)]
enum AttrState {
    True,
    False,
    /// `diff=<driver>` and any other value that is neither true nor false.
    Other,
    Unspecified,
}

#[derive(Debug, Clone)]
struct AttributesRule {
    assigns: Vec<(Attr, AttrState)>,
}

/// One `.gitattributes` file, compiled. Patterns without a `/` match the
/// base name at any depth below `dir`; patterns with one match the path
/// relative to `dir`.
#[derive(Debug, Clone)]
struct AttributesFile {
    dir: String,
    depth: usize,
    rules: Vec<AttributesRule>,
    by_name: GlobSet,
    by_name_rule: Vec<usize>,
    by_path: GlobSet,
    by_path_rule: Vec<usize>,
}

impl AttributesFile {
    /// `None` when `path` is not a `.gitattributes` file or nothing in it
    /// touches classification.
    fn parse(path: &str, contents: &[u8]) -> Option<Self> {
        if !FileClassifier::is_attributes_path(path) {
            return None;
        }
        let dir = path.rsplit_once('/').map_or("", |(dir, _)| dir).to_owned();
        let depth = if dir.is_empty() {
            0
        } else {
            dir.matches('/').count() + 1
        };
        let text = String::from_utf8_lossy(contents);
        let mut rules = Vec::new();
        let (mut by_name, mut by_name_rule) = (GlobSetBuilder::new(), Vec::new());
        let (mut by_path, mut by_path_rule) = (GlobSetBuilder::new(), Vec::new());
        for line in text.lines() {
            let Some((pattern, assigns)) = parse_line(line) else {
                continue;
            };
            // A trailing `/` matches directories only, and attributes on a
            // directory do not reach the files inside it.
            if pattern.ends_with('/') {
                continue;
            }
            let anchored = pattern.contains('/');
            let pattern = pattern.strip_prefix('/').unwrap_or(pattern);
            // A pattern git accepts and globset does not is skipped, not
            // fatal: the file is repository content, not operator input.
            let Ok(glob) = path_glob(pattern) else {
                continue;
            };
            let index = rules.len();
            rules.push(AttributesRule { assigns });
            if anchored {
                by_path.add(glob);
                by_path_rule.push(index);
            } else {
                by_name.add(glob);
                by_name_rule.push(index);
            }
        }
        if rules.is_empty() {
            return None;
        }
        Some(Self {
            dir,
            depth,
            rules,
            by_name: by_name.build().ok()?,
            by_name_rule,
            by_path: by_path.build().ok()?,
            by_path_rule,
        })
    }

    /// Apply every rule matching `rel`, in line order: a later line
    /// overrides an earlier one.
    fn apply(&self, rel: &str, state: &mut PathAttributes) {
        let mut hits: Vec<usize> = self
            .by_name
            .matches(base_name(rel))
            .into_iter()
            .map(|i| self.by_name_rule[i])
            .collect();
        hits.extend(
            self.by_path
                .matches(rel)
                .into_iter()
                .map(|i| self.by_path_rule[i]),
        );
        hits.sort_unstable();
        for index in hits {
            for &(attr, value) in &self.rules[index].assigns {
                match attr {
                    Attr::Generated => state.generated = linguist_flag(value),
                    Attr::Vendored => state.vendored = linguist_flag(value),
                    Attr::Diff => state.diff_unset = matches!(value, AttrState::False),
                }
            }
        }
    }
}

/// Linguist reads `attr` and `attr=true` as true, `-attr` and `attr=false`
/// as false.
fn linguist_flag(value: AttrState) -> Option<bool> {
    match value {
        AttrState::True => Some(true),
        AttrState::False => Some(false),
        AttrState::Other | AttrState::Unspecified => None,
    }
}

/// `pattern attr…` with the assignments classification reads. `None` for
/// blank lines, comments, macro definitions, negated and quoted patterns,
/// and lines assigning nothing relevant.
fn parse_line(line: &str) -> Option<(&str, Vec<(Attr, AttrState)>)> {
    let mut fields = line.split_whitespace();
    let pattern = fields.next()?;
    // `!pattern` is forbidden in attributes files; C-quoted patterns are
    // rare enough not to parse.
    if pattern.starts_with('#')
        || pattern.starts_with("[attr]")
        || pattern.starts_with('!')
        || pattern.starts_with('"')
    {
        return None;
    }
    let assigns: Vec<(Attr, AttrState)> = fields.filter_map(parse_assignment).collect();
    (!assigns.is_empty()).then_some((pattern, assigns))
}

fn parse_assignment(field: &str) -> Option<(Attr, AttrState)> {
    let (name, state) = if let Some(name) = field.strip_prefix('-') {
        (name, AttrState::False)
    } else if let Some(name) = field.strip_prefix('!') {
        (name, AttrState::Unspecified)
    } else if let Some((name, value)) = field.split_once('=') {
        let state = match value {
            "true" => AttrState::True,
            "false" => AttrState::False,
            _ => AttrState::Other,
        };
        (name, state)
    } else {
        (field, AttrState::True)
    };
    let attr = match name {
        "linguist-generated" => Attr::Generated,
        "linguist-vendored" => Attr::Vendored,
        "diff" => Attr::Diff,
        // The built-in macro: `-diff -merge -text`.
        "binary" if matches!(state, AttrState::True) => {
            return Some((Attr::Diff, AttrState::False));
        }
        _ => return None,
    };
    // `diff` set or `diff=<driver>` means "diff it", which is not a class
    // signal; only unsetting it is.
    let state = match (attr, state) {
        (Attr::Diff, AttrState::True | AttrState::Other) => AttrState::Unspecified,
        (_, state) => state,
    };
    Some((attr, state))
}

#[cfg(test)]
mod tests {
    use super::{FileClass, FileClassifier};

    fn builtin(path: &str) -> FileClass {
        FileClassifier::default().classify(path, b"fn main() {}\n")
    }

    fn with(files: &[(&str, &str)], path: &str) -> FileClass {
        FileClassifier::from_gitattributes(files.iter().map(|(p, c)| (*p, c.as_bytes())))
            .classify(path, b"fn main() {}\n")
    }

    #[test]
    fn lockfiles_are_named() {
        for path in [
            "Cargo.lock",
            "web/yarn.lock",
            "poetry.lock",
            "package-lock.json",
            "apps/site/pnpm-lock.yaml",
            "go.sum",
            "gradle.lockfile",
        ] {
            assert_eq!(builtin(path), FileClass::Lockfile, "{path}");
        }
        assert_eq!(builtin(".lock"), FileClass::Source);
        assert_eq!(builtin("src/lock.rs"), FileClass::Source);
        assert_eq!(builtin("go.mod"), FileClass::Source);
    }

    #[test]
    fn built_in_paths_class_vendored_and_generated() {
        assert_eq!(builtin("vendor/github.com/x/y.go"), FileClass::Vendored);
        assert_eq!(builtin("web/node_modules/a/index.js"), FileClass::Vendored);
        assert_eq!(
            builtin("src/__snapshots__/a.test.ts.snap"),
            FileClass::Generated
        );
        assert_eq!(builtin("tests/snapshots/a.snap"), FileClass::Generated);
        assert_eq!(builtin("static/app.min.js"), FileClass::Generated);
        assert_eq!(builtin("static/app.js.map"), FileClass::Generated);
        assert_eq!(builtin("src/vendor.rs"), FileClass::Source);
        assert_eq!(builtin("src/app.js"), FileClass::Source);
    }

    #[test]
    fn the_go_generated_header_marks_generated_code() {
        let classifier = FileClassifier::default();
        let generated =
            b"// Code generated by protoc-gen-go. DO NOT EDIT.\n// source: a.proto\n\npackage a\n";
        assert_eq!(
            classifier.classify("a.pb.go", generated),
            FileClass::Generated
        );

        let after_license =
            b"/*\n Copyright\n*/\n\n// Code generated by mockgen. DO NOT EDIT.\npackage a\n";
        assert_eq!(
            classifier.classify("m.go", after_license),
            FileClass::Generated
        );

        let crlf = b"// Code generated by stringer. DO NOT EDIT.\r\n\r\npackage a\r\n";
        assert_eq!(classifier.classify("s.go", crlf), FileClass::Generated);

        // After the first line of code it is a comment like any other.
        let late = b"package a\n\n// Code generated by x. DO NOT EDIT.\n";
        assert_eq!(classifier.classify("late.go", late), FileClass::Source);

        // Go's rule wants a space on both sides of the generator.
        let bare = b"// Code generated DO NOT EDIT.\npackage a\n";
        assert_eq!(classifier.classify("bare.go", bare), FileClass::Source);
        let indented = b"  // Code generated by x. DO NOT EDIT.\npackage a\n";
        assert_eq!(
            classifier.classify("indented.go", indented),
            FileClass::Source
        );
    }

    #[test]
    fn gitattributes_linguist_flags_set_the_class() {
        let root = ".gitattributes";
        let rules = "# comment\n\
                     *.pb.go linguist-generated\n\
                     third_party/** linguist-vendored=true\n\
                     schema.sql -diff\n\
                     assets/*.bin binary\n";
        assert_eq!(
            with(&[(root, rules)], "api/v1/a.pb.go"),
            FileClass::Generated
        );
        assert_eq!(
            with(&[(root, rules)], "third_party/z/lib.c"),
            FileClass::Vendored
        );
        assert_eq!(
            with(&[(root, rules)], "db/schema.sql"),
            FileClass::Generated
        );
        assert_eq!(with(&[(root, rules)], "assets/a.bin"), FileClass::Generated);
        // `assets/*.bin` is anchored, so it does not reach deeper paths.
        assert_eq!(with(&[(root, rules)], "x/assets/a.bin"), FileClass::Source);
        assert_eq!(with(&[(root, rules)], "api/v1/a.go"), FileClass::Source);
    }

    #[test]
    fn an_explicit_false_turns_the_built_in_off() {
        let rules = "vendor/** -linguist-vendored\n*.snap linguist-generated=false\n";
        assert_eq!(
            with(&[(".gitattributes", rules)], "vendor/ours/lib.go"),
            FileClass::Source
        );
        assert_eq!(
            with(&[(".gitattributes", rules)], "tests/a.snap"),
            FileClass::Source
        );
        // `!attr` returns it to unspecified, so the built-in applies again.
        let reset = "*.snap linguist-generated=false\n*.snap !linguist-generated\n";
        assert_eq!(
            with(&[(".gitattributes", reset)], "tests/a.snap"),
            FileClass::Generated
        );
    }

    #[test]
    fn a_later_line_and_a_deeper_file_win() {
        let root = "*.gen.ts linguist-generated\n*.gen.ts -linguist-generated\n";
        assert_eq!(
            with(&[(".gitattributes", root)], "a.gen.ts"),
            FileClass::Source
        );

        let files = [
            (".gitattributes", "*.ts linguist-generated\n"),
            ("web/.gitattributes", "*.ts -linguist-generated\n"),
        ];
        assert_eq!(with(&files, "web/src/a.ts"), FileClass::Source);
        assert_eq!(with(&files, "cli/a.ts"), FileClass::Generated);
        // A nested file is scoped to its own directory.
        let nested = [("web/.gitattributes", "*.ts linguist-generated\n")];
        assert_eq!(with(&nested, "cli/a.ts"), FileClass::Source);
        assert_eq!(with(&nested, "web/a.ts"), FileClass::Generated);
        // An anchored pattern is relative to the file that holds it.
        let anchored = [("web/.gitattributes", "/gen/** linguist-generated\n")];
        assert_eq!(with(&anchored, "web/gen/a.ts"), FileClass::Generated);
        assert_eq!(with(&anchored, "gen/a.ts"), FileClass::Source);
    }

    #[test]
    fn lockfile_names_beat_attributes() {
        let rules = "Cargo.lock linguist-generated\n";
        assert_eq!(
            with(&[(".gitattributes", rules)], "Cargo.lock"),
            FileClass::Lockfile
        );
    }

    #[test]
    fn lines_git_ignores_or_forbids_change_nothing() {
        let rules = "[attr]gen linguist-generated\n\
                     !*.rs linguist-generated\n\
                     \"quoted name.rs\" linguist-generated\n\
                     src/ linguist-generated\n\
                     *.rs diff=rust\n\
                     *.rs text eol=lf\n";
        assert_eq!(
            with(&[(".gitattributes", rules)], "src/main.rs"),
            FileClass::Source
        );
        // Not an attributes file at all.
        assert_eq!(
            with(
                &[("docs/gitattributes.md", "*.rs linguist-generated\n")],
                "a.rs"
            ),
            FileClass::Source
        );
    }

    #[test]
    fn classes_round_trip_their_names() {
        for class in FileClass::ALL {
            assert_eq!(FileClass::parse(class.as_str()), Some(class));
        }
        assert_eq!(FileClass::parse("Source"), None);
        assert_eq!(FileClass::default(), FileClass::Source);
    }

    #[test]
    fn an_attributes_change_covers_its_directory_and_a_root_file_covers_the_tree() {
        assert_eq!(FileClassifier::attributes_directory(".gitattributes"), "");
        assert_eq!(
            FileClassifier::attributes_directory("web/.gitattributes"),
            "web"
        );
        assert!(FileClassifier::path_is_under_attributes_directory(
            "src/lib.rs",
            ""
        ));
        assert!(FileClassifier::path_is_under_attributes_directory(
            "web/a.rs", "web"
        ));
        assert!(!FileClassifier::path_is_under_attributes_directory(
            "web2/a.rs",
            "web"
        ));
        assert_eq!(
            FileClassifier::affected_attribute_directories(["src/lib.rs", "README.md"]),
            Vec::<&str>::new()
        );
        assert_eq!(
            FileClassifier::affected_attribute_directories([
                "web/.gitattributes",
                "web/src/.gitattributes",
                "docs/.gitattributes",
            ]),
            vec!["docs", "web"]
        );
        assert_eq!(
            FileClassifier::affected_attribute_directories([
                ".gitattributes",
                "web/.gitattributes",
            ]),
            vec![""]
        );
    }
}
