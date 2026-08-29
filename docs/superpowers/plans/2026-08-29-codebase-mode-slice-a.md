# Codebase Mode Slice A Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `chekov capability bench --codebase <PATH>` turns a clean Rust repository into 24 deterministic same-file infill tasks, runs them through llama-server's `/infill`, stores gold + prediction per task, and reports scoring tiers 1–5 — never a zero for a model without FIM, never a malformed mask, never a claim the report cannot back.

**Architecture:** A new `core::bench::codebase` module with five focused files (masker, sample, filter, ladder, tree), one new crossing in `runner.rs` (`cross_infill`, direct to `/infill`, no facade), one optional row field in `store.rs` (`CodebaseRow`, scores recomputed on read), and command wiring in `commands/capability.rs` behind the existing per-candidate lifecycle. Everything is data-in/data-out and unit-tested without a model; the live run at the end is the acceptance.

**Tech Stack:** Rust (edition 2024, ≥1.88), `serde`/`serde_json`, `clap`, the house `hash::sha256_hex`; **no new crate**. Git via `std::process::Command`.

**Spec:** `docs/superpowers/specs/2026-08-29-codebase-mode-slice-a-design.md` (refines `docs/capability-spec.md` §8).

## Global Constraints

- No new dependency (the house hand-rolled SHA-256 to avoid one; the RNG is hand-rolled too).
- Every function ≤ 40 LOC, ≤ 3 parameters (bundle into a struct past that), nesting ≤ 3 — `clippy -D warnings` with the repo's pedantic set is the gate; `#[allow]`/`#[expect]` are blocked by pushkin on gated paths.
- Whole-file reads of `src/**` are blocked by pushkin: use `Read` with `offset`/`limit`, never `cat`/`grep` on `src/` paths; never `cd` in a shell command.
- Every `ChekovError` variant's Display names what failed AND a remediation command.
- Nothing degrades silently: an unknown is `??`/`N/A`/`skipped`, never a zero or a pass.
- Rust only; 24 tasks by default (`ceil(2n/3)` in_file, remainder function_body); same-file context; tiers 6–7 are `skipped` in slice A.
- Commit after every task with the trailer:
  `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>` and
  `Claude-Session: https://claude.ai/code/session_01YDaTTHJySW2ix3P85BcP6W`.
- Run `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test && pushkin floor` before each commit.

---

## File structure

| File | Responsibility |
|---|---|
| `src/core/bench/codebase/mod.rs` | `pub mod` lines; the `TaskTier` enum; `CodebaseTask` (the assembled task: id, tier, file, line, gold, prefix, suffix, excluded, label) |
| `src/core/bench/codebase/masker.rs` | `MaskSource` trait, `Candidate`, `RustBraceMasker`, and the string/comment-aware brace scanner (`balance`) shared with the ladder |
| `src/core/bench/codebase/sample.rs` | seeded xorshift64*, stratified selection, task ids, task-set hash, shortfall report |
| `src/core/bench/codebase/filter.rs` | prefix/suffix assembly, doc-comment stripping, `Excluded` |
| `src/core/bench/codebase/ladder.rs` | tiers 1–5 as pure functions; the Rust keyword list; the repo symbol set |
| `src/core/bench/codebase/tree.rs` | git: clean-tree gate, HEAD sha, worktree add/remove, `*.rs` file walk |
| `src/core/bench/runner.rs` | `+ InfillTask`, `cross_infill`, `InfillOutcome` |
| `src/core/bench/store.rs` | `+ CodebaseRow`, `Excluded` (re-export), the codebase report block |
| `src/commands/capability.rs` | `--codebase`, `Option<Suite>`, gate before launch, worktree lifecycle, `run_codebase`, estimate, `corpus_id` |
| `src/core/config.rs` | `[bench] codebase_tasks` |
| `src/error.rs` | `WorkingTreeDirty`, `CodebaseNoTasks`, `CodebaseWorktreeFailed` |
| `config.example.toml`, `README.md`, `CHANGELOG.md`, `IDEAS.md` | docs |

---

### Task 1: Config knob and error variants

**Files:**
- Modify: `src/core/config.rs` (BenchSection, its Default, tests near `bench_section_defaults_and_overrides_parse`)
- Modify: `src/error.rs` (after `BenchStreamFailed`)
- Modify: `config.example.toml` (`[bench]` block), `README.md` (no config-block change needed: `[bench]` is not in the README block)

**Interfaces:**
- Produces: `cfg.file.bench.codebase_tasks: u32` (default 24); `ChekovError::WorkingTreeDirty { path: PathBuf }`, `ChekovError::CodebaseNoTasks { path: PathBuf, reason: String }`, `ChekovError::CodebaseWorktreeFailed { step: String, reason: String }`.

- [ ] **Step 1: Write the failing tests**

In `src/core/config.rs` tests module, after `bench_section_defaults_and_overrides_parse`:

```rust
    #[test]
    fn codebase_tasks_defaults_to_24_and_overrides() {
        assert_eq!(BenchSection::default().codebase_tasks, 24);
        let root = scratch("cfg-codebase-tasks");
        std::fs::write(root.join("config.toml"), "[bench]\ncodebase_tasks = 12\n").expect("write");
        assert_eq!(Config::load(&root).expect("valid").file.bench.codebase_tasks, 12);
    }
```

In `src/error.rs` tests module:

```rust
    #[test]
    fn codebase_errors_name_their_remediation() {
        let dirty = ChekovError::WorkingTreeDirty { path: "/r".into() }.to_string();
        assert!(dirty.contains("/r") && dirty.contains("git status"), "{dirty}");
        let none = ChekovError::CodebaseNoTasks {
            path: "/r".into(),
            reason: "scanned 3 files, 0 candidate spans".into(),
        }
        .to_string();
        assert!(none.contains("0 candidate spans") && none.contains("Rust"), "{none}");
        let wt = ChekovError::CodebaseWorktreeFailed {
            step: "git worktree add".into(),
            reason: "exit status 128".into(),
        }
        .to_string();
        assert!(wt.contains("git worktree add") && wt.contains("git worktree prune"), "{wt}");
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test codebase 2>&1 | grep -E "^error" | sort -u`
Expected: `no field codebase_tasks`, `no variant named WorkingTreeDirty` (and the other two).

- [ ] **Step 3: Implement**

`src/core/config.rs` — add to `BenchSection` (after `release_interval_ms: u64,`):

```rust
    /// Tasks per `--codebase` run: two-thirds `in_file`, one-third
    /// `function_body`, sampled deterministically from HEAD.
    pub codebase_tasks: u32,
```

and to its `Default` (after `release_interval_ms: 500,`): `codebase_tasks: 24,`.

`src/error.rs` — after the `BenchStreamFailed` variant:

```rust
    #[error(
        "the working tree at {} is not clean — the codebase task set is sampled from \
         HEAD and must be reproducible; commit or stash (`git status` shows what is \
         pending), or run against a clean clone",
        path.display()
    )]
    WorkingTreeDirty { path: PathBuf },

    #[error(
        "no codebase tasks could be sampled from {} ({reason}) — slice A masks Rust \
         functions only; point --codebase at a repository with `*.rs` files outside \
         its tests",
        path.display()
    )]
    CodebaseNoTasks { path: PathBuf, reason: String },

    #[error(
        "codebase worktree step '{step}' failed ({reason}) — run `git worktree prune` \
         in the repository and retry"
    )]
    CodebaseWorktreeFailed { step: String, reason: String },
```

`config.example.toml` — after `release_interval_ms = 500`:

```toml
codebase_tasks = 24            # `--codebase` tasks per run (2/3 in_file, 1/3 function_body)
```

- [ ] **Step 4: Run tests**

Run: `cargo test 2>&1 | grep -E "test result: ok. [0-9]{3}|FAILED"`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add src/core/config.rs src/error.rs config.example.toml
git commit -m "feat(bench): codebase_tasks knob and the codebase error variants"
```

---

### Task 2: The Rust brace masker

**Files:**
- Create: `src/core/bench/codebase/mod.rs`, `src/core/bench/codebase/masker.rs`
- Modify: `src/core/bench/mod.rs` (add `pub mod codebase;` in alphabetical order, after `pub mod compare;`)

**Interfaces:**
- Produces (`codebase/mod.rs`):
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
  #[serde(rename_all = "snake_case")]
  pub enum TaskTier { InFile, FunctionBody }
  impl TaskTier { pub const fn label(self) -> &'static str }  // "in_file" | "function_body"
  pub const MASK_LABEL: &str = "boundary-scanned (not AST)";
  ```
- Produces (`codebase/masker.rs`):
  ```rust
  pub struct Candidate { pub tier: TaskTier, pub byte_range: std::ops::Range<usize>, pub line: usize, pub doc_comment: Option<std::ops::Range<usize>> }
  pub trait MaskSource { fn candidates(&self, text: &str) -> Vec<Candidate>; }
  pub struct RustBraceMasker;
  /// Balance of `{}`/`[]`/`()` outside strings, chars and comments; `None` if a
  /// closer never matches an opener. `Some(0)` means balanced.
  pub fn balance(text: &str) -> Option<i64>;
  ```
  Limits: `function_body` bodies of 3..=40 lines; `in_file` spans of 1..=8 lines.

- [ ] **Step 1: Write the failing tests** (`src/core/bench/codebase/masker.rs`, bottom)

```rust
#[cfg(test)]
mod tests {
    use super::{Candidate, MaskSource, RustBraceMasker, balance};
    use crate::core::bench::codebase::TaskTier;

    const SRC: &str = r#"//! module doc
use std::fmt;

/// Adds two numbers.
/// Second doc line.
pub fn add(a: i32, b: i32) -> i32 {
    let sum = a + b;
    let text = "not { a brace";
    // nor } this one
    sum
}

fn tiny() -> i32 { 1 }

pub(crate) fn branchy(flag: bool) -> &'static str {
    if flag {
        "yes"
    } else {
        "no"
    }
}
"#;

    fn tier(cands: &[Candidate], tier: TaskTier) -> Vec<&Candidate> {
        cands.iter().filter(|c| c.tier == tier).collect()
    }

    #[test]
    fn a_body_is_masked_between_its_braces_ignoring_braces_in_strings_and_comments() {
        let cands = RustBraceMasker.candidates(SRC);
        let bodies = tier(&cands, TaskTier::FunctionBody);
        assert_eq!(bodies.len(), 2, "add and branchy; tiny is one line: {cands:?}");
        let add = &bodies[0];
        let gold = &SRC[add.byte_range.clone()];
        assert!(gold.starts_with("\n    let sum"), "{gold:?}");
        assert!(gold.trim_end().ends_with("sum"), "{gold:?}");
        assert!(!gold.contains("pub fn add"), "the signature is context, not gold");
        assert_eq!(add.line, 7, "1-based first line of the span");
        let doc = add.doc_comment.clone().expect("adjacent /// block");
        assert!(SRC[doc].starts_with("/// Adds two numbers."));
        assert!(bodies[1].doc_comment.is_none(), "branchy has no doc comment");
    }

    #[test]
    fn in_file_spans_are_whole_balanced_statements() {
        let cands = RustBraceMasker.candidates(SRC);
        let spans: Vec<String> = tier(&cands, TaskTier::InFile)
            .iter()
            .map(|c| SRC[c.byte_range.clone()].trim().to_owned())
            .collect();
        assert!(spans.contains(&"let sum = a + b;".to_owned()), "{spans:?}");
        assert!(
            spans.iter().any(|s| s.starts_with("if flag {") && s.ends_with('}')),
            "an if with its blocks is one span: {spans:?}"
        );
        assert!(spans.iter().all(|s| balance(s) == Some(0)), "{spans:?}");
    }

    #[test]
    fn a_two_line_body_and_a_41_line_body_are_not_candidates() {
        let long_body = format!(
            "fn long() {{\n{}}}\n",
            (0..41).map(|i| format!("    let v{i} = {i};\n")).collect::<String>()
        );
        let src = format!("fn two() {{\n    1\n}}\n{long_body}");
        let bodies = RustBraceMasker.candidates(&src);
        assert!(
            !bodies.iter().any(|c| c.tier == TaskTier::FunctionBody),
            "2-line and 41-line bodies are out of range: {bodies:?}"
        );
    }

    #[test]
    fn balance_skips_strings_chars_and_comments() {
        assert_eq!(balance("{ \"}\" '}' // }\n /* } */ }"), Some(0));
        assert_eq!(balance("r#\"}\"# {"), Some(1));
        assert_eq!(balance("}"), None, "a closer with no opener");
        assert_eq!(balance("fn f() { let x = [1, (2)]; }"), Some(0));
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test codebase::masker 2>&1 | grep -E "^error" | sort -u`
Expected: unresolved imports / module not found.

- [ ] **Step 3: Implement**

`src/core/bench/mod.rs`: add `pub mod codebase;` after `pub mod compare;`.

`src/core/bench/codebase/mod.rs`:

```rust
//! `chekov capability bench --codebase` — the user's own Rust repository as
//! graded same-file infill tasks (spec §8, slice A).

pub mod filter;
pub mod ladder;
pub mod masker;
pub mod sample;
pub mod tree;

use serde::{Deserialize, Serialize};

/// Printed once per run: the masks come from a brace scanner, not a parser.
pub const MASK_LABEL: &str = "boundary-scanned (not AST)";

/// Which kind of span was masked (RepoBench taxonomy; cross-file is slice B).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskTier {
    InFile,
    FunctionBody,
}

impl TaskTier {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::InFile => "in_file",
            Self::FunctionBody => "function_body",
        }
    }
}
```

(Tasks 3–5 create `sample.rs`, `filter.rs`, `ladder.rs`, `tree.rs`; until then, create each as an empty file with a one-line `//!` doc so the module compiles: `//! Deterministic sampling (Task 3).` etc.)

`src/core/bench/codebase/masker.rs`:

```rust
//! Mask selection without a parser: a `fn` signature by regex-free scanning,
//! its body by brace balance, statements inside it by the same scanner.
//! Cruder than AST boundaries and labelled as such; a span that fails its
//! own balance check is discarded, never approximated.

use std::ops::Range;

use super::TaskTier;

const BODY_LINES: std::ops::RangeInclusive<usize> = 3..=40;
const SPAN_LINES: std::ops::RangeInclusive<usize> = 1..=8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub tier: TaskTier,
    pub byte_range: Range<usize>,
    /// 1-based first line of the span.
    pub line: usize,
    /// The `///` block directly above the function, when there is one.
    pub doc_comment: Option<Range<usize>>,
}

pub trait MaskSource {
    /// Every candidate span, in source order. Never a malformed span.
    fn candidates(&self, text: &str) -> Vec<Candidate>;
}

pub struct RustBraceMasker;

impl MaskSource for RustBraceMasker {
    fn candidates(&self, text: &str) -> Vec<Candidate> {
        let mut out = Vec::new();
        for sig in fn_signatures(text) {
            let Some(body) = body_after(text, sig.end) else {
                continue;
            };
            let interior = body.start + 1..body.end - 1;
            if BODY_LINES.contains(&line_count(&text[interior.clone()])) {
                out.push(Candidate {
                    tier: TaskTier::FunctionBody,
                    byte_range: interior.clone(),
                    line: line_of(text, interior.start + 1),
                    doc_comment: doc_comment_before(text, sig.start),
                });
            }
            out.extend(statement_spans(text, &interior));
        }
        out
    }
}

/// `fn name(` or `fn name<` at the start of a line (after visibility and
/// qualifiers). Returns the byte range of `fn` through the name.
fn fn_signatures(text: &str) -> Vec<Range<usize>> {
    let mut found = Vec::new();
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let lead = line.len() - trimmed.len();
        let rest = strip_qualifiers(trimmed);
        if let Some(after_fn) = rest.strip_prefix("fn ")
            && let Some(name_len) = ident_len(after_fn)
            && matches!(after_fn[name_len..].trim_start().chars().next(), Some('(' | '<'))
        {
            let start = offset + lead + (trimmed.len() - rest.len());
            found.push(start..start + 3 + name_len);
        }
        offset += line.len();
    }
    found
}

fn strip_qualifiers(mut s: &str) -> &str {
    loop {
        let before = s;
        for q in ["pub(crate) ", "pub(super) ", "pub ", "async ", "unsafe ", "const ", "extern \"C\" "] {
            s = s.strip_prefix(q).unwrap_or(s);
        }
        if s == before {
            return s;
        }
    }
}

fn ident_len(s: &str) -> Option<usize> {
    let n = s
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .count();
    (n > 0 && !s.starts_with(|c: char| c.is_ascii_digit())).then_some(n)
}

/// The balanced `{…}` that follows a signature: the first `{` after the
/// signature's parameter list, closed by its matching `}`. Range includes
/// both braces.
fn body_after(text: &str, from: usize) -> Option<Range<usize>> {
    let open = from + text[from..].find('{')?;
    // A `where` clause or return type never contains `{`; a `;` first means
    // a trait method without a body.
    if text[from..open].contains(';') {
        return None;
    }
    let close = matching_close(text, open)?;
    Some(open..close + 1)
}

/// Index of the `}` matching the `{` at `open`, skipping strings, chars and
/// comments.
fn matching_close(text: &str, open: usize) -> Option<usize> {
    let mut depth = 0_i64;
    let mut scanner = Scanner::new(&text[open..]);
    while let Some((i, c)) = scanner.next_code_char() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Statement spans inside a body: each line-aligned run that ends at a `;`
/// or a balanced `}` at depth 0 relative to the body, 1–8 lines, balanced.
fn statement_spans(text: &str, body: &Range<usize>) -> Vec<Candidate> {
    let mut spans = Vec::new();
    let mut start = body.start;
    let mut depth = 0_i64;
    let mut scanner = Scanner::new(&text[body.clone()]);
    while let Some((i, c)) = scanner.next_code_char() {
        let at = body.start + i;
        match c {
            '{' | '(' | '[' => depth += 1,
            '}' | ')' | ']' => depth -= 1,
            _ => {}
        }
        let ends_statement = depth == 0 && (c == ';' || c == '}');
        if !ends_statement {
            continue;
        }
        let end = at + 1;
        push_span(text, start..end, &mut spans);
        start = end;
    }
    spans
}

fn push_span(text: &str, raw: Range<usize>, spans: &mut Vec<Candidate>) {
    let span = trim_to_lines(text, raw);
    let content = &text[span.clone()];
    if content.trim().is_empty() || !SPAN_LINES.contains(&line_count(content)) {
        return;
    }
    if balance(content) != Some(0) {
        return;
    }
    spans.push(Candidate {
        tier: TaskTier::InFile,
        byte_range: span.clone(),
        line: line_of(text, span.start),
        doc_comment: None,
    });
}

/// Widen `raw` to whole lines: back to the previous `\n` (exclusive), forward
/// to the next `\n` (exclusive).
fn trim_to_lines(text: &str, raw: Range<usize>) -> Range<usize> {
    let start = text[..raw.start].rfind('\n').map_or(0, |i| i + 1);
    let end = text[raw.end..].find('\n').map_or(text.len(), |i| raw.end + i);
    let leading_blank = text[start..end].len() - text[start..end].trim_start().len();
    let start = start + text[start..end][..leading_blank].rfind('\n').map_or(0, |i| i + 1);
    start..end
}

fn line_count(s: &str) -> usize {
    s.trim_matches('\n').lines().count()
}

fn line_of(text: &str, at: usize) -> usize {
    text[..at].matches('\n').count() + 1
}

/// The `///` block whose last line is directly above `sig_start` (blank
/// lines break adjacency).
fn doc_comment_before(text: &str, sig_start: usize) -> Option<Range<usize>> {
    let head = &text[..sig_start];
    let mut lines: Vec<(usize, &str)> = Vec::new();
    let mut end = head.len();
    for line in head.rsplit_terminator('\n') {
        let start = end - line.len();
        if line.trim_start().starts_with("///") {
            lines.push((start, line));
            end = start.saturating_sub(1);
        } else if line.trim().is_empty() && lines.is_empty() {
            return None;
        } else {
            break;
        }
    }
    let first = lines.last()?.0;
    Some(first..head.len())
}

/// Balance of `{}`/`[]`/`()` outside strings, chars and comments. `None`
/// when a closer arrives with nothing open.
#[must_use]
pub fn balance(text: &str) -> Option<i64> {
    let mut depth = 0_i64;
    let mut scanner = Scanner::new(text);
    while let Some((_, c)) = scanner.next_code_char() {
        match c {
            '{' | '(' | '[' => depth += 1,
            '}' | ')' | ']' => {
                depth -= 1;
                if depth < 0 {
                    return None;
                }
            }
            _ => {}
        }
    }
    Some(depth)
}

/// Yields code characters only: string literals (plain, escaped, raw `r#…#`),
/// char literals, `//` and `/* */` comments are consumed whole.
struct Scanner<'a> {
    text: &'a str,
    pos: usize,
}

impl<'a> Scanner<'a> {
    fn new(text: &'a str) -> Self {
        Self { text, pos: 0 }
    }

    fn next_code_char(&mut self) -> Option<(usize, char)> {
        loop {
            let rest = &self.text[self.pos..];
            let c = rest.chars().next()?;
            let at = self.pos;
            if let Some(skip) = literal_len(rest) {
                self.pos += skip;
                continue;
            }
            self.pos += c.len_utf8();
            return Some((at, c));
        }
    }
}

/// Length of a string/char/comment literal starting at `rest`, or `None`.
fn literal_len(rest: &str) -> Option<usize> {
    if rest.starts_with("//") {
        return Some(rest.find('\n').map_or(rest.len(), |i| i));
    }
    if rest.starts_with("/*") {
        return Some(rest.find("*/").map_or(rest.len(), |i| i + 2));
    }
    if let Some(raw) = rest.strip_prefix('r') {
        let hashes = raw.chars().take_while(|c| *c == '#').count();
        if raw[hashes..].starts_with('"') {
            let close = format!("\"{}", "#".repeat(hashes));
            let body = &raw[hashes + 1..];
            return Some(1 + hashes + 1 + body.find(&close).map_or(body.len(), |i| i + close.len()));
        }
    }
    if rest.starts_with('"') {
        return Some(quoted_len(rest, '"'));
    }
    if rest.starts_with('\'') && rest.len() >= 3 && char_literal(rest) {
        return Some(quoted_len(rest, '\''));
    }
    None
}

/// `'a'`, `'\n'`, `'\u{1F600}'` — but not a lifetime `'a `.
fn char_literal(rest: &str) -> bool {
    let inner = &rest[1..];
    let escaped = inner.starts_with('\\');
    let after = if escaped { inner[1..].find('\'').map(|i| i + 2) } else { inner.chars().next().map(char::len_utf8) };
    after.is_some_and(|n| inner[n..].starts_with('\''))
}

fn quoted_len(rest: &str, quote: char) -> usize {
    let mut escaped = false;
    for (i, c) in rest.char_indices().skip(1) {
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == quote {
            return i + 1;
        }
    }
    rest.len()
}
```

- [ ] **Step 4: Run tests, then lint**

Run: `cargo test codebase::masker 2>&1 | grep -E "^test |test result"` — Expected: 4 pass.
Run: `cargo clippy --all-targets -- -D warnings` — fix any length/nesting lint by extracting a helper (never an `#[allow]`).

- [ ] **Step 5: Commit**

```bash
git add src/core/bench/mod.rs src/core/bench/codebase/
git commit -m "feat(bench): RustBraceMasker — boundary-scanned function bodies and statement spans"
```

---

### Task 3: Deterministic, stratified sampling

**Files:**
- Create: `src/core/bench/codebase/sample.rs` (replace the placeholder)

**Interfaces:**
- Consumes: `masker::{Candidate, MaskSource}`, `TaskTier`, `hash::sha256_hex`.
- Produces:
  ```rust
  pub struct FileCandidates { pub path: String /* worktree-relative */, pub candidates: Vec<Candidate> }
  pub struct Picked { pub path: String, pub candidate: Candidate, pub id: String }
  pub struct TaskSet { pub picked: Vec<Picked>, pub shortfall: Vec<String> /* "function_body: 5 of 8 requested (repo has 5 candidates)" */ }
  pub struct Quota { pub in_file: usize, pub function_body: usize }
  pub fn quota(total: u32) -> Quota                       // ceil(2n/3), remainder
  pub fn seed_from_head(head_sha: &str) -> u64            // first 8 bytes of sha256("chekov-codebase-v1:" + head)
  pub fn sample(files: Vec<FileCandidates>, quota: Quota, seed: u64) -> TaskSet
  pub fn task_id(path: &str, candidate: &Candidate) -> String   // "<tier>-<sha256(path)[..6]>-L<line>"
  pub fn task_set_hash(set: &TaskSet) -> String             // sha256 of ids joined by '\n', first 12 hex
  ```

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::{FileCandidates, Quota, quota, sample, seed_from_head, task_id, task_set_hash};
    use crate::core::bench::codebase::TaskTier;
    use crate::core::bench::codebase::masker::Candidate;

    fn cand(tier: TaskTier, line: usize) -> Candidate {
        Candidate { tier, byte_range: line * 10..line * 10 + 5, line, doc_comment: None }
    }

    fn files(n_files: usize, per_file: usize) -> Vec<FileCandidates> {
        (0..n_files)
            .map(|f| FileCandidates {
                path: format!("src/f{f}.rs"),
                candidates: (1..=per_file)
                    .flat_map(|l| [cand(TaskTier::InFile, l), cand(TaskTier::FunctionBody, 100 + l)])
                    .collect(),
            })
            .collect()
    }

    #[test]
    fn quota_is_two_thirds_in_file_rounded_up() {
        assert_eq!((quota(24).in_file, quota(24).function_body), (16, 8));
        assert_eq!((quota(12).in_file, quota(12).function_body), (8, 4));
        assert_eq!((quota(5).in_file, quota(5).function_body), (4, 1));
    }

    #[test]
    fn the_same_head_yields_the_same_set_and_a_different_head_does_not() {
        let a = sample(files(4, 10), quota(24), seed_from_head("abc123"));
        let b = sample(files(4, 10), quota(24), seed_from_head("abc123"));
        let c = sample(files(4, 10), quota(24), seed_from_head("def456"));
        let ids = |s: &super::TaskSet| s.picked.iter().map(|p| p.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(&a), ids(&b));
        assert_ne!(ids(&a), ids(&c));
        assert_eq!(task_set_hash(&a), task_set_hash(&b));
        assert_ne!(task_set_hash(&a), task_set_hash(&c));
        assert_eq!(a.picked.len(), 24);
    }

    #[test]
    fn selection_is_stratified_across_files() {
        // 8 files × plenty of candidates: 24 picks must touch every file
        // rather than draining the first.
        let set = sample(files(8, 20), quota(24), 7);
        let mut per_file = std::collections::BTreeMap::new();
        for p in &set.picked {
            *per_file.entry(p.path.clone()).or_insert(0) += 1;
        }
        assert_eq!(per_file.len(), 8, "{per_file:?}");
        assert!(per_file.values().all(|&n| n <= 4), "{per_file:?}");
    }

    #[test]
    fn a_short_tier_is_reported_not_filled_from_the_other() {
        let mut only_in_file = files(2, 3);
        for f in &mut only_in_file {
            f.candidates.retain(|c| c.tier == TaskTier::InFile);
        }
        let set = sample(only_in_file, Quota { in_file: 4, function_body: 8 }, 1);
        assert_eq!(set.picked.len(), 4);
        assert_eq!(set.shortfall, vec!["function_body: 0 of 8 requested (repo has 0 candidates)"]);
    }

    #[test]
    fn task_ids_are_stable_and_readable() {
        let id = task_id("src/lib.rs", &cand(TaskTier::FunctionBody, 42));
        assert!(id.starts_with("function_body-"), "{id}");
        assert!(id.ends_with("-L42"), "{id}");
        assert_eq!(id, task_id("src/lib.rs", &cand(TaskTier::FunctionBody, 42)));
        assert_ne!(id, task_id("src/main.rs", &cand(TaskTier::FunctionBody, 42)));
    }
}
```

- [ ] **Step 2: Run to verify failure** — `cargo test codebase::sample` → unresolved imports.

- [ ] **Step 3: Implement**

```rust
//! Deterministic, stratified task sampling: the same HEAD always yields the
//! same set, and a large file cannot dominate it.

use super::TaskTier;
use super::masker::Candidate;
use crate::core::hash::sha256_hex;

pub struct FileCandidates {
    /// Worktree-relative path, forward slashes.
    pub path: String,
    pub candidates: Vec<Candidate>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picked {
    pub path: String,
    pub candidate: Candidate,
    pub id: String,
}

#[derive(Debug, Default)]
pub struct TaskSet {
    pub picked: Vec<Picked>,
    /// "function_body: 5 of 8 requested (repo has 5 candidates)" — printed,
    /// never filled from the other tier.
    pub shortfall: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quota {
    pub in_file: usize,
    pub function_body: usize,
}

/// Two-thirds `in_file` rounded up, the remainder `function_body`.
#[must_use]
pub fn quota(total: u32) -> Quota {
    let total = usize::try_from(total).unwrap_or(0);
    let in_file = (total * 2).div_ceil(3);
    Quota { in_file, function_body: total - in_file }
}

/// The first 8 bytes of `sha256("chekov-codebase-v1:" + head)`.
#[must_use]
pub fn seed_from_head(head_sha: &str) -> u64 {
    let hex = sha256_hex(format!("chekov-codebase-v1:{head_sha}").as_bytes());
    u64::from_str_radix(&hex[..16], 16).unwrap_or(0x9E37_79B9_7F4A_7C15)
}

/// xorshift64* — small, fast, and part of the task-set identity (changing it
/// changes every set, which the corpus id records).
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(if seed == 0 { 0x9E37_79B9_7F4A_7C15 } else { seed })
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % u64::try_from(n.max(1)).unwrap_or(1)).unwrap_or(0)
    }

    fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = self.below(i + 1);
            items.swap(i, j);
        }
    }
}

#[must_use]
pub fn sample(mut files: Vec<FileCandidates>, quota: Quota, seed: u64) -> TaskSet {
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let mut rng = Rng::new(seed);
    let mut set = TaskSet::default();
    for (tier, want) in [
        (TaskTier::InFile, quota.in_file),
        (TaskTier::FunctionBody, quota.function_body),
    ] {
        let mut lanes = per_file_lanes(&files, tier, &mut rng);
        let picked = round_robin(&mut lanes, want, &mut rng);
        let have: usize = files
            .iter()
            .map(|f| f.candidates.iter().filter(|c| c.tier == tier).count())
            .sum();
        if picked.len() < want {
            set.shortfall.push(format!(
                "{}: {} of {want} requested (repo has {have} candidates)",
                tier.label(),
                picked.len()
            ));
        }
        set.picked.extend(picked);
    }
    set
}

/// One shuffled lane of candidates per file (files in seeded order).
fn per_file_lanes(files: &[FileCandidates], tier: TaskTier, rng: &mut Rng) -> Vec<(String, Vec<Candidate>)> {
    let mut lanes: Vec<(String, Vec<Candidate>)> = files
        .iter()
        .map(|f| {
            let mut lane: Vec<Candidate> = f.candidates.iter().filter(|c| c.tier == tier).cloned().collect();
            rng.shuffle(&mut lane);
            (f.path.clone(), lane)
        })
        .filter(|(_, lane)| !lane.is_empty())
        .collect();
    rng.shuffle(&mut lanes);
    lanes
}

/// Take one candidate per file per pass until `want` are picked or every
/// lane is empty.
fn round_robin(lanes: &mut [(String, Vec<Candidate>)], want: usize, _rng: &mut Rng) -> Vec<Picked> {
    let mut picked = Vec::new();
    while picked.len() < want {
        let before = picked.len();
        for (path, lane) in lanes.iter_mut() {
            if picked.len() == want {
                break;
            }
            if let Some(candidate) = lane.pop() {
                let id = task_id(path, &candidate);
                picked.push(Picked { path: path.clone(), candidate, id });
            }
        }
        if picked.len() == before {
            break;
        }
    }
    picked
}

/// `<tier>-<sha256(path)[..6]>-L<line>` — stable across runs on one HEAD.
#[must_use]
pub fn task_id(path: &str, candidate: &Candidate) -> String {
    let digest = sha256_hex(path.as_bytes());
    format!("{}-{}-L{}", candidate.tier.label(), &digest[..6], candidate.line)
}

/// SHA-256 over the ids in order, first 12 hex — the set's identity.
#[must_use]
pub fn task_set_hash(set: &TaskSet) -> String {
    let ids: Vec<&str> = set.picked.iter().map(|p| p.id.as_str()).collect();
    sha256_hex(ids.join("\n").as_bytes())[..12].to_owned()
}
```

- [ ] **Step 4: Run tests and clippy** — `cargo test codebase::sample` → 5 pass; clippy clean (drop the unused `_rng` parameter from `round_robin` if clippy objects — it exists only to keep the call shape; remove it and the argument).

- [ ] **Step 5: Commit** — `git commit -m "feat(bench): deterministic stratified sampling of codebase tasks"`

---

### Task 4: Context assembly and the doc-comment filter

**Files:**
- Create: `src/core/bench/codebase/filter.rs`
- Modify: `src/core/bench/codebase/mod.rs` (add `CodebaseTask`, `Excluded`)

**Interfaces:**
- Produces (`mod.rs`):
  ```rust
  #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
  pub struct Excluded { pub doc_comment: u8, pub cross_file: String }   // cross_file = "n/a: same-file"
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct CodebaseTask { pub id: String, pub tier: TaskTier, pub file: String, pub line: usize, pub gold: String, pub prefix: String, pub suffix: String, pub excluded: Excluded }
  ```
- Produces (`filter.rs`): `pub fn assemble(path: &str, text: &str, picked: &sample::Picked) -> CodebaseTask`.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::assemble;
    use crate::core::bench::codebase::masker::{MaskSource, RustBraceMasker};
    use crate::core::bench::codebase::sample::{Picked, task_id};
    use crate::core::bench::codebase::TaskTier;

    const SRC: &str = "/// Doc line.\npub fn f(a: i32) -> i32 {\n    let b = a + 1;\n    let c = b * 2;\n    c\n}\n";

    fn pick(tier: TaskTier) -> Picked {
        let c = RustBraceMasker
            .candidates(SRC)
            .into_iter()
            .find(|c| c.tier == tier)
            .expect("candidate");
        Picked { path: "src/x.rs".into(), id: task_id("src/x.rs", &c), candidate: c }
    }

    #[test]
    fn a_function_body_task_strips_the_doc_comment_and_counts_it() {
        let task = assemble("src/x.rs", SRC, &pick(TaskTier::FunctionBody));
        assert!(!task.prefix.contains("Doc line"), "{:?}", task.prefix);
        assert!(task.prefix.ends_with("-> i32 {"), "{:?}", task.prefix);
        assert_eq!(task.suffix, "\n}\n");
        assert_eq!(task.gold.trim(), "let b = a + 1;\n    let c = b * 2;\n    c");
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
```

- [ ] **Step 2: Run to verify failure.**

- [ ] **Step 3: Implement**

`mod.rs` additions:

```rust
/// What the leakage filter removed from this task's context, per rule. Slice
/// A has no cross-file context, and says so rather than claiming a count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Excluded {
    pub doc_comment: u8,
    pub cross_file: String,
}

/// One assembled task: what the model sees, what was hidden, and the answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodebaseTask {
    pub id: String,
    pub tier: TaskTier,
    pub file: String,
    pub line: usize,
    pub gold: String,
    pub prefix: String,
    pub suffix: String,
    pub excluded: Excluded,
}
```

`filter.rs`:

```rust
//! Same-file context with the leakage filter's rule (c): the doc comment
//! directly above a masked function body reveals it, so it is cut from the
//! prefix and the cut is counted. Rules (b) and (d) govern cross-file
//! context, which slice A does not build — recorded as not applicable.

use super::sample::Picked;
use super::{CodebaseTask, Excluded, TaskTier};

pub const NO_CROSS_FILE: &str = "n/a: same-file";

#[must_use]
pub fn assemble(path: &str, text: &str, picked: &Picked) -> CodebaseTask {
    let c = &picked.candidate;
    let mut prefix = text[..c.byte_range.start].to_owned();
    let mut doc_comment = 0;
    if c.tier == TaskTier::FunctionBody
        && let Some(doc) = &c.doc_comment
    {
        prefix = format!("{}{}", &text[..doc.start], &text[doc.end..c.byte_range.start]);
        doc_comment = 1;
    }
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
```

(If the masker's `doc_comment` range ends with a trailing `\n` the prefix keeps a blank line; that is fine and the test's `ends_with("-> i32 {")` holds because the range runs to the signature start.)

- [ ] **Step 4: Run tests and clippy.**

- [ ] **Step 5: Commit** — `git commit -m "feat(bench): codebase task assembly with the doc-comment leakage rule"`

---

### Task 5: The scoring ladder, tiers 1–5

**Files:**
- Create: `src/core/bench/codebase/ladder.rs`

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
  #[serde(rename_all = "snake_case")]
  pub enum Tier { Exact, EditSim, IdentF1, Parse, Symbols, Compile, Test }
  impl Tier { pub const fn label(self) -> &'static str }   // "exact" "edit_sim" "ident_f1" "parse" "symbols" "compile" "test"
  #[derive(Debug, Clone, Copy, PartialEq)]
  pub enum Score { Value(f64), Skipped(&'static str) }
  pub struct Symbols(pub std::collections::BTreeSet<String>);
  pub fn repo_symbols(files: &[(String, String)]) -> Symbols         // (path, text) pairs → declaration names + field/variant names
  pub fn file_use_symbols(text: &str) -> Vec<String>                 // last segment of each `use` path
  pub struct Scored<'a> { pub task: &'a CodebaseTask, pub prediction: &'a str, pub symbols: &'a Symbols }
  pub fn score_all(s: &Scored) -> Vec<(Tier, Score)>                 // all 7, in order; 6–7 always Skipped("slice B (--allow-exec)")
  pub fn exact(gold: &str, pred: &str) -> f64
  pub fn edit_sim(gold: &str, pred: &str) -> f64
  pub fn ident_f1(gold: &str, pred: &str) -> f64
  pub fn parse(prefix: &str, pred: &str, suffix: &str) -> f64
  pub fn symbols(pred: &str, gold: &str, known: &Symbols, file_uses: &[String]) -> f64
  pub fn identifiers(text: &str) -> Vec<String>                      // minus Rust keywords, deduplicated, in order
  ```
  Tiers 1–2 return `Score::Skipped("function_body: tiers 1-2 punish valid alternatives")` on `FunctionBody` tasks.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::{Score, Scored, Symbols, Tier, edit_sim, exact, ident_f1, identifiers, parse, repo_symbols, score_all, symbols};
    use crate::core::bench::codebase::{CodebaseTask, Excluded, TaskTier};

    #[test]
    fn exact_ignores_whitespace_only_differences() {
        assert_eq!(exact("let x = 1;", "let  x =\n1;"), 1.0);
        assert_eq!(exact("let x = 1;", "let x = 2;"), 0.0);
    }

    #[test]
    fn edit_similarity_is_one_minus_normalised_levenshtein() {
        assert_eq!(edit_sim("abc", "abc"), 1.0);
        assert!((edit_sim("kitten", "sitting") - (1.0 - 3.0 / 7.0)).abs() < 1e-9);
        assert_eq!(edit_sim("", ""), 1.0);
    }

    #[test]
    fn identifier_f1_catches_a_wrong_api() {
        assert!((ident_f1("self.log.apply_entry(e)", "self.log.append_entry(e)") - 2.0 / 3.0).abs() < 1e-9);
        assert_eq!(ident_f1("fn x() {}", "fn x() {}"), 1.0);
        assert_eq!(identifiers("let mut x = foo(y); return"), vec!["x", "foo", "y"], "keywords are not identifiers");
    }

    #[test]
    fn parse_gate_is_balance_of_the_whole_file() {
        assert_eq!(parse("fn f() {", "let a = [1];", "}"), 1.0);
        assert_eq!(parse("fn f() {", "let a = [1;", "}"), 0.0);
    }

    #[test]
    fn symbols_scores_a_fabricated_identifier_down_and_a_gold_binding_up() {
        let known = repo_symbols(&[(
            "src/a.rs".into(),
            "pub struct Ledger { balance: i64 }\npub fn apply_entry(l: &Ledger) {}\nenum E { Credit, Debit }".into(),
        )]);
        assert!(known.0.contains("apply_entry") && known.0.contains("balance") && known.0.contains("Credit"));
        let uses = vec!["HashMap".to_owned()];
        assert_eq!(symbols("apply_entry(l); let n = balance;", "", &known, &uses), 1.0);
        assert_eq!(symbols("frobnicate(l)", "", &known, &uses), 0.0);
        assert_eq!(symbols("let total = 1; total", "let total = 1;", &known, &uses), 1.0, "gold-introduced binding");
        assert_eq!(symbols("HashMap::new()", "", &known, &uses), 1.0, "a `use` target exists");
        assert_eq!(symbols("Some(1)", "", &known, &uses), 1.0, "prelude");
    }

    fn task(tier: TaskTier) -> CodebaseTask {
        CodebaseTask {
            id: "t".into(),
            tier,
            file: "src/a.rs".into(),
            line: 1,
            gold: "let a = 1;".into(),
            prefix: "fn f() {\n".into(),
            suffix: "\n}\n".into(),
            excluded: Excluded { doc_comment: 0, cross_file: "n/a: same-file".into() },
        }
    }

    #[test]
    fn all_seven_tiers_are_reported_and_the_exec_tiers_are_skipped() {
        let known = Symbols(Default::default());
        let t = task(TaskTier::InFile);
        let scores = score_all(&Scored { task: &t, prediction: "let a = 1;", symbols: &known });
        assert_eq!(scores.len(), 7);
        assert!(matches!(scores[0], (Tier::Exact, Score::Value(v)) if v == 1.0));
        assert!(matches!(scores[5], (Tier::Compile, Score::Skipped(_))));
        assert!(matches!(scores[6], (Tier::Test, Score::Skipped(_))));
        let body = task(TaskTier::FunctionBody);
        let scores = score_all(&Scored { task: &body, prediction: "let a = 1;", symbols: &known });
        assert!(matches!(scores[0], (Tier::Exact, Score::Skipped(_))), "tiers 1-2 skip on bodies");
        assert!(matches!(scores[2], (Tier::IdentF1, Score::Value(_))));
    }
}
```

- [ ] **Step 2: Run to verify failure.**

- [ ] **Step 3: Implement**

```rust
//! The deterministic scoring ladder, tiers 1–5 (spec §8): cheapest to
//! strongest, every tier reported separately, never collapsed. Tiers 6–7
//! (compile, covering test) are slice B and report `skipped`, never pass.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::masker::balance;
use super::{CodebaseTask, TaskTier};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    Exact,
    EditSim,
    IdentF1,
    Parse,
    Symbols,
    Compile,
    Test,
}

impl Tier {
    pub const ALL: [Self; 7] = [
        Self::Exact,
        Self::EditSim,
        Self::IdentF1,
        Self::Parse,
        Self::Symbols,
        Self::Compile,
        Self::Test,
    ];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::EditSim => "edit_sim",
            Self::IdentF1 => "ident_f1",
            Self::Parse => "parse",
            Self::Symbols => "symbols",
            Self::Compile => "compile",
            Self::Test => "test",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Score {
    Value(f64),
    Skipped(&'static str),
}

const EXEC_SKIPPED: &str = "slice B (--allow-exec)";
const BODY_SKIPPED: &str = "function_body: tiers 1-2 punish valid alternatives";

/// Rust keywords — never identifiers.
const KEYWORDS: [&str; 52] = [
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
    "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod",
    "move", "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super",
    "trait", "true", "type", "unsafe", "use", "where", "while", "abstract", "become",
    "box", "do", "final", "macro", "override", "priv", "typeof", "unsized", "virtual",
    "yield", "try", "gen",
];

/// Names any Rust program may use without declaring: the prelude, common
/// std types and the methods the ladder would otherwise call fabricated.
const PRELUDE: [&str; 60] = [
    "Some", "None", "Ok", "Err", "Option", "Result", "Vec", "String", "Box", "Rc", "Arc",
    "str", "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128",
    "isize", "f32", "f64", "bool", "char", "format", "println", "eprintln", "vec",
    "iter", "into_iter", "map", "filter", "collect", "unwrap", "unwrap_or", "expect",
    "len", "is_empty", "push", "clone", "to_owned", "to_string", "as_str", "as_ref",
    "and_then", "map_err", "ok_or", "get", "insert", "contains", "join", "trim", "lines",
    "chars", "new", "default",
];

#[derive(Debug, Clone, Default)]
pub struct Symbols(pub BTreeSet<String>);

pub struct Scored<'a> {
    pub task: &'a CodebaseTask,
    pub prediction: &'a str,
    pub symbols: &'a Symbols,
}

#[must_use]
pub fn score_all(s: &Scored) -> Vec<(Tier, Score)> {
    let t = s.task;
    let file_uses = file_use_symbols(&format!("{}{}", t.prefix, t.suffix));
    let line_level = t.tier == TaskTier::InFile;
    let gated = |v: f64| if line_level { Score::Value(v) } else { Score::Skipped(BODY_SKIPPED) };
    vec![
        (Tier::Exact, gated(exact(&t.gold, s.prediction))),
        (Tier::EditSim, gated(edit_sim(&t.gold, s.prediction))),
        (Tier::IdentF1, Score::Value(ident_f1(&t.gold, s.prediction))),
        (Tier::Parse, Score::Value(parse(&t.prefix, s.prediction, &t.suffix))),
        (Tier::Symbols, Score::Value(symbols(s.prediction, &t.gold, s.symbols, &file_uses))),
        (Tier::Compile, Score::Skipped(EXEC_SKIPPED)),
        (Tier::Test, Score::Skipped(EXEC_SKIPPED)),
    ]
}

fn normalise(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[must_use]
pub fn exact(gold: &str, pred: &str) -> f64 {
    if normalise(gold) == normalise(pred) { 1.0 } else { 0.0 }
}

/// `1 − lev / max(len)` over whitespace-normalised text; two-row DP.
#[must_use]
pub fn edit_sim(gold: &str, pred: &str) -> f64 {
    let (a, b): (Vec<char>, Vec<char>) = (normalise(gold).chars().collect(), normalise(pred).chars().collect());
    let longest = a.len().max(b.len());
    if longest == 0 {
        return 1.0;
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    1.0 - as_f64(prev[b.len()]) / as_f64(longest)
}

fn as_f64(n: usize) -> f64 {
    u32::try_from(n).map_or(f64::MAX, f64::from)
}

/// `[A-Za-z_][A-Za-z0-9_]*` tokens minus keywords, deduplicated, in order.
#[must_use]
pub fn identifiers(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    for c in text.chars().chain(std::iter::once(' ')) {
        let ident_char = c.is_ascii_alphanumeric() || c == '_';
        if ident_char {
            cur.push(c);
            continue;
        }
        let word = std::mem::take(&mut cur);
        let starts_ok = word.starts_with(|w: char| w.is_ascii_alphabetic() || w == '_');
        if starts_ok && !KEYWORDS.contains(&word.as_str()) && !out.contains(&word) {
            out.push(word);
        }
    }
    out
}

#[must_use]
pub fn ident_f1(gold: &str, pred: &str) -> f64 {
    let g: BTreeSet<String> = identifiers(gold).into_iter().collect();
    let p: BTreeSet<String> = identifiers(pred).into_iter().collect();
    if g.is_empty() && p.is_empty() {
        return 1.0;
    }
    let overlap = as_f64(g.intersection(&p).count());
    if overlap == 0.0 {
        return 0.0;
    }
    let (precision, recall) = (overlap / as_f64(p.len()), overlap / as_f64(g.len()));
    2.0 * precision * recall / (precision + recall)
}

#[must_use]
pub fn parse(prefix: &str, pred: &str, suffix: &str) -> f64 {
    if balance(&format!("{prefix}{pred}{suffix}")) == Some(0) { 1.0 } else { 0.0 }
}

/// Fraction of the prediction's identifiers that exist: in the repo's
/// declarations, the file's `use` targets, the prelude, or the gold's own
/// bindings. Empty prediction scores 0 — it referenced nothing that exists.
#[must_use]
pub fn symbols(pred: &str, gold: &str, known: &Symbols, file_uses: &[String]) -> f64 {
    let idents = identifiers(pred);
    if idents.is_empty() {
        return 0.0;
    }
    let gold_bindings = identifiers(gold);
    let exists = |id: &String| {
        known.0.contains(id)
            || file_uses.contains(id)
            || PRELUDE.contains(&id.as_str())
            || gold_bindings.contains(id)
    };
    as_f64(idents.iter().filter(|id| exists(id)).count()) / as_f64(idents.len())
}

/// Declaration names across the repo: `fn`/`struct`/`enum`/`trait`/`type`/
/// `const`/`static`/`mod` names, struct fields (`name: Type,`), enum
/// variants (a capitalised identifier on its own line inside `enum {}`).
#[must_use]
pub fn repo_symbols(files: &[(String, String)]) -> Symbols {
    let mut set = BTreeSet::new();
    for (_, text) in files {
        for line in text.lines() {
            collect_declarations(line, &mut set);
        }
    }
    Symbols(set)
}

fn collect_declarations(line: &str, set: &mut BTreeSet<String>) {
    let words: Vec<&str> = line.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).filter(|w| !w.is_empty()).collect();
    for (i, w) in words.iter().enumerate() {
        if matches!(*w, "fn" | "struct" | "enum" | "trait" | "type" | "const" | "static" | "mod")
            && let Some(name) = words.get(i + 1)
        {
            set.insert((*name).to_owned());
        }
    }
    let trimmed = line.trim();
    // `name: Type,` — a struct field; `Variant,` / `Variant(` / `Variant {` — an enum variant.
    if let Some((name, _)) = trimmed.split_once(':')
        && !name.contains(' ')
        && !name.is_empty()
    {
        set.insert(name.trim_start_matches("pub ").to_owned());
    }
    let variant: String = trimmed.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
    if variant.starts_with(|c: char| c.is_ascii_uppercase()) && trimmed[variant.len()..].starts_with([',', '(', '{', ' ']) {
        set.insert(variant);
    }
}

/// The last path segment of each `use` line: `use std::collections::HashMap;`
/// → `HashMap`; `use a::{B, C};` → `B`, `C`.
#[must_use]
pub fn file_use_symbols(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| l.trim().strip_prefix("use "))
        .flat_map(|rest| {
            rest.trim_end_matches(';')
                .split(['{', '}', ','])
                .map(|seg| seg.rsplit("::").next().unwrap_or(seg).trim().to_owned())
                .filter(|s| !s.is_empty() && s != "self" && s != "*")
                .collect::<Vec<_>>()
        })
        .collect()
}
```

The variant rule intentionally over-approximates (any capitalised word at line start followed by `,`/`(`/`{`/space). That leans tier 5 toward "exists", which is the safe direction — a fabricated identifier scoring as existing is a missed catch, not a false accusation.

- [ ] **Step 4: Run tests and clippy** — `cargo test codebase::ladder` → 6 pass. Extract helpers if any function exceeds 40 lines (`collect_declarations` is close: split the field/variant part into `collect_members(trimmed, set)`).

- [ ] **Step 5: Commit** — `git commit -m "feat(bench): the codebase scoring ladder, tiers 1-5"`

---

### Task 6: The infill crossing

**Files:**
- Modify: `src/core/bench/runner.rs` (after `cross_streaming`; tests at the bottom of its tests module)

**Interfaces:**
- Consumes: `ProbeWire`, `JsonRequest`, `read_timings`, `ChekovError::UpstreamRefused`.
- Produces:
  ```rust
  pub struct InfillTask<'a> { pub prefix: &'a str, pub suffix: &'a str, pub gold_lines: usize }
  pub enum InfillOutcome { Answered(ProbeArtifact), Unsupported(String) }
  pub fn cross_infill(wire: &ProbeWire, task: &InfillTask) -> Result<InfillOutcome, ChekovError>
  ```
  `ProbeArtifact.anthropic_body` carries the raw `content` string for this crossing (the field name is historical; the doc comment on `cross_infill` says so).

- [ ] **Step 1: Write the failing tests** (runner.rs tests module, after `a_transport_dispatches_to_its_door`)

```rust
    #[test]
    fn an_infill_crossing_posts_prefix_suffix_and_pins_and_returns_the_raw_fill() {
        let http = CannedUpstream::new(
            serde_json::json!({
                "content": "    a + b\n",
                "tokens_predicted": 6,
                "timings": final_frame()["timings"]
            })
            .to_string(),
        );
        let facade = ClaudeFacade::new("local-model");
        let up = fake_upstream();
        let task = super::InfillTask { prefix: "fn add(a: i32, b: i32) -> i32 {\n", suffix: "\n}\n", gold_lines: 1 };
        let outcome = super::cross_infill(&wire(&http, &facade, &up), &task).expect("crosses");
        let super::InfillOutcome::Answered(artifact) = outcome else {
            panic!("a 200 with content is an answer");
        };
        assert_eq!(artifact.anthropic_body, "    a + b\n");
        assert_eq!(artifact.timings.cache_n, 512);
        let sent = sent(&http);
        assert_eq!(sent["input_prefix"], "fn add(a: i32, b: i32) -> i32 {\n");
        assert_eq!(sent["input_suffix"], "\n}\n");
        assert_eq!(sent["prompt"], "");
        assert_eq!(sent["input_extra"], serde_json::json!([]));
        assert_eq!(sent["temperature"], 0);
        assert_eq!(sent["top_k"], 1);
        assert_eq!(sent["seed"], 42);
        assert_eq!(sent["n_predict"], 64, "max(64, 3*lines*12)");
        assert!(http.url_seen.borrow().as_deref().unwrap_or("").ends_with("/infill"));
    }

    #[test]
    fn a_model_without_fim_tokens_is_a_capability_not_a_failure() {
        struct Refusing;
        impl HttpClient for Refusing {
            fn get(&self, _url: &str) -> Result<String, ChekovError> { unreachable!() }
            fn post_json(&self, _req: &JsonRequest) -> Result<String, ChekovError> {
                Err(ChekovError::UpstreamRefused {
                    url: "http://fake/infill".into(),
                    status: 400,
                    reason: "infill is not supported by this model: missing FIM tokens".into(),
                })
            }
        }
        let facade = ClaudeFacade::new("local-model");
        let up = fake_upstream();
        let w = super::ProbeWire { http: &Refusing, facade: &facade, upstream: &up, pins: super::SamplingPins { seed: 42 } };
        let task = super::InfillTask { prefix: "x", suffix: "y", gold_lines: 1 };
        match super::cross_infill(&w, &task).expect("a refusal naming infill is an outcome") {
            super::InfillOutcome::Unsupported(reason) => assert!(reason.contains("FIM tokens"), "{reason}"),
            super::InfillOutcome::Answered(_) => panic!("must not be graded"),
        }
    }
```

`CannedUpstream` needs a `url_seen: RefCell<Option<String>>` field set in `post_json` (add it beside `bearer_seen`; initialise `RefCell::new(None)` in `new`).

- [ ] **Step 2: Run to verify failure.**

- [ ] **Step 3: Implement** (after `cross_streaming`)

```rust
/// One infill task on the wire: the file before and after the mask, and the
/// gold's line count (to bound `n_predict`).
pub struct InfillTask<'a> {
    pub prefix: &'a str,
    pub suffix: &'a str,
    pub gold_lines: usize,
}

/// What `/infill` said: a fill, or that this model cannot infill at all —
/// a capability, recorded N/A, never a zero (spec §8).
pub enum InfillOutcome {
    Answered(ProbeArtifact),
    Unsupported(String),
}

/// `POST /infill` with the same pins as every probe. llama.cpp resolves the
/// FIM sentinels from GGUF metadata; chekov never writes them. The artifact's
/// `anthropic_body` carries the raw `content` — there is no Anthropic door
/// for infill, and the graders read it as text.
pub fn cross_infill(wire: &ProbeWire, task: &InfillTask) -> Result<InfillOutcome, ChekovError> {
    let n_predict = (task.gold_lines * 36).max(64);
    let body = serde_json::json!({
        "input_prefix": task.prefix,
        "input_suffix": task.suffix,
        "prompt": "",
        "input_extra": [],
        "n_predict": n_predict,
        "temperature": 0,
        "top_k": 1,
        "seed": wire.pins.seed,
    });
    let posted = wire.http.post_json(&JsonRequest {
        url: format!("{}/infill", wire.upstream.base_url),
        body: body.to_string(),
        bearer: Some(wire.upstream.api_key.clone()),
    });
    let upstream_body = match posted {
        Ok(text) => text,
        Err(ChekovError::UpstreamRefused { reason, .. }) if reason.to_lowercase().contains("infill") => {
            return Ok(InfillOutcome::Unsupported(reason));
        }
        Err(e) => return Err(e),
    };
    let timings = read_timings(&upstream_body)?;
    let parsed: Value = serde_json::from_str(&upstream_body).map_err(|e| ChekovError::ProxyBadRequest {
        reason: format!("/infill reply is not JSON: {e}"),
    })?;
    let content = parsed.get("content").and_then(Value::as_str).unwrap_or_default().to_owned();
    Ok(InfillOutcome::Answered(ProbeArtifact {
        anthropic_body: content,
        timings,
    }))
}
```

- [ ] **Step 4: Run tests and clippy.**

- [ ] **Step 5: Commit** — `git commit -m "feat(bench): the /infill crossing — a model without FIM is a capability, not a zero"`

---

### Task 7: Storage row and the report block

**Files:**
- Modify: `src/core/bench/store.rs` (`TaskRow`, `Task`, `append`, a `codebase_block` in `render_run`, tests)

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Clone, Serialize, Deserialize)] #[serde(deny_unknown_fields)]
  pub struct CodebaseRow { pub tier: TaskTier, pub file: String, pub line: usize, pub label: String, pub gold: String, pub prediction: String, pub prefix: String, pub suffix: String, pub excluded: Excluded }
  // TaskRow and Task gain `#[serde(default, skip_serializing_if = "Option::is_none")] pub codebase: Option<CodebaseRow>`
  pub fn render_codebase(log: &RunLog, symbols: &ladder::Symbols) -> String   // the block; "" when no codebase rows
  ```
  `render_run` calls `render_codebase(log, &ladder::Symbols::default())` — the command layer (Task 9) renders with the real symbol set after the run; the stored `render_run` output for older runs shows tier 5 against an empty set and says so with `symbols n/a (rendered without the repo)`. **Simpler and honest:** store the repo symbol set once per run in `stamp.json`? No — it can be megabytes. Decision: `CodebaseRow` stores `symbols_score: f64` computed at run time (tier 5 needs the worktree, which is gone after the run), while tiers 1–4 are recomputed on read from gold/prediction/prefix/suffix. The report says `symbols (scored at run time)`.

- [ ] **Step 1: Write the failing tests** (store.rs tests, before `a_hot_cache_is_visible_in_the_rendering`)

```rust
    fn codebase_task(id: &str, tier: TaskTier, gold: &str, prediction: &str) -> Task {
        Task {
            suite: "codebase".into(),
            task_id: id.into(),
            measure: measure(&[20.0, 20.0]),
            grade: None,
            transport: Transport::Buffered,
            codebase: Some(CodebaseRow {
                tier,
                file: "src/a.rs".into(),
                line: 7,
                label: "boundary-scanned (not AST)".into(),
                gold: gold.into(),
                prediction: prediction.into(),
                prefix: "fn f() {\n".into(),
                suffix: "\n}\n".into(),
                excluded: Excluded { doc_comment: 0, cross_file: "n/a: same-file".into() },
                symbols_score: 1.0,
            }),
        }
    }

    #[test]
    fn codebase_rows_round_trip_and_the_block_recomputes_tiers_from_stored_text() {
        let eval = scratch("codebase-rows");
        let mut writer = RunWriter::create(&eval, "r10-model", &head()).expect("create");
        writer.append(codebase_task("in_file-abc123-L7", TaskTier::InFile, "let a = 1;", "let a = 1;")).expect("append");
        writer.append(codebase_task("in_file-abc123-L9", TaskTier::InFile, "let b = 2;", "let c = 3;")).expect("append");
        writer.append(codebase_task("function_body-abc123-L20", TaskTier::FunctionBody, "let x = 1;\n    x", "x")).expect("append");
        let log = RunLog::load(writer.dir()).expect("load");
        assert_eq!(log.rows[0].codebase.as_ref().map(|c| c.prediction.as_str()), Some("let a = 1;"));
        let rendered = render_run(&log);
        assert!(rendered.contains("codebase     3 tasks (2 in_file, 1 function_body) — boundary-scanned (not AST); context: same-file"), "{rendered}");
        assert!(rendered.contains("in_file        exact 0.50   edit_sim"), "{rendered}");
        assert!(rendered.contains("symbols 1.00 (scored at run time)   (n=2)"), "{rendered}");
        assert!(rendered.contains("function_body  ident_f1"), "{rendered}");
        assert!(rendered.contains("tiers 6-7 skipped: slice B (--allow-exec)"), "{rendered}");
    }

    #[test]
    fn an_infill_unsupported_run_reports_na_not_zero() {
        let eval = scratch("codebase-na");
        let mut writer = RunWriter::create(&eval, "r11-model", &head()).expect("create");
        let mut task = codebase_task("in_file-abc123-L7", TaskTier::InFile, "let a = 1;", "");
        task.grade = Some(GradeRow::unavailable("infill is not supported by this model".into()));
        writer.append(task).expect("append");
        let rendered = render_run(&RunLog::load(writer.dir()).expect("load"));
        assert!(rendered.contains("codebase     N/A — infill unsupported by this model (infill is not supported"), "{rendered}");
        assert!(!rendered.contains("exact 0.00"), "{rendered}");
    }

    #[test]
    fn a_row_written_before_the_codebase_field_loads() {
        let line = r#"{"schema":1,"run_id":"r","seq":0,"suite":"tool_emit","task_id":"te-001","measure":{"prompt_n":4,"decode_samples":[1.0],"prefill_samples":[1.0],"warmup_dropped":0}}"#;
        let row: TaskRow = serde_json::from_str(line).expect("loads");
        assert!(row.codebase.is_none());
    }
```

Add `use crate::core::bench::codebase::{Excluded, TaskTier};` and `CodebaseRow` to the tests' `use super::{…}` line.

- [ ] **Step 2: Run to verify failure.**

- [ ] **Step 3: Implement**

`TaskRow` and `Task` each gain (after `grade`):

```rust
    /// Present on `codebase` rows only: what the model saw, what it answered,
    /// and the gold. Tiers 1–4 are recomputed from these on read; tier 5
    /// needs the worktree and is scored at run time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codebase: Option<CodebaseRow>,
```

`append` copies `codebase: task.codebase`. Every existing `Task { … }` literal in tests and in `capability.rs` gains `codebase: None` (the compiler lists them; add the field to each).

The row type (near `GradeRow`):

```rust
/// A codebase task's record (spec §8, slice A). Raw text in, scores out.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodebaseRow {
    pub tier: TaskTier,
    pub file: String,
    pub line: usize,
    pub label: String,
    pub gold: String,
    pub prediction: String,
    pub prefix: String,
    pub suffix: String,
    pub excluded: Excluded,
    /// Tier 5 against the worktree's symbol set, scored at run time.
    pub symbols_score: f64,
}
```

with `use crate::core::bench::codebase::{Excluded, TaskTier};` at the top of store.rs.

The block — in `render_run`, after `suite_summaries`: `out.push_str(&render_codebase(log));`

```rust
/// The codebase block: counts and labels, then one line per tier group
/// with the mean of every tier that has a value.
#[must_use]
pub fn render_codebase(log: &RunLog) -> String {
    use crate::core::bench::codebase::TaskTier;
    let rows: Vec<&TaskRow> = rows_of(log, "codebase").filter(|r| r.codebase.is_some()).collect();
    if rows.is_empty() {
        return String::new();
    }
    if let Some(reason) = rows.iter().find(|r| is_unavailable(r)).and_then(|r| r.grade.as_ref()?.reason.clone()) {
        return format!("codebase     N/A — infill unsupported by this model ({reason})\n");
    }
    let count = |tier: TaskTier| rows.iter().filter(|r| r.codebase.as_ref().is_some_and(|c| c.tier == tier)).count();
    let mut out = format!(
        "codebase     {} tasks ({} in_file, {} function_body) — {}; context: same-file\n",
        rows.len(),
        count(TaskTier::InFile),
        count(TaskTier::FunctionBody),
        crate::core::bench::codebase::MASK_LABEL,
    );
    for tier in [TaskTier::InFile, TaskTier::FunctionBody] {
        out.push_str(&tier_line(&rows, tier));
    }
    out.push_str("             tiers 6-7 skipped: slice B (--allow-exec)\n");
    out
}

fn tier_line(rows: &[&TaskRow], tier: TaskTier) -> String {
    use crate::core::bench::codebase::ladder::{self, Score, Tier};
    let group: Vec<&CodebaseRow> = rows.iter().filter_map(|r| r.codebase.as_ref()).filter(|c| c.tier == tier).collect();
    if group.is_empty() {
        return String::new();
    }
    let mut cells = Vec::new();
    for t in [Tier::Exact, Tier::EditSim, Tier::IdentF1, Tier::Parse] {
        let values: Vec<f64> = group.iter().filter_map(|c| match recompute(c, t) { Score::Value(v) => Some(v), Score::Skipped(_) => None }).collect();
        if !values.is_empty() {
            cells.push(format!("{} {:.2}", t.label(), values.iter().sum::<f64>() / as_f64(values.len())));
        }
    }
    let symbols_mean = group.iter().map(|c| c.symbols_score).sum::<f64>() / as_f64(group.len());
    cells.push(format!("symbols {symbols_mean:.2} (scored at run time)"));
    format!("             {:<14} {}   (n={})\n", tier.label(), cells.join("   "), group.len())
}

/// Tiers 1–4 from the stored text — a stored score can never drift.
fn recompute(c: &CodebaseRow, tier: crate::core::bench::codebase::ladder::Tier) -> crate::core::bench::codebase::ladder::Score {
    use crate::core::bench::codebase::ladder::{self, Score, Tier};
    use crate::core::bench::codebase::TaskTier;
    let line_level = c.tier == TaskTier::InFile;
    match tier {
        Tier::Exact if line_level => Score::Value(ladder::exact(&c.gold, &c.prediction)),
        Tier::EditSim if line_level => Score::Value(ladder::edit_sim(&c.gold, &c.prediction)),
        Tier::Exact | Tier::EditSim => Score::Skipped("function_body"),
        Tier::IdentF1 => Score::Value(ladder::ident_f1(&c.gold, &c.prediction)),
        Tier::Parse => Score::Value(ladder::parse(&c.prefix, &c.prediction, &c.suffix)),
        Tier::Symbols | Tier::Compile | Tier::Test => Score::Skipped("not recomputed"),
    }
}

fn as_f64(n: usize) -> f64 {
    u32::try_from(n).map_or(f64::MAX, f64::from)
}
```

Note the test's expected `in_file` line: two tasks, one exact match and one miss → `exact 0.50`; `symbols 1.00 (scored at run time)   (n=2)`.

- [ ] **Step 4: Run tests and clippy** (the compiler will list every `Task { … }` literal missing `codebase: None` — add it to each; there are ~10 in `store.rs` tests, 3 in `capability.rs`, 1 in `speeds.rs` tests via `TaskRow`, 2 in `compare.rs` tests via `TaskRow`).

- [ ] **Step 5: Commit** — `git commit -m "feat(bench): codebase rows with raw gold and prediction; the report block recomputes tiers on read"`

---

### Task 8: Git: clean-tree gate, HEAD, worktree, file walk

**Files:**
- Create: `src/core/bench/codebase/tree.rs`

**Interfaces:**
- Produces:
  ```rust
  pub fn assert_clean(repo: &Path) -> Result<(), ChekovError>         // WorkingTreeDirty on any porcelain line
  pub fn head_sha(repo: &Path) -> Result<String, ChekovError>          // full sha, trimmed
  pub struct Worktree { pub path: PathBuf, repo: PathBuf }
  impl Worktree { pub fn add(repo: &Path, dest: &Path) -> Result<Self, ChekovError>; pub fn remove(self) -> Result<(), ChekovError>; }
  pub fn rust_sources(root: &Path) -> Vec<(String, String)>            // (relative path, text) of every *.rs outside target/, tests/, *_test.rs, test_*.rs, and files containing "#[cfg(test)]"; sorted by path; files > 200 KiB skipped
  ```

- [ ] **Step 1: Write the failing tests** (these create real git repos in a scratch dir; `git` is a repo requirement)

```rust
#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::process::Command;

    use super::{Worktree, assert_clean, head_sha, rust_sources};
    use crate::error::ChekovError;

    fn git(repo: &std::path::Path, args: &[&str]) {
        let ok = Command::new("git").arg("-C").arg(repo).args(args).status().expect("git").success();
        assert!(ok, "git {args:?}");
    }

    fn repo(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("chekov-test-codebase-tree").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).expect("mkdir");
        std::fs::create_dir_all(dir.join("tests")).expect("mkdir");
        std::fs::write(dir.join("src/lib.rs"), "pub fn a() -> i32 {\n    1\n}\n").expect("write");
        std::fs::write(dir.join("src/cov.rs"), "fn b() {}\n#[cfg(test)]\nmod t {}\n").expect("write");
        std::fs::write(dir.join("tests/it.rs"), "fn c() {}\n").expect("write");
        git(&dir, &["init", "-q"]);
        git(&dir, &["-c", "user.email=t@t", "-c", "user.name=t", "add", "."]);
        git(&dir, &["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q", "-m", "init"]);
        dir
    }

    #[test]
    fn a_clean_tree_passes_and_a_dirty_or_untracked_one_is_refused() {
        let dir = repo("gate");
        assert_clean(&dir).expect("clean");
        std::fs::write(dir.join("src/new.rs"), "fn d() {}\n").expect("write");
        assert!(matches!(assert_clean(&dir), Err(ChekovError::WorkingTreeDirty { .. })), "untracked counts");
    }

    #[test]
    fn head_is_a_full_sha_and_a_worktree_is_a_detached_copy_that_removes_cleanly() {
        let dir = repo("wt");
        let sha = head_sha(&dir).expect("head");
        assert_eq!(sha.len(), 40, "{sha}");
        let dest = dir.join("eval").join("tree");
        let wt = Worktree::add(&dir, &dest).expect("add");
        assert!(dest.join("src/lib.rs").exists());
        assert_eq!(head_sha(&dest).expect("head of the copy"), sha);
        wt.remove().expect("remove");
        assert!(!dest.exists());
        assert_clean(&dir).expect("the repo is untouched");
    }

    #[test]
    fn rust_sources_skip_tests_and_cfg_test_files() {
        let dir = repo("walk");
        let files = rust_sources(&dir);
        let paths: Vec<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(paths, vec!["src/lib.rs"], "{paths:?}");
        assert!(files[0].1.contains("pub fn a()"));
    }
}
```

- [ ] **Step 2: Run to verify failure.**

- [ ] **Step 3: Implement**

```rust
//! The repository side of codebase mode: the clean-tree gate, HEAD, a
//! detached worktree to read from, and the Rust file walk with the leakage
//! filter's test-file rule applied at the source.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::ChekovError;

const MAX_FILE_BYTES: u64 = 200 * 1024;

fn git(repo: &Path, args: &[&str], step: &str) -> Result<String, ChekovError> {
    let out = Command::new("git").arg("-C").arg(repo).args(args).output().map_err(|e| {
        ChekovError::CodebaseWorktreeFailed { step: step.to_owned(), reason: e.to_string() }
    })?;
    if !out.status.success() {
        return Err(ChekovError::CodebaseWorktreeFailed {
            step: step.to_owned(),
            reason: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// `git status --porcelain` must be empty — untracked files included.
pub fn assert_clean(repo: &Path) -> Result<(), ChekovError> {
    let status = git(repo, &["status", "--porcelain"], "git status")?;
    if status.is_empty() {
        Ok(())
    } else {
        Err(ChekovError::WorkingTreeDirty { path: repo.to_path_buf() })
    }
}

pub fn head_sha(repo: &Path) -> Result<String, ChekovError> {
    git(repo, &["rev-parse", "HEAD"], "git rev-parse HEAD")
}

/// A detached checkout of HEAD that the run reads from; removed after.
pub struct Worktree {
    pub path: PathBuf,
    repo: PathBuf,
}

impl Worktree {
    pub fn add(repo: &Path, dest: &Path) -> Result<Self, ChekovError> {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| ChekovError::io(format!("creating {}", parent.display()), e))?;
        }
        let dest_s = dest.display().to_string();
        git(repo, &["worktree", "add", "--detach", &dest_s, "HEAD"], "git worktree add")?;
        Ok(Self { path: dest.to_path_buf(), repo: repo.to_path_buf() })
    }

    pub fn remove(self) -> Result<(), ChekovError> {
        let path = self.path.display().to_string();
        git(&self.repo, &["worktree", "remove", "--force", &path], "git worktree remove")?;
        git(&self.repo, &["worktree", "prune"], "git worktree prune")?;
        Ok(())
    }
}

/// Every `*.rs` under `root` except test files (the leakage filter's rule
/// (a), applied at the source so a test is never a task or context), with
/// oversized files skipped. Sorted by relative path.
#[must_use]
pub fn rust_sources(root: &Path) -> Vec<(String, String)> {
    let mut files = Vec::new();
    walk(root, root, &mut files);
    files.sort_by(|a, b| a.0.cmp(&b.0));
    files
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            if !matches!(name.as_str(), "target" | "tests" | ".git") {
                walk(root, &path, out);
            }
            continue;
        }
        if let Some(text) = source_text(&path, &name) {
            let rel = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
            out.push((rel, text));
        }
    }
}

fn source_text(path: &Path, name: &str) -> Option<String> {
    let is_rust = name.ends_with(".rs");
    let is_test = name.ends_with("_test.rs") || name.starts_with("test_");
    if !is_rust || is_test {
        return None;
    }
    if std::fs::metadata(path).ok()?.len() > MAX_FILE_BYTES {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    (!text.contains("#[cfg(test)]")).then_some(text)
}
```

- [ ] **Step 4: Run tests and clippy.** (Tests shell out to `git`; they run in CI too.)

- [ ] **Step 5: Commit** — `git commit -m "feat(bench): codebase tree — clean-tree gate, detached worktree, Rust source walk"`

---

### Task 9: Command wiring — `--codebase`, gate before launch, the run, the estimate, the corpus id

**Files:**
- Modify: `src/commands/capability.rs` (`BenchOpts`, `BenchArgs`, `bench`, `measure_candidate`, `SuiteInputs`, `run_suites`, `HeadInputs`, `build_head`, `corpus_id`, new `run_codebase`, tests)
- Modify: `src/core/bench/codebase/mod.rs` (a `TaskSetPlan` that holds the sampled tasks + hash, built once before launch)

**Interfaces:**
- Consumes: everything above.
- Produces (`codebase/mod.rs`):
  ```rust
  pub struct Prepared { pub head: String, pub set_hash: String, pub tasks: Vec<CodebaseTask>, pub shortfall: Vec<String>, pub symbols: ladder::Symbols }
  /// Gate, worktree, walk, mask, sample, assemble, symbol set — then the worktree is removed. Everything the run needs is in memory.
  pub fn prepare(repo: &Path, scratch_tree: &Path, tasks: u32) -> Result<Prepared, ChekovError>
  ```

- [ ] **Step 1: Write the failing tests** (capability.rs tests)

```rust
    #[test]
    fn codebase_and_fixture_conflict_and_suite_is_optional() {
        use clap::Parser;
        assert!(
            crate::cli::Cli::try_parse_from(["chekov", "capability", "bench", "--codebase", ".", "--fixture", "f.toml"]).is_err(),
            "mutually exclusive"
        );
        let cli = crate::cli::Cli::try_parse_from(["chekov", "capability", "bench", "--codebase", "."]).expect("parses");
        match cli.cmd {
            crate::cli::Cmd::Capability(cap) => match cap.action {
                Some(super::CapAction::Bench(opts)) => {
                    assert_eq!(opts.codebase.as_deref(), Some(std::path::Path::new(".")));
                    assert_eq!(opts.suite, None, "--suite not passed");
                }
                other => panic!("expected Bench, got {other:?}"),
            },
            _ => panic!("expected capability"),
        }
    }

    #[test]
    fn the_effective_suite_is_throughput_by_default_and_nothing_extra_with_codebase_alone() {
        use crate::core::bench::lifecycle::Suite;
        assert_eq!(super::effective_suite(None, false), Some(Suite::Throughput));
        assert_eq!(super::effective_suite(None, true), None, "codebase alone runs only codebase");
        assert_eq!(super::effective_suite(Some(Suite::All), true), Some(Suite::All));
    }

    #[test]
    fn the_codebase_corpus_id_pins_head_and_the_task_set() {
        let id = super::codebase_corpus_id("0123456789abcdef0123", "fedcba987654");
        assert_eq!(id, "codebase:0123456789ab:fedcba987654");
    }
```

Update `suite_flag_parses_and_defaults_to_throughput`: `opts.suite` is now `Option<Suite>` — assert `None` for the bare form and `Some(Suite::Agentic)` for `--suite agentic`.

- [ ] **Step 2: Run to verify failure.**

- [ ] **Step 3: Implement**

`BenchOpts`:

```rust
    /// The user's own Rust repository as graded infill tasks (spec §8, slice
    /// A). Refuses a dirty tree; reads from a detached worktree. Given alone,
    /// only the codebase set runs.
    #[arg(long, conflicts_with = "fixture")]
    pub codebase: Option<std::path::PathBuf>,
    /// Which task sets to measure. Default `throughput`; unset with
    /// `--codebase` means only the codebase set.
    #[arg(long, value_enum)]
    pub suite: Option<crate::core::bench::lifecycle::Suite>,
```

Helpers (near `bench`):

```rust
/// `--suite` not passed means `throughput` — unless `--codebase` is given,
/// in which case nothing beyond the codebase set runs.
fn effective_suite(
    passed: Option<crate::core::bench::lifecycle::Suite>,
    codebase: bool,
) -> Option<crate::core::bench::lifecycle::Suite> {
    use crate::core::bench::lifecycle::Suite;
    passed.or(if codebase { None } else { Some(Suite::Throughput) })
}

fn codebase_corpus_id(head: &str, set_hash: &str) -> String {
    format!("codebase:{}:{set_hash}", &head[..12.min(head.len())])
}
```

`BenchArgs` gains `codebase: Option<&'a std::path::Path>` and its `suite` becomes `Option<Suite>` (`effective_suite(opts.suite, opts.codebase.is_some())`). Every `args.suite` consumer takes the `Option`: `runs_throughput()`/`runs_agentic()` become `args.suite.is_some_and(Suite::runs_throughput)` etc. `agentic_estimate_secs(suite: Option<Suite>)` returns 0 for `None`.

In `bench`, before `resolve_candidates`:

```rust
    let prepared = match args.codebase {
        Some(repo) => Some(codebase::prepare(repo, &ctx.config.eval_dir().join("codebase-tree"), ctx.config.file.bench.codebase_tasks)?),
        None => None,
    };
```

and the estimate adds `prepared.as_ref().map_or(0, |p| p.tasks.len() as u64 * 6)`; the dry-run plan prints, when prepared: `codebase: {n} tasks from {repo} @ {head[..12]} ({shortfall joined}) ` — put it as a line before `render_plan`'s output. `prepared` is threaded into `run_candidate` → `measure_candidate` via `BenchArgs` (add `prepared: Option<&'a codebase::Prepared>` — note `BenchArgs` is built from `BenchOpts` by `From`; build `prepared` in `bench` and construct a second args value, or make `prepared` a separate parameter bundled into a small `Codebase<'a>` struct passed alongside; keep `bench`'s callee signatures ≤ 3 params by bundling `(args, prepared)` into `struct RunInputs<'a> { args: &'a BenchArgs<'a>, prepared: Option<&'a codebase::Prepared> }`).

`HeadInputs` gains `codebase: Option<(&'a str, &'a str)>` (head, set hash); `build_head` computes `corpus_id` as:

```rust
        corpus_id: match inputs.codebase {
            Some((head, set_hash)) => codebase_corpus_id(head, set_hash),
            None => corpus_id(inputs.suite, inputs.fixture)?,
        },
```

with `corpus_id(suite: Option<Suite>, …)` mapping `None` to `"codebase-only"` (never reached when `codebase` is `Some`; keep the arm honest).

`SuiteInputs` gains `prepared: Option<&'a codebase::Prepared>`; `run_suites` ends with:

```rust
    if let Some(prepared) = inputs.prepared {
        run_codebase(sink, &wire, prepared)?;
    }
```

`run_codebase`:

```rust
/// Every sampled task through `/infill`, recorded with its raw prediction.
/// A model without FIM records every task unavailable with the reason and
/// stops firing — a capability, never a zero.
fn run_codebase(
    sink: &mut TaskSink,
    wire: &crate::core::bench::runner::ProbeWire,
    prepared: &crate::core::bench::codebase::Prepared,
) -> Result<(), ChekovError> {
    use crate::core::bench::codebase::ladder::{self, Scored};
    use crate::core::bench::runner::{InfillOutcome, InfillTask, cross_infill};
    use crate::core::bench::store::{self, TaskKey};
    let mut unsupported: Option<String> = None;
    for task in &prepared.tasks {
        if sink.is_done(&TaskKey::buffered("codebase", &task.id)) {
            continue;
        }
        let outcome = match &unsupported {
            Some(reason) => Err(reason.clone()),
            None => match cross_infill(wire, &InfillTask { prefix: &task.prefix, suffix: &task.suffix, gold_lines: task.gold.lines().count().max(1) })? {
                InfillOutcome::Answered(artifact) => Ok(artifact),
                InfillOutcome::Unsupported(reason) => {
                    eprintln!("chekov bench: infill unsupported by this model — codebase is N/A ({reason})");
                    unsupported = Some(reason.clone());
                    Err(reason)
                }
            },
        };
        let (measure, grade, prediction) = match outcome {
            Ok(artifact) => (probe_measure(&artifact.timings), None, artifact.anthropic_body),
            Err(reason) => (empty_measure(), Some(store::GradeRow::unavailable(reason)), String::new()),
        };
        let symbols_score = ladder::score_all(&Scored { task, prediction: &prediction, symbols: &prepared.symbols })
            .into_iter()
            .find_map(|(t, s)| match (t, s) { (ladder::Tier::Symbols, ladder::Score::Value(v)) => Some(v), _ => None })
            .unwrap_or(0.0);
        sink.writer.append(store::Task {
            suite: "codebase".into(),
            task_id: task.id.clone(),
            measure,
            grade,
            transport: store::Transport::Buffered,
            codebase: Some(store::CodebaseRow {
                tier: task.tier,
                file: task.file.clone(),
                line: task.line,
                label: crate::core::bench::codebase::MASK_LABEL.to_owned(),
                gold: task.gold.clone(),
                prediction,
                prefix: task.prefix.clone(),
                suffix: task.suffix.clone(),
                excluded: task.excluded.clone(),
                symbols_score,
            }),
        })?;
    }
    Ok(())
}
```

`empty_measure()` is the zeroed `Measure` already built inline in `failed_probe` — extract it into a small fn and use it in both places. Split `run_codebase` into `run_codebase` (loop + latch) and `record_codebase_task(sink, task, outcome, symbols)` to stay under 40 lines each.

`codebase::prepare` (in `mod.rs`):

```rust
/// Gate → worktree → walk → mask → sample → assemble → symbol set, then the
/// worktree is removed: everything the run needs is in memory and the
/// user's checkout was never read directly.
pub fn prepare(repo: &Path, scratch_tree: &Path, tasks: u32) -> Result<Prepared, ChekovError> {
    tree::assert_clean(repo)?;
    let head = tree::head_sha(repo)?;
    let worktree = tree::Worktree::add(repo, scratch_tree)?;
    let files = tree::rust_sources(&worktree.path);
    let candidates: Vec<sample::FileCandidates> = files
        .iter()
        .map(|(path, text)| sample::FileCandidates { path: path.clone(), candidates: masker::RustBraceMasker.candidates(text) })
        .collect();
    let set = sample::sample(candidates, sample::quota(tasks), sample::seed_from_head(&head));
    let symbols = ladder::repo_symbols(&files);
    worktree.remove()?;
    if set.picked.is_empty() {
        return Err(ChekovError::CodebaseNoTasks {
            path: repo.to_path_buf(),
            reason: format!("scanned {} files, 0 candidate spans", files.len()),
        });
    }
    let by_path: std::collections::HashMap<&str, &str> = files.iter().map(|(p, t)| (p.as_str(), t.as_str())).collect();
    let tasks = set.picked.iter().filter_map(|p| by_path.get(p.path.as_str()).map(|text| filter::assemble(&p.path, text, p))).collect();
    Ok(Prepared { head, set_hash: sample::task_set_hash(&set), tasks, shortfall: set.shortfall, symbols })
}
```

- [ ] **Step 4: Run tests and clippy**, then the live smoke: `cargo run -q -- capability bench --codebase . --dry-run` in the chekov repo (commit first so the tree is clean, or point at a clean clone). Expected: `codebase: 24 tasks from . @ <sha12>` and the plan.

- [ ] **Step 5: Commit** — `git commit -m "feat(bench): --codebase — gate, worktree, sampled infill tasks, and the run"`

---

### Task 10: Docs, live run, IDEAS

**Files:**
- Modify: `README.md` (command reference row for `capability bench`; a short "Codebase mode" paragraph under it), `CHANGELOG.md` (Unreleased/Added), `IDEAS.md` (the capability entry's status line: `slice 6 … --codebase slice A SHIPPED 2026-08-29 (Rust, same-file, tiers 1-5); slices B (cross-file + exec tiers) and C (--judge) OPEN`).

- [ ] **Step 1: Live run.** With the branch committed (clean tree): 

```bash
cargo run -q -- capability bench --codebase . --models ornith-1.5-35b-a3b --yes 2>&1 | tail -30
cargo run -q -- capability bench --codebase . --models qwen3.8-27b --yes 2>&1 | tail -30
```

Expected: a `codebase` block per model with tier means; `in_file` and `function_body` lines; the labels; no `N/A`. Copy both blocks into the PR body as the first spread. If tier means are identical to two decimals across the two models, say so in the PR ("not discriminating on this pair at n=24") rather than dressing it up.

- [ ] **Step 2: Docs.** README row: `| \`capability bench --codebase <PATH>\` | The repository at PATH (clean tree required) as 24 deterministic same-file infill tasks, sampled from HEAD (\`[bench] codebase_tasks\`), run through \`/infill\`, graded on tiers 1–5 (exact, edit similarity, identifier F1, parse, repo-symbol existence); tiers 6–7 and cross-file context are slice B. Masks are boundary-scanned, not AST, and the report says so. A model without FIM tokens is N/A, never zero. |`. CHANGELOG bullet in the same words as the spec's §1 summary. IDEAS status line.

- [ ] **Step 3: Full verification** — `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test && pushkin floor`.

- [ ] **Step 4: Commit and PR** — `git commit -m "docs: codebase mode slice A — README, changelog, status"`, then push and `gh pr create --base develop` with the live spread in the body.

---

## Self-review

**Spec coverage:** §2 gate/worktree/identity/config → Tasks 1, 8, 9. §3 masker + sampling → Tasks 2, 3. §4 filter → Task 4 (+ rule (a) in Task 8's walk). §5 crossing + capability → Task 6 (+ latch in Task 9). §6 ladder → Task 5. §7 storage/report → Task 7 (with one refinement recorded there: tier 5 is scored at run time because the worktree is gone at read time; tiers 1–4 recompute). §8 errors → Task 1. §9 tests → each task; live run → Task 10. §10 files → the table.

**Placeholders:** none; every step has code. Two known judgment calls are stated inline (prelude list is a constant to extend; the variant rule over-approximates toward "exists").

**Type consistency:** `TaskTier` (codebase/mod.rs) used by masker, sample, filter, ladder, store, capability; `Candidate.byte_range: Range<usize>`; `Picked { path, candidate, id }`; `CodebaseTask` fields as in Task 4 and consumed verbatim in Tasks 5, 7, 9; `InfillTask { prefix, suffix, gold_lines }`; `InfillOutcome::{Answered(ProbeArtifact), Unsupported(String)}`; `CodebaseRow` fields identical in Task 7 (definition and tests) and Task 9 (construction); `Prepared { head, set_hash, tasks, shortfall, symbols }`; `effective_suite(Option<Suite>, bool) -> Option<Suite>`; `codebase_corpus_id(&str, &str) -> String`.
