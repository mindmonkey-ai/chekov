//! `chekov update [--engine] [--model] [--all] [--dry-run]`.
//!
//! Engine rebuild with old→new commit report; model re-resolve with the
//! STOP-4 license-diff gate before an atomic registry repoint. Old revisions
//! are never deleted.

use std::process::ExitCode;

use super::{Command, Ctx};
use crate::error::ChekovError;

#[derive(Debug, clap::Args)]
// Four independent CLI switches, not a state machine — clap flags are the
// one place a bool-per-option struct is the correct shape.
#[allow(clippy::struct_excessive_bools)]
pub struct UpdateCmd {
    /// Update the llama.cpp engine (git pull + rebuild).
    #[arg(long)]
    pub engine: bool,
    /// Re-resolve the active model's repo revision.
    #[arg(long)]
    pub model: bool,
    /// Both.
    #[arg(long)]
    pub all: bool,
    /// Preview without changing anything.
    #[arg(long)]
    pub dry_run: bool,
}

/// STOP-4 arming rule: gate whenever a previously snapshotted license text is
/// no longer byte-identical (including its disappearance). A repo that never
/// had a license snapshot has nothing to diff.
#[must_use]
pub fn license_gate_needed(old: Option<&str>, new: Option<&str>) -> bool {
    match (old, new) {
        (Some(old), Some(new)) => old != new,
        (Some(_), None) => true,
        (None, _) => false,
    }
}

fn update_engine(ctx: &Ctx, dry_run: bool) -> Result<(), ChekovError> {
    use crate::core::engine;
    let dir = ctx.config.engine_dir();
    let before = engine::current_commit(&dir).unwrap_or_else(|| "none".into());
    engine::run_steps(&engine::setup_steps(&dir), dry_run)?;
    let after = engine::current_commit(&dir).unwrap_or_else(|| "none".into());
    println!("engine: {before} → {after}");
    if dry_run {
        return Ok(());
    }
    engine::record_commit(&ctx.config.logs_dir(), &after)?;
    // The live server is still the old binary; say so rather than let the user
    // assume the upgrade took effect (§C.2).
    if before != after && crate::core::server::live_pid(&ctx.config).is_some() {
        println!(
            "a server is still running the previous engine ({before}) — \
             apply with `chekov restart`"
        );
    }
    Ok(())
}

fn print_license_diff(old: &std::path::Path, new: &std::path::Path) {
    // /usr/bin/diff ships with macOS; a missing binary just skips the pretty diff.
    let out = std::process::Command::new("diff")
        .arg("-u")
        .args([old, new])
        .output();
    if let Ok(out) = out {
        println!("{}", String::from_utf8_lossy(&out.stdout));
    }
}

/// What to tell the user about the superseded weights.
///
/// It must NOT say `chekov rm <name>`: `repoint` has already moved
/// `entry.path` to the new revision, so `rm` would delete the freshly
/// downloaded weights and orphan the old directory forever — invisible to
/// `list`, which only iterates registered entries.
fn repoint_notice(name: &str, old_rev: &str, new_rev: &str) -> String {
    format!(
        "model '{name}': {old_rev} → {new_rev}\n\
         the previous revision's weights are still on disk at \
         models/{name}@{old_rev} — delete that directory when you no longer \
         need it. Do NOT use `chekov rm {name}`: the registry entry now points \
         at the new revision, so it would delete the download you just made."
    )
}

fn repoint(ctx: &Ctx, model: &super::pull::NewModel) -> Result<(), ChekovError> {
    let mut reg = ctx.registry()?;
    let entry = reg
        .models
        .get_mut(&model.name)
        .ok_or_else(|| ChekovError::UnknownModel {
            name: model.name.clone(),
        })?;
    let old_rev: String = entry.revision.chars().take(12).collect();
    entry.revision.clone_from(&model.sha);
    entry.path = model.registry_path();
    entry.first_shard.clone_from(&model.first_shard);
    reg.save(&ctx.config.registry_path())?;
    println!(
        "{}",
        repoint_notice(&model.name, &old_rev, &model.dir_name())
    );
    Ok(())
}

/// Re-resolve the active model. `None` when it is already current.
fn resolve_newer(
    ctx: &Ctx,
) -> Result<Option<(super::pull::NewModel, crate::core::hub::PullPlan)>, ChekovError> {
    use crate::core::{hub, pullspec::PullSpec};
    let reg = ctx.registry()?;
    let name = reg.active_name()?.to_owned();
    let entry = reg.effective(&name)?.entry;
    let repo = PullSpec::parse(&entry.repo)?.repo;
    let snapshot = hub::fetch_snapshot(ctx.http.as_ref(), &repo, None)?;
    if snapshot.sha == entry.revision {
        println!(
            "model '{name}' is already at {}",
            &entry.revision[..12usize.min(entry.revision.len())]
        );
        return Ok(None);
    }
    let plan = hub::plan_pull(
        &snapshot,
        &hub::PullTarget {
            repo: &repo,
            quant: Some(&entry.quant),
            wired_mb: super::pull::wired_budget_mb(ctx),
        },
    )?;
    // An externally-located model (absolute path) keeps new revisions beside
    // the old one: the location is the existing dir's parent.
    let location = {
        let old = std::path::Path::new(&entry.path);
        old.is_absolute()
            .then(|| old.parent().map(std::path::Path::to_path_buf))
            .flatten()
    };
    let model = super::pull::NewModel {
        name,
        repo: entry.repo,
        quant: plan.quant.clone(),
        sha: snapshot.sha,
        first_shard: plan.first_shard.clone(),
        location,
    };
    Ok(Some((model, plan)))
}

fn update_model(ctx: &Ctx, dry_run: bool) -> Result<(), ChekovError> {
    let Some((model, plan)) = resolve_newer(ctx)? else {
        return Ok(());
    };
    if dry_run {
        println!(
            "[dry-run] would download {} file(s) to models/{} and repoint after the license gate",
            plan.files.len(),
            model.dir_name()
        );
        return Ok(());
    }
    let old_path = ctx.registry()?.effective(&model.name)?.entry.path;
    let new_dir = super::pull::materialize(ctx, &model, &plan)?;
    let old_snapshot = ctx.config.root.join(&old_path).join("LICENSE.snapshot");
    let old = std::fs::read_to_string(&old_snapshot).ok();
    let new = std::fs::read_to_string(new_dir.join("LICENSE.snapshot")).ok();
    if license_gate_needed(old.as_deref(), new.as_deref()) {
        // STOP-4: explicit confirmation, never assumed.
        print_license_diff(&old_snapshot, &new_dir.join("LICENSE.snapshot"));
        super::confirm(
            &format!("license text for '{}' changed — repoint anyway", model.name),
            false,
        )?;
    }
    repoint(ctx, &model)
}

impl Command for UpdateCmd {
    fn run(&self, ctx: &Ctx) -> Result<ExitCode, ChekovError> {
        if !(self.engine || self.model || self.all) {
            return Err(ChekovError::UpdateFlagsMissing);
        }
        if self.engine || self.all {
            update_engine(ctx, self.dry_run)?;
        }
        if self.model || self.all {
            update_model(ctx, self.dry_run)?;
        }
        Ok(ExitCode::SUCCESS)
    }
}

#[cfg(test)]
mod tests {
    use super::repoint_notice;

    #[test]
    fn the_repoint_notice_names_the_stale_directory_not_a_destructive_command() {
        let note = repoint_notice("minimax-m2.7", "abc123456789", "def987654321");
        assert!(
            note.contains("models/minimax-m2.7@abc123456789"),
            "the old weights are only reachable by path — nothing else can \
             name them once the entry is repointed: {note}"
        );
        assert!(
            !note.contains("`chekov rm` when ready"),
            "the old notice recommended the command that destroys the new \
             download: {note}"
        );
        // Naming `rm` inside an explicit warning is fine — recommending it is not.
        assert!(
            note.contains("Do NOT use `chekov rm minimax-m2.7`"),
            "the trap is non-obvious enough that the notice should warn about \
             it by name: {note}"
        );
    }

    use super::license_gate_needed;

    #[test]
    fn gate_arms_only_on_real_change() {
        assert!(!license_gate_needed(None, None));
        assert!(!license_gate_needed(None, Some("MIT")));
        assert!(!license_gate_needed(Some("MIT"), Some("MIT")));
        assert!(license_gate_needed(Some("MIT"), Some("MIT + rider")));
        assert!(license_gate_needed(Some("MIT"), None));
    }
}
