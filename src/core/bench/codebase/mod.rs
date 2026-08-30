//! `chekov capability bench --codebase` — the user's own Rust repository as
//! graded same-file infill tasks (spec §8, slice A).

pub mod crossfile;
pub mod exec;
pub mod filter;
pub mod ladder;
pub mod masker;
pub mod run;
pub mod sample;
pub mod tree;

use serde::{Deserialize, Serialize};

/// Printed once per run: the masks come from a brace scanner, not a parser.
pub const MASK_LABEL: &str = "boundary-scanned (not AST)";

/// What the with-extra arm's `task_id` ends in (§5).
///
/// `run::arms` writes it and `store::base_id` strips it back off to pair the
/// two arms of one task: two literals, and a run whose ids the report could
/// not pair would show every task as two.
pub const ARM_EXTRA_SUFFIX: &str = "+extra";

/// Which kind of span was masked (`RepoBench` taxonomy; cross-file is slice B).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskTier {
    InFile,
    FunctionBody,
    CrossFileFirst,
}

impl TaskTier {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::InFile => "in_file",
            Self::FunctionBody => "function_body",
            Self::CrossFileFirst => "cross_file_first",
        }
    }
}

/// What the leakage filter removed from this task's context, per rule. Slice
/// A has no cross-file context, and says so rather than claiming a count.
///
/// `cfg_test_lines` is what the `#[cfg(test)]` cutter took out of this task's
/// file before anything else read it. Rows written before the cutter existed
/// load as 0, which is what they were.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Excluded {
    pub doc_comment: u8,
    pub cross_file: String,
    #[serde(default)]
    pub cfg_test_lines: usize,
    /// Rule (b): files other than the defining one whose text contains the
    /// gold verbatim, and so were kept out of the context. Counted even in
    /// B1, where only the defining file is ever sent, so the number exists
    /// before it starts to bite.
    #[serde(default)]
    pub cross_file_withheld: u32,
}

/// The one other file a cross-file task was shown, as the row records it.
///
/// The text is not stored: it is the file at this run's HEAD, and a 32 KiB
/// copy on every row would swamp `results.jsonl` with what the worktree can
/// reproduce.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtraFile {
    pub path: String,
    pub bytes: u64,
    pub truncated: bool,
}

/// One assembled task: what the model sees, what was hidden, and the answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodebaseTask {
    pub id: String,
    pub tier: TaskTier,
    pub file: String,
    pub line: usize,
    /// The span in the file as it sits in the worktree — test modules intact.
    ///
    /// `masker::Candidate.byte_range` indexes the ELIDED text; tier 6 splices
    /// into the original, because tier 7 runs the very test modules elision
    /// cut. `filter::original_range` is the map between them, and the
    /// invariant is `&original[byte_range] == gold`.
    pub byte_range: std::ops::Range<usize>,
    pub gold: String,
    pub prefix: String,
    pub suffix: String,
    pub excluded: Excluded,
    /// The symbol this cross-file task is keyed on, `None` on the other
    /// tiers — the name whose first use the span holds.
    pub name: Option<String>,
    /// Other names whose first use in this file also falls in this span —
    /// informational, carried onto the row.
    pub also_first_uses: Vec<String>,
    /// What the "extra" arm sent, or `None` for the other tiers.
    pub extra: Option<ExtraFile>,
    /// The extra file's bytes, empty when there is no extra. Not serialised.
    pub extra_text: String,
}

use std::path::Path;

use crate::error::ChekovError;

/// Tasks actually picked per tier — what the dry-run line and the report
/// header count.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub in_file: usize,
    pub function_body: usize,
    pub cross_file_first: usize,
}

/// `12 in_file, 6 function_body, 6 cross_file_first × 2 arms`.
///
/// The tier census the dry-run line and the report header both print, from
/// one place so the two cannot drift. `× 2 arms` is dropped when no
/// cross-file task was sampled: there is no second arm to announce.
#[must_use]
pub fn tier_counts_clause(counts: Counts) -> String {
    let arms = if counts.cross_file_first == 0 {
        ""
    } else {
        " × 2 arms"
    };
    format!(
        "{} in_file, {} function_body, {} cross_file_first{arms}",
        counts.in_file, counts.function_body, counts.cross_file_first
    )
}

/// Everything one `--codebase` run needs, sampled once before launch — the
/// worktree is gone by the time this returns.
pub struct Prepared {
    pub head: String,
    pub set_hash: String,
    pub tasks: Vec<CodebaseTask>,
    pub shortfall: Vec<String>,
    pub symbols: ladder::Symbols,
    /// Lines the `#[cfg(test)]` cutter removed across the whole walk, and how
    /// many files gave some up — printed, never silently absorbed.
    pub cfg_test_lines: usize,
    pub cfg_test_files: usize,
    pub counts: Counts,
}

/// What one file's `#[cfg(test)]` cut cost, and where it fell.
struct FileElision {
    lines: usize,
    cuts: Vec<std::ops::Range<usize>>,
}

/// Every walked file with its `#[cfg(test)]` items already cut, keyed back to
/// what each cut cost so a task's row can carry its own file's number — and
/// to WHERE each cut fell, so a span can be mapped onto the original.
struct Elisions {
    files: Vec<(String, String)>,
    per_file: std::collections::HashMap<String, FileElision>,
}

impl Elisions {
    fn lines(&self) -> usize {
        self.per_file.values().map(|e| e.lines).sum()
    }

    fn files_cut(&self) -> usize {
        self.per_file.values().filter(|e| e.lines > 0).count()
    }

    /// One file's cut list, or an empty one for a file that gave nothing up.
    fn cuts(&self, path: &str) -> &[std::ops::Range<usize>] {
        self.per_file.get(path).map_or(&[], |e| e.cuts.as_slice())
    }

    fn lines_of(&self, path: &str) -> usize {
        self.per_file.get(path).map_or(0, |e| e.lines)
    }
}

/// The cut applied to every file before masking, sampling, or the symbol set.
///
/// Idiomatic Rust keeps unit tests inline; excluding those files outright
/// would leave a real repository with almost nothing to sample from, and
/// leaving them in would offer the model its own test module as an answer.
fn elide_tests(files: Vec<(String, String)>) -> Elisions {
    let mut per_file = std::collections::HashMap::new();
    let files = files
        .into_iter()
        .map(|(path, text)| {
            let cut = filter::elide_cfg_test(&text);
            per_file.insert(
                path.clone(),
                FileElision {
                    lines: cut.lines_removed,
                    cuts: cut.cuts,
                },
            );
            (path, cut.text)
        })
        .collect();
    Elisions { files, per_file }
}

/// The short HEAD every codebase-mode name is keyed by.
#[must_use]
pub fn head12(head: &str) -> &str {
    &head[..12.min(head.len())]
}

/// Every file's candidates — the masker's spans plus the cross-file first
/// uses — with the metas and the two numbers the shortfall sentence needs.
struct Candidates {
    per_file: Vec<sample::FileCandidates>,
    /// `(file, span start)` is unique within a run.
    meta: std::collections::HashMap<(String, usize), crossfile::Meta>,
    ambiguous: std::collections::BTreeSet<String>,
    /// Rule 5's tally: names with one defining file that their own file never
    /// refers to. Distinct names, like `ambiguous`.
    no_import: std::collections::BTreeSet<String>,
    files_without_use: usize,
}

fn all_candidates(index: &crossfile::Index, elided: &Elisions) -> Candidates {
    use masker::MaskSource;
    let mut out = Candidates {
        per_file: Vec::new(),
        meta: std::collections::HashMap::new(),
        ambiguous: std::collections::BTreeSet::new(),
        no_import: std::collections::BTreeSet::new(),
        files_without_use: 0,
    };
    for (path, text) in &elided.files {
        let mut candidates = masker::RustBraceMasker.candidates(text);
        let found = crossfile::first_uses(
            index,
            &crossfile::FileText {
                path,
                text,
                spans: &candidates,
            },
        );
        out.files_without_use += usize::from(found.candidates.is_empty());
        out.ambiguous
            .extend(rule_one_ambiguous(index, path, &found.span_names));
        out.no_import.extend(found.no_import);
        for (candidate, (start, meta)) in found.candidates.into_iter().zip(found.meta) {
            out.meta.insert((path.clone(), start), meta);
            candidates.push(candidate);
        }
        out.per_file.push(sample::FileCandidates {
            path: path.clone(),
            candidates,
        });
    }
    out
}

/// The names §3.2's rule 1 actually passed over here: declared in two or more
/// files, none of which is this one.
///
/// `ambiguous_among` narrows to the names more than one file declares; a name
/// this file declares itself is among them, and rule 2 skipped that one as
/// shadowed rather than ambiguous. Counting it would let the shortfall
/// sentence claim an ambiguity the index never saw.
fn rule_one_ambiguous(
    index: &crossfile::Index,
    path: &str,
    names: &std::collections::BTreeSet<String>,
) -> std::collections::BTreeSet<String> {
    index
        .ambiguous_among(names)
        .into_iter()
        .filter(|name| matches!(index.defined_in(name, path), crossfile::Defined::Ambiguous))
        .collect()
}

/// Everything the sampled run carries out of the worktree, so `prepare`
/// stays inside 40 lines and the assembly reads one value (§3, §4).
struct Sampled {
    head: String,
    set: sample::TaskSet,
    elided: Elisions,
    candidates: Candidates,
    symbols: ladder::Symbols,
    oversized: usize,
}

/// Gate, worktree, walk, mask, index, sample, assemble, symbol set — then
/// the worktree is removed. Everything the run needs is in memory, and the
/// user's checkout was never read directly.
///
/// The scratch tree is `<scratch_root>/codebase-tree-<head12>`: keyed by the
/// HEAD it checks out, so two runs of different commits never share one, and
/// derived here rather than by the caller, which does not know the HEAD yet.
pub fn prepare(repo: &Path, scratch_root: &Path, tasks: u32) -> Result<Prepared, ChekovError> {
    tree::assert_clean(repo)?;
    let head = tree::head_sha(repo)?;
    let scratch_tree = scratch_root.join(format!("codebase-tree-{}", head12(&head)));
    let worktree = tree::Worktree::add(repo, &scratch_tree)?;
    let sources = tree::rust_sources(&worktree.path);
    let elided = elide_tests(sources.files);
    let index = crossfile::Index::build(&elided.files);
    let mut candidates = all_candidates(&index, &elided);
    let set = sample::sample(
        std::mem::take(&mut candidates.per_file),
        sample::quota(tasks),
        sample::seed_from_head(&head),
    );
    let symbols = ladder::repo_symbols(&elided.files);
    worktree.remove()?;
    if set.picked.is_empty() {
        return Err(ChekovError::CodebaseNoTasks {
            path: repo.to_path_buf(),
            reason: format!(
                "scanned {} files, {} eligible, 0 candidate spans",
                sources.scanned,
                elided.files.len()
            ),
        });
    }
    Ok(into_prepared(Sampled {
        head,
        set,
        elided,
        candidates,
        symbols,
        oversized: sources.oversized,
    }))
}

fn into_prepared(s: Sampled) -> Prepared {
    let normalised = crossfile::normalised_corpus(&s.elided.files);
    let mut shortfall = s.set.shortfall.clone();
    shortfall.extend(cross_shortfall(&s.set, &s.candidates));
    Prepared {
        set_hash: sample::task_set_hash(&s.set),
        tasks: assembled_tasks(
            &s.set.picked,
            &Assembly {
                elided: &s.elided,
                candidates: &s.candidates,
                normalised: &normalised,
            },
        ),
        counts: counts_of(&s.set),
        head: s.head,
        shortfall: with_oversized(shortfall, s.oversized),
        symbols: s.symbols,
        cfg_test_lines: s.elided.lines(),
        cfg_test_files: s.elided.files_cut(),
    }
}

fn counts_of(set: &sample::TaskSet) -> Counts {
    let picked = |tier| {
        set.lanes
            .iter()
            .find(|l| l.tier == tier)
            .map_or(0, |l| l.picked)
    };
    Counts {
        in_file: picked(TaskTier::InFile),
        function_body: picked(TaskTier::FunctionBody),
        cross_file_first: picked(TaskTier::CrossFileFirst),
    }
}

/// `cross_file_first: 4 of 6 (2 short: 17 ambiguous names skipped, 5 names
/// skipped (no import of the defining module), 9 files have no cross-file
/// use)` — the lane's own reason, which `sample` cannot know. `None` when the
/// lane was filled.
fn cross_shortfall(set: &sample::TaskSet, candidates: &Candidates) -> Option<String> {
    let lane = set
        .lanes
        .iter()
        .find(|l| l.tier == TaskTier::CrossFileFirst)?;
    if lane.picked >= lane.want {
        return None;
    }
    Some(format!(
        "cross_file_first: {} of {} ({} short: {} ambiguous names skipped, \
         {} names skipped (no import of the defining module), \
         {} files have no cross-file use)",
        lane.picked,
        lane.want,
        lane.want - lane.picked,
        candidates.ambiguous.len(),
        candidates.no_import.len(),
        candidates.files_without_use
    ))
}

/// What assembly reads besides the picked spans (§4).
struct Assembly<'a> {
    elided: &'a Elisions,
    candidates: &'a Candidates,
    normalised: &'a [(String, String)],
}

/// Assembled tasks for the picked spans, matched back to their file's elided
/// text, that file's own elision count, and — for the cross-file tier — the
/// defining file.
fn assembled_tasks(picked: &[sample::Picked], a: &Assembly) -> Vec<CodebaseTask> {
    let by_path: std::collections::HashMap<&str, &str> = a
        .elided
        .files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    picked
        .iter()
        .filter_map(|p| {
            let text = *by_path.get(p.path.as_str())?;
            Some(filter::assemble(
                p,
                &filter::Context {
                    text,
                    cfg_test_lines: a.elided.lines_of(&p.path),
                    cuts: a.elided.cuts(&p.path),
                    cross: cross_for(p, a, text).as_ref(),
                },
            ))
        })
        .collect()
}

/// One picked span's cross-file assembly, or `None` for the other tiers.
fn cross_for(p: &sample::Picked, a: &Assembly, text: &str) -> Option<crossfile::Assembled> {
    if p.candidate.tier != TaskTier::CrossFileFirst {
        return None;
    }
    let meta = a
        .candidates
        .meta
        .get(&(p.path.clone(), p.candidate.byte_range.start))?;
    crossfile::assemble_extra(
        meta,
        &text[p.candidate.byte_range.clone()],
        &crossfile::Corpus {
            files: &a.elided.files,
            normalised: a.normalised,
            task_file: &p.path,
        },
    )
}

/// The sampler's shortfall, plus the files the walk never offered it — a task
/// set drawn from less than the repository says so.
fn with_oversized(mut shortfall: Vec<String>, oversized: usize) -> Vec<String> {
    if oversized > 0 {
        shortfall.push(format!("{oversized} files over 200 KiB skipped"));
    }
    shortfall
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::process::Command;

    use super::{CodebaseTask, Prepared, TaskTier, crossfile, filter, prepare, rule_one_ambiguous};

    /// A production function with a real body, and the inline test module a
    /// Rust file of this shape always has.
    fn source(name: &str) -> String {
        format!(
            "pub fn {name}(a: i32) -> i32 {{\n    let b = a + 1;\n    let c = b * 2;\n    c\n}}\n\n\
             #[cfg(test)]\nmod tests {{\n    #[test]\n    fn t() {{\n        \
             assert_eq!(super::{name}(1), 4);\n    }}\n}}\n"
        )
    }

    fn git(repo: &std::path::Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .status()
            .expect("git")
            .success();
        assert!(ok, "git {args:?}");
    }

    /// A committed two-file repo and the `Prepared` sampled from it. The repo
    /// path comes back because the worktree is gone by then, and a clean
    /// checkout of the same HEAD has byte-identical files.
    fn prepared_fixture(name: &str) -> (PathBuf, Prepared) {
        let root = std::env::temp_dir().join("chekov-test-codebase").join(name);
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join("src")).expect("src dir");
        std::fs::write(repo.join("src/alpha.rs"), source("alpha")).expect("alpha");
        std::fs::write(repo.join("src/beta.rs"), source("beta")).expect("beta");
        git(&repo, &["init", "-q"]);
        git(&repo, &["config", "user.email", "t@t"]);
        git(&repo, &["config", "user.name", "t"]);
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-qm", "fixture"]);
        let prepared = prepare(&repo, &root.join("scratch"), 8).expect("prepare");
        (repo, prepared)
    }

    /// Every assembled task's `byte_range` indexes the file as it sits in the
    /// worktree — test modules intact — and lands exactly on the gold.
    #[test]
    fn every_tasks_byte_range_indexes_the_worktrees_own_file() {
        let (repo, prepared) = prepared_fixture("byte-range");
        assert!(!prepared.tasks.is_empty(), "the fixture yields tasks");
        for task in &prepared.tasks {
            let original =
                std::fs::read_to_string(repo.join(&task.file)).expect("the file on disk");
            assert_eq!(
                &original[task.byte_range.clone()],
                task.gold,
                "task {} in {}",
                task.id,
                task.file
            );
        }
    }

    /// `name` keys the directory: two tests that both want this shape must not
    /// build it in the same place, or one deletes the other's checkout while
    /// `git init` is still running.
    fn repo_with_inline_tests(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("chekov-test-codebase-prepare")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).expect("mkdir");
        for name in ["one", "two", "three"] {
            std::fs::write(dir.join(format!("src/{name}.rs")), source(name)).expect("write");
        }
        let author = ["-c", "user.email=t@t", "-c", "user.name=t"];
        git(&dir, &["init", "-q"]);
        git(&dir, &[&author[..], &["add", "."]].concat());
        git(
            &dir,
            &[&author[..], &["commit", "-q", "-m", "init"]].concat(),
        );
        dir
    }

    /// Three files that would all have been excluded under the old rule now
    /// all produce tasks, and no task can see a test the cutter removed.
    #[test]
    fn prepare_keeps_files_with_inline_tests_and_cuts_the_tests_out_of_them() {
        let dir = repo_with_inline_tests("inline");
        let scratch = std::env::temp_dir()
            .join("chekov-test-codebase-prepare")
            .join("scratch");
        let prepared = prepare(&dir, &scratch, 6).expect("prepare");
        let files: std::collections::BTreeSet<&str> =
            prepared.tasks.iter().map(|t| t.file.as_str()).collect();
        assert_eq!(files.len(), 3, "{files:?}");
        assert_eq!(prepared.cfg_test_files, 3);
        assert!(prepared.cfg_test_lines > 0, "{}", prepared.cfg_test_lines);
        for task in &prepared.tasks {
            assert!(task.excluded.cfg_test_lines > 0, "{}", task.file);
            for part in [&task.prefix, &task.gold, &task.suffix] {
                assert!(!part.contains("#[cfg(test)]"), "{part:?}");
                assert!(!part.contains("mod tests"), "{part:?}");
            }
        }
    }

    /// Two files: one defines, the other calls into it — the shape the
    /// cross-file tier exists for. `name` keys the directory, so two tests
    /// that want this shape never wipe each other's checkout.
    fn repo_with_a_cross_file_call(name: &str) -> PathBuf {
        let dir = scratch_for(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).expect("mkdir");
        std::fs::write(
            dir.join("src/defs.rs"),
            "pub struct Widget {\n    pub id: u32,\n}\n\n\
             pub fn build(n: u32) -> u32 {\n    let m = n + 1;\n    let k = m * 2;\n    k\n}\n",
        )
        .expect("write");
        std::fs::write(
            dir.join("src/user.rs"),
            "use crate::defs::build;\n\
             pub fn run(n: u32) -> u32 {\n    let a = build(n);\n    let b = a + 1;\n    \
             let c = b * 3;\n    c\n}\n",
        )
        .expect("write");
        let author = ["-c", "user.email=t@t", "-c", "user.name=t"];
        git(&dir, &["init", "-q"]);
        git(&dir, &[&author[..], &["add", "."]].concat());
        git(
            &dir,
            &[&author[..], &["commit", "-q", "-m", "init"]].concat(),
        );
        dir
    }

    fn scratch_for(name: &str) -> PathBuf {
        std::env::temp_dir()
            .join("chekov-test-codebase-prepare")
            .join(name)
    }

    /// Every tier but the cross-file one records that there was no other file,
    /// rather than claiming a count it does not have.
    fn assert_the_other_tiers_carry_none(tasks: &[CodebaseTask]) {
        for other in tasks.iter().filter(|t| t.tier != TaskTier::CrossFileFirst) {
            assert_eq!(other.excluded.cross_file, filter::NO_CROSS_FILE);
            assert!(other.extra.is_none(), "{}", other.id);
            assert!(other.extra_text.is_empty(), "{}", other.id);
        }
    }

    #[test]
    fn a_cross_file_task_carries_the_defining_file_and_the_others_carry_none() {
        let prepared = prepare(
            &repo_with_a_cross_file_call("cross"),
            &scratch_for("scratch-cross"),
            24,
        )
        .expect("prepare");
        let cross: Vec<_> = prepared
            .tasks
            .iter()
            .filter(|t| t.tier == TaskTier::CrossFileFirst)
            .collect();
        assert_eq!(cross.len(), 1, "{:?}", prepared.shortfall);
        let task = cross[0];
        assert_eq!(task.file, "src/user.rs");
        let extra = task.extra.as_ref().expect("the defining file");
        assert_eq!(extra.path, "src/defs.rs");
        assert!(!extra.truncated);
        assert!(
            task.extra_text.contains("pub fn build"),
            "{}",
            task.extra_text
        );
        assert_eq!(extra.bytes, task.extra_text.len() as u64);
        assert!(
            task.excluded.cross_file.starts_with("sent src/defs.rs ("),
            "{}",
            task.excluded.cross_file
        );
        assert_eq!(task.excluded.cross_file_withheld, 0);
        assert_eq!(
            task.excluded.doc_comment, 0,
            "rule (c): a cross-file span is a statement, so there is no doc comment to cut"
        );
        assert_eq!(prepared.counts.cross_file_first, 1);
        assert_the_other_tiers_carry_none(&prepared.tasks);
    }

    #[test]
    fn a_short_cross_lane_says_how_many_names_were_ambiguous_and_how_many_files_had_no_use() {
        let prepared = prepare(
            &repo_with_inline_tests("inline-short"),
            &scratch_for("scratch-noshort"),
            24,
        )
        .expect("prepare");
        let line = prepared
            .shortfall
            .iter()
            .find(|s| s.starts_with("cross_file_first: "))
            .expect("the short lane reports itself");
        assert_eq!(
            line,
            "cross_file_first: 0 of 6 (6 short: 0 ambiguous names skipped, \
             0 names skipped (no import of the defining module), \
             3 files have no cross-file use)",
            "{:?}",
            prepared.shortfall
        );
    }

    /// Three files that all call `build`, and none of which imports the file
    /// that declares it — the shape §3.2's rule 5 exists for. The lane is
    /// empty and the sentence says why, name by name.
    fn repo_without_imports() -> PathBuf {
        let dir = scratch_for("no-import");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).expect("mkdir");
        std::fs::write(
            dir.join("src/defs.rs"),
            "pub fn build(n: u32) -> u32 {\n    let m = n + 1;\n    let k = m * 2;\n    k\n}\n",
        )
        .expect("write");
        std::fs::write(
            dir.join("src/user.rs"),
            "pub fn run(x: u32) -> u32 {\n    let a = x.build();\n    let b = a + 1;\n    \
             let c = b * 3;\n    c\n}\n",
        )
        .expect("write");
        let author = ["-c", "user.email=t@t", "-c", "user.name=t"];
        git(&dir, &["init", "-q"]);
        git(&dir, &[&author[..], &["add", "."]].concat());
        git(
            &dir,
            &[&author[..], &["commit", "-q", "-m", "init"]].concat(),
        );
        dir
    }

    /// Rule 2 gets its own name back: a name F declares itself is SHADOWED,
    /// not ambiguous, and the shortfall must not count it as an ambiguity the
    /// index saw. From any other file the very same name really is ambiguous.
    #[test]
    fn a_name_declared_in_both_files_is_shadowed_not_ambiguous() {
        let files = vec![
            ("src/f.rs".to_owned(), "pub fn build() {}\n".to_owned()),
            ("src/g.rs".to_owned(), "pub fn build() {}\n".to_owned()),
        ];
        let index = crossfile::Index::build(&files);
        let names: std::collections::BTreeSet<String> = ["build".to_owned()].into();
        assert!(
            rule_one_ambiguous(&index, "src/f.rs", &names).is_empty(),
            "rule 2 skipped it as a local shadow — counting it here would claim \
             an ambiguity the index never had to resolve"
        );
        assert_eq!(
            rule_one_ambiguous(&index, "src/h.rs", &names),
            names,
            "from a third file the same name is genuinely ambiguous"
        );
    }

    /// The set is a function of HEAD, so the same commit prepares the same
    /// tasks with the same defining files — otherwise `corpus_id` promises a
    /// comparability it does not have.
    #[test]
    fn preparing_the_same_commit_twice_yields_the_same_set_and_the_same_extra_files() {
        let repo = repo_with_a_cross_file_call("cross-twice");
        let first = prepare(&repo, &scratch_for("scratch-det-a"), 24).expect("prepare");
        let second = prepare(&repo, &scratch_for("scratch-det-b"), 24).expect("prepare");
        assert_eq!(first.set_hash, second.set_hash);
        let extras = |p: &Prepared| {
            p.tasks
                .iter()
                .map(|t| {
                    (
                        t.id.clone(),
                        t.extra.as_ref().map(|e| e.path.clone()),
                        t.name.clone(),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(extras(&first), extras(&second));
        assert!(
            extras(&first)
                .iter()
                .any(|(_, path, name)| path.is_some() && name.is_some()),
            "the fixture has a crossing, so the comparison is not vacuous"
        );
    }

    /// A method call whose name happens to be declared somewhere is not a
    /// crossing: without an import, the defining file is not the file to read.
    #[test]
    fn a_call_whose_defining_module_the_file_never_names_is_counted_not_crossed() {
        let prepared =
            prepare(&repo_without_imports(), &scratch_for("scratch-noimp"), 24).expect("prepare");
        assert_eq!(prepared.counts.cross_file_first, 0);
        let line = prepared
            .shortfall
            .iter()
            .find(|s| s.starts_with("cross_file_first: "))
            .expect("the short lane reports itself");
        assert!(
            line.contains("1 names skipped (no import of the defining module)"),
            "{line}"
        );
    }
}
