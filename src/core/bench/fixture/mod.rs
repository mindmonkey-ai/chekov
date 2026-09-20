//! Graded probe sets: the compiled-in fixture-v1 (`builtin`, embedded by
//! `build.rs`, graded through codebase mode with its held-out tests) and a
//! user-supplied probe-set TOML (`load`).
//!
//! fixture-v1 is release-gated (capability-spec §9): it ships, but no
//! published number rests on it until it has been measured against three
//! models of clearly different capability with the spread published.

pub mod embedded;
pub mod manifest;
pub mod materialize;

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

use std::path::Path;

use serde::Deserialize;

use crate::core::bench::codebase::HiddenTest;
use crate::core::bench::codebase::named::{NamedTask, NamedTasks};
use crate::error::ChekovError;

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
                manifest::invalid(format!(
                    "task {}: hidden {} was not materialized",
                    task.id, task.hidden
                ))
            })?;
        hidden.push(HiddenTest {
            task_id: task.id.clone(),
            file: task.hidden.clone(),
            text,
        });
    }
    let tasks = manifest
        .tasks
        .iter()
        .map(|t| NamedTask {
            id: t.id.clone(),
            file: t.source.clone(),
            symbol: t.symbol.clone(),
        })
        .collect();
    let corpus = format!("{ID}:{}", &manifest.content_hash[..12]);
    Ok(NamedTasks {
        tasks,
        hidden,
        corpus,
    })
}

/// What this chekov knows how to read.
const SUPPORTED_VERSION: u32 = 1;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fixture {
    pub version: u32,
    pub probes: Vec<FixtureProbe>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureProbe {
    pub id: String,
    pub prompt: String,
    pub max_tokens: u32,
    /// Substrings the reply must contain (all of them), case-insensitive.
    #[serde(default)]
    pub expect_contains: Vec<String>,
}

pub fn load(path: &Path) -> Result<Fixture, ChekovError> {
    let invalid = |reason: String| ChekovError::FixtureInvalid {
        path: path.to_path_buf(),
        reason,
    };
    let text = std::fs::read_to_string(path).map_err(|e| invalid(e.to_string()))?;
    let fixture: Fixture = toml::from_str(&text).map_err(|e| invalid(e.to_string()))?;
    if fixture.version != SUPPORTED_VERSION {
        return Err(invalid(format!(
            "version {} — this chekov reads version {SUPPORTED_VERSION}",
            fixture.version
        )));
    }
    if fixture.probes.is_empty() {
        return Err(invalid(
            "no probes — a fixture with nothing to grade".to_owned(),
        ));
    }
    Ok(fixture)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use crate::core::bench::codebase::{self, exec};
    use crate::core::bench::store::{ExecRow, ExecScore};

    fn write_scratch(name: &str, text: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("chekov-test-fixture");
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let path = dir.join(name);
        std::fs::write(&path, text).expect("write fixture");
        path
    }

    #[test]
    fn a_valid_fixture_parses() {
        let path = write_scratch(
            "ok.toml",
            r#"
version = 1

[[probes]]
id = "greeting"
prompt = "Say hello."
max_tokens = 32
expect_contains = ["hello"]
"#,
        );
        let fixture = super::load(&path).expect("valid fixture");
        assert_eq!(fixture.probes.len(), 1);
        assert_eq!(fixture.probes[0].id, "greeting");
    }

    #[test]
    fn an_unknown_key_is_refused() {
        let path = write_scratch("typo.toml", "version = 1\nprobes = []\ntypo = 1\n");
        assert!(super::load(&path).is_err(), "deny_unknown_fields");
    }

    #[test]
    fn a_newer_version_is_refused_naming_what_this_chekov_reads() {
        let path = write_scratch("v2.toml", "version = 2\nprobes = []\n");
        let err = super::load(&path).expect_err("too new");
        assert!(err.to_string().contains("version 1"), "{err}");
    }

    #[test]
    fn an_empty_probe_list_is_refused() {
        let path = write_scratch("empty.toml", "version = 1\nprobes = []\n");
        assert!(
            super::load(&path).is_err(),
            "a fixture with nothing to grade is a mistake"
        );
    }

    #[test]
    fn the_embedded_manifest_parses_and_declares_the_real_content_hash() {
        let computed = super::manifest::content_hash(super::embedded::FILES);
        let manifest = super::builtin().unwrap_or_else(|e| {
            panic!(
                "{e}\n\nwrite content_hash = \"{computed}\" into fixtures/fixture-v1/manifest.toml"
            )
        });
        assert_eq!(manifest.id, super::ID);
        assert_eq!(manifest.tasks.len(), 6);
        let symbols: Vec<&str> = manifest.tasks.iter().map(|t| t.symbol.as_str()).collect();
        assert_eq!(
            symbols,
            [
                "handle_credit",
                "from_str",
                "split_evenly",
                "debit_allowed",
                "format_cents",
                "checked_apply"
            ]
        );
    }

    #[test]
    fn named_tasks_pair_every_manifest_task_with_its_hidden_text() {
        let manifest = super::builtin().expect("manifest");
        let hidden: Vec<(String, String)> = super::embedded::FILES
            .iter()
            .filter(|(p, _)| p.starts_with("hidden/"))
            .map(|(p, t)| ((*p).to_owned(), (*t).to_owned()))
            .collect();
        let named = super::named_tasks(&manifest, &hidden).expect("named");
        assert_eq!(named.tasks.len(), 6);
        assert_eq!(named.hidden.len(), 6);
        assert!(named.hidden.iter().all(|h| h.text.contains("#[test]")));
        assert!(
            named.corpus.starts_with("fixture-v1:")
                && named.corpus.len() == "fixture-v1:".len() + 12
        );
        let err = super::named_tasks(&manifest, &[])
            .expect_err("nothing materialized")
            .to_string();
        assert!(err.contains("was not materialized"), "{err}");
    }

    /// The compiled-in fixture, materialized and prepared — the setup the
    /// resolution test shares with `device_two_fails...` and any future
    /// real-cargo device test. `allow_exec` is what separates them: the
    /// resolution test needs only `git`.
    fn real_prepared(scratch: &Path, allow_exec: bool) -> codebase::Prepared {
        let manifest = super::builtin().expect("manifest");
        let m = super::materialize::materialize(scratch, &manifest.content_hash[..12])
            .expect("materialize");
        let named = super::named_tasks(&manifest, &m.hidden).expect("named");
        codebase::prepare_named(
            &m.repo,
            &named,
            &codebase::PrepareInputs {
                scratch_root: scratch,
                tasks: 0,
                allow_exec,
            },
        )
        .expect("prepare_named")
    }

    /// Each device's id, and the strings that would hand its answer over:
    /// device 2's two entry calls, device 3's exact cents both ways, and each
    /// later device's held-out edge or implementation shortcut.
    const DEVICE_LEAKS: [(&str, &[&str]); 6] = [
        (
            "device-2-near-miss-api",
            &[".apply_entry(", ".append_entry("],
        ),
        ("device-3-invariant-exact", &["249995", "2499.95"]),
        (
            "device-5-split-conserves",
            &["[34, 33, 33]", "[-33, -33, -34]"],
        ),
        ("device-6-debit-boundary", &["100.01", "c.0 <= balance"]),
        (
            "device-7-format-exact",
            &["1701411834604692317316873037158841057.28", "unsigned_abs()"],
        ),
        (
            "device-8-checked-arithmetic",
            &[".checked_add(", ".checked_sub("],
        ),
    ];

    /// One task's prompt — prefix, suffix, and the extra file when it has one —
    /// carries neither the gold body nor any of `leaks`.
    fn assert_prompt_withholds(task: &codebase::CodebaseTask, leaks: &[&str]) {
        let prompt = format!("{}{}{}", task.prefix, task.suffix, task.extra_text);
        assert!(
            !prompt.contains(&task.gold),
            "{}: the gold body is in its own prompt",
            task.id
        );
        for leak in leaks {
            assert!(
                !prompt.contains(leak),
                "{}: `{leak}` is in its prompt",
                task.id
            );
        }
    }

    /// The real compiled-in fixture resolves to its four devices and none of
    /// them can be answered from its own prompt. Needs `git`, not `cargo`, so
    /// this one is ungated — it is the check that a content edit reopening a
    /// leak cannot pass CI.
    #[test]
    fn the_six_devices_resolve_in_order_and_no_prompt_carries_its_answer() {
        let scratch = std::env::temp_dir().join("chekov-test-fixture-resolve");
        let _ = std::fs::remove_dir_all(&scratch);
        let prepared = real_prepared(&scratch, false);
        let ids: Vec<&str> = prepared.tasks.iter().map(|t| t.id.as_str()).collect();
        let expected: Vec<&str> = DEVICE_LEAKS.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, expected, "the manifest's four devices, in order");
        for (task, (id, leaks)) in prepared.tasks.iter().zip(DEVICE_LEAKS) {
            assert_eq!(task.tier, codebase::TaskTier::FunctionBody, "{id}");
            assert_prompt_withholds(task, leaks);
        }
    }

    /// One crossing against a real toolchain, unwrapped.
    fn crossing_row(env: &exec::Env, crossing: &exec::Crossing) -> ExecRow {
        exec::exec_crossing_with(env, crossing).expect("crossing")
    }

    /// Tier 6: the row's `compile` score is exactly 1.0, with cargo's own
    /// words on mismatch.
    fn assert_compiled(row: &ExecRow) {
        assert_eq!(
            row.compile,
            ExecScore::Value(1.0),
            "expected to compile: {:?}",
            row.compile_error
        );
    }

    /// Tier 7: the row's `test` score against its expected value.
    fn assert_test_score(row: &ExecRow, expected: f64) {
        assert_eq!(
            row.test,
            ExecScore::Value(expected),
            "{:?}",
            row.test_failure
        );
    }

    /// Device 2 end to end against a real `cargo`, once. Gated on
    /// `CHEKOV_TEST_EXEC=1` like `tests/codebase_exec.rs`.
    #[test]
    fn device_two_fails_the_hidden_test_on_the_near_miss_and_passes_on_the_gold() {
        if std::env::var("CHEKOV_TEST_EXEC").as_deref() != Ok("1") {
            eprintln!("skipping: set CHEKOV_TEST_EXEC=1 to run the fixture against a real cargo");
            return;
        }
        let scratch = std::env::temp_dir().join("chekov-test-fixture-real");
        let _ = std::fs::remove_dir_all(&scratch);
        let prepared = real_prepared(&scratch, true);
        let env = prepared.exec.env().expect("a toolchain");
        let task = prepared
            .tasks
            .iter()
            .find(|t| t.id == "device-2-near-miss-api")
            .expect("device 2");
        let hidden = prepared.hidden.iter().find(|h| h.task_id == task.id);
        let wrong = task.gold.replace("apply_entry", "append_entry");
        assert_ne!(wrong, task.gold, "the gold uses apply_entry");
        let row = crossing_row(
            env,
            &exec::Crossing {
                task,
                fill: &wrong,
                hidden,
            },
        );
        assert_compiled(&row);
        assert_test_score(&row, 0.0);
        let row = crossing_row(
            env,
            &exec::Crossing {
                task,
                fill: &task.gold,
                hidden,
            },
        );
        assert_test_score(&row, 1.0);
        prepared.exec.finish().expect("cleanup");
    }

    fn assert_hardened_device(prepared: &codebase::Prepared, id: &str) {
        let task = prepared
            .tasks
            .iter()
            .find(|task| task.id == id)
            .expect("hardened device");
        let hidden = prepared.hidden.iter().find(|test| test.task_id == task.id);
        let wrong = match id {
            "device-5-split-conserves" => task
                .gold
                .replace("base + i128::from(i < remainder)", "base"),
            "device-6-debit-boundary" => task.gold.replace("c.0 <= balance", "c.0 < balance"),
            "device-7-format-exact" => "value.0.to_string()".to_owned(),
            "device-8-checked-arithmetic" => "Some(apply(cmd, balance))".to_owned(),
            _ => panic!("unknown hardened device {id}"),
        };
        assert_ne!(wrong, task.gold, "the wrong body must differ from gold");
        let env = prepared.exec.env().expect("a toolchain");
        let wrong_row = crossing_row(
            env,
            &exec::Crossing {
                task,
                fill: &wrong,
                hidden,
            },
        );
        assert_compiled(&wrong_row);
        assert_test_score(&wrong_row, 0.0);
        let gold_row = crossing_row(
            env,
            &exec::Crossing {
                task,
                fill: &task.gold,
                hidden,
            },
        );
        assert_test_score(&gold_row, 1.0);
    }

    /// Devices 5–8: each obvious wrong body compiles but fails its held-out
    /// assertion, while the gold body passes. Gated on a real `cargo`.
    #[test]
    fn hardened_devices_compile_wrong_and_discriminate_at_the_test_gate() {
        if std::env::var("CHEKOV_TEST_EXEC").as_deref() != Ok("1") {
            eprintln!("skipping: set CHEKOV_TEST_EXEC=1 to run the fixture against a real cargo");
            return;
        }
        let scratch = std::env::temp_dir().join("chekov-test-fixture-hardened");
        let _ = std::fs::remove_dir_all(&scratch);
        let prepared = real_prepared(&scratch, true);
        assert_hardened_device(&prepared, "device-5-split-conserves");
        assert_hardened_device(&prepared, "device-6-debit-boundary");
        assert_hardened_device(&prepared, "device-7-format-exact");
        assert_hardened_device(&prepared, "device-8-checked-arithmetic");
        prepared.exec.finish().expect("cleanup");
    }
}
