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

/// Declaration name -> the files that declare it.
pub struct Index {
    declared: BTreeMap<String, BTreeSet<String>>,
}

impl Index {
    /// Every `fn`/`struct`/`enum`/`trait`/`type`/`const`/`static`/`mod` name
    /// in the elided texts, minus keywords and the prelude — a name every
    /// Rust program may use without reading another file teaches nothing.
    #[must_use]
    pub fn build(files: &[(String, String)]) -> Self {
        let mut declared: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (path, text) in files {
            for name in indexable_names(text) {
                declared.entry(name).or_default().insert(path.clone());
            }
        }
        Self { declared }
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

/// What a name is looked up against: the index, the file, and the file's
/// literal ranges, computed once for the whole scan rather than per span.
struct Lookup<'a> {
    index: &'a Index,
    file: &'a FileText<'a>,
    literals: Vec<Range<usize>>,
}

/// Every cross-file first use in one file, in span order.
///
/// A span may first-use several names; it yields ONE task, keyed on the name
/// whose use appears earliest in the span, the others recorded as
/// `also_first_uses`. One candidate per (file, name) follows from the same
/// rule: a name's first use is in exactly one span.
#[must_use]
pub fn first_uses(index: &Index, file: &FileText<'_>) -> Found {
    let lookup = Lookup {
        index,
        file,
        literals: masker::literal_ranges(file.text),
    };
    let mut found = Found::default();
    let mut claimed: BTreeSet<String> = BTreeSet::new();
    for span in file.spans.iter().filter(|c| c.tier == TaskTier::InFile) {
        let names = span_first_uses(&lookup, span);
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

/// The names this span first-uses, ordered by where the use appears in it.
fn span_first_uses(lookup: &Lookup<'_>, span: &Candidate) -> Vec<(String, String)> {
    let file = lookup.file;
    let mut hits: Vec<(usize, String, String)> = Vec::new();
    for name in ladder::identifiers(&file.text[span.byte_range.clone()]) {
        let Defined::In(other) = lookup.index.defined_in(&name, file.path) else {
            continue;
        };
        let Some(at) = first_use_at(file.text, &name, &lookup.literals) else {
            continue;
        };
        if span.byte_range.contains(&at) {
            hits.push((at, name, other));
        }
    }
    hits.sort_unstable();
    hits.into_iter().map(|(_, n, o)| (n, o)).collect()
}

/// The byte offset of the FIRST call-shaped use of `name` in the whole file,
/// skipping literals and `use` lines. `None` when there is no such use.
fn first_use_at(text: &str, name: &str, literals: &[Range<usize>]) -> Option<usize> {
    let mut from = 0;
    while let Some(offset) = text[from..].find(name) {
        let at = from + offset;
        from = at + name.len();
        if literals.iter().any(|r| r.contains(&at)) || !is_whole_word(text, at, name.len()) {
            continue;
        }
        if is_use_line(text, at) || !call_shaped(text, at, name.len()) {
            continue;
        }
        return Some(at);
    }
    None
}

/// `name(`, `name::`, `.name(` or `name {` — the four shapes §3.2 calls a
/// call site. Whitespace between the name and its bracket counts; a trailing
/// `{` of an `if`/`match` scrutinee does not, because those keywords are not
/// declaration names and never reach here.
fn call_shaped(text: &str, at: usize, len: usize) -> bool {
    let after = text[at + len..].trim_start();
    let method = text[..at].trim_end().ends_with('.');
    match after.chars().next() {
        Some('(') => true,
        Some(':') => after.starts_with("::"),
        Some('{') => !method,
        _ => false,
    }
}

/// The bytes either side must not be identifier bytes, so `rebuild` is not a
/// use of `build`.
fn is_whole_word(text: &str, at: usize, len: usize) -> bool {
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let before_ok = text[..at].chars().next_back().is_none_or(|c| !ident(c));
    let after_ok = text[at + len..].chars().next().is_none_or(|c| !ident(c));
    before_ok && after_ok
}

/// Whether `at` sits on a `use` line: an import is not the first CALL site.
fn is_use_line(text: &str, at: usize) -> bool {
    let start = text[..at].rfind('\n').map_or(0, |i| i + 1);
    let before = text[start..at].trim_start();
    before.starts_with("use ") || before.starts_with("pub use ")
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
        assert_eq!(idx.defined_in("Some", "src/user.rs"), Defined::Nowhere);
        assert_eq!(idx.defined_in("match", "src/user.rs"), Defined::Nowhere);
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
