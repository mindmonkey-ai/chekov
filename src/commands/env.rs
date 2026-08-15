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
    let pairs = [
        ("ANTHROPIC_BASE_URL", base_url),
        ("ANTHROPIC_AUTH_TOKEN", api_key),
        ("ANTHROPIC_DEFAULT_OPUS_MODEL", alias),
        ("ANTHROPIC_DEFAULT_SONNET_MODEL", alias),
        ("ANTHROPIC_DEFAULT_HAIKU_MODEL", alias),
    ];
    pairs
        .map(|(var, val)| format!("export {var}='{val}'\n"))
        .concat()
}

impl Command for EnvCmd {
    fn run(&self, ctx: &Ctx) -> Result<ExitCode, ChekovError> {
        let reg = ctx.registry()?;
        let alias = reg.active_name()?;
        let base = ctx.config.base_url();
        eprintln!("chekov env: pointing Anthropic clients at {base} as '{alias}'");
        print!(
            "{}",
            render_exports(&base, &ctx.config.file.server.api_key, alias)
        );
        Ok(ExitCode::SUCCESS)
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
            assert!(
                out.contains(&format!("export {var}=")),
                "missing {var}: {out}"
            );
        }
        assert!(
            out.contains("'http://127.0.0.1:8080'"),
            "unquoted url: {out}"
        );
        assert!(out.contains("'minimax-m2.7'"), "alias missing: {out}");
    }
}
