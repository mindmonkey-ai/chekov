//! llama.cpp engine management: clone/pull + cmake Metal build (§5 `setup`).
//!
//! Steps are data (§13.4) so `--dry-run` and tests inspect exactly what would
//! run; execution goes through `std::process::Command`, failures typed (§C.2).

use std::path::{Path, PathBuf};

use crate::error::ChekovError;

pub const LLAMA_CPP_GIT: &str = "https://github.com/ggml-org/llama.cpp";
pub const BUILD_TARGETS: [&str; 3] = ["llama-server", "llama-cli", "llama-gguf-split"];

/// One external command `setup`/`update --engine` will run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineStep {
    pub desc: String,
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
}

impl EngineStep {
    /// `git clone https://... /path` — the printable form.
    #[must_use]
    pub fn render(&self) -> String {
        format!("{} {}", self.program, self.args.join(" "))
    }
}

/// Clone on first contact, fast-forward pull thereafter.
fn git_step(engine_dir: &Path) -> EngineStep {
    let dir = engine_dir.display().to_string();
    if engine_dir.join(".git").exists() {
        EngineStep {
            desc: "update llama.cpp checkout".into(),
            program: "git".into(),
            args: ["-C", &dir, "pull", "--ff-only"].map(String::from).to_vec(),
            cwd: None,
        }
    } else {
        EngineStep {
            desc: "clone llama.cpp".into(),
            program: "git".into(),
            args: ["clone", LLAMA_CPP_GIT, &dir].map(String::from).to_vec(),
            cwd: None,
        }
    }
}

/// Pinned: fetch the ref by name and check it out detached. No `--branch` on
/// the clone (a sha cannot be cloned by name) and never a pull — the pin, not
/// upstream's HEAD of the day, decides what gets built.
fn pinned_steps(engine_dir: &Path, git_ref: &str) -> Vec<EngineStep> {
    let dir = engine_dir.display().to_string();
    let git = |desc: &str, args: &[&str]| EngineStep {
        desc: desc.into(),
        program: "git".into(),
        args: ["-C", &dir]
            .into_iter()
            .chain(args.iter().copied())
            .map(String::from)
            .collect(),
        cwd: None,
    };
    let mut steps = Vec::new();
    if !engine_dir.join(".git").exists() {
        steps.push(EngineStep {
            desc: "clone llama.cpp".into(),
            program: "git".into(),
            args: ["clone", LLAMA_CPP_GIT, &dir].map(String::from).to_vec(),
            cwd: None,
        });
    }
    steps.push(git(
        &format!("fetch llama.cpp ref {git_ref}"),
        &["fetch", "origin", git_ref],
    ));
    steps.push(git(
        &format!("check out {git_ref} (detached)"),
        &["checkout", "--detach", "FETCH_HEAD"],
    ));
    steps
}

/// The steps to bring the engine to a built state: clone (first time) or
/// pull (thereafter) — or, pinned, fetch and detach the ref — then cmake
/// configure with Metal and cmake build the targets.
#[must_use]
pub fn setup_steps(engine_dir: &Path, git_ref: Option<&str>) -> Vec<EngineStep> {
    let mut steps = git_ref.map_or_else(
        || vec![git_step(engine_dir)],
        |git_ref| pinned_steps(engine_dir, git_ref),
    );
    steps.push(configure_step(engine_dir));
    steps.push(compile_step(engine_dir));
    steps.push(verify_step(engine_dir));
    steps
}

/// Run the binary that was just built. A build whose output cannot print its
/// own version fails HERE, as a named step, before the commit is recorded as
/// built — never later, as a failed `run`. There is deliberately no rollback:
/// a silent multi-minute rebuild of the previous commit is the opposite of a
/// loud failure, and `logs/chekov.engine` names the commit to go back to.
fn verify_step(engine_dir: &Path) -> EngineStep {
    EngineStep {
        desc: "verify the built llama-server runs".into(),
        program: server_binary(engine_dir).display().to_string(),
        args: vec!["--version".into()],
        cwd: None,
    }
}

fn configure_step(engine_dir: &Path) -> EngineStep {
    let dir = engine_dir.display().to_string();
    let build = engine_dir.join("build").display().to_string();
    EngineStep {
        desc: "configure cmake (Metal)".into(),
        program: "cmake".into(),
        args: [
            "-S",
            &dir,
            "-B",
            &build,
            "-DGGML_METAL=ON",
            "-DCMAKE_BUILD_TYPE=Release",
        ]
        .map(String::from)
        .to_vec(),
        cwd: None,
    }
}

fn compile_step(engine_dir: &Path) -> EngineStep {
    let build = engine_dir.join("build").display().to_string();
    // Bare `-j` reaches make with no number: unbounded jobs and no jobserver.
    // One job per logical core is what a reader already assumes it means.
    let jobs = format!(
        "-j{}",
        std::thread::available_parallelism().map_or(4, std::num::NonZero::get)
    );
    let mut build_args = ["--build", &build, "--config", "Release", &jobs]
        .map(String::from)
        .to_vec();
    for target in BUILD_TARGETS {
        build_args.push("--target".into());
        build_args.push(target.into());
    }
    EngineStep {
        desc: "build llama.cpp targets".into(),
        program: "cmake".into(),
        args: build_args,
        cwd: None,
    }
}

/// Where the built server binary lands.
#[must_use]
pub fn server_binary(engine_dir: &Path) -> PathBuf {
    engine_dir.join("build").join("bin").join("llama-server")
}

/// Execute steps in order; `dry_run` prints instead. First failure aborts
/// with the step named (§C.2 — never continue past a failed build step).
pub fn run_steps(steps: &[EngineStep], dry_run: bool) -> Result<(), ChekovError> {
    for step in steps {
        if dry_run {
            println!("[dry-run] {}", step.render());
            continue;
        }
        execute(step)?;
    }
    Ok(())
}

fn execute(step: &EngineStep) -> Result<(), ChekovError> {
    let mut cmd = std::process::Command::new(&step.program);
    cmd.args(&step.args);
    if let Some(cwd) = &step.cwd {
        cmd.current_dir(cwd);
    }
    let status = cmd.status().map_err(|e| ChekovError::EngineStepFailed {
        step: step.desc.clone(),
        reason: e.to_string(),
    })?;
    if status.success() {
        Ok(())
    } else {
        Err(ChekovError::EngineStepFailed {
            step: step.desc.clone(),
            reason: format!("exit status {status}"),
        })
    }
}

/// Path of the marker recording which engine commit was last built.
fn commit_marker(logs_dir: &Path) -> PathBuf {
    logs_dir.join("chekov.engine")
}

/// Record the engine commit that was just built successfully.
pub fn record_commit(logs_dir: &Path, commit: &str) -> Result<(), ChekovError> {
    std::fs::create_dir_all(logs_dir)
        .map_err(|e| ChekovError::io(format!("creating {}", logs_dir.display()), e))?;
    let path = commit_marker(logs_dir);
    std::fs::write(&path, format!("{commit}\n"))
        .map_err(|e| ChekovError::io(format!("writing {}", path.display()), e))
}

/// The engine commit chekov last built, if one was recorded.
#[must_use]
pub fn recorded_commit(logs_dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(commit_marker(logs_dir)).ok()?;
    let commit = text.trim();
    (!commit.is_empty()).then(|| commit.to_owned())
}

/// Current HEAD commit of the engine checkout, short form.
#[must_use]
pub fn current_commit(engine_dir: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["-C"])
        .arg(engine_dir)
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::setup_steps;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("chekov-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn rendered(steps: &[super::EngineStep]) -> Vec<String> {
        steps.iter().map(super::EngineStep::render).collect()
    }

    #[test]
    fn a_pinned_fresh_dir_clones_then_fetches_and_detaches_the_ref() {
        let dir = scratch("eng-pinned-fresh");
        let steps = rendered(&setup_steps(&dir, Some("b7000")));
        assert!(steps[0].starts_with("git clone"), "{steps:?}");
        // No `--branch`: a sha is a valid pin and clone cannot take one.
        assert!(!steps[0].contains("--branch"), "{steps:?}");
        assert!(
            steps[1].contains("fetch origin b7000"),
            "the ref is fetched by name: {steps:?}"
        );
        assert!(
            steps[2].contains("checkout --detach FETCH_HEAD"),
            "and checked out detached: {steps:?}"
        );
        assert!(
            !steps.iter().any(|s| s.contains("pull")),
            "a pinned engine never pulls: {steps:?}"
        );
        assert!(
            steps.iter().any(|s| s.contains("-DGGML_METAL=ON")),
            "{steps:?}"
        );
    }

    #[test]
    fn a_pinned_existing_checkout_fetches_and_detaches_without_pulling() {
        let dir = scratch("eng-pinned-existing");
        std::fs::create_dir_all(dir.join(".git")).expect("fake checkout");
        let steps = rendered(&setup_steps(&dir, Some("v1.2.3")));
        assert!(!steps.iter().any(|s| s.contains("clone")), "{steps:?}");
        assert!(!steps.iter().any(|s| s.contains("pull")), "{steps:?}");
        assert!(steps[0].contains("fetch origin v1.2.3"), "{steps:?}");
        assert!(
            steps[1].contains("checkout --detach FETCH_HEAD"),
            "{steps:?}"
        );
    }

    #[test]
    fn the_last_step_runs_the_binary_it_just_built() {
        // A build whose output cannot even print its version must fail HERE,
        // named, not later as a failed `run` — pinned or not.
        for pin in [None, Some("b7000")] {
            let dir = scratch("eng-verify");
            let steps = setup_steps(&dir, pin);
            let last = steps.last().expect("steps");
            assert_eq!(
                last.program,
                super::server_binary(&dir).display().to_string(),
                "{last:?}"
            );
            assert_eq!(last.args, vec!["--version".to_owned()], "{last:?}");
            assert!(last.desc.contains("verify"), "{last:?}");
            let build = steps.len() - 2;
            assert!(
                steps[build].render().contains("--build"),
                "the verify step follows the build: {:?}",
                rendered(&steps)
            );
        }
    }

    #[test]
    fn fresh_dir_clones_then_builds_metal() {
        let dir = scratch("eng-fresh");
        let steps = setup_steps(&dir, None);
        let rendered: Vec<String> = steps.iter().map(super::EngineStep::render).collect();
        assert!(rendered[0].starts_with("git clone"), "{rendered:?}");
        assert!(
            rendered.iter().any(|s| s.contains("-DGGML_METAL=ON")),
            "{rendered:?}"
        );
        assert!(
            rendered.iter().any(|s| s.contains("llama-gguf-split")),
            "{rendered:?}"
        );
    }

    #[test]
    fn a_recorded_engine_commit_survives_a_round_trip() {
        let logs = scratch("engine-marker");
        assert_eq!(
            super::recorded_commit(&logs),
            None,
            "an unrecorded engine must read as unknown, never as a guess"
        );
        super::record_commit(&logs, "abc1234").expect("record");
        assert_eq!(
            super::recorded_commit(&logs).as_deref(),
            Some("abc1234"),
            "the built commit must be readable back so `status` can show what \
             is actually installed"
        );
    }

    #[test]
    fn the_build_bounds_its_job_count() {
        let steps = setup_steps(&scratch("jobs").join("llama.cpp"), None);
        let build = steps
            .iter()
            .map(super::EngineStep::render)
            .find(|r| r.contains("--build"))
            .expect("a build step");
        // A bare `-j` reaches make with no number, which means unbounded jobs
        // and a disabled jobserver — unbounded clang forks on a box holding a
        // 158 GiB model resident is the exact pressure this tool exists to avoid.
        assert!(
            !build.contains("-j ") && !build.ends_with("-j"),
            "-j must carry an explicit job count: {build}"
        );
        assert!(
            build.contains(&format!(
                "-j{}",
                std::thread::available_parallelism().map_or(4, std::num::NonZero::get)
            )) || build.contains(&format!(
                "-j {}",
                std::thread::available_parallelism().map_or(4, std::num::NonZero::get)
            )),
            "job count should track available parallelism: {build}"
        );
    }

    #[test]
    fn existing_checkout_pulls_instead_of_cloning() {
        let dir = scratch("eng-existing");
        std::fs::create_dir_all(dir.join(".git")).expect("fake checkout");
        let steps = setup_steps(&dir, None);
        assert!(steps[0].render().starts_with("git "), "{:?}", steps[0]);
        assert!(steps[0].render().contains("pull"), "{:?}", steps[0]);
    }
}
