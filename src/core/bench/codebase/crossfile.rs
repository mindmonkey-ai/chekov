//! Cross-file first-use tasks (slice B1 §3).
//!
//! One index of every declaration in the repository, then per file the first
//! call-shaped use of a name declared in exactly one OTHER file. That span is
//! the mask: a model that has not read the other file cannot recover the
//! signature, and one that has can.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

use super::TaskTier;
use super::ladder;
use super::masker::{self, Candidate};

/// Where a name is declared, from the index's point of view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Defined {
    /// Not declared in this repository (or dropped: a keyword or prelude name).
    Nowhere,
    /// Declared in two or more files — the other file is not identifiable, so
    /// the name is never a candidate, and the skip is counted.
    Ambiguous,
    /// Declared in exactly one other file.
    In(String),
}

/// Declaration name -> the files that declare it, plus the subset of those
/// names that a `{` can open a struct literal for.
pub struct Index {
    declared: BTreeMap<String, BTreeSet<String>>,
    types: BTreeSet<String>,
}

impl Index {
    /// Every `fn`/`struct`/`enum`/`trait`/`type`/`const`/`static`/`mod` name
    /// in the elided texts, minus keywords and the prelude — a name every
    /// Rust program may use without reading another file teaches nothing.
    #[must_use]
    pub fn build(files: &[(String, String)]) -> Self {
        let mut declared: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut types = BTreeSet::new();
        for (path, text) in files {
            for name in indexable_names(text) {
                declared.entry(name).or_default().insert(path.clone());
            }
            for line in text.lines() {
                ladder::type_declaration_names(line, &mut types);
            }
        }
        Self { declared, types }
    }

    /// Whether the index knows this name as a `struct` or an `enum`.
    fn declares_type(&self, name: &str) -> bool {
        self.types.contains(name)
    }

    /// Rule 1 and rule 2 of §3.2 in one answer: exactly one declaring file,
    /// and it is not `not_in`.
    #[must_use]
    pub fn defined_in(&self, name: &str, not_in: &str) -> Defined {
        let Some(files) = self.declared.get(name) else {
            return Defined::Nowhere;
        };
        if files.contains(not_in) {
            return Defined::Nowhere;
        }
        let mut it = files.iter();
        match (it.next(), it.next()) {
            (Some(one), None) => Defined::In(one.clone()),
            (Some(_), Some(_)) => Defined::Ambiguous,
            _ => Defined::Nowhere,
        }
    }

    /// Which of these names the index cannot place: declared in two or more
    /// files, so §3.2's rule 1 skips them. The shortfall sentence counts the
    /// distinct names, not the number of times one was passed over.
    #[must_use]
    pub fn ambiguous_among(&self, names: &BTreeSet<String>) -> BTreeSet<String> {
        names
            .iter()
            .filter(|n| {
                self.declared
                    .get(n.as_str())
                    .is_some_and(|files| files.len() > 1)
            })
            .cloned()
            .collect()
    }
}

/// One file's declaration names, minus keywords and the prelude: a name every
/// Rust program may use without reading another file teaches nothing, so it
/// never earns a cross-file task.
fn indexable_names(text: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for line in text.lines() {
        ladder::declaration_names(line, &mut names);
    }
    names.retain(|name| {
        !ladder::KEYWORDS.contains(&name.as_str()) && !ladder::PRELUDE.contains(&name.as_str())
    });
    names
}

/// One file's inputs: its elided text and the `in_file` spans the masker
/// already produced for it.
pub struct FileText<'a> {
    pub path: &'a str,
    pub text: &'a str,
    pub spans: &'a [Candidate],
}

/// What a span first-uses, recorded beside the candidate it produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Meta {
    pub name: String,
    pub defined_in: String,
    pub also_first_uses: Vec<String>,
}

/// Candidates and their metas, keyed by `byte_range.start` — unique in a file.
#[derive(Debug, Default)]
pub struct Found {
    pub candidates: Vec<Candidate>,
    pub meta: Vec<(usize, Meta)>,
}

/// Where each name's first call-shaped use in one file sits, and the file
/// that declares it.
type FirstUses = BTreeMap<String, (usize, String)>;

/// The name being looked for in one file's text.
///
/// `is_type` says whether the index knows the name as a struct or an enum —
/// only those can open a struct literal with `{`. The literal and `use`-
/// statement ranges are the file's, computed once for the whole scan.
struct Needle<'a> {
    text: &'a str,
    name: &'a str,
    is_type: bool,
    skip: &'a Skipped,
}

/// The stretches of a file that are never a call site: string, char and
/// comment literals, and whole `use` statements.
struct Skipped {
    literals: Vec<Range<usize>>,
    uses: Vec<Range<usize>>,
}

impl Skipped {
    fn covers(&self, at: usize) -> bool {
        self.literals
            .iter()
            .chain(&self.uses)
            .any(|r| r.contains(&at))
    }
}

/// Every cross-file first use in one file, in span order.
///
/// A span may first-use several names; it yields ONE task, keyed on the name
/// whose use appears earliest in the span, the others recorded as
/// `also_first_uses`. One candidate per (file, name) follows from the same
/// rule: a name's first use is in exactly one span.
#[must_use]
pub fn first_uses(index: &Index, file: &FileText<'_>) -> Found {
    let firsts = first_use_offsets(index, file);
    let mut found = Found::default();
    let mut claimed: BTreeSet<String> = BTreeSet::new();
    for span in file.spans.iter().filter(|c| c.tier == TaskTier::InFile) {
        let names = names_first_used_in(&firsts, &span.byte_range);
        let Some((first, rest)) = names.split_first() else {
            continue;
        };
        if !claimed.insert(first.0.clone()) {
            continue;
        }
        found.meta.push((
            span.byte_range.start,
            Meta {
                name: first.0.clone(),
                defined_in: first.1.clone(),
                also_first_uses: rest.iter().map(|(n, _)| n.clone()).collect(),
            },
        ));
        found.candidates.push(Candidate {
            tier: TaskTier::CrossFileFirst,
            byte_range: span.byte_range.clone(),
            line: span.line,
            doc_comment: None,
        });
    }
    found
}

/// §3.2 rule 4 scans F's text once *per name*, so the offset is found once
/// here for every name any span mentions, not once per (name, span).
fn first_use_offsets(index: &Index, file: &FileText<'_>) -> FirstUses {
    let literals = masker::literal_ranges(file.text);
    let skip = Skipped {
        uses: use_statements(file.text, &literals),
        literals,
    };
    let mut out = FirstUses::new();
    for name in span_identifiers(file) {
        let Defined::In(other) = index.defined_in(&name, file.path) else {
            continue;
        };
        let needle = Needle {
            text: file.text,
            name: &name,
            is_type: index.declares_type(&name),
            skip: &skip,
        };
        if let Some(at) = first_use_at(&needle) {
            out.insert(name, (at, other));
        }
    }
    out
}

/// The distinct identifiers of this file's `in_file` spans — the only names a
/// cross-file task could ever key on.
fn span_identifiers(file: &FileText<'_>) -> BTreeSet<String> {
    file.spans
        .iter()
        .filter(|c| c.tier == TaskTier::InFile)
        .flat_map(|c| ladder::identifiers(&file.text[c.byte_range.clone()]))
        .collect()
}

/// The names whose first use lands in this span, earliest use first.
fn names_first_used_in(firsts: &FirstUses, span: &Range<usize>) -> Vec<(String, String)> {
    let mut hits: Vec<(usize, String, String)> = firsts
        .iter()
        .filter(|(_, (at, _))| span.contains(at))
        .map(|(name, (at, other))| (*at, name.clone(), other.clone()))
        .collect();
    hits.sort_unstable();
    hits.into_iter().map(|(_, n, o)| (n, o)).collect()
}

/// The byte offset of the FIRST call-shaped use of the name in the whole
/// file. `None` when there is no such use.
fn first_use_at(needle: &Needle<'_>) -> Option<usize> {
    let (text, name) = (needle.text, needle.name);
    let mut from = 0;
    while let Some(offset) = text[from..].find(name) {
        let at = from + offset;
        from = at + name.len();
        if needle.skip.covers(at) || !is_whole_word(text, at, name.len()) {
            continue;
        }
        if call_shaped(needle, at) {
            return Some(at);
        }
    }
    None
}

/// `name(`, `name::`, `.name(` or `name {` — the four shapes §3.2 calls a
/// call site. Whitespace between the name and its bracket counts.
fn call_shaped(needle: &Needle<'_>, at: usize) -> bool {
    let text = needle.text;
    let after = text[at + needle.name.len()..].trim_start();
    match after.chars().next() {
        Some('(') => true,
        Some(':') => after.starts_with("::"),
        Some('{') => needle.is_type && opens_a_literal(&text[..at]),
        _ => false,
    }
}

/// Whether the `{` after the name opens a struct literal rather than a block.
///
/// A type position takes a `{` too — `-> Widget {`, `impl Widget {`,
/// `impl Trait for Widget {`, `x: Widget {` — and none of them is a call
/// site. Counting one would spend the file's single first use on a line no
/// `in_file` span can hold, silently dropping the real `Widget { … }` below.
fn opens_a_literal(before: &str) -> bool {
    let before = before.trim_end();
    if before.ends_with("->") || before.ends_with('.') {
        return false;
    }
    if before.ends_with(':') && !before.ends_with("::") {
        return false;
    }
    let previous_word = before
        .rsplit(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .find(|w| !w.is_empty());
    !matches!(
        previous_word,
        Some("impl" | "for" | "in" | "as" | "dyn" | "where")
    )
}

/// The bytes either side must not be identifier bytes, so `rebuild` is not a
/// use of `build`.
fn is_whole_word(text: &str, at: usize, len: usize) -> bool {
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let before_ok = text[..at].chars().next_back().is_none_or(|c| !ident(c));
    let after_ok = text[at + len..].chars().next().is_none_or(|c| !ident(c));
    before_ok && after_ok
}

/// Byte ranges of the file's `use` statements — an import is not the first
/// CALL site.
///
/// Whole statements, not lines: rustfmt wraps a grouped import across many
/// lines, and a name on a continuation line of `use crate::{ … };` is still
/// inside the import.
fn use_statements(text: &str, literals: &[Range<usize>]) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::new();
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let start = offset;
        offset += line.len();
        let inside_something = out.last().is_some_and(|last| last.end > start)
            || literals.iter().any(|r| r.contains(&start));
        if !inside_something && ladder::strip_visibility(line).starts_with("use ") {
            out.push(start..statement_end(text, start));
        }
    }
    out
}

/// The end of the statement starting at `from`: its terminating `;` outside
/// any brace group, or the end of the file.
fn statement_end(text: &str, from: usize) -> usize {
    let mut depth: usize = 0;
    for (i, c) in text[from..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            ';' if depth == 0 => return from + i + 1,
            _ => {}
        }
    }
    text.len()
}

#[cfg(test)]
mod tests {
    use super::{Defined, FileText, Index, first_uses};
    use crate::core::bench::codebase::TaskTier;
    use crate::core::bench::codebase::masker::{MaskSource, RustBraceMasker};

    const DEFS: &str = "pub struct Widget {\n    pub id: u32,\n}\n\n\
                        pub fn build(n: u32) -> u32 {\n    n + 1\n}\n\n\
                        pub mod paint {\n    pub fn go() {}\n}\n";

    fn index(files: &[(&str, &str)]) -> Index {
        Index::build(
            &files
                .iter()
                .map(|(p, t)| ((*p).to_owned(), (*t).to_owned()))
                .collect::<Vec<_>>(),
        )
    }

    /// One file's cross-file candidates, as (line, name) pairs.
    fn uses(index: &Index, path: &str, text: &str) -> Vec<(usize, String)> {
        let spans = RustBraceMasker.candidates(text);
        let found = first_uses(
            index,
            &FileText {
                path,
                text,
                spans: &spans,
            },
        );
        found
            .candidates
            .iter()
            .zip(found.meta.iter())
            .map(|(c, (_, m))| {
                assert_eq!(c.tier, TaskTier::CrossFileFirst);
                (c.line, m.name.clone())
            })
            .collect()
    }

    #[test]
    fn the_first_use_wins_and_a_later_one_is_not_a_second_task() {
        let idx = index(&[("src/defs.rs", DEFS)]);
        let user = "pub fn run() {\n    let a = build(1);\n    let b = build(2);\n    a + b\n}\n";
        assert_eq!(
            uses(&idx, "src/user.rs", user),
            vec![(2, "build".to_owned())]
        );
    }

    #[test]
    fn every_call_shape_counts_and_a_use_line_never_does() {
        let idx = index(&[("src/defs.rs", DEFS)]);
        for (src, want) in [
            ("pub fn r() {\n    let w = build(1);\n    w\n}\n", "build"),
            (
                "pub fn r() {\n    let w = paint::go();\n    w\n}\n",
                "paint",
            ),
            (
                "pub fn r() {\n    let w = Widget { id: 1 };\n    w\n}\n",
                "Widget",
            ),
        ] {
            let got = uses(&idx, "src/user.rs", src);
            assert_eq!(got.len(), 1, "{src}");
            assert_eq!(got[0].1, want, "{src}");
        }
        let method = "pub fn r(x: Thing) {\n    let w = x.build();\n    let _ = w;\n}\n";
        assert_eq!(uses(&idx, "src/user.rs", method)[0].1, "build");
        let imported = "use crate::defs::build;\npub fn r() {\n    let _ = 1;\n}\n";
        assert!(
            uses(&idx, "src/user.rs", imported).is_empty(),
            "a use line is an import, not a call site"
        );
    }

    #[test]
    fn an_ambiguous_a_local_and_a_prelude_name_are_all_skipped() {
        let two = index(&[
            ("src/a.rs", "pub fn build() {}\n"),
            ("src/b.rs", "pub fn build() {}\n"),
        ]);
        assert_eq!(two.defined_in("build", "src/c.rs"), Defined::Ambiguous);
        let user = "pub fn r() {\n    let w = build(1);\n    w\n}\n";
        assert!(
            uses(&two, "src/c.rs", user).is_empty(),
            "ambiguous is skipped"
        );

        let idx = index(&[("src/defs.rs", DEFS)]);
        let shadow = "fn build(n: u32) -> u32 {\n    n\n}\n\
                      pub fn r() {\n    let w = build(1);\n    w\n}\n";
        assert!(
            uses(&idx, "src/user.rs", shadow).is_empty(),
            "a local declaration makes the other file irrelevant"
        );
        // The index the run builds holds F too, which is what makes rule 2
        // bite: without F in it, `defined_in` never sees the local shadow.
        let with_f = index(&[("src/defs.rs", DEFS), ("src/user.rs", shadow)]);
        assert_eq!(with_f.defined_in("build", "src/user.rs"), Defined::Nowhere);
        assert!(
            uses(&with_f, "src/user.rs", shadow).is_empty(),
            "F declares build itself, so no other file is the one to read"
        );
        assert_eq!(idx.defined_in("Some", "src/user.rs"), Defined::Nowhere);
        assert_eq!(idx.defined_in("match", "src/user.rs"), Defined::Nowhere);
    }

    #[test]
    fn a_type_position_is_not_a_first_use_but_the_literal_below_it_is() {
        let idx = index(&[
            ("src/defs.rs", DEFS),
            ("src/consts.rs", "pub const ROWS: [u8; 1] = [0];\n"),
        ]);
        let typed = "impl Widget {\n}\nfn make() -> Widget {\n    todo!()\n}\n\
                     pub fn r() {\n    let w = Widget { id: 1 };\n    w\n}\n";
        assert_eq!(
            uses(&idx, "src/user.rs", typed),
            vec![(7, "Widget".to_owned())],
            "impl and -> take a block, not a struct literal"
        );

        let looped = "pub fn r() {\n    for x in ROWS {\n        drop(x);\n    }\n}\n";
        assert!(
            uses(&idx, "src/user.rs", looped).is_empty(),
            "a loop head is not a call site"
        );
    }

    #[test]
    fn a_use_block_wrapped_across_lines_is_still_one_import() {
        let idx = index(&[("src/defs.rs", DEFS)]);
        let wrapped = "use crate::{\n    paint::go,\n    defs::build,\n};\n\
                       pub fn r() {\n    let w = paint::go();\n    w\n}\n";
        assert_eq!(
            uses(&idx, "src/user.rs", wrapped),
            vec![(6, "paint".to_owned())],
            "a continuation line of a use block is not the first call site"
        );
    }

    #[test]
    fn a_name_inside_a_string_or_a_comment_is_not_a_use() {
        let idx = index(&[("src/defs.rs", DEFS)]);
        let quoted = "pub fn r() {\n    let s = \"build(1)\";\n    // build(2)\n    s\n}\n";
        assert!(uses(&idx, "src/user.rs", quoted).is_empty(), "{quoted}");
    }

    #[test]
    fn a_span_that_first_uses_two_names_is_one_task_keyed_on_the_earlier() {
        let idx = index(&[("src/defs.rs", DEFS)]);
        let both = "pub fn r() {\n    let w = build(Widget { id: 1 });\n    w\n}\n";
        let spans = RustBraceMasker.candidates(both);
        let found = first_uses(
            &idx,
            &FileText {
                path: "src/user.rs",
                text: both,
                spans: &spans,
            },
        );
        assert_eq!(found.candidates.len(), 1, "one span, one task");
        let meta = &found.meta[0].1;
        assert_eq!(meta.name, "build");
        assert_eq!(meta.defined_in, "src/defs.rs");
        assert_eq!(meta.also_first_uses, vec!["Widget".to_owned()]);
    }
}
