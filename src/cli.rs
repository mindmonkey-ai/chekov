//! clap derive tree + dispatch (§2.2): parsing is declarative, dispatch is one
//! exhaustive match (§C.5).

use std::process::ExitCode;

use clap::{CommandFactory, Parser, Subcommand};

use crate::commands::{self, Command as _, Ctx};
use crate::error::ChekovError;

#[derive(Debug, Parser)]
#[command(
    name = "chekov",
    version,
    about = "Local llama.cpp inference stack manager"
)]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Start llama-server for a model (backgrounds by default; --foreground blocks)
    Run(commands::run::RunCmd),
    /// Stop the running server (SIGTERM, then SIGKILL after 20s)
    Stop(commands::stop::StopCmd),
    /// Stop (if running) then start in the background; swaps models in one motion
    Restart(commands::restart::RestartCmd),
    /// Show server state, model, revision, uptime, wired limit
    Status(commands::status::StatusCmd),
    /// Report what this Mac is and what it can hold, with each number's provenance
    Capability(commands::capability::CapabilityCmd),
    /// Download and register a model: org/repo[:QUANT][@rev]
    Pull(commands::pull::PullCmd),
    /// List registered models
    List(commands::list::ListCmd),
    /// Set the active model
    Use(commands::use_::UseCmd),
    /// Remove a model and its files (refuses active/running)
    Rm(commands::rm::RmCmd),
    /// Print the resolved server invocation and license provenance
    Show(commands::show::ShowCmd),
    /// Run the five health checks against the live server
    Doctor(commands::doctor::DoctorCmd),
    /// Build llama.cpp (Metal) and verify the environment
    Setup(commands::setup::SetupCmd),
    /// Update the engine and/or re-resolve the active model
    Update(commands::update::UpdateCmd),
    /// Print ANTHROPIC_* exports for the local server (stdout only)
    Env(commands::env::EnvCmd),
    /// Wire up Hermes or Claude Code against the local server
    Integrate(commands::integrate::IntegrateCmd),
    /// Start an agent wired to the local model (proxy + settings; --proxy-only for the translator alone)
    Launch(commands::launch::LaunchCmd),
    /// Emit shell completions (used by `make install`).
    #[command(hide = true)]
    Completions {
        /// Target shell.
        shell: clap_complete::Shell,
    },
}

/// Parse argv, build the production context, dispatch.
pub fn run() -> Result<ExitCode, ChekovError> {
    let cli = Cli::parse();
    if let Cmd::Completions { shell } = cli.cmd {
        let mut cmd = Cli::command();
        clap_complete::generate(shell, &mut cmd, "chekov", &mut std::io::stdout());
        return Ok(ExitCode::SUCCESS);
    }
    let ctx = Ctx::from_env()?;
    dispatch(&cli.cmd, &ctx)
}

/// One exhaustive match from parsed args to command structs (§C.5).
pub fn dispatch(cmd: &Cmd, ctx: &Ctx) -> Result<ExitCode, ChekovError> {
    match cmd {
        Cmd::Run(c) => c.run(ctx),
        Cmd::Stop(c) => c.run(ctx),
        Cmd::Restart(c) => c.run(ctx),
        Cmd::Status(c) => c.run(ctx),
        Cmd::Pull(c) => c.run(ctx),
        Cmd::List(c) => c.run(ctx),
        Cmd::Use(c) => c.run(ctx),
        Cmd::Rm(c) => c.run(ctx),
        Cmd::Show(c) => c.run(ctx),
        Cmd::Capability(c) => c.run(ctx),
        Cmd::Doctor(c) => c.run(ctx),
        Cmd::Setup(c) => c.run(ctx),
        Cmd::Update(c) => c.run(ctx),
        Cmd::Env(c) => c.run(ctx),
        Cmd::Integrate(c) => c.run(ctx),
        Cmd::Launch(c) => c.run(ctx),
        Cmd::Completions { .. } => Ok(ExitCode::SUCCESS),
    }
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::{Cli, Cmd};

    #[test]
    fn clap_tree_is_internally_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_use_and_default_background_run() {
        let cli = <Cli as clap::Parser>::try_parse_from(["chekov", "use", "m"]).expect("parse");
        assert!(matches!(cli.cmd, Cmd::Use(ref c) if c.name == "m"));
        // A bare `run` now backgrounds by default: no --foreground flag set.
        let cli = <Cli as clap::Parser>::try_parse_from(["chekov", "run"]).expect("parse");
        assert!(matches!(cli.cmd, Cmd::Run(ref c) if !c.foreground && c.name.is_none()));
        // --foreground opts into the blocking path.
        let cli = <Cli as clap::Parser>::try_parse_from(["chekov", "run", "--foreground"])
            .expect("parse");
        assert!(matches!(cli.cmd, Cmd::Run(ref c) if c.foreground));
        // Legacy --daemon still parses (hidden no-op alias) and stays background.
        let cli =
            <Cli as clap::Parser>::try_parse_from(["chekov", "run", "--daemon"]).expect("parse");
        assert!(matches!(cli.cmd, Cmd::Run(ref c) if !c.foreground));
    }

    #[test]
    fn pull_accepts_model_loc() {
        let cli = <Cli as clap::Parser>::try_parse_from([
            "chekov",
            "pull",
            "org/repo:Q8_0",
            "--model-loc",
            "/Volumes/external/models",
        ])
        .expect("parse");
        assert!(matches!(cli.cmd, Cmd::Pull(ref c)
            if c.model_loc.as_deref() == Some(std::path::Path::new("/Volumes/external/models"))));
    }

    #[test]
    fn update_flags_parse_independently() {
        let cli = <Cli as clap::Parser>::try_parse_from(["chekov", "update", "--all", "--dry-run"])
            .expect("parse");
        assert!(matches!(cli.cmd, Cmd::Update(ref c) if c.all && c.dry_run && !c.engine));
    }
}
