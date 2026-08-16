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
    /// Store the model under this directory instead of <root>/models.
    /// Files already present there (hf-cli layout) are size-verified and
    /// hard-linked instead of re-downloaded.
    #[arg(long)]
    pub model_loc: Option<std::path::PathBuf>,
}

/// Everything needed to register one pulled model.
#[derive(Debug, Clone)]
pub struct NewModel {
    pub name: String,
    pub repo: String,
    pub quant: String,
    pub sha: String,
    pub first_shard: String,
    /// `--model-loc`: absolute external home for the weights, when set.
    pub location: Option<std::path::PathBuf>,
}

impl NewModel {
    /// `models/<name>@<rev12>` — one directory per model@revision (§4.2).
    #[must_use]
    pub fn dir_name(&self) -> String {
        let rev12: String = self.sha.chars().take(12).collect();
        format!("{}@{rev12}", self.name)
    }

    /// The registry `path` value: relative `models/<dir>` by default, or the
    /// absolute `<model-loc>/<dir>` when a location is set. Consumers resolve
    /// via `root.join(path)`, which passes absolute paths through unchanged.
    #[must_use]
    pub fn registry_path(&self) -> String {
        self.location.as_ref().map_or_else(
            || format!("models/{}", self.dir_name()),
            |loc| loc.join(self.dir_name()).display().to_string(),
        )
    }

    /// The registry entry this pull produces (`hermes_ok` seeded true; flags
    /// inherit `[defaults]` at resolve time — concatenation semantics, §4.3).
    #[must_use]
    pub fn entry(&self) -> crate::core::registry::ModelEntry {
        crate::core::registry::ModelEntry {
            repo: self.repo.clone(),
            quant: self.quant.clone(),
            revision: self.sha.clone(),
            path: self.registry_path(),
            first_shard: self.first_shard.clone(),
            hermes_ok: true,
            ctx_size: None,
            extra_flags: vec![],
        }
    }
}

/// The memory budget quant choices are judged against: the live effective
/// wired limit (sysctl, 0-sentinel already resolved), falling back to the
/// configured requirement when sysctl is unreadable.
#[must_use]
pub(crate) fn wired_budget_mb(ctx: &Ctx) -> Option<u64> {
    crate::core::checks::wired_limit_mb()
        .map(|(actual, _is_default)| actual)
        .or(Some(ctx.config.file.limits.wired_limit_mb))
}

/// `LICENSE.provenance` content: where the snapshot came from and when.
#[must_use]
pub fn provenance_text(model: &NewModel, source: &str, fetched_utc: &str) -> String {
    format!(
        "repo = {}\nrevision = {}\nsource = {source}\nfetched = {fetched_utc}\n",
        model.repo, model.sha
    )
}

/// Download the plan's files into `models/<name>@<rev12>/`, then write the
/// `REVISION` file and license snapshot. Returns the model directory.
pub(crate) fn materialize(
    ctx: &Ctx,
    model: &NewModel,
    plan: &crate::core::hub::PullPlan,
) -> Result<std::path::PathBuf, ChekovError> {
    let dir = ctx.config.root.join(model.registry_path());
    std::fs::create_dir_all(&dir)
        .map_err(|e| ChekovError::io(format!("creating {}", dir.display()), e))?;
    crate::core::hub::download_plan(
        &crate::core::hub::DownloadSpec {
            repo: &model.repo,
            revision: &model.sha,
            dest: &dir,
            adopt_from: model.location.as_deref(),
        },
        plan,
    )?;
    std::fs::write(dir.join("REVISION"), format!("{}\n", model.sha))
        .map_err(|e| ChekovError::io("writing REVISION", e))?;
    snapshot_license(ctx, model, &dir)?;
    Ok(dir)
}

/// Snapshot the repo license (and optionally a base-model license) with
/// provenance. A repo without a license file is recorded as such, loudly.
fn snapshot_license(ctx: &Ctx, model: &NewModel, dir: &std::path::Path) -> Result<(), ChekovError> {
    let mut source = "none found (checked LICENSE, LICENSE.md, LICENSE.txt)".to_owned();
    for candidate in ["LICENSE", "LICENSE.md", "LICENSE.txt"] {
        let url = format!(
            "https://huggingface.co/{}/raw/{}/{candidate}",
            model.repo, model.sha
        );
        if let Ok(text) = ctx.http.get(&url) {
            std::fs::write(dir.join("LICENSE.snapshot"), text)
                .map_err(|e| ChekovError::io("writing LICENSE.snapshot", e))?;
            source = url;
            break;
        }
    }
    let stamp = crate::core::clock::utc_compact_now();
    std::fs::write(
        dir.join("LICENSE.provenance"),
        provenance_text(model, &source, &stamp),
    )
    .map_err(|e| ChekovError::io("writing LICENSE.provenance", e))
}

/// Fetch `--license-url` (base-model license) when given.
fn snapshot_base_license(ctx: &Ctx, url: &str, dir: &std::path::Path) -> Result<(), ChekovError> {
    let text = ctx.http.get(url)?;
    std::fs::write(dir.join("LICENSE.base.snapshot"), text)
        .map_err(|e| ChekovError::io("writing LICENSE.base.snapshot", e))
}

fn print_plan(model: &NewModel, plan: &crate::core::hub::PullPlan) {
    println!(
        "[dry-run] pull {}:{} @ {}",
        model.repo,
        model.quant,
        &model.sha[..12.min(model.sha.len())]
    );
    println!("[dry-run] destination: {}", model.registry_path());
    for file in &plan.files {
        println!(
            "[dry-run]   {} ({} bytes)",
            file.path,
            file.size.unwrap_or(0)
        );
    }
    println!("[dry-run] first shard: {}", plan.first_shard);
    println!(
        "[dry-run] would register '{}' and snapshot the license",
        model.name
    );
}

/// Re-pull of the same revision with the shard on disk is a verified no-op.
fn is_noop(ctx: &Ctx, model: &NewModel, existing_rev: Option<&str>) -> bool {
    existing_rev == Some(model.sha.as_str())
        && ctx
            .config
            .root
            .join(model.entry().path)
            .join(&model.first_shard)
            .exists()
}

/// Register a fresh model, or — when the name already exists at an older
/// revision — leave the registry untouched (§4.2.5: repointing is `update`'s
/// gated job).
fn register_or_notice(
    ctx: &Ctx,
    model: &NewModel,
    existing_rev: Option<&str>,
) -> Result<(), ChekovError> {
    if existing_rev.is_some() && existing_rev != Some(model.sha.as_str()) {
        println!(
            "'{}' already registered at an older revision — new files are downloaded but the \
             registry was NOT repointed; run `chekov update --model` for the gated repoint",
            model.name
        );
        return Ok(());
    }
    let mut reg = ctx.registry()?;
    reg.models.insert(model.name.clone(), model.entry());
    reg.save(&ctx.config.registry_path())?;
    println!(
        "registered '{}' — next: `chekov use {}` then `chekov run`",
        model.name, model.name
    );
    Ok(())
}

impl Command for PullCmd {
    fn run(&self, ctx: &Ctx) -> Result<ExitCode, ChekovError> {
        use crate::core::{hub, pullspec::PullSpec};
        let spec = PullSpec::parse(&self.spec)?;
        let snapshot =
            hub::fetch_snapshot(ctx.http.as_ref(), &spec.repo, spec.revision.as_deref())?;
        let plan = hub::plan_pull(
            &snapshot,
            &hub::PullTarget {
                repo: &spec.repo,
                quant: spec.quant.as_deref(),
                wired_mb: wired_budget_mb(ctx),
            },
        )?;
        let model = NewModel {
            name: self.name.clone().unwrap_or_else(|| spec.short_name()),
            repo: spec.repo.to_string(),
            quant: plan.quant.clone(),
            sha: snapshot.sha,
            first_shard: plan.first_shard.clone(),
            location: self.model_loc.clone(),
        };
        if self.dry_run {
            print_plan(&model, &plan);
            return Ok(ExitCode::SUCCESS);
        }
        let reg = ctx.registry()?;
        let existing_rev = reg.models.get(&model.name).map(|e| e.revision.clone());
        if is_noop(ctx, &model, existing_rev.as_deref()) {
            println!(
                "'{}' is already at {} — verified no-op",
                model.name,
                model.dir_name()
            );
            return Ok(ExitCode::SUCCESS);
        }
        let dir = materialize(ctx, &model, &plan)?;
        if let Some(url) = &self.license_url {
            snapshot_base_license(ctx, url, &dir)?;
        }
        register_or_notice(ctx, &model, existing_rev.as_deref())?;
        Ok(ExitCode::SUCCESS)
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
            location: None,
        }
    }

    #[test]
    fn registry_path_is_absolute_when_located() {
        let mut located = model();
        located.location = Some("/Volumes/jane/models".into());
        assert_eq!(
            located.registry_path(),
            "/Volumes/jane/models/minimax-m2.7@0123456789ab"
        );
        assert_eq!(model().registry_path(), "models/minimax-m2.7@0123456789ab");
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
        let text = provenance_text(
            &model(),
            "https://huggingface.co/x/LICENSE",
            "20260815T120000Z",
        );
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
