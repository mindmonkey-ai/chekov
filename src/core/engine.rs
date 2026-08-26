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

/// The steps to bring the engine to a built state: clone (first time) or
/// pull (thereafter), cmake configure with Metal, cmake build the targets.
#[must_use]
pub fn setup_steps(engine_dir: &Path) -> Vec<EngineStep> {
    let dir = engine_dir.display().to_string();
    let build = engine_dir.join("build").display().to_string();
    let configure = EngineStep {
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
    };
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
    let compile = EngineStep {
        desc: "build llama.cpp targets".into(),
        program: "cmake".into(),
        args: build_args,
        cwd: None,
    };
    vec![git_step(engine_dir), configure, compile]
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

    #[test]
    fn fresh_dir_clones_then_builds_metal() {
        let dir = scratch("eng-fresh");
        let steps = setup_steps(&dir);
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
        let steps = setup_steps(&scratch("jobs").join("llama.cpp"));
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
        let steps = setup_steps(&dir);
        assert!(steps[0].render().starts_with("git "), "{:?}", steps[0]);
        assert!(steps[0].render().contains("pull"), "{:?}", steps[0]);
    }
}
