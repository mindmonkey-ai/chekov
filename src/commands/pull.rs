//! `chekov pull <spec>` (§4.2).
//!
//! Resolve revision, download quant-matching files, snapshot the license,
//! register with defaults-seeded flags. Idempotent; a new revision never
//! repoints (that is `update`'s job).

use std::process::ExitCode;

use super::{Command, Ctx};
use crate::error::ChekovError;

#[derive(Debug, clap::Args)]
pub struct PullCmd {
    /// `org/repo[:QUANT][@rev]` or a huggingface.co URL.
    pub spec: String,
    /// Override the derived short name.
    #[arg(long)]
    pub name: Option<String>,
    /// Plan only: print what would be downloaded and registered.
    #[arg(long)]
    pub dry_run: bool,
    /// Also snapshot the base model's license from this URL.
    #[arg(long)]
    pub license_url: Option<String>,
}

/// Everything needed to register one pulled model.
#[derive(Debug, Clone)]
pub struct NewModel {
    pub name: String,
    pub repo: String,
    pub quant: String,
    pub sha: String,
    pub first_shard: String,
}

impl NewModel {
    /// `models/<name>@<rev12>` — one directory per model@revision (§4.2).
    #[must_use]
    pub fn dir_name(&self) -> String {
        todo!("cycle 5b red")
    }

    /// The registry entry this pull produces (hermes_ok seeded true; flags
    /// inherit `[defaults]` at resolve time — concatenation semantics, §4.3).
    #[must_use]
    pub fn entry(&self) -> crate::core::registry::ModelEntry {
        todo!("cycle 5b red")
    }
}

/// `LICENSE.provenance` content: where the snapshot came from and when.
#[must_use]
pub fn provenance_text(model: &NewModel, source: &str, fetched_utc: &str) -> String {
    let _ = (model, source, fetched_utc);
    todo!("cycle 5b red")
}

impl Command for PullCmd {
    fn run(&self, ctx: &Ctx) -> Result<ExitCode, ChekovError> {
        let _ = ctx;
        todo!("cycle 5b red")
    }
}

#[cfg(test)]
mod tests {
    use super::{NewModel, provenance_text};

    fn model() -> NewModel {
        NewModel {
            name: "minimax-m2.7".into(),
            repo: "unsloth/MiniMax-M2.7-GGUF".into(),
            quant: "UD-Q5_K_XL".into(),
            sha: "0123456789abcdef0123456789abcdef01234567".into(),
            first_shard: "UD-Q5_K_XL/MiniMax-M2.7-UD-Q5_K_XL-00001-of-00004.gguf".into(),
        }
    }

    #[test]
    fn dir_name_is_name_at_rev12() {
        assert_eq!(model().dir_name(), "minimax-m2.7@0123456789ab");
    }

    #[test]
    fn entry_pins_full_revision_and_relative_path() {
        let entry = model().entry();
        assert_eq!(entry.revision, "0123456789abcdef0123456789abcdef01234567");
        assert_eq!(entry.path, "models/minimax-m2.7@0123456789ab");
        assert!(entry.hermes_ok, "pulled models default to hermes_ok");
        assert!(entry.extra_flags.is_empty(), "flags inherit [defaults]");
    }

    #[test]
    fn provenance_records_repo_revision_source_and_time() {
        let text = provenance_text(&model(), "https://huggingface.co/x/LICENSE", "20260815T120000Z");
        for needle in [
            "unsloth/MiniMax-M2.7-GGUF",
            "0123456789abcdef0123456789abcdef01234567",
            "https://huggingface.co/x/LICENSE",
            "20260815T120000Z",
        ] {
            assert!(text.contains(needle), "missing {needle}: {text}");
        }
    }
}
