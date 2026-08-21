//! Command pattern (§2.2 of the bootstrap prompt): one module per subcommand,
//! each implementing `Command` against a shared `Ctx`. Traits carry behavior
//! only (§C.6).

use std::path::PathBuf;
use std::process::ExitCode;

use crate::core::config::{Config, resolve_root};
use crate::core::hub::{HttpClient, UreqClient};
use crate::core::registry::Registry;
use crate::error::ChekovError;

pub mod doctor;
pub mod env;
pub mod integrate;
pub mod launch;
pub mod list;
pub mod pull;
pub mod restart;
pub mod rm;
pub mod run;
pub mod setup;
pub mod show;
pub mod status;
pub mod stop;
pub mod update;
pub mod use_;

/// Shared command context: resolved config + the HTTP seam (§8.2 — tests
/// construct one with a fake client and a scratch root).
pub struct Ctx {
    pub config: Config,
    pub http: Box<dyn HttpClient>,
}

impl Ctx {
    /// Production context: `$CHEKOV_HOME` (or `~/personal_dev/chekov`) + ureq.
    pub fn from_env() -> Result<Self, ChekovError> {
        let home = directories::UserDirs::new()
            .map_or_else(|| PathBuf::from("/"), |u| u.home_dir().to_path_buf());
        let env_home = std::env::var("CHEKOV_HOME").ok();
        let root = resolve_root(env_home.as_deref(), &home);
        Ok(Self {
            config: Config::load(&root)?,
            http: Box::new(UreqClient),
        })
    }

    /// Load the registry from this context's root.
    pub fn registry(&self) -> Result<Registry, ChekovError> {
        Registry::load(&self.config.registry_path())
    }
}

/// Every subcommand implements this (§2.2).
pub trait Command {
    fn run(&self, ctx: &Ctx) -> Result<ExitCode, ChekovError>;
}

/// Minimal aligned-columns table (hand-rolled instead of `tabled` — §9).
#[must_use]
pub fn render_table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate().take(widths.len()) {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }
    let fmt_row = |cells: &[&str]| -> String {
        cells
            .iter()
            .zip(&widths)
            .map(|(cell, w)| format!("{cell:<w$}"))
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_owned()
    };
    let mut out = fmt_row(headers);
    for row in rows {
        let cells: Vec<&str> = row.iter().map(String::as_str).collect();
        out.push('\n');
        out.push_str(&fmt_row(&cells));
    }
    out
}

/// Interactive yes/no gate for destructive actions; `assume_yes` skips it.
/// Anything but an explicit `y`/`yes` declines (§C.2 — loud, never default-yes).
pub fn confirm(action: &str, assume_yes: bool) -> Result<(), ChekovError> {
    if assume_yes {
        return Ok(());
    }
    eprint!("{action} — proceed? [y/N] ");
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|e| ChekovError::io("reading confirmation from stdin", e))?;
    match line.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => Ok(()),
        _ => Err(ChekovError::ConfirmationDeclined {
            action: action.to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::render_table;

    #[test]
    fn table_aligns_columns_and_headers() {
        let rows = vec![
            vec!["minimax-m2.7".to_owned(), "UD-Q5_K_XL".to_owned()],
            vec!["short".to_owned(), "Q8_0".to_owned()],
        ];
        let table = render_table(&["NAME", "QUANT"], &rows);
        let lines: Vec<&str> = table.lines().collect();
        assert_eq!(lines.len(), 3);
        let quant_col = lines[0].find("QUANT").expect("header present");
        assert_eq!(lines[1].find("UD-Q5_K_XL"), Some(quant_col), "{table}");
        assert_eq!(lines[2].find("Q8_0"), Some(quant_col), "{table}");
    }
}
