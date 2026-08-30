# Codebase mode, slice A — design

`chekov capability bench --codebase <PATH>`: the user's own repository as graded
infill tasks. Slice A of three; refines `docs/capability-spec.md` §8 for what
ships first. Human decisions taken 2026-08-29: Rust only; 24 tasks per run;
slice A is same-file context with tiers 1–5 and no execution.

## 1. Why this slice, and what it deliberately leaves out

A private codebase is the only corpus a local user has that is guaranteed not
to be in any model's training data. Slice A builds the whole pipeline — gate,
worktree, deterministic task sampling, honest masking, the infill crossing,
storage, the deterministic scoring ladder, the report — over the narrowest
task shape that already discriminates: same-file infill in Rust.

Left to later slices, and said in every report so nothing is over-claimed:

- **Slice B1** — `cross_file_first` tasks with `input_extra` context, which is
  where the leakage filter's rules (a), (b) and (d) become live. Specified and
  shipped: `docs/superpowers/specs/2026-08-29-codebase-mode-slice-b1-design.md`.
- **Slice B2** — tiers 6 (compile gate) and 7 (covering test) behind
  `--allow-exec`.
- **Slice C** — `--judge`.
- Other languages behind the same `MaskSource` trait.
- Any composite score (§7.5 is a separate decision).

## 2. Command surface and lifecycle

```
chekov capability bench --codebase <PATH> [--models a,b] [--resume <id>] [--dry-run] [--yes]
```

- `--codebase` and `--fixture` are mutually exclusive (clap `conflicts_with`).
- `--codebase` selects the `codebase` task set. Given alone, ONLY that set
  runs — the throughput/agentic sets run alongside it only when `--suite`
  is passed explicitly (`--suite all --codebase .` runs everything). To
  tell "not passed" from the default, `--suite` becomes `Option<Suite>` in
  the parser with the same effective default (`throughput`) when
  `--codebase` is absent. The dry-run plan lists the codebase set with its
  task count and estimate (`codebase_tasks × 6 s`).
- Per-candidate lifecycle is unchanged: launch behind `run`'s preflight,
  `/health`+pid readiness, the `/props` context assertion, run, teardown with
  the budget-release check.

**The gate.** Before anything is launched, `PATH` must be a git repository
with a clean working tree (`git status --porcelain` empty, untracked files
included). Otherwise `ChekovError::WorkingTreeDirty { path }` — "commit or
stash, or run against a clean clone; the task set is sampled from HEAD and
must be reproducible". The gate runs before model launch so a refusal costs
nothing.

**Isolation.** `git -C PATH worktree add --detach <eval>/<run_id>/tree HEAD`.
Every read in the run is from that tree, never the user's checkout. After the
run (success or failure) the worktree is removed with `git worktree remove
--force` and `git worktree prune`; the run directory keeps every task record,
gold span and prediction, so the worktree is not needed for audit. A crash
that skips removal leaves a worktree the next `--resume` reuses if present or
recreates if absent; `IDEAS.md` notes that `git worktree prune` is the manual
cleanup.

**Identity.** `stamp.corpus_id = "codebase:<HEAD sha, 12 hex>:<task-set
hash, 12 hex>"`, where the task-set hash is the SHA-256 of the sampled task
ids in order. `compare` therefore refuses across differing HEADs through the
existing first-differing-field rule, and a differing `codebase_tasks` refuses
too (the set differs). `--resume` keys on `(suite, task_id, transport)` as
today; codebase tasks are buffered-transport only (`/infill` has no stream
the agent uses).

**Config.** `[bench] codebase_tasks = 24` (README and `config.example.toml`
updated; the README-equals-defaults test holds). Two-thirds `in_file`
(rounded up), one-third `function_body`.

## 3. Task generation

### 3.1 `MaskSource`

```rust
pub trait MaskSource {
    /// Every candidate span in one file, in source order. Never a malformed
    /// span: a candidate that fails its own balance check is not returned.
    fn candidates(&self, path: &Path, text: &str) -> Vec<Candidate>;
}

pub struct Candidate {
    pub tier: TaskTier,        // InFile | FunctionBody
    pub byte_range: Range<usize>,
    pub line: usize,           // 1-based first line of the span
    pub doc_comment: Option<Range<usize>>, // `///` block directly above a fn
}
```

One implementation in slice A: `RustBraceMasker`.

### 3.2 `RustBraceMasker`

Files: every `*.rs` under the worktree except `target/`, `tests/`, `*_test.rs`,
`test_*.rs`, and any file containing `#[cfg(test)]` (the test globs are the
leakage filter's rule (a), applied at candidate time so a test file is never
a task either — masking an assertion measures nothing). Files over 200 KiB
are skipped and counted.

> **Amended 2026-08-29:** a file containing `#[cfg(test)]` is kept; each
> `#[cfg(test)]`-attributed item is cut (attribute through the matching `}` or
> the terminating `;`, literal-aware) before masking and before the symbol set
> is built, and the report says how many lines were elided.

A scanner walks the text once, tracking string literals (`"…"` with escapes,
raw strings `r#"…"#`), char literals, line comments and block comments, so
braces inside them never count. On each `fn` signature (regex
`\bfn\s+[A-Za-z_][A-Za-z0-9_]*\s*[<(]`, at line start after optional
`pub(…)`/`pub`/`async`/`unsafe`/`const`), the body is the balanced `{…}`
that follows the signature's closing `)` and optional `-> T`/`where` clause.

- `function_body`: the body's interior (between the braces), when it spans
  3–40 lines. The `///` block immediately above the signature is recorded as
  `doc_comment`.
- `in_file`: inside a qualifying body, each balanced statement-span of 1–8
  lines: a `let …;`, an expression statement ending in `;`, a `match`/`if`/
  `for`/`while`/`loop` with its block, or a bare block. Spans start at a line
  start and end at a line end; a span whose braces/brackets/parens do not
  balance is discarded (never trimmed or extended).

Every task carries the label `boundary-scanned (not AST)`; the report prints
it once per run. Tree-sitter or `syn` can later replace this behind the same
trait with no caller change.

### 3.3 Deterministic sampling

- Seed: the first 8 bytes of `sha256("chekov-codebase-v1:" + HEAD sha)`,
  driving a small hand-rolled xorshift64* (no new crate; the seed pins the
  set and the generator is part of the task-set identity).
- Candidates are collected per file, files sorted by path. Selection is
  stratified: round-robin across files in a seeded order, taking one
  candidate per file per pass, so a large file cannot dominate the set.
- 16 `in_file` and 8 `function_body` for the default 24 (`ceil(2n/3)` and
  the remainder in general). If a tier has fewer candidates than its quota,
  the shortfall is reported, not filled from the other tier — the run says
  "function_body: 5 of 8 requested (repo has 5 candidates)".
- Task id: `<tier>-<sha256(path)[..6]>-L<line>`, stable across runs on the
  same HEAD and readable in the report.
- `CodebaseNoTasks { path, reason }` when the whole repo yields zero
  candidates ("scanned 12 files, 0 candidate spans — Rust only in slice A").

## 4. Context and the leakage filter (slice A scope)

For each task: `input_prefix` = the file's text before the span,
`input_suffix` = the text after it. No `input_extra` in slice A.

Leakage filter rules, and what slice A does with each:

| rule | what §8 says | slice A |
|---|---|---|
| (a) test files | dropped from context | applied at candidate time (§3.2); no test file is ever a task or context |
| (b) files containing the masked identifier | dropped from context | not applicable — no cross-file context; recorded as `n/a: same-file` |
| (c) the doc comment above the masked span | dropped | **applied**: for `function_body` tasks the `///` block is cut from the prefix; `excluded.doc_comment = 1` |
| (d) docs naming the symbol | dropped | not applicable — recorded as `n/a: same-file` |

> **Rule (a), amended 2026-08-29:** a file containing `#[cfg(test)]` is kept;
> each `#[cfg(test)]`-attributed item is cut (attribute through the matching
> `}` or the terminating `;`, literal-aware) before masking and before the
> symbol set is built, and the report says how many lines were elided. The
> record carries the file's own count as `excluded.cfg_test_lines`.

The task record carries `excluded: { doc_comment: 0|1, cross_file: "n/a: same-file" }`
and the report prints, once per run, `context: same-file (cross-file context
and its leakage filter arrive in slice B)`. The filter machinery is built
now with the cross-file set empty; slice B fills in numbers rather than
adding a mechanism. As §8 states, this is a mitigation, not a proof.

## 5. The infill crossing

`runner::cross_infill(wire: &ProbeWire, task: &InfillTask) -> Result<ProbeArtifact, ChekovError>`:

- `POST {upstream.base_url}/infill` with bearer auth, body
  `{ "input_prefix", "input_suffix", "prompt": "", "input_extra": [],
  "n_predict": N, "temperature": 0, "top_k": 1, "seed": <pinned> }`,
  where `N = max(64, 3 × gold_lines × 12)` — generous enough for any
  reasonable fill, bounded so a runaway costs seconds, not minutes.
- llama.cpp resolves the FIM sentinels from GGUF metadata; chekov never
  writes `<PRE>`/`<|fim_prefix|>` or chooses PSM/SPM.
- The artifact is the raw `content` plus `timings` (read as for every other
  probe; missing timings are `BenchNoTimings`).
- **Capability, not failure.** A model without FIM tokens answers a non-2xx
  whose body names infill; that `UpstreamRefused` becomes
  `Capability::InfillUnsupported { reason }`, the run records every codebase
  task as `unavailable` with that reason (never a zero score), prints the
  reason once, and does not fire the remaining tasks. Any other refusal is
  the task's own unavailability, as in the agentic suites.

Verified live 2026-08-29: `/infill` answers on both `qwen3.8-27b` (`a + b`)
and `ornith-1.5-35b-a3b` (`return a + b;\n}\n` — overran the suffix, which
is the kind of signal this mode exists to see).

## 6. The scoring ladder, tiers 1–5

`bench/codebase/ladder.rs`, pure functions over `(gold, prediction, context)`,
each ≤ 40 lines, each reported separately, never collapsed:

1. **exact** — whitespace-normalised equality (collapse runs of whitespace,
   trim). Scored on `in_file` tasks only.
2. **edit_sim** — `1 − lev(pred, gold) / max(len)` over the normalised
   strings; two-row DP. `in_file` only.
3. **ident_f1** — identifier sets (`[A-Za-z_][A-Za-z0-9_]*`) minus the Rust
   keyword list; F1 of prediction vs gold. Both tiers.
4. **parse** — `prefix + prediction + suffix` balances braces, brackets and
   parens outside strings/comments (the same scanner as the masker). 0/1.
   Both tiers.
5. **symbols** — the fraction of the prediction's identifiers that exist in
   the repo's symbol set: every `fn`/`struct`/`enum`/`trait`/`type`/`const`/
   `static`/`mod` declaration name in the worktree, every enum variant and
   struct field name (the `name` in `name: Type,` inside a `struct { … }`
   block), the last segment of every `use` path in the task's file, and a
   fixed Rust prelude/std list (`Some`, `Ok`, `Vec`, `String`, `format`,
   method names such as `iter`, `map`, `collect`, `unwrap_or` …) kept as a
   constant in `ladder.rs`. Identifiers the gold itself introduces (`let x`)
   count as existing. Both tiers. This is the API-hallucination probe.

Tiers 6 and 7 are recorded on every task as `skipped: slice B
(--allow-exec)` — reported, never Pass.

## 7. Storage and report

New suite `codebase`. One `TaskRow` per task; `Measure` carries the timings;
a new optional field on `TaskRow`, `codebase: Option<CodebaseRow>`
(`serde default`, absent on every other suite and on every older row):

```rust
pub struct CodebaseRow {
    pub tier: TaskTier,
    pub file: String,          // worktree-relative path
    pub line: usize,
    pub label: String,         // "boundary-scanned (not AST)"
    pub gold: String,
    pub prediction: String,    // raw model output, unmodified
    pub excluded: Excluded,    // { doc_comment: u8, cross_file: String }
}
```

Scores are **not stored**: they are recomputed on read from `gold`,
`prediction` and the file text kept in the record — the same rule as
medians recomputed from samples, so a stored score can never drift from
its inputs. `GradeRow` carries `unavailable` for the infill-unsupported
case; otherwise `pass` is unused for this suite and the ladder is the
verdict.

Report block, after the agentic lines:

```
codebase     24 tasks (16 in_file, 8 function_body) — boundary-scanned (not AST); context: same-file
             in_file        exact 0.31   edit_sim 0.62   ident_f1 0.71   parse 0.94   symbols 0.88   (n=16)
             function_body  ident_f1 0.55   parse 0.75   symbols 0.81   (n=8)
             tiers 6-7 skipped: slice B (--allow-exec)
```

or `codebase     N/A — infill unsupported by this model (<server's words>)`.

## 8. Errors

- `WorkingTreeDirty { path }` — names `git status` and the clean-clone
  alternative.
- `CodebaseNoTasks { path, reason }` — names what was scanned and that slice
  A is Rust only.
- `CodebaseWorktreeFailed { step, reason }` — the git command that failed
  and `git worktree prune` as the remediation.

All three follow `error.rs`'s contract: what failed and the remediation
command.

## 9. Tests

- **masker**: a body with braces inside strings and comments masks at the
  right boundary; a 2-line and a 41-line body are not candidates; `in_file`
  spans balance; an unbalanced statement is discarded; the doc comment
  range is found only when directly adjacent; test-glob files yield nothing.
- **sampling**: same HEAD → identical task ids in identical order; a
  different seed → a different set; stratification takes at most one span
  per file per pass; quota shortfall is reported.
- **filter**: the doc comment is cut from the prefix and counted; a task
  without one records 0; cross-file rules record `n/a`.
- **ladder**: exact and edit_sim on a near miss; ident_f1 on a wrong-API
  answer; parse on an unbalanced prediction; symbols scores a fabricated
  identifier down and a gold-introduced binding as existing; keywords are
  never identifiers.
- **crossing**: the wire carries prefix/suffix/pins and empty `input_extra`;
  an infill refusal becomes `InfillUnsupported` and every task is recorded
  unavailable with the reason; timings missing is loud.
- **store/report**: a row round-trips with the raw prediction; scores are
  recomputed from stored text; the report block and the N/A line render;
  older rows without the field load.
- **command**: `--codebase` and `--fixture` conflict; a dirty tree is refused
  before launch; the dry-run plan counts the tasks.
- **live**: chekov's own repository on ornith and qwen3.8-27b; the tier
  spread goes in the PR as the first evidence of discrimination.

## 10. Files

`src/core/bench/codebase/{mod,masker,sample,filter,ladder}.rs` (new),
`src/core/bench/runner.rs` (+`cross_infill`), `src/core/bench/store.rs`
(+`CodebaseRow`, report block), `src/commands/capability.rs` (flag, gate,
worktree, wiring, estimate, corpus id), `src/core/config.rs`
(`codebase_tasks`), `src/error.rs`, `config.example.toml`, `README.md`,
`CHANGELOG.md`, `IDEAS.md`.
