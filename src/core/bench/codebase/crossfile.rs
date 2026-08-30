//! Cross-file first-use tasks (slice B1 §3).
//!
//! One index of every declaration in the repository, then per file the first
//! call-shaped use of a name declared in exactly one OTHER file. That span is
//! the mask: a model that has not read the other file cannot recover the
//! signature, and one that has can.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

use super::ladder;
use super::masker::{self, Candidate};
use super::{ExtraFile, TaskTier};

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

/// §4.1's cap. One file, never more, and never more than this much of it.
pub const EXTRA_CAP: usize = 32 * 1024;

/// Everything a cross-file task carries beyond its span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assembled {
    pub extra: ExtraFile,
    pub extra_text: String,
    pub also_first_uses: Vec<String>,
    pub withheld: u32,
}

/// The elided corpus, and the same texts whitespace-normalised once so rule
/// (b) does not re-normalise every file for every task.
pub struct Corpus<'a> {
    pub files: &'a [(String, String)],
    pub normalised: &'a [(String, String)],
    /// The task's own file. It holds the gold by construction — it IS the
    /// masked file — so it is context, not a leak, and never counts as
    /// withheld.
    pub task_file: &'a str,
}

/// The normalised twin of every file, in the same order.
#[must_use]
pub fn normalised_corpus(files: &[(String, String)]) -> Vec<(String, String)> {
    files
        .iter()
        .map(|(p, t)| (p.clone(), ladder::normalise(t)))
        .collect()
}

/// The extra file for one task: G's elided text (windowed at the cap) plus
/// rule (b)'s count over every OTHER file.
///
/// `None` only when G has left the corpus, which cannot happen for a candidate
/// the index produced — the caller treats it as "no cross-file task here"
/// rather than sending a task with no definition.
#[must_use]
pub fn assemble_extra(meta: &Meta, gold: &str, corpus: &Corpus<'_>) -> Option<Assembled> {
    let (_, text) = corpus.files.iter().find(|(p, _)| *p == meta.defined_in)?;
    let extra_text = window(text, &meta.name);
    Some(Assembled {
        extra: ExtraFile {
            path: meta.defined_in.clone(),
            bytes: extra_text.len() as u64,
            truncated: extra_text.len() < text.len(),
        },
        extra_text,
        also_first_uses: meta.also_first_uses.clone(),
        withheld: withheld_count(gold, &meta.defined_in, corpus),
    })
}

/// The whole file under the cap; otherwise the 32 KiB window centred on the
/// declaration line.
///
/// The start snaps outward to a line start and the end back to a line end, so
/// the window holds whole lines only and still fits the cap — a window that
/// grew past 32 KiB to finish a line would break the promise §4.1 makes to
/// the context budget.
fn window(text: &str, name: &str) -> String {
    if text.len() <= EXTRA_CAP {
        return text.to_owned();
    }
    let at = declaration_offset(text, name).unwrap_or(text.len() / 2);
    let start = line_start_containing(text, at.saturating_sub(EXTRA_CAP / 2));
    text[start..last_line_end_within_cap(text, start)].to_owned()
}

/// The start of the line `at` falls in. A line start is a character boundary
/// too, so a window cut here can never split a UTF-8 character — which the
/// raw `at − 16 KiB` offset lands in the middle of whenever the file is not
/// pure ASCII.
fn line_start_containing(text: &str, at: usize) -> usize {
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        if at < offset + line.len() {
            return offset;
        }
        offset += line.len();
    }
    offset
}

/// The end of the last whole line that fits in the cap, counting from `start`.
///
/// A single line longer than the whole cap is the one case where the line has
/// to give: the window is cut at the last character boundary inside the cap
/// rather than sending nothing at all.
fn last_line_end_within_cap(text: &str, start: usize) -> usize {
    let mut end = start;
    for line in text[start..].split_inclusive('\n') {
        if end + line.len() > start + EXTRA_CAP {
            break;
        }
        end += line.len();
    }
    if end == start {
        text.floor_char_boundary(start + EXTRA_CAP)
    } else {
        end
    }
}

/// The start of the line that declares `name`, so the window is centred on
/// the definition rather than on the middle of the file.
fn declaration_offset(text: &str, name: &str) -> Option<usize> {
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        let mut names = BTreeSet::new();
        ladder::declaration_names(line, &mut names);
        if names.contains(name) {
            return Some(at);
        }
        at += line.len();
    }
    None
}

/// Rule (b), amended for B1: every file OTHER than G and other than the
/// masked file, whose text contains the gold verbatim (whitespace-normalised),
/// is a verbatim answer and is withheld. G is never withheld — without the
/// definition the tier is unanswerable.
fn withheld_count(gold: &str, defining: &str, corpus: &Corpus<'_>) -> u32 {
    let needle = ladder::normalise(gold);
    if needle.is_empty() {
        return 0;
    }
    let n = corpus
        .normalised
        .iter()
        .filter(|(path, text)| {
            path != defining && path != corpus.task_file && text.contains(&needle)
        })
        .count();
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// `excluded.cross_file` for a cross-file task: what was sent and what was
/// kept back, in words the report can print unchanged.
#[must_use]
pub fn cross_file_note(a: &Assembled) -> String {
    let truncated = if a.extra.truncated { ", truncated" } else { "" };
    let kib = ladder::as_f64(usize::try_from(a.extra.bytes).unwrap_or(usize::MAX)) / 1024.0;
    format!(
        "sent {} ({kib:.1} KiB{truncated}); withheld {} (contain the answer)",
        a.extra.path, a.withheld
    )
}

#[cfg(test)]
mod tests {
    use super::{
        Assembled, Corpus, Defined, FileText, Index, Meta, assemble_extra, cross_file_note,
        first_uses, ladder, normalised_corpus,
    };
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

    fn owned(files: &[(&str, &str)]) -> Vec<(String, String)> {
        files
            .iter()
            .map(|(p, t)| ((*p).to_owned(), (*t).to_owned()))
            .collect()
    }

    fn assembled(files: &[(&str, &str)], gold: &str) -> Assembled {
        let owned = owned(files);
        let normalised = normalised_corpus(&owned);
        let meta = Meta {
            name: "build".into(),
            defined_in: "src/defs.rs".into(),
            also_first_uses: vec!["Widget".into()],
        };
        assemble_extra(
            &meta,
            gold,
            &Corpus {
                files: &owned,
                normalised: &normalised,
                task_file: "src/user.rs",
            },
        )
        .expect("the defining file is in the corpus")
    }

    /// 59 bytes a line, so the fixtures below can be reasoned about in bytes.
    fn padding(lines: usize) -> String {
        "// padding padding padding padding padding padding padding\n".repeat(lines)
    }

    #[test]
    fn an_under_cap_file_is_sent_whole() {
        let a = assembled(&[("src/defs.rs", DEFS)], "let w = build(1);");
        assert_eq!(a.extra.path, "src/defs.rs");
        assert_eq!(a.extra.bytes, DEFS.len() as u64);
        assert!(!a.extra.truncated);
        assert_eq!(a.extra_text, DEFS);
        assert_eq!(a.withheld, 0);
        assert_eq!(
            cross_file_note(&a),
            format!(
                "sent src/defs.rs ({:.1} KiB); withheld 0 (contain the answer)",
                ladder::as_f64(DEFS.len()) / 1024.0
            )
        );
    }

    #[test]
    fn an_over_cap_file_is_windowed_on_the_declaration_line() {
        let filler = padding(1200);
        let big = format!("{filler}pub fn build(n: u32) -> u32 {{\n    n + 1\n}}\n{filler}");
        assert!(
            big.len() > super::EXTRA_CAP * 2,
            "the fixture must exceed the cap"
        );
        let a = assembled(&[("src/defs.rs", &big)], "let w = build(1);");
        assert!(a.extra.truncated);
        assert_eq!(a.extra.bytes, a.extra_text.len() as u64);
        assert!(
            a.extra_text.len() <= super::EXTRA_CAP,
            "{}",
            a.extra_text.len()
        );
        assert!(
            a.extra_text.contains("pub fn build(n: u32) -> u32 {"),
            "the window is centred on the declaration"
        );
        assert!(
            a.extra_text.starts_with("// padding"),
            "snapped to a line start"
        );
        assert!(a.extra_text.ends_with('\n'), "snapped to a line end");
        assert!(
            cross_file_note(&a).contains(", truncated)"),
            "{}",
            cross_file_note(&a)
        );
    }

    #[test]
    fn a_window_never_splits_a_character() {
        // 58 bytes a line, its 3-byte dashes placed so that the centred
        // window's raw start offset lands on a continuation byte.
        let filler = format!("//{}!\n", "—".repeat(18)).repeat(600);
        assert_eq!(filler.len(), 600 * 58);
        let big = format!("{filler}pub fn build(n: u32) -> u32 {{\n    n + 1\n}}\n{filler}");
        assert!(!big.is_char_boundary(filler.len() - super::EXTRA_CAP / 2));
        let a = assembled(&[("src/defs.rs", &big)], "let w = build(1);");
        assert!(a.extra.truncated);
        assert!(
            a.extra_text.len() <= super::EXTRA_CAP,
            "{}",
            a.extra_text.len()
        );
        assert!(big.contains(&a.extra_text), "the window is a slice of G");
        assert!(a.extra_text.contains("pub fn build(n: u32) -> u32 {"));
        assert!(a.extra_text.starts_with("//—"), "snapped to a line start");
    }

    #[test]
    fn a_declaration_at_either_end_of_a_big_file_is_still_in_the_window() {
        let filler = padding(1200);
        let decl = "pub fn build(n: u32) -> u32 {\n    n + 1\n}\n";
        let top = assembled(&[("src/defs.rs", &format!("{decl}{filler}"))], "build(1)");
        assert!(top.extra.truncated);
        assert!(top.extra_text.len() <= super::EXTRA_CAP);
        assert!(
            top.extra_text.starts_with(decl),
            "a declaration on line 1 has nothing before it to centre on"
        );

        let last = format!("{filler}pub fn build(n: u32) -> u32 {{ n + 1 }}");
        let end = assembled(&[("src/defs.rs", &last)], "build(1)");
        assert!(end.extra.truncated);
        assert!(end.extra_text.len() <= super::EXTRA_CAP);
        assert!(
            end.extra_text
                .ends_with("pub fn build(n: u32) -> u32 { n + 1 }"),
            "an unterminated last line is a whole line"
        );
    }

    #[test]
    fn rule_b_withholds_a_verbatim_answer_elsewhere_and_never_the_defining_file() {
        let gold = "let w = build(1);";
        let a = assembled(
            &[
                ("src/defs.rs", DEFS),
                (
                    "src/copy.rs",
                    "pub fn other() {\n    let  w  =  build(1);\n}\n",
                ),
                ("src/unrelated.rs", "pub fn nothing() {}\n"),
            ],
            gold,
        );
        assert_eq!(a.withheld, 1, "whitespace differences do not hide a copy");
        assert!(cross_file_note(&a).contains("withheld 1 (contain the answer)"));

        let in_g = format!("{DEFS}pub fn again() {{\n    let w = build(1);\n}}\n");
        let b = assembled(&[("src/defs.rs", &in_g)], gold);
        assert_eq!(b.withheld, 0, "G is the definition and is never withheld");

        let both = assembled(
            &[
                ("src/defs.rs", &in_g),
                (
                    "src/copy.rs",
                    "pub fn other() {\n    let w = build(1);\n}\n",
                ),
            ],
            gold,
        );
        assert_eq!(both.withheld, 1, "G holding the answer too adds nothing");

        let masked = assembled(
            &[
                ("src/defs.rs", DEFS),
                ("src/user.rs", "pub fn r() {\n    let w = build(1);\n}\n"),
            ],
            gold,
        );
        assert_eq!(
            masked.withheld, 0,
            "the task's own file holds the gold by construction"
        );
    }

    #[test]
    fn also_first_uses_ride_along() {
        let a = assembled(&[("src/defs.rs", DEFS)], "let w = build(1);");
        assert_eq!(a.also_first_uses, vec!["Widget".to_owned()]);
    }
}
