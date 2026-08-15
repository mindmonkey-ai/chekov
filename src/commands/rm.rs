//! `chekov rm <name>` — remove a model: confirmation required, refuses the
//! active or running model (§5 of the bootstrap prompt).

use std::process::ExitCode;

use super::{Command, Ctx};
use crate::error::ChekovError;

#[derive(Debug, clap::Args)]
pub struct RmCmd {
    /// Registered model name.
    pub name: String,
    /// Skip the interactive confirmation.
    #[arg(long)]
    pub yes: bool,
}

impl Command for RmCmd {
    fn run(&self, ctx: &Ctx) -> Result<ExitCode, ChekovError> {
        use crate::core::server;
        let mut reg = ctx.registry()?;
        if server::live_pid(&ctx.config).is_some()
            && server::read_run_state(&ctx.config).as_deref() == Some(self.name.as_str())
        {
            return Err(ChekovError::RemovalRefused {
                name: self.name.clone(),
                reason: "it is currently running (`chekov stop` first)".into(),
            });
        }
        if reg.active.as_deref() == Some(self.name.as_str()) {
            return Err(ChekovError::RemovalRefused {
                name: self.name.clone(),
                reason: "it is the active model".into(),
            });
        }
        super::confirm(
            &format!("remove model '{}' and delete its files", self.name),
            self.yes,
        )?;
        let entry = reg.remove(&self.name)?;
        let dir = ctx.config.root.join(&entry.path);
        if dir.exists() {
            std::fs::remove_dir_all(&dir)
                .map_err(|e| ChekovError::io(format!("removing {}", dir.display()), e))?;
        }
        reg.save(&ctx.config.registry_path())?;
        println!("removed '{}' ({})", self.name, dir.display());
        Ok(ExitCode::SUCCESS)
    }
}
