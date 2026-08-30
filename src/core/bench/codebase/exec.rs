//! Tiers 6 and 7: what happens when the fill is actually built.
//!
//! Everything in this module runs a subprocess, and nothing in it runs
//! without `--allow-exec`. The bounds are the worktree (the only place
//! written), `--offline` after one fetch, a scratch `CARGO_TARGET_DIR`, a
//! wall-clock timeout with a process-group kill, and a revert verified byte
//! for byte before the next crossing is measured.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::error::ChekovError;

/// How long each of the two cargo invocations may take before its process
/// group is killed (spec §3, §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeouts {
    pub check: Duration,
    pub test: Duration,
}

impl Timeouts {
    /// The spec's ceilings. Carried on `Env` rather than read from a constant
    /// at the call site so the integration test can lower them.
    pub const DEFAULT: Self = Self {
        check: Duration::from_mins(2),
        test: Duration::from_mins(5),
    };
}

/// How long `<program> --version` may take to answer.
///
/// A rustup shim's first call can be slow; anything past this is not a
/// toolchain the run should wait on.
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

/// One cargo invocation.
///
/// The executable is a field rather than an ambient lookup: the caller that
/// resolved it once (`probe`) is the caller that passes it, so `run_cargo`
/// reads no environment and the tests hand it a shell script directly.
pub struct CargoRun<'a> {
    pub program: &'a Path,
    pub args: &'a [&'a str],
    pub cwd: &'a Path,
    pub target_dir: &'a Path,
    pub timeout: Duration,
}

/// What it did. `status` is `None` when a signal ended it — which, when
/// `timed_out` is set, is the kill below.
#[derive(Debug)]
pub struct CargoOutcome {
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub secs: f64,
    pub timed_out: bool,
}

/// Which cargo to spawn, resolved once by `probe` and carried from there on.
///
/// `$CHEKOV_CARGO` overrides for the integration test; otherwise `cargo` off
/// `PATH`. This READS the environment — safe, and the crate's
/// `#![forbid(unsafe_code)]` means nothing here may write it, which is why
/// the executable travels as a field instead of as a variable the tests
/// would have to set.
fn cargo_program() -> PathBuf {
    std::env::var_os("CHEKOV_CARGO").map_or_else(|| PathBuf::from("cargo"), PathBuf::from)
}

/// Spawn, drain both pipes on their own threads, poll the clock, kill the
/// group at the deadline.
pub fn run_cargo(run: &CargoRun) -> Result<CargoOutcome, ChekovError> {
    use std::os::unix::process::CommandExt;
    let started = Instant::now();
    let mut child = Command::new(run.program)
        .args(run.args)
        .current_dir(run.cwd)
        .env("CARGO_TARGET_DIR", run.target_dir)
        .env("CARGO_TERM_COLOR", "never")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|e| {
            // The path, not just the args: `cargo` missing from PATH and a
            // wrong $CHEKOV_CARGO read identically without it.
            let what = format!("spawning {} {:?}", run.program.display(), run.args);
            ChekovError::io(what, e)
        })?;
    let out = drain(child.stdout.take());
    let err = drain(child.stderr.take());
    let timed_out = wait_or_kill(&mut child, run.timeout);
    let status = child.wait().ok().and_then(|s| s.code());
    Ok(CargoOutcome {
        status,
        stdout: out.join().unwrap_or_default(),
        stderr: err.join().unwrap_or_default(),
        secs: started.elapsed().as_secs_f64(),
        timed_out,
    })
}

/// Read one pipe to the end on its own thread.
///
/// `cargo check --message-format=json` writes megabytes. A `try_wait` loop
/// that never reads would wedge the child on a full pipe buffer, and the
/// "timeout" it then reported would be chekov's own deadlock.
fn drain<R: std::io::Read + Send + 'static>(pipe: Option<R>) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let Some(mut pipe) = pipe else {
            return String::new();
        };
        let mut buffer = Vec::new();
        let _ = std::io::Read::read_to_end(&mut pipe, &mut buffer);
        String::from_utf8_lossy(&buffer).into_owned()
    })
}

/// `true` when the deadline expired and the group was killed.
fn wait_or_kill(child: &mut Child, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => return false,
            Ok(None) => {}
        }
        if Instant::now() >= deadline {
            kill_group(child.id());
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// SIGKILL the child's whole process group.
///
/// `process_group(0)` made the child its own group leader, so its pgid IS its
/// pid and a negative pid reaches every rustc `cargo` spawned. `nix` is
/// already a dependency for exactly this call (`core/server.rs`); no `libc`,
/// and no shelling out to `kill(1)`.
fn kill_group(pid: u32) {
    let Ok(raw) = i32::try_from(pid) else {
        return;
    };
    let _ = nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(-raw),
        nix::sys::signal::Signal::SIGKILL,
    );
}

/// The cargo the run will use, and the line it answered `--version` with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    pub program: PathBuf,
    pub version: String,
}

/// `Cargo.toml` at the root and a `cargo` that answers `--version`, or the
/// reason there is no toolchain.
///
/// A missing toolchain is a capability of the machine, never a failing score,
/// so this returns the reason every crossing will record rather than an error
/// that would stop the run. This is the ONE place the executable is resolved.
pub fn probe(root: &Path) -> Result<Probe, String> {
    probe_program(root, &cargo_program())
}

/// `probe` against a named executable — the seam the unit tests use, so none
/// of them needs a toolchain or has to write the environment to avoid one.
fn probe_program(root: &Path, program: &Path) -> Result<Probe, String> {
    if !root.join("Cargo.toml").is_file() {
        return Err("no Rust toolchain: no Cargo.toml at the repository root".to_owned());
    }
    // `--version` builds nothing, so the scratch target directory does not
    // exist yet and the root stands in for it: neither is written either way.
    let outcome = run_cargo(&CargoRun {
        program,
        args: &["--version"],
        cwd: root,
        target_dir: root,
        timeout: PROBE_TIMEOUT,
    })
    .map_err(|e| format!("no Rust toolchain: cargo is not runnable ({e})"))?;
    if outcome.status == Some(0) {
        return Ok(Probe {
            program: program.to_path_buf(),
            version: outcome.stdout.trim().to_owned(),
        });
    }
    Err(format!(
        "no Rust toolchain: {} --version failed ({})",
        program.display(),
        version_failure(&outcome)
    ))
}

/// Why `--version` did not answer, in cargo's own words where it left any.
fn version_failure(outcome: &CargoOutcome) -> String {
    if outcome.timed_out {
        return format!("no answer within {PROBE_TIMEOUT:?}");
    }
    let stderr = outcome.stderr.trim();
    if stderr.is_empty() {
        format!("exit {:?}, no output", outcome.status)
    } else {
        stderr.to_owned()
    }
}

/// The worktree, the scratch target directory and the toolchain the exec
/// tiers run in.
pub struct Env {
    pub worktree: super::tree::Worktree,
    pub cargo: PathBuf,
    pub target_dir: PathBuf,
    pub cargo_version: String,
    pub timeouts: Timeouts,
}

impl Env {
    /// The target directory, then the worktree — both explicit, so a cleanup
    /// failure is reported instead of being swallowed by `Worktree::drop`.
    pub fn finish(self) -> Result<(), ChekovError> {
        if self.target_dir.exists() {
            std::fs::remove_dir_all(&self.target_dir).map_err(|e| {
                ChekovError::io(format!("removing {}", self.target_dir.display()), e)
            })?;
        }
        self.worktree.remove()
    }
}

/// Whether the exec tiers run, and if not, why not.
pub enum Exec {
    /// `--allow-exec` was not given. Nothing was built and nothing was kept.
    Off,
    /// The flag was given and the machine cannot honour it — the reason every
    /// crossing records, once in the header.
    Unavailable(String),
    Ready(Env),
}

impl Exec {
    #[must_use]
    pub const fn allowed(&self) -> bool {
        !matches!(self, Self::Off)
    }

    #[must_use]
    pub const fn cargo_version(&self) -> Option<&str> {
        match self {
            Self::Ready(env) => Some(env.cargo_version.as_str()),
            _ => None,
        }
    }

    #[must_use]
    pub const fn env(&self) -> Option<&Env> {
        match self {
            Self::Ready(env) => Some(env),
            _ => None,
        }
    }

    /// Remove what a ready environment is holding; the other two hold nothing.
    pub fn finish(self) -> Result<(), ChekovError> {
        match self {
            Self::Ready(env) => env.finish(),
            Self::Off | Self::Unavailable(_) => Ok(()),
        }
    }
}

/// The probe, the scratch target directory, and one online `cargo fetch`.
///
/// The worktree is CONSUMED: a ready environment keeps it for the run, and an
/// unavailable one removes it here, so the lifetime question has exactly one
/// answer per outcome.
pub fn prepare_env(
    worktree: super::tree::Worktree,
    scratch_root: &Path,
    head12: &str,
) -> Result<Exec, ChekovError> {
    let prep = Prepared {
        cargo: &cargo_program(),
        scratch_root,
        head12,
    };
    prepare_env_with(worktree, &prep)
}

/// What `prepare_env` has resolved by the time it can build an `Env`.
///
/// Bundled so the seam below stays inside the three-parameter limit while the
/// public entry point keeps the signature the run loop calls.
struct Prepared<'a> {
    cargo: &'a Path,
    scratch_root: &'a Path,
    head12: &'a str,
}

/// `prepare_env` against a named executable — the seam its tests use, so they
/// need no toolchain and reach no network.
fn prepare_env_with(worktree: super::tree::Worktree, prep: &Prepared) -> Result<Exec, ChekovError> {
    let probed = match probe_program(&worktree.path, prep.cargo) {
        Ok(probed) => probed,
        Err(reason) => return Ok(unavailable(worktree, reason)),
    };
    let target_dir = prep.scratch_root.join(format!("target-{}", prep.head12));
    std::fs::create_dir_all(&target_dir)
        .map_err(|e| ChekovError::io(format!("creating {}", target_dir.display()), e))?;
    fetch(&probed.program, &worktree.path, &target_dir);
    Ok(Exec::Ready(Env {
        worktree,
        cargo: probed.program,
        target_dir,
        cargo_version: probed.version,
        timeouts: Timeouts::DEFAULT,
    }))
}

/// The probe said no: the worktree goes now, since nothing will read it.
///
/// A removal failure JOINS the reason instead of replacing it. Returning the
/// io error here would throw away the toolchain reason every crossing is about
/// to record — two things went wrong, and the run must be able to say both.
fn unavailable(worktree: super::tree::Worktree, reason: String) -> Exec {
    match worktree.remove() {
        Ok(()) => Exec::Unavailable(reason),
        Err(e) => Exec::Unavailable(format!(
            "{reason}; and the worktree could not be removed: {e}"
        )),
    }
}

/// The one invocation allowed the network, before the loop.
///
/// Its failure is not fatal: every later crossing carries `--offline`, and a
/// check that then needs the network records `needs network` with cargo's own
/// words — a per-crossing skip is more informative than refusing the run.
fn fetch(cargo: &Path, worktree: &Path, target_dir: &Path) {
    let outcome = run_cargo(&CargoRun {
        program: cargo,
        args: &["fetch"],
        cwd: worktree,
        target_dir,
        timeout: Timeouts::DEFAULT.check,
    });
    match outcome {
        Ok(out) if out.status == Some(0) => {}
        Ok(out) => eprintln!(
            "chekov bench: `cargo fetch` did not succeed ({}) — the exec tiers run offline \
             from here, and a crossing that needs the registry is skipped with cargo's reason",
            out.stderr.lines().next().unwrap_or("no output").trim()
        ),
        Err(e) => eprintln!("chekov bench: `cargo fetch` could not run ({e})"),
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::Duration;

    use super::super::tree::Worktree;
    use super::{CargoRun, Prepared, Timeouts};

    /// A twelve-character stand-in for a HEAD, so the scratch target's name is
    /// checked against a literal rather than against the code that built it.
    const HEAD12: &str = "0123456789ab";

    /// A scratch directory keyed by name, cleared first: every test names its
    /// own, so two fake cargos never share a path and the suite is safe to run
    /// in parallel.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("chekov-test-exec").join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    /// An executable shell script standing in for `cargo`, handed to the code
    /// under test as `CargoRun::program`. No test in this module needs a
    /// toolchain, and none of them touches the environment.
    fn fake_cargo(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("fake-cargo");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write fake cargo");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod fake cargo");
        path
    }

    fn git(repo: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .status()
            .expect("git")
            .success();
        assert!(ok, "git {args:?}");
    }

    /// A committed one-file crate: `Worktree::add` needs a git repository and
    /// `probe` needs a `Cargo.toml` at the root of the checkout.
    ///
    /// Duplicated from `tree.rs`'s test helper rather than promoting that one:
    /// this fixture needs the manifest that one has no use for, and making one
    /// test module's helper visible to a sibling couples both to nine lines of
    /// `git init`.
    fn repo(name: &str) -> PathBuf {
        let dir = scratch(name).join("repo");
        std::fs::create_dir_all(dir.join("src")).expect("mkdir");
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"x\"\n").expect("manifest");
        std::fs::write(dir.join("src/lib.rs"), "pub fn a() {}\n").expect("write");
        let who = ["-c", "user.email=t@t", "-c", "user.name=t"];
        git(&dir, &["init", "-q"]);
        git(&dir, &[&who[..], &["add", "."]].concat());
        git(&dir, &[&who[..], &["commit", "-q", "-m", "init"]].concat());
        dir
    }

    #[test]
    fn a_cargo_run_reports_its_streams_its_status_and_its_wall_clock() {
        let dir = scratch("streams");
        let cargo = fake_cargo(&dir, "echo out-line\necho err-line >&2\nexit 3");
        let outcome = super::run_cargo(&CargoRun {
            program: &cargo,
            args: &["check"],
            cwd: &dir,
            target_dir: &dir.join("target"),
            timeout: Duration::from_secs(30),
        })
        .expect("the fake cargo runs");
        assert_eq!(outcome.status, Some(3));
        assert!(outcome.stdout.contains("out-line"), "{}", outcome.stdout);
        assert!(outcome.stderr.contains("err-line"), "{}", outcome.stderr);
        assert!(!outcome.timed_out);
        assert!(outcome.secs >= 0.0, "the wall clock is recorded");
    }

    /// The timeout is the point: a build script that sleeps forever must not
    /// hold the run, and the whole process GROUP has to go, or `cargo`'s
    /// rustc children outlive it.
    #[test]
    fn a_run_past_its_timeout_is_killed_and_says_so() {
        let dir = scratch("timeout");
        // `sleep 30 &  wait` puts the sleep in a CHILD of the script, holding
        // the inherited stdout pipe. Killing only the script would leave the
        // sleep — and the pipe — behind, so the reader thread would block for
        // the full 30 s and the elapsed bound below would fail. The bound is
        // what proves the kill reached the group.
        let cargo = fake_cargo(&dir, "sleep 30 &\nwait");
        let started = std::time::Instant::now();
        let outcome = super::run_cargo(&CargoRun {
            program: &cargo,
            args: &["check"],
            cwd: &dir,
            target_dir: &dir.join("target"),
            timeout: Duration::from_millis(300),
        })
        .expect("the runner returns rather than hanging");
        assert!(outcome.timed_out, "the expiry is reported, not inferred");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the kill happened at the deadline, not at the sleep's end"
        );
    }

    /// A file large enough to fill a pipe buffer: the reader threads are what
    /// keep this from deadlocking, so the test is the reason they exist.
    #[test]
    fn a_chatty_run_does_not_wedge_on_a_full_pipe() {
        let dir = scratch("chatty");
        let cargo = fake_cargo(
            &dir,
            "i=0\nwhile [ $i -lt 20000 ]; do\n  \
             echo 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'\n  i=$((i+1))\ndone",
        );
        let outcome = super::run_cargo(&CargoRun {
            program: &cargo,
            args: &["check"],
            cwd: &dir,
            target_dir: &dir.join("target"),
            timeout: Duration::from_mins(1),
        })
        .expect("the fake cargo runs");
        assert!(!outcome.timed_out, "a chatty child is not a slow one");
        assert!(outcome.stdout.len() > 800_000, "{}", outcome.stdout.len());
    }

    #[test]
    fn the_probe_refuses_a_root_without_a_cargo_toml_and_names_which() {
        let dir = scratch("no-manifest");
        let reason = super::probe(&dir).expect_err("no Cargo.toml, no toolchain");
        assert!(reason.starts_with("no Rust toolchain: "), "{reason}");
        assert!(reason.contains("Cargo.toml"), "{reason}");
    }

    #[test]
    fn the_probe_reports_cargos_version_line_verbatim() {
        let dir = scratch("version");
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"x\"\n").expect("manifest");
        let cargo = fake_cargo(&dir, "echo 'cargo 1.95.0 (deadbeef 2026-01-01)'");
        let probe = super::probe_program(&dir, &cargo).expect("the fake cargo answers --version");
        assert_eq!(probe.version, "cargo 1.95.0 (deadbeef 2026-01-01)");
        assert_eq!(probe.program, cargo, "the resolved program is carried out");
    }

    #[test]
    fn a_cargo_that_cannot_run_is_a_missing_toolchain_and_not_an_error() {
        let dir = scratch("no-cargo");
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"x\"\n").expect("manifest");
        let missing = dir.join("nothing-here");
        let reason = super::probe_program(&dir, &missing).expect_err("nothing to run");
        assert!(reason.starts_with("no Rust toolchain: "), "{reason}");
        assert!(
            reason.contains(&missing.display().to_string()),
            "the path that could not be spawned is named, not just the args: {reason}"
        );
    }

    #[test]
    fn the_default_timeouts_are_the_specs_two_minutes_and_five() {
        // In seconds, because the spec states them in seconds and a
        // `Duration::from_mins` on both sides would only restate the constant.
        assert_eq!(Timeouts::DEFAULT.check.as_secs(), 120);
        assert_eq!(Timeouts::DEFAULT.test.as_secs(), 300);
    }

    /// The bounds only hold if they actually reach the child: the scratch
    /// target as `$CARGO_TARGET_DIR`, the args as given, and a stdin that is
    /// closed rather than the harness's — a cargo that stopped to ask a
    /// question would otherwise hang until the timeout.
    #[test]
    fn the_child_gets_the_scratch_target_the_args_and_a_closed_stdin() {
        let dir = scratch("environment");
        let target = dir.join("scratch-target");
        let cargo = fake_cargo(
            &dir,
            "echo \"target=$CARGO_TARGET_DIR\"\necho \"args=$@\"\n\
             if read line; then echo 'stdin=open'; else echo 'stdin=closed'; fi",
        );
        let outcome = super::run_cargo(&CargoRun {
            program: &cargo,
            args: &["check", "--offline"],
            cwd: &dir,
            target_dir: &target,
            timeout: Duration::from_secs(30),
        })
        .expect("the fake cargo runs");
        let said = outcome.stdout;
        assert!(
            said.contains(&format!("target={}", target.display())),
            "{said}"
        );
        assert!(said.contains("args=check --offline"), "{said}");
        assert!(said.contains("stdin=closed"), "{said}");
    }

    /// `cargo fetch` is the one invocation allowed the network, and its
    /// failure must not take the run down: every crossing after it is
    /// `--offline` anyway, and a per-crossing skip says more than a refusal.
    #[test]
    fn a_failed_fetch_still_readies_the_run_and_names_the_scratch_target() {
        let dir = scratch("prepare");
        let source = repo("prepare-repo");
        let worktree = Worktree::add(&source, &dir.join("wt")).expect("worktree");
        let cargo = fake_cargo(
            &dir,
            "case \"$1\" in\n  --version) echo 'cargo 9.9.9 (fake)' ;;\n  \
             *) echo 'the registry is unreachable' >&2; exit 1 ;;\nesac",
        );
        let prep = Prepared {
            cargo: &cargo,
            scratch_root: &dir,
            head12: HEAD12,
        };
        let exec = super::prepare_env_with(worktree, &prep).expect("a failed fetch is not fatal");
        let env = exec.env().expect("ready in spite of the fetch");
        assert_eq!(env.target_dir, dir.join(format!("target-{HEAD12}")));
        assert!(env.target_dir.is_dir(), "the scratch target is created");
        assert_eq!(env.cargo, cargo, "the probed executable is carried on");
        assert_eq!(exec.cargo_version(), Some("cargo 9.9.9 (fake)"));
        assert!(exec.allowed());
    }

    #[test]
    fn finish_removes_the_scratch_target_and_the_worktree() {
        let dir = scratch("finish");
        let source = repo("finish-repo");
        let worktree = Worktree::add(&source, &dir.join("wt")).expect("worktree");
        let cargo = fake_cargo(&dir, "echo 'cargo 9.9.9 (fake)'");
        let prep = Prepared {
            cargo: &cargo,
            scratch_root: &dir,
            head12: HEAD12,
        };
        let exec = super::prepare_env_with(worktree, &prep).expect("prepared");
        let env = exec.env().expect("ready");
        let (target, checkout) = (env.target_dir.clone(), env.worktree.path.clone());
        assert!(target.is_dir() && checkout.is_dir(), "both exist first");
        exec.finish().expect("both come off cleanly");
        assert!(!target.exists(), "the scratch target is removed");
        assert!(!checkout.exists(), "the worktree is removed");
    }

    /// A machine without a toolchain is a capability, not a failure: the
    /// worktree goes back immediately, no scratch target is made for a run
    /// that cannot happen, and the reason survives to be recorded.
    #[test]
    fn a_root_without_a_manifest_is_unavailable_and_hands_the_worktree_back() {
        let dir = scratch("unavailable");
        let source = repo("unavailable-repo");
        let worktree = Worktree::add(&source, &dir.join("wt")).expect("worktree");
        let checkout = worktree.path.clone();
        std::fs::remove_file(checkout.join("Cargo.toml")).expect("drop the manifest");
        let cargo = fake_cargo(&dir, "echo 'cargo 9.9.9 (fake)'");
        let prep = Prepared {
            cargo: &cargo,
            scratch_root: &dir,
            head12: HEAD12,
        };
        let exec = super::prepare_env_with(worktree, &prep).expect("no toolchain is not an error");
        let super::Exec::Unavailable(reason) = exec else {
            panic!("a root without a manifest is Unavailable");
        };
        assert!(reason.contains("Cargo.toml"), "{reason}");
        assert!(!checkout.exists(), "the worktree is handed back at once");
        assert!(
            !dir.join(format!("target-{HEAD12}")).exists(),
            "no scratch target for a run that cannot happen"
        );
    }

    #[test]
    fn off_and_unavailable_hold_nothing_and_finish_cleanly() {
        let off = super::Exec::Off;
        assert!(!off.allowed(), "no --allow-exec, no exec tiers");
        assert!(off.cargo_version().is_none());
        assert!(off.env().is_none());
        off.finish().expect("Off holds nothing to remove");

        let missing = super::Exec::Unavailable("no Rust toolchain: none here".to_owned());
        assert!(
            missing.allowed(),
            "the flag WAS given; the machine could not honour it, which is a different row"
        );
        assert!(missing.cargo_version().is_none());
        assert!(missing.env().is_none());
        missing
            .finish()
            .expect("Unavailable holds nothing to remove");
    }
}
