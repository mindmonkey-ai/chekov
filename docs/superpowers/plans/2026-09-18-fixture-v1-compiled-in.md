# fixture-v1 Compiled In — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `chekov capability bench --fixture --allow-exec` run the compiled-in fixture-v1 crate through codebase mode's pipeline with manifest-named tasks and held-out tests injected only at grading time.

**Architecture:** A root `build.rs` embeds `fixtures/fixture-v1/` as a `(path, text)` table. At run time the manifest is parsed and hash-verified, the visible files are materialized into a scratch git repo, and `codebase::prepare_named` runs the existing walk, mask, leakage-filter and cargo tiers over the four named bodies. Tier 7 writes the task's hidden test into the exec worktree's `tests/` for exactly one `cargo test` and removes it after.

**Tech Stack:** Rust 2024 edition (rust-version 1.95), clap 4 derive, serde + toml, the crate's own `hash::sha256_hex`. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-09-18-fixture-v1-compiled-in-design.md` (read it first; its "Amendments" section records what this plan changed).

## Global Constraints

- Functions ≤ 40 LOC, ≤ 3 parameters (bundle into a struct beyond that), no boolean flag parameters, nesting ≤ 3 — `clippy.toml` enforces these and `make lint` runs clippy with `-D warnings`.
- No `unwrap()`/`expect()` outside `#[cfg(test)]`; no `#[allow(...)]`; never loosen a gate.
- `tests/**` is write-protected. `tests/codebase_exec.rs` constructs `CodebaseTask`, `Env` and calls `exec::exec_crossing(&env, &task, fill)` by literal, so **those two structs gain no mandatory fields and that signature does not change**.
- Every externally-read struct is serde with `deny_unknown_fields`.
- Logging/printing conventions: refusals name the fixture and the rule; nothing is silently skipped.
- Never `cd` in shell commands. Run the fixture crate's own tests with `cargo test --manifest-path fixtures/fixture-v1/Cargo.toml`.
- Commit messages follow the repo's `type(scope): summary` form and end with the attribution lines the session provides.
- Run `make lint && make test` before every commit that touches `src/`.

---

## File Structure

| File | Responsibility |
|------|----------------|
| `build.rs` (new) | Walk `fixtures/fixture-v1/`, write `$OUT_DIR/fixture_v1_files.rs` with `pub const FILES: &[(&str, &str)]`. |
| `fixtures/fixture-v1/src/**` (modified) | Scrubbed of answer-leaking comments. |
| `fixtures/fixture-v1/manifest.toml` (modified) | `symbol` per task, real `content_hash`. |
| `src/core/bench/fixture/mod.rs` (moved from `fixture.rs`) | External probe-set loader (unchanged) + `ID`, `builtin()`, `named_tasks()`. |
| `src/core/bench/fixture/embedded.rs` (new) | `include!` of the generated table + its tests. |
| `src/core/bench/fixture/manifest.rs` (new) | Strict `Manifest`, `content_hash`, `parse`, validation. |
| `src/core/bench/fixture/materialize.rs` (new) | Visible files → scratch git repo; hidden files kept in memory. |
| `src/core/bench/codebase/tree.rs` (modified) | `init_and_commit(repo, message)`. |
| `src/core/bench/codebase/mod.rs` (modified) | `HiddenTest`, `Prepared.hidden` + `Prepared.corpus`, `walk`/`finish` split, `prepare_named`. |
| `src/core/bench/codebase/named.rs` (new) | `NamedTask`, `NamedTasks`, `pick` by symbol with `Owner::` disambiguation. |
| `src/core/bench/codebase/exec.rs` (modified) | `Crossing`, `exec_crossing_with`, `injected_tier`. |
| `src/core/bench/codebase/run.rs` (modified) | `exec_row` looks up the task's hidden test. |
| `src/commands/capability.rs` (modified) | `FixtureArg`, `prepare_fixture`, corpus override, plan line. |
| `README.md`, `CHANGELOG.md`, `IDEAS.md` (modified) | Documentation. |

---

### Task 1: Scrub the fixture sources and name the masked symbols

**Files:**
- Modify: `fixtures/fixture-v1/src/store/mod.rs:1-13`, `:77-84`
- Modify: `fixtures/fixture-v1/src/api/mod.rs:1-7`, `:30-40`
- Modify: `fixtures/fixture-v1/src/domain/money.rs:9-24`
- Modify: `fixtures/fixture-v1/src/store/replay.rs:1-12`, `:40-61`
- Modify: `fixtures/fixture-v1/manifest.toml:42-77`
- Modify: `fixtures/fixture-v1/README.md` (symbol column)

**Interfaces:**
- Produces: `manifest.toml` tasks each carry `symbol = "..."` — consumed by Task 3's `ManifestTask.symbol` and Task 5's `named::pick`.

Why: the visible sources currently tell the candidate the answer. `money.rs:20-24` and `replay.rs:55-61` are stray `// MASKED — TASK n` blocks *outside* the function body (the masker only removes the body, and the leakage filter only drops a `///` doc comment directly above a masked body). `api/mod.rs:37-40` says "The correct call is `self.ledger.apply_entry(cmd)`". A comment inside a masked body is removed with the body and may stay.

- [ ] **Step 1: Rewrite `store/mod.rs` module and struct docs**

Replace lines 1-13 with:

```rust
//! An audit sink: ledger entries recorded then flushed. A trait and two impls
//! with different failure semantics: `VecStore` never refuses on `record`;
//! `LimitedStore` rejects with `Err(StoreError::Full)` once it holds
//! `capacity` entries. A caller of either relies on that contract.
```

Replace lines 77-84 (the `LimitedStore` doc) with:

```rust
/// A capacity-bounded audit sink: `record` rejects with `Err(StoreError::Full)`
/// once the buffer holds `capacity` entries — the deliberate opposite of
/// `VecStore`.
```

- [ ] **Step 2: Rewrite `api/mod.rs` module and method docs**

Replace lines 1-7 with:

```rust
//! The command dispatcher over a ledger. `Ledger` exposes two entry APIs,
//! `append_entry` and `apply_entry`; the dispatcher's job is to keep the
//! running balance projection current as commands arrive.
```

Replace lines 30-40 (the `handle_credit` doc) with:

```rust
    /// Dispatch a credit command to the ledger.
    ///
    /// Must fold the command into the running projection and return the
    /// exact `CreditOutcome` the ledger produced.
```

- [ ] **Step 3: Rewrite `money.rs` doc and delete the stray block**

Replace lines 9-24 (the `from_str` doc plus the stray `// MASKED` block) with:

```rust
/// Parse a decimal currency string into exact `i128` cents.
///
/// This is the only constructor that must be used. `2499.95` is `249995`.
/// Exactly two fractional digits are allowed: a string with a third (`12.345`)
/// is rejected, not truncated, so the parse is total over the accepted format.
```

- [ ] **Step 4: Rewrite `replay.rs` docs and delete the stray block**

Replace lines 1-12 with:

```rust
//! Replay the recorded entries through a `Filter`. `replay_filtered` returns
//! a lazy iterator over `entries`; the filter's predicate decides which
//! entries survive.
```

Replace lines 40-61 (the `replay_filtered` doc plus the stray `// MASKED` block) with:

```rust
/// Stream the entries that satisfy `filter`, lazily, borrowing both for the
/// same `'a`.
```

- [ ] **Step 5: Grep for anything left**

Run: `grep -rn -i "MASKED\|TASK [0-9]\|hidden/\|trap\|near-miss\|device" fixtures/fixture-v1/src --include=*.rs | grep -v "#\[cfg(test)\]" `
Expected: hits only inside `#[cfg(test)]` modules or inside the four masked bodies (`LimitedStore::record`, `handle_credit`, `from_str`, `replay_filtered`). Fix any other hit the same way: keep the behavioural contract, drop the device name, the hidden-file name and any statement of which of two APIs is right. `domain/mod.rs`'s invariant comment ("balances are `i128` cents, never built from an `f64`") stays — it is the contract device 3 tests.

- [ ] **Step 6: Add `symbol` to every manifest task**

In `fixtures/fixture-v1/manifest.toml`, add one line to each `[[tasks]]` table directly under `tier`:

```toml
[[tasks]]
id = "device-1-store-limited-full"
# ...existing keys...
tier = 7
symbol = "LimitedStore::record"
```

```toml
id = "device-2-near-miss-api"
# ...
symbol = "handle_credit"
```

```toml
id = "device-3-invariant-exact"
# ...
symbol = "from_str"
```

```toml
id = "device-4-lifetime-knot"
# ...
symbol = "replay_filtered"
```

Add this comment above the first `[[tasks]]`: `# symbol: the masked fn — "name", or "Owner::name" when the name repeats in the file (both stores define record).`

In `README.md`'s device table change the "Masked source" cells to name the symbol exactly as the manifest does.

- [ ] **Step 7: The fixture crate still builds and its own tests pass**

Run: `cargo test --manifest-path fixtures/fixture-v1/Cargo.toml`
Expected: `test result: ok` (its inline tests are untouched).

- [ ] **Step 8: Commit**

```bash
git add fixtures/fixture-v1
git commit -m "fix(fixture-v1): scrub answer-leaking comments; name each masked symbol"
```

---

### Task 2: Embed the fixture at build time

**Files:**
- Create: `build.rs`
- Create: `src/core/bench/fixture/embedded.rs` (the module is wired in Task 3; this task makes it compile by doing the move now)
- Modify: `src/core/bench/fixture.rs` → move to `src/core/bench/fixture/mod.rs`

**Interfaces:**
- Produces: `crate::core::bench::fixture::embedded::FILES: &[(&str, &str)]` — sorted, fixture-relative paths with `/` separators; everything under `fixtures/fixture-v1/` except `target/`, `.git/`, `README.md`, `.gitignore`.

- [ ] **Step 1: Move the module so it can hold children**

```bash
git mv src/core/bench/fixture.rs src/core/bench/fixture/mod.rs
```

Add at the top of `src/core/bench/fixture/mod.rs`, after the `//!` doc block:

```rust
pub mod embedded;
```

- [ ] **Step 2: Write the failing embedded-table tests**

Create `src/core/bench/fixture/embedded.rs`:

```rust
//! The compiled-in fixture-v1 tree: every file `build.rs` found under
//! `fixtures/fixture-v1/`, as `(fixture-relative path, text)`, sorted.
//!
//! The hidden tests are in here too — they are excluded from prompts by the
//! manifest and the materializer, never by this table.

include!(concat!(env!("OUT_DIR"), "/fixture_v1_files.rs"));

#[cfg(test)]
mod tests {
    use super::FILES;

    fn has(path: &str) -> bool {
        FILES.iter().any(|(p, _)| *p == path)
    }

    #[test]
    fn the_table_carries_the_manifest_the_crate_and_every_hidden_test() {
        for path in [
            "manifest.toml",
            "Cargo.toml",
            "Cargo.lock",
            "src/lib.rs",
            "src/store/mod.rs",
            "hidden/store_limited_full.rs",
            "hidden/near_miss_api.rs",
            "hidden/invariant_exact.rs",
            "hidden/lifetime_knot.rs",
        ] {
            assert!(has(path), "{path} missing from FILES");
        }
    }

    #[test]
    fn nothing_from_target_or_the_readme_is_embedded_and_paths_are_sorted() {
        for (path, _) in FILES {
            assert!(!path.starts_with("target/"), "{path} is build output");
            assert!(!path.contains('\\'), "{path} must use forward slashes");
            assert_ne!(*path, "README.md");
            assert_ne!(*path, ".gitignore");
        }
        let paths: Vec<&str> = FILES.iter().map(|(p, _)| *p).collect();
        let mut sorted = paths.clone();
        sorted.sort_unstable();
        assert_eq!(paths, sorted, "build.rs sorts the table");
    }
}
```

- [ ] **Step 3: Run the tests to see them fail**

Run: `cargo test --locked embedded::tests`
Expected: compile error — `fixture_v1_files.rs` does not exist (no `build.rs` yet).

- [ ] **Step 4: Write `build.rs`**

Create `build.rs` at the crate root (cargo picks it up by name; no `Cargo.toml` change):

```rust
//! Embeds `fixtures/fixture-v1/` into the binary as a `(path, text)` table.
//!
//! No build dependency: the content hash is computed at run time from the
//! same table (`core::bench::fixture::manifest::content_hash`). This script
//! never touches the crate's own CLI (AGENTS.md §12 override 3 still holds).

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

const FIXTURE_DIR: &str = "fixtures/fixture-v1";
const SKIPPED_DIRS: [&str; 2] = ["target", ".git"];
const SKIPPED_FILES: [&str; 2] = ["README.md", ".gitignore"];

fn main() -> Result<(), Box<dyn Error>> {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?).join(FIXTURE_DIR);
    println!("cargo:rerun-if-changed={}", root.display());
    let mut files = Vec::new();
    walk(&root, &root, &mut files)?;
    files.sort();
    let out = PathBuf::from(std::env::var("OUT_DIR")?).join("fixture_v1_files.rs");
    fs::write(&out, table(&root, &files))?;
    Ok(())
}

/// Every embedded file under `dir`, as a fixture-relative `/`-separated path.
fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<(), Box<dyn Error>> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if path.is_dir() {
            if !SKIPPED_DIRS.contains(&name.as_str()) {
                walk(root, &path, out)?;
            }
        } else if !SKIPPED_FILES.contains(&name.as_str()) {
            let relative = path.strip_prefix(root)?.to_string_lossy().replace('\\', "/");
            out.push(relative);
        }
    }
    Ok(())
}

/// `pub const FILES: &[(&str, &str)] = &[ ("Cargo.toml", include_str!("…")), … ];`
fn table(root: &Path, files: &[String]) -> String {
    let mut text = String::from("pub const FILES: &[(&str, &str)] = &[\n");
    for relative in files {
        let absolute = root.join(relative).display().to_string();
        text.push_str(&format!("    ({relative:?}, include_str!({absolute:?})),\n"));
    }
    text.push_str("];\n");
    text
}
```

- [ ] **Step 5: Run the tests to see them pass**

Run: `cargo test --locked embedded::tests`
Expected: 2 passed.

- [ ] **Step 6: Lint**

Run: `make lint`
Expected: exit 0. If clippy flags `format!` inside `push_str`, replace that line with `use std::fmt::Write; writeln!(text, "    ({relative:?}, include_str!({absolute:?})),")?;` and make `table` return `Result<String, std::fmt::Error>`.

- [ ] **Step 7: Commit**

```bash
git add build.rs src/core/bench/fixture
git commit -m "feat(fixture): embed fixtures/fixture-v1 at build time"
```

---

### Task 3: The manifest contract and the real content hash

**Files:**
- Create: `src/core/bench/fixture/manifest.rs`
- Modify: `src/core/bench/fixture/mod.rs` (module doc, `pub mod manifest`, `ID`, `builtin()`)
- Modify: `fixtures/fixture-v1/manifest.toml:32` (`content_hash`)

**Interfaces:**
- Produces:
  - `fixture::ID: &str = "fixture-v1"`
  - `fixture::manifest::{Manifest, ManifestTask, MANIFEST_VERSION, MANIFEST_PATH}`
  - `fixture::manifest::content_hash(files: &[(&str, &str)]) -> String`
  - `fixture::manifest::parse(text: &str, files: &[(&str, &str)]) -> Result<Manifest, ChekovError>`
  - `fixture::manifest::invalid(reason: String) -> ChekovError`
  - `fixture::builtin() -> Result<Manifest, ChekovError>`
- Consumes: `embedded::FILES` (Task 2), `crate::core::hash::sha256_hex(&[u8]) -> String`, `ChekovError::FixtureInvalid { path: PathBuf, reason: String }`.

- [ ] **Step 1: Write the failing manifest tests**

Create `src/core/bench/fixture/manifest.rs` with only the tests for now:

```rust
//! The fixture-v1 grading contract (`manifest.toml`), read strictly.

#[cfg(test)]
mod tests {
    use super::{Manifest, content_hash, parse};

    const SRC: &str = "pub fn a() -> u32 {\n    1\n}\n";
    const HIDDEN: &str = "#[test]\nfn t() {}\n";

    /// A three-file fixture whose manifest declares the hash of the other two.
    fn files(manifest_body: &str) -> Vec<(String, String)> {
        let content = [("src/a.rs", SRC), ("hidden/t.rs", HIDDEN)];
        let hash = content_hash(&content);
        let manifest = format!(
            "version = 1\nid = \"fixture-v1\"\ncontent_hash = \"{hash}\"\ntask_slots = 10\n{manifest_body}"
        );
        let mut out: Vec<(String, String)> = content
            .iter()
            .map(|(p, t)| ((*p).to_owned(), (*t).to_owned()))
            .collect();
        out.push(("manifest.toml".to_owned(), manifest));
        out
    }

    fn borrowed(files: &[(String, String)]) -> Vec<(&str, &str)> {
        files.iter().map(|(p, t)| (p.as_str(), t.as_str())).collect()
    }

    fn parse_body(body: &str) -> Result<Manifest, String> {
        let files = files(body);
        let refs = borrowed(&files);
        let text = refs.iter().find(|(p, _)| *p == "manifest.toml").map(|(_, t)| *t).expect("manifest");
        parse(text, &refs).map_err(|e| e.to_string())
    }

    const ONE_TASK: &str = "[[tasks]]\nid = \"d1\"\ndevice = \"x\"\nsource = \"src/a.rs\"\nhidden = \"hidden/t.rs\"\ntier = 7\nsymbol = \"a\"\n";

    #[test]
    fn a_valid_manifest_parses_and_the_hash_excludes_the_manifest_itself() {
        let manifest = parse_body(ONE_TASK).expect("parses");
        assert_eq!(manifest.tasks.len(), 1);
        assert_eq!(manifest.tasks[0].symbol, "a");
        let with = content_hash(&[("src/a.rs", SRC), ("hidden/t.rs", HIDDEN), ("manifest.toml", "anything")]);
        let without = content_hash(&[("src/a.rs", SRC), ("hidden/t.rs", HIDDEN)]);
        assert_eq!(with, without, "editing the manifest never changes the content hash");
    }

    #[test]
    fn a_wrong_hash_is_refused_naming_both_values() {
        let files = files(ONE_TASK);
        let mut refs = borrowed(&files);
        refs.push(("src/extra.rs", "pub fn extra() {}\n"));
        let text = refs.iter().find(|(p, _)| *p == "manifest.toml").map(|(_, t)| *t).expect("manifest");
        let err = parse(text, &refs).expect_err("refused").to_string();
        assert!(err.contains("content hash") && err.contains("does not match"), "{err}");
    }

    #[test]
    fn an_unknown_key_a_stray_hidden_a_duplicate_id_and_a_bad_tier_are_refused() {
        let unknown = format!("{ONE_TASK}extra = 1\n");
        assert!(parse_body(&unknown).expect_err("unknown key").contains("extra"));
        let stray = ONE_TASK.replace("hidden/t.rs", "src/a.rs");
        assert!(parse_body(&stray).expect_err("hidden outside hidden/").contains("hidden/"));
        let twice = format!("{ONE_TASK}{ONE_TASK}");
        assert!(parse_body(&twice).expect_err("duplicate").contains("listed twice"));
        let five = ONE_TASK.replace("tier = 7", "tier = 5");
        assert!(parse_body(&five).expect_err("tier").contains("tier 5"));
        let empty = ONE_TASK.replace("symbol = \"a\"", "symbol = \"\"");
        assert!(parse_body(&empty).expect_err("symbol").contains("symbol is empty"));
        assert!(parse_body("").expect_err("no tasks").contains("no tasks"));
    }

    #[test]
    fn a_newer_version_is_refused_naming_what_this_chekov_reads() {
        let files = files(ONE_TASK);
        let refs = borrowed(&files);
        let text = refs.iter().find(|(p, _)| *p == "manifest.toml").map(|(_, t)| *t).expect("manifest");
        let bumped = text.replace("version = 1", "version = 2");
        let err = parse(&bumped, &refs).expect_err("refused").to_string();
        assert!(err.contains("version 2") && err.contains("manifest version 1"), "{err}");
    }
}
```

- [ ] **Step 2: Run to see them fail**

Run: `cargo test --locked manifest::tests`
Expected: compile errors — `Manifest`, `content_hash`, `parse` undefined.

- [ ] **Step 3: Implement the manifest module**

Insert above the `#[cfg(test)]` block in `manifest.rs`:

```rust
use std::path::PathBuf;

use serde::Deserialize;

use crate::core::hash::sha256_hex;
use crate::error::ChekovError;

/// What this chekov knows how to read.
pub const MANIFEST_VERSION: u32 = 1;
/// The manifest's path inside the fixture — the one embedded file the
/// content hash leaves out, so writing the hash in never changes it.
pub const MANIFEST_PATH: &str = "manifest.toml";
const HIDDEN_DIR: &str = "hidden/";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    pub id: String,
    pub content_hash: String,
    /// Reserve slots (capability-spec §9) — reported, never enforced here.
    pub task_slots: u32,
    pub tasks: Vec<ManifestTask>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestTask {
    pub id: String,
    pub device: String,
    /// The file holding the masked body.
    pub source: String,
    /// The held-out test, under `hidden/`; never in a prompt.
    pub hidden: String,
    /// The scoring tier this device is reported on: 6 (compile) or 7 (test).
    pub tier: u32,
    /// The masked fn: `name`, or `Owner::name` when the name repeats in the file.
    pub symbol: String,
}

pub fn invalid(reason: String) -> ChekovError {
    ChekovError::FixtureInvalid {
        path: PathBuf::from(format!("fixture-v1/{MANIFEST_PATH}")),
        reason,
    }
}

/// SHA-256 over every embedded file but the manifest, as `path\0text\0` in
/// table order — an edit to any fixture file without a manifest bump is a
/// refused run, not a silently different corpus.
#[must_use]
pub fn content_hash(files: &[(&str, &str)]) -> String {
    let mut bytes = Vec::new();
    for (path, text) in files.iter().filter(|(p, _)| *p != MANIFEST_PATH) {
        bytes.extend_from_slice(path.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(text.as_bytes());
        bytes.push(0);
    }
    sha256_hex(&bytes)
}

pub fn parse(text: &str, files: &[(&str, &str)]) -> Result<Manifest, ChekovError> {
    let manifest: Manifest = toml::from_str(text).map_err(|e| invalid(e.to_string()))?;
    if manifest.version != MANIFEST_VERSION {
        return Err(invalid(format!(
            "version {} — this chekov reads manifest version {MANIFEST_VERSION}",
            manifest.version
        )));
    }
    let computed = content_hash(files);
    if manifest.content_hash != computed {
        return Err(invalid(format!(
            "content hash {computed} does not match manifest {}",
            manifest.content_hash
        )));
    }
    validate_tasks(&manifest.tasks, files).map_err(invalid)?;
    Ok(manifest)
}

fn validate_tasks(tasks: &[ManifestTask], files: &[(&str, &str)]) -> Result<(), String> {
    if tasks.is_empty() {
        return Err("no tasks — a manifest with nothing to grade".to_owned());
    }
    let mut ids = std::collections::BTreeSet::new();
    for task in tasks {
        if !ids.insert(task.id.as_str()) {
            return Err(format!("task {} is listed twice", task.id));
        }
        validate_task(task, files)?;
    }
    Ok(())
}

fn validate_task(task: &ManifestTask, files: &[(&str, &str)]) -> Result<(), String> {
    let has = |path: &str| files.iter().any(|(p, _)| *p == path);
    if !has(&task.source) {
        return Err(format!("task {}: source {} is not in the fixture", task.id, task.source));
    }
    if !task.hidden.starts_with(HIDDEN_DIR) || !has(&task.hidden) {
        return Err(format!(
            "task {}: hidden {} must be an embedded file under {HIDDEN_DIR}",
            task.id, task.hidden
        ));
    }
    if !matches!(task.tier, 6 | 7) {
        return Err(format!(
            "task {}: tier {} — only 6 (compile) and 7 (test) are graded",
            task.id, task.tier
        ));
    }
    if task.symbol.trim().is_empty() {
        return Err(format!("task {}: symbol is empty", task.id));
    }
    Ok(())
}
```

- [ ] **Step 4: Wire `builtin()` and `ID` into `fixture/mod.rs`**

Replace the module doc (lines 1-6 of `mod.rs`) with:

```rust
//! Graded probe sets: the compiled-in fixture-v1 (`builtin`, embedded by
//! `build.rs`, graded through codebase mode with its held-out tests) and a
//! user-supplied probe-set TOML (`load`).
//!
//! fixture-v1 is release-gated (capability-spec §9): it ships, but no
//! published number rests on it until it has been measured against three
//! models of clearly different capability with the spread published.
```

Under `pub mod embedded;` add:

```rust
pub mod manifest;

/// The compiled-in fixture's id — in every corpus id it produces.
pub const ID: &str = "fixture-v1";

/// The embedded manifest, parsed and checked against the embedded bytes.
pub fn builtin() -> Result<manifest::Manifest, ChekovError> {
    let text = embedded::FILES
        .iter()
        .find(|(p, _)| *p == manifest::MANIFEST_PATH)
        .map(|(_, t)| *t)
        .ok_or_else(|| manifest::invalid("manifest.toml is not embedded".to_owned()))?;
    manifest::parse(text, embedded::FILES)
}
```

Add to the existing test module in `mod.rs`:

```rust
    #[test]
    fn the_embedded_manifest_parses_and_declares_the_real_content_hash() {
        let computed = super::manifest::content_hash(super::embedded::FILES);
        let manifest = super::builtin()
            .unwrap_or_else(|e| panic!("{e}\n\nwrite content_hash = \"{computed}\" into fixtures/fixture-v1/manifest.toml"));
        assert_eq!(manifest.id, super::ID);
        assert_eq!(manifest.tasks.len(), 4);
        let symbols: Vec<&str> = manifest.tasks.iter().map(|t| t.symbol.as_str()).collect();
        assert_eq!(symbols, ["LimitedStore::record", "handle_credit", "from_str", "replay_filtered"]);
    }
```

- [ ] **Step 5: Run, read the computed hash off the failure, write it in**

Run: `cargo test --locked fixture::tests::the_embedded_manifest_parses`
Expected: FAIL, the panic message ends with `write content_hash = "<64 hex>" into fixtures/fixture-v1/manifest.toml`.

Edit `fixtures/fixture-v1/manifest.toml:32` from `content_hash = "pending-materialization"` to `content_hash = "<that value>"`. Because `manifest.toml` is excluded from the hash, this edit does not move the value.

- [ ] **Step 6: Run everything in the module and see it pass**

Run: `cargo test --locked fixture::`
Expected: all pass, including the four pre-existing `load` tests and the manifest tests.

- [ ] **Step 7: Lint, then commit**

Run: `make lint && make test`
Expected: exit 0.

```bash
git add src/core/bench/fixture fixtures/fixture-v1/manifest.toml
git commit -m "feat(fixture): strict manifest contract with a verified content hash"
```

---

### Task 4: Materialize the visible tree into a scratch git repo

**Files:**
- Modify: `src/core/bench/codebase/tree.rs` (add `init_and_commit` after `head_sha`, line 63)
- Create: `src/core/bench/fixture/materialize.rs`
- Modify: `src/core/bench/fixture/mod.rs` (`pub mod materialize;`)

**Interfaces:**
- Produces:
  - `tree::init_and_commit(repo: &Path, message: &str) -> Result<(), ChekovError>` (`pub(crate)`)
  - `fixture::materialize::Materialized { repo: PathBuf, hidden: Vec<(String, String)> }` — `hidden` holds `(embedded path such as "hidden/near_miss_api.rs", text)`.
  - `fixture::materialize::materialize(scratch_root: &Path, hash12: &str) -> Result<Materialized, ChekovError>`
- Consumes: `embedded::FILES`, `tree::head_sha`.

- [ ] **Step 1: Write the failing materialize tests**

Create `src/core/bench/fixture/materialize.rs`:

```rust
//! The visible fixture tree on disk as a committed git repository, so the
//! codebase-mode pipeline can check it out like a user's repository. The
//! hidden tests never touch this tree: they come back in memory.

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::materialize;
    use crate::core::bench::codebase::tree;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("chekov-test-materialize").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        dir
    }

    #[test]
    fn the_tree_has_the_crate_a_head_and_no_hidden_directory() {
        let root = scratch("tree");
        let m = materialize(&root, "0123456789ab").expect("materialize");
        assert_eq!(m.repo, root.join("fixture-v1-0123456789ab"));
        assert!(m.repo.join("Cargo.toml").exists());
        assert!(m.repo.join("src/api/mod.rs").exists());
        assert!(m.repo.join("manifest.toml").exists());
        assert!(!m.repo.join("hidden").exists(), "hidden tests are never on disk here");
        tree::assert_clean(&m.repo).expect("committed, nothing untracked");
        tree::head_sha(&m.repo).expect("a HEAD");
        assert_eq!(m.hidden.len(), 4);
        assert!(m.hidden.iter().all(|(p, _)| p.starts_with("hidden/")));
    }

    #[test]
    fn a_second_call_reuses_the_repository_and_its_head() {
        let root = scratch("reuse");
        let first = materialize(&root, "0123456789ab").expect("first");
        let head = tree::head_sha(&first.repo).expect("head");
        let second = materialize(&root, "0123456789ab").expect("second");
        assert_eq!(tree::head_sha(&second.repo).expect("head"), head);
        tree::assert_clean(&second.repo).expect("still clean");
    }
}
```

- [ ] **Step 2: Run to see them fail**

Run: `cargo test --locked materialize::tests`
Expected: compile error — `materialize` undefined (add `pub mod materialize;` to `fixture/mod.rs` first so the file is compiled).

- [ ] **Step 3: Add `init_and_commit` to `tree.rs`**

Insert after `head_sha` (after line 63):

```rust
/// `git init`, add everything, one commit — for a tree chekov wrote itself
/// (the compiled-in fixture), so `assert_clean` and `head_sha` hold for it
/// exactly as for a user's repository.
pub(crate) fn init_and_commit(repo: &Path, message: &str) -> Result<(), ChekovError> {
    let who = ["-c", "user.email=chekov@localhost", "-c", "user.name=chekov"];
    git(repo, &["init", "-q"], "git init")?;
    git(repo, &[&who[..], &["add", "-A"]].concat(), "git add")?;
    git(
        repo,
        &[&who[..], &["commit", "-q", "-m", message]].concat(),
        "git commit",
    )?;
    Ok(())
}
```

- [ ] **Step 4: Implement `materialize`**

Insert above the test module in `materialize.rs`:

```rust
use std::path::{Path, PathBuf};

use super::embedded::FILES;
use crate::core::bench::codebase::tree;
use crate::error::ChekovError;

const HIDDEN_DIR: &str = "hidden/";

/// Where the visible files went, and the hidden files that did not.
pub struct Materialized {
    pub repo: PathBuf,
    /// `(embedded path, text)` for every file under `hidden/`.
    pub hidden: Vec<(String, String)>,
}

/// Write every embedded file but `hidden/*` under
/// `<scratch_root>/fixture-v1-<hash12>/` and commit it. Keyed by the content
/// hash, so a second run of the same fixture reuses the repository, and a
/// different fixture never shares one.
pub fn materialize(scratch_root: &Path, hash12: &str) -> Result<Materialized, ChekovError> {
    let repo = scratch_root.join(format!("fixture-v1-{hash12}"));
    let mut hidden = Vec::new();
    for (path, text) in FILES {
        if path.starts_with(HIDDEN_DIR) {
            hidden.push(((*path).to_owned(), (*text).to_owned()));
            continue;
        }
        write_file(&repo.join(path), text)?;
    }
    if !repo.join(".git").exists() {
        tree::init_and_commit(&repo, "fixture-v1 materialized")?;
    }
    Ok(Materialized { repo, hidden })
}

fn write_file(dest: &Path, text: &str) -> Result<(), ChekovError> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ChekovError::io(format!("creating {}", parent.display()), e))?;
    }
    std::fs::write(dest, text).map_err(|e| ChekovError::io(format!("writing {}", dest.display()), e))
}
```

- [ ] **Step 5: Run to see them pass**

Run: `cargo test --locked materialize::tests`
Expected: 2 passed.

- [ ] **Step 6: Lint and commit**

Run: `make lint && make test`
Expected: exit 0.

```bash
git add src/core/bench/fixture src/core/bench/codebase/tree.rs
git commit -m "feat(fixture): materialize the visible tree as a committed scratch repo"
```

---

### Task 5: Tasks named by the manifest

**Files:**
- Create: `src/core/bench/codebase/named.rs`
- Modify: `src/core/bench/codebase/mod.rs` (`pub mod named;`, `HiddenTest`, `Prepared` fields at 148-163, `prepare` at 317-365 split into `walk`/`finish`, `prepare_named`, `into_prepared` at 382-404)
- Modify: `src/core/bench/codebase/run.rs:593-612` and the other `Prepared {` literals in that file's tests (`prepared_cross`, `prepared_two_cross`)
- Modify: `src/commands/capability.rs:3622-3640` (`prepared_counts`)
- Modify: `src/core/bench/fixture/mod.rs` (`named_tasks`)

**Interfaces:**
- Produces:
  - `codebase::HiddenTest { task_id: String, file: String, text: String }`
  - `Prepared.hidden: Vec<HiddenTest>` and `Prepared.corpus: Option<String>` (both empty/`None` from `prepare`)
  - `codebase::named::NamedTask { id, file, symbol }`, `NamedTasks { tasks: Vec<NamedTask>, hidden: Vec<HiddenTest>, corpus: String }`
  - `codebase::named::pick(files: &[sample::FileCandidates], texts: &[(String, String)], tasks: &[NamedTask]) -> Result<sample::TaskSet, ChekovError>`
  - `codebase::prepare_named(repo: &Path, named: &NamedTasks, inputs: &PrepareInputs) -> Result<Prepared, ChekovError>`
  - `fixture::named_tasks(manifest: &Manifest, hidden_files: &[(String, String)]) -> Result<NamedTasks, ChekovError>`
- Consumes: `masker::enclosing_fn(text, at) -> Option<String>` (`pub(super)`, visible to `named` as a child of `codebase`), `masker::matching_close(text, open) -> Option<usize>` (`pub(crate)`), `sample::{FileCandidates, Picked, Lane, TaskSet}`, `ChekovError::CodebaseNoTasks { path: PathBuf, reason: String }`.

- [ ] **Step 1: Write the failing `named::pick` tests**

Create `src/core/bench/codebase/named.rs`:

```rust
//! Tasks named by a manifest instead of sampled by seed — how the compiled-in
//! fixture's devices reach the codebase pipeline.

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
    y
}
";

    fn files() -> (Vec<FileCandidates>, Vec<(String, String)>) {
        let candidates = RustBraceMasker.candidates(STORE);
        (
            vec![FileCandidates { path: "src/store.rs".into(), candidates }],
            vec![("src/store.rs".into(), STORE.into())],
        )
    }

    fn task(id: &str, symbol: &str) -> NamedTask {
        NamedTask { id: id.into(), file: "src/store.rs".into(), symbol: symbol.into() }
    }

    #[test]
    fn an_owner_qualified_name_picks_exactly_that_impls_body() {
        let (files, texts) = files();
        let set = pick(&files, &texts, &[task("d1", "LimitedStore::record"), task("d2", "handle_credit")])
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
        let twice = pick(&files, &texts, &[task("d1", "record")]).expect_err("ambiguous").to_string();
        assert!(twice.contains("d1") && twice.contains("2 function bodies named record"), "{twice}");
        let none = pick(&files, &texts, &[task("d9", "nope")]).expect_err("absent").to_string();
        assert!(none.contains("0 function bodies named nope"), "{none}");
        let wrong_file = pick(&files, &texts, &[NamedTask { id: "d1".into(), file: "src/other.rs".into(), symbol: "record".into() }])
            .expect_err("file");
        assert!(wrong_file.to_string().contains("src/other.rs"));
    }
}
```

- [ ] **Step 2: Run to see them fail**

Add `pub mod named;` to `codebase/mod.rs` next to the other `pub mod` lines, then:

Run: `cargo test --locked named::tests`
Expected: compile error — `NamedTask`, `pick` undefined.

- [ ] **Step 3: Implement `named.rs`**

Insert above the test module:

```rust
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
    let text = texts.iter().find(|(p, _)| *p == task.file).map(|(_, t)| t.as_str());
    let spans = files.iter().find(|f| f.path == task.file).map(|f| f.candidates.as_slice());
    let (Some(text), Some(spans)) = (text, spans) else {
        return Err(missing(task, 0));
    };
    let (owner, name) = split_symbol(&task.symbol);
    let found: Vec<&Candidate> = spans
        .iter()
        .filter(|c| c.tier == TaskTier::FunctionBody)
        .filter(|c| masker::enclosing_fn(text, c.byte_range.start).as_deref() == Some(name))
        .filter(|c| owner.is_none_or(|o| impl_header_around(text, c.byte_range.start).is_some_and(|h| names(h, o))))
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
```

- [ ] **Step 4: Run to see them pass**

Run: `cargo test --locked named::tests`
Expected: 2 passed.

- [ ] **Step 5: Add `HiddenTest` and the two `Prepared` fields**

In `codebase/mod.rs`, after `ExtraFile` (line 83) add:

```rust
/// A held-out test the grader writes into the crate for tier 7 and removes
/// after — never on disk while a prompt is assembled, never in a prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HiddenTest {
    pub task_id: String,
    /// The embedded path (`hidden/near_miss_api.rs`); its stem names the test.
    pub file: String,
    pub text: String,
}
```

In `Prepared` (after `pub exec: exec::Exec,`) add:

```rust
    /// The compiled-in fixture's held-out tests, keyed by task id — empty on
    /// a user's repository.
    pub hidden: Vec<HiddenTest>,
    /// The corpus id the head records when the tasks came from a manifest;
    /// `None` when they were sampled, and the head derives it from HEAD.
    pub corpus: Option<String>,
```

In `into_prepared` add `hidden: Vec::new(), corpus: None,` to the `Prepared { … }` literal.

Add `hidden: vec![], corpus: None,` to every `Prepared {` literal in tests: `run.rs` (`prepared_pair`, `prepared_cross`, `prepared_two_cross`) and `capability.rs` (`prepared_counts`). `cargo build --tests` names any you missed.

- [ ] **Step 6: Split `prepare` into `walk` + `finish`, add `prepare_named`**

Replace `prepare` (lines 317-365) with:

```rust
/// What the walk produced, before any task was chosen.
struct Walked {
    head: String,
    worktree: tree::Worktree,
    scanned: usize,
    oversized: usize,
    elided: Elisions,
    candidates: Candidates,
    symbols: ladder::Symbols,
}

/// Gate, worktree, walk, mask, index, symbol set — everything both entry
/// points share. The scratch tree is `<scratch_root>/codebase-tree-<head12>`,
/// keyed by the HEAD it checks out.
fn walk(repo: &Path, inputs: &PrepareInputs) -> Result<Walked, ChekovError> {
    tree::assert_clean(repo)?;
    let head = tree::head_sha(repo)?;
    let scratch_tree = inputs
        .scratch_root
        .join(format!("codebase-tree-{}", head12(&head)));
    let worktree = tree::Worktree::add(repo, &scratch_tree)?;
    let sources = tree::rust_sources(&worktree.path);
    let elided = elide_tests(sources.files);
    let index = crossfile::Index::build(&elided.files);
    let candidates = all_candidates(&index, &elided);
    let symbols = ladder::repo_symbols(&elided.files);
    Ok(Walked {
        head,
        worktree,
        scanned: sources.scanned,
        oversized: sources.oversized,
        elided,
        candidates,
        symbols,
    })
}

/// Walk, sample, assemble — then the worktree is removed, unless
/// `--allow-exec` keeps it for the exec tiers. Everything the run needs is
/// in memory, and the user's checkout was never read directly.
pub fn prepare(repo: &Path, inputs: &PrepareInputs) -> Result<Prepared, ChekovError> {
    let mut w = walk(repo, inputs)?;
    let set = sample::sample(
        std::mem::take(&mut w.candidates.per_file),
        sample::quota(inputs.tasks),
        sample::seed_from_head(&w.head),
    );
    if set.picked.is_empty() {
        let reason = format!(
            "scanned {} files, {} eligible, 0 candidate spans",
            w.scanned,
            w.elided.files.len()
        );
        w.worktree.remove()?;
        return Err(ChekovError::CodebaseNoTasks {
            path: repo.to_path_buf(),
            reason,
        });
    }
    finish(w, set, inputs)
}

/// The same walk, with the tasks named by a manifest instead of sampled —
/// the compiled-in fixture's entry point. A name that does not resolve
/// removes the worktree and refuses.
pub fn prepare_named(
    repo: &Path,
    named: &named::NamedTasks,
    inputs: &PrepareInputs,
) -> Result<Prepared, ChekovError> {
    let w = walk(repo, inputs)?;
    let set = match named::pick(&w.candidates.per_file, &w.elided.files, &named.tasks) {
        Ok(set) => set,
        Err(e) => {
            w.worktree.remove()?;
            return Err(e);
        }
    };
    let mut prepared = finish(w, set, inputs)?;
    prepared.hidden.clone_from(&named.hidden);
    prepared.corpus = Some(named.corpus.clone());
    Ok(prepared)
}

fn finish(w: Walked, set: sample::TaskSet, inputs: &PrepareInputs) -> Result<Prepared, ChekovError> {
    let short = head12(&w.head).to_owned();
    let exec = exec_state(w.worktree, inputs, &short)?;
    Ok(into_prepared(Sampled {
        head: w.head,
        set,
        elided: w.elided,
        candidates: w.candidates,
        symbols: w.symbols,
        oversized: w.oversized,
        exec,
    }))
}
```

- [ ] **Step 7: Write the failing `prepare_named` test**

In `codebase/mod.rs`'s test module (it already has `repo_fixture(name)` building a committed two-file repo whose `src/alpha.rs` and `src/beta.rs` come from `source("alpha")` — read `source` at `mod.rs:515-548` to pick a fn name it defines; the steps below assume it defines `pub fn alpha_two(...)` with a multi-line body — substitute the real name):

```rust
    #[test]
    fn prepare_named_picks_the_named_bodies_and_carries_the_hidden_tests() {
        let (repo, root) = repo_fixture("named");
        let named = super::named::NamedTasks {
            tasks: vec![super::named::NamedTask {
                id: "device-x".into(),
                file: "src/alpha.rs".into(),
                symbol: "alpha_two".into(),
            }],
            hidden: vec![super::HiddenTest {
                task_id: "device-x".into(),
                file: "hidden/x.rs".into(),
                text: "#[test]\nfn x() {}\n".into(),
            }],
            corpus: "fixture-v1:0123456789ab".into(),
        };
        let prepared = super::prepare_named(
            &repo,
            &named,
            &super::PrepareInputs { scratch_root: &root.join("scratch"), tasks: 0, allow_exec: false },
        )
        .expect("prepare_named");
        assert_eq!(prepared.tasks.len(), 1);
        assert_eq!(prepared.tasks[0].id, "device-x");
        assert_eq!(prepared.tasks[0].tier, super::TaskTier::FunctionBody);
        assert_eq!(prepared.counts.function_body, 1);
        assert_eq!(prepared.hidden, named.hidden);
        assert_eq!(prepared.corpus.as_deref(), Some("fixture-v1:0123456789ab"));
        for task in &prepared.tasks {
            assert!(!task.prefix.contains("fn x()") && !task.suffix.contains("fn x()"), "hidden text in a prompt");
        }
        let absent = super::prepare_named(
            &repo,
            &super::named::NamedTasks { tasks: vec![super::named::NamedTask { id: "d".into(), file: "src/alpha.rs".into(), symbol: "nope".into() }], hidden: vec![], corpus: String::new() },
            &super::PrepareInputs { scratch_root: &root.join("scratch"), tasks: 0, allow_exec: false },
        );
        assert!(absent.is_err(), "an unresolved name refuses");
    }
```

- [ ] **Step 8: Run the codebase tests**

Run: `cargo test --locked codebase::`
Expected: all pass, including the new one and every pre-existing `prepare` test (the split is behaviour-preserving).

- [ ] **Step 9: Add `fixture::named_tasks`**

In `src/core/bench/fixture/mod.rs`:

```rust
use crate::core::bench::codebase::HiddenTest;
use crate::core::bench::codebase::named::{NamedTask, NamedTasks};

/// The manifest's tasks as codebase-mode named tasks, each with its held-out
/// test's text — refused when a named hidden file was not materialized.
pub fn named_tasks(
    manifest: &manifest::Manifest,
    hidden_files: &[(String, String)],
) -> Result<NamedTasks, ChekovError> {
    let mut hidden = Vec::with_capacity(manifest.tasks.len());
    for task in &manifest.tasks {
        let text = hidden_files
            .iter()
            .find(|(p, _)| *p == task.hidden)
            .map(|(_, t)| t.clone())
            .ok_or_else(|| {
                manifest::invalid(format!("task {}: hidden {} was not materialized", task.id, task.hidden))
            })?;
        hidden.push(HiddenTest { task_id: task.id.clone(), file: task.hidden.clone(), text });
    }
    let tasks = manifest
        .tasks
        .iter()
        .map(|t| NamedTask { id: t.id.clone(), file: t.source.clone(), symbol: t.symbol.clone() })
        .collect();
    let corpus = format!("{ID}:{}", &manifest.content_hash[..12]);
    Ok(NamedTasks { tasks, hidden, corpus })
}
```

And a test in the same module:

```rust
    #[test]
    fn named_tasks_pair_every_manifest_task_with_its_hidden_text() {
        let manifest = super::builtin().expect("manifest");
        let hidden: Vec<(String, String)> = super::embedded::FILES
            .iter()
            .filter(|(p, _)| p.starts_with("hidden/"))
            .map(|(p, t)| ((*p).to_owned(), (*t).to_owned()))
            .collect();
        let named = super::named_tasks(&manifest, &hidden).expect("named");
        assert_eq!(named.tasks.len(), 4);
        assert_eq!(named.hidden.len(), 4);
        assert!(named.hidden.iter().all(|h| h.text.contains("#[test]")));
        assert!(named.corpus.starts_with("fixture-v1:") && named.corpus.len() == "fixture-v1:".len() + 12);
        let err = super::named_tasks(&manifest, &[]).expect_err("nothing materialized").to_string();
        assert!(err.contains("was not materialized"), "{err}");
    }
```

- [ ] **Step 10: Lint, test, commit**

Run: `make lint && make test`
Expected: exit 0.

```bash
git add src/core/bench/codebase src/core/bench/fixture src/commands/capability.rs
git commit -m "feat(codebase): prepare_named — tasks named by the fixture manifest"
```

---

### Task 6: Tier 7 with the hidden test injected

**Files:**
- Modify: `src/core/bench/codebase/exec.rs:752-836` (`exec_crossing`, `tiers`, `test_tier`) and its test module
- Modify: `src/core/bench/codebase/run.rs:401-419` (`exec_row`)

**Interfaces:**
- Produces:
  - `exec::Crossing<'a> { task: &'a CodebaseTask, fill: &'a str, hidden: Option<&'a HiddenTest> }`
  - `exec::exec_crossing_with(env: &Env, crossing: &Crossing) -> Result<ExecRow, ChekovError>`
  - `exec::exec_crossing(env, task, fill)` unchanged in signature (now a wrapper) — `tests/codebase_exec.rs` keeps compiling.
- Consumes: `codebase::HiddenTest` (Task 5), `Prepared.hidden`.

- [ ] **Step 1: Write the failing injection tests**

In `exec.rs`'s test module (which already has `scratch`, `fake_cargo`, `git`, `repo` helpers), add:

```rust
    use super::super::{CodebaseTask, Excluded, HiddenTest, TaskTier};
    use super::{Crossing, Env, exec_crossing_with};

    /// A `CodebaseTask` masking `needle` in the worktree's `src/lib.rs`.
    fn task_masking(worktree: &Path, needle: &str) -> CodebaseTask {
        let text = std::fs::read_to_string(worktree.join("src/lib.rs")).expect("lib.rs");
        let start = text.find(needle).expect("needle");
        CodebaseTask {
            id: "device-t".into(),
            tier: TaskTier::FunctionBody,
            file: "src/lib.rs".into(),
            line: 1,
            byte_range: start..start + needle.len(),
            gold: needle.into(),
            prefix: text[..start].into(),
            suffix: text[start + needle.len()..].into(),
            excluded: Excluded::default(),
            name: None,
            also_first_uses: vec![],
            extra: None,
            extra_text: String::new(),
        }
    }

    fn env_with(name: &str, cargo_body: &str, timeouts: Timeouts) -> (PathBuf, Env) {
        let dir = scratch(name);
        let repo = repo(name);
        let worktree = Worktree::add(&repo, &dir.join("wt")).expect("worktree");
        let cargo = fake_cargo(&dir, cargo_body);
        let env = Env {
            worktree,
            cargo,
            target_dir: dir.join("target"),
            cargo_version: "fake".into(),
            timeouts,
        };
        (dir, env)
    }

    fn hidden() -> HiddenTest {
        HiddenTest {
            task_id: "device-t".into(),
            file: "hidden/near_miss_api.rs".into(),
            text: "#[test]\nfn pins() {}\n".into(),
        }
    }

    #[test]
    fn the_hidden_test_is_on_disk_only_for_the_cargo_test_run_and_named_on_the_row() {
        let body = "if [ \"$1\" = \"check\" ]; then exit 0; fi\n\
                    ls tests > \"$(dirname \"$0\")/seen.txt\"\n\
                    echo \"$@\" >> \"$(dirname \"$0\")/seen.txt\"\n\
                    exit 0";
        let (dir, env) = env_with("inject", body, Timeouts::DEFAULT);
        std::fs::create_dir_all(dir.join("target")).expect("target");
        let task = task_masking(&env.worktree.path, "a()");
        let row = exec_crossing_with(&env, &Crossing { task: &task, fill: "a()", hidden: Some(&hidden()) })
            .expect("the crossing runs");
        let seen = std::fs::read_to_string(dir.join("seen.txt")).expect("the fake cargo ran the test");
        assert!(seen.contains("near_miss_api.rs"), "present during cargo test: {seen}");
        assert!(seen.contains("--test near_miss_api"), "run by stem: {seen}");
        assert!(!env.worktree.path.join("tests/near_miss_api.rs").exists(), "removed after");
        assert_eq!(row.tests, vec!["near_miss_api".to_owned()]);
        assert_eq!(row.test, ExecScore::Value(1.0));
    }

    #[test]
    fn a_failing_hidden_test_scores_zero_and_a_timeout_still_removes_it() {
        let (_, env) = env_with("inject-fail", "if [ \"$1\" = \"check\" ]; then exit 0; fi\necho 'test pins ... FAILED'\nexit 101", Timeouts::DEFAULT);
        let task = task_masking(&env.worktree.path, "a()");
        let row = exec_crossing_with(&env, &Crossing { task: &task, fill: "a()", hidden: Some(&hidden()) }).expect("runs");
        assert_eq!(row.test, ExecScore::Value(0.0));
        assert!(row.test_failure.as_deref().is_some_and(|f| f.contains("FAILED")));
        let (_, env) = env_with(
            "inject-timeout",
            "if [ \"$1\" = \"check\" ]; then exit 0; fi\nsleep 30 &\nwait",
            Timeouts { test: Duration::from_secs(1), ..Timeouts::DEFAULT },
        );
        let task = task_masking(&env.worktree.path, "a()");
        let row = exec_crossing_with(&env, &Crossing { task: &task, fill: "a()", hidden: Some(&hidden()) }).expect("runs");
        assert!(matches!(row.test, ExecScore::Skipped(ref r) if r.contains("timed out")), "{:?}", row.test);
        assert!(!env.worktree.path.join("tests/near_miss_api.rs").exists(), "removed on the timeout path too");
    }

    #[test]
    fn without_a_hidden_test_the_wrapper_is_the_old_crossing() {
        let (_, env) = env_with("no-hidden", "exit 0", Timeouts::DEFAULT);
        let task = task_masking(&env.worktree.path, "a()");
        let row = super::exec_crossing(&env, &task, "a()").expect("runs");
        assert!(matches!(row.test, ExecScore::Skipped(_)), "no covering test in the one-file crate");
    }
```

Check `Timeouts`'s fields at `exec.rs:18-34` — if it has more than `check` and `test`, the struct-update form above still compiles; if `test` is named differently, use that name.

- [ ] **Step 2: Run to see them fail**

Run: `cargo test --locked exec::tests::the_hidden_test`
Expected: compile error — `Crossing`, `exec_crossing_with` undefined.

- [ ] **Step 3: Implement the injection**

Replace `exec_crossing` and `tiers` (lines 752-796) and `test_tier` (858-875) with:

```rust
/// One crossing's inputs beyond the environment (§4): the task, the fill,
/// and — for the compiled-in fixture — the held-out test tier 7 injects.
pub struct Crossing<'a> {
    pub task: &'a CodebaseTask,
    pub fill: &'a str,
    pub hidden: Option<&'a HiddenTest>,
}

/// One crossing's tiers 6 and 7 against the crate's own covering tests.
pub fn exec_crossing(env: &Env, task: &CodebaseTask, fill: &str) -> Result<ExecRow, ChekovError> {
    exec_crossing_with(env, &Crossing { task, fill, hidden: None })
}

/// Splice, check, tests, revert. The revert runs whatever the tiers decided,
/// and its failure is the run's abort. Nothing here returns an `Err` for a
/// cargo outcome — a timeout, an offline registry, an unbuildable test module
/// are all skips with reasons — but a hidden test that cannot be written or
/// removed is an `io` error attributed to its path, never a silent skip.
pub fn exec_crossing_with(env: &Env, crossing: &Crossing) -> Result<ExecRow, ChekovError> {
    let task = crossing.task;
    let path = env.worktree.path.join(&task.file);
    let original = std::fs::read_to_string(&path)
        .map_err(|e| ChekovError::io(format!("reading {}", path.display()), e))?;
    apply(
        &Splice {
            path: &path,
            original: &original,
            span: task.byte_range.clone(),
        },
        crossing.fill,
    )?;
    let row = tiers(
        env,
        &Graded {
            task,
            original: &original,
            hidden: crossing.hidden,
        },
    );
    revert(env, &task.file, &original)?;
    row
}

/// What the tiers read after the splice (§4).
struct Graded<'a> {
    task: &'a CodebaseTask,
    original: &'a str,
    hidden: Option<&'a HiddenTest>,
}

/// Tier 6, and tier 7 only if tier 6 passed.
fn tiers(env: &Env, g: &Graded) -> Result<ExecRow, ChekovError> {
    let (compile, compile_error, check_secs) = check_tier(env);
    let mut row = ExecRow {
        compile,
        compile_error,
        tests: Vec::new(),
        test: ExecScore::Skipped(DID_NOT_COMPILE.to_owned()),
        test_failure: None,
        check_secs,
        test_secs: 0.0,
    };
    if row.compile != ExecScore::Value(1.0) {
        return Ok(row);
    }
    let seven = match g.hidden {
        Some(hidden) => injected_tier(env, g.task, hidden)?,
        None => test_tier(env, g.task, g.original),
    };
    row.tests = seven.tests;
    row.test = seven.score;
    row.test_failure = seven.failure;
    row.test_secs = seven.secs;
    Ok(row)
}

/// Tier 7 with the fixture's held-out test written into `tests/` for exactly
/// one `cargo test --test <stem>`, then removed — on the timeout path too.
fn injected_tier(env: &Env, task: &CodebaseTask, hidden: &HiddenTest) -> Result<Seven, ChekovError> {
    let Some(krate) = crate_of(&env.worktree.path, &task.file) else {
        return Ok(Seven::skipped(NO_CRATE));
    };
    let Some(stem) = Path::new(&hidden.file).file_stem().map(|s| s.to_string_lossy().into_owned()) else {
        return Ok(Seven::skipped("hidden test has no file name"));
    };
    let dest = env.worktree.path.join("tests").join(format!("{stem}.rs"));
    write_hidden(&dest, &hidden.text)?;
    let (verdict, secs) = integration_test(env, &krate.name, &stem);
    std::fs::remove_file(&dest)
        .map_err(|e| ChekovError::io(format!("removing {}", dest.display()), e))?;
    Ok(seven_from(verdict, vec![stem], secs))
}

fn write_hidden(dest: &Path, text: &str) -> Result<(), ChekovError> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ChekovError::io(format!("creating {}", parent.display()), e))?;
    }
    std::fs::write(dest, text).map_err(|e| ChekovError::io(format!("writing {}", dest.display()), e))
}

/// `cargo test -p <crate> --test <stem> --offline`: the one integration test
/// file the hidden test became.
fn integration_test(env: &Env, krate: &str, stem: &str) -> (TestVerdict, f64) {
    let timeout = env.timeouts.test;
    let outcome = run_cargo(&CargoRun {
        program: &env.cargo,
        args: &["test", "-p", krate, "--test", stem, "--offline"],
        cwd: &env.worktree.path,
        target_dir: &env.target_dir,
        timeout,
    });
    let Ok(outcome) = outcome else {
        return (
            TestVerdict::Skipped(format!("cargo test failed to run: {stem}")),
            0.0,
        );
    };
    (test_verdict(&outcome, stem, timeout), outcome.secs)
}

/// The symbol, the crate, the candidates, the run — tier 7 against the
/// crate's own covering tests.
fn test_tier(env: &Env, task: &CodebaseTask, original: &str) -> Seven {
    let Some(symbols) = tier_seven_symbols(task, original) else {
        return Seven::skipped(NO_ENCLOSING_FN);
    };
    let Some(krate) = crate_of(&env.worktree.path, &task.file) else {
        return Seven::skipped(NO_CRATE);
    };
    let tests = covering_tests(&krate.root, &symbols);
    if tests.is_empty() {
        return Seven::skipped(NO_COVERING_TEST);
    }
    let (verdict, secs) = run_tests(&TestRun {
        env,
        krate: &krate.name,
        tests: &tests,
    });
    seven_from(verdict, tests, secs)
}
```

Add `use super::HiddenTest;` beside the existing `use super::CodebaseTask;` (line 741).

- [ ] **Step 4: Thread the hidden test through `run.rs`**

Replace the `Ready` arm of `exec_row` (run.rs:412-417) with:

```rust
        super::exec::Exec::Ready(env) => {
            let fill = ladder::trimmed_to_gold(&task.gold, prediction);
            let hidden = prepared.hidden.iter().find(|h| h.task_id == task.id);
            let row = super::exec::exec_crossing_with(
                env,
                &super::exec::Crossing {
                    task,
                    fill: &fill,
                    hidden,
                },
            )?;
            parts.timing.borrow_mut().record(row.check_secs);
            Ok(Some(row))
        }
```

- [ ] **Step 5: Run to see them pass**

Run: `cargo test --locked exec::tests`
Expected: all pass, the three new ones included.

- [ ] **Step 6: Lint, test, commit**

Run: `make lint && make test`
Expected: exit 0.

```bash
git add src/core/bench/codebase/exec.rs src/core/bench/codebase/run.rs
git commit -m "feat(codebase): tier 7 injects the fixture's held-out test for one cargo test"
```

---

### Task 7: The CLI: bare `--fixture`, the corpus id, the plan line

**Files:**
- Modify: `src/commands/capability.rs` — `BenchOpts::fixture` (122-125), `BenchArgs`/`bench_args`/`effective_suite` (903-961), `prepare_codebase` (973-988), `render_dry_run` (1167-1169), `head_inputs` (1378-1383), `CodebaseHead` (2450-2455), `head_corpus` (2541-2544), tests `bench_and_compare_parse` (2908-2911) and the three `CodebaseHead {` literals in tests (around 3569, 3667, 3680).

**Interfaces:**
- Produces: `capability::FixtureArg { Builtin, External(PathBuf) }`, `BenchArgs.builtin_fixture: bool`, `CodebaseHead.corpus: Option<&str>`.
- Consumes: `fixture::{builtin, ID, named_tasks, materialize::materialize}`, `codebase::{prepare_named, PrepareInputs}`, `Prepared.corpus`.

- [ ] **Step 1: Write the failing parse and args tests**

In `capability.rs`'s test module, next to `bench_and_compare_parse`:

```rust
    fn bench_opts(argv: &[&str]) -> super::BenchOpts {
        use clap::Parser;
        let cli = crate::cli::Cli::try_parse_from([&["chekov", "capability", "bench"][..], argv].concat())
            .expect("parses");
        match cli.cmd {
            crate::cli::Cmd::Capability(cap) => match cap.action {
                Some(super::CapAction::Bench(opts)) => opts,
                other => panic!("expected Bench, got {other:?}"),
            },
            _ => panic!("expected capability"),
        }
    }

    #[test]
    fn a_bare_fixture_is_the_compiled_in_one_and_a_path_is_external() {
        use super::FixtureArg;
        assert_eq!(bench_opts(&["--fixture"]).fixture, Some(FixtureArg::Builtin));
        assert_eq!(
            bench_opts(&["--fixture", "probes.toml"]).fixture,
            Some(FixtureArg::External("probes.toml".into()))
        );
        assert_eq!(bench_opts(&[]).fixture, None);
        let args = super::bench_args(&bench_opts(&["--fixture", "--allow-exec"])).expect("args");
        assert!(args.builtin_fixture);
        assert_eq!(args.fixture, None, "no external path");
        assert_eq!(args.suite, None, "the fixture is the whole run, like --codebase");
        let external = super::bench_args(&bench_opts(&["--fixture", "p.toml"])).expect("args");
        assert!(!external.builtin_fixture);
        assert_eq!(external.suite, Some(crate::core::bench::lifecycle::Suite::Throughput));
    }

    #[test]
    fn the_compiled_in_fixture_refuses_without_allow_exec_before_touching_disk() {
        let scratch = std::env::temp_dir().join("chekov-test-fixture-refuse");
        let _ = std::fs::remove_dir_all(&scratch);
        let err = super::prepare_fixture(&scratch, false).expect_err("refused").to_string();
        assert!(err.contains("--allow-exec"), "{err}");
        assert!(!scratch.exists(), "nothing materialized");
    }

    #[test]
    fn a_manifest_corpus_overrides_the_head_derived_id() {
        let head = super::CodebaseHead { head: "4818813deeaa1111", set_hash: "abcdef123456", allow_exec: true, cargo_version: None, corpus: Some("fixture-v1:0123456789ab") };
        assert_eq!(super::codebase_corpus(&head), "fixture-v1:0123456789ab");
        let sampled = super::CodebaseHead { corpus: None, ..head };
        assert_eq!(super::codebase_corpus(&sampled), "codebase:4818813deeaa:abcdef123456");
    }
```

Update `bench_and_compare_parse` line 2908-2911 to:

```rust
                    assert_eq!(
                        opts.fixture,
                        Some(super::FixtureArg::External("probes.toml".into()))
                    );
```

- [ ] **Step 2: Run to see them fail**

Run: `cargo test --locked capability::tests::a_bare_fixture`
Expected: compile errors — `FixtureArg`, `builtin_fixture`, `prepare_fixture`, `codebase_corpus`, `CodebaseHead.corpus` undefined.

- [ ] **Step 3: `FixtureArg` and the flag**

Above `BenchOpts` add:

```rust
/// What `--fixture` named: the compiled-in fixture-v1, or a probe-set path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FixtureArg {
    Builtin,
    External(std::path::PathBuf),
}

impl FixtureArg {
    /// The value clap fills in for a bare `--fixture`; not a legal path.
    pub const BUILTIN: &'static str = "builtin";

    pub fn external(&self) -> Option<&std::path::Path> {
        match self {
            Self::External(path) => Some(path),
            Self::Builtin => None,
        }
    }
}

impl std::str::FromStr for FixtureArg {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(if s == Self::BUILTIN {
            Self::Builtin
        } else {
            Self::External(s.into())
        })
    }
}
```

Replace `BenchOpts::fixture` (lines 122-125) with:

```rust
    /// Graded probes. Bare `--fixture` runs the compiled-in fixture-v1
    /// through codebase mode — its four masked bodies graded by held-out
    /// tests, so `--allow-exec` is required; release-gated on the
    /// three-model campaign (spec §9). `--fixture <PATH>` reads your own
    /// probe set (TOML) instead.
    #[arg(long, num_args = 0..=1, default_missing_value = FixtureArg::BUILTIN, value_name = "PATH")]
    pub fixture: Option<FixtureArg>,
```

If clap's derive rejects `Option<FixtureArg>` without an explicit parser, add `value_parser = clap::value_parser!(FixtureArg)` to the attribute.

- [ ] **Step 4: `BenchArgs`, `bench_args`, `effective_suite`**

Add to `BenchArgs` after `fixture`:

```rust
    /// Bare `--fixture`: the compiled-in fixture-v1 is the whole run.
    builtin_fixture: bool,
```

In `bench_args`:

```rust
        fixture: opts.fixture.as_ref().and_then(FixtureArg::external),
        builtin_fixture: matches!(opts.fixture, Some(FixtureArg::Builtin)),
        // ...
        suite: effective_suite(
            opts.suite,
            opts.codebase.is_some() || matches!(opts.fixture, Some(FixtureArg::Builtin)),
        ),
```

Rename `effective_suite`'s second parameter from `codebase` to `standalone` and its doc to: "`--suite` not passed means `throughput` — unless `--codebase` or a bare `--fixture` is given, in which case nothing beyond that set runs."

- [ ] **Step 5: `prepare_codebase` and `prepare_fixture`**

Replace `prepare_codebase` (973-988) with:

```rust
fn prepare_codebase(
    ctx: &Ctx,
    args: &BenchArgs,
) -> Result<Option<crate::core::bench::codebase::Prepared>, ChekovError> {
    use crate::core::bench::codebase;
    let scratch = ctx.config.eval_dir().join(".scratch");
    if let Some(repo) = args.codebase {
        return Ok(Some(codebase::prepare(
            repo,
            &codebase::PrepareInputs {
                scratch_root: &scratch,
                tasks: ctx.config.file.bench.codebase_tasks,
                allow_exec: args.allow_exec,
            },
        )?));
    }
    if args.builtin_fixture {
        return prepare_fixture(&scratch, args.allow_exec).map(Some);
    }
    Ok(None)
}

/// The compiled-in fixture: verified, materialized, its bodies named —
/// refused without `--allow-exec`, since the held-out tests ARE its grade.
fn prepare_fixture(
    scratch: &std::path::Path,
    allow_exec: bool,
) -> Result<crate::core::bench::codebase::Prepared, ChekovError> {
    use crate::core::bench::{codebase, fixture};
    if !allow_exec {
        return Err(ChekovError::FixtureInvalid {
            path: fixture::ID.into(),
            reason: "the compiled-in fixture is graded by its held-out tests (tiers 6-7), \
                     which run only under --allow-exec"
                .to_owned(),
        });
    }
    let manifest = fixture::builtin()?;
    let materialized = fixture::materialize::materialize(scratch, &manifest.content_hash[..12])?;
    let named = fixture::named_tasks(&manifest, &materialized.hidden)?;
    codebase::prepare_named(
        &materialized.repo,
        &named,
        &codebase::PrepareInputs {
            scratch_root: scratch,
            tasks: 0,
            allow_exec: true,
        },
    )
}
```

Keep the original doc comment about `<eval>/.scratch/` above `prepare_codebase`.

- [ ] **Step 6: The plan line and the corpus**

In `render_dry_run` replace lines 1167-1169 with:

```rust
    if let Some(p) = inputs.prepared {
        let repo = inputs
            .args
            .codebase
            .unwrap_or_else(|| std::path::Path::new("fixture-v1 (compiled in)"));
        out.push_str(&codebase_plan_line(p, repo, inputs.args.allow_exec));
    }
```

Add `corpus: Option<&'a str>,` to `CodebaseHead` with the doc `/// The manifest-given corpus id, when the tasks were named rather than sampled.` and set it in `head_inputs`: `corpus: p.corpus.as_deref(),`. Add `corpus: None,` to the three `CodebaseHead {` literals in the test module.

Add below `codebase_corpus_id`:

```rust
/// The corpus the head records for a codebase run: the manifest's own id
/// when there is one, otherwise derived from HEAD and the sampled set.
fn codebase_corpus(c: &CodebaseHead) -> String {
    c.corpus
        .map_or_else(|| codebase_corpus_id(c.head, c.set_hash), str::to_owned)
}
```

and in `head_corpus` change `Some(c) => codebase_corpus_id(c.head, c.set_hash),` to `Some(c) => codebase_corpus(c),`.

- [ ] **Step 7: Run to see them pass**

Run: `cargo test --locked capability::tests`
Expected: all pass, including `bench_and_compare_parse` and `codebase_and_fixture_conflict_and_suite_is_optional`.

- [ ] **Step 8: Lint, test, commit**

Run: `make lint && make test`
Expected: exit 0.

```bash
git add src/commands/capability.rs
git commit -m "feat(bench): bare --fixture runs the compiled-in fixture-v1 under --allow-exec"
```

---

### Task 8: Documentation

**Files:**
- Modify: `README.md:108` (the `capability bench` row) and add a row after `:109`
- Modify: `CHANGELOG.md` (`### Added` under `[Unreleased]`)
- Modify: `IDEAS.md` (a dated line under the 2026-09-18 follow-up)
- Modify: `fixtures/fixture-v1/README.md:5-7` and `fixtures/fixture-v1/src/lib.rs:5-10` (no longer "not wired into src/")

- [ ] **Step 1: README**

In the `capability bench` row (line 108) replace the clause "`--fixture` supplies graded probes from your own TOML (there is deliberately no compiled-in fixture)." with "`--fixture <PATH>` supplies graded probes from your own TOML; bare `--fixture` is the compiled-in fixture-v1 (next row)."

Add after the `--codebase` row (line 109):

```markdown
| `capability bench --fixture --allow-exec` | The compiled-in **fixture-v1**: a small event-sourced ledger crate embedded in the binary, run through codebase mode with four bodies named by its manifest — a cross-file capacity check, a near-miss API pair where the wrong call compiles, an `i128`-cents invariant a float path silently breaks, and a lifetime knot the compile gate settles. The held-out tests are never on disk while a prompt is assembled and never in any prompt; tier 7 writes each one into the scratch checkout for exactly one `cargo test` and removes it. `--allow-exec` is required because those tests are the grade. The run head's corpus id is `fixture-v1:<content hash>`, so a changed fixture never compares against an old run. **Release-gated** (spec §9): shipped, but no published number rests on it until it has been measured against three models of clearly different capability with the spread published. |
```

- [ ] **Step 2: CHANGELOG**

Under `## [Unreleased]` → `### Added`, add as the first bullet:

```markdown
- `chekov capability bench --fixture --allow-exec` runs the compiled-in
  fixture-v1 (embedded by `build.rs` from `fixtures/fixture-v1/`) through
  codebase mode: four bodies named by `manifest.toml` (`symbol`, with
  `Owner::name` where a name repeats), the leakage filter and exclusion
  counts unchanged, and tier 7 grading against held-out tests that are
  written into the scratch checkout for one `cargo test` and removed after.
  The manifest's `content_hash` is verified against the embedded bytes at
  every run; the corpus id is `fixture-v1:<hash>`. `--fixture <PATH>` keeps
  its external probe-set meaning. Release-gated per capability-spec §9.
```

- [ ] **Step 3: IDEAS and the fixture's own docs**

Append under the 2026-09-18 follow-up in `IDEAS.md`:

```markdown
2026-09-18, later: the assembler and grader shipped — spec
`docs/superpowers/specs/2026-09-18-fixture-v1-compiled-in-design.md`. The
fixture is compiled in; the release gate (three models, published spread) is
the remaining step, and decisions 2 and 3 above are still open.
```

In `fixtures/fixture-v1/README.md` lines 5-7 replace "and **not wired into `src/`** (see `AGENTS.md` scope discipline and `docs/capability-spec.md` §9)" with "embedded into the chekov binary by `build.rs` and run by `chekov capability bench --fixture --allow-exec` (`docs/capability-spec.md` §9)". In `src/lib.rs` lines 5-10 make the same change.

- [ ] **Step 4: Commit**

```bash
git add README.md CHANGELOG.md IDEAS.md fixtures/fixture-v1/README.md fixtures/fixture-v1/src/lib.rs
git commit -m "docs: the compiled-in fixture-v1 and its release gate"
```

Note: editing `fixtures/fixture-v1/src/lib.rs` changes the content hash. Re-run `cargo test --locked fixture::tests::the_embedded_manifest_parses`, write the new hash into `manifest.toml:32`, and include `fixtures/fixture-v1/manifest.toml` in this commit.

---

### Task 9: End-to-end verification against the real toolchain

**Files:**
- Modify: `src/core/bench/fixture/mod.rs` (one gated test)

- [ ] **Step 1: Write the real-cargo test, gated like `tests/codebase_exec.rs`**

```rust
    /// Device 2 end to end against a real `cargo`, once. Gated on
    /// `CHEKOV_TEST_EXEC=1` like `tests/codebase_exec.rs`.
    #[test]
    fn device_two_fails_the_hidden_test_on_the_near_miss_and_passes_on_the_gold() {
        if std::env::var("CHEKOV_TEST_EXEC").as_deref() != Ok("1") {
            eprintln!("skipping: set CHEKOV_TEST_EXEC=1 to run the fixture against a real cargo");
            return;
        }
        use crate::core::bench::codebase::{self, exec};
        let scratch = std::env::temp_dir().join("chekov-test-fixture-real");
        let _ = std::fs::remove_dir_all(&scratch);
        let manifest = super::builtin().expect("manifest");
        let m = super::materialize::materialize(&scratch, &manifest.content_hash[..12]).expect("materialize");
        let named = super::named_tasks(&manifest, &m.hidden).expect("named");
        let prepared = codebase::prepare_named(
            &m.repo,
            &named,
            &codebase::PrepareInputs { scratch_root: &scratch, tasks: 0, allow_exec: true },
        )
        .expect("prepare_named");
        let env = prepared.exec.env().expect("a toolchain");
        let task = prepared.tasks.iter().find(|t| t.id == "device-2-near-miss-api").expect("device 2");
        let hidden = prepared.hidden.iter().find(|h| h.task_id == task.id);
        let wrong = task.gold.replace("apply_entry", "append_entry");
        assert_ne!(wrong, task.gold, "the gold uses apply_entry");
        let row = exec::exec_crossing_with(env, &exec::Crossing { task, fill: &wrong, hidden }).expect("wrong");
        assert_eq!(row.compile, crate::core::bench::store::ExecScore::Value(1.0), "the near miss compiles: {:?}", row.compile_error);
        assert_eq!(row.test, crate::core::bench::store::ExecScore::Value(0.0), "and fails the hidden test: {:?}", row.test_failure);
        let row = exec::exec_crossing_with(env, &exec::Crossing { task, fill: &task.gold, hidden }).expect("gold");
        assert_eq!(row.test, crate::core::bench::store::ExecScore::Value(1.0), "{:?}", row.test_failure);
        prepared.exec.finish().expect("cleanup");
    }
```

- [ ] **Step 2: Run it for real**

Run: `CHEKOV_TEST_EXEC=1 cargo test --locked device_two_fails -- --nocapture`
Expected: PASS. If tier 6 reports `needs network`, the fixture's `Cargo.lock` is missing from `FILES` (check Task 2) — `--offline` needs it.

- [ ] **Step 3: The whole gate**

Run: `make lint && make test`
Expected: exit 0, test count above the pre-plan 935.

- [ ] **Step 4: A dry run through the binary**

Run: `cargo run --locked -- capability bench --fixture --allow-exec --dry-run`
Expected: the plan prints a line beginning `codebase: 4 tasks from fixture-v1 (compiled in) @ ` with `(0 in_file, 4 function_body, 0 cross_file_first)` and `+ exec:`; no server launches. If no model is registered it refuses at candidate resolution, which is fine — the fixture line prints first.

Run: `cargo run --locked -- capability bench --fixture`
Expected: exit non-zero, error text contains `--allow-exec`.

- [ ] **Step 5: Commit**

```bash
git add src/core/bench/fixture/mod.rs
git commit -m "test(fixture): device 2 end to end against a real cargo (gated)"
```

---

## Self-review notes

- Spec §4.1 (embedding) → Task 2; §4.2 (manifest) → Task 3; §4.3 (materialize) → Task 4; §4.4 (named tasks) → Task 5; §4.5 (grading) → Task 6; §4.6 (identity) and §4.7 (CLI) → Task 7; §7 tests → Tasks 2–7, 9; §8 files → all. The spec's `src/core/bench/codebase/named.rs` and `fixture/{manifest,materialize}.rs` paths hold. Deviations are recorded in the spec's Amendments section: rows keep suite `codebase`; git plumbing lives in `tree::init_and_commit`; the hidden test rides `Prepared.hidden` and an `exec::Crossing` bundle rather than a `CodebaseTask` field, because `tests/codebase_exec.rs` is protected and builds `CodebaseTask` by literal.
- Task 1 is the one thing the spec did not foresee: the fixture sources leaked their own answers. Without it the content slice does not measure what §9 says it measures.
