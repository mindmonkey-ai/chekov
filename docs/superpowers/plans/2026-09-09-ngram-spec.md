# n-gram spec stage Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `[tune] spec_drafts` accepts `ngram:<type>` for the engine's five history-based drafters; the spec stage skips per candidate (no head → `mtp:` only; the two stateful n-gram types by name; the engine's own type list by whole-token match), prints each speculative trial's draft acceptance, and `--apply` strips a stale draft length behind an n-gram winner.

**Architecture:** `core::tune` grows `NgramType` and the third `SpecDraft` arm, carries the draft counts on `Measured` and `Trial`, and prints the acceptance clause in `stage_line`. `commands::tune` replaces the whole-stage `Plan.spec_skip` with a per-candidate `SpecGate` (head reason, engine reason, the parsed `--spec-type` list) consulted by `spec_skip`, `max_launches` and `stage_plan_line`; `spec_incumbent` reads an n-gram incumbent; `foreign_spec_skip` keeps only what the stage cannot reason about. Config, error text and docs follow.

**Tech Stack:** Rust (edition 2024). No new dependencies.

**Spec:** `docs/superpowers/specs/2026-09-09-ngram-spec-design.md` — §1–§12 as approved, amended by §13, which wins where it differs.

## Global Constraints

- Functions ≤ 40 LOC, ≤ 3 arguments, nesting ≤ 3 (`clippy.toml`); pedantic+nursery under `-D warnings`: no boolean-flag parameters (split the function or pass an enum), `const fn` where clippy can prove it, `map_or_else` over `if let … else`, first doc paragraph one short sentence, no `similar_names`.
- `unwrap()`/`expect()` only in `#[cfg(test)]`; exhaustive `match` on our enums (a new `SpecDraft`/`NgramType` variant must break every match at compile time — never a wildcard arm on them); `#[serde(deny_unknown_fields)]` stays on `Trial`; new stored fields carry `#[serde(default)]` so every `tune/*.json` on disk loads.
- `tests/**` is read-only; every test is inline `#[cfg(test)]`. `src/core/hub.rs` is frozen (untouched here).
- Reading `src/`: `mcp__scout__file_outline` / `keyword_search` for line numbers, then `Read(file, offset, limit)`; never `cat`/`grep`/`sed`/`git diff` on a `src/` path (the whole Bash call is denied, and it strikes). The compiler is the grep for literals: `cargo build --manifest-path <repo>/Cargo.toml --all-targets --message-format=short 2>&1 | grep -E "error"`.
- Never `cd`. Gate by exit status, each step `|| exit 1`, never `&&` across a commit heredoc: `cargo fmt`, `make -C <repo> lint`, `make -C <repo> test`. A red commit may fail `make test`; it must pass `cargo fmt --check`.
- Commits: `test(tune): red — …` then `feat(tune): …`, each ending `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`. Branch `feat/tune-ngram-spec` from `origin/develop`; PR base `develop`; nothing pushed until the last task.
- `<repo>` = `/Users/amoscoletti/personal_dev/chekov`.

## File map

| file | responsibility |
|---|---|
| `src/core/tune.rs` | `NgramType`, `SpecDraft::Ngram`, `parse`/`label`, `apply_spec`, `applied_extra_flags`' new rule, `Measured` (+2), `classify`, `Trial` (+2), `stage_line`'s acceptance clause |
| `src/commands/tune.rs` | `SpecGate` replacing `Plan.spec_skip`; `spec_gate`, `engine_gate` (type list), the two stateful-type skip, `spec_skip` per candidate, `spec_incumbent`, `foreign_spec_skip`, `max_launches`, `stage_plan_line`, `trial_row`, `measured_of`, `carried`, `baseline_line` |
| `src/core/config.rs`, `src/error.rs`, `config.example.toml` | the doc comment, the remedy text, the example spelling |
| `README.md`, `CHANGELOG.md`, `IDEAS.md`, `docs/superpowers/specs/2026-09-01-tune-spec-stage-design.md` | docs and status lines |

---

### Task 1: The grammar — `NgramType`, `SpecDraft::Ngram`, the rewrite, the `--apply` rule, the remedy text

**Files:**
- Modify: `src/core/tune.rs` (`SpecDraft` 184-216, `apply_spec` 228-237, `applied_extra_flags` 270-281; tests `the_spec_grammar_is_off_or_mtp_n` ~1295, `apply_strips_the_spec_flags_when_the_winner_dropped_them` ~1384)
- Modify: `src/error.rs` (`TuneBadSpecCandidate` ~443-446)
- Modify: `src/core/config.rs` (`spec_drafts` doc ~211-213)
- Test: inline in `src/core/tune.rs`, `src/error.rs`

**Interfaces:**
- Produces: `pub enum NgramType { Simple, MapK, MapK4v, Mod, Cache }` with `pub const fn label(self) -> &'static str` (the engine spelling: `ngram-simple`, `ngram-map-k`, `ngram-map-k4v`, `ngram-mod`, `ngram-cache`), `pub fn parse(name: &str) -> Option<Self>`, `pub const fn keeps_memory(self) -> bool` (`Mod` and `Cache`), `pub const ALL: [NgramType; 5]`; `SpecDraft::Ngram(NgramType)` with `label()` = `ngram:<engine spelling>`; `apply_spec` writes `--spec-type <spelling>` and strips `--spec-draft-n-max` for it; `applied_extra_flags` strips a current `--spec-draft-n-max` whenever the winner lacks it. Tasks 3–4 consume `NgramType`.

- [ ] **Step 1: Write the failing tests**

In `core/tune.rs`'s test module, extend the `use super::{…}` line with `NgramType`, and add beside `the_spec_grammar_is_off_or_mtp_n` (do not grow that test; it is near the cap):

```rust
    #[test]
    fn the_spec_grammar_accepts_the_five_ngram_types_and_refuses_the_rest() {
        for (spelling, expected) in [
            ("ngram:ngram-simple", NgramType::Simple),
            ("ngram:ngram-map-k", NgramType::MapK),
            ("ngram:ngram-map-k4v", NgramType::MapK4v),
            ("ngram:ngram-mod", NgramType::Mod),
            ("ngram:ngram-cache", NgramType::Cache),
        ] {
            let parsed = SpecDraft::parse(spelling).expect(spelling);
            assert_eq!(parsed, SpecDraft::Ngram(expected));
            assert_eq!(parsed.label(), spelling, "the label is the spelling");
            assert_eq!(expected.label(), &spelling["ngram:".len()..]);
        }
        for bad in ["ngram:", "ngram:mtp", "ngram:ngram-simple,ngram-mod", "ngram:ngram-map-k4"] {
            let err = SpecDraft::parse(bad).expect_err(bad);
            assert!(
                matches!(&err, ChekovError::TuneBadSpecCandidate { value } if value == bad),
                "{bad}: {err}"
            );
        }
        assert!(NgramType::Mod.keeps_memory() && NgramType::Cache.keeps_memory());
        assert!(!NgramType::Simple.keeps_memory());
        assert_eq!(NgramType::ALL.len(), 5);
    }

    #[test]
    fn an_ngram_candidate_writes_the_type_and_strips_the_draft_length_only() {
        let cfg = TuneSection {
            spec_drafts: vec!["off".into(), "ngram:ngram-mod".into()],
            ..TuneSection::default()
        };
        let drafted = argv(&[
            "--spec-type",
            "draft-mtp",
            "--spec-draft-n-max",
            "3",
            "--spec-ngram-mod-n-match",
            "24",
        ]);
        let spec = candidates(Stage::Spec, &drafted, &cfg);
        let values: Vec<&str> = spec.iter().map(|c| c.value.as_str()).collect();
        assert_eq!(values, vec!["off", "ngram:ngram-mod"]);
        assert_eq!(
            spec[1].argv,
            argv(&["--spec-type", "ngram-mod", "--spec-ngram-mod-n-match", "24"]),
            "the type is rewritten in place, the length stripped, the engine's own knob kept"
        );
    }

    #[test]
    fn apply_strips_a_stale_draft_length_behind_an_ngram_winner() {
        let current = argv(&["--temp", "0.6", "--spec-type", "draft-mtp", "--spec-draft-n-max", "3"]);
        let ngram_won = argv(&["--temp", "0.6", "--spec-type", "ngram-mod"]);
        assert_eq!(
            super::applied_extra_flags(&current, &ngram_won),
            argv(&["--temp", "0.6", "--spec-type", "ngram-mod"]),
            "a winner without the length lost it in the stage; the current flags lose it too"
        );
        let mtp_won = argv(&["--temp", "0.6", "--spec-type", "draft-mtp", "--spec-draft-n-max", "1"]);
        assert_eq!(super::applied_extra_flags(&current, &mtp_won), mtp_won);
    }
```

In `src/error.rs`'s test `the_tune_refusals_name_the_remedy` (~681), find the assertion on `TuneBadSpecCandidate`'s text and change its expected substring to `expected "off", "mtp:<n>" with n ≥ 1, or "ngram:<type>"` (read the test first and match its exact form).

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --manifest-path <repo>/Cargo.toml -- ngram_types ngram_candidate stale_draft 2>&1 | grep -E "^error" | sort | uniq -c | head -4`
Expected: `cannot find NgramType`, `no variant Ngram`.

- [ ] **Step 3: Commit the red**

```bash
cargo fmt --manifest-path <repo>/Cargo.toml || exit 1
git -C <repo> add src/core/tune.rs src/error.rs || exit 1
git -C <repo> commit -F - <<'EOF' || exit 1
test(tune): red — ngram:<type> is a third spec spelling, writes the type and strips the length, and --apply strips a stale length behind it

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
EOF
```

- [ ] **Step 4: Implement**

In `core/tune.rs`, above `SpecDraft`:

```rust
/// One of the engine's history-based drafters (`--spec-type ngram-*`): no
/// head, no draft file — it proposes runs of tokens the model already saw.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NgramType {
    Simple,
    MapK,
    MapK4v,
    Mod,
    Cache,
}

impl NgramType {
    pub const ALL: [Self; 5] = [Self::Simple, Self::MapK, Self::MapK4v, Self::Mod, Self::Cache];

    /// The engine's own spelling, as `--spec-type` takes it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Simple => "ngram-simple",
            Self::MapK => "ngram-map-k",
            Self::MapK4v => "ngram-map-k4v",
            Self::Mod => "ngram-mod",
            Self::Cache => "ngram-cache",
        }
    }

    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.label() == name)
    }

    /// Whether the drafter keeps its table across requests (design §13):
    /// `ngram-mod` shares one table over every sequence and resets it only
    /// on occupancy; `ngram-cache`'s per-request reset is a no-op. On a probe
    /// that repeats one prompt, both replay the first reply.
    #[must_use]
    pub const fn keeps_memory(self) -> bool {
        matches!(self, Self::Mod | Self::Cache)
    }
}
```

`SpecDraft` gains `Ngram(NgramType)`; `parse` gains, before the `mtp:` line:

```rust
        if let Some(name) = value.strip_prefix("ngram:") {
            return NgramType::parse(name).map(Self::Ngram).ok_or_else(bad);
        }
```

`label` gains `Self::Ngram(t) => format!("ngram:{}", t.label()),`. `apply_spec` gains the arm:

```rust
        Ok(SpecDraft::Ngram(t)) => {
            let typed = rewrite(incumbent, Flag::SpecType, t.label());
            strip(&typed, Flag::SpecDraftNMax)
        }
```

`applied_extra_flags`: replace the trailing strip with

```rust
    if value_of(winner, Flag::SpecType).is_none() {
        out = strip_spec(&out);
    } else if value_of(winner, Flag::SpecDraftNMax).is_none() {
        // The winner is a full argv derived from `current`: a length it
        // lacks is one the stage removed (an n-gram winner), never one it
        // forgot (design §13).
        out = strip(&out, Flag::SpecDraftNMax);
    }
```

`error.rs`: `"tune: [tune] spec_drafts entry '{value}' — expected \"off\", \"mtp:<n>\" with n ≥ 1, or \"ngram:<type>\" (ngram-simple, ngram-map-k, ngram-map-k4v, ngram-mod, ngram-cache)"`. `config.rs` `spec_drafts` doc: `/// Stage spec: \`off\`, \`mtp:<n>\` — llama.cpp's native MTP draft head at draft length \`n\` — or \`ngram:<type>\`, one of the engine's five history-based drafters. Validated at plan time, not here.`

- [ ] **Step 5: Run, gate, commit**

```bash
cargo fmt --manifest-path <repo>/Cargo.toml || exit 1
make -C <repo> lint >/tmp/ng-lint.log 2>&1 || { head -40 /tmp/ng-lint.log; exit 1; }
make -C <repo> test >/tmp/ng-test.log 2>&1 || { grep -E "FAILED|panicked|^error" /tmp/ng-test.log | head; exit 1; }
git -C <repo> add src/core/tune.rs src/error.rs src/core/config.rs || exit 1
git -C <repo> commit -F - <<'EOF' || exit 1
feat(tune): ngram:<type> — five history-based drafters as a third spec spelling; the type is written and the draft length stripped; --apply strips a stale length behind an n-gram winner

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
EOF
```

---

### Task 2: The draft counts on `Measured` and `Trial`; the acceptance clause

**Files:**
- Modify: `src/core/tune.rs` (`Measured` 377-381, `classify` 402-417, `stage_line` 586-599 + a new `accept_note`, `Trial` 651-670; test helper `measured` ~914, `sample_record` ~1176, DepthResult fixture ~1044)
- Modify: `src/commands/tune.rs` (`measured_of` ~427, `trial_row` ~510, `carried` ~545, `baseline_line` ~436; test fixtures `measured_trial` ~1008 and the `Measured` literal ~1506)
- Test: inline in both

**Interfaces:**
- Produces: `Measured { …, draft_n: u64, draft_n_accepted: u64 }`; `Trial { …, #[serde(default)] draft_n: u64, #[serde(default)] draft_n_accepted: u64 }`; `pub fn accept_note(label: &CandidateLabel, measured: &Measured) -> String` (`   acceptance 63% (189 of 300 drafted)` when `draft_n > 0`; `   no drafts` when the candidate is a spec-stage `mtp:`/`ngram:` value and `draft_n == 0`; empty otherwise).

- [ ] **Step 1: Write the failing tests**

`core/tune.rs`, beside `a_stage_line_carries_the_cells_the_phrase_and_the_dirty_clock` (a sibling, not an extension):

```rust
    #[test]
    fn a_stage_line_prints_the_acceptance_when_the_trial_drafted_and_no_drafts_only_on_a_spec_candidate() {
        let drafted = super::Measured {
            draft_n: 300,
            draft_n_accepted: 189,
            ..measured(74.7, 126.0)
        };
        let line = super::stage_line(
            &super::CandidateLabel { stage: super::Stage::Fa, value: "off" },
            &super::Outcome::Measured(drafted),
            &super::LineContext { verdict: None, dirty: None },
        );
        assert!(line.ends_with("   acceptance 63% (189 of 300 drafted)"), "{line}");
        let dry = super::Outcome::Measured(measured(60.0, 140.0));
        let ngram = super::CandidateLabel { stage: super::Stage::Spec, value: "ngram:ngram-simple" };
        let line = super::stage_line(&ngram, &dry, &super::LineContext { verdict: None, dirty: None });
        assert!(line.ends_with("   no drafts"), "{line}");
        let off = super::CandidateLabel { stage: super::Stage::Spec, value: "off" };
        let line = super::stage_line(&off, &dry, &super::LineContext { verdict: None, dirty: None });
        assert!(!line.contains("drafts"), "off never drafted by design: {line}");
        let kv = super::CandidateLabel { stage: super::Stage::Kv, value: "f16" };
        let line = super::stage_line(&kv, &dry, &super::LineContext { verdict: None, dirty: None });
        assert!(!line.contains("drafts"), "{line}");
    }

    #[test]
    fn classify_carries_the_draft_counts_and_a_record_from_before_them_loads_with_zeros() {
        let mut result = depth_result(&[30.0, 31.0, 31.5], &[400.0, 402.0, 404.0]);
        result.draft_n = 300;
        result.draft_n_accepted = 189;
        let super::Outcome::Measured(m) = super::classify(&result, 4096) else {
            panic!("measured");
        };
        assert_eq!((m.draft_n, m.draft_n_accepted), (300, 189));
        let record = sample_record(argv(&[]), crate::core::bench::stamp::launch_flags(&[]));
        let json = serde_json::to_string(&record).expect("ser");
        assert!(json.contains("\"draft_n\":0,"), "{json}");
        let old = json.replace("\"draft_n\":0,", "").replace("\"draft_n_accepted\":0,", "");
        let back: super::Record = serde_json::from_str(&old).expect("a pre-count record loads");
        assert_eq!(back.trials[0].draft_n, 0);
    }
```

(Read the test module for the existing `DepthResult` fixture in `a_trial_that_did_not_reach_the_depth…` and either reuse its helper or add `fn depth_result(decode: &[f64], prefill: &[f64]) -> DepthResult` mirroring that literal with `depth: 4096, prompt_n: 4101` and `summarize` on both sample sets; `measured` gets `draft_n: 0, draft_n_accepted: 0`; if `Measured` has no `..` support because it lacks `Clone`, build the drafted one as a full literal.)

`commands/tune.rs`, a test beside `the_report_ends_with_the_record_and_how_to_apply_or_says_defaults_won`:

```rust
    #[test]
    fn the_baseline_line_prints_its_acceptance_when_it_drafted() {
        let mut trial = measured_trial("baseline", None, argv(&["--spec-type", "ngram-mod"]));
        trial.draft_n = 40;
        trial.draft_n_accepted = 10;
        let line = super::baseline_line(&trial);
        assert!(line.contains("acceptance 25% (10 of 40 drafted)"), "{line}");
        let plain = measured_trial("baseline", None, argv(&[]));
        assert!(!super::baseline_line(&plain).contains("drafts"));
    }
```

- [ ] **Step 2: Run to verify they fail** — `cargo test … -- acceptance draft_counts 2>&1 | grep -E "^error" | sort | uniq -c | head -4` → `no field draft_n`.

- [ ] **Step 3: Commit the red** (`test(tune): red — the draft counts ride Measured and Trial; a stage line prints acceptance or no drafts; a pre-count record loads`).

- [ ] **Step 4: Implement**

`Measured` gains `pub draft_n: u64, pub draft_n_accepted: u64` (doc: `/// Draft tokens proposed and accepted over every repetition, the warmup included — zero-both when nothing drafted.`); `classify` copies `result.draft_n`, `result.draft_n_accepted`. `Trial` gains, after `prompt_n`:

```rust
    /// Draft tokens proposed and accepted over the probe's repetitions
    /// (design §13). Records from before the fields load as zero-both.
    #[serde(default)]
    pub draft_n: u64,
    #[serde(default)]
    pub draft_n_accepted: u64,
```

Beside `dirty_note`:

```rust
/// `   acceptance 63% (189 of 300 drafted)` for a trial that drafted;
/// `   no drafts` for a spec-stage candidate that did not — the expected
/// reading of a history-based drafter on a prompt with nothing to match
/// (design §13); nothing for any other trial.
pub fn accept_note(label: &CandidateLabel, measured: &Measured) -> String {
    if measured.draft_n > 0 {
        let pct = u128::from(measured.draft_n_accepted) * 200 / u128::from(measured.draft_n);
        return format!(
            "   acceptance {}% ({} of {} drafted)",
            pct.div_ceil(2).min(100),
            measured.draft_n_accepted,
            measured.draft_n
        );
    }
    let speculative = label.stage == Stage::Spec && label.value != "off";
    if speculative {
        "   no drafts".to_owned()
    } else {
        String::new()
    }
}
```

`stage_line`'s `Measured` arm: `let accept = accept_note(label, measured);` and the format becomes `"  {stage:<10} {value:<8} {cells}   {phrase}{note}{accept}"` — the acceptance goes last, after the dirty-clock note (the order the test asserts with `ends_with`; if the dirty test asserts an exact full line, it has no drafts and is unchanged).

`commands/tune.rs`: `measured_of` reads `draft_n: trial.draft_n, draft_n_accepted: trial.draft_n_accepted`; `trial_row` writes `draft_n: measured.map_or(0, |m| m.draft_n), draft_n_accepted: measured.map_or(0, |m| m.draft_n_accepted)`; `carried` copies both; `baseline_line` appends `tune::accept_note(&label, &m)` for the measured baseline with `label = CandidateLabel { stage: Stage::Spec, value: "off" }` — no: that would print nothing for a drafting baseline under the `speculative` branch only when draft_n == 0, and `acceptance …` when it drafted, which is exactly right; use `Stage::Spec`/`"off"` so an undrafting baseline prints nothing. Every `Measured`/`Trial` literal the compiler names gets zeros.

- [ ] **Step 5: Run, gate, commit** (`feat(tune): the draft counts ride Measured and Trial; every drafting trial prints its acceptance in the bench's words, a spec candidate that drafted nothing says no drafts`).

---

### Task 3: The per-candidate gate — `SpecGate`, the type list, the stateful-type skip, the plan line

**Files:**
- Modify: `src/commands/tune.rs` (`Plan` 47-58, `TuneCmd::plan` ~873-884, `max_launches` 201-208, `stage_plan_line` 212-232, `engine_gate` 360-367, `spec_gate` 369-380, `spec_skip` 394-407; tests `the_engine_gate_names_an_engine_without_the_flag_and_trusts_an_unreadable_help` ~1427, `a_gated_spec_stage_counts_no_launches_and_says_why_on_the_plan` ~1465, `a_gated_spec_candidate_is_skipped_before_any_spawn` ~1492, `plan_for` ~982)
- Test: inline

**Interfaces:**
- Produces: `struct SpecGate { head: Option<String>, engine: Option<String>, types: Option<Vec<String>> }` on `Plan` as `spec_gate: SpecGate` (replacing `spec_skip`); `fn spec_types(help: &str) -> Option<Vec<String>>` (the `--spec-type` line's second token split on commas); `fn spec_skip(plan, candidate, incumbent) -> Option<String>` deciding per candidate.

- [ ] **Step 1: Write the failing tests**

Rewrite `the_engine_gate_names_an_engine_without_the_flag_and_trusts_an_unreadable_help` to also assert `super::spec_types("--spec-type none,draft-mtp,ngram-map-k\n   more") == Some(vec!["none","draft-mtp","ngram-map-k"])` (as `String`s) and `super::spec_types("--flash-attn") == None`. Rewrite `a_gated_spec_stage_counts_no_launches_and_says_why_on_the_plan` so that a head-gated plan with the DEFAULT list prints `  spec       4 candidates   (1 is the incumbent; mtp skipped: no MTP head in the GGUF (nextn_predict_layers 0))\n` and `max_launches == 10` (unchanged count: `off` is the incumbent, the three mtp cost nothing), and an ENGINE-gated plan (`engine: Some(…)`) prints the whole-stage `(skipped: engine … has no --spec-type — chekov update --engine)` form. Add:

```rust
    #[test]
    fn a_headless_model_trials_the_ngram_candidates_and_counts_them() {
        let tune = TuneSection {
            spec_drafts: vec!["off".into(), "mtp:1".into(), "ngram:ngram-simple".into(), "ngram:ngram-mod".into()],
            ..TuneSection::default()
        };
        let mut plan = plan_for(&tune, &[]);
        plan.spec_gate = super::SpecGate {
            head: Some("no MTP head in the GGUF (nextn_predict_layers 0)".into()),
            engine: None,
            types: Some(super::spec_types("--spec-type none,draft-mtp,ngram-simple,ngram-mod").expect("list")),
        };
        let skip = |value: &str| {
            let candidate = Candidate { stage: Stage::Spec, value: value.into(), argv: vec![] };
            super::spec_skip(&plan, &candidate, &[])
        };
        assert_eq!(skip("mtp:1").as_deref(), Some("no MTP head in the GGUF (nextn_predict_layers 0)"));
        assert!(skip("ngram:ngram-simple").is_none(), "no head needed");
        assert_eq!(
            skip("ngram:ngram-mod").as_deref(),
            Some("the engine keeps ngram-mod's draft memory across requests; the tune probe repeats one prompt, so every repetition after the first would replay the reply — not measurable on this probe")
        );
        assert!(skip("off").is_none());
        assert_eq!(super::max_launches(&plan), 1 + 1 + 2 + 1 + 3 + 3, "off is the incumbent; only ngram-simple launches in the spec stage");
        let text = super::plan_text(&plan, None, None);
        assert!(text.contains("  spec       4 candidates   (1 is the incumbent; mtp skipped: no MTP head in the GGUF (nextn_predict_layers 0))\n"), "{text}");
    }

    #[test]
    fn an_engine_whose_type_list_lacks_a_name_skips_exactly_that_candidate() {
        let tune = TuneSection {
            spec_drafts: vec!["off".into(), "ngram:ngram-simple".into(), "ngram:ngram-map-k4v".into()],
            ..TuneSection::default()
        };
        let mut plan = plan_for(&tune, &[]);
        plan.spec_gate = super::SpecGate {
            head: None,
            engine: None,
            types: Some(super::spec_types("--spec-type none,draft-mtp,ngram-simple,ngram-map-k").expect("list")),
        };
        let skip = |value: &str| {
            let candidate = Candidate { stage: Stage::Spec, value: value.into(), argv: vec![] };
            super::spec_skip(&plan, &candidate, &[])
        };
        assert!(skip("ngram:ngram-simple").is_none());
        assert!(
            skip("ngram:ngram-map-k4v").as_deref().is_some_and(|r| r.contains("has no ngram-map-k4v in --spec-type")),
            "a whole-token match: ngram-map-k is not ngram-map-k4v"
        );
    }
```

`plan_for` sets `spec_gate: super::SpecGate::default()` (derive `Default`). In `a_gated_spec_candidate_is_skipped_before_any_spawn`, replace the `plan.spec_skip = Some(…)` line with `plan.spec_gate.head = Some(…)`.

- [ ] **Step 2: Run to verify they fail** — compile errors on `SpecGate`, `spec_types`, `spec_gate` field.

- [ ] **Step 3: Commit the red** (`test(tune): red — the spec gate decides per candidate: mtp on the head, the two stateful n-gram types by name, every type against the engine's own list`).

- [ ] **Step 4: Implement**

```rust
/// Why the spec stage's candidates cannot measure on this machine, decided
/// once at plan time and applied per candidate (design §13).
#[derive(Debug, Default, Clone)]
struct SpecGate {
    /// Skip 1: no MTP head — gates the `mtp:` candidates only.
    head: Option<String>,
    /// Skip 2a: an engine with no `--spec-type` at all — gates every
    /// candidate but `off`.
    engine: Option<String>,
    /// The types the engine's `--spec-type` line lists; `None` when the
    /// help could not be captured (the launch-time assertion's problem).
    types: Option<Vec<String>>,
}
```

`Plan.spec_skip` → `spec_gate: SpecGate`; `TuneCmd::plan` builds it with `spec_gate(ctx, &eff)` when the stage runs, else `SpecGate::default()`.

```rust
/// The engine's `--spec-type` type list off its `--help`: the line whose
/// first token is the flag, its second token split on commas. Whole
/// tokens, because `ngram-map-k` is a prefix of `ngram-map-k4v` and every
/// type name recurs inside the per-type knob flags.
fn spec_types(help: &str) -> Option<Vec<String>> {
    help.lines()
        .map(str::trim_start)
        .find(|line| line.starts_with("--spec-type"))
        .and_then(|line| line.split_whitespace().nth(1))
        .map(|list| list.split(',').map(str::to_owned).collect())
}

/// Skips 1 and 2, decided once per run before the confirm gate. The help
/// is read even when the head is absent: an n-gram candidate on a headless
/// model still needs the engine's word.
fn spec_gate(ctx: &Ctx, eff: &Effective) -> SpecGate {
    let cfg = &ctx.config;
    let shard = crate::core::server::shard_path(cfg, eff);
    let help = lifecycle::server_help(&cfg.engine_dir());
    let commit = crate::core::engine::current_commit(&cfg.engine_dir())
        .unwrap_or_else(|| "unknown".to_owned());
    SpecGate {
        head: head_gate(crate::core::gguf::read_geometry(&shard), &shard),
        engine: engine_gate(help.as_deref(), &commit),
        types: help.as_deref().and_then(spec_types),
    }
}
```

(`engine_gate` keeps its shape: `None` help → `None`; help without the flag → the reason.) `spec_skip`:

```rust
/// Every pre-launch skip the spec stage names, per candidate (design §13):
/// `off` never; `mtp:` on the head; the two memory-keeping n-gram types by
/// name; every speculative type against the engine's own list.
fn spec_skip(plan: &Plan, candidate: &tune::Candidate, incumbent: &[String]) -> Option<String> {
    if candidate.stage != Stage::Spec {
        return None;
    }
    let gate = &plan.spec_gate;
    let draft = tune::SpecDraft::parse(&candidate.value).ok()?;
    let gated = match draft {
        tune::SpecDraft::Off => None,
        tune::SpecDraft::Mtp(_) => gate.head.clone().or_else(|| gate.engine.clone()),
        tune::SpecDraft::Ngram(t) if t.keeps_memory() => Some(memory_skip(t)),
        tune::SpecDraft::Ngram(_) => gate.engine.clone(),
    };
    gated
        .or_else(|| type_skip(gate, draft))
        .or_else(|| foreign_spec_skip(candidate, incumbent))
}

/// Skip 4: a drafter whose table outlives the request (design §13).
fn memory_skip(t: tune::NgramType) -> String {
    format!(
        "the engine keeps {}'s draft memory across requests; the tune probe repeats one \
         prompt, so every repetition after the first would replay the reply — not \
         measurable on this probe",
        t.label()
    )
}

/// Skip 2b: the engine's list lacks the candidate's type.
fn type_skip(gate: &SpecGate, draft: tune::SpecDraft) -> Option<String> {
    let name = match draft {
        tune::SpecDraft::Off => return None,
        tune::SpecDraft::Mtp(_) => "draft-mtp",
        tune::SpecDraft::Ngram(t) => t.label(),
    };
    let types = gate.types.as_ref()?;
    (!types.iter().any(|t| t == name))
        .then(|| format!("the engine has no {name} in --spec-type — chekov update --engine"))
}
```

(The `type_skip` message carries no commit because `SpecGate` does not store one; if the existing engine-gate test wording must be matched, add `commit: String` to `SpecGate` — decide by reading the test.) `max_launches` counts per candidate:

```rust
fn max_launches(plan: &Plan) -> usize {
    1 + plan
        .stages
        .iter()
        .map(|&stage| {
            let argv = counted_against(plan, stage);
            planned(stage, &argv, plan.tune)
                .iter()
                .filter(|c| spec_skip(plan, c, &argv).is_none())
                .count()
        })
        .sum::<usize>()
}
```

`stage_plan_line`: the whole-stage `(skipped: …)` form only for `gate.engine`; otherwise the incumbent form with a `partial_skip_note`: `; mtp skipped: <head reason>` when `gate.head` is set and the list holds an `mtp:` entry; the standing note becomes `; needs an MTP head in the GGUF for mtp candidates` only when the list holds an `mtp:` entry and the head is present. Keep the function under 40 lines by extracting `spec_plan_note(plan) -> String`.

- [ ] **Step 5: Run, gate, commit** (`feat(tune): the spec gate decides per candidate — mtp on the head, the two memory-keeping n-gram types by name, every type against the engine's own list; the launch ceiling counts only what launches`).

---

### Task 4: The incumbent and the foreign skip

**Files:**
- Modify: `src/commands/tune.rs` (`spec_incumbent` 144-156, `foreign_spec_skip` 382-392; tests `the_spec_incumbent_reads_off_mtp_n_and_the_engine_default_length` ~1371, `a_foreign_speculative_incumbent_skips_every_spec_candidate_and_only_those` ~1440)

- [ ] **Step 1: Write the failing tests**

Rewrite `a_foreign_speculative_incumbent_skips_every_spec_candidate_and_only_those`: `--spec-type ngram-mod` now yields `None` (it is the incumbent, §3); `--spec-type draft-simple` yields `Some("the spec stage tunes draft-mtp and the n-gram types; the incumbent runs --spec-type draft-simple")`; `--spec-type draft-mtp,ngram-mod` (a comma chain) and `--spec-type draft-mtp --spec-type ngram-mod` (a repeated flag) both `Some`; `--spec-type ngram-cache -lcs cache.bin` and `… --lookup-cache-dynamic d.bin` both `Some` naming the cache; `--spec-type ngram-cache` alone `None`; the `Fa` candidate stays `None`. Extend the incumbent test: `argv(&["--spec-type", "ngram-map-k"])` → `spec_incumbent == "ngram:ngram-map-k"`; `argv(&["--spec-type", "draft-simple"])` → `"off"`.

- [ ] **Step 2–3: Red, commit** (`test(tune): red — an n-gram incumbent is the incumbent; only draft-file types, chains and a lookup cache are foreign`).

- [ ] **Step 4: Implement**

```rust
fn spec_incumbent(incumbent: &[String]) -> String {
    match tune::value_of(incumbent, tune::Flag::SpecType).as_deref() {
        Some("draft-mtp") => {
            let length = tune::value_of(incumbent, tune::Flag::SpecDraftNMax)
                .and_then(|value| value.parse().ok())
                .unwrap_or(tune::ENGINE_DEFAULT_SPEC_DRAFT_N_MAX);
            tune::SpecDraft::Mtp(length).label()
        }
        Some(name) => tune::NgramType::parse(name)
            .map_or_else(|| engine_default(Stage::Spec), |t| tune::SpecDraft::Ngram(t).label()),
        None => engine_default(Stage::Spec),
    }
}

const LOOKUP_CACHE_FLAGS: [&str; 4] =
    ["-lcs", "--lookup-cache-static", "-lcd", "--lookup-cache-dynamic"];

/// Skip 3: what the stage cannot reason about (design §13) — a draft-file
/// type, a chain (a comma, or the flag repeated: the engine appends), or an
/// `ngram-cache` fed from a lookup-cache file.
fn foreign_spec_skip(candidate: &tune::Candidate, incumbent: &[String]) -> Option<String> {
    if candidate.stage != Stage::Spec {
        return None;
    }
    let value = tune::value_of(incumbent, tune::Flag::SpecType)?;
    let repeated = incumbent.iter().filter(|a| a.as_str() == "--spec-type").count() > 1;
    let cached = value == "ngram-cache"
        && incumbent.iter().any(|a| LOOKUP_CACHE_FLAGS.contains(&a.as_str()));
    let known = value == "draft-mtp" || tune::NgramType::parse(&value).is_some();
    (!known || value.contains(',') || repeated || cached).then(|| {
        format!("the spec stage tunes draft-mtp and the n-gram types; the incumbent runs --spec-type {value}")
    })
}
```

(Extend the message for the cache case with ` with a lookup cache` if the test asserts it — keep one `format!`.)

- [ ] **Step 5: Run, gate, commit** (`feat(tune): an n-gram incumbent is the incumbent; the foreign skip keeps only draft-file types, chains and a lookup cache`).

---

### Task 5: Docs, the closing caution, the status lines, push, PR

**Files:**
- Modify: `src/commands/tune.rs` (`report` ~491: append the caution line when any measured trial's `stage == "spec"` and `value` starts with `ngram:`), with a test beside `the_report_ends_with_the_record_and_how_to_apply_or_says_defaults_won`
- Modify: `config.example.toml:53` comment → `# stage spec: off, mtp:<n> (draft-mtp at that length), or ngram:<type> — the engine's five history-based drafters; opt in per machine`
- Modify: `README.md` tune row (line 107 sentence) → `` `spec` (llama.cpp's native MTP draft head via `--spec-type draft-mtp` at each configured `--spec-draft-n-max`, one of the engine's history-based n-gram drafters via `ngram:<type>`, or off) ``; the runbook's "What tune cannot see" paragraph gains one sentence: `An n-gram candidate (`ngram:<type>`) is measured on the same probe, which never repeats a twelve-token window, so its line reads `no drafts` and its verdict is the drafter's overhead — the report says so, and the two types that keep memory across requests (`ngram-mod`, `ngram-cache`) are skipped by name because a repeated prompt would replay the first reply as a fake win.`; the `[tune]` config block's `spec_drafts` comment.
- Modify: `CHANGELOG.md` (first bullet under Added), `IDEAS.md` (status → SHIPPED, live check owed), `docs/superpowers/specs/2026-09-01-tune-spec-stage-design.md` (a status line under §3 and §4 pointing at the new design).

- [ ] **Step 1: The caution** — in `report`, after the record line: when `record.trials.iter().any(|t| t.stage == "spec" && t.value.as_deref().is_some_and(|v| v.starts_with("ngram:")) && t.outcome == "measured")`, push `"  n-gram drafting depends on repetition in the workload; the probe is a poor instrument for it — confirm with a codebase bench under the candidate's flags and compare --cross-flags\n"`. Test: a record with a measured `ngram:ngram-simple` trial carries the line; one without does not.

- [ ] **Step 2: CHANGELOG entry**

```markdown
- `chekov tune`'s `spec` stage trials the engine's history-based drafters:
  `[tune] spec_drafts` accepts `ngram:<type>` (`ngram-simple`,
  `ngram-map-k`, `ngram-map-k4v`, `ngram-mod`, `ngram-cache`) beside `off`
  and `mtp:<n>`, writing `--spec-type <type>` and stripping
  `--spec-draft-n-max` (inert for these types). The default list is
  unchanged — opt in per machine. The stage now skips per candidate: no
  MTP head skips the `mtp:` candidates only (five of this registry's models
  are headless), the engine's own `--spec-type` list is matched by whole
  token, and two types are skipped by name before any launch —
  `ngram-mod` and `ngram-cache` keep their draft table across requests, so
  tune's repeated probe would replay the first reply as a fake win. The
  other three cannot draft on the probe at all (a counting reply never
  repeats a twelve-token window), so their line reads `no drafts` and the
  report closes with the caution that a codebase bench under the
  candidate's flags, read with `compare --cross-flags`, is the confirming
  measurement. Every drafting trial now prints its acceptance
  (`acceptance 63% (189 of 300 drafted)`, recorded on the trial), and
  `--apply` strips a stale draft length behind an n-gram winner. Live
  acceptance is owed to the stopped-driver window.
```

- [ ] **Step 3: Gate, commit, push, PR** (`docs: ngram:<type> in the README, CHANGELOG and the spec-stage status lines; the idea shipped, the live check owed`), then `git push -u origin feat/tune-ngram-spec` and `gh pr create --base develop --head feat/tune-ngram-spec --title "feat(tune): the spec stage trials the engine's n-gram drafters on headless models" --body …` (summarize the CHANGELOG entry; end with `🤖 Generated with [Claude Code](https://claude.com/claude-code)`).
