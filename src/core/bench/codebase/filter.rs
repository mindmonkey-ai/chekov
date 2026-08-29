//! Same-file context with the leakage filter's rule (c).
//!
//! The doc comment directly above a masked function body reveals it, so it
//! is cut from the prefix and the cut is counted. Rules (b) and (d) govern
//! cross-file context, which slice A does not build — recorded as not
//! applicable.

use super::masker::Candidate;
use super::sample::Picked;
use super::{CodebaseTask, Excluded, TaskTier};

pub const NO_CROSS_FILE: &str = "n/a: same-file";

#[must_use]
pub fn assemble(path: &str, text: &str, picked: &Picked) -> CodebaseTask {
    let c = &picked.candidate;
    let (prefix, doc_comment) = prefix_and_doc_flag(text, c);
    CodebaseTask {
        id: picked.id.clone(),
        tier: c.tier,
        file: path.to_owned(),
        line: c.line,
        gold: text[c.byte_range.clone()].to_owned(),
        prefix,
        suffix: text[c.byte_range.end..].to_owned(),
        excluded: Excluded {
            doc_comment,
            cross_file: NO_CROSS_FILE.to_owned(),
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

    fn pick(tier: TaskTier) -> Picked {
        let c = RustBraceMasker
            .candidates(SRC)
            .into_iter()
            .find(|c| c.tier == tier)
            .expect("candidate");
        Picked {
            path: "src/x.rs".into(),
            id: task_id("src/x.rs", &c),
            candidate: c,
        }
    }

    #[test]
    fn a_function_body_task_strips_the_doc_comment_and_counts_it() {
        let task = assemble("src/x.rs", SRC, &pick(TaskTier::FunctionBody));
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

    #[test]
    fn an_in_file_task_keeps_the_doc_comment_and_records_zero() {
        let task = assemble("src/x.rs", SRC, &pick(TaskTier::InFile));
        assert!(task.prefix.contains("Doc line"));
        assert_eq!(task.excluded.doc_comment, 0);
        assert_eq!(format!("{}{}{}", task.prefix, task.gold, task.suffix), SRC);
    }
}
