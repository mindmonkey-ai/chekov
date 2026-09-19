//! Tasks named by a manifest instead of sampled by seed — how the compiled-in
//! fixture's devices reach the codebase pipeline.

use super::TaskTier;
use super::masker::{self, Candidate};
use super::sample::{FileCandidates, Lane, Picked, TaskSet};
use crate::error::ChekovError;

/// One masked body by name: `name`, or `Owner::name` when the name repeats
/// in the file (a trait implemented twice).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedTask {
    pub id: String,
    pub file: String,
    pub symbol: String,
}

/// What a manifest-driven run carries into `prepare_named`.
#[derive(Debug)]
pub struct NamedTasks {
    pub tasks: Vec<NamedTask>,
    pub hidden: Vec<super::HiddenTest>,
    /// The corpus id the run head records — the fixture's id and content hash.
    pub corpus: String,
}

/// Every named task as a `function_body` pick, in manifest order. One lane,
/// filled exactly: a name that resolves to zero or several bodies is a
/// refused run, never a shortfall.
pub fn pick(
    files: &[FileCandidates],
    texts: &[(String, String)],
    tasks: &[NamedTask],
) -> Result<TaskSet, ChekovError> {
    let mut set = TaskSet::default();
    for task in tasks {
        set.picked.push(pick_one(files, texts, task)?);
    }
    let n = set.picked.len();
    set.lanes.push(Lane {
        tier: TaskTier::FunctionBody,
        picked: n,
        want: n,
        have: n,
    });
    Ok(set)
}

fn pick_one(
    files: &[FileCandidates],
    texts: &[(String, String)],
    task: &NamedTask,
) -> Result<Picked, ChekovError> {
    let text = texts
        .iter()
        .find(|(p, _)| *p == task.file)
        .map(|(_, t)| t.as_str());
    let spans = files
        .iter()
        .find(|f| f.path == task.file)
        .map(|f| f.candidates.as_slice());
    let (Some(text), Some(spans)) = (text, spans) else {
        return Err(missing(task, 0));
    };
    let (owner, name) = split_symbol(&task.symbol);
    let found: Vec<&Candidate> = spans
        .iter()
        .filter(|c| c.tier == TaskTier::FunctionBody)
        .filter(|c| masker::enclosing_fn(text, c.byte_range.start).as_deref() == Some(name))
        .filter(|c| {
            owner.is_none_or(|o| {
                impl_header_around(text, c.byte_range.start).is_some_and(|h| names(h, o))
            })
        })
        .collect();
    match found.as_slice() {
        [one] => Ok(Picked {
            path: task.file.clone(),
            candidate: (*one).clone(),
            id: task.id.clone(),
        }),
        several => Err(missing(task, several.len())),
    }
}

fn missing(task: &NamedTask, found: usize) -> ChekovError {
    ChekovError::CodebaseNoTasks {
        path: task.file.clone().into(),
        reason: format!(
            "fixture task {}: {found} function bodies named {} in {}",
            task.id, task.symbol, task.file
        ),
    }
}

/// `Owner::name` → `(Some("Owner"), "name")`; `name` → `(None, "name")`.
fn split_symbol(symbol: &str) -> (Option<&str>, &str) {
    symbol
        .rsplit_once("::")
        .map_or((None, symbol), |(owner, name)| (Some(owner), name))
}

/// The `impl …` header whose block contains `at`, or `None` for a free fn.
fn impl_header_around(text: &str, at: usize) -> Option<&str> {
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let rest = line.trim_start().strip_prefix("impl");
        let is_impl = rest.is_some_and(|r| r.starts_with('<') || r.starts_with(' '));
        if is_impl
            && let Some(open) = text[offset..].find('{').map(|i| offset + i)
            && let Some(close) = masker::matching_close(text, open)
            && (open..close).contains(&at)
        {
            return Some(text[offset..open].trim());
        }
        offset += line.len();
    }
    None
}

/// `owner` as a whole word in the header (`impl Audit for LimitedStore`).
fn names(header: &str, owner: &str) -> bool {
    header
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .any(|word| word == owner)
}

#[cfg(test)]
mod tests {
    use super::{NamedTask, pick};
    use crate::core::bench::codebase::masker::{MaskSource, RustBraceMasker};
    use crate::core::bench::codebase::sample::FileCandidates;

    const STORE: &str = "\
pub struct VecStore;
impl Audit for VecStore {
    fn record(&mut self) -> u32 {
        let n = 1;
        n
    }
}
pub struct LimitedStore;
impl Audit for LimitedStore {
    fn record(&mut self) -> u32 {
        let n = 2;
        n
    }
}
pub fn handle_credit(x: u32) -> u32 {
    let y = x + 1;
    let z = y + 1;
    z
}
";

    fn files() -> (Vec<FileCandidates>, Vec<(String, String)>) {
        let candidates = RustBraceMasker.candidates(STORE);
        (
            vec![FileCandidates {
                path: "src/store.rs".into(),
                candidates,
            }],
            vec![("src/store.rs".into(), STORE.into())],
        )
    }

    fn task(id: &str, symbol: &str) -> NamedTask {
        NamedTask {
            id: id.into(),
            file: "src/store.rs".into(),
            symbol: symbol.into(),
        }
    }

    #[test]
    fn an_owner_qualified_name_picks_exactly_that_impls_body() {
        let (files, texts) = files();
        let set = pick(
            &files,
            &texts,
            &[
                task("d1", "LimitedStore::record"),
                task("d2", "handle_credit"),
            ],
        )
        .expect("both resolve");
        assert_eq!(set.picked.len(), 2);
        assert_eq!(set.picked[0].id, "d1");
        assert!(STORE[set.picked[0].candidate.byte_range.clone()].contains("let n = 2;"));
        assert_eq!(set.picked[1].id, "d2");
        assert!(STORE[set.picked[1].candidate.byte_range.clone()].contains("x + 1"));
        assert_eq!(set.lanes.len(), 1);
        assert_eq!((set.lanes[0].picked, set.lanes[0].want), (2, 2));
        assert!(set.shortfall.is_empty());
    }

    #[test]
    fn a_bare_name_that_repeats_and_a_name_that_is_absent_are_both_refused_with_the_count() {
        let (files, texts) = files();
        let twice = pick(&files, &texts, &[task("d1", "record")])
            .expect_err("ambiguous")
            .to_string();
        assert!(
            twice.contains("d1") && twice.contains("2 function bodies named record"),
            "{twice}"
        );
        let none = pick(&files, &texts, &[task("d9", "nope")])
            .expect_err("absent")
            .to_string();
        assert!(none.contains("0 function bodies named nope"), "{none}");
        let wrong_file = pick(
            &files,
            &texts,
            &[NamedTask {
                id: "d1".into(),
                file: "src/other.rs".into(),
                symbol: "record".into(),
            }],
        )
        .expect_err("file");
        assert!(wrong_file.to_string().contains("src/other.rs"));
    }
}
