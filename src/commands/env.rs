//! `chekov env` — stdout-only exports for Claude Code; diagnostics on stderr
//! so `eval "$(chekov env)"` is always safe.

use std::process::ExitCode;

use super::{Command, Ctx};
use crate::error::ChekovError;

#[derive(Debug, clap::Args)]
pub struct EnvCmd {}

/// The export block, shell-quoted. Pure so tests pin the exact contract.
#[must_use]
pub fn render_exports(base_url: &str, api_key: &str, alias: &str) -> String {
    let _ = (base_url, api_key, alias);
    todo!("cycle 5a red")
}

impl Command for EnvCmd {
    fn run(&self, ctx: &Ctx) -> Result<ExitCode, ChekovError> {
        let _ = ctx;
        todo!("cycle 5a red")
    }
}

#[cfg(test)]
mod tests {
    use super::render_exports;

    #[test]
    fn exports_all_five_anthropic_variables() {
        let out = render_exports("http://127.0.0.1:8080", "sekrit", "minimax-m2.7");
        for var in [
            "ANTHROPIC_BASE_URL",
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_DEFAULT_OPUS_MODEL",
            "ANTHROPIC_DEFAULT_SONNET_MODEL",
            "ANTHROPIC_DEFAULT_HAIKU_MODEL",
        ] {
            assert!(out.contains(&format!("export {var}=")), "missing {var}: {out}");
        }
        assert!(out.contains("'http://127.0.0.1:8080'"), "unquoted url: {out}");
        assert!(out.contains("'minimax-m2.7'"), "alias missing: {out}");
    }
}
