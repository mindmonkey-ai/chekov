# Reasoning stamp and thinking share — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Seven reasoning-side launch flags join the bench stamp (so `compare` refuses across them by name and stored runs still load and compare), every probe row records thinking and answer characters read off the upstream body, and the report prints one `thinking` line of per-suite median shares.

**Architecture:** `stamp::LaunchFlags` grows from eight to fifteen flag-sourced fields, read by one reader and copied onto `Stamp` by one `set_flags`; the run-log loader re-derives them from the stored argv so old runs hydrate instead of defaulting. A new `core::bench::thinkspan` module owns the thinking-tag table and the two counters (`reply_chars` for a buffered `message`, `stream_reply_chars` for an SSE body); `runner::Timings` and `store::Measure` carry the two counts through every door; `store::thinking_line` renders the medians.

**Tech Stack:** Rust (edition 2024), serde/serde_json already in tree. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-09-09-reasoning-stamp-design.md` — §1–§10 as approved, amended by §11 (seven flags, negative numbers, load-time hydration, unclosed spans, the tag table, the foreign alias, the forced-arm note, where the fold lives, `suite_summaries`, characters). §11 wins where it differs from an earlier section.

## Global Constraints

- Functions ≤ 40 LOC, ≤ 3 arguments, nesting ≤ 3 (`clippy.toml`); `#![warn(clippy::pedantic, clippy::nursery)]` with `-D warnings`: no `as` casts on counts (use `u64::try_from(x).unwrap_or(u64::MAX)`), a pure helper clippy can prove const must be `const fn`, `map_or_else` over `if let … else`, no `format!` inside `.collect::<String>()`, no `similar_names` (`set`/`sent`), first doc paragraph one short sentence.
- `unwrap()`/`expect()` only inside `#[cfg(test)]`; every fallible path returns `Result`.
- Exhaustive `match` on our own enums; every externally-deserialized struct `#[serde(deny_unknown_fields)]`; new fields on stored structs carry serde defaults so every `eval/*/results.jsonl`, `eval/*/stamp.json` and `tune/*.json` on disk still loads.
- `tests/**` is read-only under pushkin: every test is an inline `#[cfg(test)]` module. `src/core/hub.rs` is agent-frozen: no edit there (none is needed).
- Reading `src/`: `mcp__scout__file_outline` / `keyword_search` for line numbers, then `Read(file, offset, limit)`. No `cat`/`grep`/`sed`/`git diff` on a `src/` path, ever — the gate denies the whole Bash call and counts a strike per path. The compiler is the allowed grep: `cargo build --all-targets --message-format=short 2>&1 | grep -E "error"` names every literal a new field breaks.
- Never `cd`. Use `-C`/`--manifest-path` and absolute paths.
- Commit protocol: tests first as `test(bench): red — …`, then the implementation as `feat(bench): …`. Every commit message ends with the two trailers `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>` and `Claude-Session: https://claude.ai/code/session_01USTtBCMA5rsVnigD6fhoc3`.
- The gate before every green commit, by exit status, each step guarded with `|| exit 1` (never a bare `;` or an `&&` chain that a heredoc ends): `cargo fmt`, then `make -C <repo> lint` (fmt --check + clippy), then `make -C <repo> test`. A red commit may fail `make test`; it must pass `cargo fmt --check`.
- Branch `feat/bench-reasoning-stamp`, cut from `origin/develop` after PR #74 (the design) is merged; PR base is `develop`. Nothing is pushed until Task 9.
- Repo root: `/Users/amoscoletti/personal_dev/chekov` (called `<repo>` below).

## File map

| file | responsibility in this change |
|---|---|
| `src/core/bench/stamp.rs` | `LaunchFlags` (15 fields), the reader (`launch_flags`, `flag_value` with negative numbers, negated switches), `unmanaged_flags`, `Stamp` (+7), `Stamp::set_flags`, `first_mismatch` split into identity / flags / rest |
| `src/commands/capability.rs` | `assemble_stamp` split so the literal stays under 40 lines; test stamp fixtures |
| `src/core/bench/store.rs` | `RunLog::load` hydrates flags; `Measure` (+2); `thinking_line`; test fixtures |
| `src/core/bench/compare.rs` | 15/21 allow-lists, masks, tests |
| `src/core/bench/thinkspan.rs` (new) | tag table, `split_thinking`, `ReplyChars`, `reply_chars`, `stream_reply_chars` |
| `src/core/bench/runner.rs` | `Timings` (+2); counts attached on the buffered, llama.cpp-streamed and foreign-streamed doors |
| `src/core/bench/codebase/run.rs` | `empty_measure`, `probe_measure` |
| `src/core/bench/sweep.rs` | `DepthResult` (+2 sums) so throughput rows carry counts |
| `src/core/bench/toolloop.rs` | `fold` sums the counts; test fixture |
| `src/core/bench/speeds.rs`, `src/core/tune.rs`, `src/core/bench/mod.rs` | test literals the compiler names; `pub mod thinkspan`; the tune record test |
| `README.md`, `CHANGELOG.md`, `IDEAS.md` | the three "eight" mentions, the bench row, the entry, the status line |

---

### Task 1: The stamp — fifteen flag-sourced fields, one reader, one setter, a split mismatch table

**Files:**
- Modify: `src/core/bench/stamp.rs` (module doc line 1; `Stamp` 15-80; `LaunchFlags` 143-162; `launch_flags` 166-177; `unmanaged_flags` 181-193; `first_mismatch` 197-235; `flag_value` 258-266; tests from 278)
- Modify: every `Stamp { … }` literal the compiler names (`src/commands/capability.rs` `assemble_stamp` ~2464 and the test fixtures `eligible_stamp` ~3713; `src/core/bench/store.rs` `stamp()` ~1797; `src/core/bench/compare.rs` `stamp()` ~1234; `src/core/bench/speeds.rs` `stamp()` ~205; `src/core/bench/codebase/run.rs` `run_head` ~513)
- Test: inline in `src/core/bench/stamp.rs`

**Interfaces:**
- Produces: `LaunchFlags { …8 existing…, reasoning, reasoning_format, reasoning_effort, reasoning_budget, reasoning_budget_message, reasoning_preserve, chat_template_kwargs: String }` (all seven `#[serde(default = "engine_default_flag")]`); the same seven on `Stamp` after `spec_draft_n_max`; `pub fn switch_value(args, on: &str, off: &str) -> String`; `impl Stamp { pub fn set_flags(&mut self, flags: &LaunchFlags) }`; `pub const FLAG_FIELDS: usize = 15`. Tasks 2, 3, 8 consume these.

- [ ] **Step 1: Write the failing tests**

In `stamp.rs`'s test module, rename `launch_flags_read_all_eight` → `launch_flags_read_all_fifteen` and `unmanaged_is_eight_sentinels_and_a_six_field_record_loads` → `unmanaged_is_fifteen_sentinels_and_an_eight_field_record_loads` (keep their bodies; extend as below), and add:

```rust
    #[test]
    fn the_reasoning_flags_read_under_every_spelling_and_absence_is_engine_default() {
        let argv: Vec<String> = "-rea off --reasoning-format none --reasoning-effort low \
                                 --reasoning-budget -1 --reasoning-budget-message hurry \
                                 --no-reasoning-preserve --chat-template-kwargs {\"enable_thinking\":false}"
            .split(' ')
            .map(str::to_owned)
            .collect();
        let flags = launch_flags(&argv);
        assert_eq!(flags.reasoning, "off");
        assert_eq!(flags.reasoning_format, "none");
        assert_eq!(flags.reasoning_effort, "low");
        assert_eq!(flags.reasoning_budget, "-1", "a negative number is a value, not a switch");
        assert_eq!(flags.reasoning_budget_message, "hurry");
        assert_eq!(flags.reasoning_preserve, "off", "the negated spelling reads as off");
        assert_eq!(flags.chat_template_kwargs, "{\"enable_thinking\":false}");
        let long: Vec<String> = ["--reasoning", "auto", "--reasoning-preserve"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        let flags = launch_flags(&long);
        assert_eq!(flags.reasoning, "auto");
        assert_eq!(flags.reasoning_preserve, "on");
        let none = launch_flags(&[]);
        for value in [
            &none.reasoning,
            &none.reasoning_format,
            &none.reasoning_effort,
            &none.reasoning_budget,
            &none.reasoning_budget_message,
            &none.reasoning_preserve,
            &none.chat_template_kwargs,
        ] {
            assert_eq!(value, "engine-default");
        }
    }

    #[test]
    fn a_negative_number_after_a_flag_is_its_value() {
        let argv: Vec<String> = ["--reasoning-budget", "-1", "-fa"].iter().map(|s| (*s).to_owned()).collect();
        assert_eq!(flag_value(&argv, "--reasoning-budget"), "-1");
        assert_eq!(flag_value(&argv, "-fa"), "on", "a following flag is still a switch");
    }

    #[test]
    fn reasoning_effort_differs_after_spec_draft_n_max_and_before_allow_exec() {
        let a = stamp();
        let mut b = stamp();
        b.reasoning_effort = "high".into();
        b.allow_exec = true;
        assert_eq!(first_mismatch(&a, &b), Some("reasoning_effort"));
        let mut c = stamp();
        c.spec_draft_n_max = "2".into();
        c.reasoning_effort = "high".into();
        assert_eq!(first_mismatch(&a, &c), Some("spec_draft_n_max"));
        assert_eq!(super::FLAG_FIELDS, 15);
    }

    #[test]
    fn a_stamp_without_the_reasoning_fields_reads_as_engine_default_and_set_flags_copies_all_fifteen() {
        let json = serde_json::to_string(&stamp()).expect("ser");
        let stripped = json
            .replace("\"reasoning\":\"engine-default\",", "")
            .replace("\"reasoning_format\":\"engine-default\",", "")
            .replace("\"reasoning_effort\":\"engine-default\",", "")
            .replace("\"reasoning_budget\":\"engine-default\",", "")
            .replace("\"reasoning_budget_message\":\"engine-default\",", "")
            .replace("\"reasoning_preserve\":\"engine-default\",", "")
            .replace("\"chat_template_kwargs\":\"engine-default\",", "");
        assert_ne!(json, stripped, "the fixture carried the fields to strip");
        let parsed: Stamp = serde_json::from_str(&stripped).expect("an old stamp loads");
        assert_eq!(parsed.reasoning_format, "engine-default");
        let argv: Vec<String> = ["--reasoning-format", "none", "-b", "4096"].iter().map(|s| (*s).to_owned()).collect();
        let mut hydrated = parsed;
        hydrated.set_flags(&launch_flags(&argv));
        assert_eq!(hydrated.reasoning_format, "none");
        assert_eq!(hydrated.n_batch, "4096");
        assert_eq!(hydrated.reasoning_effort, "engine-default");
    }
```

Extend `launch_flags_read_all_fifteen`'s existing assertions with nothing (its argv sets none of the seven; add a final `assert_eq!(flags.reasoning_format, "engine-default");`). In `unmanaged_is_fifteen_sentinels_and_an_eight_field_record_loads`, extend the sentinel loop to the seven new fields and keep the eight-field JSON load assertion (it now proves the seven default).

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --manifest-path <repo>/Cargo.toml stamp 2>&1 | grep -E "^error" | sort | uniq -c | head`
Expected: `no field reasoning…`, `cannot find value FLAG_FIELDS`, `no method set_flags`.

- [ ] **Step 3: Commit the red**

```bash
cargo fmt --manifest-path <repo>/Cargo.toml
git -C <repo> add src/core/bench/stamp.rs
git -C <repo> commit -F - <<'EOF'
test(bench): red — fifteen flag-sourced fields, every reasoning spelling, a negative number is a value, an old stamp hydrates through set_flags

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01USTtBCMA5rsVnigD6fhoc3
EOF
```

- [ ] **Step 4: The flags and the reader**

Module doc line 1: `//! The 32-field configuration stamp (spec §7.4).`

`LaunchFlags` doc: `/// The fifteen flag-sourced values a launch argv pins.` Add after `spec_draft_n_max`:

```rust
    /// The seven reasoning-side flags (reasoning-stamp design §3, §11): how
    /// much the model may think and where its thoughts land. Records and
    /// stamps from before them load as engine-default and are hydrated from
    /// their argv where one is stored.
    #[serde(default = "engine_default_flag")]
    pub reasoning: String,
    #[serde(default = "engine_default_flag")]
    pub reasoning_format: String,
    #[serde(default = "engine_default_flag")]
    pub reasoning_effort: String,
    #[serde(default = "engine_default_flag")]
    pub reasoning_budget: String,
    #[serde(default = "engine_default_flag")]
    pub reasoning_budget_message: String,
    #[serde(default = "engine_default_flag")]
    pub reasoning_preserve: String,
    #[serde(default = "engine_default_flag")]
    pub chat_template_kwargs: String,
```

```rust
/// How many fields a launch argv pins — the size `unmanaged_flags` and the
/// mismatch table are checked against.
pub const FLAG_FIELDS: usize = 15;
```

`launch_flags` gains, after `spec_draft_n_max: …,`:

```rust
        reasoning: flag_value_either(argv, &["-rea", "--reasoning"]),
        reasoning_format: flag_value_either(argv, &["--reasoning-format"]),
        reasoning_effort: flag_value_either(argv, &["--reasoning-effort"]),
        reasoning_budget: flag_value_either(argv, &["--reasoning-budget"]),
        reasoning_budget_message: flag_value_either(argv, &["--reasoning-budget-message"]),
        reasoning_preserve: switch_value(argv, "--reasoning-preserve", "--no-reasoning-preserve"),
        chat_template_kwargs: flag_value_either(argv, &["--chat-template-kwargs"]),
```

(`launch_flags` is then 18 lines; fine.) `unmanaged_flags` gains seven `sentinel()` lines and its doc reads `/// Every launch flag of a foreign server, all fifteen unobservable.`

Replace `flag_value`'s match and add the switch reader:

```rust
/// One flag's value out of a launch argv. `--flag value` yields the value; a
/// bare switch yields "on"; an absent flag yields "engine-default". A next
/// token that parses as a number is a value even when it starts with `-`
/// (`--reasoning-budget -1` is llama-server's own spelling of unrestricted).
#[must_use]
pub fn flag_value(args: &[String], flag: &str) -> String {
    let Some(position) = args.iter().position(|a| a == flag) else {
        return FLAG_ENGINE_DEFAULT.to_owned();
    };
    match args.get(position + 1) {
        Some(next) if is_value(next) => next.clone(),
        _ => "on".to_owned(),
    }
}

fn is_value(token: &str) -> bool {
    !token.starts_with('-') || token.parse::<i64>().is_ok()
}

/// A switch with a negated spelling: `on` when the positive form is present,
/// `off` for the `--no-` form, engine-default when neither.
#[must_use]
pub fn switch_value(args: &[String], on: &str, off: &str) -> String {
    if args.iter().any(|a| a == off) {
        return "off".to_owned();
    }
    if args.iter().any(|a| a == on) {
        return "on".to_owned();
    }
    FLAG_ENGINE_DEFAULT.to_owned()
}
```

- [ ] **Step 5: The stamp, the setter, the split table**

`Stamp` gains the same seven fields after `spec_draft_n_max` with a doc on the first:

```rust
    /// The seven reasoning-side launch flags as the argv said them (design
    /// §11). `reasoning_format` is the server-wide launch value; the grammar
    /// arm's per-request override lives in `RunHead::forced_reasoning_format`.
    #[serde(default = "engine_default_flag")]
    pub reasoning: String,
    … (the other six exactly as on LaunchFlags)
```

Add after the struct:

```rust
impl Stamp {
    /// Copy every flag-sourced field from a read argv — the one place the
    /// stamp and the flags are joined, used by the writer and the loader alike.
    pub fn set_flags(&mut self, flags: &LaunchFlags) {
        self.kv_unified.clone_from(&flags.kv_unified);
        self.n_batch.clone_from(&flags.n_batch);
        self.n_ubatch.clone_from(&flags.n_ubatch);
        self.type_k.clone_from(&flags.type_k);
        self.type_v.clone_from(&flags.type_v);
        self.flash_attn.clone_from(&flags.flash_attn);
        self.spec_type.clone_from(&flags.spec_type);
        self.spec_draft_n_max.clone_from(&flags.spec_draft_n_max);
        self.reasoning.clone_from(&flags.reasoning);
        self.reasoning_format.clone_from(&flags.reasoning_format);
        self.reasoning_effort.clone_from(&flags.reasoning_effort);
        self.reasoning_budget.clone_from(&flags.reasoning_budget);
        self.reasoning_budget_message.clone_from(&flags.reasoning_budget_message);
        self.reasoning_preserve.clone_from(&flags.reasoning_preserve);
        self.chat_template_kwargs.clone_from(&flags.chat_template_kwargs);
    }
}
```

Replace `first_mismatch` with three functions that keep declaration order:

```rust
/// The FIRST differing field name, in declaration order — or `None` if equal.
#[must_use]
pub fn first_mismatch(a: &Stamp, b: &Stamp) -> Option<&'static str> {
    identity_mismatch(a, b)
        .or_else(|| flag_mismatch(a, b))
        .or_else(|| trailing_mismatch(a, b))
}

/// The fields before the launch flags.
fn identity_mismatch(a: &Stamp, b: &Stamp) -> Option<&'static str> {
    let pairs: [(&'static str, bool); 8] = [
        ("machine_id", a.machine_id != b.machine_id),
        ("runtime", a.runtime != b.runtime),
        ("timing_source", a.timing_source != b.timing_source),
        ("engine_build_commit", a.engine_build_commit != b.engine_build_commit),
        ("weights_revision", a.weights_revision != b.weights_revision),
        ("quant", a.quant != b.quant),
        ("ctx", a.ctx != b.ctx),
        ("n_parallel", a.n_parallel != b.n_parallel),
    ];
    first_differing(&pairs)
}

/// The fifteen flag-sourced fields, in `LaunchFlags` order.
fn flag_mismatch(a: &Stamp, b: &Stamp) -> Option<&'static str> {
    let pairs: [(&'static str, bool); FLAG_FIELDS] = [
        ("kv_unified", a.kv_unified != b.kv_unified),
        ("n_batch", a.n_batch != b.n_batch),
        ("n_ubatch", a.n_ubatch != b.n_ubatch),
        ("type_k", a.type_k != b.type_k),
        ("type_v", a.type_v != b.type_v),
        ("flash_attn", a.flash_attn != b.flash_attn),
        ("spec_type", a.spec_type != b.spec_type),
        ("spec_draft_n_max", a.spec_draft_n_max != b.spec_draft_n_max),
        ("reasoning", a.reasoning != b.reasoning),
        ("reasoning_format", a.reasoning_format != b.reasoning_format),
        ("reasoning_effort", a.reasoning_effort != b.reasoning_effort),
        ("reasoning_budget", a.reasoning_budget != b.reasoning_budget),
        ("reasoning_budget_message", a.reasoning_budget_message != b.reasoning_budget_message),
        ("reasoning_preserve", a.reasoning_preserve != b.reasoning_preserve),
        ("chat_template_kwargs", a.chat_template_kwargs != b.chat_template_kwargs),
    ];
    first_differing(&pairs)
}

/// The fields after the launch flags.
fn trailing_mismatch(a: &Stamp, b: &Stamp) -> Option<&'static str> {
    let pairs: [(&'static str, bool); 9] = [
        ("allow_exec", a.allow_exec != b.allow_exec),
        ("cargo_version", a.cargo_version != b.cargo_version),
        ("exec_target", a.exec_target != b.exec_target),
        ("seed", a.seed != b.seed),
        ("temperature_milli", a.temperature_milli != b.temperature_milli),
        ("chekov_version", a.chekov_version != b.chekov_version),
        ("prompt_set_hash", a.prompt_set_hash != b.prompt_set_hash),
        ("corpus_id", a.corpus_id != b.corpus_id),
        ("judge", a.judge != b.judge),
    ];
    first_differing(&pairs)
}

fn first_differing(pairs: &[(&'static str, bool)]) -> Option<&'static str> {
    pairs.iter().find(|(_, differs)| *differs).map(|(name, _)| *name)
}
```

- [ ] **Step 6: Every `Stamp` literal**

Run: `cargo build --manifest-path <repo>/Cargo.toml --all-targets --message-format=short 2>&1 | grep -E "missing field" | sort -u`

For each test fixture (`stamp()` in stamp.rs, store.rs, compare.rs, speeds.rs; `run_head` in codebase/run.rs; `eligible_stamp` in capability.rs) add after `spec_draft_n_max: "engine-default".into(),` the seven lines `reasoning: "engine-default".into(),` … `chat_template_kwargs: "engine-default".into(),` (a Python script over the exact anchor string is fine for the fixtures; each file's anchor is unique within it).

For `assemble_stamp` in `src/commands/capability.rs` (35 code lines; seven more would break the gate): replace the eight `kv_unified: parts.flags.kv_unified,` … `spec_draft_n_max: …` lines with placeholders and set the flags once at the end. Concretely: rename the current function `stamp_without_flags` and set every flag field to `String::new()` in it (eight existing lines become `kv_unified: String::new(),` etc. — no new lines for the seven, because the new function below sets all fifteen), then:

```rust
/// The stamp itself, given the setup and inputs `build_head` already has plus
/// everything else bundled in `parts` — the flags copied through the one
/// setter the loader also uses, so writer and reader cannot disagree.
fn assemble_stamp(
    setup: &Candidate,
    inputs: &HeadInputs,
    parts: StampParts,
) -> crate::core::bench::stamp::Stamp {
    let flags = parts.flags.clone();
    let mut stamp = stamp_without_flags(setup, inputs, parts);
    stamp.set_flags(&flags);
    stamp
}
```

(`StampParts.flags` is moved into `stamp_without_flags`; clone it first as shown. If `LaunchFlags` is not `Clone`, it derives `Clone` already — check the derive line at stamp.rs:149.)

- [ ] **Step 7: Run, gate, commit**

Run: `cargo test --manifest-path <repo>/Cargo.toml stamp 2>&1 | grep -E "test result|FAILED|panicked" | head -3` — all pass.

```bash
cargo fmt --manifest-path <repo>/Cargo.toml || exit 1
make -C <repo> lint >/tmp/lint.log 2>&1 || { head -40 /tmp/lint.log; exit 1; }
make -C <repo> test >/tmp/test.log 2>&1 || { grep -E "FAILED|panicked|^error" /tmp/test.log | head; exit 1; }
git -C <repo> add -A src/ || exit 1
git -C <repo> commit -F - <<'EOF' || exit 1
feat(bench): fifteen flag-sourced fields on LaunchFlags and Stamp — the seven reasoning flags read off the argv, a negative number is a value, one set_flags joins them

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01USTtBCMA5rsVnigD6fhoc3
EOF
```

---

### Task 2: Stored runs hydrate their flags at load

**Files:**
- Modify: `src/core/bench/store.rs` (`RunLog::load` ~431-444)
- Test: inline in `src/core/bench/store.rs`

**Interfaces:**
- Consumes: `Stamp::set_flags`, `launch_flags`, `unmanaged_flags`, `RUNTIME_LLAMA_CPP` (Task 1 / existing).
- Produces: `RunLog::load` returns a head whose fifteen flag fields are derived from `launch_args` (llama.cpp) or the `unmanaged` sentinel (foreign). Everything that reads a stored run (`compare`, `--resume`, `adopt_judge`, `speeds`) goes through it.

- [ ] **Step 1: Write the failing test**

In `store.rs`'s test module, beside `a_row_written_before_transport_loads_as_buffered`:

```rust
    #[test]
    fn a_stored_run_hydrates_its_flags_from_its_argv_and_a_foreign_one_reads_unmanaged() {
        use crate::core::bench::stamp::{FLAG_UNMANAGED, Stamp};
        let eval = scratch("hydrate-flags");
        let mut old = head();
        old.launch_args = ["-m", "model.gguf", "--reasoning-format", "none", "-b", "2048"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        let dir = eval.join("old-run");
        std::fs::create_dir_all(&dir).expect("dir");
        let mut json = serde_json::to_value(&old).expect("ser");
        let stamp = json["stamp"].as_object_mut().expect("stamp object");
        for field in [
            "reasoning", "reasoning_format", "reasoning_effort", "reasoning_budget",
            "reasoning_budget_message", "reasoning_preserve", "chat_template_kwargs",
        ] {
            stamp.remove(field);
        }
        stamp["n_batch"] = serde_json::Value::String("engine-default".into());
        std::fs::write(dir.join("stamp.json"), json.to_string()).expect("write");
        let loaded = RunLog::load(&dir).expect("loads");
        assert_eq!(loaded.head.stamp.reasoning_format, "none", "read off the stored argv");
        assert_eq!(loaded.head.stamp.n_batch, "2048", "every flag field is re-derived, not only the new ones");
        assert_eq!(loaded.head.stamp.reasoning_effort, "engine-default");
        let mut foreign = head();
        foreign.launch_args = Vec::new();
        foreign.stamp.runtime = "mlx-lm 0.31.3".into();
        let dir = eval.join("foreign-run");
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(dir.join("stamp.json"), serde_json::to_string(&foreign).expect("ser")).expect("write");
        let loaded = RunLog::load(&dir).expect("loads");
        assert_eq!(loaded.head.stamp.reasoning, FLAG_UNMANAGED);
        assert_eq!(loaded.head.stamp.spec_type, FLAG_UNMANAGED, "the two speculative fields hydrate the same way");
        let _: &Stamp = &loaded.head.stamp;
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --manifest-path <repo>/Cargo.toml a_stored_run_hydrates 2>&1 | grep -E "panicked|test result" | head -2`
Expected: the assertion on `reasoning_format` fails with `engine-default`.

- [ ] **Step 3: Commit the red**

```bash
cargo fmt --manifest-path <repo>/Cargo.toml
git -C <repo> add src/core/bench/store.rs
git -C <repo> commit -F - <<'EOF'
test(bench): red — a stored run's flags are read off its own argv at load; a foreign run's read unmanaged

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01USTtBCMA5rsVnigD6fhoc3
EOF
```

- [ ] **Step 4: Implement**

In `RunLog::load`, after the `head` is parsed and before the rows:

```rust
        let mut head: RunHead =
            serde_json::from_str(&head_text).map_err(|e| invalid(&stamp_path, e))?;
        hydrate_flags(&mut head);
```

and add near `RunLog`:

```rust
/// The flag-sourced stamp fields, re-derived from the argv the head stores.
///
/// A stamp written before a flag field existed carries the honest value one
/// key above it, in `launch_args`; reading it there instead of defaulting
/// keeps every stored run comparable with, and resumable under, runs made
/// after the field. Idempotent for a fresh stamp — the writer computed the
/// same fields from the same argv. A foreign server's flags are unobserved.
fn hydrate_flags(head: &mut RunHead) {
    use crate::core::bench::stamp::{RUNTIME_LLAMA_CPP, launch_flags, unmanaged_flags};
    let flags = if head.stamp.runtime == RUNTIME_LLAMA_CPP {
        launch_flags(&head.launch_args)
    } else {
        unmanaged_flags()
    };
    head.stamp.set_flags(&flags);
}
```

- [ ] **Step 5: Run, gate, commit**

Run the test, then the gate as in Task 1 step 7 (every existing store/compare/speeds test must still pass — a fixture whose `head()` has `launch_args: ["-m", "model.gguf"]` hydrates to engine-default everywhere, which is what its stamp said).

```bash
git -C <repo> add src/core/bench/store.rs
git -C <repo> commit -F - <<'EOF'
feat(bench): RunLog::load hydrates every flag-sourced field from the stored argv — old runs compare and resume, foreign runs read unmanaged

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01USTtBCMA5rsVnigD6fhoc3
EOF
```

---

### Task 3: `compare` — fifteen under `--cross-flags`, twenty-one under `--cross-runtime`

**Files:**
- Modify: `src/core/bench/compare.rs` (`CompareOpts` doc 166; `CROSS_RUNTIME_ALLOWED` 172-187; `CROSS_FLAGS_ALLOWED` 192-201; `mask_cross_flags` 272-281; `mask_cross_runtime` 287-302; docs at 270, 283; tests `cross_flags_masks_exactly_the_eight_launch_flags_and_names_them` ~1319 and `timing_source_is_allow_listed_only_under_cross_runtime` ~1490)
- Test: inline in `src/core/bench/compare.rs`

- [ ] **Step 1: Write the failing tests**

Rename `cross_flags_masks_exactly_the_eight_launch_flags_and_names_them` → `…the_fifteen…` and change its `assert_eq!(super::CROSS_FLAGS_ALLOWED.len(), 8)` to `15`; in `timing_source_is_allow_listed_only_under_cross_runtime` change `14` to `21`. Add, modelled on `a_differing_spec_type_is_refused_and_allow_listed_only_under_cross_runtime`:

```rust
    #[test]
    fn a_differing_reasoning_budget_is_refused_and_masked_by_both_masks() {
        let a = run("m1", stamp("dda1b0d67", "r1/s1"), &[38.0, 40.0, 41.0, 40.5]);
        let mut budgeted = stamp("dda1b0d67", "r1/s1");
        budgeted.reasoning_budget = "0".into();
        let b = run("m1", budgeted, &[39.0, 40.0, 41.0, 40.5]);
        let err = compare_runs(&a, &b, &opts(5.0)).expect_err("a differing budget refuses");
        assert!(
            matches!(&err, ChekovError::BenchStampMismatch { field, .. } if field == "reasoning_budget"),
            "{err}"
        );
        let flags = CompareOpts {
            cross_flags: true,
            ..opts(5.0)
        };
        compare_runs(&a, &b, &flags).expect("masked under --cross-flags");
        let banner = cross_flags_banner(&a.head, &b.head);
        assert!(
            banner.contains("reasoning_budget: \"engine-default\" vs \"0\""),
            "{banner}"
        );
        let runtime = CompareOpts {
            cross_runtime: true,
            ..opts(5.0)
        };
        compare_runs(&a, &b, &runtime).expect("masked under --cross-runtime");
    }
```

(Use the test module's existing names for `run`, `stamp`, `opts`, `CompareOpts`, `cross_flags_banner` — read the neighbouring test to match them exactly.)

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --manifest-path <repo>/Cargo.toml -- cross_flags cross_runtime reasoning_budget 2>&1 | grep -E "panicked|test result" | head -4`
Expected: the `.len()` assertions and the mask test fail.

- [ ] **Step 3: Commit the red**

```bash
cargo fmt --manifest-path <repo>/Cargo.toml
git -C <repo> add src/core/bench/compare.rs
git -C <repo> commit -F - <<'EOF'
test(bench): red — compare refuses a differing reasoning flag by name and masks it under --cross-flags and --cross-runtime

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01USTtBCMA5rsVnigD6fhoc3
EOF
```

- [ ] **Step 4: Implement**

`CROSS_RUNTIME_ALLOWED: [&str; 21]` — insert the seven names after `"spec_draft_n_max"` and before `"prompt_set_hash"`; `CROSS_FLAGS_ALLOWED: [&str; 15]` — append the seven. Both masks: replace the eight `clone_from` lines with `b_env.set_flags(&flags_of(a));` where:

```rust
/// A stamp's flag-sourced fields as the `LaunchFlags` view `set_flags` takes.
fn flags_of(s: &Stamp) -> LaunchFlags {
    LaunchFlags {
        kv_unified: s.kv_unified.clone(),
        n_batch: s.n_batch.clone(),
        n_ubatch: s.n_ubatch.clone(),
        type_k: s.type_k.clone(),
        type_v: s.type_v.clone(),
        flash_attn: s.flash_attn.clone(),
        spec_type: s.spec_type.clone(),
        spec_draft_n_max: s.spec_draft_n_max.clone(),
        reasoning: s.reasoning.clone(),
        reasoning_format: s.reasoning_format.clone(),
        reasoning_effort: s.reasoning_effort.clone(),
        reasoning_budget: s.reasoning_budget.clone(),
        reasoning_budget_message: s.reasoning_budget_message.clone(),
        reasoning_preserve: s.reasoning_preserve.clone(),
        chat_template_kwargs: s.chat_template_kwargs.clone(),
    }
}
```

(Put `flags_of` in `stamp.rs` as `impl Stamp { pub fn flags(&self) -> LaunchFlags }` if compare.rs would otherwise need to import `LaunchFlags` for nothing else — either is fine; one definition.) Update the docs: `CompareOpts.cross_flags` "Mask exactly the fifteen launch-flag fields", `mask_cross_flags` "Masks exactly the fifteen launch-flag fields", `mask_cross_runtime` "the 21-entry allow-list".

- [ ] **Step 5: Run, gate, commit**

```bash
git -C <repo> add src/core/bench/compare.rs src/core/bench/stamp.rs
git -C <repo> commit -F - <<'EOF'
feat(bench): --cross-flags masks fifteen and --cross-runtime twenty-one — the reasoning flags refuse by name and are named in the banners

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01USTtBCMA5rsVnigD6fhoc3
EOF
```

---

### Task 4: `thinkspan` — the tag table and the two counters

**Files:**
- Create: `src/core/bench/thinkspan.rs`
- Modify: `src/core/bench/mod.rs` (`pub mod thinkspan;` after `store`)
- Test: inline in `src/core/bench/thinkspan.rs`

**Interfaces:**
- Consumes: `crate::core::proxy::claude::{THINK_OPEN, THINK_CLOSE}` (`pub(crate)`, `"<think>"` / `"</think>"`).
- Produces: `#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)] pub struct ReplyChars { pub thinking: u64, pub answer: u64 }`; `pub fn split_thinking(content: &str) -> ReplyChars`; `pub fn reply_chars(message: &Value) -> ReplyChars`; `pub fn stream_reply_chars<'a>(frames: impl Iterator<Item = &'a str>) -> ReplyChars`; `pub const THINKING_TAGS: [ThinkTags; 5]`. Task 5 consumes them.

- [ ] **Step 1: Write the failing tests**

Create the file with its module doc and a test module:

```rust
//! Thinking versus answer, counted in characters.
//!
//! Read where the thinking still exists — the `OpenAI` body before
//! translation — because a `--reasoning-format none` run leaves its thoughts
//! inline in `content` as the template family's own tags, and the proxy
//! strips them on the way to the agent (reasoning-stamp design §4, §11).

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{ReplyChars, reply_chars, split_thinking, stream_reply_chars};

    fn chars(thinking: u64, answer: u64) -> ReplyChars {
        ReplyChars { thinking, answer }
    }

    #[test]
    fn a_closed_think_span_is_thinking_and_the_rest_is_answer() {
        assert_eq!(split_thinking("<think>abc</think>de"), chars(3, 2));
        assert_eq!(split_thinking("no tags at all"), chars(0, 14));
        assert_eq!(split_thinking(""), chars(0, 0));
    }

    #[test]
    fn an_unclosed_span_is_thinking_to_the_end_and_every_span_counts() {
        assert_eq!(split_thinking("<think>abc"), chars(3, 0), "cut by max_tokens");
        assert_eq!(split_thinking("<think>abc<tool_call>{}"), chars(3, 0), "a tool call ends the span under none; the call itself is not content");
        assert_eq!(split_thinking("x<think>ab</think>y<think>c</think>z"), chars(3, 3));
    }

    #[test]
    fn every_family_in_the_table_is_scanned_and_tags_are_never_counted() {
        assert_eq!(split_thinking("[THINK]ab[/THINK]c"), chars(2, 1), "Mistral");
        assert_eq!(split_thinking("<|channel|>analysis<|message|>ab<|end|>c"), chars(2, 1), "gpt-oss");
        assert_eq!(split_thinking("<|channel>thoughtab<channel|>c"), chars(2, 1), "Gemma");
        assert_eq!(split_thinking("<mm:think>ab</mm:think>c"), chars(2, 1), "MiniMax");
    }

    #[test]
    fn characters_are_scalars_not_bytes() {
        assert_eq!(split_thinking("<think>ééé</think>日本"), chars(3, 2));
    }

    #[test]
    fn a_buffered_message_counts_reasoning_content_spans_and_tool_arguments() {
        let message = json!({
            "reasoning_content": "abcd",
            "content": "<think>xy</think>ok",
            "tool_calls": [{"function": {"name": "read_file", "arguments": "{\"path\":\"a\"}"}}]
        });
        assert_eq!(reply_chars(&message), chars(6, 2 + 12));
        assert_eq!(reply_chars(&json!({})), chars(0, 0));
        assert_eq!(reply_chars(&json!({"reasoning": "ab"})), chars(2, 0), "the foreign spelling is an alias");
    }

    #[test]
    fn a_stream_is_folded_before_it_is_scanned() {
        let frames = [
            r#"{"choices":[{"delta":{"content":"<thi"}}]}"#,
            r#"{"choices":[{"delta":{"content":"nk>ab</thi"}}]}"#,
            r#"{"choices":[{"delta":{"reasoning_content":"zz","content":"nk>c"}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c","function":{"name":"f","arguments":""}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"a\":"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"1}"}}]}}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"stop"}],"usage":null}"#,
            "not json",
            "[DONE]",
        ];
        assert_eq!(stream_reply_chars(frames.into_iter()), chars(2 + 2, 1 + 7));
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Add `pub mod thinkspan;` to `src/core/bench/mod.rs`, then run: `cargo test --manifest-path <repo>/Cargo.toml thinkspan 2>&1 | grep -E "^error" | sort | uniq -c | head -3`
Expected: unresolved imports.

- [ ] **Step 3: Commit the red**

```bash
cargo fmt --manifest-path <repo>/Cargo.toml
git -C <repo> add src/core/bench/thinkspan.rs src/core/bench/mod.rs
git -C <repo> commit -F - <<'EOF'
test(bench): red — thinking spans counted by family tag, unclosed to the end, a stream folded before it is scanned

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01USTtBCMA5rsVnigD6fhoc3
EOF
```

- [ ] **Step 4: Implement**

Above the test module:

```rust
use serde_json::Value;

use crate::core::proxy::claude::{THINK_CLOSE, THINK_OPEN};

/// One template family's thinking span: how it opens and every way it ends.
pub struct ThinkTags {
    pub open: &'static str,
    pub closes: &'static [&'static str],
}

/// The families llama.cpp's chat parser knows (`common/chat.cpp`,
/// `thinking_start_tag` / `thinking_end_tags` at the pinned commit). Under
/// `--reasoning-format none` these are what a reply carries inline. A family
/// missing here reads as answer, so the CHANGELOG names the table.
pub const THINKING_TAGS: [ThinkTags; 5] = [
    ThinkTags {
        open: THINK_OPEN,
        closes: &[THINK_CLOSE, "<tool_call>"],
    },
    ThinkTags {
        open: "[THINK]",
        closes: &["[/THINK]"],
    },
    ThinkTags {
        open: "<|channel|>analysis<|message|>",
        closes: &["<|end|>"],
    },
    ThinkTags {
        open: "<|channel>thought",
        closes: &["<channel|>"],
    },
    ThinkTags {
        open: "<mm:think>",
        closes: &["</mm:think>"],
    },
];

/// Characters of a reply spent thinking and spent answering.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReplyChars {
    pub thinking: u64,
    pub answer: u64,
}

impl ReplyChars {
    fn add(self, other: Self) -> Self {
        Self {
            thinking: self.thinking + other.thinking,
            answer: self.answer + other.answer,
        }
    }
}

fn count(text: &str) -> u64 {
    u64::try_from(text.chars().count()).unwrap_or(u64::MAX)
}

/// The earliest opening tag in `text`, with its family.
fn next_open(text: &str) -> Option<(usize, &'static ThinkTags)> {
    THINKING_TAGS
        .iter()
        .filter_map(|tags| text.find(tags.open).map(|at| (at, tags)))
        .min_by_key(|(at, _)| *at)
}

/// The earliest closing tag of `tags` in `text`: its start and its length.
fn next_close(text: &str, tags: &ThinkTags) -> Option<(usize, usize)> {
    tags.closes
        .iter()
        .filter_map(|close| text.find(close).map(|at| (at, close.len())))
        .min_by_key(|(at, _)| *at)
}

/// `content` split into thinking and answer characters: every span of every
/// family counts, tags themselves never do, and a span that never closes is
/// thinking to the end — the reply the model was cut off in, or the one
/// that went straight to a tool call (design §11).
#[must_use]
pub fn split_thinking(content: &str) -> ReplyChars {
    let mut rest = content;
    let mut out = ReplyChars::default();
    while let Some((at, tags)) = next_open(rest) {
        out.answer += count(&rest[..at]);
        let inner = &rest[at + tags.open.len()..];
        match next_close(inner, tags) {
            Some((end, close_len)) => {
                out.thinking += count(&inner[..end]);
                rest = &inner[end + close_len..];
            }
            None => {
                out.thinking += count(inner);
                return out;
            }
        }
    }
    out.answer += count(rest);
    out
}

fn text_of<'v>(value: &'v Value, key: &str) -> &'v str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// The reasoning field under either spelling: llama.cpp's `reasoning_content`,
/// or the `reasoning` other OpenAI-compatible servers use.
fn reasoning_of(value: &Value) -> &str {
    let content = text_of(value, "reasoning_content");
    if content.is_empty() {
        text_of(value, "reasoning")
    } else {
        content
    }
}

/// Every `tool_calls[].function.arguments` fragment's characters — partial
/// JSON on a stream, whole on a buffered body; never parsed, only counted.
fn arguments_chars(value: &Value) -> u64 {
    value
        .get("tool_calls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|call| call.get("function"))
        .map(|f| count(text_of(f, "arguments")))
        .sum()
}

/// A buffered `choices[0].message`: reasoning, the spans in `content`, and
/// the tool-call arguments as answer.
#[must_use]
pub fn reply_chars(message: &Value) -> ReplyChars {
    let spans = split_thinking(text_of(message, "content"));
    ReplyChars {
        thinking: count(reasoning_of(message)) + spans.thinking,
        answer: spans.answer + arguments_chars(message),
    }
}

/// An SSE body's frames: `content` fragments are concatenated BEFORE the span
/// scan so a tag split across chunks still counts; reasoning and tool
/// arguments are summed per fragment. Frames that are not JSON, carry no
/// delta, or are the `[DONE]` sentinel add nothing.
pub fn stream_reply_chars<'a>(frames: impl Iterator<Item = &'a str>) -> ReplyChars {
    let mut content = String::new();
    let mut rest = ReplyChars::default();
    for delta in frames.filter_map(delta_of) {
        content.push_str(text_of(&delta, "content"));
        rest = rest.add(ReplyChars {
            thinking: count(reasoning_of(&delta)),
            answer: arguments_chars(&delta),
        });
    }
    split_thinking(&content).add(rest)
}

fn delta_of(frame: &str) -> Option<Value> {
    serde_json::from_str::<Value>(frame)
        .ok()?
        .get("choices")?
        .get(0)?
        .get("delta")
        .cloned()
}
```

- [ ] **Step 5: Run, gate, commit**

Run: `cargo test --manifest-path <repo>/Cargo.toml thinkspan 2>&1 | grep -E "test result|FAILED|panicked" | head -3`, then the gate.

```bash
git -C <repo> add src/core/bench/thinkspan.rs src/core/bench/mod.rs
git -C <repo> commit -F - <<'EOF'
feat(bench): thinkspan — five families' thinking tags, spans counted to the end when unclosed, a buffered message and a folded stream counted the same way

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01USTtBCMA5rsVnigD6fhoc3
EOF
```

---

### Task 5: `Timings` carries the counts on every door

**Files:**
- Modify: `src/core/bench/runner.rs` (`Timings` ~117-133; `cross_streaming` ~309-341; `cross_stream_timed` ~343-369; `stream_timings` ~400-407; `timings_from` ~636-657; the production `Timings` literal in `timings_from_stream` ~465)
- Modify: every `Timings { … }` literal the compiler names (`src/core/bench/sweep.rs` test `artifact()` ~108; `src/commands/capability.rs` test `timings_fixture` ~3338; `src/core/bench/toolloop.rs` test `timings()` ~650; runner.rs tests)
- Test: inline in `src/core/bench/runner.rs`

**Interfaces:**
- Consumes: `thinkspan::{ReplyChars, reply_chars, stream_reply_chars}` (Task 4).
- Produces: `Timings { …, thinking_chars: u64, answer_chars: u64 }` set on the buffered door (`timings_from`), the llama.cpp streamed door (`stream_timings`) and the foreign streamed door (`cross_stream_timed`). Task 6 consumes them.

- [ ] **Step 1: Write the failing tests**

In `runner.rs`'s test module, beside `timings_carry_the_draft_counts_when_the_server_drafted`:

```rust
    #[test]
    fn buffered_timings_count_the_thinking_span_the_translator_will_strip() {
        let body = json!({
            "choices": [{ "message": { "content": "<think>\nweighing\n</think>The answer." }, "finish_reason": "stop" }],
            "timings": { "prompt_n": 9, "prompt_per_second": 90.0, "predicted_n": 8, "predicted_per_second": 8.0 }
        });
        let timings = super::timings_from(&body).expect("timings");
        assert_eq!(timings.thinking_chars, 10, "the span's characters, tags excluded");
        assert_eq!(timings.answer_chars, 11);
        let extracted = json!({
            "choices": [{ "message": { "reasoning_content": "weighing", "content": "The answer." }, "finish_reason": "stop" }],
            "timings": { "prompt_n": 9, "prompt_per_second": 90.0, "predicted_n": 8, "predicted_per_second": 8.0 }
        });
        let timings = super::timings_from(&extracted).expect("timings");
        assert_eq!((timings.thinking_chars, timings.answer_chars), (8, 11));
    }

    #[test]
    fn the_streamed_door_counts_across_frames_on_both_clocks() {
        let frames = [
            text_frame("<thi"),
            text_frame("nk>ab</think>"),
            text_frame("cd"),
            json!({ "id": "c1", "choices": [{ "delta": {}, "finish_reason": "stop" }],
                    "usage": { "prompt_tokens": 900, "completion_tokens": 100 },
                    "timings": final_frame()["timings"] }),
        ];
        let http = CannedUpstream::new(sse(&frames));
        let facade = ClaudeFacade::new("local-model");
        let up = fake_upstream();
        let artifact = super::cross_streaming(&wire(&http, &facade, &up), &anthropic_request("hi")).expect("crossed");
        assert_eq!((artifact.timings.thinking_chars, artifact.timings.answer_chars), (2, 2));
        let http = CannedUpstream::new_streamed(sse(&frames[..3]) + "data: {\"id\":\"c1\",\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":3}}\n\ndata: [DONE]\n\n", some_marks());
        let artifact = super::cross_stream_timed(&wire(&http, &facade, &up), &anthropic_request("hi")).expect("crossed");
        assert_eq!((artifact.timings.thinking_chars, artifact.timings.answer_chars), (2, 2), "the foreign clock counts the same frames");
    }
```

(`sse()` appends its own `[DONE]`; for the foreign case build the body from `frames[..3]` plus the usage frame as shown, or add a second helper `sse_with(frames, extra)` — whichever keeps the test under 40 lines. If `sse` cannot be composed, write the foreign half as its own test.)

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --manifest-path <repo>/Cargo.toml -- thinking_span streamed_door_counts 2>&1 | grep -E "^error" | sort | uniq -c | head -3`
Expected: `no field thinking_chars`.

- [ ] **Step 3: Commit the red**

```bash
cargo fmt --manifest-path <repo>/Cargo.toml
git -C <repo> add src/core/bench/runner.rs
git -C <repo> commit -F - <<'EOF'
test(bench): red — Timings carry thinking and answer characters on the buffered door and on both streamed clocks

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01USTtBCMA5rsVnigD6fhoc3
EOF
```

- [ ] **Step 4: Implement**

`Timings` gains, after `draft_n_accepted`:

```rust
    /// Characters of the reply spent thinking and answering, read off the
    /// upstream body where a `<think>` span still exists (design §4). Both
    /// zero on a crossing that carried no message — never "no thinking".
    pub thinking_chars: u64,
    pub answer_chars: u64,
```

`timings_from`: before the `match`, `let chars = message_chars(parsed);` and in the literal `thinking_chars: chars.thinking, answer_chars: chars.answer,` with:

```rust
/// The buffered reply's counts — zero-both when the body has no message
/// (an `/infill` body has none, and records no measurement).
fn message_chars(parsed: &Value) -> crate::core::bench::thinkspan::ReplyChars {
    parsed
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .map(crate::core::bench::thinkspan::reply_chars)
        .unwrap_or_default()
}
```

`stream_timings`: after `let timings = read_timings(&last.to_string())?;`:

```rust
    let chars = crate::core::bench::thinkspan::stream_reply_chars(data_lines(sse));
    Ok(Timings {
        thinking_chars: chars.thinking,
        answer_chars: chars.answer,
        ..timings
    })
```

`timings_from_stream`'s literal: `thinking_chars: 0, answer_chars: 0,` (the caller attaches them). In `cross_stream_timed`, replace the `timings:` line:

```rust
    let chars = crate::core::bench::thinkspan::stream_reply_chars(data_lines(&sse));
    Ok(ProbeArtifact {
        anthropic_body,
        timings: Timings {
            thinking_chars: chars.thinking,
            answer_chars: chars.answer,
            ..timings_from_stream(&usage, &marks)?
        },
    })
```

(If `cross_stream_timed` passes 40 lines, move the `usage` lookup into a `stream_usage_or_refuse(sse)` helper.) Then every `Timings` literal the compiler names gets `thinking_chars: 0, answer_chars: 0,` — except `toolloop.rs`'s test `timings(prompt_n)` which gets `thinking_chars: 5, answer_chars: 15,` so Task 6's fold test can observe sums.

- [ ] **Step 5: Run, gate, commit**

```bash
git -C <repo> add -A src/
git -C <repo> commit -F - <<'EOF'
feat(bench): Timings carry thinking and answer characters — read off choices[0].message on the buffered door, folded over the frames on both streamed clocks

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01USTtBCMA5rsVnigD6fhoc3
EOF
```

---

### Task 6: `Measure` carries them — probes, loops, the throughput sweep

**Files:**
- Modify: `src/core/bench/store.rs` (`Measure` ~85-103), `src/core/bench/codebase/run.rs` (`empty_measure` ~42, `probe_measure` ~54), `src/core/bench/toolloop.rs` (`fold` ~318), `src/core/bench/sweep.rs` (`DepthResult` ~29, `measure_depth` ~58-88), `src/commands/capability.rs` (`run_throughput`'s `Measure` literal ~2246)
- Modify: every `Measure { … }` / `DepthResult { … }` literal the compiler names (store.rs test `measure()`, speeds.rs `depth_row`, compare.rs three sites, core/tune.rs test ~1044, sweep.rs tests)
- Test: inline in `store.rs`, `toolloop.rs`, `sweep.rs`

- [ ] **Step 1: Write the failing tests**

`store.rs`, beside `a_row_written_before_cache_n_loads_as_zero_cached`:

```rust
    #[test]
    fn a_row_written_before_the_thinking_counts_loads_as_unmeasured() {
        let line = r#"{"schema":1,"run_id":"r","seq":0,"suite":"tool_emit","task_id":"te-001",
            "measure":{"prompt_n":4,"decode_samples":[1.0],"prefill_samples":[1.0],"warmup_dropped":0}}"#;
        let row: TaskRow = serde_json::from_str(line).expect("an old row still loads");
        assert_eq!((row.measure.thinking_chars, row.measure.answer_chars), (0, 0));
    }
```

`toolloop.rs`, beside `read_then_edit_then_stop_reaches_the_goal_with_one_sample_per_turn`:

```rust
    #[test]
    fn the_loop_sums_thinking_and_answer_characters_over_its_turns() {
        let set = unchanged_set();
        let script = Scripted::new(vec![
            reply(vec![use_block("t1", "read_file", json!({"path": "src/legacy.rs"}))], "tool_use"),
            reply(vec![text_block("src/legacy.rs does not exist.")], "end_turn"),
        ]);
        let outcome = drive(&mut script.door(), &run(&set, 8)).expect("drove");
        assert_eq!((outcome.measure.thinking_chars, outcome.measure.answer_chars), (10, 30), "5 and 15 per timed turn, two turns");
    }
```

`sweep.rs`, beside its existing `measure_depth` test (read it for the helper names — `artifact(decode_tps)` builds a `ProbeArtifact`):

```rust
    #[test]
    fn a_depth_sums_the_thinking_and_answer_characters_over_its_repetitions() {
        let plan = SweepPlan { depths: vec![1024], repetitions: 3, max_tokens: 64 };
        let mut exec = |_: &HttpRequest| Ok(artifact(20.0));
        let result = measure_depth(&plan, 1024, &mut exec).expect("measured");
        assert_eq!((result.thinking_chars, result.answer_chars), (3 * 7, 3 * 11));
    }
```

(and set `thinking_chars: 7, answer_chars: 11` in `artifact()`'s `Timings` literal when Task 5's compiler pass reaches it — do it now if it was zeroed.)

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --manifest-path <repo>/Cargo.toml -- thinking_counts thinking_and_answer 2>&1 | grep -E "^error" | sort | uniq -c | head -3`
Expected: `no field thinking_chars on Measure` / `DepthResult`.

- [ ] **Step 3: Commit the red**

```bash
cargo fmt --manifest-path <repo>/Cargo.toml
git -C <repo> add src/core/bench/store.rs src/core/bench/toolloop.rs src/core/bench/sweep.rs
git -C <repo> commit -F - <<'EOF'
test(bench): red — Measure carries the counts, an old row loads unmeasured, a loop and a depth sum them

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01USTtBCMA5rsVnigD6fhoc3
EOF
```

- [ ] **Step 4: Implement**

`Measure`, after `draft_n_accepted`:

```rust
    /// Characters of the reply spent thinking and answering, summed over the
    /// crossings the row holds (design §4). Zero-both on rows written before
    /// the fields and on untimed crossings — "unmeasured", never "no thinking".
    #[serde(default)]
    pub thinking_chars: u64,
    #[serde(default)]
    pub answer_chars: u64,
```

`empty_measure`: `thinking_chars: 0, answer_chars: 0,`. `probe_measure`: `thinking_chars: timings.thinking_chars, answer_chars: timings.answer_chars,`. `LoopState::fold`: `self.measure.thinking_chars += t.thinking_chars; self.measure.answer_chars += t.answer_chars;`. `DepthResult` gains `pub thinking_chars: u64, pub answer_chars: u64,` (doc: "summed over the repetitions, warmup included, like the drafts"); `measure_depth` sums them in its loop (`let (mut thinking_chars, mut answer_chars) = (0_u64, 0_u64);` — if the function passes 40 lines, fold the five accumulators into a small `DepthSums` struct with an `add(&mut self, &Timings)` method). `run_throughput`'s `Measure` literal copies `result.thinking_chars` / `result.answer_chars`. Then every literal the compiler names gets zeros.

- [ ] **Step 5: Run, gate, commit**

```bash
git -C <repo> add -A src/
git -C <repo> commit -F - <<'EOF'
feat(bench): Measure carries thinking and answer characters — copied per probe, summed per loop and per depth, zero-both means unmeasured

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01USTtBCMA5rsVnigD6fhoc3
EOF
```

---

### Task 7: The `thinking` line

**Files:**
- Modify: `src/core/bench/store.rs` (`suite_summaries` ~587-606: insert before `asymmetry_lines`; new helpers beside `speculative_line`)
- Test: inline in `src/core/bench/store.rs`

**Interfaces:**
- Consumes: `Measure.thinking_chars/answer_chars`, `log.head.stamp`, `log.head.forced_reasoning_format`, `FLAG_ENGINE_DEFAULT`, `RUNTIME_LLAMA_CPP`.
- Produces: `fn thinking_line(log: &RunLog) -> Option<String>`.

- [ ] **Step 1: Write the failing tests**

```rust
    /// A row of `suite` with the given counts, buffered, passing.
    fn thought(suite: &str, id: &str, thinking: u64, answer: u64) -> Task {
        let mut task = graded(suite, id, GradeRow::pass());
        task.measure.thinking_chars = thinking;
        task.measure.answer_chars = answer;
        task
    }

    #[test]
    fn the_thinking_line_prints_per_suite_medians_and_skips_unmeasured_suites() {
        let eval = scratch("thinking-line");
        let mut writer = RunWriter::create(&eval, "r-think", &head()).expect("create");
        for task in [
            thought("tool_emit", "te-001", 10, 90),
            thought("tool_emit", "te-002", 50, 50),
            thought("tool_emit", "te-003", 100, 0),
            thought("instruction", "if-001", 0, 0),
            thought("tool_loop", "tl-001", 30, 70),
        ] {
            writer.append(task).expect("append");
        }
        let rendered = render_run(&RunLog::load(writer.dir()).expect("load"));
        assert!(
            rendered.contains("thinking     share of reply characters spent thinking, median per case: tool_emit 50%, tool_loop 30% (launched with no reasoning flag)\n"),
            "{rendered}"
        );
        assert!(!rendered.contains("instruction 0%"), "a suite with no measured row is omitted: {rendered}");
    }

    #[test]
    fn the_thinking_line_is_absent_without_a_measurement_and_names_the_forced_arm_and_the_foreign_runtime() {
        let eval = scratch("thinking-none");
        let writer = graded_run(&eval);
        let rendered = render_run(&RunLog::load(writer.dir()).expect("load"));
        assert!(!rendered.contains("thinking     "), "zero-both everywhere prints nothing: {rendered}");
        let eval = scratch("thinking-forced");
        let mut forced = head();
        forced.forced_reasoning_format = Some("deepseek".into());
        forced.launch_args = vec!["--reasoning-format".into(), "none".into()];
        let mut writer = RunWriter::create(&eval, "r-forced", &forced).expect("create");
        writer.append(thought("grammar_gap", "gg-te-001", 20, 80)).expect("append");
        let rendered = render_run(&RunLog::load(writer.dir()).expect("load"));
        assert!(
            rendered.contains("thinking     share of reply characters spent thinking, median per case: grammar_gap 20%; grammar_gap measured with reasoning extracted (deepseek)\n"),
            "a run launched with a reasoning flag has no default footnote: {rendered}"
        );
        let eval = scratch("thinking-foreign");
        let mut foreign = head();
        foreign.stamp.runtime = "mlx-lm 0.31.3".into();
        let mut writer = RunWriter::create(&eval, "r-foreign", &foreign).expect("create");
        writer.append(thought("tool_emit", "te-001", 1, 3)).expect("append");
        let rendered = render_run(&RunLog::load(writer.dir()).expect("load"));
        assert!(rendered.contains("tool_emit 25% (reasoning flags unmanaged on this runtime)\n"), "{rendered}");
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test --manifest-path <repo>/Cargo.toml the_thinking_line 2>&1 | grep -E "panicked|test result" | head -3`
Expected: both fail on the missing line.

- [ ] **Step 3: Commit the red**

```bash
cargo fmt --manifest-path <repo>/Cargo.toml
git -C <repo> add src/core/bench/store.rs
git -C <repo> commit -F - <<'EOF'
test(bench): red — the thinking line prints per-suite medians, omits unmeasured suites, footnotes the flagless launch, the forced arm and the foreign runtime

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01USTtBCMA5rsVnigD6fhoc3
EOF
```

- [ ] **Step 4: Implement**

In `suite_summaries`, before `out.push_str(&asymmetry_lines(log));`: `out.extend(thinking_line(log));`. Beside `speculative_line`:

```rust
/// The suites in the order the line names them.
const THINKING_SUITES: [&str; 6] = [
    "throughput", "tool_emit", "grammar_gap", "instruction", "tool_loop", "codebase",
];

/// `thinking     share of reply characters spent thinking, median per case:
/// tool_emit 12%, …` — printed only when some row measured any characters,
/// footnoted with what the launch said about thinking (design §5, §11).
fn thinking_line(log: &RunLog) -> Option<String> {
    let cells: Vec<String> = THINKING_SUITES
        .iter()
        .filter_map(|suite| suite_share(log, suite).map(|pct| format!("{suite} {pct}%")))
        .collect();
    if cells.is_empty() {
        return None;
    }
    Some(format!(
        "thinking     share of reply characters spent thinking, median per case: {}{}{}\n",
        cells.join(", "),
        forced_arm_note(log),
        reasoning_launch_note(&log.head.stamp)
    ))
}

/// The median whole-percent share over the suite's measured rows, `None`
/// when no row of the suite measured any characters. Rounded per row the
/// way `percent` rounds, then the upper-middle median, both in integers.
fn suite_share(log: &RunLog, suite: &str) -> Option<u128> {
    let mut shares: Vec<u128> = rows_of(log, suite)
        .map(|row| (row.measure.thinking_chars, row.measure.answer_chars))
        .filter(|(thinking, answer)| thinking + answer > 0)
        .map(|(thinking, answer)| {
            (u128::from(thinking) * 200 / u128::from(thinking + answer)).div_ceil(2).min(100)
        })
        .collect();
    if shares.is_empty() {
        return None;
    }
    shares.sort_unstable();
    Some(shares[shares.len() / 2])
}

/// The grammar arm ran under its own extraction, whatever the launch said.
fn forced_arm_note(log: &RunLog) -> String {
    log.head.forced_reasoning_format.as_deref().map_or_else(String::new, |mode| {
        format!("; grammar_gap measured with reasoning extracted ({mode})")
    })
}

/// What the launch said about thinking: nothing at all, or unobservable.
fn reasoning_launch_note(stamp: &crate::core::bench::stamp::Stamp) -> &'static str {
    use crate::core::bench::stamp::{FLAG_ENGINE_DEFAULT, RUNTIME_LLAMA_CPP};
    if stamp.runtime != RUNTIME_LLAMA_CPP {
        return " (reasoning flags unmanaged on this runtime)";
    }
    let flagless = [
        &stamp.reasoning,
        &stamp.reasoning_format,
        &stamp.reasoning_effort,
        &stamp.reasoning_budget,
        &stamp.reasoning_budget_message,
        &stamp.reasoning_preserve,
        &stamp.chat_template_kwargs,
    ]
    .iter()
    .all(|f| *f == FLAG_ENGINE_DEFAULT);
    if flagless {
        " (launched with no reasoning flag)"
    } else {
        ""
    }
}
```

(Note: the forced-arm test's head sets `launch_args` with `--reasoning-format none`; `RunLog::load` hydrates the stamp from it, so the flagless footnote is absent there — that is the behaviour under test.)

- [ ] **Step 5: Run, gate, commit**

```bash
git -C <repo> add src/core/bench/store.rs
git -C <repo> commit -F - <<'EOF'
feat(bench): the thinking line — per-suite median share of reply characters spent thinking, footnoted with what the launch said

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01USTtBCMA5rsVnigD6fhoc3
EOF
```

---

### Task 8: The tune record carries the flags

**Files:**
- Modify: `src/core/tune.rs` (test module beside `a_record_round_trips_and_names_its_launch_flags` ~1212)

- [ ] **Step 1: Write the test, run it (it passes already — `trial_row` reads `launch_flags(&argv)`), commit it as the pin**

```rust
    #[test]
    fn a_trials_stamp_reads_the_reasoning_flags_off_its_argv() {
        let argv: Vec<String> = ["--reasoning-effort", "low", "--reasoning-budget", "-1"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        let flags = crate::core::bench::stamp::launch_flags(&argv);
        let record = sample_record(argv, flags);
        let stamp = &record.trials[0].stamp;
        assert_eq!((stamp.reasoning_effort.as_str(), stamp.reasoning_budget.as_str()), ("low", "-1"));
        assert_eq!(stamp.reasoning_format, "engine-default");
    }
```

(Read `sample_record`'s signature first — `sample_record(argv: Vec<String>, stamp: LaunchFlags) -> Record` — and match it.) Run: `cargo test --manifest-path <repo>/Cargo.toml a_trials_stamp 2>&1 | grep -E "test result" | head -1` → passes. Gate, then:

```bash
git -C <repo> add src/core/tune.rs
git -C <repo> commit -F - <<'EOF'
test(tune): a trial's record stamp carries the reasoning flags read off its argv

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01USTtBCMA5rsVnigD6fhoc3
EOF
```

---

### Task 9: Docs, the live check, push, PR

**Files:**
- Modify: `README.md` (compare row ~110: "eight" → "fifteen" and the field list; foreign-runtimes paragraph ~230: "the eight unmanaged fields" → "the fifteen unmanaged fields"; tune runbook ~508: "exactly the eight launch-flag fields" → "exactly the fifteen launch-flag fields"; bench row ~108: after the draft-acceptance sentence add: `Every row also records how many characters of the upstream reply were thinking and how many were answer — the \`<think>\` span a \`--reasoning-format none\` launch leaves inline included — and the report prints one \`thinking\` line of per-suite median shares (characters, not tokens; a tool reply's JSON tokenizes differently from prose, so the tool suites' share is biased and the line says characters).`)
- Modify: `CHANGELOG.md` (first bullet under `[Unreleased]` → `### Added`)
- Modify: `IDEAS.md` (the reasoning entry's status line → `SHIPPED 2026-09-09`)

- [ ] **Step 1: The CHANGELOG entry**

```markdown
- The bench stamp reads the seven reasoning-side launch flags off the argv
  (`--reasoning`, `--reasoning-format`, `--reasoning-effort`,
  `--reasoning-budget`, `--reasoning-budget-message`,
  `--reasoning-preserve`, `--chat-template-kwargs`), so two runs that differ
  in how much the model was allowed to think refuse to compare by name;
  `--cross-flags` masks fifteen launch-flag fields and `--cross-runtime`
  twenty-one, and both banners name the ones that differ. Every stored run
  under `eval/` keeps comparing and resuming: the loader re-derives every
  flag-sourced field from the `launch_args` the head already stores (and
  reads them `unmanaged` on a foreign runtime), so a run launched with
  `--reasoning-format none` before this change reads `none` now, not
  "engine-default". `--reasoning-budget -1` — llama-server's own spelling of
  unrestricted — reads as `-1`, not as a switch.
- Every probe row records how many characters of the upstream reply were
  thinking and how many were answer (`thinking_chars`, `answer_chars` on
  the measure; loops and depths sum them), counted where the thinking still
  exists — the `OpenAI` body before translation — on the buffered door and
  on both streamed clocks. `reasoning_content` (or the `reasoning` spelling
  other servers use) counts as thinking, so does every inline span of the
  five families llama.cpp's parser knows (`<think>`, `[THINK]`,
  `<|channel|>analysis<|message|>`, `<|channel>thought`, `<mm:think>`), a
  span that never closes counts to the end (the reply cut by `max_tokens`,
  or the one that went straight to a tool call), and tool-call arguments
  count as answer. The report prints one line —
  `thinking     share of reply characters spent thinking, median per case:
  tool_emit 12%, …` — omitting suites with no measured row, footnoted
  `(launched with no reasoning flag)`, `(reasoning flags unmanaged on this
  runtime)` or `; grammar_gap measured with reasoning extracted (deepseek)`
  as the run warrants. Characters, not tokens: llama-server reports no
  reasoning token count anywhere. Live check on this desk: <fill from the
  run below>.
```

- [ ] **Step 2: The live check**

The daily driver is up (`pgrep -fl llama-server`). Run `target/debug/chekov capability bench --models ornith-1.5-35b-a3b --suite agentic --dry-run`, then the real run with `--yes` (~10 min; loops close early), and read the report's `thinking` line: the driver launches `--reasoning-format none`, so the unconstrained suites' share is the span path proving itself and `grammar_gap`'s is the extracted path. Then `target/debug/chekov capability compare 20260909T181700Z-ornith-1.5-35b-a3b <new run>` must NOT refuse on a reasoning field (both hydrate to `none` from their argv; it refuses on `prompt_set_hash` only if the agentic set changed, which it did not since PR #73 — expect a clean agentic comparison). Put the line's text and the compare verdict into the CHANGELOG placeholder.

- [ ] **Step 3: Gate, commit, push, PR**

```bash
cargo fmt --manifest-path <repo>/Cargo.toml || exit 1
make -C <repo> lint >/tmp/lint.log 2>&1 || { head -40 /tmp/lint.log; exit 1; }
make -C <repo> test >/tmp/test.log 2>&1 || { grep -E "FAILED|panicked|^error" /tmp/test.log | head; exit 1; }
git -C <repo> add README.md CHANGELOG.md IDEAS.md docs/superpowers/plans/2026-09-09-reasoning-stamp.md || exit 1
git -C <repo> commit -F - <<'EOF' || exit 1
docs: the reasoning flags on the stamp and the thinking line in the README and CHANGELOG; the idea shipped; the live check on the daily driver

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01USTtBCMA5rsVnigD6fhoc3
EOF
git -C <repo> push -u origin feat/bench-reasoning-stamp || exit 1
gh pr create --base develop --head feat/bench-reasoning-stamp --title "feat(bench): the reasoning flags on the stamp, the thinking share on every row" --body "$(cat <<'EOF'
Implements `docs/superpowers/specs/2026-09-09-reasoning-stamp-design.md` (approved 2026-09-09, amended §11 after a seven-agent seam map and critique) via `docs/superpowers/plans/2026-09-09-reasoning-stamp.md`.

- Seven reasoning-side launch flags join `LaunchFlags`/`Stamp` (fifteen flag-sourced fields); `compare` refuses across them by name; `--cross-flags` masks fifteen, `--cross-runtime` twenty-one.
- Stored runs hydrate every flag-sourced field from their own `launch_args` at load — nothing under `eval/` stops comparing or resuming.
- Every row's measure carries `thinking_chars`/`answer_chars`, counted on the upstream body (five families' inline tags, unclosed spans, tool arguments as answer) on every door; loops and depths sum them.
- The report prints one `thinking` line of per-suite median shares with the footnotes the run warrants.

Live check: <from Task 9>.

🤖 Generated with [Claude Code](https://claude.com/claude-code)

https://claude.ai/code/session_01USTtBCMA5rsVnigD6fhoc3
EOF
)"
```
