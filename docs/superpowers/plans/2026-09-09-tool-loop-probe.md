# `tool_loop` probe Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `capability bench --suite agentic` gains `tool_loop` — six canned read→edit→verify cases driven to a terminal state through an in-process tool environment, on both doors, graded on the end state.

**Architecture:** The probe set (`agentic_v0.toml`) grows a third array of cases with canned files, a palette and a goal. A new `core::bench::toolloop` module holds the canned environment (`ToolEnv`, a pure function of case + calls) and the driver (`drive`, one door closure in, one `LoopOutcome` out). The bench's existing agentic pass calls the driver per case per door and appends a row that carries a typed `LoopRow` beside its grade; the report, `compare` and the estimate learn the new suite by name.

**Tech Stack:** Rust (edition 2024, ≥1.88), serde/serde_json/toml already in the tree, no new dependencies.

**Spec:** `docs/superpowers/specs/2026-09-06-tool-loop-probe-design.md` (approved 2026-09-09). Three amendments made by this plan and recorded in the spec's closing section by Task 11: the goal's `contains` is a list `contains_any` (two correct spellings of one edit must both pass); an `Edited` goal may name `untouched` files that must stay byte-identical; and the "not discriminating" clause is per run (`render_run` sees one run), not per candidate set.

## Global Constraints

- Functions ≤ 40 LOC, ≤ 3 arguments, nesting ≤ 3 (`clippy.toml` enforces all three; bundle extra inputs into a struct).
- `unwrap()`/`expect()` only inside `#[cfg(test)]`; every fallible path returns `Result`.
- Exhaustive `match` on every enum of ours; no wildcard arm on our own enums.
- Every externally-deserialized struct carries `#[serde(deny_unknown_fields)]`.
- `tests/**` is read-only (pushkin gate): every new test is an inline `#[cfg(test)]` module beside the code.
- No new `ChekovError` variants. Probe-set defects use `probeset::invalid`; an unreadable loop reply is `ChekovError::ProxyBadRequest { reason }` (chekov's own translator failed, never the model).
- Commit protocol (AGENTS.md): tests first as `test(bench): red — …`, then the implementation as `feat(bench): …`. Every commit message ends with the two trailers:
  `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>` and
  `Claude-Session: https://claude.ai/code/session_01USTtBCMA5rsVnigD6fhoc3`.
- The gate before every commit that is meant to be green: `make lint && make test` (fmt --check, clippy -D warnings, cargo test --locked). A red commit is allowed to fail `make test`; it must still pass `cargo fmt --check`.
- Never `cd` in Bash; use `-C`/`--manifest-path` and absolute paths (a drifted cwd locks the session's tools).
- Branch: `feat/bench-tool-loop`, cut from `origin/develop`; PR base is `develop`.

---

### Task 1: The turn budget knob

**Files:**
- Modify: `src/core/config.rs:141-196` (`BenchSection` and its `Default`)
- Modify: `config.example.toml:48` (after `judge_reasoning_effort`)
- Test: inline `#[cfg(test)]` in `src/core/config.rs` (beside `the_judge_knobs_default_and_parse`)

**Interfaces:**
- Produces: `BenchSection::tool_loop_max_turns: u32` (default 8), read by Task 10.

- [ ] **Step 1: Write the failing test**

Add after `the_judge_knobs_default_and_parse` in `src/core/config.rs`'s test module:

```rust
    #[test]
    fn tool_loop_max_turns_defaults_to_8_and_overrides() {
        assert_eq!(BenchSection::default().tool_loop_max_turns, 8);
        let cfg: super::FileConfig =
            toml::from_str("[bench]\ntool_loop_max_turns = 3\n").expect("overrides parse");
        assert_eq!(cfg.bench.tool_loop_max_turns, 3);
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --manifest-path /Users/amoscoletti/personal_dev/chekov/Cargo.toml tool_loop_max_turns 2>&1 | tail -5`
Expected: compile error `no field tool_loop_max_turns`.

- [ ] **Step 3: Add the field and its default**

In `BenchSection`, after `judge_reasoning_effort`:

```rust
    /// Turn budget (K) for every `tool_loop` case: a loop still calling tools
    /// past it ends `TurnsExhausted`. Part of the agentic prompt-set hash, so
    /// runs judged under different budgets never compare (tool-loop design §8).
    pub tool_loop_max_turns: u32,
```

In `impl Default for BenchSection`, after `judge_reasoning_effort: ReasoningEffort::Low,`:

```rust
            tool_loop_max_turns: 8,
```

In `config.example.toml`, after the `judge_reasoning_effort` line:

```toml
tool_loop_max_turns = 8 # --suite agentic: turn budget per tool_loop case; still calling tools past it fails as exhausted
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --manifest-path /Users/amoscoletti/personal_dev/chekov/Cargo.toml tool_loop_max_turns 2>&1 | tail -3`
Expected: `test result: ok. 1 passed`.

- [ ] **Step 5: Gate and commit**

Run: `make -C /Users/amoscoletti/personal_dev/chekov lint && make -C /Users/amoscoletti/personal_dev/chekov test 2>&1 | grep -E "^test result" | head -1`
Expected: lint clean, `ok. 748 passed`.

```bash
git -C /Users/amoscoletti/personal_dev/chekov add src/core/config.rs config.example.toml
git -C /Users/amoscoletti/personal_dev/chekov commit -F - <<'EOF'
feat(config): [bench] tool_loop_max_turns — the loop probe's turn budget, default 8

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01USTtBCMA5rsVnigD6fhoc3
EOF
```

(One small knob with its test in one commit: the red/green split is for behaviour, and this task's test is the same three lines either way.)

---

### Task 2: The probe set — `LoopCase`, `Goal`, validation, six cases

**Files:**
- Modify: `src/core/bench/probeset.rs` (whole file: types at 14-60, `agentic_v0` at 62-76, `validate_ids` at 156-169, tests at 171-234)
- Modify: `src/core/bench/agentic_v0.toml` (top-level `loop_system`, six `[[tool_loop]]` entries appended after the last `[[instruction]]`)
- Test: inline in `src/core/bench/probeset.rs`

**Interfaces:**
- Produces: `ProbeSet { version, loop_system: String, tool_emit, instruction, tool_loop: Vec<LoopCase> }`, `LoopCase { id, prompt, files: Vec<CannedFile>, goal: Goal, tools: Vec<ToolDef> }`, `CannedFile { path, text }`, `Goal::Edited { file, contains_any: Vec<String>, untouched: Vec<String>, tests_fail: Option<String> } | Goal::Unchanged { reply_mentions: String }`, `pub(crate) fn parse(text: &str) -> Result<ProbeSet, ChekovError>`. Tasks 4, 5, 6, 10 consume these.

- [ ] **Step 1: Write the failing tests**

Replace the `use` line of the test module with `use super::{Expect, ProbeSet, agentic_v0, content_hash};` and add:

```rust
    /// A one-case set for validation tests: the goal and palette are the
    /// variable parts; the file is `src/a.rs` holding `const A: u32 = 3;`.
    fn loop_set(goal: &str, tools: &[&str]) -> Result<ProbeSet, crate::error::ChekovError> {
        let palette: String = tools
            .iter()
            .map(|name| {
                let schema = match *name {
                    "grep" => r#"{"type":"object","properties":{"pattern":{"type":"string"},"path":{"type":"string"}},"required":["pattern","path"]}"#,
                    "edit_file" => r#"{"type":"object","properties":{"path":{"type":"string"},"old":{"type":"string"},"new":{"type":"string"}},"required":["path","old","new"]}"#,
                    "run_tests" => r#"{"type":"object","properties":{"filter":{"type":"string"}},"required":["filter"]}"#,
                    _ => r#"{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}"#,
                };
                format!("[[tool_loop.tools]]\nname = \"{name}\"\ndescription = \"d\"\ninput_schema = '{schema}'\n")
            })
            .collect();
        super::parse(&format!(
            "version = 0\nloop_system = \"s\"\ntool_emit = []\ninstruction = []\n\
             [[tool_loop]]\nid = \"tl-x\"\nprompt = \"p\"\n\
             [[tool_loop.files]]\npath = \"src/a.rs\"\ntext = \"const A: u32 = 3;\\n\"\n\
             [tool_loop.goal]\n{goal}\n{palette}"
        ))
    }

    const EDITED: &str = "kind = \"edited\"\nfile = \"src/a.rs\"\ncontains_any = [\"const A: u32 = 5;\"]";

    #[test]
    fn the_shipped_loop_cases_parse_with_the_seed_count() {
        let set = agentic_v0().expect("valid");
        assert_eq!(set.tool_loop.len(), 6);
        assert!(!set.loop_system.is_empty(), "the system text rides in the set");
        assert!(set.tool_loop.iter().all(|c| c.id.starts_with("tl-")));
    }

    #[test]
    fn a_loop_goal_already_met_by_the_canned_files_is_refused() {
        let err = loop_set(
            "kind = \"edited\"\nfile = \"src/a.rs\"\ncontains_any = [\"const A: u32 = 3;\"]",
            &["read_file", "edit_file"],
        )
        .expect_err("a met goal grades doing nothing as done");
        assert!(err.to_string().contains("already in 'src/a.rs'"), "{err}");
    }

    #[test]
    fn a_loop_goal_naming_a_file_the_case_lacks_is_refused() {
        let err = loop_set(
            "kind = \"edited\"\nfile = \"src/b.rs\"\ncontains_any = [\"x\"]",
            &["read_file", "edit_file"],
        )
        .expect_err("no such canned file");
        assert!(err.to_string().contains("'src/b.rs' is not in the case's files"), "{err}");
    }

    #[test]
    fn an_edited_goal_needs_edit_file_and_run_tests_exactly_with_tests_fail() {
        let err = loop_set(EDITED, &["read_file"]).expect_err("no edit_file");
        assert!(err.to_string().contains("needs edit_file"), "{err}");
        let err = loop_set(EDITED, &["read_file", "edit_file", "run_tests"])
            .expect_err("run_tests without tests_fail");
        assert!(err.to_string().contains("exactly when tests_fail"), "{err}");
        let with_tests = format!("{EDITED}\ntests_fail = \"test a ... FAILED\"");
        loop_set(&with_tests, &["read_file", "edit_file", "run_tests"]).expect("consistent");
        let err = loop_set(&with_tests, &["read_file", "edit_file"]).expect_err("tests_fail without run_tests");
        assert!(err.to_string().contains("exactly when tests_fail"), "{err}");
    }

    #[test]
    fn an_unchanged_goal_offers_no_tests_and_a_palette_tool_must_be_canned() {
        let err = loop_set(
            "kind = \"unchanged\"\nreply_mentions = \"src/legacy.rs\"",
            &["read_file", "run_tests"],
        )
        .expect_err("nothing to test");
        assert!(err.to_string().contains("no tests to run"), "{err}");
        let err = loop_set(EDITED, &["read_file", "edit_file", "delete_file"])
            .expect_err("the environment cannot answer delete_file");
        assert!(err.to_string().contains("'delete_file' has no canned behaviour"), "{err}");
    }

    #[test]
    fn a_loop_case_id_may_not_repeat_an_id_from_another_array() {
        let text = format!(
            "version = 0\nloop_system = \"s\"\ninstruction = []\n\
             [[tool_emit]]\nid = \"tl-x\"\nprompt = \"p\"\nexpect = \"abstain\"\n\
             [[tool_emit.tools]]\nname = \"read_file\"\ndescription = \"d\"\n\
             input_schema = '{{\"type\":\"object\",\"properties\":{{\"path\":{{\"type\":\"string\"}}}},\"required\":[\"path\"]}}'\n\
             [[tool_loop]]\nid = \"tl-x\"\nprompt = \"p\"\n\
             [[tool_loop.files]]\npath = \"src/a.rs\"\ntext = \"const A: u32 = 3;\\n\"\n\
             [tool_loop.goal]\n{EDITED}\n\
             [[tool_loop.tools]]\nname = \"edit_file\"\ndescription = \"d\"\n\
             input_schema = '{{\"type\":\"object\",\"properties\":{{\"path\":{{\"type\":\"string\"}},\"old\":{{\"type\":\"string\"}},\"new\":{{\"type\":\"string\"}}}},\"required\":[\"path\",\"old\",\"new\"]}}'\n"
        );
        let err = super::parse(&text).expect_err("duplicate across arrays");
        assert!(err.to_string().contains("duplicate case id 'tl-x'"), "{err}");
    }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --manifest-path /Users/amoscoletti/personal_dev/chekov/Cargo.toml probeset 2>&1 | grep -E "^error|no function|no field" | head -5`
Expected: compile errors — `parse` and `tool_loop` do not exist.

- [ ] **Step 3: Commit the red**

```bash
cargo fmt --manifest-path /Users/amoscoletti/personal_dev/chekov/Cargo.toml
git -C /Users/amoscoletti/personal_dev/chekov add src/core/bench/probeset.rs
git -C /Users/amoscoletti/personal_dev/chekov commit -F - <<'EOF'
test(bench): red — loop cases parse with a goal and a palette, and every way a case can lie is refused at load

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01USTtBCMA5rsVnigD6fhoc3
EOF
```

- [ ] **Step 4: The types**

In `src/core/bench/probeset.rs`, replace `ProbeSet` with:

```rust
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeSet {
    pub version: u32,
    /// The system text every `tool_loop` turn carries — in the set, so the
    /// content hash covers it.
    pub loop_system: String,
    pub tool_emit: Vec<ToolCase>,
    pub instruction: Vec<InstructionCase>,
    pub tool_loop: Vec<LoopCase>,
}
```

After `InstructionCase`, add:

```rust
/// A `tool_loop` case (tool-loop design §3): a canned repository, a task, a
/// palette, and the terminal state that counts as done.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoopCase {
    pub id: String,
    pub prompt: String,
    pub files: Vec<CannedFile>,
    pub goal: Goal,
    pub tools: Vec<ToolDef>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CannedFile {
    pub path: String,
    pub text: String,
}

/// What "done" means. `Edited`: the named file carries one of `contains_any`
/// (alternatives, because two correct spellings of one edit must both pass)
/// and every `untouched` file is byte-identical to its canned copy.
/// `Unchanged`: no file differs and the final reply names `reply_mentions`.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum Goal {
    Edited {
        file: String,
        contains_any: Vec<String>,
        #[serde(default)]
        untouched: Vec<String>,
        /// What `run_tests` answers before the goal is met. Its presence is
        /// what puts `run_tests` in the palette.
        #[serde(default)]
        tests_fail: Option<String>,
    },
    Unchanged {
        reply_mentions: String,
    },
}

/// The tools the canned environment can answer (`toolloop::ToolEnv`). A
/// palette naming anything else is refused at load, never answered wrong.
pub(crate) const CANNED_TOOLS: [&str; 5] = ["read_file", "list_dir", "grep", "edit_file", "run_tests"];
```

(If `deny_unknown_fields` on the internally tagged enum fails to compile or to parse the TOML, drop it from the enum only — the containing `LoopCase` still denies unknown keys at its level.)

- [ ] **Step 5: Parse, validate**

Replace `agentic_v0` with:

```rust
/// The v0 set, validated. Loud on any defect — a malformed case must never
/// silently grade.
pub fn agentic_v0() -> Result<ProbeSet, ChekovError> {
    parse(AGENTIC_V0)
}

/// Any set's text, parsed and validated — the compiled-in one, or a test's.
pub(crate) fn parse(text: &str) -> Result<ProbeSet, ChekovError> {
    let set: ProbeSet = toml::from_str(text).map_err(|e| invalid(e.to_string()))?;
    if set.version != SUPPORTED_VERSION {
        return Err(invalid(format!(
            "version {} — this chekov reads {SUPPORTED_VERSION}",
            set.version
        )));
    }
    validate_tool_cases(&set)?;
    validate_checks(&set)?;
    validate_loop_cases(&set)?;
    validate_ids(&set)?;
    Ok(set)
}
```

Add after `validate_tool_cases`:

```rust
/// Every loop case must be answerable from the palette it offers and unmet
/// at turn zero — a goal the canned files already satisfy would grade a
/// model that did nothing as done.
fn validate_loop_cases(set: &ProbeSet) -> Result<(), ChekovError> {
    for case in &set.tool_loop {
        for tool in &case.tools {
            serde_json::from_str::<serde_json::Value>(&tool.input_schema).map_err(|e| {
                invalid(format!("{}: tool {} schema is not JSON: {e}", case.id, tool.name))
            })?;
            if !CANNED_TOOLS.contains(&tool.name.as_str()) {
                return Err(invalid(format!(
                    "{}: '{}' has no canned behaviour in the loop environment",
                    case.id, tool.name
                )));
            }
        }
        validate_goal(case)?;
    }
    Ok(())
}

fn validate_goal(case: &LoopCase) -> Result<(), ChekovError> {
    let offers = |name: &str| case.tools.iter().any(|t| t.name == name);
    match &case.goal {
        Goal::Edited {
            file,
            contains_any,
            untouched,
            tests_fail,
        } => {
            validate_edit_target(case, file, contains_any)?;
            if let Some(missing) = untouched.iter().find(|p| canned_text(case, p).is_none()) {
                return Err(invalid(format!(
                    "{}: untouched file '{missing}' is not in the case's files",
                    case.id
                )));
            }
            if !offers("edit_file") {
                return Err(invalid(format!("{}: an edited goal needs edit_file in the palette", case.id)));
            }
            if tests_fail.is_some() != offers("run_tests") {
                return Err(invalid(format!(
                    "{}: run_tests is in the palette exactly when tests_fail is set",
                    case.id
                )));
            }
        }
        Goal::Unchanged { .. } => {
            if offers("run_tests") {
                return Err(invalid(format!("{}: an unchanged goal has no tests to run", case.id)));
            }
        }
    }
    Ok(())
}

/// The goal file exists in the case and does not already carry the answer.
fn validate_edit_target(case: &LoopCase, file: &str, wanted: &[String]) -> Result<(), ChekovError> {
    let text = canned_text(case, file).ok_or_else(|| {
        invalid(format!("{}: goal file '{file}' is not in the case's files", case.id))
    })?;
    if wanted.is_empty() {
        return Err(invalid(format!("{}: contains_any is empty", case.id)));
    }
    if let Some(present) = wanted.iter().find(|w| text.contains(w.as_str())) {
        return Err(invalid(format!(
            "{}: goal text {present:?} is already in '{file}'",
            case.id
        )));
    }
    Ok(())
}

pub(crate) fn canned_text<'a>(case: &'a LoopCase, path: &str) -> Option<&'a str> {
    case.files
        .iter()
        .find(|f| f.path == path)
        .map(|f| f.text.as_str())
}
```

In `validate_ids`, extend the chain:

```rust
    let ids = set
        .tool_emit
        .iter()
        .map(|c| &c.id)
        .chain(set.instruction.iter().map(|c| &c.id))
        .chain(set.tool_loop.iter().map(|c| &c.id));
```

- [ ] **Step 6: The content**

In `src/core/bench/agentic_v0.toml`, change the header comment's count line to
`# Counts are a seed (7 call / 2 abstention / 1 missing-function; 12
# instruction; 6 tool_loop) — grow toward the spec's 30/40, never edit in place.`
and after `version = 0` add:

```toml

# The system text every tool_loop turn carries (tool-loop design §5).
loop_system = "You are a coding agent working in a small repository through the tools provided. Read before you edit, edit with exact text, and verify with the test tool whenever it is offered. When the task is complete, or cannot be done with the tools you have, reply in plain text and stop."
```

Append at the end of the file:

```toml

# ---------------------------------------------------------------- tool_loop
# Six canned read→edit→verify loops (tool-loop design §3). Every tool answer
# is a pure function of the case and the calls so far; the grade is the end
# state, never the path.

[[tool_loop]]
id = "tl-001"
prompt = "MAX_RETRIES in src/config.rs must be 5, not 3. Fix it and run the tests."
[[tool_loop.files]]
path = "src/config.rs"
text = "/// How many times a request is retried before it fails.\npub const MAX_RETRIES: u32 = 3;\n"
[tool_loop.goal]
kind = "edited"
file = "src/config.rs"
contains_any = ["pub const MAX_RETRIES: u32 = 5;"]
tests_fail = "test retries_default ... FAILED: expected 5, got 3"
[[tool_loop.tools]]
name = "read_file"
description = "Read a file from the repository and return its contents."
input_schema = '{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}'
[[tool_loop.tools]]
name = "edit_file"
description = "Replace one exact text occurrence in a file."
input_schema = '{"type":"object","properties":{"path":{"type":"string"},"old":{"type":"string"},"new":{"type":"string"}},"required":["path","old","new"]}'
[[tool_loop.tools]]
name = "run_tests"
description = "Run the test suite, optionally filtered to test names containing a string."
input_schema = '{"type":"object","properties":{"filter":{"type":"string"}},"required":["filter"]}'

[[tool_loop]]
id = "tl-002"
prompt = "The function port_of returns the wrong default port: it must return 8080, not 8000. Find it and fix it."
[[tool_loop.files]]
path = "src/server.rs"
text = "pub fn port_of(explicit: Option<u16>) -> u16 {\n    explicit.unwrap_or(8000)\n}\n"
[[tool_loop.files]]
path = "src/client.rs"
text = "pub fn connect(port: u16) -> String {\n    format!(\"http://127.0.0.1:{port}\")\n}\n"
[tool_loop.goal]
kind = "edited"
file = "src/server.rs"
contains_any = ["explicit.unwrap_or(8080)"]
untouched = ["src/client.rs"]
[[tool_loop.tools]]
name = "grep"
description = "Search files under a path for a pattern; returns matching lines."
input_schema = '{"type":"object","properties":{"pattern":{"type":"string"},"path":{"type":"string"}},"required":["pattern","path"]}'
[[tool_loop.tools]]
name = "read_file"
description = "Read a file from the repository and return its contents."
input_schema = '{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}'
[[tool_loop.tools]]
name = "edit_file"
description = "Replace one exact text occurrence in a file."
input_schema = '{"type":"object","properties":{"path":{"type":"string"},"old":{"type":"string"},"new":{"type":"string"}},"required":["path","old","new"]}'

# The near miss: nothing in the file says what the right value is; only the
# failing test does. A loop that edits without running the tests guesses.

[[tool_loop]]
id = "tl-003"
prompt = "The paging test fails. Fix src/paging.rs so it passes; use the tests to check."
[[tool_loop.files]]
path = "src/paging.rs"
text = "/// Rows per page returned to the client.\npub const PAGE_SIZE: usize = 50;\n"
[tool_loop.goal]
kind = "edited"
file = "src/paging.rs"
contains_any = ["pub const PAGE_SIZE: usize = 100;"]
tests_fail = "test page_size_matches_api ... FAILED: expected PAGE_SIZE == 100 (the API's maximum), got a different value"
[[tool_loop.tools]]
name = "read_file"
description = "Read a file from the repository and return its contents."
input_schema = '{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}'
[[tool_loop.tools]]
name = "edit_file"
description = "Replace one exact text occurrence in a file."
input_schema = '{"type":"object","properties":{"path":{"type":"string"},"old":{"type":"string"},"new":{"type":"string"}},"required":["path","old","new"]}'
[[tool_loop.tools]]
name = "run_tests"
description = "Run the test suite, optionally filtered to test names containing a string."
input_schema = '{"type":"object","properties":{"filter":{"type":"string"}},"required":["filter"]}'

# Disambiguation: two files define `version`; the prompt names the module.

[[tool_loop]]
id = "tl-004"
prompt = "In the api module, version() must return \"v2\", not \"v1\". Change only that module."
[[tool_loop.files]]
path = "src/lib.rs"
text = "pub mod api;\nmod internal;\n"
[[tool_loop.files]]
path = "src/api.rs"
text = "pub fn version() -> &'static str {\n    \"v1\"\n}\n"
[[tool_loop.files]]
path = "src/internal.rs"
text = "pub(crate) fn version() -> &'static str {\n    \"v1\"\n}\n"
[tool_loop.goal]
kind = "edited"
file = "src/api.rs"
contains_any = ["\"v2\""]
untouched = ["src/internal.rs", "src/lib.rs"]
[[tool_loop.tools]]
name = "list_dir"
description = "List the entries of a repository directory."
input_schema = '{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}'
[[tool_loop.tools]]
name = "read_file"
description = "Read a file from the repository and return its contents."
input_schema = '{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}'
[[tool_loop.tools]]
name = "edit_file"
description = "Replace one exact text occurrence in a file."
input_schema = '{"type":"object","properties":{"path":{"type":"string"},"old":{"type":"string"},"new":{"type":"string"}},"required":["path","old","new"]}'

# Abstention inside a loop: the named file does not exist. Done means no
# file changed and the reply says which path is missing.

[[tool_loop]]
id = "tl-005"
prompt = "Fix the off-by-one in src/legacy.rs: the loop bound should be <= rather than <."
[[tool_loop.files]]
path = "src/main.rs"
text = "fn main() {\n    println!(\"hello\");\n}\n"
[[tool_loop.files]]
path = "src/lib.rs"
text = "pub mod util;\n"
[[tool_loop.files]]
path = "src/util.rs"
text = "pub fn twice(n: u32) -> u32 {\n    n * 2\n}\n"
[tool_loop.goal]
kind = "unchanged"
reply_mentions = "src/legacy.rs"
[[tool_loop.tools]]
name = "list_dir"
description = "List the entries of a repository directory."
input_schema = '{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}'
[[tool_loop.tools]]
name = "read_file"
description = "Read a file from the repository and return its contents."
input_schema = '{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}'
[[tool_loop.tools]]
name = "edit_file"
description = "Replace one exact text occurrence in a file."
input_schema = '{"type":"object","properties":{"path":{"type":"string"},"old":{"type":"string"},"new":{"type":"string"}},"required":["path","old","new"]}'

# Cross-file: the invariant is documented in one file and violated in another.

[[tool_loop]]
id = "tl-006"
prompt = "src/ledger.rs must honour the invariant documented in src/money.rs. Make the one change needed in src/ledger.rs."
[[tool_loop.files]]
path = "src/money.rs"
text = "/// Amounts are integer cents. INVARIANT: never construct a `Cents` from a\n/// float — use `Cents::from_str_major` for decimal strings.\npub struct Cents(pub i128);\n\nimpl Cents {\n    pub fn from_major(major: f64) -> Self {\n        Self((major * 100.0) as i128)\n    }\n\n    pub fn from_str_major(text: &str) -> Self {\n        let (whole, frac) = text.split_once('.').unwrap_or((text, \"0\"));\n        Self(whole.parse::<i128>().unwrap_or(0) * 100 + frac.parse::<i128>().unwrap_or(0))\n    }\n}\n"
[[tool_loop.files]]
path = "src/ledger.rs"
text = "use crate::money::Cents;\n\npub fn credit(amount: &str) -> Cents {\n    Cents::from_major(amount.parse::<f64>().unwrap_or(0.0))\n}\n"
[tool_loop.goal]
kind = "edited"
file = "src/ledger.rs"
contains_any = ["Cents::from_str_major(amount)", "Cents::from_str_major(amount.trim())"]
untouched = ["src/money.rs"]
[[tool_loop.tools]]
name = "read_file"
description = "Read a file from the repository and return its contents."
input_schema = '{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}'
[[tool_loop.tools]]
name = "edit_file"
description = "Replace one exact text occurrence in a file."
input_schema = '{"type":"object","properties":{"path":{"type":"string"},"old":{"type":"string"},"new":{"type":"string"}},"required":["path","old","new"]}'
```

- [ ] **Step 7: Run the probeset tests to verify they pass**

Run: `cargo test --manifest-path /Users/amoscoletti/personal_dev/chekov/Cargo.toml probeset 2>&1 | grep -E "^test |test result"`
Expected: every `probeset::tests::*` passes, including the four pre-existing ones.

- [ ] **Step 8: Gate and commit**

Run: `make -C /Users/amoscoletti/personal_dev/chekov lint && make -C /Users/amoscoletti/personal_dev/chekov test 2>&1 | grep -E "^test result" | head -1`
Expected: clean; all green (the estimate test in `capability.rs` still passes — it does not count loop cases yet).

```bash
git -C /Users/amoscoletti/personal_dev/chekov add src/core/bench/probeset.rs src/core/bench/agentic_v0.toml
git -C /Users/amoscoletti/personal_dev/chekov commit -F - <<'EOF'
feat(bench): the probe set's third array — six tool_loop cases with canned files, a goal and a palette, every lie refused at load

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01USTtBCMA5rsVnigD6fhoc3
EOF
```

---

### Task 3: The store's vocabulary — `LoopEnd`, `LoopRow`, the row field, the suite names

**Files:**
- Modify: `src/core/bench/store.rs:28-32` (consts), `:52-77` (`TaskRow`), `:280-291` (`Task`), `:346-369` (`append`), plus the `JudgeRow` neighbourhood for the new types
- Modify: every `Task { … }` / `TaskRow { … }` literal in `src/` (the compiler lists them)
- Test: inline in `src/core/bench/store.rs` (beside `a_judge_row_round_trips_and_an_old_row_loads_without_one`)

**Interfaces:**
- Produces: `store::LoopEnd` (`GoalMet | GoalUnmet { wanted } | TurnsExhausted | Truncated | FabricatedTool { name } | MalformedCall { name, key }`), `store::LoopRow { turns: u32, tool_calls: u32, end: LoopEnd }`, `Task::tool_loop: Option<LoopRow>`, `TaskRow::tool_loop: Option<LoopRow>`, `AGENTIC` now four suites, `PAIRED` three. Tasks 4–10 consume these.

- [ ] **Step 1: Write the failing test**

After `a_judge_row_round_trips_and_an_old_row_loads_without_one` in `store.rs`'s test module:

```rust
    #[test]
    fn a_tool_loop_row_round_trips_and_an_old_row_loads_without_one() {
        let row: TaskRow = serde_json::from_str(PRE_C_ROW).expect("loads");
        assert!(row.tool_loop.is_none(), "rows from before the field carry none");
        let end = LoopEnd::FabricatedTool { name: "rm".into() };
        let json = serde_json::to_string(&LoopRow {
            turns: 2,
            tool_calls: 3,
            end: end.clone(),
        })
        .expect("ser");
        assert_eq!(
            json,
            r#"{"turns":2,"tool_calls":3,"end":{"kind":"fabricated_tool","name":"rm"}}"#
        );
        let back: LoopRow = serde_json::from_str(&json).expect("de");
        assert_eq!(back.end, end);
        assert_eq!(
            serde_json::to_string(&LoopEnd::GoalMet).expect("ser"),
            r#"{"kind":"goal_met"}"#
        );
        assert!(AGENTIC.contains(&"tool_loop") && PAIRED.contains(&"tool_loop"));
    }
```

Add `LoopEnd, LoopRow` to the test module's `use super::{…}` line (find the line that imports `JudgeRow`).

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --manifest-path /Users/amoscoletti/personal_dev/chekov/Cargo.toml a_tool_loop_row_round_trips 2>&1 | grep -E "^error" | head -3`
Expected: `cannot find type LoopEnd`.

- [ ] **Step 3: Commit the red**

```bash
cargo fmt --manifest-path /Users/amoscoletti/personal_dev/chekov/Cargo.toml
git -C /Users/amoscoletti/personal_dev/chekov add src/core/bench/store.rs
git -C /Users/amoscoletti/personal_dev/chekov commit -F - <<'EOF'
test(bench): red — a tool_loop row carries a typed end state and turn count; rows from before the field load without one

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01USTtBCMA5rsVnigD6fhoc3
EOF
```

- [ ] **Step 4: The types and the field**

In `store.rs`, change the two consts:

```rust
/// The suites whose rows are graded per case.
pub(crate) const AGENTIC: [&str; 4] = ["tool_emit", "grammar_gap", "instruction", "tool_loop"];

/// The suites crossed through both doors, so a case can disagree with itself.
const PAIRED: [&str; 3] = ["tool_emit", "instruction", "tool_loop"];
```

After `JudgeRow`, add:

```rust
/// How a `tool_loop` crossing ended (tool-loop design §6). The grade is a
/// function of this and nothing else; the path is never scored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LoopEnd {
    GoalMet,
    GoalUnmet { wanted: String },
    TurnsExhausted,
    Truncated,
    FabricatedTool { name: String },
    MalformedCall { name: String, key: String },
}

/// What a `tool_loop` row records beside its grade: how long the loop ran
/// and how it stopped. Printed beside the count, never folded into it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoopRow {
    pub turns: u32,
    pub tool_calls: u32,
    pub end: LoopEnd,
}
```

In `TaskRow`, after `judge`:

```rust
    /// Present on `tool_loop` rows only: turns, calls and the end state.
    /// Rows written before the field load as `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_loop: Option<LoopRow>,
```

In `Task`, after `judge`:

```rust
    /// Present on `tool_loop` rows only — see `TaskRow::tool_loop`.
    pub tool_loop: Option<LoopRow>,
```

In `RunWriter::append`, after `judge: task.judge,`: `tool_loop: task.tool_loop,`.

- [ ] **Step 5: Every literal**

Run: `cargo build --manifest-path /Users/amoscoletti/personal_dev/chekov/Cargo.toml --all-targets 2>&1 | grep -B2 "missing field .tool_loop." | grep -oE "src/[^:]+:[0-9]+" | sort -u`

Add `tool_loop: None,` after `judge: None,` (or after `judge: Some(…),`) in every listed literal. Expected sites: `src/commands/capability.rs` (`append_probe`, `append_unavailable`, test helpers `crossing_task`, the judge-run fixture), `src/core/bench/codebase/run.rs` (`record_codebase_task`, `run_head` if it builds a `Task`), `src/core/bench/judge.rs` (the verdict row and its test), `src/core/bench/store.rs` tests (`graded`, `judge_task`), `src/core/bench/compare.rs` tests (`graded_row`, `codebase_row`, the judge row fixture), `src/core/bench/speeds.rs` test `depth_row`. Repeat the build until the list is empty.

- [ ] **Step 6: Run the test to verify it passes**

Run: `cargo test --manifest-path /Users/amoscoletti/personal_dev/chekov/Cargo.toml a_tool_loop_row_round_trips 2>&1 | tail -3`
Expected: `ok. 1 passed`.

- [ ] **Step 7: Gate and commit**

Run: `make -C /Users/amoscoletti/personal_dev/chekov lint && make -C /Users/amoscoletti/personal_dev/chekov test 2>&1 | grep -E "^test result" | head -1`
Expected: clean and green. (`suite_summaries` and `compare` now see `tool_loop` in `AGENTIC`; with no such rows in any fixture nothing changes yet.)

```bash
git -C /Users/amoscoletti/personal_dev/chekov add -A src/
git -C /Users/amoscoletti/personal_dev/chekov commit -F - <<'EOF'
feat(bench): LoopEnd and LoopRow on the row — a tool_loop crossing records how it stopped; tool_loop joins the graded and the paired suites

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01USTtBCMA5rsVnigD6fhoc3
EOF
```

---

### Task 4: The canned environment — `toolloop::ToolEnv`

**Files:**
- Create: `src/core/bench/toolloop.rs`
- Modify: `src/core/bench/mod.rs` (add `pub mod toolloop;` after `store`)
- Modify: `src/core/bench/grade.rs:68-94` (`ToolUse` with ids; `tool_uses` re-expressed over it)
- Test: inline in `src/core/bench/toolloop.rs`

**Interfaces:**
- Consumes: `probeset::{Goal, LoopCase, ToolDef, canned_text, parse}` (Task 2), `store::LoopEnd` (Task 3).
- Produces: `grade::ToolUse { id: String, name: String, input: Value }`, `grade::tool_use_blocks(body) -> Result<Vec<ToolUse>, Grade>`, `toolloop::ToolEnv::new(&LoopCase)`, `ToolEnv::answer(&mut self, &ToolUse) -> Result<String, LoopEnd>`, `ToolEnv::finish(&self, final_text: &str) -> LoopEnd`. Task 6 consumes these.

- [ ] **Step 1: Write the failing tests**

Create `src/core/bench/toolloop.rs` with only a test module for now:

```rust
//! The `tool_loop` probe's canned environment and driver (tool-loop design
//! §4–§5): a repository as a map, every tool answer a pure function of the
//! case and the calls so far, and a loop that stops at a terminal state.

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::ToolEnv;
    use crate::core::bench::grade::ToolUse;
    use crate::core::bench::probeset::{LoopCase, ProbeSet, parse};
    use crate::core::bench::store::LoopEnd;

    const TOOLS: &str = r#"
[[tool_loop.tools]]
name = "read_file"
description = "d"
input_schema = '{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}'
[[tool_loop.tools]]
name = "list_dir"
description = "d"
input_schema = '{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}'
[[tool_loop.tools]]
name = "grep"
description = "d"
input_schema = '{"type":"object","properties":{"pattern":{"type":"string"},"path":{"type":"string"}},"required":["pattern","path"]}'
[[tool_loop.tools]]
name = "edit_file"
description = "d"
input_schema = '{"type":"object","properties":{"path":{"type":"string"},"old":{"type":"string"},"new":{"type":"string"}},"required":["path","old","new"]}'
"#;

    /// Two files under `src/`, an edited goal on `src/a.rs`, a test tool.
    pub(super) fn edited_set() -> ProbeSet {
        parse(&format!(
            "version = 0\nloop_system = \"s\"\ntool_emit = []\ninstruction = []\n\
             [[tool_loop]]\nid = \"tl-x\"\nprompt = \"p\"\n\
             [[tool_loop.files]]\npath = \"src/a.rs\"\ntext = \"const A: u32 = 3;\\nconst B: u32 = 3;\\n\"\n\
             [[tool_loop.files]]\npath = \"src/sub/b.rs\"\ntext = \"fn b() {{}}\\n\"\n\
             [tool_loop.goal]\nkind = \"edited\"\nfile = \"src/a.rs\"\n\
             contains_any = [\"const A: u32 = 5;\"]\nuntouched = [\"src/sub/b.rs\"]\n\
             tests_fail = \"test a ... FAILED: expected 5\"\n{TOOLS}\
             [[tool_loop.tools]]\nname = \"run_tests\"\ndescription = \"d\"\n\
             input_schema = '{{\"type\":\"object\",\"properties\":{{\"filter\":{{\"type\":\"string\"}}}},\"required\":[\"filter\"]}}'\n"
        ))
        .expect("valid")
    }

    pub(super) fn unchanged_set() -> ProbeSet {
        parse(&format!(
            "version = 0\nloop_system = \"s\"\ntool_emit = []\ninstruction = []\n\
             [[tool_loop]]\nid = \"tl-u\"\nprompt = \"p\"\n\
             [[tool_loop.files]]\npath = \"src/a.rs\"\ntext = \"const A: u32 = 3;\\n\"\n\
             [tool_loop.goal]\nkind = \"unchanged\"\nreply_mentions = \"src/legacy.rs\"\n{TOOLS}"
        ))
        .expect("valid")
    }

    fn case(set: &ProbeSet) -> &LoopCase {
        &set.tool_loop[0]
    }

    fn call(name: &str, input: Value) -> ToolUse {
        ToolUse {
            id: "t1".into(),
            name: name.into(),
            input,
        }
    }

    fn edit(path: &str, old: &str, new: &str) -> ToolUse {
        call("edit_file", json!({"path": path, "old": old, "new": new}))
    }

    #[test]
    fn read_file_answers_the_text_or_names_the_missing_path() {
        let set = edited_set();
        let mut env = ToolEnv::new(case(&set));
        assert_eq!(
            env.answer(&call("read_file", json!({"path": "src/a.rs"}))).expect("answered"),
            "const A: u32 = 3;\nconst B: u32 = 3;\n"
        );
        assert_eq!(
            env.answer(&call("read_file", json!({"path": "src/zz.rs"}))).expect("answered"),
            "no such file: src/zz.rs"
        );
    }

    #[test]
    fn list_dir_shows_direct_children_and_marks_directories() {
        let set = edited_set();
        let mut env = ToolEnv::new(case(&set));
        assert_eq!(
            env.answer(&call("list_dir", json!({"path": "src"}))).expect("answered"),
            "a.rs\nsub/"
        );
        assert_eq!(
            env.answer(&call("list_dir", json!({"path": "."}))).expect("answered"),
            "src/"
        );
        assert_eq!(
            env.answer(&call("list_dir", json!({"path": "docs"}))).expect("answered"),
            "no such directory: docs"
        );
    }

    #[test]
    fn grep_is_a_substring_match_over_a_path_prefix_or_one_file() {
        let set = edited_set();
        let mut env = ToolEnv::new(case(&set));
        assert_eq!(
            env.answer(&call("grep", json!({"pattern": ": u32", "path": "src"}))).expect("answered"),
            "src/a.rs:1: const A: u32 = 3;\nsrc/a.rs:2: const B: u32 = 3;"
        );
        assert_eq!(
            env.answer(&call("grep", json!({"pattern": ".", "path": "src/sub/b.rs"}))).expect("answered"),
            "no matches",
            "a dot is a literal dot, not a regex"
        );
    }

    #[test]
    fn edit_file_replaces_exactly_one_occurrence_and_names_zero_or_many() {
        let set = edited_set();
        let mut env = ToolEnv::new(case(&set));
        assert_eq!(
            env.answer(&edit("src/a.rs", "= 3;\nconst B", "= 5;\nconst B")).expect("answered"),
            "edited src/a.rs"
        );
        assert_eq!(
            env.answer(&call("read_file", json!({"path": "src/a.rs"}))).expect("answered"),
            "const A: u32 = 5;\nconst B: u32 = 3;\n"
        );
        assert_eq!(
            env.answer(&edit("src/a.rs", "nope", "x")).expect("answered"),
            "old text not found in src/a.rs"
        );
        assert_eq!(
            env.answer(&edit("src/a.rs", "u32", "u64")).expect("answered"),
            "old text occurs 2 times in src/a.rs; make it unique"
        );
        assert_eq!(
            env.answer(&edit("src/zz.rs", "a", "b")).expect("answered"),
            "no such file: src/zz.rs"
        );
    }

    #[test]
    fn run_tests_fails_with_the_canned_line_until_the_goal_is_met() {
        let set = edited_set();
        let mut env = ToolEnv::new(case(&set));
        let tests = call("run_tests", json!({"filter": ""}));
        assert_eq!(env.answer(&tests).expect("answered"), "test a ... FAILED: expected 5");
        env.answer(&edit("src/a.rs", "A: u32 = 3", "A: u32 = 5")).expect("answered");
        assert_eq!(env.answer(&tests).expect("answered"), "ok. 1 passed");
        assert_eq!(env.finish("done"), LoopEnd::GoalMet);
    }

    #[test]
    fn an_untouched_file_that_changed_keeps_the_goal_unmet() {
        let set = edited_set();
        let mut env = ToolEnv::new(case(&set));
        env.answer(&edit("src/a.rs", "A: u32 = 3", "A: u32 = 5")).expect("answered");
        env.answer(&edit("src/sub/b.rs", "fn b", "fn c")).expect("answered");
        assert_eq!(
            env.finish("done"),
            LoopEnd::GoalUnmet {
                wanted: "src/a.rs containing \"const A: u32 = 5;\"".into()
            }
        );
    }

    #[test]
    fn a_tool_outside_the_palette_or_a_call_missing_a_required_key_ends_the_loop() {
        let set = edited_set();
        let mut env = ToolEnv::new(case(&set));
        assert_eq!(
            env.answer(&call("delete_file", json!({"path": "src/a.rs"}))).expect_err("fabricated"),
            LoopEnd::FabricatedTool {
                name: "delete_file".into()
            }
        );
        assert_eq!(
            env.answer(&call("edit_file", json!({"path": "src/a.rs", "old": "x"}))).expect_err("malformed"),
            LoopEnd::MalformedCall {
                name: "edit_file".into(),
                key: "new".into()
            }
        );
    }

    #[test]
    fn an_unchanged_goal_is_met_only_when_nothing_changed_and_the_reply_names_the_path() {
        let set = unchanged_set();
        let mut env = ToolEnv::new(case(&set));
        assert_eq!(
            env.answer(&call("read_file", json!({"path": "src/legacy.rs"}))).expect("answered"),
            "no such file: src/legacy.rs"
        );
        assert_eq!(env.finish("There is no SRC/LEGACY.RS in this repository."), LoopEnd::GoalMet);
        assert_eq!(
            env.finish("Fixed it."),
            LoopEnd::GoalUnmet {
                wanted: "no file changed and a reply naming src/legacy.rs".into()
            }
        );
        env.answer(&edit("src/a.rs", "3", "4")).expect("answered");
        assert!(matches!(env.finish("no src/legacy.rs here"), LoopEnd::GoalUnmet { .. }));
    }

    #[test]
    fn two_environments_fed_the_same_calls_hold_the_same_state() {
        let set = edited_set();
        let (mut one, mut two) = (ToolEnv::new(case(&set)), ToolEnv::new(case(&set)));
        for env in [&mut one, &mut two] {
            env.answer(&edit("src/a.rs", "A: u32 = 3", "A: u32 = 5")).expect("answered");
        }
        let read = call("read_file", json!({"path": "src/a.rs"}));
        assert_eq!(one.answer(&read), two.answer(&read));
        assert_eq!(one.finish(""), two.finish(""));
    }
}
```

And in `src/core/bench/grade.rs`'s test module (find it after `known_check`'s tests), add:

```rust
    #[test]
    fn tool_use_blocks_keep_the_ids_a_tool_result_must_echo() {
        let body = serde_json::json!({
            "content": [
                {"type": "text", "text": "reading"},
                {"type": "tool_use", "id": "toolu_1", "name": "read_file", "input": {"path": "a"}},
                {"type": "tool_use", "id": "toolu_2", "name": "grep", "input": {"pattern": "x", "path": "."}}
            ]
        })
        .to_string();
        let uses = super::tool_use_blocks(&body).expect("readable");
        assert_eq!(uses.len(), 2);
        assert_eq!(uses[0].id, "toolu_1");
        assert_eq!(uses[1].name, "grep");
        assert_eq!(uses[1].input["pattern"], "x");
        assert!(super::tool_use_blocks("not json").is_err());
    }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --manifest-path /Users/amoscoletti/personal_dev/chekov/Cargo.toml toolloop 2>&1 | grep -E "^error" | head -3`
Expected: `unresolved import` / `file not found for module` — `mod.rs` does not declare the module yet.

- [ ] **Step 3: Commit the red**

Add `pub mod toolloop;` after `pub mod store;` in `src/core/bench/mod.rs` so the red is a missing-symbol red, not a missing-file red; then:

```bash
cargo fmt --manifest-path /Users/amoscoletti/personal_dev/chekov/Cargo.toml
git -C /Users/amoscoletti/personal_dev/chekov add src/core/bench/toolloop.rs src/core/bench/mod.rs src/core/bench/grade.rs
git -C /Users/amoscoletti/personal_dev/chekov commit -F - <<'EOF'
test(bench): red — the canned loop environment answers five tools as a pure function of the case and the calls so far

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01USTtBCMA5rsVnigD6fhoc3
EOF
```

- [ ] **Step 4: `ToolUse` in `grade.rs`**

Replace `tool_uses` (lines 68-94) with:

```rust
/// One `tool_use` block as the agent would act on it — the id is what a
/// `tool_result` must echo back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolUse {
    pub id: String,
    pub name: String,
    pub input: Value,
}

/// The reply's content blocks — or the translation-failure refusal.
pub(crate) fn content_blocks(anthropic_body: &str) -> Result<Vec<Value>, Grade> {
    let Ok(parsed) = serde_json::from_str::<Value>(anthropic_body) else {
        return Err(Grade::Fail {
            reason: "artifact is not JSON".to_owned(),
        });
    };
    let Some(blocks) = parsed.get("content").and_then(Value::as_array) else {
        return Err(Grade::Fail {
            reason: "no content in the artifact — a translation failure, not an empty reply"
                .to_owned(),
        });
    };
    Ok(blocks.clone())
}

/// The reply's `tool_use` blocks with their ids, in order.
pub(crate) fn tool_use_blocks(anthropic_body: &str) -> Result<Vec<ToolUse>, Grade> {
    let text = |b: &Value, key: &str| b.get(key).and_then(Value::as_str).unwrap_or_default().to_owned();
    Ok(content_blocks(anthropic_body)?
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"))
        .map(|b| ToolUse {
            id: text(b, "id"),
            name: text(b, "name"),
            input: b.get("input").cloned().unwrap_or(Value::Null),
        })
        .collect())
}

/// The reply's `tool_use` blocks as (name, input) pairs.
fn tool_uses(anthropic_body: &str) -> Result<Vec<(String, Value)>, Grade> {
    Ok(tool_use_blocks(anthropic_body)?
        .into_iter()
        .map(|u| (u.name, u.input))
        .collect())
}
```

Also make `artifact_text` `pub(crate)` and re-express its body over `content_blocks` (drop the duplicated JSON/`content` checks):

```rust
/// The reply's text blocks, joined — or the translation-failure refusal.
pub(crate) fn artifact_text(anthropic_body: &str) -> Result<String, Grade> {
    Ok(content_blocks(anthropic_body)?
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|b| b.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n"))
}
```

- [ ] **Step 5: The environment**

Above the test module in `src/core/bench/toolloop.rs`:

```rust
use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::core::bench::grade::ToolUse;
use crate::core::bench::probeset::{Goal, LoopCase, ToolDef, canned_text};
use crate::core::bench::store::LoopEnd;

/// The canned repository for one case, and the goal it is judged against.
///
/// No clock, no randomness, no filesystem: two environments fed the same
/// calls hold the same state, which is what makes the probe deterministic
/// for a deterministic model.
pub struct ToolEnv<'a> {
    case: &'a LoopCase,
    files: BTreeMap<String, String>,
}

impl<'a> ToolEnv<'a> {
    #[must_use]
    pub fn new(case: &'a LoopCase) -> Self {
        let files = case
            .files
            .iter()
            .map(|f| (f.path.clone(), f.text.clone()))
            .collect();
        Self { case, files }
    }

    /// Answer one call, or end the loop: a tool outside the palette, or a
    /// call missing a key the tool's schema requires, is not answered.
    pub fn answer(&mut self, call: &ToolUse) -> Result<String, LoopEnd> {
        let tool = self
            .case
            .tools
            .iter()
            .find(|t| t.name == call.name)
            .ok_or_else(|| LoopEnd::FabricatedTool {
                name: call.name.clone(),
            })?;
        if let Some(key) = missing_key(tool, &call.input) {
            return Err(LoopEnd::MalformedCall {
                name: call.name.clone(),
                key,
            });
        }
        Ok(match call.name.as_str() {
            "read_file" => self.read_file(arg(&call.input, "path")),
            "list_dir" => self.list_dir(arg(&call.input, "path")),
            "grep" => self.grep(arg(&call.input, "pattern"), arg(&call.input, "path")),
            "edit_file" => self.edit_file(&call.input),
            "run_tests" => self.run_tests(),
            other => format!("tool '{other}' is offered but has no canned behaviour"),
        })
    }

    fn read_file(&self, path: &str) -> String {
        self.files
            .get(path)
            .cloned()
            .unwrap_or_else(|| format!("no such file: {path}"))
    }

    /// Direct children only; a subdirectory is named with a trailing slash.
    fn list_dir(&self, path: &str) -> String {
        let prefix = dir_prefix(path);
        let entries: BTreeSet<String> = self
            .files
            .keys()
            .filter_map(|p| p.strip_prefix(&prefix))
            .map(|rest| match rest.split_once('/') {
                Some((dir, _)) => format!("{dir}/"),
                None => rest.to_owned(),
            })
            .collect();
        if entries.is_empty() {
            return format!("no such directory: {path}");
        }
        entries.into_iter().collect::<Vec<_>>().join("\n")
    }

    /// Plain substring, never a regex: no prompt asks for one, and a model
    /// that sends `.` means a dot.
    fn grep(&self, pattern: &str, path: &str) -> String {
        let prefix = dir_prefix(path);
        let hits: Vec<String> = self
            .files
            .iter()
            .filter(|(p, _)| p.starts_with(&prefix) || p.as_str() == path)
            .flat_map(|(p, text)| {
                text.lines()
                    .enumerate()
                    .filter(|(_, line)| line.contains(pattern))
                    .map(move |(i, line)| format!("{p}:{}: {line}", i + 1))
            })
            .collect();
        if hits.is_empty() {
            "no matches".to_owned()
        } else {
            hits.join("\n")
        }
    }

    /// Claude Code's own `Edit` contract: exactly one occurrence, or say why.
    fn edit_file(&mut self, input: &Value) -> String {
        let (path, old, new) = (arg(input, "path"), arg(input, "old"), arg(input, "new"));
        let Some(text) = self.files.get(path) else {
            return format!("no such file: {path}");
        };
        match text.matches(old).count() {
            0 => format!("old text not found in {path}"),
            1 => {
                let edited = text.replacen(old, new, 1);
                self.files.insert(path.to_owned(), edited);
                format!("edited {path}")
            }
            n => format!("old text occurs {n} times in {path}; make it unique"),
        }
    }

    /// The canned failure until the goal is met — the same words every time,
    /// never the answer.
    fn run_tests(&self) -> String {
        match &self.case.goal {
            Goal::Edited {
                tests_fail: Some(fail),
                ..
            } if !self.goal_met() => fail.clone(),
            Goal::Edited { .. } | Goal::Unchanged { .. } => "ok. 1 passed".to_owned(),
        }
    }

    fn goal_met(&self) -> bool {
        match &self.case.goal {
            Goal::Edited {
                file,
                contains_any,
                untouched,
                ..
            } => {
                self.files
                    .get(file)
                    .is_some_and(|t| contains_any.iter().any(|w| t.contains(w.as_str())))
                    && untouched.iter().all(|p| self.unchanged(p))
            }
            Goal::Unchanged { .. } => self.case.files.iter().all(|f| self.unchanged(&f.path)),
        }
    }

    fn unchanged(&self, path: &str) -> bool {
        self.files.get(path).map(String::as_str) == canned_text(self.case, path)
    }

    /// The end state when the model stops talking: met, or what was wanted.
    #[must_use]
    pub fn finish(&self, final_text: &str) -> LoopEnd {
        let mentioned = match &self.case.goal {
            Goal::Unchanged { reply_mentions } => final_text
                .to_lowercase()
                .contains(&reply_mentions.to_lowercase()),
            Goal::Edited { .. } => true,
        };
        if self.goal_met() && mentioned {
            LoopEnd::GoalMet
        } else {
            LoopEnd::GoalUnmet {
                wanted: self.wanted(),
            }
        }
    }

    fn wanted(&self) -> String {
        match &self.case.goal {
            Goal::Edited {
                file, contains_any, ..
            } => format!(
                "{file} containing {}",
                contains_any
                    .iter()
                    .map(|w| format!("{w:?}"))
                    .collect::<Vec<_>>()
                    .join(" or ")
            ),
            Goal::Unchanged { reply_mentions } => {
                format!("no file changed and a reply naming {reply_mentions}")
            }
        }
    }
}

fn arg<'v>(input: &'v Value, key: &str) -> &'v str {
    input.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// The first `required` key of the tool's schema the call did not supply as
/// a string, if any — read off the schema, never a hard-coded list.
fn missing_key(tool: &ToolDef, input: &Value) -> Option<String> {
    let schema: Value = serde_json::from_str(&tool.input_schema).unwrap_or(Value::Null);
    schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .find(|key| input.get(key).and_then(Value::as_str).is_none())
        .map(str::to_owned)
}

/// `src` → `src/`; the root (`""`, `.`) → `""`.
fn dir_prefix(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() || trimmed == "." {
        String::new()
    } else {
        format!("{trimmed}/")
    }
}
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test --manifest-path /Users/amoscoletti/personal_dev/chekov/Cargo.toml toolloop 2>&1 | grep -E "^test |test result"` and `cargo test --manifest-path /Users/amoscoletti/personal_dev/chekov/Cargo.toml tool_use_blocks 2>&1 | tail -2`
Expected: all nine environment tests and the grade test pass.

- [ ] **Step 7: Gate and commit**

Run: `make -C /Users/amoscoletti/personal_dev/chekov lint && make -C /Users/amoscoletti/personal_dev/chekov test 2>&1 | grep -E "^test result" | head -1`
Expected: clean and green. (Clippy may flag `edit_file`'s borrow of `text` across the insert; if so, compute `let count = text.matches(old).count();` and `let edited = text.replacen(old, new, 1);` before the `match`.)

```bash
git -C /Users/amoscoletti/personal_dev/chekov add src/core/bench/toolloop.rs src/core/bench/grade.rs src/core/bench/mod.rs
git -C /Users/amoscoletti/personal_dev/chekov commit -F - <<'EOF'
feat(bench): the canned loop environment — read, list, grep, an Edit-shaped edit_file, and tests that fail with one fixed line until the goal is met

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01USTtBCMA5rsVnigD6fhoc3
EOF
```

---

### Task 5: The turn request and the pinned hash — `probes::loop_probe`, `HashPins`

**Files:**
- Modify: `src/core/bench/probes.rs:39-84` (`suite_prompt_hash`, `tool_probe`), plus `loop_probe`
- Modify: every caller of `suite_prompt_hash` (`src/commands/capability.rs:2330` `head_corpus`, and the test at `:3515`)
- Test: inline in `src/core/bench/probes.rs` and the existing capability test

**Interfaces:**
- Consumes: `probeset::LoopCase` (Task 2).
- Produces: `probes::HashPins { seed: u32, max_turns: u32 }`, `probes::suite_prompt_hash(suite, plan, pins: HashPins)`, `probes::loop_probe(case: &LoopCase, system: &str, messages: &[Value]) -> HttpRequest`. Tasks 6 and 10 consume these.

- [ ] **Step 1: Write the failing tests**

In `src/core/bench/probes.rs`'s test module:

```rust
    #[test]
    fn a_loop_turn_carries_the_system_text_the_palette_and_the_transcript_in_order() {
        let set = crate::core::bench::probeset::agentic_v0().expect("valid");
        let case = &set.tool_loop[0];
        let messages = vec![
            serde_json::json!({"role": "user", "content": "do it"}),
            serde_json::json!({"role": "assistant", "content": [{"type": "text", "text": "ok"}]}),
        ];
        let req = super::loop_probe(case, &set.loop_system, &messages);
        let body: serde_json::Value = serde_json::from_slice(&req.body).expect("json");
        assert_eq!(body["system"], set.loop_system);
        assert_eq!(body["tools"].as_array().map(Vec::len), Some(case.tools.len()));
        assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
        assert_eq!(body["messages"][1]["role"], "assistant");
        assert_eq!(body["max_tokens"], 512);
    }

    #[test]
    fn a_different_turn_budget_changes_the_agentic_hash_and_not_the_throughput_one() {
        use crate::core::bench::lifecycle::Suite;
        use crate::core::bench::sweep::SweepPlan;
        let plan = SweepPlan {
            depths: vec![1024],
            repetitions: 5,
            max_tokens: 128,
        };
        let eight = super::HashPins {
            seed: 42,
            max_turns: 8,
        };
        let three = super::HashPins {
            seed: 42,
            max_turns: 3,
        };
        assert_ne!(
            super::suite_prompt_hash(Suite::Agentic, &plan, eight),
            super::suite_prompt_hash(Suite::Agentic, &plan, three)
        );
        assert_eq!(
            super::suite_prompt_hash(Suite::Throughput, &plan, eight),
            super::suite_prompt_hash(Suite::Throughput, &plan, three),
            "a throughput-only run's hash never saw the budget"
        );
    }
```

Rewrite `the_suite_hash_keeps_throughput_stable_and_separates_the_rest` in `src/commands/capability.rs` to pass `probes::HashPins { seed: 42, max_turns: 8 }` wherever it passed `42` to `suite_prompt_hash` (the `prompt_set_hash(&plan, 42)` calls stay).

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --manifest-path /Users/amoscoletti/personal_dev/chekov/Cargo.toml probes 2>&1 | grep -E "^error" | head -3`
Expected: `cannot find function loop_probe`, `cannot find struct HashPins`.

- [ ] **Step 3: Commit the red**

```bash
cargo fmt --manifest-path /Users/amoscoletti/personal_dev/chekov/Cargo.toml
git -C /Users/amoscoletti/personal_dev/chekov add src/core/bench/probes.rs src/commands/capability.rs
git -C /Users/amoscoletti/personal_dev/chekov commit -F - <<'EOF'
test(bench): red — a loop turn is the system text, the palette and the transcript; the turn budget pins the agentic hash

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01USTtBCMA5rsVnigD6fhoc3
EOF
```

- [ ] **Step 4: Implement**

In `probes.rs`, replace `suite_prompt_hash` with:

```rust
/// What pins the agentic task set beyond its content: the sampling seed and
/// the loop's turn budget (tool-loop design §8). Two runs judged under
/// different budgets measured different tasks.
#[derive(Debug, Clone, Copy)]
pub struct HashPins {
    pub seed: u32,
    pub max_turns: u32,
}

/// The suite-aware prompt-set hash. A throughput-only run keeps the original
/// value, so runs recorded before `--suite` existed stay comparable.
#[must_use]
pub fn suite_prompt_hash(
    suite: crate::core::bench::lifecycle::Suite,
    plan: &crate::core::bench::sweep::SweepPlan,
    pins: HashPins,
) -> String {
    use crate::core::bench::lifecycle::Suite;
    let throughput = prompt_set_hash(plan, pins.seed);
    let agentic = crate::core::bench::probeset::content_hash();
    let (seed, turns) = (pins.seed, pins.max_turns);
    match suite {
        Suite::Throughput => throughput,
        Suite::Agentic => hash12(&format!("agentic|{agentic}|turns={turns}|seed={seed}")),
        Suite::All => hash12(&format!("all|{throughput}|{agentic}|turns={turns}|seed={seed}")),
    }
}

fn hash12(canonical: &str) -> String {
    crate::core::hash::sha256_hex(canonical.as_bytes())[..12].to_owned()
}
```

Replace `tool_probe`'s inline `tools` mapping with a shared helper and add `loop_probe`:

```rust
/// The palette as Anthropic `tools`, shared by every probe that offers one.
fn palette(tools: &[crate::core::bench::probeset::ToolDef]) -> Vec<serde_json::Value> {
    tools
        .iter()
        .map(|tool| {
            serde_json::json!({
                "name": tool.name,
                "description": tool.description,
                "input_schema": parse_schema(&tool.input_schema),
            })
        })
        .collect()
}

/// A `tool_emit` case: the palette rides as real Anthropic `tools`, so the
/// call crosses the translator's tool mapping exactly as an agent's would.
#[must_use]
pub fn tool_probe(case: &crate::core::bench::probeset::ToolCase) -> HttpRequest {
    anthropic_post(&serde_json::json!({
        "model": "claude-sonnet-4",
        "max_tokens": 256,
        "tools": palette(&case.tools),
        "messages": [{"role": "user", "content": case.prompt}],
    }))
}

/// One turn of a `tool_loop` case: the set's system text, the palette, and
/// the transcript so far — the shape Claude Code sends on every turn.
#[must_use]
pub fn loop_probe(
    case: &crate::core::bench::probeset::LoopCase,
    system: &str,
    messages: &[serde_json::Value],
) -> HttpRequest {
    anthropic_post(&serde_json::json!({
        "model": "claude-sonnet-4",
        "max_tokens": 512,
        "system": system,
        "tools": palette(&case.tools),
        "messages": messages,
    }))
}
```

In `src/commands/capability.rs` `head_corpus`, replace the closure body:

```rust
        |suite| {
            probes::suite_prompt_hash(
                suite,
                inputs.plan,
                probes::HashPins {
                    seed: bench_cfg.seed,
                    max_turns: bench_cfg.tool_loop_max_turns,
                },
            )
        },
```

Run `grep -rn "suite_prompt_hash(" /Users/amoscoletti/personal_dev/chekov/src/` and convert any remaining caller the same way.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --manifest-path /Users/amoscoletti/personal_dev/chekov/Cargo.toml -- probes suite_hash 2>&1 | grep -E "^test |test result"`
Expected: all pass.

- [ ] **Step 6: Gate and commit**

Run: `make -C /Users/amoscoletti/personal_dev/chekov lint && make -C /Users/amoscoletti/personal_dev/chekov test 2>&1 | grep -E "^test result" | head -1`

```bash
git -C /Users/amoscoletti/personal_dev/chekov add src/core/bench/probes.rs src/commands/capability.rs
git -C /Users/amoscoletti/personal_dev/chekov commit -F - <<'EOF'
feat(bench): loop_probe and HashPins — every turn ships the palette and the transcript; the turn budget is part of the agentic prompt-set hash

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01USTtBCMA5rsVnigD6fhoc3
EOF
```

---

### Task 6: The driver — `toolloop::drive`

**Files:**
- Modify: `src/core/bench/toolloop.rs` (driver above the environment's tests; tests appended to the existing module)

**Interfaces:**
- Consumes: `ToolEnv` (Task 4), `probes::loop_probe` (Task 5), `grade::{content_blocks, artifact_text, tool_use_blocks}` (Task 4), `runner::Timings`, `codebase::run::empty_measure`, `store::Measure`.
- Produces: `toolloop::Turn { body: String, timings: Option<Timings> }`, `toolloop::Door<'a> = dyn FnMut(&HttpRequest) -> Result<Turn, ChekovError> + 'a`, `toolloop::LoopRun<'a> { case: &'a LoopCase, system: &'a str, max_turns: u32 }`, `toolloop::LoopOutcome { end: LoopEnd, turns: u32, tool_calls: u32, measure: Measure }`, `toolloop::drive(door: &mut Door, run: &LoopRun) -> Result<LoopOutcome, ChekovError>`. Tasks 7 and 10 consume these.

- [ ] **Step 1: Write the failing tests**

Append to the test module in `toolloop.rs` (add `use std::cell::RefCell; use std::collections::VecDeque;`, `use super::{LoopRun, Turn, drive};`, `use crate::core::bench::runner::Timings;`, `use crate::core::proxy::http::HttpRequest;` to its imports):

```rust
    fn reply(content: Vec<Value>, stop: &str) -> String {
        json!({
            "id": "msg_1", "type": "message", "role": "assistant", "model": "m",
            "content": content, "stop_reason": stop, "stop_sequence": null,
            "usage": {"input_tokens": 1, "output_tokens": 1}
        })
        .to_string()
    }

    fn use_block(id: &str, name: &str, input: Value) -> Value {
        json!({"type": "tool_use", "id": id, "name": name, "input": input})
    }

    fn text_block(text: &str) -> Value {
        json!({"type": "text", "text": text})
    }

    fn timings(prompt_n: u64) -> Timings {
        Timings {
            prompt_n,
            prompt_per_second: 100.0,
            predicted_n: 20,
            predicted_per_second: 10.0,
            cache_n: prompt_n / 2,
            draft_n: 4,
            draft_n_accepted: 3,
        }
    }

    /// A door that answers from a script and records what it was sent.
    struct Scripted {
        bodies: RefCell<VecDeque<String>>,
        sent: RefCell<Vec<Value>>,
    }

    impl Scripted {
        fn new(bodies: Vec<String>) -> Self {
            Self {
                bodies: RefCell::new(bodies.into()),
                sent: RefCell::new(Vec::new()),
            }
        }

        fn door(&self) -> impl FnMut(&HttpRequest) -> Result<Turn, crate::error::ChekovError> + '_ {
            move |req: &HttpRequest| {
                self.sent
                    .borrow_mut()
                    .push(serde_json::from_slice(&req.body).expect("json"));
                let body = self.bodies.borrow_mut().pop_front().expect("scripted");
                let turn = self.sent.borrow().len() as u64;
                Ok(Turn {
                    body,
                    timings: Some(timings(100 * turn)),
                })
            }
        }
    }

    fn run<'a>(set: &'a ProbeSet, max_turns: u32) -> LoopRun<'a> {
        LoopRun {
            case: case(set),
            system: &set.loop_system,
            max_turns,
        }
    }

    #[test]
    fn read_then_edit_then_stop_reaches_the_goal_and_the_transcript_echoes_the_ids() {
        let set = edited_set();
        let script = Scripted::new(vec![
            reply(vec![text_block("looking"), use_block("t1", "read_file", json!({"path": "src/a.rs"}))], "tool_use"),
            reply(vec![use_block("t2", "edit_file", json!({"path": "src/a.rs", "old": "A: u32 = 3", "new": "A: u32 = 5"}))], "tool_use"),
            reply(vec![text_block("done")], "end_turn"),
        ]);
        let outcome = drive(&mut script.door(), &run(&set, 8)).expect("drove");
        assert_eq!(outcome.end, LoopEnd::GoalMet);
        assert_eq!((outcome.turns, outcome.tool_calls), (3, 2));
        assert_eq!(outcome.measure.decode_samples.len(), 3, "one sample per timed turn");
        assert_eq!(outcome.measure.prompt_n, 300, "the deepest turn's prompt");
        assert_eq!(outcome.measure.cache_n, 150, "the max seen");
        assert_eq!(outcome.measure.draft_n_accepted, 9, "drafts summed");
        let sent = script.sent.borrow();
        let third = &sent[2]["messages"];
        assert_eq!(third[1]["role"], "assistant");
        assert_eq!(third[1]["content"][1]["id"], "t1", "the reply's blocks ride back verbatim");
        assert_eq!(third[2]["content"][0]["type"], "tool_result");
        assert_eq!(third[2]["content"][0]["tool_use_id"], "t1");
        assert_eq!(third[2]["content"][0]["content"], "const A: u32 = 3;\nconst B: u32 = 3;\n");
        assert_eq!(third[4]["content"][0]["tool_use_id"], "t2");
    }

    #[test]
    fn a_fabricated_tool_ends_the_loop_at_that_turn() {
        let set = edited_set();
        let script = Scripted::new(vec![reply(
            vec![use_block("t1", "delete_file", json!({"path": "src/a.rs"}))],
            "tool_use",
        )]);
        let outcome = drive(&mut script.door(), &run(&set, 8)).expect("drove");
        assert_eq!(outcome.end, LoopEnd::FabricatedTool { name: "delete_file".into() });
        assert_eq!((outcome.turns, outcome.tool_calls), (1, 1));
    }

    #[test]
    fn stopping_short_of_the_goal_is_unmet_and_a_cut_reply_is_truncated() {
        let set = edited_set();
        let script = Scripted::new(vec![reply(vec![text_block("all good")], "end_turn")]);
        let outcome = drive(&mut script.door(), &run(&set, 8)).expect("drove");
        assert!(matches!(outcome.end, LoopEnd::GoalUnmet { ref wanted } if wanted.starts_with("src/a.rs containing")));
        let script = Scripted::new(vec![reply(vec![text_block("I will now")], "max_tokens")]);
        let outcome = drive(&mut script.door(), &run(&set, 8)).expect("drove");
        assert_eq!(outcome.end, LoopEnd::Truncated);
    }

    #[test]
    fn a_loop_still_calling_at_the_budget_is_exhausted_at_exactly_k_turns() {
        let set = edited_set();
        let read = || reply(vec![use_block("t", "read_file", json!({"path": "src/a.rs"}))], "tool_use");
        let script = Scripted::new(vec![read(), read(), read(), read()]);
        let outcome = drive(&mut script.door(), &run(&set, 3)).expect("drove");
        assert_eq!(outcome.end, LoopEnd::TurnsExhausted);
        assert_eq!((outcome.turns, outcome.tool_calls), (3, 3));
        assert_eq!(script.sent.borrow().len(), 3, "the fourth reply was never asked for");
    }

    #[test]
    fn an_unreadable_reply_fails_the_crossing_rather_than_grading() {
        let set = edited_set();
        let script = Scripted::new(vec!["not json".to_owned()]);
        let err = drive(&mut script.door(), &run(&set, 8)).expect_err("chekov's fault");
        assert!(err.to_string().contains("loop reply unreadable"), "{err}");
    }

    #[test]
    fn an_unchanged_goal_passes_when_the_model_reports_the_missing_file() {
        let set = unchanged_set();
        let script = Scripted::new(vec![
            reply(vec![use_block("t1", "read_file", json!({"path": "src/legacy.rs"}))], "tool_use"),
            reply(vec![text_block("src/legacy.rs does not exist; nothing to fix.")], "end_turn"),
        ]);
        let outcome = drive(&mut script.door(), &run(&set, 8)).expect("drove");
        assert_eq!(outcome.end, LoopEnd::GoalMet);
    }

    #[test]
    fn an_untimed_door_leaves_the_measure_empty() {
        let set = unchanged_set();
        let mut door = |_: &HttpRequest| {
            Ok(Turn {
                body: reply(vec![text_block("no src/legacy.rs")], "end_turn"),
                timings: None,
            })
        };
        let outcome = drive(&mut door, &run(&set, 8)).expect("drove");
        assert!(outcome.measure.decode_samples.is_empty());
        assert_eq!(outcome.measure.prompt_n, 0);
    }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --manifest-path /Users/amoscoletti/personal_dev/chekov/Cargo.toml toolloop 2>&1 | grep -E "^error" | head -3`
Expected: `cannot find function drive` and friends.

- [ ] **Step 3: Commit the red**

```bash
cargo fmt --manifest-path /Users/amoscoletti/personal_dev/chekov/Cargo.toml
git -C /Users/amoscoletti/personal_dev/chekov add src/core/bench/toolloop.rs
git -C /Users/amoscoletti/personal_dev/chekov commit -F - <<'EOF'
test(bench): red — the loop driver reaches a terminal state, echoes tool_use ids on the transcript, and stops at exactly K turns

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01USTtBCMA5rsVnigD6fhoc3
EOF
```

- [ ] **Step 4: Implement the driver**

In `toolloop.rs`, extend the imports:

```rust
use serde_json::{Value, json};

use crate::core::bench::codebase::run::empty_measure;
use crate::core::bench::grade::{self, Grade, ToolUse};
use crate::core::bench::probeset::{Goal, LoopCase, ToolDef, canned_text};
use crate::core::bench::runner::Timings;
use crate::core::bench::store::{LoopEnd, Measure};
use crate::core::bench::{probes, probeset};
use crate::core::proxy::http::HttpRequest;
use crate::error::ChekovError;
```

(Drop `probeset` from the `use` if nothing references it by path.) Then, after the `ToolEnv` impl block and its free functions, add:

```rust
/// One door's answer to one turn.
pub struct Turn {
    pub body: String,
    pub timings: Option<Timings>,
}

/// The door a loop crosses: the bench's own `agentic_cross`, or a scripted
/// fake in tests. The driver never learns which transport it rode.
pub type Door<'a> = dyn FnMut(&HttpRequest) -> Result<Turn, ChekovError> + 'a;

/// One case's run: the case, the set's system text, and the turn budget.
pub struct LoopRun<'a> {
    pub case: &'a LoopCase,
    pub system: &'a str,
    pub max_turns: u32,
}

/// How a loop ended and what it cost.
#[derive(Debug, Clone, PartialEq)]
pub struct LoopOutcome {
    pub end: LoopEnd,
    pub turns: u32,
    pub tool_calls: u32,
    pub measure: Measure,
}

/// Drive one case to a terminal state (tool-loop design §5): a reply with no
/// tool call ends the turn and is judged; a reply with calls is answered and
/// the loop goes on; the budget ends it as exhausted.
pub fn drive(door: &mut Door, run: &LoopRun) -> Result<LoopOutcome, ChekovError> {
    let mut state = LoopState::new(run.case);
    for turn in 1..=run.max_turns {
        let req = probes::loop_probe(run.case, run.system, &state.messages);
        let reply = door(&req)?;
        state.fold(reply.timings.as_ref());
        let parsed = Reply::parse(&reply.body)?;
        if let Some(end) = state.step(&parsed) {
            return Ok(state.outcome(end, turn));
        }
    }
    Ok(state.outcome(LoopEnd::TurnsExhausted, run.max_turns))
}

struct LoopState<'a> {
    env: ToolEnv<'a>,
    messages: Vec<Value>,
    tool_calls: u32,
    measure: Measure,
}

impl<'a> LoopState<'a> {
    fn new(case: &'a LoopCase) -> Self {
        Self {
            env: ToolEnv::new(case),
            messages: vec![json!({"role": "user", "content": case.prompt})],
            tool_calls: 0,
            measure: empty_measure(),
        }
    }

    /// One reply applied. `None` means the loop goes on.
    fn step(&mut self, reply: &Reply) -> Option<LoopEnd> {
        if reply.tool_uses.is_empty() {
            return Some(if reply.stop_reason.as_deref() == Some("max_tokens") {
                LoopEnd::Truncated
            } else {
                self.env.finish(&reply.text)
            });
        }
        self.messages
            .push(json!({"role": "assistant", "content": reply.content}));
        let mut results = Vec::new();
        for call in &reply.tool_uses {
            self.tool_calls += 1;
            match self.env.answer(call) {
                Ok(text) => results.push(
                    json!({"type": "tool_result", "tool_use_id": call.id, "content": text}),
                ),
                Err(end) => return Some(end),
            }
        }
        self.messages.push(json!({"role": "user", "content": results}));
        None
    }

    /// One sample per timed turn; the deepest prompt; the largest cache hit;
    /// drafts summed. An untimed door adds nothing.
    fn fold(&mut self, timings: Option<&Timings>) {
        let Some(t) = timings else { return };
        self.measure.decode_samples.push(t.predicted_per_second);
        self.measure.prefill_samples.push(t.prompt_per_second);
        self.measure.prompt_n = t.prompt_n;
        self.measure.cache_n = self.measure.cache_n.max(t.cache_n);
        self.measure.draft_n += t.draft_n;
        self.measure.draft_n_accepted += t.draft_n_accepted;
    }

    fn outcome(self, end: LoopEnd, turns: u32) -> LoopOutcome {
        LoopOutcome {
            end,
            turns,
            tool_calls: self.tool_calls,
            measure: self.measure,
        }
    }
}

/// A reply as the loop reads it.
struct Reply {
    content: Vec<Value>,
    text: String,
    stop_reason: Option<String>,
    tool_uses: Vec<ToolUse>,
}

impl Reply {
    /// A body the translator could not produce is chekov's fault, never the
    /// model's: it fails the crossing rather than grading.
    fn parse(body: &str) -> Result<Self, ChekovError> {
        let unreadable = |g: Grade| ChekovError::ProxyBadRequest {
            reason: format!("loop reply unreadable: {}", reason_of(&g)),
        };
        let content = grade::content_blocks(body).map_err(unreadable)?;
        let text = grade::artifact_text(body).map_err(unreadable)?;
        let tool_uses = grade::tool_use_blocks(body).map_err(unreadable)?;
        let stop_reason = serde_json::from_str::<Value>(body)
            .ok()
            .and_then(|v| v.get("stop_reason")?.as_str().map(str::to_owned));
        Ok(Self {
            content,
            text,
            stop_reason,
            tool_uses,
        })
    }
}

fn reason_of(grade: &Grade) -> String {
    match grade {
        Grade::Pass => String::new(),
        Grade::Fail { reason } => reason.clone(),
    }
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --manifest-path /Users/amoscoletti/personal_dev/chekov/Cargo.toml toolloop 2>&1 | grep -E "^test |test result"`
Expected: all sixteen `toolloop::tests::*` pass.

- [ ] **Step 6: Gate and commit**

Run: `make -C /Users/amoscoletti/personal_dev/chekov lint && make -C /Users/amoscoletti/personal_dev/chekov test 2>&1 | grep -E "^test result" | head -1`

```bash
git -C /Users/amoscoletti/personal_dev/chekov add src/core/bench/toolloop.rs
git -C /Users/amoscoletti/personal_dev/chekov commit -F - <<'EOF'
feat(bench): the loop driver — one door in, a terminal state out; the transcript carries the reply's blocks verbatim and one tool_result per call

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01USTtBCMA5rsVnigD6fhoc3
EOF
```

---

### Task 7: The grade — `grade::grade_tool_loop`

**Files:**
- Modify: `src/core/bench/grade.rs` (after `grade_instruction`)
- Test: inline in `src/core/bench/grade.rs`

**Interfaces:**
- Consumes: `toolloop::LoopOutcome` (Task 6), `store::LoopEnd` (Task 3).
- Produces: `grade::grade_tool_loop(&LoopOutcome) -> Grade`. Task 10 consumes it.

- [ ] **Step 1: Write the failing test**

```rust
    #[test]
    fn the_loop_grade_is_the_end_state_with_the_turns_in_the_reason_only() {
        use crate::core::bench::codebase::run::empty_measure;
        use crate::core::bench::store::LoopEnd;
        use crate::core::bench::toolloop::LoopOutcome;
        let outcome = |end: LoopEnd| LoopOutcome {
            end,
            turns: 4,
            tool_calls: 6,
            measure: empty_measure(),
        };
        assert_eq!(super::grade_tool_loop(&outcome(LoopEnd::GoalMet)), super::Grade::Pass);
        let reason = |end: LoopEnd| match super::grade_tool_loop(&outcome(end)) {
            super::Grade::Pass => panic!("a failure"),
            super::Grade::Fail { reason } => reason,
        };
        assert_eq!(
            reason(LoopEnd::GoalUnmet { wanted: "src/a.rs containing \"x\"".into() }),
            "stopped with the goal unmet after 4 turns: src/a.rs containing \"x\""
        );
        assert_eq!(reason(LoopEnd::TurnsExhausted), "no terminal state in 4 turns (6 tool calls)");
        assert_eq!(reason(LoopEnd::Truncated), "final reply hit max_tokens");
        assert_eq!(
            reason(LoopEnd::FabricatedTool { name: "rm".into() }),
            "called 'rm' — not in this case's palette"
        );
        assert_eq!(
            reason(LoopEnd::MalformedCall { name: "edit_file".into(), key: "new".into() }),
            "'edit_file' called without new"
        );
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --manifest-path /Users/amoscoletti/personal_dev/chekov/Cargo.toml the_loop_grade 2>&1 | grep -E "^error" | head -2`
Expected: `cannot find function grade_tool_loop`.

- [ ] **Step 3: Commit the red**

```bash
cargo fmt --manifest-path /Users/amoscoletti/personal_dev/chekov/Cargo.toml
git -C /Users/amoscoletti/personal_dev/chekov add src/core/bench/grade.rs
git -C /Users/amoscoletti/personal_dev/chekov commit -F - <<'EOF'
test(bench): red — a loop's grade is its end state; the turn count rides in the reason and never in the verdict

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01USTtBCMA5rsVnigD6fhoc3
EOF
```

- [ ] **Step 4: Implement**

After `grade_instruction` in `grade.rs`:

```rust
/// The loop's end IS the grade (tool-loop design §6): reached, or why not.
/// Turn and call counts go in the reason; a model that needed six turns and
/// one that needed two both pass.
#[must_use]
pub fn grade_tool_loop(outcome: &crate::core::bench::toolloop::LoopOutcome) -> Grade {
    use crate::core::bench::store::LoopEnd;
    let turns = outcome.turns;
    let fail = |reason: String| Grade::Fail { reason };
    match &outcome.end {
        LoopEnd::GoalMet => Grade::Pass,
        LoopEnd::GoalUnmet { wanted } => {
            fail(format!("stopped with the goal unmet after {turns} turns: {wanted}"))
        }
        LoopEnd::TurnsExhausted => fail(format!(
            "no terminal state in {turns} turns ({} tool calls)",
            outcome.tool_calls
        )),
        LoopEnd::Truncated => fail("final reply hit max_tokens".to_owned()),
        LoopEnd::FabricatedTool { name } => {
            fail(format!("called '{name}' — not in this case's palette"))
        }
        LoopEnd::MalformedCall { name, key } => fail(format!("'{name}' called without {key}")),
    }
}
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test --manifest-path /Users/amoscoletti/personal_dev/chekov/Cargo.toml the_loop_grade 2>&1 | tail -2`

- [ ] **Step 6: Gate and commit**

```bash
make -C /Users/amoscoletti/personal_dev/chekov lint && make -C /Users/amoscoletti/personal_dev/chekov test 2>&1 | grep -E "^test result" | head -1
git -C /Users/amoscoletti/personal_dev/chekov add src/core/bench/grade.rs
git -C /Users/amoscoletti/personal_dev/chekov commit -F - <<'EOF'
feat(bench): grade_tool_loop — six end states, one pass, five named failures

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01USTtBCMA5rsVnigD6fhoc3
EOF
```

---

### Task 8: The report line — `store::tool_loop_line`

**Files:**
- Modify: `src/core/bench/store.rs:557-574` (`suite_summaries`) and beside `tool_emit_line` (~1562)
- Test: inline in `src/core/bench/store.rs`

**Interfaces:**
- Consumes: `LoopRow`, `AGENTIC`, `PAIRED` (Task 3), `Tally`, `door_label`, `excluded_note`, `unavailable_reason`.
- Produces: one `tool_loop` summary line per door in `render_run`.

- [ ] **Step 1: Write the failing tests**

In `store.rs`'s test module, after `with_streamed`:

```rust
    fn looped(id: &str, grade: GradeRow, turns: u32, end: LoopEnd) -> Task {
        Task {
            tool_loop: Some(LoopRow {
                turns,
                tool_calls: turns,
                end,
            }),
            ..graded("tool_loop", id, grade)
        }
    }

    #[test]
    fn the_tool_loop_line_counts_reached_and_prints_the_turns_beside_it() {
        let eval = scratch("tool-loop-line");
        let mut writer = RunWriter::create(&eval, "r-tl", &head()).expect("create");
        let unmet = "stopped with the goal unmet after 2 turns: src/a.rs containing \"x\"";
        for task in [
            looped("tl-001", GradeRow::pass(), 2, LoopEnd::GoalMet),
            looped("tl-002", GradeRow::pass(), 3, LoopEnd::GoalMet),
            looped("tl-003", GradeRow::pass(), 6, LoopEnd::GoalMet),
            looped("tl-004", GradeRow::fail(unmet.to_owned()), 2, LoopEnd::GoalUnmet { wanted: "x".into() }),
            Task {
                transport: Transport::Streamed,
                ..looped("tl-001", GradeRow::fail("final reply hit max_tokens".to_owned()), 1, LoopEnd::Truncated)
            },
        ] {
            writer.append(task).expect("append");
        }
        let rendered = render_run(&RunLog::load(writer.dir()).expect("load"));
        assert!(
            rendered.contains("tool_loop    3/4 reached   turns 2/3/6 (min/median/max over reached)\n"),
            "{rendered}"
        );
        assert!(
            rendered.contains("tool_loop    streamed 0/1 reached (saturated: rank across candidates, not on this line)\n"),
            "no turns note when nothing reached; a 0/N or N/N is flagged: {rendered}"
        );
        assert!(rendered.contains(&format!("tool_loop FAIL tl-004  {unmet}")), "{rendered}");
        assert!(
            rendered.contains("asymmetry    tool_loop tl-001: buffered PASS, streamed FAIL — final reply hit max_tokens"),
            "the loop is a paired suite: {rendered}"
        );
    }

    #[test]
    fn an_all_unavailable_tool_loop_axis_is_na_with_its_reason() {
        let eval = scratch("tool-loop-na");
        let mut writer = RunWriter::create(&eval, "r-tl-na", &head()).expect("create");
        writer
            .append(graded("tool_loop", "tl-001", GradeRow::unavailable("server died".to_owned())))
            .expect("append");
        let rendered = render_run(&RunLog::load(writer.dir()).expect("load"));
        assert!(rendered.contains("tool_loop    N/A — nothing was measured (server died)"), "{rendered}");
    }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --manifest-path /Users/amoscoletti/personal_dev/chekov/Cargo.toml tool_loop_line 2>&1 | grep -E "panicked|^test result" | head -3`
Expected: the tests compile and fail — no `tool_loop` line is rendered yet.

- [ ] **Step 3: Commit the red**

```bash
cargo fmt --manifest-path /Users/amoscoletti/personal_dev/chekov/Cargo.toml
git -C /Users/amoscoletti/personal_dev/chekov add src/core/bench/store.rs
git -C /Users/amoscoletti/personal_dev/chekov commit -F - <<'EOF'
test(bench): red — the report's tool_loop line counts reached, prints turns beside it, flags saturation, and pairs the doors

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01USTtBCMA5rsVnigD6fhoc3
EOF
```

- [ ] **Step 4: Implement**

In `suite_summaries`, insert after `out.extend(instruction_line(log, Transport::Buffered));`:

```rust
    out.extend(tool_loop_line(log, Transport::Buffered));
```

and after `out.extend(instruction_line(log, Transport::Streamed));`:

```rust
    out.extend(tool_loop_line(log, Transport::Streamed));
```

After `tool_emit_line`, add:

```rust
/// `tool_loop    4/6 reached   turns 2/3/6 (min/median/max over reached)` —
/// the count is the grade; the turns are printed beside it, never scored.
fn tool_loop_line(log: &RunLog, transport: Transport) -> Option<String> {
    let rows: Vec<&TaskRow> = rows_via(log, "tool_loop", transport).collect();
    if rows.is_empty() {
        return None;
    }
    let label = door_label(transport);
    let tally = Tally::of(&rows);
    if tally.total == 0 {
        return Some(format!(
            "tool_loop    {label}N/A — nothing was measured ({})\n",
            unavailable_reason(&rows)
        ));
    }
    Some(format!(
        "tool_loop    {label}{} reached{}{}{}\n",
        tally.cell(),
        turns_note(&rows),
        saturation_note(tally),
        excluded_note(tally.excluded)
    ))
}

/// `   turns min/median/max (…)` over the rows that reached the goal; nothing
/// when none did.
fn turns_note(rows: &[&TaskRow]) -> String {
    let mut turns: Vec<u32> = rows
        .iter()
        .filter(|r| r.grade.as_ref().is_some_and(|g| g.pass))
        .filter_map(|r| r.tool_loop.as_ref())
        .map(|l| l.turns)
        .collect();
    if turns.is_empty() {
        return String::new();
    }
    turns.sort_unstable();
    let (min, max) = (turns[0], turns[turns.len() - 1]);
    let median = turns[turns.len() / 2];
    format!("   turns {min}/{median}/{max} (min/median/max over reached)")
}

/// A run every case of which reached, or none did, ranks nothing by itself.
fn saturation_note(tally: Tally) -> &'static str {
    if tally.total > 0 && (tally.passed == 0 || tally.passed == tally.total) {
        " (saturated: rank across candidates, not on this line)"
    } else {
        ""
    }
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --manifest-path /Users/amoscoletti/personal_dev/chekov/Cargo.toml tool_loop 2>&1 | grep -E "^test |test result"`
Expected: both new store tests pass, every earlier one still does.

- [ ] **Step 6: Gate and commit**

```bash
make -C /Users/amoscoletti/personal_dev/chekov lint && make -C /Users/amoscoletti/personal_dev/chekov test 2>&1 | grep -E "^test result" | head -1
git -C /Users/amoscoletti/personal_dev/chekov add src/core/bench/store.rs
git -C /Users/amoscoletti/personal_dev/chekov commit -F - <<'EOF'
feat(bench): the tool_loop report line — reached over measured, turns min/median/max beside it, N/A when nothing ran, saturation named

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01USTtBCMA5rsVnigD6fhoc3
EOF
```

---

### Task 9: `compare` totals — `tool_loop_totals`

**Files:**
- Modify: `src/core/bench/compare.rs:450-461` (`agentic_totals`) and beside `tool_emit_totals` (474)
- Test: inline in `src/core/bench/compare.rs`

**Interfaces:**
- Consumes: `Sides`, `rows_via`, `Tally`, `door_tag`, `SuiteTotals`.
- Produces: `tool_loop` / `tool_loop [streamed]` rows in `AgenticComparison::totals`.

- [ ] **Step 1: Write the failing test**

In `compare.rs`'s test module, after `the_agentic_totals_stand_side_by_side_in_the_reports_own_counting`:

```rust
    #[test]
    fn the_tool_loop_totals_and_disagreements_ride_the_agentic_comparison() {
        let a = agentic_run(
            "m1",
            vec![
                Case::pass("tool_loop", "tl-001"),
                Case::fail("tool_loop", "tl-002", "no terminal state in 8 turns (8 tool calls)"),
            ],
        );
        let b = agentic_run(
            "m2",
            vec![Case::pass("tool_loop", "tl-001"), Case::pass("tool_loop", "tl-002")],
        );
        let compared = compare_runs(&a, &b, &opts(5.0)).expect("same environment");
        assert_eq!(cells(&compared.agentic.totals, "tool_loop"), ("1/2".into(), "2/2".into()));
        let delta = compared
            .agentic
            .disagreements
            .iter()
            .find(|d| d.suite == "tool_loop" && d.task_id == "tl-002")
            .expect("the case one run reached and the other did not");
        assert!(!delta.a_pass && delta.b_pass);
        assert_eq!(delta.a_reason.as_deref(), Some("no terminal state in 8 turns (8 tool calls)"));
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test --manifest-path /Users/amoscoletti/personal_dev/chekov/Cargo.toml the_tool_loop_totals 2>&1 | grep -E "panicked|no total" | head -2`
Expected: panics `no total labelled tool_loop`.

- [ ] **Step 3: Commit the red**

```bash
cargo fmt --manifest-path /Users/amoscoletti/personal_dev/chekov/Cargo.toml
git -C /Users/amoscoletti/personal_dev/chekov add src/core/bench/compare.rs
git -C /Users/amoscoletti/personal_dev/chekov commit -F - <<'EOF'
test(bench): red — compare shows tool_loop side by side and names the case only one run reached

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01USTtBCMA5rsVnigD6fhoc3
EOF
```

- [ ] **Step 4: Implement**

In `agentic_totals`, after `out.extend(instruction_totals(&sides, Transport::Buffered));` add `out.extend(tool_loop_totals(&sides, Transport::Buffered));`, and after the streamed instruction line add `out.extend(tool_loop_totals(&sides, Transport::Streamed));`. After `tool_emit_totals`:

```rust
fn tool_loop_totals(sides: &Sides, transport: Transport) -> Option<SuiteTotals> {
    let a = rows_via(sides.a, "tool_loop", transport);
    if a.is_empty() {
        return None;
    }
    let b = rows_via(sides.b, "tool_loop", transport);
    Some(SuiteTotals {
        label: format!("tool_loop{}", door_tag(transport)),
        a: Tally::of(&a).cell(),
        b: Tally::of(&b).cell(),
    })
}
```

- [ ] **Step 5: Run, gate, commit**

```bash
cargo test --manifest-path /Users/amoscoletti/personal_dev/chekov/Cargo.toml the_tool_loop_totals 2>&1 | tail -2
make -C /Users/amoscoletti/personal_dev/chekov lint && make -C /Users/amoscoletti/personal_dev/chekov test 2>&1 | grep -E "^test result" | head -1
git -C /Users/amoscoletti/personal_dev/chekov add src/core/bench/compare.rs
git -C /Users/amoscoletti/personal_dev/chekov commit -F - <<'EOF'
feat(bench): compare's tool_loop totals per door, counted by the store's own Tally

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01USTtBCMA5rsVnigD6fhoc3
EOF
```

---

### Task 10: Wiring — the agentic pass runs the loop cases; the estimate counts them

**Files:**
- Modify: `src/commands/capability.rs:1713-1743` (`run_suites`), `:1772-1776` (`SuitePass`), `:1791-1809` (`agentic_estimate_secs` and its caller), `:1819-1841` (`run_agentic`), new `run_loop_case`/`append_loop` beside `run_tool_case`
- Test: inline in `src/commands/capability.rs` (`the_agentic_estimate_counts_both_doors`, `a_resumed_run_still_owes_the_other_door`)

**Interfaces:**
- Consumes: everything above.
- Produces: `tool_loop` rows in every `--suite agentic|all` run.

- [ ] **Step 1: Write the failing tests**

Rewrite `the_agentic_estimate_counts_both_doors`:

```rust
    #[test]
    fn the_agentic_estimate_counts_both_doors_and_the_loops_upper_bound() {
        use crate::core::bench::lifecycle::Suite;
        use crate::core::bench::probeset::{Expect, agentic_v0};
        let set = agentic_v0().expect("the compiled-in set is valid");
        let forced = set
            .tool_emit
            .iter()
            .filter(|c| c.expect == Expect::Call)
            .count();
        // Every unconstrained case crosses twice (buffered and streamed); the
        // forced pass crosses once; a loop case may cross up to K times per door.
        let cases = 2 * set.tool_emit.len() + forced + 2 * set.instruction.len();
        let loops = 2 * set.tool_loop.len() * 8;
        assert_eq!(
            super::agentic_estimate_secs(Some(Suite::Agentic), 8).expect("estimate"),
            (cases + loops) as u64 * 8
        );
        assert_eq!(
            super::agentic_estimate_secs(Some(Suite::Throughput), 8).expect("estimate"),
            0
        );
    }
```

In `a_resumed_run_still_owes_the_other_door` (line ~4001), append before its closing brace:

```rust
        let done = vec![("tool_loop".to_owned(), "tl-001".to_owned(), Transport::Buffered)];
        assert!(super::already_done(&done, &TaskKey::buffered("tool_loop", "tl-001")));
        assert!(!super::already_done(&done, &TaskKey::streamed("tool_loop", "tl-001")));
```

(Match the test's existing imports: it already uses `TaskKey`; add `Transport` to its `use` if absent.)

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo test --manifest-path /Users/amoscoletti/personal_dev/chekov/Cargo.toml agentic_estimate 2>&1 | grep -E "^error" | head -2`
Expected: `this function takes 1 argument but 2 were supplied`.

- [ ] **Step 3: Commit the red**

```bash
cargo fmt --manifest-path /Users/amoscoletti/personal_dev/chekov/Cargo.toml
git -C /Users/amoscoletti/personal_dev/chekov add src/commands/capability.rs
git -C /Users/amoscoletti/personal_dev/chekov commit -F - <<'EOF'
test(bench): red — the plan's estimate carries the loop's upper bound; a resumed loop row still owes its other door

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01USTtBCMA5rsVnigD6fhoc3
EOF
```

- [ ] **Step 4: Implement**

`SuitePass` gains a field:

```rust
struct SuitePass<'a> {
    wire: &'a crate::core::bench::runner::ProbeWire<'a>,
    clock: TimingClock,
    runtime: Option<&'a str>,
    /// `[bench] tool_loop_max_turns` — the loop probe's K.
    max_turns: u32,
}
```

In `run_suites`, the `SuitePass` literal gains `max_turns: ctx.config.file.bench.tool_loop_max_turns,`. Run `grep -n "SuitePass {" /Users/amoscoletti/personal_dev/chekov/src/commands/capability.rs` and give every other literal (tests, the judge pass if it builds one) the same field.

`agentic_estimate_secs`:

```rust
/// Rough extra seconds for the agentic suites (8s per crossing), from the
/// validated set — a suite that will not run costs nothing. Unconstrained
/// cases cross twice (both doors); the forced pass once; a loop case up to
/// `max_turns` times per door — the ceiling, since a loop that closes in two
/// turns costs a quarter of it.
fn agentic_estimate_secs(
    suite: Option<crate::core::bench::lifecycle::Suite>,
    max_turns: u32,
) -> Result<u64, ChekovError> {
    use crate::core::bench::lifecycle::Suite;
    if !suite.is_some_and(Suite::runs_agentic) {
        return Ok(0);
    }
    let set = crate::core::bench::probeset::agentic_v0()?;
    let forced = set
        .tool_emit
        .iter()
        .filter(|c| c.expect == crate::core::bench::probeset::Expect::Call)
        .count();
    let loops = 2 * set.tool_loop.len() * max_turns as usize;
    let crossings = 2 * set.tool_emit.len() + forced + 2 * set.instruction.len() + loops;
    Ok(crossings as u64 * 8)
}
```

Find its caller (`grep -n "agentic_estimate_secs(" …`) and pass `ctx.config.file.bench.tool_loop_max_turns` (or the `BenchSection` in scope there).

`run_agentic`: after the instruction loop inside the door loop:

```rust
        for case in &set.tool_loop {
            let run = crate::core::bench::toolloop::LoopRun {
                case,
                system: &set.loop_system,
                max_turns: suite.max_turns,
            };
            run_loop_case(sink, &pass, &run)?;
        }
```

After `run_instruction_case`, add:

```rust
/// One loop case through this door. The driver rides `agentic_cross`, so a
/// foreign run's untimed buffered door and llama.cpp's timed doors all work,
/// and the row's measure is whatever the door could time (tool-loop design §5).
fn run_loop_case(
    sink: &mut TaskSink,
    pass: &AgenticPass,
    run: &crate::core::bench::toolloop::LoopRun,
) -> Result<(), ChekovError> {
    use crate::core::bench::store::TaskKey;
    use crate::core::bench::toolloop;
    let key = TaskKey {
        suite: "tool_loop",
        task_id: &run.case.id,
        transport: pass.transport,
    };
    if sink.is_done(&key) {
        return Ok(());
    }
    let mut door = |req: &crate::core::proxy::http::HttpRequest| {
        agentic_cross(pass, req).map(|(timings, body)| toolloop::Turn { body, timings })
    };
    let outcome = row_outcome(toolloop::drive(&mut door, run), pass.suite.runtime);
    append_loop(sink, key, outcome)
}

/// A finished loop is a graded row with its turn record; a crossing that
/// failed mid-loop is unavailable, as every suite records it.
fn append_loop(
    sink: &mut TaskSink,
    key: crate::core::bench::store::TaskKey,
    outcome: Result<crate::core::bench::toolloop::LoopOutcome, ChekovError>,
) -> Result<(), ChekovError> {
    use crate::core::bench::{grade, store};
    let (measure, verdict, tool_loop) = match outcome {
        Ok(done) => {
            let row = store::LoopRow {
                turns: done.turns,
                tool_calls: done.tool_calls,
                end: done.end.clone(),
            };
            (done.measure, grade_row(grade::grade_tool_loop(&done)), Some(row))
        }
        Err(e) => {
            let (measure, verdict) = failed_probe(&e);
            (measure, verdict, None)
        }
    };
    sink.writer.append(store::Task {
        suite: key.suite.into(),
        task_id: key.task_id.into(),
        measure,
        grade: Some(verdict),
        transport: key.transport,
        codebase: None,
        judge: None,
        tool_loop,
    })
}
```

(`grade_tool_loop` borrows `done` after `done.measure` is moved — build `row` and the grade before moving `measure`: `let verdict = grade_row(grade::grade_tool_loop(&done)); (done.measure, verdict, Some(row))`.)

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --manifest-path /Users/amoscoletti/personal_dev/chekov/Cargo.toml -- agentic_estimate resumed_run 2>&1 | grep -E "^test |test result"`

- [ ] **Step 6: Gate and commit**

```bash
make -C /Users/amoscoletti/personal_dev/chekov lint && make -C /Users/amoscoletti/personal_dev/chekov test 2>&1 | grep -E "^test result" | head -1
git -C /Users/amoscoletti/personal_dev/chekov add src/commands/capability.rs
git -C /Users/amoscoletti/personal_dev/chekov commit -F - <<'EOF'
feat(bench): the agentic pass drives every tool_loop case through both doors; the plan's estimate carries the loop's ceiling

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01USTtBCMA5rsVnigD6fhoc3
EOF
```

- [ ] **Step 7: A live smoke run (no code change)**

The daily driver is up (`pgrep -fl llama-server` names `ornith-1.5-35b-a3b`). A single-model `--suite agentic` run reuses it: run `target/debug/chekov capability bench ornith-1.5-35b-a3b --suite agentic --dry-run` first and read the estimate, then the real run with `--yes`, and read the report's `tool_loop` lines. Record the result in the CHANGELOG entry of Task 11 (the counts per door, and the turns note). If the run cannot be made in this session, the CHANGELOG says so and the spread is owed.

---

### Task 11: Documentation, the spec's amendments, the gate

**Files:**
- Modify: `README.md:107` (the `agentic` clause of the `capability bench` row), `README.md` `[bench]` block (after `judge_reasoning_effort`)
- Modify: `CHANGELOG.md:9` (a new first bullet under `## [Unreleased]` → `### Added`)
- Modify: `docs/capability-spec.md:816` (a status line under the §7.2 table)
- Modify: `docs/superpowers/specs/2026-09-06-tool-loop-probe-design.md` (an "Amendments" section at the end)

- [ ] **Step 1: README**

Replace, on line 107, the clause
`` `agentic` runs the probe set (`tool_emit`, `grammar_gap`, `instruction`), with each unconstrained case crossing both the buffered and the streamed door so an asymmetry between them is named. ``
with
`` `agentic` runs the probe set (`tool_emit`, `grammar_gap`, `instruction`, and `tool_loop` — six canned read→edit→verify cases each driven to a terminal state through an in-process tool environment whose `edit_file` has Claude Code's own exactly-one-occurrence contract; the count of goals reached is the grade, the turn counts are printed beside it and never scored, and a loop still calling tools after `[bench] tool_loop_max_turns` turns fails as exhausted), with each unconstrained case crossing both the buffered and the streamed door so an asymmetry between them is named. ``

In the `[bench]` block of the Configuration section, after the `judge_reasoning_effort` line:

```toml
tool_loop_max_turns = 8          # turn budget per tool_loop case; part of the agentic prompt-set hash
```

- [ ] **Step 2: CHANGELOG**

Insert as the first bullet under `### Added` in `[Unreleased]`:

```markdown
- `capability bench --suite agentic` gains `tool_loop` (spec §7.2 row 4):
  six canned read→edit→verify cases, each driven to a terminal state through
  an in-process tool environment — `read_file`, `list_dir`, `grep`, an
  `edit_file` with Claude Code's own exactly-one-occurrence contract, and a
  `run_tests` that answers one fixed failing line until the goal is met —
  on both doors, within `[bench] tool_loop_max_turns` turns (default 8).
  The grade is the end state (reached; stopped unmet; exhausted; truncated;
  a fabricated tool; a malformed call) and never the path: the report
  prints `tool_loop    4/6 reached   turns 2/3/6 (min/median/max over
  reached)` per door, `compare` shows the two counts side by side and names
  the cases only one run reached, and every row carries a typed `tool_loop`
  record (`turns`, `tool_calls`, `end`). Why: single-turn `tool_emit` had
  stopped separating the models this desk benches (8–10/10 across four),
  and Claude Code is a loop, not a call. Adding the cases changes the
  agentic `prompt_set_hash` — runs recorded before this change compare
  with each other, new runs with new runs — and the turn budget is part of
  that hash, so two runs judged under different budgets refuse by name.
  Live spread on this desk: <fill from Task 10 step 7, or "owed">.
```

- [ ] **Step 3: The capability spec and the design's amendments**

In `docs/capability-spec.md`, after the §7.2 table (before `### 7.3`), add:

```markdown
Status 2026-09-09: `tool_loop` shipped — see `docs/superpowers/specs/2026-09-06-tool-loop-probe-design.md`. Still deferred: `diff_fidelity`, `think_leak` (§13 Q5), `long_ctx_trace` (pays only past the 16K depth the sweep stops at), `hallucination` (largely covered by codebase tier 5).
```

Append to the design spec:

```markdown
## 13. Amendments made during implementation (2026-09-09)

- The goal's `contains` became `contains_any`, a list: two correct spellings
  of one edit (`from_str_major(amount)` / `from_str_major(amount.trim())`)
  must both pass, and a single string could not say so.
- An `Edited` goal may name `untouched` files that must stay byte-identical
  to their canned copy, so a model that edits both `version()` definitions
  in `tl-004` does not pass by editing everything.
- The "not discriminating" clause is per run — `render_run` sees one run —
  and reads `(saturated: rank across candidates, not on this line)` on a
  `0/N` or `N/N`; `compare` is where candidates stand side by side.
- A loop reply chekov's own translator could not produce fails the crossing
  as `ProxyBadRequest` (unavailable, never a model failure), which is the
  spec's §9 "no new variants" honoured with the variant the runner already
  uses for its own body faults.
```

- [ ] **Step 4: The gate**

Run: `make -C /Users/amoscoletti/personal_dev/chekov lint && make -C /Users/amoscoletti/personal_dev/chekov test 2>&1 | grep -E "^test result|FAILED"`
Expected: clean; every suite green.

- [ ] **Step 5: Commit, push, PR**

```bash
git -C /Users/amoscoletti/personal_dev/chekov add README.md CHANGELOG.md docs/capability-spec.md docs/superpowers/specs/2026-09-06-tool-loop-probe-design.md docs/superpowers/plans/2026-09-09-tool-loop-probe.md
git -C /Users/amoscoletti/personal_dev/chekov commit -F - <<'EOF'
docs: tool_loop in the README, CHANGELOG and the capability spec; the design's four amendments recorded

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01USTtBCMA5rsVnigD6fhoc3
EOF
git -C /Users/amoscoletti/personal_dev/chekov push -u origin feat/bench-tool-loop
gh pr create --base develop --head feat/bench-tool-loop --title "feat(bench): tool_loop — six canned read→edit→verify loops, graded on the end state" --body "$(cat <<'EOF'
Implements `docs/superpowers/specs/2026-09-06-tool-loop-probe-design.md` (approved 2026-09-09) via `docs/superpowers/plans/2026-09-09-tool-loop-probe.md`.

- `agentic_v0.toml`: six `[[tool_loop]]` cases (canned files, a palette, a goal) and the loop's system text; every way a case can lie is refused at load.
- `core::bench::toolloop`: the canned environment (five tools, pure over case + calls) and the driver (one door in, a terminal state out).
- Rows carry a typed `tool_loop` record; the report prints reached counts with turns beside them; `compare` shows both doors side by side.
- `[bench] tool_loop_max_turns` (default 8) is part of the agentic prompt-set hash.

Comparability: the agentic hash changes with the set, by design — old runs compare with old, new with new.

🤖 Generated with [Claude Code](https://claude.com/claude-code)

https://claude.ai/code/session_01USTtBCMA5rsVnigD6fhoc3
EOF
)"
```
