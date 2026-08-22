//! llama-server lifecycle: pidfile, launch arguments, signal escalation.
//!
//! State lives in a typed `PidFile` (§13.4/§C.4); every failure is a typed
//! `Result` (§C.2). No async runtime — blocking waits with a deadline.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::core::config::Config;
use crate::core::registry::Effective;
use crate::error::ChekovError;

/// The pidfile at `logs/chekov.pid` — presence + liveness defines "running".
#[derive(Debug, Clone)]
pub struct PidFile {
    path: PathBuf,
}

/// How a stop ended: clean SIGTERM exit, or SIGKILL escalation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopOutcome {
    Terminated,
    Killed,
}

impl PidFile {
    #[must_use]
    pub const fn new(path: PathBuf) -> Self {
        Self { path }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The recorded pid, if the file exists and parses. Garbage → `None`
    /// (treated as stale, never trusted).
    #[must_use]
    pub fn read(&self) -> Option<i32> {
        std::fs::read_to_string(&self.path)
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    pub fn write(&self, pid: i32) -> Result<(), ChekovError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ChekovError::io(format!("creating {}", parent.display()), e))?;
        }
        std::fs::write(&self.path, format!("{pid}\n"))
            .map_err(|e| ChekovError::io(format!("writing {}", self.path.display()), e))
    }

    pub fn remove(&self) -> Result<(), ChekovError> {
        if self.path.exists() {
            std::fs::remove_file(&self.path)
                .map_err(|e| ChekovError::io(format!("removing {}", self.path.display()), e))?;
        }
        Ok(())
    }
}

/// True when `pid` names a live process (signal-0 probe).
#[must_use]
pub fn process_alive(pid: i32) -> bool {
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok()
}

/// SIGTERM, wait up to `grace`, then SIGKILL with a warning to stderr.
pub fn stop_pid(pid: i32, grace: Duration) -> Result<StopOutcome, ChekovError> {
    use nix::sys::signal::{Signal, kill};
    let target = nix::unistd::Pid::from_raw(pid);
    if kill(target, Signal::SIGTERM).is_err() {
        return Ok(StopOutcome::Terminated); // already gone
    }
    let deadline = std::time::Instant::now() + grace;
    while std::time::Instant::now() < deadline {
        if !process_alive(pid) {
            return Ok(StopOutcome::Terminated);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    eprintln!("warning: pid {pid} ignored SIGTERM for {grace:?} — escalating to SIGKILL");
    let _ = kill(target, Signal::SIGKILL);
    Ok(StopOutcome::Killed)
}

/// The fully resolved llama-server argv (minus the program itself).
///
/// What `chekov show` prints and `chekov run` executes. Flag order: shard,
/// ctx, host/port/api-key, then registry flags (defaults ++ extra, §4.3).
#[must_use]
pub fn launch_args(cfg: &Config, eff: &Effective) -> Vec<String> {
    let mut args = vec![
        "-m".to_owned(),
        shard_path(cfg, eff).display().to_string(),
        "--ctx-size".to_owned(),
        eff.ctx_size.to_string(),
        "--host".to_owned(),
        cfg.file.server.host.clone(),
        "--port".to_owned(),
        cfg.file.server.port.to_string(),
        "--api-key".to_owned(),
        cfg.file.server.api_key.clone(),
    ];
    args.extend(eff.flags.iter().cloned());
    args
}

/// Absolute path to the model's first shard.
#[must_use]
pub fn shard_path(cfg: &Config, eff: &Effective) -> PathBuf {
    cfg.root.join(&eff.entry.path).join(&eff.entry.first_shard)
}

fn run_state_path(cfg: &Config) -> PathBuf {
    cfg.logs_dir().join("chekov.model")
}

/// Record which model the daemon is serving (read by status/rm).
pub fn write_run_state(cfg: &Config, name: &str) -> Result<(), ChekovError> {
    let path = run_state_path(cfg);
    std::fs::write(&path, format!("{name}\n"))
        .map_err(|e| ChekovError::io(format!("writing {}", path.display()), e))
}

/// The model recorded as running, when a live server exists.
#[must_use]
pub fn read_run_state(cfg: &Config) -> Option<String> {
    let name = std::fs::read_to_string(run_state_path(cfg)).ok()?;
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

/// Remove the run-state marker (with the pidfile, on stop).
pub fn clear_run_state(cfg: &Config) -> Result<(), ChekovError> {
    let path = run_state_path(cfg);
    if path.exists() {
        std::fs::remove_file(&path)
            .map_err(|e| ChekovError::io(format!("removing {}", path.display()), e))?;
    }
    Ok(())
}

/// The live server's pid, if the pidfile names a running process.
#[must_use]
pub fn live_pid(cfg: &Config) -> Option<i32> {
    PidFile::new(cfg.pidfile())
        .read()
        .filter(|&pid| process_alive(pid))
}

fn server_command(cfg: &Config, eff: &Effective) -> std::process::Command {
    let binary = crate::core::engine::server_binary(&cfg.engine_dir());
    let mut cmd = std::process::Command::new(binary);
    cmd.args(launch_args(cfg, eff));
    cmd
}

/// Detached start: own process group, output appended to the server log,
/// pidfile written. The child outlives this process by design.
pub fn spawn_daemon(cfg: &Config, eff: &Effective) -> Result<i32, ChekovError> {
    use std::os::unix::process::CommandExt;
    std::fs::create_dir_all(cfg.logs_dir())
        .map_err(|e| ChekovError::io(format!("creating {}", cfg.logs_dir().display()), e))?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(cfg.server_log())
        .map_err(|e| ChekovError::io(format!("opening {}", cfg.server_log().display()), e))?;
    let log_err = log
        .try_clone()
        .map_err(|e| ChekovError::io("cloning log handle", e))?;
    let mut cmd = server_command(cfg, eff);
    cmd.stdin(std::process::Stdio::null())
        .stdout(log)
        .stderr(log_err)
        .process_group(0);
    let child = cmd
        .spawn()
        .map_err(|e| ChekovError::io("spawning llama-server", e))?;
    let pid = i32::try_from(child.id()).unwrap_or(i32::MAX);
    PidFile::new(cfg.pidfile()).write(pid)?;
    // Intentionally not reaped: chekov exits immediately and the daemon is
    // adopted by launchd.
    drop(child);
    Ok(pid)
}

/// Foreground start: inherits the terminal; returns the server's exit status.
pub fn run_foreground(
    cfg: &Config,
    eff: &Effective,
) -> Result<std::process::ExitCode, ChekovError> {
    let status = server_command(cfg, eff)
        .status()
        .map_err(|e| ChekovError::io("spawning llama-server", e))?;
    Ok(if status.success() {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::FAILURE
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{PidFile, StopOutcome, launch_args, process_alive, stop_pid};
    use crate::core::config::Config;
    use crate::core::registry::{ModelEntry, Registry};

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("chekov-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    fn effective() -> (Config, crate::core::registry::Effective) {
        let root = scratch("srv-args");
        let cfg = Config::load(&root).expect("defaults");
        let mut reg = Registry::default();
        reg.models.insert(
            "m".into(),
            ModelEntry {
                repo: "org/repo".into(),
                quant: "Q8_0".into(),
                revision: "abc".into(),
                path: "models/m@abc".into(),
                first_shard: "m-Q8_0.gguf".into(),
                hermes_ok: false,
                ctx_size: None,
                extra_flags: vec!["--temp".into(), "1.0".into()],
            },
        );
        (cfg, reg.effective("m").expect("registered"))
    }

    #[test]
    fn pidfile_roundtrips_and_rejects_garbage() {
        let dir = scratch("srv-pidfile");
        let pf = PidFile::new(dir.join("chekov.pid"));
        assert_eq!(pf.read(), None);
        pf.write(4242).expect("write");
        assert_eq!(pf.read(), Some(4242));
        std::fs::write(pf.path(), "not-a-pid").expect("corrupt");
        assert_eq!(pf.read(), None);
        pf.remove().expect("remove");
        assert_eq!(pf.read(), None);
    }

    #[test]
    fn liveness_probe_sees_self_not_ghost() {
        let own = i32::try_from(std::process::id()).expect("pid fits");
        assert!(process_alive(own));
        assert!(!process_alive(99_999_999));
    }

    /// Reap `child` in the background so it never lingers as a zombie —
    /// `kill(pid, 0)` reports zombies as alive, which would defeat the poll.
    fn reap(child: std::process::Child) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            let mut child = child;
            let _ = child.wait();
        })
    }

    #[test]
    fn stop_terminates_a_cooperative_process() {
        let child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep");
        let pid = i32::try_from(child.id()).expect("pid fits");
        let reaper = reap(child);
        let outcome = stop_pid(pid, Duration::from_secs(5)).expect("stop");
        assert_eq!(outcome, StopOutcome::Terminated);
        reaper.join().expect("reaper");
    }

    #[test]
    fn stop_escalates_when_sigterm_is_ignored() {
        let child = std::process::Command::new("/bin/sh")
            .args(["-c", "trap '' TERM; sleep 30"])
            .spawn()
            .expect("spawn trap");
        let pid = i32::try_from(child.id()).expect("pid fits");
        std::thread::sleep(Duration::from_millis(200)); // let the trap install
        let reaper = reap(child);
        let outcome = stop_pid(pid, Duration::from_millis(400)).expect("stop");
        assert_eq!(outcome, StopOutcome::Killed);
        reaper.join().expect("reaper");
    }

    #[test]
    fn shard_path_passes_absolute_entry_paths_through() {
        let (cfg, mut eff) = effective();
        eff.entry.path = "/Volumes/external/models/m@abc".into();
        let shard = super::shard_path(&cfg, &eff);
        assert_eq!(
            shard,
            std::path::PathBuf::from("/Volumes/external/models/m@abc/m-Q8_0.gguf")
        );
    }

    #[test]
    fn launch_args_resolve_shard_ctx_and_concatenated_flags() {
        let (cfg, eff) = effective();
        let args = launch_args(&cfg, &eff).join(" ");
        assert!(
            args.contains("models/m@abc/m-Q8_0.gguf"),
            "shard missing: {args}"
        );
        assert!(args.contains("--ctx-size 98304"), "ctx missing: {args}");
        assert!(args.contains("--port 8080"), "port missing: {args}");
        assert!(args.contains("-np 1"), "single slot missing: {args}");
        let jinja = args.find("--jinja").expect("default flag");
        let temp = args.find("--temp").expect("extra flag");
        assert!(jinja < temp, "defaults must precede extras: {args}");
    }
}
