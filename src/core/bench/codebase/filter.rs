//! Same-file context with the leakage filter's rule (c), and the
//! `#[cfg(test)]` cutter every file passes through before it.
//!
//! The doc comment directly above a masked function body reveals it, so it
//! is cut from the prefix and the cut is counted. Rules (b) and (d) govern
//! cross-file context, which slice A does not build — recorded as not
//! applicable.

use std::ops::Range;

use super::masker::{Candidate, literal_ranges, matching_close};
use super::sample::Picked;
use super::{CodebaseTask, Excluded, TaskTier};

pub const NO_CROSS_FILE: &str = "n/a: same-file";

const CFG_TEST: &str = "#[cfg(test)]";

/// A file's text with its `#[cfg(test)]` items removed, and what that cost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Elided {
    pub text: String,
    pub lines_removed: usize,
}

/// The file kept, its `#[cfg(test)]` items cut out.
///
/// Idiomatic Rust keeps unit tests inline, so excluding every file that has
/// them throws most of a real repository away. Each attributed item is cut
/// instead, from the start of its attribute line through the matching `}` of
/// its first `{` or through its terminating `;` — literal-aware, so a brace
/// inside a string in the test module cannot end the cut early, and an
/// occurrence of the attribute inside a string or a comment is not a cut
/// point at all.
#[must_use]
pub fn elide_cfg_test(text: &str) -> Elided {
    let cuts = cfg_test_cuts(text);
    if cuts.is_empty() {
        return Elided {
            text: text.to_owned(),
            lines_removed: 0,
        };
    }
    let mut kept = String::with_capacity(text.len());
    let mut lines_removed = 0;
    let mut from = 0;
    for cut in cuts {
        kept.push_str(&text[from..cut.start]);
        lines_removed += text[cut.clone()].matches('\n').count();
        from = cut.end;
    }
    kept.push_str(&text[from..]);
    Elided {
        text: kept,
        lines_removed,
    }
}

/// Every region to remove, ascending and non-overlapping. An attribute nested
/// inside a region already claimed is consumed by it: the search resumes past
/// the outer cut's end rather than cutting twice.
fn cfg_test_cuts(text: &str) -> Vec<Range<usize>> {
    let literals = literal_ranges(text);
    let mut cuts = Vec::new();
    let mut from = 0;
    while let Some(offset) = text[from..].find(CFG_TEST) {
        let at = from + offset;
        if literals.iter().any(|r| r.contains(&at)) {
            from = at + CFG_TEST.len();
            continue;
        }
        let cut = cut_range(text, at, &literals);
        from = cut.end;
        cuts.push(cut);
    }
    cuts
}

/// One attributed item's region: the attribute's whole line through the end
/// of the item it decorates.
fn cut_range(text: &str, at: usize, literals: &[Range<usize>]) -> Range<usize> {
    let start = text[..at].rfind('\n').map_or(0, |i| i + 1);
    let end = item_end(text, at + CFG_TEST.len(), literals);
    start..drop_one_blank_line(text, start, end)
}

/// Where the decorated item ends: after the matching `}` of its first `{`, or
/// after its `;`, whichever comes first outside literals. Stacked attributes
/// and `///` lines below the attribute fall inside the region on the way
/// there — neither carries a `{` or a `;` of its own. A malformed tail with
/// neither delimiter cuts to the end of the file rather than guessing.
fn item_end(text: &str, from: usize, literals: &[Range<usize>]) -> usize {
    let Some((at, delimiter)) = first_delimiter(text, from, literals) else {
        return text.len();
    };
    if delimiter == ';' {
        return to_line_end(text, at + 1);
    }
    matching_close(text, at).map_or(text.len(), |close| to_line_end(text, close + 1))
}

/// The first `{` or `;` at or after `from` that is code rather than the
/// contents of a string, a char, or a comment.
fn first_delimiter(text: &str, from: usize, literals: &[Range<usize>]) -> Option<(usize, char)> {
    text[from..]
        .char_indices()
        .map(|(offset, c)| (from + offset, c))
        .find(|(at, c)| matches!(c, '{' | ';') && !literals.iter().any(|r| r.contains(at)))
}

/// Through the end of `at`'s line when only whitespace follows it there: the
/// newline the item left behind goes with the item. Code sharing the line
/// keeps its place, and the cut stops short.
fn to_line_end(text: &str, at: usize) -> usize {
    let end = text[at..].find('\n').map_or(text.len(), |i| at + i + 1);
    if text[at..end].trim().is_empty() {
        end
    } else {
        at
    }
}

/// The one blank line a cut between two of them would otherwise leave behind.
fn drop_one_blank_line(text: &str, start: usize, end: usize) -> usize {
    if text[..start].ends_with("\n\n") && text[end..].starts_with('\n') {
        return end + 1;
    }
    end
}

/// One task from its own file's already-elided text.
///
/// `cfg_test_lines` is what `elide_cfg_test` removed from THAT file, carried
/// onto the row so the report can say what the repository's inline tests
/// cost.
#[must_use]
pub fn assemble(text: &str, picked: &Picked, cfg_test_lines: usize) -> CodebaseTask {
    let c = &picked.candidate;
    let (prefix, doc_comment) = prefix_and_doc_flag(text, c);
    CodebaseTask {
        id: picked.id.clone(),
        tier: c.tier,
        file: picked.path.clone(),
        line: c.line,
        gold: text[c.byte_range.clone()].to_owned(),
        prefix,
        suffix: text[c.byte_range.end..].to_owned(),
        excluded: Excluded {
            doc_comment,
            cross_file: NO_CROSS_FILE.to_owned(),
            cfg_test_lines,
        },
    }
}

/// The prefix up to the masked span, with rule (c) applied: for a function
/// body whose doc comment sits directly above it, the comment is cut from
/// the prefix and the cut is counted (1); otherwise nothing is cut (0).
fn prefix_and_doc_flag(text: &str, c: &Candidate) -> (String, u8) {
    if c.tier == TaskTier::FunctionBody
        && let Some(doc) = &c.doc_comment
    {
        let prefix = format!(
            "{}{}",
            &text[..doc.start],
            &text[doc.end..c.byte_range.start]
        );
        return (prefix, 1);
    }
    (text[..c.byte_range.start].to_owned(), 0)
}

#[cfg(test)]
mod tests {
    use super::assemble;
    use crate::core::bench::codebase::TaskTier;
    use crate::core::bench::codebase::masker::{MaskSource, RustBraceMasker};
    use crate::core::bench::codebase::sample::{Picked, task_id};

    const SRC: &str = "/// Doc line.\npub fn f(a: i32) -> i32 {\n    let b = a + 1;\n    let c = b * 2;\n    c\n}\n";

    fn pick_from(src: &str, tier: TaskTier) -> Picked {
        let c = RustBraceMasker
            .candidates(src)
            .into_iter()
            .find(|c| c.tier == tier)
            .expect("candidate");
        Picked {
            path: "src/x.rs".into(),
            id: task_id("src/x.rs", &c),
            candidate: c,
        }
    }

    fn pick(tier: TaskTier) -> Picked {
        pick_from(SRC, tier)
    }

    #[test]
    fn a_function_body_task_strips_the_doc_comment_and_counts_it() {
        let task = assemble(SRC, &pick(TaskTier::FunctionBody), 4);
        assert_eq!(
            task.excluded.cfg_test_lines, 4,
            "the file's own cut rides along"
        );
        assert!(!task.prefix.contains("Doc line"), "{:?}", task.prefix);
        assert!(task.prefix.ends_with("-> i32 {"), "{:?}", task.prefix);
        assert_eq!(task.suffix, "}\n");
        assert_eq!(
            task.gold.trim(),
            "let b = a + 1;\n    let c = b * 2;\n    c"
        );
        assert_eq!(task.excluded.doc_comment, 1);
        assert_eq!(task.excluded.cross_file, "n/a: same-file");
        assert_eq!(task.file, "src/x.rs");
        assert_eq!(task.tier, TaskTier::FunctionBody);
    }

    /// An attribute between the doc block and the signature must not save the
    /// doc from the cut: `#[must_use]` is not a blank line, and the doc still
    /// names the answer.
    #[test]
    fn an_attribute_between_the_doc_and_the_signature_does_not_defeat_the_cut() {
        const ATTRIBUTED: &str = "/// Returns a plus one.\n#[must_use]\npub fn f(a: i32) -> i32 {\n    let b = a + 1;\n    let c = b * 2;\n    c\n}\n";
        let task = assemble(
            ATTRIBUTED,
            &pick_from(ATTRIBUTED, TaskTier::FunctionBody),
            0,
        );
        assert!(
            !task.prefix.contains("Returns a plus one"),
            "{:?}",
            task.prefix
        );
        assert!(
            task.prefix
                .ends_with("#[must_use]\npub fn f(a: i32) -> i32 {"),
            "the attribute stays in the prefix: {:?}",
            task.prefix
        );
        assert_eq!(task.excluded.doc_comment, 1);
    }

    #[test]
    fn an_in_file_task_keeps_the_doc_comment_and_records_zero() {
        let task = assemble(SRC, &pick(TaskTier::InFile), 0);
        assert!(task.prefix.contains("Doc line"));
        assert_eq!(task.excluded.doc_comment, 0);
        assert_eq!(format!("{}{}{}", task.prefix, task.gold, task.suffix), SRC);
    }
}

#[cfg(test)]
mod elision_tests {
    use super::elide_cfg_test;

    const TRAILING: &str = r#"pub fn keep() -> i32 {
    1
}

#[cfg(test)]
mod tests {
    #[test]
    fn t() {
        assert_eq!("}", "}");
    }
}
"#;

    const MIDFILE: &str = "fn before() -> i32 {\n    1\n}\n\n\
#[cfg(test)]\nfn helper() -> i32 {\n    2\n}\n\n\
fn after() -> i32 {\n    3\n}\n";

    const STACKED: &str = "pub fn keep() {}\n\n\
#[cfg(test)]\n#[cfg(feature = \"slow\")]\n/// Helpers for the tests below.\n\
mod tests {\n    fn t() {}\n}\n";

    #[test]
    fn a_trailing_test_module_is_cut_and_a_brace_in_a_string_does_not_end_it_early() {
        let elided = elide_cfg_test(TRAILING);
        assert_eq!(elided.text, "pub fn keep() -> i32 {\n    1\n}\n\n");
        assert_eq!(elided.lines_removed, 7, "the block is seven lines");
    }

    #[test]
    fn a_mid_file_test_helper_is_cut_and_the_production_code_below_it_survives() {
        let elided = elide_cfg_test(MIDFILE);
        assert_eq!(
            elided.text,
            "fn before() -> i32 {\n    1\n}\n\nfn after() -> i32 {\n    3\n}\n"
        );
        assert_eq!(
            elided.lines_removed, 5,
            "four lines plus the blank left over"
        );
    }

    #[test]
    fn an_attributed_use_is_cut_at_its_semicolon() {
        let elided = elide_cfg_test("use std::fmt;\n#[cfg(test)]\nuse foo::Bar;\npub fn f() {}\n");
        assert_eq!(elided.text, "use std::fmt;\npub fn f() {}\n");
        assert_eq!(elided.lines_removed, 2);
    }

    #[test]
    fn the_attribute_inside_a_string_or_a_comment_is_not_a_cut_point() {
        const QUOTED: &str =
            "const A: &str = \"#[cfg(test)]\";\n// #[cfg(test)] names it\npub fn f() {}\n";
        let elided = elide_cfg_test(QUOTED);
        assert_eq!(elided.text, QUOTED);
        assert_eq!(elided.lines_removed, 0);
    }

    #[test]
    fn a_file_without_the_attribute_comes_back_unchanged() {
        const PLAIN: &str = "/// Doc line.\npub fn f(a: i32) -> i32 {\n    a + 1\n}\n";
        let elided = elide_cfg_test(PLAIN);
        assert_eq!(elided.text, PLAIN);
        assert_eq!(elided.lines_removed, 0);
    }

    #[test]
    fn a_stacked_attribute_and_the_doc_below_it_go_with_the_item() {
        let elided = elide_cfg_test(STACKED);
        assert_eq!(elided.text, "pub fn keep() {}\n\n");
        assert!(!elided.text.contains("feature ="), "{:?}", elided.text);
        assert!(!elided.text.contains("Helpers for"), "{:?}", elided.text);
        assert_eq!(elided.lines_removed, 6);
    }

    #[test]
    fn a_nested_attribute_is_consumed_by_the_outer_cut() {
        const NESTED: &str = "pub fn keep() {}\n\n#[cfg(test)]\nmod tests {\n    \
#[cfg(test)]\n    fn inner() {}\n}\npub fn also_keep() {}\n";
        let elided = elide_cfg_test(NESTED);
        assert_eq!(elided.text, "pub fn keep() {}\n\npub fn also_keep() {}\n");
        assert_eq!(elided.lines_removed, 5);
    }

    #[test]
    fn two_independent_regions_are_both_cut_in_one_pass() {
        const TWO: &str = "#[cfg(test)]\nuse a::B;\npub fn keep() {}\n#[cfg(test)]\nmod t {\n}\n";
        let elided = elide_cfg_test(TWO);
        assert_eq!(elided.text, "pub fn keep() {}\n");
        assert_eq!(elided.lines_removed, 5);
    }
}
