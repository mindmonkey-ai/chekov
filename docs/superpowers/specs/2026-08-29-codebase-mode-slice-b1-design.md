# Codebase mode, slice B1 — cross-file first-use tasks with `input_extra`

Date: 2026-08-29. Builds on slice A (`2026-08-29-codebase-mode-slice-a-design.md`,
shipped in PR #47 and amended the same day for `#[cfg(test)]` elision). Slice B2 —
tiers 6 (compile gate) and 7 (covering test) behind `--allow-exec` — is a separate
spec; nothing here depends on it.

## 1. Why this slice, and what it deliberately leaves out

Slice A measures same-file infill. Same-file completion is where models saturate:
the file in front of them carries most of the answer. The tier where models separate
is the one the umbrella spec calls `cross_file_first` — mask the **first** use in a
file of a symbol defined in another file. A model that has not read the other file
cannot recover the signature; a model that has can. Slice B1 adds that tier, gives
the model the other file through llama.cpp's `input_extra`, and — because the point
is to measure what reading the repo buys — crosses every such task **twice**, with
and without the extra file, and prints the lift.

Left out, and said in every report:

- **Slice B2** — tiers 6–7 behind `--allow-exec`.
- **Slice C** — `--judge`.
- More than one extra file per task, docs as context, other languages, any
  composite score. Rule (d) of the leakage filter (docs naming the symbol) stays
  vacuous here: B1 never sends a documentation file.

## 2. Command surface and lifecycle

No new flags. `capability bench --codebase <PATH>` gains the tier automatically.
`[bench] codebase_tasks` (default 24) now splits **12 / 6 / 6** — `in_file`,
`function_body`, `cross_file_first` — through the existing `sample::quota`. The
clean-tree gate, detached worktree, `#[cfg(test)]` elision, seeding from HEAD,
`--resume`, `--dry-run` and the estimate are unchanged, except:

- the estimate adds one crossing per cross-file task (two arms), so `tasks × 6 s`
  becomes `(in_file + function_body + 2 × cross_file_first) × 6 s`;
- the dry-run line names the tier's count: `codebase: 24 tasks from <repo> @ <sha12>
  (12 in_file, 6 function_body, 6 cross_file_first × 2 arms)`, plus the existing
  shortfall and elision clauses;
- `corpus_id`'s set hash covers the new tier's ids, so a repository that yields
  cross-file tasks gets a new `corpus_id`. Runs recorded before this slice are not
  comparable with runs after it; `compare` refuses them by that field, as it should.

## 3. Task generation

### 3.1 Inputs

Detection runs on the **elided** file texts (test items already cut) after the
symbol pass, so it sees exactly what the model will see. Two indices are built once
per run:

- `declared: BTreeMap<name, BTreeSet<file>>` — declaration names only: `fn`,
  `struct`, `enum`, `trait`, `type`, `const`, `static`, `mod` (from
  `ladder::collect_declarations`; struct fields and enum variants are **not**
  call sites and are not indexed). Names in `ladder::PRELUDE` and Rust keywords
  are dropped from the index.
- per file, the ordered `in_file` statement spans the masker already produces
  (`Candidate { tier: InFile, byte_range, line, … }`).

### 3.2 A `cross_file_first` candidate

In file F, walking F's `in_file` spans in byte order, a span S is a candidate for
`name` when all of the following hold:

1. `declared[name]` is exactly one file G, and G ≠ F. (A name declared in two or
   more files is **ambiguous** and never a candidate; the count of skipped
   ambiguous names goes into the tier's shortfall reason.)
2. `name` is not declared anywhere in F (a local shadow makes the other file
   irrelevant).
3. S's text contains a call-shaped use — `name(`, `name::`, `.name(` or `name {`
   (a struct literal) — outside string and comment literals; S is not a `use`
   statement (a `use` line is an import, not the first *call site* the umbrella
   spec asks for).
4. S contains the **first** such use of `name` in F: no earlier span or line of F
   uses `name` in a call shape. The first use is found by scanning F's text once
   per name, literal-aware.

> Amended 2026-08-30: two changes, both narrowing the candidate set.
>
> 5. F must **refer to G's module**. G's module stem is its file stem, or the
>    parent directory name for `mod.rs`, `lib.rs` and `main.rs`
>    (`src/core/bench/store.rs` → `store`; `src/agents/mod.rs` → `agents`).
>    F refers to it when the stem appears as an identifier inside one of F's
>    `use` statements, or immediately before a `::` anywhere else in F's
>    literal-blanked text. A G that F never names is not a candidate: rule 1
>    alone matched a bare `x.next()` to whichever file happened to declare
>    `fn next`, and that file says nothing about the call. The skipped names
>    are counted, distinct, and reported in the shortfall as
>    `n names skipped (no import of the defining module)`.
>
> `mod` leaves the index (§3.1): a module declaration names a file, not a
> callable symbol. Tier 5's declaration list is unchanged. Both declaration
> scans — `Index::build` and the window's `declaration_offset` — run over
> literal-blanked text, so `/// the fn build …` declares nothing.

A span may first-use several names; it yields **one** task, keyed on the name
whose first use appears earliest in S, with the others recorded on the task as
`also_first_uses` (informational, in the row). One candidate per (F, name).

The tier is `TaskTier::CrossFileFirst` with label `cross_file_first`. Task id
`cross_file_first-<sha256(F)[..6]>-L<line>`. The gold, prefix and suffix are the
`in_file` span's, unchanged.

### 3.3 Sampling

`sample::Quota` gains the third lane. Stratification across files applies to the
new lane exactly as to the others (≤ 3 per file). A lane that cannot be filled is
reported as today: `cross_file_first: 4 of 6 (2 short: 17 ambiguous names skipped,
9 files have no cross-file use)`. `task_set_hash` is over every picked id in order,
so it covers the new tier.

## 4. Context and the leakage filter

### 4.1 The extra file

For a `cross_file_first` task on `name` defined in G:

- `extra.path = G` (worktree-relative), `extra.text` = G's **elided** text.
- **Cap: 32 KiB.** When G's elided text exceeds 32 768 bytes, send the 32 KiB window
  centred on the byte offset of `name`'s declaration line, snapped outward to line
  boundaries; the row records `extra.truncated = true` and `extra.bytes` = the bytes
  actually sent. Otherwise `truncated = false`, `bytes = len`.
- Exactly one extra file per task in B1. (llama.cpp keeps the **tail** of the extra
  tokens when they exceed `n_ctx − n_batch − 2·n_predict`; with one file under 32 KiB
  at ctx ≥ 32K nothing is ever dropped, and the report says the budget in words.)

### 4.2 Rules, amended for cross-file context

| rule | umbrella text | B1 |
|---|---|---|
| (a) test files | dropped | unchanged: name-excluded files never exist; `#[cfg(test)]` items elided |
| (b) files containing the masked identifier | dropped | **amended reading:** every file *other than G* whose elided text contains the gold span's whitespace-normalised text is withheld — that is a verbatim answer, not a definition. G is never withheld: without the definition the tier is unanswerable. Counted as `cross_file_withheld`. (In B1 nothing but G is ever sent, so "withheld" records what a multi-file B-later would have had to drop — the count is computed and printed now so the number exists when it starts to bite.) |
| (c) the doc comment above the span | cut for `function_body` | unchanged; a cross-file span is a statement, so `doc_comment = 0` |
| (d) docs naming the symbol | dropped | vacuous in B1 — no `.md`/docs file is ever sent; recorded `0` |

The record's `excluded.cross_file` string, `"n/a: same-file"` in slice A, becomes
descriptive: `"sent <G> (<bytes> KiB[, truncated]); withheld <k> (contain the
answer)"` for cross-file tasks, and stays `"n/a: same-file"` for the other tiers.

## 5. The crossing — two arms

`runner::InfillTask` gains `extra: Option<ExtraChunk<'a>>` with
`pub struct ExtraChunk<'a> { pub filename: &'a str, pub text: &'a str }`.
`cross_infill` serialises it as `"input_extra": [{ "filename", "text" }]` — the
llama.cpp shape — and `[]` when `None`. Nothing else on the wire changes: same pins,
same `n_predict = max(64, 36 × gold_lines)`, same prefix/suffix.

Each cross-file task is crossed **twice**, in this fixed order:

1. **without** context — `task_id = <id>`, `arm = "no_extra"`;
2. **with** context — `task_id = <id>+extra`, `arm = "extra"`.

Distinct `task_id`s mean `--resume` skips per arm through the existing
`TaskKey::buffered(suite, task_id)`. The `Unsupported` latch (a model without FIM
tokens) and per-task unavailability apply to both arms identically; an arm that
failed is one unavailable row.

## 6. Scoring

Tiers 1–5 as in slice A, for every arm. One change: tier 5's `Known.context` for the
**with** arm is `prefix + suffix + extra.text` — the model was shown G, so G's names
exist for it. The **without** arm scores against `prefix + suffix` only. Tiers 6–7
remain `Skipped("slice B2 (--allow-exec)")`.

> Amended 2026-08-30: tiers 1–4 score the prediction **trimmed to the gold's
> line count** — the first `gold.lines().count()` lines of the fill, ending
> the way the gold ends. `n_predict` is `max(64, 36 × gold_lines)`, so a model
> that reproduced a one-line gold and then wrote the rest of the function was
> graded on the run-on: the token budget, not the answer. Tier 5 keeps the
> whole prediction — what it asks is which identifiers the model emitted.
> The trim lives in `ladder::stored_tier`, so the run and the recompute agree.
> Stored predictions are untouched, and the report recomputes, so the rendered
> `in_file`, `function_body` and cross-file numbers of runs already on disk
> change. The header says so (§7.2).

## 7. Storage and report

### 7.1 Row

`CodebaseRow` gains, all `#[serde(default)]` so slice-A rows load:

```rust
pub arm: Option<String>,                 // "no_extra" | "extra"; None for other tiers
pub extra: Option<ExtraFile>,            // Some only on the "extra" arm
pub also_first_uses: Vec<String>,        // other names first-used in the span

pub struct ExtraFile { pub path: String, pub bytes: u64, pub truncated: bool }
```

`Excluded` gains `pub cross_file_withheld: u32` (`serde(default)`); its
`cross_file: String` carries the descriptive text of §4.2. `TaskTier` gains
`CrossFileFirst` (serde `cross_file_first`); `deny_unknown_fields` stays on
`CodebaseRow` — the new fields are additions, and an unknown field is still a
schema error.

> Amended 2026-08-30: `CodebaseRow` gains two more `#[serde(default)]` fields,
> and `CodebaseTask` the first of them.
>
> ```rust
> pub name: Option<String>,     // the symbol the crossing is keyed on
> pub n_predict: Option<u32>,   // the budget this crossing actually sent
> ```
>
> `name` is what makes a crossing auditable — which symbol the model was asked
> to recover, beside `extra.path`, which file it came from. `n_predict` is set
> by the run loop from `runner::n_predict_for`, the same function the wire
> uses, so a short fill can be told from one the budget cut off. Both are
> `None` on rows written before them, which is what they were: unrecorded.

### 7.2 Report

The block keeps its header, with the `context:` note amended:

```
codebase     24 tasks, 30 crossings, from 19 files (12 in_file, 6 function_body, 6 cross_file_first × 2 arms) — boundary-scanned (not AST); context: same-file, plus the defining file for cross_file_first (engine window ≤ n_batch; extra from ctx); tests elided: 3272 lines in 20 files
             in_file                 exact 0.19   edit_sim 0.49   ident_f1 0.70   parse 0.44   symbols 0.96 (scored at run time)   (n=12)
             function_body           ident_f1 0.69   parse 0.88   symbols 0.89 (scored at run time)   (n=6)
             cross_file_first        exact 0.17   edit_sim 0.41   ident_f1 0.55   parse 0.67   symbols 0.83 (scored at run time)   (n=6)
             cross_file_first+extra  exact 0.50   edit_sim 0.71   ident_f1 0.80   parse 0.83   symbols 0.91 (scored at run time)   (n=6)
             context lift            exact +0.33  edit_sim +0.30  ident_f1 +0.25  parse +0.17  symbols +0.08   (6 files sent, 41.2 KiB, 1 truncated; 2 withheld)
             tiers 6-7 skipped: slice B2 (--allow-exec)
```

- The tier label column widens to 24 characters for every line (one format, so the
  columns align).
- `context lift` is the per-tier difference of arm means over the tasks present in
  **both** arms; a task unavailable in either arm is excluded from the lift and the
  line says `(n=k of 6)`. With no cross-file tasks the three lines are omitted and
  the header says `0 cross_file_first` with the shortfall reason on its own line.
- Tier 5 stays labelled `(scored at run time)`. Recompute-on-read for tiers 1–4 is
  unchanged for every arm — the stored prefix/suffix/gold are the same text.

> Amended 2026-08-30: three wording changes.
>
> - The `context:` sentence ends `; tiers 1-4 score the first gold_lines lines
>   of each fill` (§6, amended), before the elision and exclusion clauses.
> - `none sampled` is decided from the run's **own rows**, not from the rows
>   that survived the unavailable filter. A lane that was sampled and whose
>   every crossing failed prints
>   `cross_file_first        all N crossings unavailable — <reason>`; only a
>   run with no cross-file row at all says `none sampled`. The old line blamed
>   the repository for the server.
> - `excluded.cross_file` is arm-aware, built per row in
>   `run::record_codebase_task`: the `extra` row keeps
>   `sent <G> (<KiB>[, truncated]); withheld <k> (contain the answer)`, and
>   the `no_extra` row reads
>   `defining file <G> (<KiB>[, truncated]) withheld from this arm; withheld
>   <k> (contain the answer)`. A row has to be true read on its own.

## 8. Errors and edge cases

No new `ChekovError` variants. A repository with no unambiguous cross-file first use
runs the other 18 tasks and reports the tier's shortfall with its reason; only a
repository with **zero** candidates in every tier is `CodebaseNoTasks`. A defining
file G larger than 32 KiB is windowed (§4.1), never dropped. `G == F` cannot occur
(rule 1). A candidate whose gold text appears verbatim in G itself (a second call
site in the defining file) stays a task — G is the definition, and the report's
`withheld` count makes the leakage surface auditable rather than pretending it away.

## 9. Tests

- Detection (`codebase::crossfile` unit tests on fixture texts): first use wins over
  a later use; a `use` line is never the mask; a name declared in two files is
  skipped and counted; a name declared in F is skipped; a prelude name is skipped;
  call shapes `name(`, `name::`, `.name(`, `name {` each detected; a use inside a
  string literal or comment is not a use.
- Extra assembly: under-cap file sent whole; over-cap file windowed on the
  declaration line with `truncated = true` and exact `bytes`; rule (b) withholding
  counts a verbatim-answer file and not G.
- Quota: `quota(24)` is 12/6/6; stratification ≤ 3 per file holds for the new lane;
  `task_set_hash` differs when a cross-file id is added.
- Crossing: `cross_infill` posts `input_extra` verbatim with the chunk, `[]`
  without; both arms' task ids; `--resume` skips one arm and not the other.
- Scoring: with-arm `Known.context` includes G's names; without-arm does not.
- Storage/report: row round-trip with `arm`/`extra`/`also_first_uses`; a slice-A row
  without them loads; the three report lines with exact strings; the lift over
  tasks present in both arms only; the all-unavailable and mixed cases still render
  as slice A specifies.
- Live: one run of `ornith-1.5-35b-a3b` on a clean clone of this repo and one on a
  clean clone of pushkin; both blocks go in the PR body, with the honesty line
  "the set hash changed; not comparable with pre-B1 runs".

## 10. Files

`src/core/bench/codebase/crossfile.rs` (new: the index, detection, extra assembly,
rule (b)), `codebase/{mod,masker,sample,filter,ladder}.rs` (tier, quota, `Known`
context), `src/core/bench/codebase/run.rs` (new: the per-task loop moves here from
`commands/capability.rs`, gaining the two arms — the slice-A "run cluster lives in
the command layer" item, retired), `src/core/bench/runner.rs` (`ExtraChunk`,
`InfillTask.extra`), `src/core/bench/store.rs` (row fields, three report lines),
`src/commands/capability.rs` (estimate, dry-run line), the slice-A spec (a pointer
here), `README.md`, `CHANGELOG.md`, `IDEAS.md`.
