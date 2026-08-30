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

/// One crossing's edit: which file, what it held, and the bytes to replace.
pub struct Splice<'a> {
    pub path: &'a Path,
    pub original: &'a str,
    pub span: std::ops::Range<usize>,
}

/// `original` with `span` replaced by `fill`, every other byte identical —
/// test modules included, because tier 7 runs them.
#[must_use]
pub fn spliced(splice: &Splice, fill: &str) -> String {
    let mut out = String::with_capacity(splice.original.len() + fill.len());
    out.push_str(&splice.original[..splice.span.start]);
    out.push_str(fill);
    out.push_str(&splice.original[splice.span.end..]);
    out
}

/// The splice, written. No other file in the worktree is touched.
pub fn apply(splice: &Splice, fill: &str) -> Result<(), ChekovError> {
    std::fs::write(splice.path, spliced(splice, fill))
        .map_err(|e| ChekovError::io(format!("writing {}", splice.path.display()), e))
}

/// The first `error` diagnostic in a `--message-format=json` stream, as
/// `<file>:<line>: <message>`.
///
/// Warnings are ignored — a fill that compiles with warnings compiles — and a
/// line that is not JSON is ignored, because cargo interleaves plain progress
/// text on the same stream. The diagnostics, not the exit status, are the
/// verdict: cargo exits non-zero for things it also reports, and the stream is
/// the auditable record.
#[must_use]
pub fn first_error(stdout: &str) -> Option<String> {
    stdout.lines().find_map(error_line)
}

fn error_line(line: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    let message = value.get("message")?;
    if message.get("level")?.as_str()? != "error" {
        return None;
    }
    let text = message.get("message")?.as_str()?;
    Some(match primary_span(message) {
        Some((file, at)) => format!("{file}:{at}: {text}"),
        None => text.to_owned(),
    })
}

fn primary_span(message: &serde_json::Value) -> Option<(String, u64)> {
    let span = message
        .get("spans")?
        .as_array()?
        .iter()
        .find(|s| s.get("is_primary").and_then(serde_json::Value::as_bool) == Some(true))?;
    Some((
        span.get("file_name")?.as_str()?.to_owned(),
        span.get("line_start")?.as_u64()?,
    ))
}

/// cargo's own line when `--offline` is what stopped it, or `None`.
///
/// This is never retried online: the run fetched once before the loop, and a
/// crossing that still wants the registry is a skip with cargo's words, not a
/// second trip to the network mid-benchmark.
#[must_use]
pub fn needs_network(stderr: &str) -> Option<String> {
    const MARKERS: [&str; 4] = [
        "--offline",
        "failed to download",
        "no matching package named",
        "unable to get packages from source",
    ];
    stderr
        .lines()
        .map(str::trim)
        .find(|line| MARKERS.iter().any(|marker| line.contains(marker)))
        .map(str::to_owned)
}

/// `git checkout -- F`, then the bytes back.
///
/// A revert that does not restore aborts the run: every later crossing would
/// be measured against a file nobody can vouch for, and a benchmark that
/// cannot say what it compiled has measured nothing.
pub fn revert(env: &Env, file: &str, original: &str) -> Result<(), ChekovError> {
    super::tree::git(
        &env.worktree.path,
        &["checkout", "--", file],
        "git checkout (undo the tier-6 splice)",
    )?;
    let path = env.worktree.path.join(file);
    let now = std::fs::read_to_string(&path)
        .map_err(|e| ChekovError::io(format!("re-reading {}", path.display()), e))?;
    if now == original {
        return Ok(());
    }
    Err(ChekovError::ExecWorktreeDirty {
        path: env.worktree.path.clone(),
        file: file.to_owned(),
    })
}

/// At most this many covering tests are run for one crossing (spec §4).
///
/// Tier 7's question is whether the fill kept the code working, not how much
/// of the suite it survives; five is enough to answer it inside the timeout.
pub const CAP: usize = 5;

/// The crate a masked file belongs to.
pub struct Crate {
    pub name: String,
    pub root: PathBuf,
}

/// Only the two keys tier 7 needs, out of a manifest chekov does not own.
///
/// No `deny_unknown_fields` here, unlike every struct chekov defines the
/// schema for: this one reads someone else's file, and a manifest with a
/// `[dependencies]` table is not a schema error.
#[derive(serde::Deserialize)]
struct Manifest {
    package: Option<ManifestPackage>,
}

#[derive(serde::Deserialize)]
struct ManifestPackage {
    name: String,
}

/// The nearest `Cargo.toml` at or above `file` with a `[package] name`.
///
/// A virtual workspace root has no `[package]`, so a file with no nearer
/// manifest belongs to no crate and tier 7 records `no crate` — `-p` needs a
/// package name, and inventing one would run the wrong tests.
#[must_use]
pub fn crate_of(worktree: &Path, file: &str) -> Option<Crate> {
    let mut dir = worktree.join(file).parent()?.to_path_buf();
    loop {
        if let Some(name) = package_name(&dir.join("Cargo.toml")) {
            return Some(Crate { name, root: dir });
        }
        if dir == worktree || !dir.pop() {
            return None;
        }
    }
}

fn package_name(manifest: &Path) -> Option<String> {
    let text = std::fs::read_to_string(manifest).ok()?;
    let parsed: Manifest = toml::from_str(&text).ok()?;
    parsed.package.map(|p| p.name)
}

/// `#[test]` functions in the crate whose body mentions one of `symbols` as a
/// whole word outside literals and comments. File order, capped at `CAP`.
///
/// The walk covers `tests/` too: the task set's own walk skips it on purpose
/// (masking an assertion measures nothing), and that is exactly where an
/// integration test covering the masked symbol lives.
#[must_use]
pub fn covering_tests(root: &Path, symbols: &[String]) -> Vec<String> {
    let mut found = Vec::new();
    for (_, text) in crate_rust_files(root) {
        tests_in(&text, symbols, &mut found);
        if found.len() >= CAP {
            found.truncate(CAP);
            return found;
        }
    }
    found
}

/// Every `*.rs` under the crate — `tests/` included, `target/` and `.git/`
/// excluded — as `(relative path, text)`, sorted by path.
fn crate_rust_files(root: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    walk_crate(root, root, &mut out);
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn walk_crate(root: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if !matches!(name.as_str(), "target" | ".git") {
                walk_crate(root, &path, out);
            }
        } else {
            // `take_rs` answers `None` for a non-`.rs` file or an unreadable
            // one; neither is worth reporting, and the `Option` is `must_use`.
            let _ = take_rs(root, &path, out);
        }
    }
}

fn take_rs(root: &Path, path: &Path, out: &mut Vec<(String, String)>) -> Option<()> {
    if !path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("rs"))
    {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    let relative = path.strip_prefix(root).ok()?.to_string_lossy().into_owned();
    out.push((relative, text));
    Some(())
}

/// One file's covering tests, appended in source order.
fn tests_in(text: &str, symbols: &[String], out: &mut Vec<String>) {
    let code = super::ladder::code_only(text);
    for at in test_attribute_offsets(&code) {
        let Some((name, body)) = test_fn_after(&code, at) else {
            continue;
        };
        if symbols.iter().any(|s| mentions(&code[body.clone()], s)) {
            out.push(name);
        }
        if out.len() >= CAP {
            return;
        }
    }
}

/// Every offset of a `#[test]` attribute at the start of a line.
///
/// Read from the literal-blanked text, so `"#[test]"` inside a string is not
/// one. `#[test]` is the whole trimmed line — `#[test_case]` is a different
/// attribute and does not match.
fn test_attribute_offsets(code: &str) -> Vec<usize> {
    let mut offsets = Vec::new();
    let mut at = 0;
    for line in code.split_inclusive('\n') {
        if line.trim() == "#[test]" {
            offsets.push(at + line.len());
        }
        at += line.len();
    }
    offsets
}

/// The `fn <name>` below a `#[test]`, and the byte range of its body.
///
/// Attribute lines and blank lines between the two are stepped over — an
/// `#[ignore]` under the `#[test]` does not stop it being a test — and
/// anything else ends the search.
fn test_fn_after(code: &str, from: usize) -> Option<(String, std::ops::Range<usize>)> {
    let mut at = from;
    loop {
        let line = &code[at..code[at..].find('\n').map_or(code.len(), |i| at + i + 1)];
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("fn ") {
            let end = rest
                .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .unwrap_or(rest.len());
            let open = at + code[at..].find('{')?;
            let close = super::masker::matching_close(code, open)?;
            return Some((rest[..end].to_owned(), open + 1..close));
        }
        if !(trimmed.is_empty() || trimmed.starts_with('#')) {
            return None;
        }
        at += line.len();
        if at >= code.len() {
            return None;
        }
    }
}

/// `symbol` as a whole word somewhere in `code`.
fn mentions(code: &str, symbol: &str) -> bool {
    code.match_indices(symbol).any(|(at, _)| {
        let before = code[..at].chars().next_back();
        let after = code[at + symbol.len()..].chars().next();
        !before.is_some_and(word_char) && !after.is_some_and(word_char)
    })
}

const fn word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Tier 7's inputs (§4 — keeps `run_tests` at one parameter).
pub struct TestRun<'a> {
    pub env: &'a Env,
    pub krate: &'a str,
    pub tests: &'a [String],
}

/// What tier 7 saw. `Skipped` is never a fail: a timeout, an offline
/// registry, or a test module that will not build are all things the fill
/// cannot be blamed for.
#[derive(Debug)]
pub enum TestVerdict {
    Passed,
    Failed(String),
    Skipped(String),
}

/// Every candidate, stopping at the first that does not pass.
///
/// Tier 7 passes only when all of them pass, so there is nothing to learn
/// from running the rest — and the timeout budget is per invocation.
#[must_use]
pub fn run_tests(run: &TestRun) -> (TestVerdict, f64) {
    let mut spent = 0.0;
    for name in run.tests {
        let (verdict, secs) = one_test(run, name);
        spent += secs;
        if !matches!(verdict, TestVerdict::Passed) {
            return (verdict, spent);
        }
    }
    (TestVerdict::Passed, spent)
}

fn one_test(run: &TestRun, name: &str) -> (TestVerdict, f64) {
    let timeout = run.env.timeouts.test;
    let outcome = run_cargo(&CargoRun {
        program: &run.env.cargo,
        args: &["test", "-p", run.krate, "--offline", "--", name, "--exact"],
        cwd: &run.env.worktree.path,
        target_dir: &run.env.target_dir,
        timeout,
    });
    let Ok(outcome) = outcome else {
        return (
            TestVerdict::Skipped(format!("cargo test failed to run: {name}")),
            0.0,
        );
    };
    (test_verdict(&outcome, name, timeout), outcome.secs)
}

fn test_verdict(outcome: &CargoOutcome, name: &str, timeout: Duration) -> TestVerdict {
    if outcome.timed_out {
        return TestVerdict::Skipped(format!("test timed out after {} s", timeout.as_secs()));
    }
    if let Some(line) = needs_network(&outcome.stderr) {
        return TestVerdict::Skipped(format!("needs network: {line}"));
    }
    if outcome.status == Some(0) {
        return TestVerdict::Passed;
    }
    TestVerdict::Failed(format!("{name}: {}", failure_text(outcome)))
}

/// The most useful line cargo left behind, or a plain statement that it left
/// none — never an empty string standing in for a reason.
fn failure_text(outcome: &CargoOutcome) -> String {
    outcome
        .stdout
        .lines()
        .chain(outcome.stderr.lines())
        .map(str::trim)
        .find(|line| line.contains("FAILED") || line.starts_with("error"))
        .map_or_else(
            || {
                format!(
                    "cargo test exited {:?} with no failure line",
                    outcome.status
                )
            },
            str::to_owned,
        )
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

    fn splice_of<'a>(
        path: &'a Path,
        original: &'a str,
        span: std::ops::Range<usize>,
    ) -> super::Splice<'a> {
        super::Splice {
            path,
            original,
            span,
        }
    }

    #[test]
    fn a_splice_replaces_the_span_and_leaves_every_other_byte_alone() {
        let original = "fn f() {\n    let a = 1;\n}\n\n#[cfg(test)]\nmod t {\n    fn q() {}\n}\n";
        let at = original.find("let a = 1;").expect("the span");
        let out = super::spliced(
            &splice_of(Path::new("/nowhere"), original, at..at + 10),
            "let a = 2;",
        );
        assert_eq!(out, original.replace("let a = 1;", "let a = 2;"));
        assert!(
            out.contains("#[cfg(test)]"),
            "the test module is intact: {out}"
        );
    }

    #[test]
    fn a_span_at_byte_zero_and_a_span_at_eof_both_splice() {
        let original = "abcdef";
        let head = super::spliced(&splice_of(Path::new("/n"), original, 0..3), "XY");
        assert_eq!(head, "XYdef");
        let tail = super::spliced(&splice_of(Path::new("/n"), original, 6..6), "Z");
        assert_eq!(tail, "abcdefZ");
    }

    #[test]
    fn the_first_error_wins_and_warnings_are_ignored() {
        let stream = concat!(
            r#"{"reason":"compiler-artifact","package_id":"x"}"#,
            "\n",
            r#"{"reason":"compiler-message","message":{"level":"warning","message":"unused","spans":[{"file_name":"src/a.rs","line_start":3,"is_primary":true}]}}"#,
            "\n",
            r#"{"reason":"compiler-message","message":{"level":"error","message":"mismatched types","spans":[{"file_name":"src/b.rs","line_start":42,"is_primary":true}]}}"#,
            "\n",
        );
        assert_eq!(
            super::first_error(stream).as_deref(),
            Some("src/b.rs:42: mismatched types")
        );
    }

    #[test]
    fn warnings_alone_are_a_pass_and_malformed_lines_are_ignored() {
        let stream = concat!(
            "warning: this line is not JSON at all\n",
            "{ not json either\n",
            r#"{"reason":"compiler-message","message":{"level":"warning","message":"unused","spans":[]}}"#,
            "\n",
        );
        assert_eq!(super::first_error(stream), None);
    }

    /// A fill can break a caller in another file — that IS the point of the
    /// cross-file tier — so an error anywhere in the workspace counts.
    #[test]
    fn an_error_in_another_file_still_counts() {
        let stream = concat!(
            r#"{"reason":"compiler-message","message":{"level":"error","message":"no method `zap`","spans":[{"file_name":"src/caller.rs","line_start":9,"is_primary":true}]}}"#,
            "\n",
        );
        assert_eq!(
            super::first_error(stream).as_deref(),
            Some("src/caller.rs:9: no method `zap`")
        );
    }

    /// An error with no primary span is still an error.
    #[test]
    fn an_error_without_a_primary_span_keeps_its_message() {
        let stream = r#"{"reason":"compiler-message","message":{"level":"error","message":"linking failed","spans":[]}}"#;
        assert_eq!(
            super::first_error(stream).as_deref(),
            Some("linking failed")
        );
    }

    #[test]
    fn cargos_offline_complaint_is_recognised_and_quoted() {
        let stderr = "    Updating crates.io index\nerror: no matching package named `serde` \
                      found\nperhaps you meant to use --offline\n";
        let found = super::needs_network(stderr).expect("cargo said it needed the registry");
        assert!(found.contains("no matching package named"), "{found}");
        assert_eq!(super::needs_network("error: mismatched types\n"), None);
    }

    /// A tiny crate on disk: `Cargo.toml`, a `src/` file, and whatever else
    /// the test wants.
    fn crate_fixture(name: &str, files: &[(&str, &str)]) -> PathBuf {
        let dir = scratch(name);
        std::fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"widget\"\nversion = \"0.1.0\"\n",
        )
        .expect("manifest");
        for (path, text) in files {
            let full = dir.join(path);
            if let Some(parent) = full.parent() {
                std::fs::create_dir_all(parent).expect("parent");
            }
            std::fs::write(full, text).expect("file");
        }
        dir
    }

    #[test]
    fn the_crate_is_the_nearest_manifest_with_a_package_name() {
        let root = crate_fixture("crate-of", &[("src/deep/a.rs", "fn f() {}\n")]);
        let found = super::crate_of(&root, "src/deep/a.rs").expect("a crate");
        assert_eq!(found.name, "widget");
        assert_eq!(found.root, root);
    }

    /// A virtual workspace root has no `[package]`, so a file under one with
    /// no nearer manifest belongs to no crate.
    #[test]
    fn a_workspace_root_without_a_package_is_no_crate() {
        let dir = scratch("virtual-workspace");
        std::fs::create_dir_all(dir.join("src")).expect("src");
        std::fs::write(dir.join("Cargo.toml"), "[workspace]\nmembers = [\"a\"]\n")
            .expect("manifest");
        std::fs::write(dir.join("src/a.rs"), "fn f() {}\n").expect("a.rs");
        assert!(super::crate_of(&dir, "src/a.rs").is_none());
    }

    #[test]
    fn a_covering_test_is_found_inline_and_in_the_tests_directory() {
        let root = crate_fixture(
            "covering",
            &[
                (
                    "src/lib.rs",
                    "pub fn alpha() -> u8 { 1 }\npub fn beta() -> u8 { 2 }\n\n\
                     #[cfg(test)]\nmod t {\n    #[test]\n    fn covers_alpha() {\n        \
                     assert_eq!(super::alpha(), 1);\n    }\n    #[test]\n    fn covers_beta() \
                     {\n        assert_eq!(super::beta(), 2);\n    }\n}\n",
                ),
                (
                    "tests/outer.rs",
                    "#[test]\nfn integration_alpha() {\n    assert_eq!(widget::alpha(), 1);\n}\n",
                ),
            ],
        );
        let found = super::covering_tests(&root, &["alpha".to_owned()]);
        assert_eq!(
            found,
            vec!["covers_alpha".to_owned(), "integration_alpha".to_owned()]
        );
        assert!(super::covering_tests(&root, &["gamma".to_owned()]).is_empty());
    }

    /// `#[test]` with an attribute between it and the `fn` still counts.
    #[test]
    fn an_attribute_between_the_test_marker_and_the_fn_does_not_hide_it() {
        let root = crate_fixture(
            "adjacency",
            &[(
                "src/lib.rs",
                "pub fn alpha() {}\n#[test]\n#[ignore]\nfn covers_alpha() { alpha(); }\n",
            )],
        );
        assert_eq!(
            super::covering_tests(&root, &["alpha".to_owned()]),
            vec!["covers_alpha".to_owned()]
        );
    }

    /// A mention inside a string or a comment is prose, not a call.
    #[test]
    fn a_symbol_named_only_in_a_literal_or_a_comment_does_not_cover() {
        let root = crate_fixture(
            "prose",
            &[(
                "src/lib.rs",
                "pub fn alpha() {}\n#[test]\nfn mentions_alpha() {\n    // alpha is nice\n    \
                 let s = \"alpha\";\n    assert!(!s.is_empty());\n}\n",
            )],
        );
        assert!(super::covering_tests(&root, &["alpha".to_owned()]).is_empty());
    }

    /// Whole words only: `alphabet` is not `alpha`.
    #[test]
    fn a_longer_identifier_that_merely_contains_the_symbol_does_not_cover() {
        let root = crate_fixture(
            "whole-word",
            &[(
                "src/lib.rs",
                "pub fn alpha() {}\n#[test]\nfn t() {\n    let alphabet = 1;\n    \
                 assert_eq!(alphabet, 1);\n}\n",
            )],
        );
        assert!(super::covering_tests(&root, &["alpha".to_owned()]).is_empty());
    }

    #[test]
    fn the_candidates_stop_at_five_in_file_order() {
        use std::fmt::Write;
        let body = (0..8).fold(String::new(), |mut out, i| {
            let _ = write!(out, "#[test]\nfn covers_{i}() {{ alpha(); }}\n");
            out
        });
        let root = crate_fixture(
            "cap",
            &[("src/lib.rs", &format!("pub fn alpha() {{}}\n{body}"))],
        );
        let found = super::covering_tests(&root, &["alpha".to_owned()]);
        assert_eq!(found.len(), super::CAP);
        assert_eq!(found[0], "covers_0", "file order, not hash order");
        assert_eq!(found[4], "covers_4");
    }

    /// An `Env` over a plain directory and a named fake cargo — the runner
    /// tests need the paths and the timeouts, and nothing git does.
    fn env_for(dir: &Path, cargo: PathBuf, timeouts: Timeouts) -> super::Env {
        super::Env {
            worktree: Worktree::detached_for_test(dir.join("tree")),
            cargo,
            target_dir: dir.join("target"),
            cargo_version: "cargo 1.95.0".to_owned(),
            timeouts,
        }
    }

    #[test]
    fn every_candidate_must_pass_for_tier_seven_to_pass() {
        let dir = scratch("tests-pass");
        std::fs::create_dir_all(dir.join("tree")).expect("tree");
        let cargo = fake_cargo(&dir, "exit 0");
        let env = env_for(
            &dir,
            cargo,
            Timeouts {
                check: Duration::from_secs(5),
                test: Duration::from_secs(5),
            },
        );
        let (verdict, secs) = super::run_tests(&super::TestRun {
            env: &env,
            krate: "widget",
            tests: &["covers_alpha".to_owned(), "covers_beta".to_owned()],
        });
        assert!(matches!(verdict, super::TestVerdict::Passed), "{verdict:?}");
        assert!(secs >= 0.0);
    }

    #[test]
    fn the_first_failing_candidate_is_named_with_cargos_text() {
        let dir = scratch("tests-fail");
        std::fs::create_dir_all(dir.join("tree")).expect("tree");
        let cargo = fake_cargo(&dir, "echo 'test covers_alpha ... FAILED'\nexit 101");
        let env = env_for(
            &dir,
            cargo,
            Timeouts {
                check: Duration::from_secs(5),
                test: Duration::from_secs(5),
            },
        );
        let (verdict, _) = super::run_tests(&super::TestRun {
            env: &env,
            krate: "widget",
            tests: &["covers_alpha".to_owned()],
        });
        let super::TestVerdict::Failed(text) = verdict else {
            panic!("expected a failure, got {verdict:?}");
        };
        assert!(text.starts_with("covers_alpha: "), "{text}");
        assert!(text.contains("FAILED"), "{text}");
    }

    /// A hanging test under a bad fill is information, not a fail.
    #[test]
    fn a_test_past_its_timeout_is_skipped_and_never_failed() {
        let dir = scratch("tests-timeout");
        std::fs::create_dir_all(dir.join("tree")).expect("tree");
        let cargo = fake_cargo(&dir, "sleep 30 &\nwait");
        let env = env_for(
            &dir,
            cargo,
            Timeouts {
                check: Duration::from_secs(5),
                test: Duration::from_millis(300),
            },
        );
        let (verdict, _) = super::run_tests(&super::TestRun {
            env: &env,
            krate: "widget",
            tests: &["covers_alpha".to_owned()],
        });
        let super::TestVerdict::Skipped(reason) = verdict else {
            panic!("expected a skip, got {verdict:?}");
        };
        assert!(reason.starts_with("test timed out after "), "{reason}");
    }

    /// A committed crate whose `src/a.rs` bytes the revert test knows, a
    /// worktree over it, and the `Env` the revert acts on — with the file's
    /// text as the worktree holds it. `repo` writes a different file, so this
    /// builds its own.
    fn revert_fixture(dir: &Path) -> (super::Env, String) {
        let repo = dir.join("repo");
        std::fs::create_dir_all(repo.join("src")).expect("src");
        std::fs::write(repo.join("Cargo.toml"), "[package]\nname = \"x\"\n").expect("manifest");
        std::fs::write(repo.join("src/a.rs"), "fn f() {\n    let a = 1;\n}\n").expect("a.rs");
        let who = ["-c", "user.email=t@t", "-c", "user.name=t"];
        git(&repo, &["init", "-q"]);
        git(&repo, &[&who[..], &["add", "-A"]].concat());
        git(&repo, &[&who[..], &["commit", "-qm", "fixture"]].concat());
        let worktree = Worktree::add(&repo, &dir.join("tree")).expect("worktree");
        let original =
            std::fs::read_to_string(worktree.path.join("src/a.rs")).expect("the original");
        let env = super::Env {
            worktree,
            cargo: PathBuf::from("cargo"),
            target_dir: dir.join("target"),
            cargo_version: "cargo 1.95.0".to_owned(),
            timeouts: Timeouts::DEFAULT,
        };
        (env, original)
    }

    /// A worktree the revert restores, and one it cannot: the second is the
    /// abort, and it names the file.
    #[test]
    fn a_revert_restores_the_file_and_a_failure_to_restore_aborts() {
        let dir = scratch("revert");
        let (env, original) = revert_fixture(&dir);
        let path = env.worktree.path.join("src/a.rs");
        super::apply(
            &super::Splice {
                path: &path,
                original: &original,
                span: 9..19,
            },
            "let a = 2;",
        )
        .expect("apply");
        assert_ne!(
            std::fs::read_to_string(&path).expect("read"),
            original,
            "the splice landed"
        );
        super::revert(&env, "src/a.rs", &original).expect("the revert restores");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), original);

        // A file whose committed content is not what we claim it was: the
        // checkout succeeds and the bytes still differ, which is the abort.
        let wrong = format!("{original}// drifted\n");
        let err = super::revert(&env, "src/a.rs", &wrong)
            .expect_err("a worktree that will not restore stops the run");
        let text = err.to_string();
        assert!(text.contains("src/a.rs"), "{text}");
        assert!(text.contains("--resume"), "{text}");
        env.finish().expect("cleanup");
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
