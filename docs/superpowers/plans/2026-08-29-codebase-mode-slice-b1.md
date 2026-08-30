# Codebase Mode Slice B1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `capability bench --codebase <PATH>` gains the `cross_file_first` tier — mask the first use in a file of a symbol defined in another file, cross it twice (without and with that other file in llama.cpp's `input_extra`), and print the measured lift reading the repository buys.

**Architecture:** One new module, `core/bench/codebase/crossfile.rs`, builds a declaration index over the elided file texts, finds first-use spans, and assembles the one extra file (32 KiB cap, windowed on the declaration line) plus rule (b)'s withheld count. `sample::Quota` grows a third lane (12/6/6 at the default 24). `runner::InfillTask` grows `extra: Option<ExtraChunk>` and serialises `input_extra`. The per-task run loop moves out of `commands/capability.rs` into a new `core/bench/codebase/run.rs` — first as a pure move, then gaining the two-arm crossing. `store.rs` gains three fields and three report lines. Everything below the live run is unit-tested without a model.

**Tech Stack:** Rust (edition 2024, ≥1.88), `serde`/`serde_json`, `clap`, the house `hash::sha256_hex`; **no new crate**. Git via `std::process::Command`.

**Spec:** `docs/superpowers/specs/2026-08-29-codebase-mode-slice-b1-design.md` (builds on `2026-08-29-codebase-mode-slice-a-design.md`, shipped in PR #47).

## Global Constraints

- Rust 2024; **no new crates**; every function ≤ 40 LOC, ≤ 3 parameters (bundle into a struct past that), nesting ≤ 3 — `clippy.toml` sets `too-many-arguments-threshold = 3`, `too-many-lines-threshold = 40`, `excessive-nesting-threshold = 4`; `cargo clippy --all-targets -- -D warnings` with the crate's pedantic+nursery set is the gate; `#[allow]`/`#[expect]` are blocked by pushkin on gated paths — extract a helper instead.
- clippy's `float_cmp` is on — f64 test assertions go through an `approx` helper; `missing_const_for_fn`, `too_long_first_doc_paragraph`, `similar_names`, `default_trait_access`, `case_sensitive_file_extension_comparisons` all fire in this crate.
- Every `ChekovError` Display names its remediation; nothing degrades silently (N/A / skipped are never rendered as zero or pass; every exclusion is counted and printed).
- `CodebaseRow` keeps `#[serde(deny_unknown_fields)]`; every new field is `#[serde(default)]` so slice-A rows load.
- Commit trailer on every commit (both lines verbatim):
  `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`
  `Claude-Session: https://claude.ai/code/session_01YDaTTHJySW2ix3P85BcP6W`
- `pushkin floor` (fmt + clippy + tests) green before every commit.
- Bash chains use `&&`, never `;`; never `cd`; src files are read with ranged `Read` (`offset`/`limit`) and written with `Edit` — whole-file reads and `cat`/`grep`/`sed` on `src/**` are blocked by the pushkin gate.
- **Line numbers in this plan are from HEAD `357dead`, and unrelated work on `bench/compare.rs` has already shifted `store.rs` by ~60 lines in its middle.** Locate every edit site by the symbol named beside the number (`render_codebase`, `assemble`, `infill_body`, …), never by the number alone; if the two disagree, the symbol wins.

---

## File structure

| File | Responsibility in this slice |
|---|---|
| `src/core/bench/codebase/mod.rs` | `TaskTier::CrossFileFirst`; `ExtraFile`; `CodebaseTask.{extra, extra_text, also_first_uses}`; `Excluded.cross_file_withheld`; `Prepared.counts`; the `prepare` wiring that builds the index and merges the third lane |
| `src/core/bench/codebase/crossfile.rs` | **new** — the declaration index, first-use detection, candidate + meta production, the 32 KiB extra window, rule (b) withholding, the descriptive `excluded.cross_file` string |
| `src/core/bench/codebase/sample.rs` | `Quota.cross_file_first`, the new rounding rule, `Lane`, the third sampling lane |
| `src/core/bench/codebase/filter.rs` | `assemble` takes an `Assembly` bundle so a cross-file task carries its extra |
| `src/core/bench/codebase/ladder.rs` | `normalise` and the declaration-only half of `collect_declarations` become `pub(super)`; `Scored.extra` feeds tier 5's `Known.context` on the with-extra arm |
| `src/core/bench/codebase/run.rs` | **new** — the per-task run loop moved out of the command layer, then the two arms, the latch, and the `--resume` key per arm |
| `src/core/bench/runner.rs` | `ExtraChunk<'a>`, `InfillTask.extra`, `input_extra` on the wire |
| `src/core/bench/store.rs` | `CodebaseRow.{arm, extra, also_first_uses}`; the header's crossings/arms counts; the three new report lines; the 24-wide label column |
| `src/commands/capability.rs` | the estimate, the dry-run line, the `Sink` handoff; the run cluster leaves this file |
| `README.md`, `CHANGELOG.md`, `IDEAS.md`, the slice-A spec | docs |

---

### Task 1: The third tier, the third quota lane, and the task's new fields

**Files:**
- Modify: `src/core/bench/codebase/mod.rs:16-58` (`TaskTier`, `Excluded`, `CodebaseTask`), plus a new `ExtraFile`
- Modify: `src/core/bench/codebase/sample.rs:29-110` (`Quota`, `quota`, `TaskSet`, `sample`), tests at `:187-277`
- Modify: `src/core/bench/codebase/filter.rs:137-154` (`assemble` fills the new fields with their empty values for now)
- Modify: `src/commands/capability.rs:2308-2323` (`codebase_task_fixture` gains the new fields)

**Interfaces:**
- Produces:
  ```rust
  // codebase/mod.rs
  pub enum TaskTier { InFile, FunctionBody, CrossFileFirst }   // serde snake_case
  impl TaskTier { pub const fn label(self) -> &'static str }   // + "cross_file_first"

  #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
  pub struct ExtraFile { pub path: String, pub bytes: u64, pub truncated: bool }

  pub struct Excluded {
      pub doc_comment: u8,
      pub cross_file: String,
      pub cfg_test_lines: usize,
      pub cross_file_withheld: u32,          // #[serde(default)]
  }

  pub struct CodebaseTask {
      pub id: String, pub tier: TaskTier, pub file: String, pub line: usize,
      pub gold: String, pub prefix: String, pub suffix: String,
      pub excluded: Excluded,
      pub also_first_uses: Vec<String>,
      pub extra: Option<ExtraFile>,
      pub extra_text: String,                // "" when there is no extra; never serialised
  }

  // codebase/sample.rs
  pub struct Quota { pub in_file: usize, pub function_body: usize, pub cross_file_first: usize }
  pub fn quota(total: u32) -> Quota;
  pub struct Lane { pub tier: TaskTier, pub picked: usize, pub want: usize, pub have: usize }
  pub struct TaskSet { pub picked: Vec<Picked>, pub shortfall: Vec<String>, pub lanes: Vec<Lane> }
  ```
- Consumes: nothing from later tasks.

**Why `extra_text` is on the task but not on the row:** the model needs the bytes at run time; the row needs only enough to audit what was sent. `CodebaseTask` is a plain struct (no `Serialize`), so the text costs nothing on disk, and a 32 KiB copy on every row would swamp `results.jsonl` with text already recoverable from the repository at this run's HEAD.

- [ ] **Step 1: Write the failing tests**

In `src/core/bench/codebase/sample.rs`, replace the body of `quota_is_two_thirds_in_file_rounded_up` (`:212-217`) with this test, renamed:

```rust
    #[test]
    fn quota_is_half_in_file_then_an_even_split_and_never_loses_a_task() {
        let q = quota(24);
        assert_eq!((q.in_file, q.function_body, q.cross_file_first), (12, 6, 6));
        let q = quota(10);
        assert_eq!((q.in_file, q.function_body, q.cross_file_first), (5, 3, 2));
        let q = quota(1);
        assert_eq!((q.in_file, q.function_body, q.cross_file_first), (1, 0, 0));
        for total in 0_u32..=40 {
            let q = quota(total);
            assert_eq!(
                q.in_file + q.function_body + q.cross_file_first,
                total as usize,
                "quota({total}) must spend every task"
            );
        }
    }
```

In the same tests module, extend the `files` helper (`:196-210`) so every file offers all three tiers, and add a lane test:

```rust
    fn files(n_files: usize, per_file: usize) -> Vec<FileCandidates> {
        (0..n_files)
            .map(|f| FileCandidates {
                path: format!("src/f{f}.rs"),
                candidates: (1..=per_file)
                    .flat_map(|l| {
                        [
                            cand(TaskTier::InFile, l),
                            cand(TaskTier::FunctionBody, 100 + l),
                            cand(TaskTier::CrossFileFirst, 200 + l),
                        ]
                    })
                    .collect(),
            })
            .collect()
    }

    #[test]
    fn the_cross_file_lane_is_sampled_and_reported_like_the_others() {
        let set = sample(files(4, 10), quota(24), seed_from_head("abc123"));
        let cross = set
            .picked
            .iter()
            .filter(|p| p.candidate.tier == TaskTier::CrossFileFirst)
            .count();
        assert_eq!(cross, 6, "the third lane takes its 6");
        let lane = set
            .lanes
            .iter()
            .find(|l| l.tier == TaskTier::CrossFileFirst)
            .copied()
            .expect("a lane per tier");
        assert_eq!((lane.picked, lane.want, lane.have), (6, 6, 40));
        let mut per_file = std::collections::BTreeMap::new();
        for p in set
            .picked
            .iter()
            .filter(|p| p.candidate.tier == TaskTier::CrossFileFirst)
        {
            *per_file.entry(p.path.clone()).or_insert(0) += 1;
        }
        assert_eq!(per_file.len(), 4, "stratified across every file: {per_file:?}");
        assert!(
            per_file.values().all(|&n| n <= 3),
            "at most one per file per pass: {per_file:?}"
        );
    }

    #[test]
    fn a_cross_file_id_changes_the_set_hash() {
        let a = sample(files(4, 10), quota(24), 5);
        let mut b = sample(files(4, 10), quota(24), 5);
        assert_eq!(task_set_hash(&a), task_set_hash(&b));
        b.picked.retain(|p| p.candidate.tier != TaskTier::CrossFileFirst);
        assert_ne!(
            task_set_hash(&a),
            task_set_hash(&b),
            "the hash covers the new tier's ids"
        );
    }
```

Update `a_short_tier_is_reported_not_filled_from_the_other` (`:245-264`) for the third field:

```rust
    #[test]
    fn a_short_tier_is_reported_not_filled_from_the_other() {
        let mut only_in_file = files(2, 3);
        for f in &mut only_in_file {
            f.candidates.retain(|c| c.tier == TaskTier::InFile);
        }
        let set = sample(
            only_in_file,
            Quota {
                in_file: 4,
                function_body: 8,
                cross_file_first: 0,
            },
            1,
        );
        assert_eq!(set.picked.len(), 4);
        assert_eq!(
            set.shortfall,
            vec!["function_body: 0 of 8 requested (repo has 0 candidates)"],
            "the cross lane wanted nothing, so it is not short"
        );
    }
```

And add the id-shape assertion to `task_ids_are_stable_and_readable` (`:266-276`), at the end of its body:

```rust
        let cross = task_id("src/lib.rs", &cand(TaskTier::CrossFileFirst, 12));
        assert_eq!(&cross[..17], "cross_file_first-", "{cross}");
        assert!(cross.ends_with("-L12"), "{cross}");
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --lib codebase::sample 2>&1 | tail -30`
Expected: compile errors — `no variant or associated item named 'CrossFileFirst'`, `struct 'Quota' has no field named 'cross_file_first'`, `no field 'lanes' on type 'TaskSet'`.

- [ ] **Step 3: Implement the tier and the task fields**

`src/core/bench/codebase/mod.rs` — extend the enum (`:18-31`):

```rust
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
```

Add the new field to `Excluded` (after `cfg_test_lines`):

```rust
    /// Rule (b): files other than the defining one whose text contains the
    /// gold verbatim, and so were kept out of the context. Counted even in
    /// B1, where only the defining file is ever sent, so the number exists
    /// before it starts to bite.
    #[serde(default)]
    pub cross_file_withheld: u32,
```

Add `ExtraFile` directly below `Excluded`:

```rust
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
```

Extend `CodebaseTask` (`:49-58`) with three fields:

```rust
pub struct CodebaseTask {
    pub id: String,
    pub tier: TaskTier,
    pub file: String,
    pub line: usize,
    pub gold: String,
    pub prefix: String,
    pub suffix: String,
    pub excluded: Excluded,
    /// Other names whose first use in this file also falls in this span —
    /// informational, carried onto the row.
    pub also_first_uses: Vec<String>,
    /// What the "extra" arm sent, or `None` for the other tiers.
    pub extra: Option<ExtraFile>,
    /// The extra file's bytes, empty when there is no extra. Not serialised.
    pub extra_text: String,
}
```

- [ ] **Step 4: Implement the quota lane**

`src/core/bench/codebase/sample.rs` — replace `Quota`/`quota` (`:29-44`):

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quota {
    pub in_file: usize,
    pub function_body: usize,
    pub cross_file_first: usize,
}

/// Half `in_file`, the remainder split evenly — 12/6/6 at the default 24.
///
/// Rounding: `in_file = ceil(total/2)`, then `function_body = ceil(rest/2)`
/// and `cross_file_first` takes what is left. Every odd task therefore goes
/// to the earlier lane, and the lane most likely to come up short never
/// holds a task the repository cannot supply. The three always sum to
/// `total`, so `codebase_tasks` still means what it says.
#[must_use]
pub fn quota(total: u32) -> Quota {
    let total = usize::try_from(total).unwrap_or(0);
    let in_file = total.div_ceil(2);
    let rest = total - in_file;
    let function_body = rest.div_ceil(2);
    Quota {
        in_file,
        function_body,
        cross_file_first: rest - function_body,
    }
}
```

Add `Lane` above `TaskSet` and the field to `TaskSet` (`:21-27`):

```rust
/// One tier's accounting for this run: what was asked for, what the repo
/// had, what was taken. The caller builds the cross-file lane's shortfall
/// sentence from this, which needs a reason `sample` does not have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lane {
    pub tier: TaskTier,
    pub picked: usize,
    pub want: usize,
    pub have: usize,
}

#[derive(Debug, Default)]
pub struct TaskSet {
    pub picked: Vec<Picked>,
    /// "`function_body`: 5 of 8 requested (repo has 5 candidates)" — printed,
    /// never filled from another tier. The cross-file lane's own sentence is
    /// added by `codebase::prepare`, which knows why it was short.
    pub shortfall: Vec<String>,
    pub lanes: Vec<Lane>,
}
```

Replace `sample`'s loop (`:85-110`):

```rust
#[must_use]
pub fn sample(mut files: Vec<FileCandidates>, quota: Quota, seed: u64) -> TaskSet {
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let mut rng = Rng::new(seed);
    let mut set = TaskSet::default();
    for (tier, want) in [
        (TaskTier::InFile, quota.in_file),
        (TaskTier::FunctionBody, quota.function_body),
        (TaskTier::CrossFileFirst, quota.cross_file_first),
    ] {
        let mut lanes = per_file_lanes(&files, tier, &mut rng);
        let picked = round_robin(&mut lanes, want);
        let have: usize = files
            .iter()
            .map(|f| f.candidates.iter().filter(|c| c.tier == tier).count())
            .sum();
        if picked.len() < want && tier != TaskTier::CrossFileFirst {
            set.shortfall.push(format!(
                "{}: {} of {want} requested (repo has {have} candidates)",
                tier.label(),
                picked.len()
            ));
        }
        set.lanes.push(Lane {
            tier,
            picked: picked.len(),
            want,
            have,
        });
        set.picked.extend(picked);
    }
    set
}
```

- [ ] **Step 5: Fill the new fields at their empty values everywhere a literal builds one**

Both structs gained fields, so every exhaustive literal in the crate must name them. There are four:

1. `src/core/bench/codebase/filter.rs` — in `assemble` (`:137-154`), after `excluded` in the returned `CodebaseTask`:

```rust
        also_first_uses: Vec::new(),
        extra: None,
        extra_text: String::new(),
```

and inside its `Excluded { … }`, after `cfg_test_lines`:

```rust
            cross_file_withheld: 0,
```

2. `src/core/bench/codebase/ladder.rs` — the tests' `task(tier, gold)` helper (`:660`) builds a `CodebaseTask` and an `Excluded`: add the same four lines.
3. `src/core/bench/store.rs` — the tests' `codebase_task(fixture)` (`:1304-1329`) builds an `Excluded` inside its `CodebaseRow`: add `cross_file_withheld: 0,` after `cfg_test_lines: 0,`.
4. `src/commands/capability.rs` — `codebase_task_fixture` (`:2308-2323`) builds a `CodebaseTask` and an `Excluded`: add the same four lines.

- [ ] **Step 6: Run the tests**

Run: `cargo test --lib codebase 2>&1 | tail -20`
Expected: `test result: ok.` — every `codebase::*` test passes, including the two new sample tests.

Run: `cargo test 2>&1 | grep -E "test result|FAILED"`
Expected: all `ok`. (`the_same_head_yields_the_same_set_and_a_different_head_does_not` still picks 24 because `files` now offers all three tiers.)

- [ ] **Step 7: Commit**

```bash
git add src/core/bench/codebase/mod.rs src/core/bench/codebase/sample.rs \
        src/core/bench/codebase/filter.rs src/commands/capability.rs && \
git commit -m "$(cat <<'EOF'
feat(codebase): the cross_file_first tier and its 12/6/6 quota lane

The tier, its label and its id shape; ExtraFile and the task fields the
extra arm will fill; Excluded.cross_file_withheld. quota() splits half /
quarter / quarter instead of two-thirds / one-third, and TaskSet carries a
Lane per tier so the caller can say WHY the cross lane came up short.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01YDaTTHJySW2ix3P85BcP6W
EOF
)"
```

---

### Task 2: `crossfile.rs` — the declaration index and first-use detection

**Files:**
- Create: `src/core/bench/codebase/crossfile.rs`
- Modify: `src/core/bench/codebase/mod.rs:4-8` (add `pub mod crossfile;` in alphabetical order, before `pub mod filter;`)
- Modify: `src/core/bench/codebase/ladder.rs:448-463` (split `collect_declarations` so the declaration-only half is reusable)

**Interfaces:**
- Consumes (Task 1): `TaskTier::CrossFileFirst`.
- Consumes (existing): `masker::Candidate { tier, byte_range, line, doc_comment }`, `masker::literal_ranges(&str) -> Vec<Range<usize>>` (`pub(crate)`), `ladder::PRELUDE: &[&str]`, `ladder::KEYWORDS: [&str; 52]`.
- Produces:
  ```rust
  // codebase/crossfile.rs
  pub struct Index {
      /// name -> the files that declare it. A name with 2+ entries is
      /// ambiguous and never a candidate.
      declared: std::collections::BTreeMap<String, std::collections::BTreeSet<String>>,
  }
  impl Index {
      pub fn build(files: &[(String, String)]) -> Self;
      pub fn defined_in(&self, name: &str, not_in: &str) -> Defined;
      pub fn ambiguous_among(&self, names: &BTreeSet<String>) -> BTreeSet<String>;
  }
  pub enum Defined { Nowhere, Ambiguous, In(String) }

  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct Meta { pub name: String, pub defined_in: String, pub also_first_uses: Vec<String> }

  #[derive(Debug, Default)]
  pub struct Found { pub candidates: Vec<masker::Candidate>, pub meta: Vec<(usize, Meta)> }

  /// Cross-file first-use candidates in ONE file. `meta` is keyed by the
  /// candidate's `byte_range.start`, which is unique within a file.
  pub fn first_uses(index: &Index, file: &FileText) -> Found;

  pub struct FileText<'a> { pub path: &'a str, pub text: &'a str, pub spans: &'a [masker::Candidate] }
  ```
- Produces (`ladder.rs`): `pub(super) fn declaration_names(line: &str, set: &mut BTreeSet<String>)`.

**Spec ambiguity resolved:** §3.1 says the index comes from `ladder::collect_declarations` but that struct fields and enum variants are *not* indexed — and `collect_declarations` today also collects members. The declaration-keyword half is therefore split out as `declaration_names`, and `collect_declarations` keeps calling both, so tier 5 is unchanged and the index is declarations only.

- [ ] **Step 1: Write the failing tests**

Create `src/core/bench/codebase/crossfile.rs` with only its tests module for now (the implementation lands in Steps 3–5):

```rust
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
        assert_eq!(uses(&idx, "src/user.rs", user), vec![(2, "build".to_owned())]);
    }

    #[test]
    fn every_call_shape_counts_and_a_use_line_never_does() {
        let idx = index(&[("src/defs.rs", DEFS)]);
        for (src, want) in [
            ("pub fn r() {\n    let w = build(1);\n    w\n}\n", "build"),
            ("pub fn r() {\n    let w = paint::go();\n    w\n}\n", "paint"),
            ("pub fn r() {\n    let w = Widget { id: 1 };\n    w\n}\n", "Widget"),
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
        assert!(uses(&two, "src/c.rs", user).is_empty(), "ambiguous is skipped");

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
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --lib codebase::crossfile 2>&1 | tail -20`
Expected: `error[E0432]: unresolved import` / `cannot find function 'first_uses'` — the module has no implementation and is not declared.

- [ ] **Step 3: Make the declaration half of the ladder reusable**

`src/core/bench/codebase/ladder.rs` — replace `collect_declarations` (`:448-463`) with two functions:

```rust
fn collect_declarations(line: &str, set: &mut BTreeSet<String>) {
    declaration_names(line, set);
    collect_members(line.trim(), set);
}

/// The `fn`/`struct`/`enum`/`trait`/`type`/`const`/`static`/`mod` name on
/// this line, if any.
///
/// Split from `collect_declarations` because the cross-file index wants
/// declarations only: a struct field and an enum variant are not call sites,
/// so indexing them would make every `name:` in the repository look like a
/// definition to cross a file for.
pub(super) fn declaration_names(line: &str, set: &mut BTreeSet<String>) {
    let words: Vec<&str> = line
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty())
        .collect();
    for (i, w) in words.iter().enumerate() {
        if matches!(
            *w,
            "fn" | "struct" | "enum" | "trait" | "type" | "const" | "static" | "mod"
        ) && let Some(name) = words.get(i + 1)
        {
            set.insert((*name).to_owned());
        }
    }
}
```

Also widen two visibilities the new module needs — on `:62` change `const PRELUDE` to `pub(super) const PRELUDE`, on `:52` change `const KEYWORDS` to `pub(super) const KEYWORDS`.

- [ ] **Step 4: Write the index**

At the top of `src/core/bench/codebase/crossfile.rs`, above the tests module:

```rust
//! Cross-file first-use tasks (slice B1 §3).
//!
//! One index of every declaration in the repository, then per file the first
//! call-shaped use of a name declared in exactly one OTHER file. That span is
//! the mask: a model that has not read the other file cannot recover the
//! signature, and one that has can.

use std::collections::{BTreeMap, BTreeSet};

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
            let mut names = BTreeSet::new();
            for line in text.lines() {
                ladder::declaration_names(line, &mut names);
            }
            for name in names {
                if ladder::KEYWORDS.contains(&name.as_str())
                    || ladder::PRELUDE.contains(&name.as_str())
                {
                    continue;
                }
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
```

- [ ] **Step 5: Write first-use detection**

Append to `crossfile.rs` (still above the tests module):

```rust
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

/// Every cross-file first use in one file, in span order.
///
/// A span may first-use several names; it yields ONE task, keyed on the name
/// whose use appears earliest in the span, the others recorded as
/// `also_first_uses`. One candidate per (file, name) follows from the same
/// rule: a name's first use is in exactly one span.
#[must_use]
pub fn first_uses(index: &Index, file: &FileText) -> Found {
    let literals = masker::literal_ranges(file.text);
    let mut found = Found::default();
    let mut claimed: BTreeSet<String> = BTreeSet::new();
    for span in file.spans.iter().filter(|c| c.tier == TaskTier::InFile) {
        let names = span_first_uses(index, file, span, &literals);
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
fn span_first_uses(
    index: &Index,
    file: &FileText,
    span: &Candidate,
    literals: &[std::ops::Range<usize>],
) -> Vec<(String, String)> {
    let mut hits: Vec<(usize, String, String)> = Vec::new();
    for name in ladder::identifiers(&file.text[span.byte_range.clone()]) {
        let Defined::In(other) = index.defined_in(&name, file.path) else {
            continue;
        };
        let Some(at) = first_use_at(file.text, &name, literals) else {
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
fn first_use_at(
    text: &str,
    name: &str,
    literals: &[std::ops::Range<usize>],
) -> Option<usize> {
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
    text[start..at].trim_start().starts_with("use ")
        || text[start..at].trim_start().starts_with("pub use ")
}
```

Declare the module in `src/core/bench/codebase/mod.rs` (`:4`):

```rust
pub mod crossfile;
pub mod filter;
```

- [ ] **Step 6: Run the tests**

Run: `cargo test --lib codebase::crossfile 2>&1 | tail -20`
Expected: `test result: ok. 5 passed`.

Run: `cargo test --lib codebase::ladder 2>&1 | tail -5`
Expected: `test result: ok.` — splitting `collect_declarations` changed nothing tier 5 can see.

- [ ] **Step 7: Commit**

```bash
git add src/core/bench/codebase/crossfile.rs src/core/bench/codebase/mod.rs \
        src/core/bench/codebase/ladder.rs && \
git commit -m "$(cat <<'EOF'
feat(codebase): the declaration index and cross-file first-use detection

Index every fn/struct/enum/trait/type/const/static/mod name to the file
that declares it, minus keywords and the prelude. In a file, the first
call-shaped use (name(, name::, .name(, name {) of a name declared in
exactly one OTHER file is the mask — literal-aware, never a use line,
never an ambiguous or locally shadowed name. A span that first-uses
several names is one task keyed on the earliest, the rest recorded.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01YDaTTHJySW2ix3P85BcP6W
EOF
)"
```

---

### Task 3: `crossfile.rs` — the extra file, the 32 KiB window, and rule (b)

**Files:**
- Modify: `src/core/bench/codebase/crossfile.rs` (append to the implementation and the tests module)
- Modify: `src/core/bench/codebase/ladder.rs:300-302` (`normalise` becomes `pub(super)`)

**Interfaces:**
- Consumes (Task 1): `ExtraFile { path, bytes, truncated }`.
- Consumes (Task 2): `Meta { name, defined_in, also_first_uses }`, `Index`.
- Produces:
  ```rust
  // codebase/crossfile.rs
  pub const EXTRA_CAP: usize = 32 * 1024;

  /// Everything a cross-file task needs beyond its span.
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct Assembled {
      pub extra: ExtraFile,
      pub extra_text: String,
      pub also_first_uses: Vec<String>,
      pub withheld: u32,
  }

  pub struct Corpus<'a> { pub files: &'a [(String, String)], pub normalised: &'a [(String, String)] }
  pub fn normalised_corpus(files: &[(String, String)]) -> Vec<(String, String)>;
  pub fn assemble_extra(meta: &Meta, gold: &str, corpus: &Corpus) -> Option<Assembled>;
  pub fn cross_file_note(a: &Assembled) -> String;
  ```
- Produces (`ladder.rs`): `pub(super) fn normalise(s: &str) -> String`.

- [ ] **Step 1: Write the failing tests**

Append to the tests module in `src/core/bench/codebase/crossfile.rs`:

```rust
    use super::{Assembled, Corpus, Meta, assemble_extra, cross_file_note, normalised_corpus};

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
            format!("sent src/defs.rs ({:.1} KiB); withheld 0 (contain the answer)", DEFS.len() as f64 / 1024.0)
        );
    }

    #[test]
    fn an_over_cap_file_is_windowed_on_the_declaration_line() {
        let filler = "// padding padding padding padding padding padding padding\n".repeat(1200);
        let big = format!("{filler}pub fn build(n: u32) -> u32 {{\n    n + 1\n}}\n{filler}");
        assert!(big.len() > super::EXTRA_CAP * 2, "the fixture must exceed the cap");
        let a = assembled(&[("src/defs.rs", &big)], "let w = build(1);");
        assert!(a.extra.truncated);
        assert_eq!(a.extra.bytes, a.extra_text.len() as u64);
        assert!(a.extra_text.len() <= super::EXTRA_CAP, "{}", a.extra_text.len());
        assert!(
            a.extra_text.contains("pub fn build(n: u32) -> u32 {"),
            "the window is centred on the declaration"
        );
        assert!(a.extra_text.starts_with("// padding"), "snapped to a line start");
        assert!(a.extra_text.ends_with('\n'), "snapped to a line end");
        assert!(
            cross_file_note(&a).contains(", truncated)"),
            "{}",
            cross_file_note(&a)
        );
    }

    #[test]
    fn rule_b_withholds_a_verbatim_answer_elsewhere_and_never_the_defining_file() {
        let gold = "let w = build(1);";
        let a = assembled(
            &[
                ("src/defs.rs", DEFS),
                ("src/copy.rs", "pub fn other() {\n    let  w  =  build(1);\n}\n"),
                ("src/unrelated.rs", "pub fn nothing() {}\n"),
            ],
            gold,
        );
        assert_eq!(a.withheld, 1, "whitespace differences do not hide a copy");
        assert!(cross_file_note(&a).contains("withheld 1 (contain the answer)"));

        let in_g = format!("{DEFS}pub fn again() {{\n    let w = build(1);\n}}\n");
        let b = assembled(&[("src/defs.rs", &in_g)], gold);
        assert_eq!(b.withheld, 0, "G is the definition and is never withheld");
    }

    #[test]
    fn also_first_uses_ride_along() {
        let a = assembled(&[("src/defs.rs", DEFS)], "let w = build(1);");
        assert_eq!(a.also_first_uses, vec!["Widget".to_owned()]);
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --lib codebase::crossfile 2>&1 | tail -20`
Expected: `cannot find function 'assemble_extra' in this scope` (and `normalised_corpus`, `cross_file_note`, `EXTRA_CAP`).

- [ ] **Step 3: Widen `normalise`**

`src/core/bench/codebase/ladder.rs:300` — change

```rust
fn normalise(s: &str) -> String {
```

to

```rust
/// Runs of whitespace collapsed to one space, trimmed. `pub(super)` because
/// rule (b) asks whether another file contains the gold's text, and "the
/// same code, differently indented" is the same answer.
pub(super) fn normalise(s: &str) -> String {
```

- [ ] **Step 4: Implement the extra file and rule (b)**

Append to `src/core/bench/codebase/crossfile.rs`, above the tests module:

```rust
use super::ExtraFile;

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
pub fn assemble_extra(meta: &Meta, gold: &str, corpus: &Corpus) -> Option<Assembled> {
    let (_, text) = corpus
        .files
        .iter()
        .find(|(p, _)| *p == meta.defined_in)?;
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
/// declaration line, snapped outward to line boundaries.
fn window(text: &str, name: &str) -> String {
    if text.len() <= EXTRA_CAP {
        return text.to_owned();
    }
    let at = declaration_offset(text, name).unwrap_or(text.len() / 2);
    let half = EXTRA_CAP / 2;
    let raw_start = at.saturating_sub(half);
    let start = text[..raw_start].rfind('\n').map_or(0, |i| i + 1);
    let raw_end = (start + EXTRA_CAP).min(text.len());
    let end = text[..raw_end].rfind('\n').map_or(raw_end, |i| i + 1);
    text[start..end.max(start)].to_owned()
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
fn withheld_count(gold: &str, defining: &str, corpus: &Corpus) -> u32 {
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
    format!(
        "sent {} ({:.1} KiB{truncated}); withheld {} (contain the answer)",
        a.extra.path,
        a.extra.bytes as f64 / 1024.0,
        a.withheld
    )
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test --lib codebase::crossfile 2>&1 | tail -20`
Expected: `test result: ok. 9 passed`.

- [ ] **Step 6: Run the whole gate**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings 2>&1 | tail -20`
Expected: `Finished` with no warnings. If `cast_precision_loss` fires on `a.extra.bytes as f64`, use `crate::core::bench::codebase::ladder::as_f64` on a `usize` instead of casting the `u64`.

- [ ] **Step 7: Commit**

```bash
git add src/core/bench/codebase/crossfile.rs src/core/bench/codebase/ladder.rs && \
git commit -m "$(cat <<'EOF'
feat(codebase): the extra file, its 32 KiB window, and rule (b)

The defining file is sent whole under 32 KiB and otherwise as the 32 KiB
window centred on the declaration line, snapped to line boundaries, with
truncated and the exact bytes recorded. Rule (b) counts every OTHER file
whose whitespace-normalised text contains the gold — never the defining
file, without which the tier is unanswerable, and never the task's own.
excluded.cross_file stops saying "n/a: same-file" and says what was sent.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01YDaTTHJySW2ix3P85BcP6W
EOF
)"
```

---

### Task 4: `prepare` wiring — the index, the third lane, and the shortfall reason

**Files:**
- Modify: `src/core/bench/codebase/mod.rs:60-199` (`Prepared`, `prepare`, `file_candidates`, `assembled_tasks`), tests at `:201-270`
- Modify: `src/core/bench/codebase/filter.rs:131-171` (`assemble` takes a `Context` bundle)

**Interfaces:**
- Consumes (Task 1): `TaskTier::CrossFileFirst`, `Quota.cross_file_first`, `sample::Lane`, `TaskSet.lanes`, `ExtraFile`, `CodebaseTask.{also_first_uses, extra, extra_text}`, `Excluded.cross_file_withheld`.
- Consumes (Task 2): `crossfile::Index::{build, defined_in, ambiguous_among}`, `crossfile::first_uses`, `crossfile::{FileText, Meta, Found}`.
- Consumes (Task 3): `crossfile::{Assembled, Corpus, normalised_corpus, assemble_extra, cross_file_note}`.
- Produces:
  ```rust
  // codebase/mod.rs
  #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
  pub struct Counts { pub in_file: usize, pub function_body: usize, pub cross_file_first: usize }

  pub struct Prepared {
      pub head: String,
      pub set_hash: String,
      pub tasks: Vec<CodebaseTask>,
      pub shortfall: Vec<String>,
      pub symbols: ladder::Symbols,
      pub cfg_test_lines: usize,
      pub cfg_test_files: usize,
      pub counts: Counts,               // NEW — the dry-run line's tier counts
  }
  pub fn prepare(repo: &Path, scratch_root: &Path, tasks: u32) -> Result<Prepared, ChekovError>;

  // codebase/filter.rs
  pub struct Context<'a> {
      pub text: &'a str,
      pub cfg_test_lines: usize,
      pub cross: Option<&'a crossfile::Assembled>,
  }
  pub fn assemble(picked: &Picked, ctx: &Context) -> CodebaseTask;   // signature CHANGED
  ```

- [ ] **Step 1: Write the failing tests**

In `src/core/bench/codebase/mod.rs`'s tests module, add a second fixture repo and two tests after `prepare_keeps_files_with_inline_tests_and_cuts_the_tests_out_of_them` (`:250-269`):

```rust
    /// Two files: one defines, the other calls into it — the shape the
    /// cross-file tier exists for.
    fn repo_with_a_cross_file_call() -> PathBuf {
        let dir = std::env::temp_dir()
            .join("chekov-test-codebase-prepare")
            .join("cross");
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
            "pub fn run(n: u32) -> u32 {\n    let a = build(n);\n    let b = a + 1;\n    \
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

    #[test]
    fn a_cross_file_task_carries_the_defining_file_and_the_others_carry_none() {
        use crate::core::bench::codebase::TaskTier;
        let prepared = prepare(
            &repo_with_a_cross_file_call(),
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
        assert!(task.extra_text.contains("pub fn build"), "{}", task.extra_text);
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
        for other in prepared
            .tasks
            .iter()
            .filter(|t| t.tier != TaskTier::CrossFileFirst)
        {
            assert_eq!(
                other.excluded.cross_file,
                crate::core::bench::codebase::filter::NO_CROSS_FILE
            );
            assert!(other.extra.is_none(), "{}", other.id);
            assert!(other.extra_text.is_empty(), "{}", other.id);
        }
    }

    #[test]
    fn a_short_cross_lane_says_how_many_names_were_ambiguous_and_how_many_files_had_no_use() {
        let prepared = prepare(
            &repo_with_inline_tests(),
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
             3 files have no cross-file use)",
            "{:?}",
            prepared.shortfall
        );
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --lib codebase::tests 2>&1 | tail -25`
Expected: `no field 'counts' on type 'Prepared'`, and both new tests fail — `prepared.tasks` has no `CrossFileFirst` entries because nothing produces them yet.

- [ ] **Step 3: Rework `filter::assemble` onto a context bundle**

`src/core/bench/codebase/filter.rs` — replace `assemble` (`:131-154`) with:

```rust
/// What `assemble` reads besides the picked span (§4 — three parameters).
pub struct Context<'a> {
    /// The file's already-elided text.
    pub text: &'a str,
    /// What `elide_cfg_test` removed from THAT file, carried onto the row so
    /// the report can say what the repository's inline tests cost.
    pub cfg_test_lines: usize,
    /// The cross-file assembly, for a `cross_file_first` span only.
    pub cross: Option<&'a super::crossfile::Assembled>,
}

/// One task from its own file's already-elided text, plus the other file a
/// cross-file task was given.
#[must_use]
pub fn assemble(picked: &Picked, ctx: &Context) -> CodebaseTask {
    let c = &picked.candidate;
    let (prefix, doc_comment) = prefix_and_doc_flag(ctx.text, c);
    CodebaseTask {
        id: picked.id.clone(),
        tier: c.tier,
        file: picked.path.clone(),
        line: c.line,
        gold: ctx.text[c.byte_range.clone()].to_owned(),
        prefix,
        suffix: ctx.text[c.byte_range.end..].to_owned(),
        excluded: Excluded {
            doc_comment,
            cross_file: ctx
                .cross
                .map_or_else(|| NO_CROSS_FILE.to_owned(), super::crossfile::cross_file_note),
            cfg_test_lines: ctx.cfg_test_lines,
            cross_file_withheld: ctx.cross.map_or(0, |a| a.withheld),
        },
        also_first_uses: ctx
            .cross
            .map_or_else(Vec::new, |a| a.also_first_uses.clone()),
        extra: ctx.cross.map(|a| a.extra.clone()),
        extra_text: ctx.cross.map_or_else(String::new, |a| a.extra_text.clone()),
    }
}
```

In `filter.rs`'s own tests, the `pick_from`/`pick` helpers (`:182-198`) call `assemble(text, &picked, 0)` — change every call site to `assemble(&picked, &Context { text, cfg_test_lines: 0, cross: None })` and add `Context` to the tests module's `use super::{…}` line.

- [ ] **Step 4: Wire `prepare`**

`src/core/bench/codebase/mod.rs` — add `pub mod crossfile;` if Task 2 has not already, then replace `Prepared` (`:64-76`), `assembled_tasks` (`:119-136`), `prepare` (`:138-178`) and `file_candidates` (`:180-190`) with:

```rust
/// Tasks actually picked per tier — what the dry-run line and the report
/// header count.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub in_file: usize,
    pub function_body: usize,
    pub cross_file_first: usize,
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

/// Every file's candidates — the masker's spans plus the cross-file first
/// uses — with the metas and the two numbers the shortfall sentence needs.
struct Candidates {
    per_file: Vec<sample::FileCandidates>,
    /// `(file, span start)` is unique within a run.
    meta: std::collections::HashMap<(String, usize), crossfile::Meta>,
    ambiguous: std::collections::BTreeSet<String>,
    files_without_use: usize,
}

fn all_candidates(index: &crossfile::Index, elided: &Elisions) -> Candidates {
    use masker::MaskSource;
    let mut out = Candidates {
        per_file: Vec::new(),
        meta: std::collections::HashMap::new(),
        ambiguous: std::collections::BTreeSet::new(),
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
            .extend(index.ambiguous_among(&span_identifiers(text, &candidates)));
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

/// Every identifier appearing in this file's `in_file` spans — the names the
/// cross-file rule had an opinion about.
fn span_identifiers(
    text: &str,
    candidates: &[masker::Candidate],
) -> std::collections::BTreeSet<String> {
    candidates
        .iter()
        .filter(|c| c.tier == TaskTier::InFile)
        .flat_map(|c| ladder::identifiers(&text[c.byte_range.clone()]))
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
/// HEAD it checks out, so two runs of different commits never share one.
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

/// `cross_file_first: 4 of 6 (2 short: 17 ambiguous names skipped, 9 files
/// have no cross-file use)` — the lane's own reason, which `sample` cannot
/// know. `None` when the lane was filled.
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
         {} files have no cross-file use)",
        lane.picked,
        lane.want,
        lane.want - lane.picked,
        candidates.ambiguous.len(),
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
                    cfg_test_lines: a.elided.per_file.get(&p.path).copied().unwrap_or(0),
                    cross: cross_for(p, a, text).as_ref(),
                },
            ))
        })
        .collect()
}

/// One picked span's cross-file assembly, or `None` for the other tiers.
fn cross_for(
    p: &sample::Picked,
    a: &Assembly,
    text: &str,
) -> Option<crossfile::Assembled> {
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
```

Delete the now-unused `file_candidates`.

- [ ] **Step 5: Run the tests**

Run: `cargo test --lib codebase 2>&1 | tail -20`
Expected: `test result: ok.` — including both new `mod.rs` tests and the reworked `filter.rs` tests.

- [ ] **Step 6: Run the whole gate**

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings 2>&1 | tail -20 && cargo test 2>&1 | grep -E "test result|FAILED"`
Expected: no clippy warnings; every `test result: ok.`

- [ ] **Step 7: Commit**

```bash
git add src/core/bench/codebase/mod.rs src/core/bench/codebase/filter.rs && \
git commit -m "$(cat <<'EOF'
feat(codebase): prepare builds the index and samples the third lane

The declaration index is built from the elided texts, cross-file first uses
join the masker's candidates in the same per-file lanes, and one sample call
strata all three tiers. A cross-file task is assembled with its defining
file, its also_first_uses and rule (b)'s count; every other tier still
records "n/a: same-file". A short cross lane says why: how many names were
ambiguous and how many files had no cross-file use at all.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01YDaTTHJySW2ix3P85BcP6W
EOF
)"
```

---

### Task 5: `runner.rs` — `ExtraChunk` and `input_extra` on the wire

**Files:**
- Modify: `src/core/bench/runner.rs:478-545` (`InfillTask`, `infill_body`), tests at `:894-995`
- Modify: `src/commands/capability.rs:1502-1506` (the one `InfillTask` construction — `extra: None` until Task 6 moves it)

**Interfaces:**
- Consumes: nothing from earlier tasks (this task is independent of Tasks 1–4).
- Produces:
  ```rust
  // core/bench/runner.rs
  /// One extra file on the wire, in llama.cpp's `input_extra` shape.
  pub struct ExtraChunk<'a> { pub filename: &'a str, pub text: &'a str }

  pub struct InfillTask<'a> {
      pub prefix: &'a str,
      pub suffix: &'a str,
      pub gold_lines: usize,
      pub extra: Option<ExtraChunk<'a>>,
  }
  ```
  `cross_infill` and `InfillOutcome` are unchanged.

- [ ] **Step 1: Write the failing test**

In `src/core/bench/runner.rs`'s tests module, after `an_infill_crossing_posts_prefix_suffix_and_pins_and_returns_the_raw_fill` (`:894-933`):

```rust
    #[test]
    fn an_extra_chunk_goes_up_as_input_extra_in_llama_cpps_shape() {
        let http = CannedUpstream::new(
            serde_json::json!({
                "content": "    Widget { id: 1 }\n",
                "tokens_predicted": 6,
                "timings": final_frame()["timings"]
            })
            .to_string(),
        );
        let facade = ClaudeFacade::new("local-model");
        let up = fake_upstream();
        let task = super::InfillTask {
            prefix: "fn f() {\n",
            suffix: "\n}\n",
            gold_lines: 1,
            extra: Some(super::ExtraChunk {
                filename: "src/defs.rs",
                text: "pub struct Widget { pub id: u32 }\n",
            }),
        };
        super::cross_infill(&wire(&http, &facade, &up), &task).expect("crosses");
        let sent = sent(&http);
        assert_eq!(sent["input_extra"].as_array().map(Vec::len), Some(1), "{sent}");
        assert_eq!(sent["input_extra"][0]["filename"], "src/defs.rs");
        assert_eq!(
            sent["input_extra"][0]["text"],
            "pub struct Widget { pub id: u32 }\n"
        );
        assert_eq!(sent["input_prefix"], "fn f() {\n", "nothing else moved");
        assert_eq!(sent["n_predict"], 64);
        assert_eq!(sent["temperature"], 0);
        assert_eq!(sent["top_k"], 1);
        assert_eq!(sent["seed"], 42);
    }
```

The empty case is already covered: `an_infill_crossing_posts_prefix_suffix_and_pins_and_returns_the_raw_fill` asserts `sent["input_extra"] == serde_json::json!([])`, and gains `extra: None` in Step 3.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --lib runner::tests::an_extra_chunk 2>&1 | tail -15`
Expected: `cannot find struct 'ExtraChunk'` and `struct 'InfillTask' has no field named 'extra'`.

- [ ] **Step 3: Implement**

`src/core/bench/runner.rs` — replace `InfillTask` (`:478-484`):

```rust
/// One extra file the model is shown beside the masked one, in llama.cpp's
/// `input_extra` shape. The engine keeps the TAIL of the extra tokens when
/// they exceed `n_ctx − n_batch − 2·n_predict`; one file under 32 KiB at
/// ctx ≥ 32K is never trimmed.
pub struct ExtraChunk<'a> {
    pub filename: &'a str,
    pub text: &'a str,
}

/// One infill task on the wire: the file before and after the mask, the
/// gold's line count (to bound `n_predict`), and the other file when this
/// arm sends one.
pub struct InfillTask<'a> {
    pub prefix: &'a str,
    pub suffix: &'a str,
    pub gold_lines: usize,
    pub extra: Option<ExtraChunk<'a>>,
}
```

and `infill_body` (`:533-545`):

```rust
/// The `/infill` request body: prefix/suffix, no chat prompt, the pins, the
/// extra files (one or none), and an `n_predict` bounded by the gold's size
/// (three tokens per twelve characters of line, floored at 64 so a one-liner
/// still gets room).
fn infill_body(task: &InfillTask, seed: u32) -> Value {
    let n_predict = (task.gold_lines * 36).max(64);
    let input_extra = task.extra.as_ref().map_or_else(
        || serde_json::json!([]),
        |e| serde_json::json!([{ "filename": e.filename, "text": e.text }]),
    );
    serde_json::json!({
        "input_prefix": task.prefix,
        "input_suffix": task.suffix,
        "prompt": "",
        "input_extra": input_extra,
        "n_predict": n_predict,
        "temperature": 0,
        "top_k": 1,
        "seed": seed,
    })
}
```

Add `extra: None` to the three existing `InfillTask` literals in `runner.rs`'s tests (`:906-910`, `:942-946`, and the one in `a_model_without_fim_tokens_is_a_capability_not_a_failure` at `:979`), and to the one in `capability.rs::infill_or_latch` (`:1502-1506`).

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib runner 2>&1 | tail -10`
Expected: `test result: ok.`

- [ ] **Step 5: Commit**

```bash
git add src/core/bench/runner.rs src/commands/capability.rs && \
git commit -m "$(cat <<'EOF'
feat(bench): InfillTask carries an optional extra file

ExtraChunk { filename, text } serialises as llama.cpp's input_extra —
one object with the chunk, [] without. Nothing else on the wire moves:
same pins, same n_predict, same prefix and suffix.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01YDaTTHJySW2ix3P85BcP6W
EOF
)"
```

---

### Task 6: `codebase/run.rs` — the move, then the two arms

This task is **two commits**: a pure move with no behaviour change, then the arms. Do not combine them — a reviewer must be able to see that the move changed nothing.

**Files:**
- Create: `src/core/bench/codebase/run.rs`
- Modify: `src/core/bench/codebase/mod.rs` (add `pub mod run;`)
- Modify: `src/commands/capability.rs` (delete the moved cluster, call the new module, keep `TaskSink`)
- Modify: `src/core/bench/codebase/ladder.rs:234-298` (`Scored.extra`, `score_all`'s context)

**Interfaces:**
- Consumes (Task 1): `TaskTier::CrossFileFirst`, `CodebaseTask.{extra, extra_text, also_first_uses}`.
- Consumes (Task 4): `Prepared.{tasks, symbols}`.
- Consumes (Task 5): `runner::{ExtraChunk, InfillTask}`.
- Consumes (Task 7 — write it in this task, Task 7 renders it): `store::CodebaseRow.{arm, extra, also_first_uses}`.
- Produces:
  ```rust
  // core/bench/codebase/run.rs
  pub const NO_EXTRA: &str = "no_extra";
  pub const WITH_EXTRA: &str = "extra";

  /// Where rows land and what a resumed run already holds.
  pub struct Sink<'a> {
      pub writer: &'a mut crate::core::bench::store::RunWriter,
      pub done: &'a [(String, String, crate::core::bench::store::Transport)],
  }

  pub fn run_codebase(
      sink: &mut Sink,
      wire: &crate::core::bench::runner::ProbeWire,
      prepared: &super::Prepared,
  ) -> Result<(), ChekovError>;

  pub(crate) const fn empty_measure() -> crate::core::bench::store::Measure;
  pub(crate) fn probe_measure(t: &crate::core::bench::runner::Timings)
      -> crate::core::bench::store::Measure;
  ```
- Produces (`ladder.rs`):
  ```rust
  pub struct Scored<'a> {
      pub task: &'a CodebaseTask,
      pub prediction: &'a str,
      pub symbols: &'a Symbols,
      /// The extra file's text on the with-extra arm, "" otherwise — the
      /// model was shown G, so G's names exist for it (§6).
      pub extra: &'a str,
  }
  ```

#### Commit A — the pure move

- [ ] **Step 1: Create `run.rs` with the cluster, unchanged**

Create `src/core/bench/codebase/run.rs` and move these items verbatim out of `src/commands/capability.rs`, changing only their paths and visibility: `empty_measure` (`:1474-1482`), `probe_measure` (`:1693-1703`), `infill_or_latch` (`:1493-1525`), `Unavailable` + its two constructors (`:1534-1555`), `Recorded` (`:1559-1562`), `symbols_tier_score` (`:1566-1582`), `record_codebase_task` (`:1588-1620`), `RowParts` + `row_parts` (`:1623-1645`), `run_codebase` (`:1651-1673`). Head the file with:

```rust
//! The `--codebase` run loop: every sampled task through `/infill`, recorded
//! with its raw prediction.
//!
//! It lives beside the task generation rather than in the command layer: the
//! command owns the run directory and the lifecycle, this module owns what a
//! codebase task IS on the wire (spec §10 — the slice-A "run cluster lives in
//! the command layer" item, retired).

use crate::core::bench::codebase::{CodebaseTask, MASK_LABEL, ladder};
use crate::core::bench::runner::{self, ProbeArtifact};
use crate::core::bench::store::{self, TaskKey};
use crate::error::ChekovError;

/// Where rows land and what a resumed run already holds. The command layer
/// owns the writer; this module owns the loop.
pub struct Sink<'a> {
    pub writer: &'a mut store::RunWriter,
    pub done: &'a [(String, String, store::Transport)],
}

impl Sink<'_> {
    /// The `--resume` skip test: the same task through the same door.
    fn is_done(&self, key: &TaskKey) -> bool {
        self.done.iter().any(|(suite, task_id, transport)| {
            suite == key.suite && task_id == key.task_id && *transport == key.transport
        })
    }
}
```

Every moved function takes `&mut Sink` where it took `&mut TaskSink`; `empty_measure` and `probe_measure` become `pub(crate)`; `run_codebase` becomes `pub`. Move the tests too: `ScriptedInfill`, `scratch`, `run_head`, `codebase_task_fixture`, `prepared_pair`, `infill_200`, `drive_codebase`, `refused`, `unavailable_reason`, `a_model_without_infill_records_every_task_unavailable_and_asks_only_once`, `a_task_that_failed_for_another_reason_is_unavailable_alone` (`capability.rs:2240-2454`) into a `#[cfg(test)] mod tests` in `run.rs`, with `super::Sink` in place of `super::TaskSink`.

- [ ] **Step 2: Point the command layer at it**

`src/core/bench/codebase/mod.rs` — add `pub mod run;` after `pub mod masker;`.

`src/commands/capability.rs` — in `run_suites` (`:1119-1121`):

```rust
    if let Some(prepared) = inputs.prepared {
        crate::core::bench::codebase::run::run_codebase(
            &mut crate::core::bench::codebase::run::Sink {
                writer: sink.writer,
                done: sink.done,
            },
            &wire,
            prepared,
        )?;
    }
```

Replace the two remaining `empty_measure()` call sites (`failed_probe` at `:1688`, and `append_unavailable`) and the `probe_measure` call site in `append_probe` with `crate::core::bench::codebase::run::empty_measure()` / `::probe_measure(…)`, and delete the local definitions.

- [ ] **Step 3: Verify the move changed nothing**

Run: `cargo test 2>&1 | grep -E "test result|FAILED"`
Expected: every `test result: ok.` — the same test count as before the move (the two run-path tests now report under `core::bench::codebase::run::tests`).

Run: `cargo clippy --all-targets -- -D warnings 2>&1 | tail -10`
Expected: no warnings.

- [ ] **Step 4: Commit the move**

```bash
git add src/core/bench/codebase/run.rs src/core/bench/codebase/mod.rs \
        src/commands/capability.rs && \
git commit -m "$(cat <<'EOF'
refactor(codebase): move the run loop out of the command layer

run_codebase, infill_or_latch, Unavailable, record_codebase_task, Recorded,
symbols_tier_score, row_parts and the two measure constructors move from
commands/capability.rs to core/bench/codebase/run.rs unchanged, with their
tests. The command layer keeps the run directory and the lifecycle and hands
this module a Sink. No behaviour change.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01YDaTTHJySW2ix3P85BcP6W
EOF
)"
```

#### Commit B — the two arms

- [ ] **Step 5: Write the failing tests**

In `run.rs`'s tests module, extend `ScriptedInfill` to keep the bodies it was sent, and add the fixtures and the two tests:

```rust
    struct ScriptedInfill {
        replies: RefCell<Vec<Result<String, ChekovError>>>,
        posts: RefCell<usize>,
        bodies: RefCell<Vec<String>>,
    }

    impl HttpClient for ScriptedInfill {
        fn get(&self, _url: &str) -> Result<String, ChekovError> {
            unreachable!("the codebase run only POSTs")
        }

        fn post_json(&self, req: &JsonRequest) -> Result<String, ChekovError> {
            *self.posts.borrow_mut() += 1;
            self.bodies.borrow_mut().push(req.body.clone());
            let mut replies = self.replies.borrow_mut();
            assert!(!replies.is_empty(), "one POST more than the script allows");
            replies.remove(0)
        }
    }

    /// One cross-file task with a defining file to send on the second arm.
    fn cross_task() -> CodebaseTask {
        CodebaseTask {
            id: "cross_file_first-abc123-L2".into(),
            tier: TaskTier::CrossFileFirst,
            file: "src/user.rs".into(),
            line: 2,
            gold: "let a = build(1);".into(),
            prefix: "pub fn run() {\n".into(),
            suffix: "\n    a\n}\n".into(),
            excluded: Excluded {
                doc_comment: 0,
                cross_file: "sent src/defs.rs (0.1 KiB); withheld 0 (contain the answer)".into(),
                cfg_test_lines: 0,
                cross_file_withheld: 0,
            },
            also_first_uses: vec!["Widget".into()],
            extra: Some(ExtraFile {
                path: "src/defs.rs".into(),
                bytes: 34,
                truncated: false,
            }),
            extra_text: "pub fn build(n: u32) -> u32 { n + 1 }\n".into(),
        }
    }

    fn prepared_cross() -> Prepared {
        Prepared {
            head: "4818813deeaa11112222333344445555666677".into(),
            set_hash: "abcdef123456".into(),
            tasks: vec![cross_task()],
            shortfall: vec![],
            symbols: Symbols::default(),
            cfg_test_lines: 0,
            cfg_test_files: 0,
            counts: Counts {
                in_file: 0,
                function_body: 0,
                cross_file_first: 1,
            },
        }
    }

    /// Drive `run_codebase` over one prepared set with a scripted upstream and
    /// a `--resume` ledger: the rows, the ask count, and the bodies sent.
    fn drive(
        name: &str,
        prepared: &Prepared,
        script: (Vec<Result<String, ChekovError>>, Vec<Done>),
    ) -> (Vec<TaskRow>, usize, Vec<serde_json::Value>) {
        let (replies, done) = script;
        let http = ScriptedInfill {
            replies: RefCell::new(replies),
            posts: RefCell::new(0),
            bodies: RefCell::new(Vec::new()),
        };
        let facade = ClaudeFacade::new("local-model");
        let up = Upstream {
            base_url: "http://fake".into(),
            api_key: "sekrit".into(),
        };
        let wire = runner::ProbeWire {
            http: &http,
            facade: &facade,
            upstream: &up,
            pins: runner::SamplingPins { seed: 42 },
        };
        let mut writer =
            RunWriter::create(&scratch(name), "r-codebase", &run_head()).expect("create");
        {
            let mut sink = super::Sink {
                writer: &mut writer,
                done: &done,
            };
            super::run_codebase(&mut sink, &wire, prepared).expect("the run completes");
        }
        let log = RunLog::load(writer.dir()).expect("load");
        let bodies = http
            .bodies
            .into_inner()
            .iter()
            .map(|b| serde_json::from_str(b).expect("json"))
            .collect();
        (log.rows, http.posts.into_inner(), bodies)
    }

    /// The row ledger `--resume` reads.
    type Done = (String, String, crate::core::bench::store::Transport);

    #[test]
    fn a_cross_file_task_crosses_twice_without_then_with_the_defining_file() {
        let (rows, posts, bodies) = drive(
            "two-arms",
            &prepared_cross(),
            (vec![Ok(infill_200()), Ok(infill_200())], vec![]),
        );
        assert_eq!(posts, 2, "two arms, two crossings");
        assert_eq!(bodies[0]["input_extra"], serde_json::json!([]));
        assert_eq!(bodies[1]["input_extra"][0]["filename"], "src/defs.rs");
        assert_eq!(
            bodies[1]["input_extra"][0]["text"],
            "pub fn build(n: u32) -> u32 { n + 1 }\n"
        );
        assert_eq!(bodies[0]["input_prefix"], bodies[1]["input_prefix"]);

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].task_id, "cross_file_first-abc123-L2");
        assert_eq!(rows[1].task_id, "cross_file_first-abc123-L2+extra");
        let arm = |r: &TaskRow| r.codebase.as_ref().and_then(|c| c.arm.clone());
        assert_eq!(arm(&rows[0]).as_deref(), Some("no_extra"));
        assert_eq!(arm(&rows[1]).as_deref(), Some("extra"));
        let extra = |r: &TaskRow| r.codebase.as_ref().and_then(|c| c.extra.clone());
        assert!(extra(&rows[0]).is_none(), "the no_extra arm sent nothing");
        assert_eq!(
            extra(&rows[1]).map(|e| e.path),
            Some("src/defs.rs".to_owned())
        );
        assert_eq!(
            rows[1]
                .codebase
                .as_ref()
                .map(|c| c.also_first_uses.clone()),
            Some(vec!["Widget".to_owned()])
        );
    }

    #[test]
    fn resume_skips_one_arm_and_still_owes_the_other() {
        let done = vec![(
            "codebase".to_owned(),
            "cross_file_first-abc123-L2".to_owned(),
            crate::core::bench::store::Transport::Buffered,
        )];
        let (rows, posts, bodies) =
            drive("resume-arm", &prepared_cross(), (vec![Ok(infill_200())], done));
        assert_eq!(posts, 1, "only the extra arm was still owed");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].task_id, "cross_file_first-abc123-L2+extra");
        assert_eq!(bodies[0]["input_extra"][0]["filename"], "src/defs.rs");
    }
```

Rewrite the two moved tests to call `drive(name, &prepared_pair(), (replies, vec![]))` and ignore the third element, and give `prepared_pair` the `counts: Counts { in_file: 2, function_body: 0, cross_file_first: 0 }` field.

- [ ] **Step 6: Run to verify they fail**

Run: `cargo test --lib codebase::run 2>&1 | tail -20`
Expected: `no field 'arm' on type 'CodebaseRow'` (Step 7 adds it) and `no field 'bodies'` on `ScriptedInfill`. Once those compile, the arm tests fail on `assertion `left == right` failed: posts` — one POST, not two.

- [ ] **Step 7: Add the row's three fields**

`src/core/bench/store.rs` — in `CodebaseRow` (`:135-156`), after `unsupported`:

```rust
    /// Which arm of a cross-file crossing this row is — `"no_extra"` or
    /// `"extra"`. `None` on the same-file tiers, which have one arm and so
    /// no arm to name. Slice-A rows load as `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arm: Option<String>,
    /// The file the "extra" arm sent, and how much of it. `None` everywhere
    /// else — including the "no_extra" arm, which sent nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<ExtraFile>,
    /// Other names whose first use in the file also falls in this span.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub also_first_uses: Vec<String>,
```

and extend the import on `:17` to `use crate::core::bench::codebase::{Excluded, ExtraFile, TaskTier};`.

- [ ] **Step 8: Implement the arms**

`src/core/bench/codebase/run.rs` — add above `run_codebase`:

```rust
/// The `arm` a cross-file row records.
pub const NO_EXTRA: &str = "no_extra";
pub const WITH_EXTRA: &str = "extra";

/// One crossing of one task: the id it is recorded under, the arm it names,
/// and whether the defining file goes up with it.
struct Arm {
    id: String,
    label: Option<&'static str>,
    with_extra: bool,
}

/// The arms one task is crossed on: one for the same-file tiers, two for
/// `cross_file_first` — without the defining file, then with it, in that
/// fixed order (§5). Distinct ids mean `--resume` skips per arm.
fn arms(task: &CodebaseTask) -> Vec<Arm> {
    if task.tier != crate::core::bench::codebase::TaskTier::CrossFileFirst {
        return vec![Arm {
            id: task.id.clone(),
            label: None,
            with_extra: false,
        }];
    }
    vec![
        Arm {
            id: task.id.clone(),
            label: Some(NO_EXTRA),
            with_extra: false,
        },
        Arm {
            id: format!("{}+extra", task.id),
            label: Some(WITH_EXTRA),
            with_extra: true,
        },
    ]
}

/// One arm's crossing inputs (§4 — keeps `infill_or_latch` at 3 parameters).
struct Crossing<'a> {
    task: &'a CodebaseTask,
    with_extra: bool,
}

/// What this arm sends beside the file: the defining file on the "extra"
/// arm, nothing on the "no_extra" arm or on a same-file tier.
fn extra_chunk<'a>(crossing: &'a Crossing) -> Option<runner::ExtraChunk<'a>> {
    if !crossing.with_extra {
        return None;
    }
    let extra = crossing.task.extra.as_ref()?;
    Some(runner::ExtraChunk {
        filename: &extra.path,
        text: &crossing.task.extra_text,
    })
}
```

Change `infill_or_latch`'s second parameter from `task: &CodebaseTask` to `crossing: &Crossing`, bind `let task = crossing.task;` at the top of the non-latched path, and set `extra: extra_chunk(crossing)` on the `InfillTask` literal. The latch, the `eprintln!`s and the two `Unavailable` constructors are unchanged.

Replace `run_codebase`'s loop:

```rust
/// Every sampled task through `/infill`, recorded with its raw prediction. A
/// cross-file task is crossed twice — without the defining file, then with
/// it — and each arm is its own row and its own `--resume` key. A model
/// without FIM records every arm unavailable with the reason and stops
/// firing: a capability, never a zero.
pub fn run_codebase(
    sink: &mut Sink,
    wire: &runner::ProbeWire,
    prepared: &super::Prepared,
) -> Result<(), ChekovError> {
    let mut unsupported: Option<String> = None;
    for task in &prepared.tasks {
        for arm in arms(task) {
            if sink.is_done(&TaskKey::buffered("codebase", &arm.id)) {
                continue;
            }
            let crossing = Crossing {
                task,
                with_extra: arm.with_extra,
            };
            let outcome = infill_or_latch(wire, &crossing, &mut unsupported);
            record_codebase_task(
                sink,
                task,
                Recorded {
                    outcome,
                    symbols: &prepared.symbols,
                    arm: &arm,
                },
            )?;
        }
    }
    Ok(())
}
```

Extend `Recorded` and `record_codebase_task`:

```rust
/// What one arm's outcome needs to become a row (§4).
struct Recorded<'a> {
    outcome: Result<ProbeArtifact, Unavailable>,
    symbols: &'a ladder::Symbols,
    arm: &'a Arm,
}

fn record_codebase_task(
    sink: &mut Sink,
    task: &CodebaseTask,
    recorded: Recorded,
) -> Result<(), ChekovError> {
    let parts = row_parts(recorded.outcome);
    let symbols_score = parts
        .grade
        .is_none()
        .then(|| symbols_tier_score(&scored_for(task, &parts.prediction, &recorded)))
        .flatten();
    sink.writer.append(store::Task {
        suite: "codebase".into(),
        task_id: recorded.arm.id.clone(),
        measure: parts.measure,
        grade: parts.grade,
        transport: store::Transport::Buffered,
        codebase: Some(store::CodebaseRow {
            tier: task.tier,
            file: task.file.clone(),
            line: task.line,
            label: MASK_LABEL.to_owned(),
            gold: task.gold.clone(),
            prediction: parts.prediction,
            prefix: task.prefix.clone(),
            suffix: task.suffix.clone(),
            excluded: task.excluded.clone(),
            symbols_score,
            unsupported: parts.unsupported,
            arm: recorded.arm.label.map(str::to_owned),
            extra: recorded
                .arm
                .with_extra
                .then(|| task.extra.clone())
                .flatten(),
            also_first_uses: task.also_first_uses.clone(),
        }),
    })
}

/// Tier 5's inputs for this arm: the with-extra arm was shown G, so G's
/// names exist for it; the without arm was not, and is scored without them.
fn scored_for<'a>(
    task: &'a CodebaseTask,
    prediction: &'a str,
    recorded: &'a Recorded,
) -> ladder::Scored<'a> {
    ladder::Scored {
        task,
        prediction,
        symbols: recorded.symbols,
        extra: if recorded.arm.with_extra {
            &task.extra_text
        } else {
            ""
        },
    }
}
```

Change `symbols_tier_score` to take the built `Scored`:

```rust
/// Tier 5 for one prediction, or `None` when the ladder skips it — never a
/// zero standing in for "not scored".
fn symbols_tier_score(scored: &ladder::Scored) -> Option<f64> {
    ladder::score_all(scored)
        .into_iter()
        .find_map(|(tier, score)| match (tier, score) {
            (ladder::Tier::Symbols, ladder::Score::Value(v)) => Some(v),
            _ => None,
        })
}
```

- [ ] **Step 9: Give the ladder the extra text**

`src/core/bench/codebase/ladder.rs` — extend `Scored` (`:234-238`):

```rust
pub struct Scored<'a> {
    pub task: &'a CodebaseTask,
    pub prediction: &'a str,
    pub symbols: &'a Symbols,
    /// The extra file's text on the with-extra arm, `""` otherwise. The model
    /// was shown that file, so its names exist for it (§6); the without arm
    /// is scored against the page it actually saw.
    pub extra: &'a str,
}
```

and in `score_all` (`:270-273`) change the context line to:

```rust
    let context = format!("{}{}{}", t.prefix, t.suffix, s.extra);
```

Add `extra: ""` to the `Scored` literals in `ladder.rs`'s own tests (`symbols_scores_a_fabricated_identifier_down_and_a_gold_binding_up` and `all_seven_tiers_are_reported_and_the_exec_tiers_are_skipped`), plus a new test after the first of those:

```rust
    #[test]
    fn a_name_only_the_extra_file_carries_exists_on_the_with_arm_and_not_without() {
        let t = task(TaskTier::CrossFileFirst, "let a = build(1);");
        let scored = |extra| {
            score_all(&Scored {
                task: &t,
                prediction: "let a = build(1);",
                symbols: &Symbols(BTreeSet::new()),
                extra,
            })
            .into_iter()
            .find_map(|(tier, s)| match (tier, s) {
                (Tier::Symbols, Score::Value(v)) => Some(v),
                _ => None,
            })
            .expect("tier 5 has a value")
        };
        assert!(
            scored("pub fn build(n: u32) -> u32 { n }\n") > scored(""),
            "the extra file makes its names exist"
        );
    }
```

(`task(tier, gold)` is the existing helper at `ladder.rs:660`; Task 1 already gave it the new `CodebaseTask` and `Excluded` fields.)

Tiers 1 and 2 must also apply to the new tier. A cross-file span IS an `in_file` span (spec §3.2: "the gold, prefix and suffix are the `in_file` span's, unchanged"), and §7.2's report shows `exact` and `edit_sim` on both cross-file lines — but `stored_tier` gates them on `tier == InFile` alone. In `ladder.rs:257`, replace

```rust
    let line_level = text.tier == TaskTier::InFile;
```

with

```rust
    // A cross-file span is a statement, exactly as an `in_file` span is: same
    // mask shape, one right answer expected, so tiers 1-2 mean the same thing
    // there. Only `function_body`, where many different bodies are correct,
    // skips them.
    let line_level = text.tier != TaskTier::FunctionBody;
```

and add this test to `ladder.rs`'s tests module:

```rust
    #[test]
    fn a_cross_file_span_is_scored_on_tiers_one_and_two_like_an_in_file_span() {
        let t = task(TaskTier::CrossFileFirst, "let a = build(1);");
        let scores = score_all(&Scored {
            task: &t,
            prediction: "let a = build(1);",
            symbols: &Symbols(BTreeSet::new()),
            extra: "",
        });
        for want in [Tier::Exact, Tier::EditSim] {
            let score = scores
                .iter()
                .find_map(|(tier, s)| (*tier == want).then_some(*s))
                .expect("the tier is reported");
            assert!(
                matches!(score, Score::Value(v) if approx(v, 1.0)),
                "{want:?} {score:?}"
            );
        }
    }
```

- [ ] **Step 10: Run the tests**

Run: `cargo test --lib codebase 2>&1 | tail -20`
Expected: `test result: ok.` — the two arm tests, the resume test, the ladder test and the two moved run-path tests all pass.

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings 2>&1 | tail -10 && cargo test 2>&1 | grep -E "test result|FAILED"`
Expected: no warnings; all `ok`.

- [ ] **Step 11: Commit**

```bash
git add src/core/bench/codebase/run.rs src/core/bench/codebase/ladder.rs \
        src/core/bench/store.rs && \
git commit -m "$(cat <<'EOF'
feat(codebase): cross every cross-file task twice, without then with G

The no_extra arm keeps the task's own id; the extra arm takes <id>+extra and
sends the defining file as input_extra, so --resume skips per arm through the
existing TaskKey. The row records which arm it is and what that arm sent.
Tier 5's Known.context includes the extra text on the with arm only: the
model was shown G, so G's names exist for it. The FIM latch and per-task
unavailability apply per arm — an arm that failed is one unavailable row.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01YDaTTHJySW2ix3P85BcP6W
EOF
)"
```

---

### Task 7: `store.rs` — the header, the three new lines, and the 24-wide column

**Files:**
- Modify: `src/core/bench/store.rs:593-704` (`render_codebase`, `tier_line`, `tier_mean`, `symbols_cell`)
- Modify: `src/core/bench/codebase/mod.rs` (add `tier_counts_clause`)
- Modify: `src/core/bench/codebase/ladder.rs:47` (`EXEC_SKIPPED` says slice **B2**)
- Modify: `src/core/bench/store.rs:1270-1500` (the codebase test fixtures and their exact strings)

**Interfaces:**
- Consumes (Task 1): `TaskTier::CrossFileFirst`, `ExtraFile`, `Excluded.cross_file_withheld`.
- Consumes (Task 4): `Counts { in_file, function_body, cross_file_first }`.
- Consumes (Task 6): `CodebaseRow.{arm, extra, also_first_uses}` — the three fields land there because the run loop writes them; this task renders them. `run::{NO_EXTRA, WITH_EXTRA}`.
- Produces:
  ```rust
  // codebase/mod.rs
  pub fn tier_counts_clause(counts: Counts) -> String;   // "12 in_file, 6 function_body, 6 cross_file_first × 2 arms"

  // core/bench/store.rs — all private to the module
  fn base_id(task_id: &str) -> &str;                     // strips a trailing "+extra"
  fn scores_line(label: &str, group: &[&CodebaseRow]) -> String;   // replaces tier_line
  fn lift_line(rows: &[&TaskRow]) -> String;
  ```

**Spec ambiguity resolved (§7.2, "the shortfall reason on its own line"):** the shortfall's counts live in `Prepared`, which the stored run does not carry, so the report cannot repeat the sampler's reason. It prints what a run's rows can back — `cross_file_first        none sampled — no unambiguous cross-file first use in this repository` — and the sampler's own numbers stay on the dry-run plan line (Task 8). The omission is therefore stated, never silent.

**Spec ambiguity resolved (the lift's parenthetical):** §7.2 shows `(6 files sent, 41.2 KiB, 1 truncated; 2 withheld)` for a complete tier and says the line "says `(n=k of 6)`" when a task is missing an arm. Both are rendered from one format: the `n=k of N; ` clause is prefixed only when `k < N`, so the complete case matches the spec's string exactly.

- [ ] **Step 1: Write the failing tests**

In `src/core/bench/store.rs`'s tests module, extend the fixtures and add three tests. First, give `codebase_task` (`:1304-1329`) the new fields — inside its `CodebaseRow`, after `unsupported: false,`:

```rust
                arm: None,
                extra: None,
                also_first_uses: vec![],
```

and inside its `Excluded`, after `cfg_test_lines: 0,`: `cross_file_withheld: 0,`.

Then add:

```rust
    /// One arm of a cross-file task: same span, same gold, different context
    /// and so a different prediction.
    fn cross_arm(id: &str, arm: &str, prediction: &str) -> Task {
        let mut task = codebase_task(CodebaseFixture {
            id,
            tier: TaskTier::InFile,
            gold: "let a = build(1);",
            prediction,
        });
        task.task_id = id.into();
        if let Some(row) = task.codebase.as_mut() {
            row.tier = TaskTier::CrossFileFirst;
            row.arm = Some(arm.into());
            row.also_first_uses = vec!["Widget".into()];
            row.symbols_score = Some(if arm == "extra" { 1.0 } else { 0.5 });
            if arm == "extra" {
                row.extra = Some(ExtraFile {
                    path: "src/defs.rs".into(),
                    bytes: 2048,
                    truncated: false,
                });
                row.excluded.cross_file =
                    "sent src/defs.rs (2.0 KiB); withheld 2 (contain the answer)".into();
                row.excluded.cross_file_withheld = 2;
            }
        }
        task
    }

    #[test]
    fn the_block_reports_both_arms_and_the_lift_between_them() {
        let eval = scratch("codebase-arms");
        let mut writer = RunWriter::create(&eval, "r20-model", &head()).expect("create");
        for task in codebase_fixtures().into_iter().take(2).map(codebase_task) {
            writer.append(task).expect("append");
        }
        for (id, arm, prediction) in [
            ("cross_file_first-abc123-L4", "no_extra", "let a = guess(1);"),
            ("cross_file_first-abc123-L4+extra", "extra", "let a = build(1);"),
        ] {
            writer.append(cross_arm(id, arm, prediction)).expect("append");
        }
        let rendered = render_run(&RunLog::load(writer.dir()).expect("load"));
        for line in [
            "codebase     3 tasks, 4 crossings, from 1 files (2 in_file, 0 function_body, \
             1 cross_file_first × 2 arms) — boundary-scanned (not AST); context: same-file, \
             plus the defining file for cross_file_first (engine window ≤ n_batch; extra from ctx)",
            "             cross_file_first        exact 0.00",
            "             cross_file_first+extra  exact 1.00",
            "             context lift            exact +1.00",
            "(1 files sent, 2.0 KiB, 0 truncated; 2 withheld)",
            "             tiers 6-7 skipped: slice B2 (--allow-exec)",
        ] {
            assert!(rendered.contains(line), "{line}\n---\n{rendered}");
        }
        assert!(
            rendered.contains("symbols +0.50"),
            "tier 5's lift comes from the stored scores: {rendered}"
        );
    }

    /// A cross-file task answered on only one arm cannot contribute a
    /// difference, so it leaves the lift — and the line says so.
    #[test]
    fn a_task_measured_on_one_arm_only_is_excluded_from_the_lift() {
        let eval = scratch("codebase-half-arm");
        let mut writer = RunWriter::create(&eval, "r21-model", &head()).expect("create");
        for (id, arm, prediction) in [
            ("cross_file_first-abc123-L4", "no_extra", "let a = guess(1);"),
            ("cross_file_first-abc123-L4+extra", "extra", "let a = build(1);"),
            ("cross_file_first-abc123-L9", "no_extra", "let a = guess(2);"),
        ] {
            writer.append(cross_arm(id, arm, prediction)).expect("append");
        }
        writer
            .append(unavailable_codebase_task(
                "cross_file_first-abc123-L9+extra",
                "the server stopped answering",
                false,
            ))
            .expect("append");
        let rendered = render_run(&RunLog::load(writer.dir()).expect("load"));
        assert!(
            rendered.contains("(n=1 of 2; 1 files sent, 2.0 KiB, 0 truncated; 2 withheld)"),
            "{rendered}"
        );
        assert!(
            rendered.contains("(1 unavailable, excluded)"),
            "{rendered}"
        );
    }

    #[test]
    fn a_run_without_cross_file_tasks_says_so_instead_of_printing_three_empty_lines() {
        let eval = scratch("codebase-no-cross");
        let mut writer = RunWriter::create(&eval, "r22-model", &head()).expect("create");
        for task in codebase_fixtures().map(codebase_task) {
            writer.append(task).expect("append");
        }
        let rendered = render_run(&RunLog::load(writer.dir()).expect("load"));
        assert!(
            rendered.contains("(2 in_file, 1 function_body, 0 cross_file_first)"),
            "no arms to announce when there are no cross tasks: {rendered}"
        );
        assert!(
            rendered.contains(
                "             cross_file_first        none sampled — no unambiguous \
                 cross-file first use in this repository"
            ),
            "{rendered}"
        );
        assert!(!rendered.contains("context lift"), "{rendered}");
        assert!(!rendered.contains("cross_file_first+extra"), "{rendered}");
    }

    /// A slice-A row predates all three fields and must still load.
    #[test]
    fn a_row_written_before_the_arm_fields_loads_with_none() {
        let old = serde_json::json!({
            "tier": "in_file",
            "file": "src/a.rs",
            "line": 7,
            "label": "boundary-scanned (not AST)",
            "gold": "let a = 1;",
            "prediction": "let a = 1;",
            "prefix": "fn f() {\n",
            "suffix": "\n}\n",
            "excluded": { "doc_comment": 0, "cross_file": "n/a: same-file" }
        })
        .to_string();
        let row: CodebaseRow = serde_json::from_str(&old).expect("a slice-A row still loads");
        assert!(row.arm.is_none());
        assert!(row.extra.is_none());
        assert!(row.also_first_uses.is_empty());
        assert_eq!(row.excluded.cross_file_withheld, 0);
        assert_eq!(row.excluded.cfg_test_lines, 0);
    }
```

Add `ExtraFile` to the tests module's `use crate::core::bench::codebase::{…}` line.

Update the three existing codebase assertions for the widened column and the new header:

- `codebase_rows_round_trip_and_the_block_recomputes_tiers_from_stored_text` (`:1379-1386`) — the `expected` array becomes:

```rust
        let expected = [
            "codebase     3 tasks, 3 crossings, from 1 files (2 in_file, 1 function_body, \
             0 cross_file_first) — boundary-scanned (not AST); context: same-file, plus the \
             defining file for cross_file_first (engine window ≤ n_batch; extra from ctx)",
            "in_file                 exact 0.50   edit_sim",
            "symbols 1.00 (scored at run time)   (n=2)",
            "function_body           ident_f1",
            "tiers 6-7 skipped: slice B2 (--allow-exec)",
        ];
```

- `a_codebase_block_says_how_many_test_lines_were_elided_and_from_how_many_files` (`:1412-1417`) — the substring becomes `"(engine window ≤ n_batch; extra from ctx); tests elided: 21 lines in 2 files"`.
- `a_partly_unavailable_codebase_run_scores_what_was_answered_and_says_how_many_were_not` (`:1436`) — the substring becomes `"codebase     2 tasks, 2 crossings, from 1 files (2 in_file, 0 function_body, 0 cross_file_first)"`.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --lib store 2>&1 | tail -30`
Expected: the three new tests fail on the missing header/lines, and the three updated ones fail on the old 14-wide labels.

- [ ] **Step 3: Say slice B2**

`src/core/bench/codebase/ladder.rs:47` — `const EXEC_SKIPPED: &str = "slice B2 (--allow-exec)";`, and update the assertion in `all_seven_tiers_are_reported_and_the_exec_tiers_are_skipped` if it names the old text.

- [ ] **Step 4: Add the shared tier census**

`src/core/bench/codebase/mod.rs`, beside `Counts`:

```rust
/// `12 in_file, 6 function_body, 6 cross_file_first × 2 arms` — the tier
/// census the dry-run line and the report header both print, from one place
/// so the two cannot drift. `× 2 arms` is dropped when no cross-file task was
/// sampled: there is no second arm to announce.
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
```

- [ ] **Step 5: Rewrite the report block**

`src/core/bench/store.rs` — replace `render_codebase` (`:593-634`) and `tier_line` (`:655-677`) with:

```rust
/// The codebase block: counts and labels, then one line per tier group —
/// two of them for `cross_file_first`, one per arm — and the lift between
/// the arms.
///
/// The header says `engine window ≤ n_batch` because llama.cpp's `/infill`
/// caps the prefix at ~¾·`n_batch` tokens and the suffix at ~¼·`n_batch`;
/// `extra from ctx` because the extra chunk is bounded by the context, not
/// by the batch. chekov sends whole files and grades over whole files, but a
/// long file reaches the model only in part.
#[must_use]
pub fn render_codebase(log: &RunLog) -> String {
    let rows: Vec<&TaskRow> = rows_of(log, "codebase")
        .filter(|r| r.codebase.is_some())
        .collect();
    if rows.is_empty() {
        return String::new();
    }
    let (kept, excluded) = measured(&rows);
    if kept.is_empty() {
        return codebase_na_line(&rows);
    }
    let mut out = codebase_header(&kept, excluded);
    out.push_str(&scores_line("in_file", &group(&kept, TaskTier::InFile, None)));
    out.push_str(&scores_line(
        "function_body",
        &group(&kept, TaskTier::FunctionBody, None),
    ));
    out.push_str(&cross_lines(&kept));
    out.push_str("             tiers 6-7 skipped: slice B2 (--allow-exec)\n");
    out
}

fn codebase_header(kept: &[&TaskRow], excluded: usize) -> String {
    let counts = crate::core::bench::codebase::Counts {
        in_file: tier_tasks(kept, TaskTier::InFile),
        function_body: tier_tasks(kept, TaskTier::FunctionBody),
        cross_file_first: tier_tasks(kept, TaskTier::CrossFileFirst),
    };
    format!(
        "codebase     {} tasks, {} crossings, from {} files ({}) — {}; context: same-file, \
         plus the defining file for cross_file_first (engine window ≤ n_batch; extra from \
         ctx){}{}\n",
        distinct_tasks(kept),
        kept.len(),
        distinct_files(kept),
        crate::core::bench::codebase::tier_counts_clause(counts),
        crate::core::bench::codebase::MASK_LABEL,
        elided_note(kept),
        excluded_note(excluded),
    )
}

/// The cross-file tier's two arm lines and the lift — or, when no cross-file
/// task was sampled, one line saying that rather than three empty ones.
fn cross_lines(kept: &[&TaskRow]) -> String {
    use crate::core::bench::codebase::run::{NO_EXTRA, WITH_EXTRA};
    let without = group(kept, TaskTier::CrossFileFirst, Some(NO_EXTRA));
    if without.is_empty() {
        return "             cross_file_first        none sampled — no unambiguous \
                cross-file first use in this repository\n"
            .to_owned();
    }
    let with = group(kept, TaskTier::CrossFileFirst, Some(WITH_EXTRA));
    format!(
        "{}{}{}",
        scores_line("cross_file_first", &without),
        scores_line("cross_file_first+extra", &with),
        lift_line(kept)
    )
}

/// A cross-file task's id without its arm suffix — the two arms are one task.
fn base_id(task_id: &str) -> &str {
    task_id.strip_suffix("+extra").unwrap_or(task_id)
}

/// Distinct tasks behind these rows: the header counts tasks, the crossings
/// count is `rows.len()`.
fn distinct_tasks(rows: &[&TaskRow]) -> usize {
    rows.iter()
        .map(|r| base_id(&r.task_id))
        .collect::<std::collections::BTreeSet<&str>>()
        .len()
}

fn tier_tasks(rows: &[&TaskRow], tier: TaskTier) -> usize {
    rows.iter()
        .filter(|r| r.codebase.as_ref().is_some_and(|c| c.tier == tier))
        .map(|r| base_id(&r.task_id))
        .collect::<std::collections::BTreeSet<&str>>()
        .len()
}

/// One tier's rows, optionally restricted to one arm.
fn group<'a>(rows: &[&'a TaskRow], tier: TaskTier, arm: Option<&str>) -> Vec<&'a CodebaseRow> {
    rows.iter()
        .filter_map(|r| r.codebase.as_ref())
        .filter(|c| c.tier == tier && arm.is_none_or(|a| c.arm.as_deref() == Some(a)))
        .collect()
}

/// One line of tier means for a group, at the 24-wide label column every
/// line of the block shares.
fn scores_line(label: &str, group: &[&CodebaseRow]) -> String {
    if group.is_empty() {
        return String::new();
    }
    let mut cells = Vec::new();
    for t in [Tier::Exact, Tier::EditSim, Tier::IdentF1, Tier::Parse] {
        if let Some(mean) = tier_mean(group, t) {
            cells.push(format!("{} {mean:.2}", t.label()));
        }
    }
    cells.push(symbols_cell(group));
    format!(
        "             {label:<24}{}   (n={})\n",
        cells.join("   "),
        group.len()
    )
}
```

- [ ] **Step 6: Implement the lift**

Append to `src/core/bench/store.rs`, after `scores_line`:

```rust
/// The per-tier difference of arm means over the tasks measured in BOTH arms.
///
/// A task unavailable in either arm never reaches `kept`, so it is excluded
/// here by construction — a difference against a missing arm is not a
/// measurement, and would read as a lift of exactly the arm that answered.
fn lift_line(kept: &[&TaskRow]) -> String {
    let pairs = arm_pairs(kept);
    if pairs.is_empty() {
        return String::new();
    }
    let mut cells = Vec::new();
    for t in [Tier::Exact, Tier::EditSim, Tier::IdentF1, Tier::Parse] {
        if let Some(delta) = tier_delta(&pairs, t) {
            cells.push(format!("{} {delta:+.2}", t.label()));
        }
    }
    if let Some(delta) = symbols_delta(&pairs) {
        cells.push(format!("symbols {delta:+.2}"));
    }
    format!(
        "             {:<24}{}   ({})\n",
        "context lift",
        cells.join("  "),
        lift_note(kept, &pairs)
    )
}

/// `(no_extra, extra)` for every cross-file task measured in both arms.
fn arm_pairs<'a>(kept: &[&'a TaskRow]) -> Vec<(&'a CodebaseRow, &'a CodebaseRow)> {
    use crate::core::bench::codebase::run::{NO_EXTRA, WITH_EXTRA};
    let with = arm_map(kept, WITH_EXTRA);
    arm_map(kept, NO_EXTRA)
        .into_iter()
        .filter_map(|(id, without)| with.get(id).map(|w| (without, *w)))
        .collect()
}

/// The cross-file rows of one arm, keyed by the task they belong to.
fn arm_map<'a>(
    kept: &[&'a TaskRow],
    arm: &str,
) -> std::collections::BTreeMap<&'a str, &'a CodebaseRow> {
    kept.iter()
        .filter_map(|r| Some((base_id(&r.task_id), r.codebase.as_ref()?)))
        .filter(|(_, c)| c.tier == TaskTier::CrossFileFirst && c.arm.as_deref() == Some(arm))
        .collect()
}

/// The mean of `with − without` for the tiers recomputed from stored text.
fn tier_delta(pairs: &[(&CodebaseRow, &CodebaseRow)], tier: Tier) -> Option<f64> {
    let deltas: Vec<f64> = pairs
        .iter()
        .filter_map(|(a, b)| match (recompute(a, tier), recompute(b, tier)) {
            (Score::Value(x), Score::Value(y)) => Some(y - x),
            _ => None,
        })
        .collect();
    if deltas.is_empty() {
        return None;
    }
    Some(deltas.iter().sum::<f64>() / as_f64(deltas.len()))
}

/// Tier 5's lift, from the scores stored at run time on both arms.
fn symbols_delta(pairs: &[(&CodebaseRow, &CodebaseRow)]) -> Option<f64> {
    let deltas: Vec<f64> = pairs
        .iter()
        .filter_map(|(a, b)| Some(b.symbols_score? - a.symbols_score?))
        .collect();
    if deltas.is_empty() {
        return None;
    }
    Some(deltas.iter().sum::<f64>() / as_f64(deltas.len()))
}

/// `6 files sent, 41.2 KiB, 1 truncated; 2 withheld`, prefixed `n=k of N; `
/// when a task was measured on one arm only — the lift never runs quietly
/// over fewer tasks than the tier has.
fn lift_note(kept: &[&TaskRow], pairs: &[(&CodebaseRow, &CodebaseRow)]) -> String {
    let total = tier_tasks(kept, TaskTier::CrossFileFirst);
    let sent: Vec<&crate::core::bench::codebase::ExtraFile> =
        pairs.iter().filter_map(|(_, b)| b.extra.as_ref()).collect();
    let bytes: usize = sent
        .iter()
        .map(|e| usize::try_from(e.bytes).unwrap_or(0))
        .sum();
    let truncated = sent.iter().filter(|e| e.truncated).count();
    let withheld: u32 = pairs
        .iter()
        .map(|(_, b)| b.excluded.cross_file_withheld)
        .sum();
    let scope = if pairs.len() == total {
        String::new()
    } else {
        format!("n={} of {total}; ", pairs.len())
    };
    format!(
        "{scope}{} files sent, {:.1} KiB, {truncated} truncated; {withheld} withheld",
        sent.len(),
        as_f64(bytes) / 1024.0,
    )
}
```

- [ ] **Step 7: Run the tests**

Run: `cargo test --lib store 2>&1 | tail -20`
Expected: `test result: ok.`

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings 2>&1 | tail -10 && cargo test 2>&1 | grep -E "test result|FAILED"`
Expected: no warnings; all `ok`.

- [ ] **Step 8: Commit**

```bash
git add src/core/bench/store.rs src/core/bench/codebase/mod.rs \
        src/core/bench/codebase/ladder.rs && \
git commit -m "$(cat <<'EOF'
feat(bench): the report's two cross-file arms and the context lift

The header counts tasks AND crossings and names the tier census through one
shared clause, so the plan line and the block cannot drift. Two arm lines
plus a context lift over the tasks measured in BOTH arms, with what was sent
(files, KiB, truncated) and what rule (b) withheld. The label column widens
to 24 for every line. A run with no cross-file task says so on one line
rather than printing three empty ones.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01YDaTTHJySW2ix3P85BcP6W
EOF
)"
```

---

### Task 8: The estimate, the dry-run line, and the docs

**Files:**
- Modify: `src/commands/capability.rs:847-918` (`codebase_plan_line`, `bench_estimate`), tests near `:2045`
  — Task 6's move took `CodebaseTask`/`Excluded`/`Prepared`/`Symbols` out of this file's test
  imports, so Step 1 re-adds the ones the new fixture needs.
- Modify: `README.md:103` (the `--codebase` row) and `README.md:119-138` (the codebase-mode paragraph)
- Modify: `CHANGELOG.md:37` (`### Added`, at the top of the list)
- Modify: `IDEAS.md:134` (the capability entry's status line)
- Modify: `docs/superpowers/specs/2026-08-29-codebase-mode-slice-a-design.md:18-21` (a pointer to B1)

**Interfaces:**
- Consumes (Task 4): `Prepared.counts`, `Counts`.
- Consumes (Task 7): `codebase::tier_counts_clause(Counts) -> String`.
- Produces: nothing later tasks consume.

- [ ] **Step 1: Write the failing tests**

In `src/commands/capability.rs`'s tests module, after `the_effective_suite_is_throughput_by_default_and_nothing_extra_with_codebase_alone` (`:2045`):

```rust
    use crate::core::bench::codebase::ladder::Symbols;
    use crate::core::bench::codebase::{
        CodebaseTask, Counts, Excluded, Prepared, TaskTier,
    };

    /// A task shaped only enough to be counted — the plan line reads
    /// `tasks.len()` and `counts`, nothing else.
    fn plan_task(line: usize) -> CodebaseTask {
        CodebaseTask {
            id: format!("in_file-abc123-L{line}"),
            tier: TaskTier::InFile,
            file: "src/a.rs".into(),
            line,
            gold: "let a = 1;".into(),
            prefix: "fn f() {\n".into(),
            suffix: "\n}\n".into(),
            excluded: Excluded {
                doc_comment: 0,
                cross_file: "n/a: same-file".into(),
                cfg_test_lines: 0,
                cross_file_withheld: 0,
            },
            also_first_uses: vec![],
            extra: None,
            extra_text: String::new(),
        }
    }

    fn prepared_counts(in_file: usize, function_body: usize, cross: usize) -> Prepared {
        Prepared {
            head: "4818813deeaa11112222333344445555666677".into(),
            set_hash: "abcdef123456".into(),
            tasks: (0..in_file + function_body + cross).map(plan_task).collect(),
            shortfall: vec![],
            symbols: Symbols::default(),
            cfg_test_lines: 0,
            cfg_test_files: 0,
            counts: Counts {
                in_file,
                function_body,
                cross_file_first: cross,
            },
        }
    }

    #[test]
    fn the_plan_line_names_every_tier_and_the_second_arm() {
        let line = super::codebase_plan_line(
            &prepared_counts(12, 6, 6),
            std::path::Path::new("/r"),
        );
        assert_eq!(
            line,
            "codebase: 24 tasks from /r @ 4818813deeaa (12 in_file, 6 function_body, \
             6 cross_file_first × 2 arms)\n",
            "{line}"
        );
        let none = super::codebase_plan_line(
            &prepared_counts(12, 12, 0),
            std::path::Path::new("/r"),
        );
        assert!(
            none.contains("(12 in_file, 12 function_body, 0 cross_file_first)"),
            "no second arm to announce: {none}"
        );
    }

    #[test]
    fn the_estimate_counts_a_cross_file_task_twice() {
        assert_eq!(super::codebase_estimate_secs(&prepared_counts(12, 6, 6)), 180);
        assert_eq!(super::codebase_estimate_secs(&prepared_counts(12, 12, 0)), 144);
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --lib commands::capability::tests::the_plan_line 2>&1 | tail -15`
Expected: `cannot find function 'codebase_estimate_secs'`, and the plan line assertion fails on the missing tier census.

- [ ] **Step 3: Implement the estimate and the plan line**

`src/commands/capability.rs` — replace `codebase_plan_line` (`:847-870`):

```rust
/// `codebase: {n} tasks from {repo} @ {head[..12]} ({tier census})`, with what
/// the `#[cfg(test)]` cutter took and the shortfall parenthetical appended
/// only when there is something to say.
fn codebase_plan_line(
    prepared: &crate::core::bench::codebase::Prepared,
    repo: &std::path::Path,
) -> String {
    let head12 = &prepared.head[..12.min(prepared.head.len())];
    let census = crate::core::bench::codebase::tier_counts_clause(prepared.counts);
    let elided = if prepared.cfg_test_files == 0 {
        String::new()
    } else {
        format!(", tests elided in {} files", prepared.cfg_test_files)
    };
    let shortfall = if prepared.shortfall.is_empty() {
        String::new()
    } else {
        format!(" ({})", prepared.shortfall.join(", "))
    };
    format!(
        "codebase: {} tasks from {} @ {head12} ({census}){elided}{shortfall}\n",
        prepared.tasks.len(),
        repo.display()
    )
}

/// Six seconds per CROSSING, not per task: a cross-file task is crossed
/// twice, so the estimate is `(in_file + function_body + 2 × cross) × 6`.
fn codebase_estimate_secs(prepared: &crate::core::bench::codebase::Prepared) -> u64 {
    let c = prepared.counts;
    let crossings = c.in_file + c.function_body + 2 * c.cross_file_first;
    u64::try_from(crossings).unwrap_or(0) * 6
}
```

and in `bench_estimate` (`:892-902`) change the codebase term:

```rust
    let codebase_secs = inputs.prepared.map_or(0, codebase_estimate_secs);
```

- [ ] **Step 4: Run the tests**

Run: `cargo test --lib commands::capability 2>&1 | tail -15`
Expected: `test result: ok.`

- [ ] **Step 5: Update the README**

`README.md:103` — replace the `capability bench --codebase <PATH>` row's cell with:

```
| `capability bench --codebase <PATH>` | The repository at PATH (clean tree required) as 24 deterministic infill tasks, sampled from HEAD (`[bench] codebase_tasks`, split 12 `in_file` / 6 `function_body` / 6 `cross_file_first`), run through `/infill`, graded on tiers 1–5 (exact, edit similarity, identifier F1, parse, repo-symbol existence); tiers 6–7 are slice B2. A `cross_file_first` task masks the first use in a file of a symbol defined in **another** file, and is crossed **twice** — without that file and with it in `input_extra` — so the report can print what reading the repository buys. Masks are boundary-scanned, not AST, and the report says so. A model without FIM tokens is N/A, never zero. Given without `--suite`, the codebase corpus is the whole run — the throughput sweep does not come along. |
```

`README.md:119-138` — after the sentence ending `…deferred to slice B.` (replacing that clause) and before `Masks are boundary-scanned`, insert:

```
Six of the 24 tasks are `cross_file_first`: the mask is the first use in a
file of a symbol defined in exactly one other file, and each is crossed
twice — once with nothing but its own file, once with the defining file sent
as llama.cpp's `input_extra` (capped at 32 KiB, windowed on the declaration
line when the file is larger, and the row records which). The report prints
both arms and the `context lift` between them over the tasks answered in
both, which is the measurement this mode exists for. A name declared in two
or more files is ambiguous and never masked, and the shortfall line says how
many were skipped. Only tiers 1–5 are scored; tiers 6–7 (compile gate,
covering test) are deferred to slice B2 behind `--allow-exec`. Because the
task set now includes the new tier's ids, its hash — and so `corpus_id` —
changed: runs recorded before this are not comparable with runs after it, and
`compare` refuses them by that field.
```

- [ ] **Step 6: Update the CHANGELOG**

`CHANGELOG.md` — insert at the top of the `### Added` list (`:37`), in the file's existing style (full sentences, the reasoning included):

```markdown
- `capability bench --codebase` adds the `cross_file_first` tier: the mask is
  the first use in a file of a symbol declared in exactly one **other** file,
  found over the elided texts with a declaration index — never an ambiguous
  name (declared in two or more files), never a name the file declares itself,
  never a `use` line, and never a hit inside a string or a comment. The
  default 24 tasks now split 12 / 6 / 6 rather than 16 / 8. Each cross-file
  task is crossed **twice**: once with nothing but its own file, once with the
  defining file sent as llama.cpp's `input_extra` — capped at 32 KiB and
  otherwise windowed on the declaration line, with `truncated` and the exact
  bytes on the row. The report prints both arms and the `context lift` between
  them over the tasks answered in both, with what was sent (files, KiB,
  truncated) and what the leakage filter's rule (b) withheld — every other
  file whose text contains the answer verbatim, never the defining file,
  without which the tier is unanswerable. The two arms take distinct task ids
  (`<id>` and `<id>+extra`), so `--resume` skips per arm and an arm that
  failed is one unavailable row. Because the set hash covers the new tier's
  ids, any repository that yields cross-file tasks gets a new `corpus_id`:
  runs recorded before this slice are not comparable with runs after it, and
  `compare` refuses them by that field, as it should.
```

- [ ] **Step 7: Update IDEAS.md and the slice-A spec**

`IDEAS.md:134` — in the status line, replace

```
--codebase slice A SHIPPED 2026-08-29 (Rust, same-file, tiers 1-5); `#[cfg(test)]` rule amended 2026-08-29 (items elided, file kept); slices B (cross-file + exec tiers) and C (--judge) OPEN)**
```

with

```
--codebase slice A SHIPPED 2026-08-29 (Rust, same-file, tiers 1-5); `#[cfg(test)]` rule amended 2026-08-29 (items elided, file kept); slice B1 SHIPPED 2026-08-29 (cross_file_first, input_extra, two arms and the measured context lift; quota 12/6/6, corpus_id changed); slices B2 (exec tiers behind --allow-exec) and C (--judge) OPEN)**
```

`docs/superpowers/specs/2026-08-29-codebase-mode-slice-a-design.md:18-20` — replace the `**Slice B**` bullet with:

```markdown
- **Slice B1** — `cross_file_first` tasks with `input_extra` context, which is
  where the leakage filter's rules (a), (b) and (d) become live. Specified and
  shipped: `docs/superpowers/specs/2026-08-29-codebase-mode-slice-b1-design.md`.
- **Slice B2** — tiers 6 (compile gate) and 7 (covering test) behind
  `--allow-exec`.
```

- [ ] **Step 8: Check the docs against the code**

Run: `cargo test 2>&1 | grep -E "test result|FAILED"`
Expected: all `ok` — including the README-equals-defaults test, which reads `[bench]` defaults and is unaffected (`codebase_tasks` is still 24).

Run: `cargo fmt && cargo clippy --all-targets -- -D warnings 2>&1 | tail -10`
Expected: no warnings.

- [ ] **Step 9: Commit**

```bash
git add src/commands/capability.rs README.md CHANGELOG.md IDEAS.md \
        docs/superpowers/specs/2026-08-29-codebase-mode-slice-a-design.md && \
git commit -m "$(cat <<'EOF'
feat(capability): the two-arm estimate and dry-run line, and the docs

The estimate is (in_file + function_body + 2 × cross) × 6 seconds, and the
plan line names the tier census through the same clause the report header
uses. README, CHANGELOG and IDEAS say what the tier is, that a cross-file
task is crossed twice, and that corpus_id changed — runs from before this
slice are not comparable with runs after it. The slice-A spec points here.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01YDaTTHJySW2ix3P85BcP6W
EOF
)"
```

---

### Task 9: The live runs and the branch's evidence

The unit tests prove the mechanism; only a live model proves the tier discriminates. Two runs, on two repositories, both blocks quoted in the PR body.

**Files:**
- Modify: none in `src/`. The PR body is written to `/private/tmp/claude-501/-Users-amoscoletti-personal-dev-chekov/96078410-14c8-40ed-a7cf-651f31a43606/scratchpad/pr-body-b1.md`.

**Interfaces:**
- Consumes (Tasks 1–8): the whole slice, on `main`-equivalent behaviour.
- Produces: nothing in code.

**This task does not push and does not open a PR.** The controller does that after the whole-branch review. Leave the branch local, the PR body on disk, and say in the hand-off that both are ready.

- [ ] **Step 1: Build the release binary and confirm the plan**

```bash
cargo build --release && \
git -C /Users/amoscoletti/personal_dev/chekov status --porcelain
```
Expected: an empty `git status` (the gate refuses a dirty tree), and a built `target/release/chekov`.

- [ ] **Step 2: Make the two clean clones**

```bash
rm -rf /tmp/b1-chekov /tmp/b1-pushkin && \
git clone --quiet /Users/amoscoletti/personal_dev/chekov /tmp/b1-chekov && \
git clone --quiet /Users/amoscoletti/personal_dev/pushkin /tmp/b1-pushkin && \
git -C /tmp/b1-chekov rev-parse --short=12 HEAD && \
git -C /tmp/b1-pushkin rev-parse --short=12 HEAD
```
Expected: two 12-hex shas. Clones, not the working copies: the gate requires a clean tree and the run samples from HEAD.

- [ ] **Step 3: Dry-run both, and read the plan lines**

```bash
/Users/amoscoletti/personal_dev/chekov/target/release/chekov capability bench \
  --models ornith-1.5-35b-a3b --codebase /tmp/b1-chekov --dry-run && \
/Users/amoscoletti/personal_dev/chekov/target/release/chekov capability bench \
  --models ornith-1.5-35b-a3b --codebase /tmp/b1-pushkin --dry-run
```
Expected: each prints one `codebase: N tasks from /tmp/b1-… @ <sha12> (12 in_file, 6 function_body, 6 cross_file_first × 2 arms)…` line, plus the step table and an estimate around `(12 + 6 + 12) × 6 = 180 s` per candidate. If a lane is short, the line says why — record it for the PR body.

- [ ] **Step 4: Run both for real**

```bash
/Users/amoscoletti/personal_dev/chekov/target/release/chekov capability bench \
  --models ornith-1.5-35b-a3b --codebase /tmp/b1-chekov --yes
```
Expected: `run: <eval>/<timestamp>-ornith-1.5-35b-a3b`. Then the same for `/tmp/b1-pushkin`. Each takes roughly 3–6 minutes plus launch and teardown.

- [ ] **Step 5: Read both blocks back**

```bash
/Users/amoscoletti/personal_dev/chekov/target/release/chekov capability compare \
  --help > /dev/null && ls -1t "$(/Users/amoscoletti/personal_dev/chekov/target/release/chekov env 2>/dev/null | grep -o '/[^ ]*eval' | head -1)" | head -4
```

Then render each run's report (the run directory is what `bench` printed) and copy the two `codebase` blocks verbatim.

Expected in each block: the header with `N tasks, M crossings`, the four tier lines, and a `context lift` line. **Read the lift.** A positive lift on `exact`/`edit_sim` is the result this slice exists to produce; a zero or negative lift is a finding, not a failure — record it as measured and say so.

- [ ] **Step 6: Write the PR body**

Write `/private/tmp/claude-501/-Users-amoscoletti-personal-dev-chekov/96078410-14c8-40ed-a7cf-651f31a43606/scratchpad/pr-body-b1.md` containing, in order:

1. One paragraph: what B1 adds (the tier, `input_extra`, the two arms, the lift).
2. The two `codebase` blocks verbatim, each headed by the repository and its HEAD sha12.
3. This line, verbatim, under both blocks:

```
The set hash changed; not comparable with pre-B1 runs.
```

4. What is deliberately left out: slice B2 (tiers 6–7 behind `--allow-exec`), slice C (`--judge`), more than one extra file per task, docs as context, other languages, any composite score. Rule (d) stays vacuous — B1 never sends a documentation file.
5. Any lane that came up short, with the sampler's own reason quoted from the dry-run.

- [ ] **Step 7: Final gate on the whole branch**

```bash
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test 2>&1 | grep -E "test result|FAILED"
```
Expected: no diff from `fmt --check`, no clippy warnings, every `test result: ok.`

Run: `pushkin floor`
Expected: green.

- [ ] **Step 8: Commit nothing, hand off**

There is nothing to commit in this task — the runs live under `eval/`, which is not tracked, and the PR body is in the scratchpad. Report to the controller: the branch is complete and local, `pushkin floor` is green, and the PR body with both live blocks is at the scratchpad path above, ready for the push and the PR **after** the whole-branch review.
