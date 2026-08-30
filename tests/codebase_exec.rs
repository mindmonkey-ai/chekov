//! Tiers 6 and 7 against a real `cargo`, once.
//!
//! Gated on `CHEKOV_TEST_EXEC=1`: a real check costs tens of seconds and
//! needs a toolchain, and `make test` has to pass on a machine with neither.
//! Run it with `CHEKOV_TEST_EXEC=1 cargo test --locked --test codebase_exec`.

use std::path::{Path, PathBuf};

use chekov::core::bench::codebase::exec::{self, Env, Timeouts};
use chekov::core::bench::codebase::{CodebaseTask, Excluded, TaskTier, tree};
use chekov::core::bench::store::ExecScore;

/// `true` when the caller asked for the real toolchain.
fn opted_in() -> bool {
    if std::env::var("CHEKOV_TEST_EXEC").as_deref() == Ok("1") {
        return true;
    }
    eprintln!("skipping: set CHEKOV_TEST_EXEC=1 to run the exec tiers against a real cargo");
    false
}

/// One crate, two functions, one of them covered by a test.
const LIB_RS: &str = "\
pub fn alpha(n: u32) -> u32 {
    let doubled = n * 2;
    doubled
}

pub fn beta(n: u32) -> u32 {
    n + 1
}

#[cfg(test)]
mod tests {
    #[test]
    fn covers_alpha() {
        assert_eq!(super::alpha(2), 4);
    }
}
";

const MANIFEST: &str =
    "[package]\nname = \"widget\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n";

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("chekov-it-exec").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    dir
}

/// A committed one-crate repository, plus extra files the caller wants.
fn repo(dir: &Path, extra: &[(&str, &str)]) -> PathBuf {
    let repo = dir.join("repo");
    std::fs::create_dir_all(repo.join("src")).expect("src");
    std::fs::write(repo.join("Cargo.toml"), MANIFEST).expect("manifest");
    std::fs::write(repo.join("src/lib.rs"), LIB_RS).expect("lib.rs");
    for (path, text) in extra {
        let full = repo.join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("parent");
        }
        std::fs::write(full, text).expect("extra");
    }
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "t@t"],
        vec!["config", "user.name", "t"],
        vec!["add", "-A"],
        vec!["commit", "-qm", "fixture"],
    ] {
        tree::Worktree::run_git_for_test(&repo, &args).expect("git");
    }
    repo
}

/// A `CodebaseTask` masking `needle` in `src/lib.rs`.
fn task_on(worktree: &Path, symbol: Option<&str>, needle: &str) -> CodebaseTask {
    let text = std::fs::read_to_string(worktree.join("src/lib.rs")).expect("lib.rs");
    let at = text.find(needle).expect("the span");
    CodebaseTask {
        id: "in_file-fixture-L2".into(),
        tier: TaskTier::InFile,
        file: "src/lib.rs".into(),
        line: 2,
        byte_range: at..at + needle.len(),
        gold: needle.to_owned(),
        prefix: text[..at].to_owned(),
        suffix: text[at + needle.len()..].to_owned(),
        excluded: Excluded {
            doc_comment: 0,
            cross_file: "n/a: same-file".into(),
            cfg_test_lines: 0,
            cross_file_withheld: 0,
        },
        name: symbol.map(str::to_owned),
        also_first_uses: Vec::new(),
        extra: None,
        extra_text: String::new(),
    }
}

fn env_over(dir: &Path, repo: &Path, timeouts: Timeouts) -> Env {
    let worktree = tree::Worktree::add(repo, &dir.join("tree")).expect("worktree");
    Env {
        worktree,
        cargo: PathBuf::from("cargo"),
        target_dir: dir.join("target"),
        cargo_version: "real".to_owned(),
        timeouts,
    }
}

#[test]
fn a_correct_fill_passes_both_tiers_and_names_the_test_it_ran() {
    if !opted_in() {
        return;
    }
    let dir = scratch("correct");
    let repo = repo(&dir, &[]);
    let env = env_over(&dir, &repo, Timeouts::DEFAULT);
    let task = task_on(&env.worktree.path, None, "let doubled = n * 2;");
    let original = std::fs::read_to_string(env.worktree.path.join("src/lib.rs")).expect("read");
    let row = exec::exec_crossing(&env, &task, "let doubled = n * 2;").expect("the crossing runs");
    assert_eq!(row.compile, ExecScore::Value(1.0), "{row:?}");
    assert_eq!(row.tests, vec!["covers_alpha".to_owned()]);
    assert_eq!(row.test, ExecScore::Value(1.0), "{row:?}");
    assert!(row.check_secs > 0.0 && row.test_secs > 0.0);
    assert_eq!(
        std::fs::read_to_string(env.worktree.path.join("src/lib.rs")).expect("read"),
        original
    );
    env.finish().expect("cleanup");
}

#[test]
fn a_type_error_fails_six_stores_the_message_and_skips_seven() {
    if !opted_in() {
        return;
    }
    let dir = scratch("type-error");
    let repo = repo(&dir, &[]);
    let env = env_over(&dir, &repo, Timeouts::DEFAULT);
    let task = task_on(&env.worktree.path, None, "let doubled = n * 2;");
    let row =
        exec::exec_crossing(&env, &task, "let doubled = \"two\";").expect("the crossing runs");
    assert_eq!(row.compile, ExecScore::Value(0.0), "{row:?}");
    let message = row
        .compile_error
        .as_deref()
        .expect("the first error is stored");
    assert!(message.contains("src/lib.rs:"), "{message}");
    assert!(message.contains("mismatched types"), "{message}");
    assert_eq!(
        row.test,
        ExecScore::Skipped("did not compile".to_owned()),
        "{row:?}"
    );
    env.finish().expect("cleanup");
}

#[test]
fn a_fill_in_the_untested_function_is_skipped_for_want_of_a_covering_test() {
    if !opted_in() {
        return;
    }
    let dir = scratch("untested");
    let repo = repo(&dir, &[]);
    let env = env_over(&dir, &repo, Timeouts::DEFAULT);
    let task = task_on(&env.worktree.path, None, "n + 1");
    let row = exec::exec_crossing(&env, &task, "n + 1").expect("the crossing runs");
    assert_eq!(row.compile, ExecScore::Value(1.0), "{row:?}");
    assert_eq!(
        row.test,
        ExecScore::Skipped(exec::NO_COVERING_TEST.to_owned()),
        "beta has no test naming it"
    );
    assert!(row.tests.is_empty());
    env.finish().expect("cleanup");
}

/// A `build.rs` that sleeps past the ceiling: a skip with the reason, and the
/// file still restored. The ceiling is lowered through `Env.timeouts`, which
/// is why it lives on the environment rather than in a constant.
#[test]
fn a_build_script_that_sleeps_past_the_ceiling_is_a_skip_and_the_file_comes_back() {
    if !opted_in() {
        return;
    }
    let dir = scratch("timeout");
    let repo = repo(
        &dir,
        &[(
            "build.rs",
            "fn main() { std::thread::sleep(std::time::Duration::from_secs(120)); }\n",
        )],
    );
    let env = env_over(
        &dir,
        &repo,
        Timeouts {
            check: std::time::Duration::from_secs(5),
            test: std::time::Duration::from_secs(5),
        },
    );
    let task = task_on(&env.worktree.path, None, "let doubled = n * 2;");
    let original = std::fs::read_to_string(env.worktree.path.join("src/lib.rs")).expect("read");
    let row = exec::exec_crossing(&env, &task, "let doubled = n * 2;").expect("the crossing runs");
    let ExecScore::Skipped(reason) = row.compile else {
        panic!("expected a skip, got {:?}", row.compile);
    };
    // The seconds in the message are the CONFIGURED ceiling — 5 here, 120 in
    // production — so the message is true rather than a copied constant.
    assert!(reason.starts_with("check timed out after "), "{reason}");
    assert!(reason.ends_with(" s"), "{reason}");
    assert_eq!(
        std::fs::read_to_string(env.worktree.path.join("src/lib.rs")).expect("read"),
        original,
        "a killed check still gives the file back"
    );
    env.finish().expect("cleanup");
}
