//! Local-directory plugin carry-forward for `chekov launch`.
//!
//! A session runs under its own `CLAUDE_CONFIG_DIR`, so plugins the user
//! installed from a local marketplace (`extraKnownMarketplaces` with
//! `source.source = "directory"`) are invisible to it: `enabledPlugins` names
//! them, but the session's `plugins/` tree is empty and Claude Code warns
//! "marketplace not found". This module mirrors exactly what is needed —
//! `known_marketplaces.json`, `installed_plugins.json`, and a symlink per
//! plugin under `plugins/cache/<marketplace>/<plugin>/` — and touches nothing
//! else. Real marketplaces (git-backed) are left to Claude Code's own
//! installer, which works unchanged under the session dir.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::ChekovError;

/// One row of Claude Code's `installed_plugins.json`. The shape is Claude
/// Code's, so the field set is mirrored rather than designed.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginInstallEntry {
    pub scope: String,
    pub install_path: String,
    pub version: String,
    pub installed_at: i64,
    pub last_updated: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_commit_sha: Option<String>,
}

/// Claude Code's `installed_plugins.json`.
///
/// Read permissively: the file gains keys between Claude Code releases, a
/// shape we do not recognise must not stop a launch, and the session copy is
/// regenerated from the user's real one on every run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct InstalledPlugins {
    pub version: u8,
    pub plugins: HashMap<String, Vec<PluginInstallEntry>>,
}

/// A local marketplace worth mirroring: its key in `extraKnownMarketplaces`
/// and the directory its `source.path` points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalMarketplace {
    pub name: String,
    pub dir: PathBuf,
}

/// Local-directory marketplaces declared in the user's settings. Git-backed
/// and shapeless entries are skipped — only `source.source == "directory"`
/// needs a mirror.
#[must_use]
pub fn local_marketplaces(source: Option<&Value>) -> Vec<LocalMarketplace> {
    let Some(Value::Object(markets)) = source.and_then(|v| v.get("extraKnownMarketplaces")) else {
        return Vec::new();
    };
    markets
        .iter()
        .filter_map(|(name, spec)| {
            directory_source_path(spec).map(|dir| LocalMarketplace {
                name: name.clone(),
                dir: PathBuf::from(dir),
            })
        })
        .collect()
}

fn directory_source_path(spec: &Value) -> Option<&str> {
    let source = spec.get("source")?;
    if source.get("source")?.as_str()? != "directory" {
        return None;
    }
    source.get("path")?.as_str()
}

/// `extraKnownMarketplaces` merged over an existing `known_marketplaces.json`
/// value; the user's declaration wins on a name clash because it is the
/// thing the session was launched to carry.
#[must_use]
pub fn merge_known_marketplaces(existing: Option<Value>, source: Option<&Value>) -> Value {
    let mut out = match existing {
        Some(Value::Object(map)) => map,
        _ => Map::new(),
    };
    if let Some(Value::Object(markets)) = source.and_then(|v| v.get("extraKnownMarketplaces")) {
        out.extend(markets.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    Value::Object(out)
}

/// Register `entry` unless a row with the same install path is already there,
/// so a relaunch never duplicates the plugin list.
pub fn upsert_plugin_entry(installed: &mut InstalledPlugins, key: &str, entry: PluginInstallEntry) {
    let rows = installed.plugins.entry(key.to_owned()).or_default();
    if !rows
        .iter()
        .any(|row| row.install_path == entry.install_path)
    {
        rows.push(entry);
    }
}

/// The name Claude Code indexes a plugin under: `plugin.json`'s `name`, or
/// the marketplace name when the manifest is absent or shapeless.
#[must_use]
pub fn plugin_name(plugin_dir: &Path, fallback: &str) -> String {
    let declared = std::fs::read_to_string(plugin_dir.join("plugin.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|manifest| manifest.get("name")?.as_str().map(str::to_owned));
    safe_component(declared.as_deref().unwrap_or(fallback), fallback)
}

/// True for a name that is exactly one ordinary path component.
fn is_safe_component(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains('/') && !name.contains('\0')
}

/// The manifest name becomes a path component under the marketplace cache,
/// and `link_plugin` removes and recreates that path — so a name carrying
/// separators or `..` would let an untrusted `plugin.json` reach outside the
/// cache dir, and an absolute one would replace it entirely. Fall back rather
/// than trust it, and say so (§C.2 — never silently).
fn safe_component(candidate: &str, fallback: &str) -> String {
    if is_safe_component(candidate) {
        return candidate.to_owned();
    }
    eprintln!("chekov: ignoring unsafe plugin name {candidate:?} — not a single path component");
    if is_safe_component(fallback) {
        return fallback.to_owned();
    }
    "plugin".to_owned()
}

/// Mirror every local marketplace from `source` into `<session_dir>/plugins`.
/// A no-op when the user declares none.
pub fn sync_local_plugins(session_dir: &Path, source: Option<&Value>) -> Result<(), ChekovError> {
    let markets = local_marketplaces(source);
    if markets.is_empty() {
        return Ok(());
    }
    let plugins_dir = session_dir.join("plugins");
    let cache_dir = plugins_dir.join("cache");
    std::fs::create_dir_all(&cache_dir)
        .map_err(|e| ChekovError::io(format!("creating {}", cache_dir.display()), e))?;
    write_known_marketplaces(&plugins_dir, source)?;
    let mut installed = read_installed(&plugins_dir);
    for market in markets.iter().filter(|m| m.dir.is_dir()) {
        let entry = link_plugin(&cache_dir.join(&market.name), market)?;
        upsert_plugin_entry(&mut installed, &market.name, entry);
    }
    write_json(&plugins_dir.join("installed_plugins.json"), &installed)
}

fn write_known_marketplaces(plugins_dir: &Path, source: Option<&Value>) -> Result<(), ChekovError> {
    let path = plugins_dir.join("known_marketplaces.json");
    let existing = read_json(&path);
    write_json(&path, &merge_known_marketplaces(existing, source))
}

fn read_installed(plugins_dir: &Path) -> InstalledPlugins {
    read_json(&plugins_dir.join("installed_plugins.json"))
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default()
}

/// Symlink the plugin into the marketplace cache and describe the row that
/// registers it. A dangling link from a moved checkout is replaced, not
/// tripped over — `exists()` follows links and would report it absent.
fn link_plugin(
    market_cache: &Path,
    market: &LocalMarketplace,
) -> Result<PluginInstallEntry, ChekovError> {
    std::fs::create_dir_all(market_cache)
        .map_err(|e| ChekovError::io(format!("creating {}", market_cache.display()), e))?;
    let link = market_cache.join(plugin_name(&market.dir, &market.name));
    if link.symlink_metadata().is_ok() && !link.is_dir() {
        std::fs::remove_file(&link)
            .map_err(|e| ChekovError::io(format!("replacing {}", link.display()), e))?;
    }
    if link.symlink_metadata().is_err() {
        std::os::unix::fs::symlink(&market.dir, &link)
            .map_err(|e| ChekovError::io(format!("linking {}", link.display()), e))?;
    }
    let now = unix_millis_now();
    Ok(PluginInstallEntry {
        scope: "user".to_owned(),
        install_path: link.display().to_string(),
        version: "local".to_owned(),
        installed_at: now,
        last_updated: now,
        git_commit_sha: None,
    })
}

fn unix_millis_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

fn read_json(path: &Path) -> Option<Value> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
}

/// Scratch path for an atomic write. Process-unique: a fixed `.tmp` would let
/// two chekov processes rename each other's half-written state into place.
fn temp_path_for(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".tmp.{}", std::process::id()));
    path.with_file_name(name)
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), ChekovError> {
    let mut text = serde_json::to_string_pretty(value)
        .map_err(|e| ChekovError::io(format!("serializing {}", path.display()), e.into()))?;
    text.push('\n');
    // Atomic: this rewrites another tool's live state file, so a crash or a
    // concurrent chekov must never leave it truncated (mirrors Registry::save).
    let tmp = temp_path_for(path);
    std::fs::write(&tmp, text)
        .map_err(|e| ChekovError::io(format!("writing {}", tmp.display()), e))?;
    std::fs::rename(&tmp, path)
        .map_err(|e| ChekovError::io(format!("renaming {} into place", tmp.display()), e))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("chekov-plugins-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    fn settings_with(markets: &Value) -> Value {
        json!({ "enabledPlugins": { "pushkin-review@pushkin-review": true },
                "extraKnownMarketplaces": markets })
    }

    fn directory_market(path: &str) -> Value {
        json!({ "source": { "source": "directory", "path": path } })
    }

    #[test]
    fn a_traversing_plugin_name_cannot_escape_the_marketplace_cache() {
        let dir = scratch("traversal");
        std::fs::write(
            dir.join("plugin.json"),
            json!({ "name": "../../../escaped" }).to_string(),
        )
        .expect("manifest");
        let name = plugin_name(&dir, "fallback");
        assert!(
            !name.contains('/') && name != ".." && !name.is_empty(),
            "an untrusted manifest name is joined onto the cache dir and the \
             result is removed and recreated — it must stay one safe \
             component, got: {name:?}"
        );
    }

    #[test]
    fn an_absolute_plugin_name_cannot_redirect_the_link() {
        let dir = scratch("absolute");
        std::fs::write(
            dir.join("plugin.json"),
            json!({ "name": "/etc/cron.d/evil" }).to_string(),
        )
        .expect("manifest");
        let cache = scratch("absolute-cache");
        let link = cache.join(plugin_name(&dir, "fallback"));
        assert!(
            link.starts_with(&cache),
            "Path::join replaces the base when given an absolute path: {link:?}"
        );
    }

    #[test]
    fn the_atomic_write_scratch_path_is_unique_per_process() {
        let target = Path::new("/r/plugins/installed_plugins.json");
        let tmp = temp_path_for(target);
        assert_ne!(tmp, target, "the scratch path must not be the target");
        assert!(
            tmp.to_string_lossy()
                .contains(&std::process::id().to_string()),
            "two chekov processes writing concurrently would race on a shared \
             scratch file and one would rename the other's half-written state \
             into place, got: {tmp:?}"
        );
    }

    #[test]
    fn only_directory_marketplaces_are_mirrored() {
        let settings = settings_with(&json!({
            "pushkin-review": directory_market("/opt/pushkin"),
            "official": { "source": { "source": "github", "repo": "anthropics/claude-plugins" } },
            "shapeless": 42,
        }));
        let found = local_marketplaces(Some(&settings));
        assert_eq!(
            found,
            vec![LocalMarketplace {
                name: "pushkin-review".into(),
                dir: PathBuf::from("/opt/pushkin")
            }]
        );
        assert!(local_marketplaces(None).is_empty());
        assert!(local_marketplaces(Some(&json!({}))).is_empty());
    }

    #[test]
    fn user_declaration_wins_over_existing_marketplace_entry() {
        let existing = json!({ "pushkin-review": { "stale": true }, "other": { "kept": true } });
        let settings = settings_with(&json!({ "pushkin-review": directory_market("/new") }));
        let out = merge_known_marketplaces(Some(existing), Some(&settings));
        assert_eq!(out["pushkin-review"]["source"]["path"], json!("/new"));
        assert_eq!(out["other"]["kept"], json!(true));
        assert_eq!(merge_known_marketplaces(None, None), json!({}));
    }

    fn entry(path: &str) -> PluginInstallEntry {
        PluginInstallEntry {
            scope: "user".into(),
            install_path: path.into(),
            version: "local".into(),
            installed_at: 1,
            last_updated: 1,
            git_commit_sha: None,
        }
    }

    #[test]
    fn upsert_is_idempotent_per_install_path() {
        let mut installed = InstalledPlugins::default();
        upsert_plugin_entry(&mut installed, "mp", entry("/a"));
        upsert_plugin_entry(&mut installed, "mp", entry("/a"));
        upsert_plugin_entry(&mut installed, "mp", entry("/b"));
        assert_eq!(installed.plugins["mp"].len(), 2);
    }

    #[test]
    fn installed_plugins_round_trips_claude_code_shape() {
        let text = r#"{"version":2,"plugins":{"mp":[{"scope":"user","installPath":"/x",
            "version":"1.0","installedAt":5,"lastUpdated":6,"gitCommitSha":"abc"}]},"futureKey":1}"#;
        let parsed: InstalledPlugins = serde_json::from_str(text).expect("permissive parse");
        assert_eq!(parsed.version, 2);
        assert_eq!(
            parsed.plugins["mp"][0].git_commit_sha.as_deref(),
            Some("abc")
        );
        let back = serde_json::to_value(&parsed).expect("serialize");
        assert_eq!(back["plugins"]["mp"][0]["installPath"], json!("/x"));
    }

    #[test]
    fn plugin_name_prefers_manifest_then_marketplace() {
        let dir = scratch("name");
        assert_eq!(plugin_name(&dir, "fallback"), "fallback");
        std::fs::write(dir.join("plugin.json"), r#"{"name":"real-name"}"#).expect("write");
        assert_eq!(plugin_name(&dir, "fallback"), "real-name");
    }

    #[test]
    fn sync_links_plugin_and_registers_it_once() {
        let plugin = scratch("src-plugin");
        std::fs::write(plugin.join("plugin.json"), r#"{"name":"pushkin-review"}"#).expect("write");
        let session = scratch("session");
        let settings = settings_with(&json!({
            "pushkin-review": directory_market(plugin.to_str().expect("utf8")),
            "missing": directory_market("/definitely/not/here"),
        }));

        sync_local_plugins(&session, Some(&settings)).expect("first sync");
        sync_local_plugins(&session, Some(&settings)).expect("relaunch sync");

        let link = session.join("plugins/cache/pushkin-review/pushkin-review");
        assert_eq!(std::fs::read_link(&link).expect("symlink"), plugin);
        let installed: InstalledPlugins = serde_json::from_str(
            &std::fs::read_to_string(session.join("plugins/installed_plugins.json")).expect("read"),
        )
        .expect("parse");
        assert_eq!(installed.plugins["pushkin-review"].len(), 1);
        assert!(!installed.plugins.contains_key("missing"));
        let known: Value = serde_json::from_str(
            &std::fs::read_to_string(session.join("plugins/known_marketplaces.json"))
                .expect("read"),
        )
        .expect("parse");
        assert!(known.get("pushkin-review").is_some());
    }

    #[test]
    fn sync_replaces_a_dangling_link() {
        let plugin = scratch("src-plugin-2");
        let session = scratch("session-2");
        let market_cache = session.join("plugins/cache/mp");
        std::fs::create_dir_all(&market_cache).expect("mkdir");
        std::os::unix::fs::symlink("/moved/away", market_cache.join("mp")).expect("dangling");
        let settings =
            settings_with(&json!({ "mp": directory_market(plugin.to_str().expect("utf8")) }));

        sync_local_plugins(&session, Some(&settings)).expect("sync over dangling link");
        assert_eq!(
            std::fs::read_link(market_cache.join("mp")).expect("relinked"),
            plugin
        );
    }

    #[test]
    fn sync_without_marketplaces_touches_nothing() {
        let session = scratch("session-3");
        sync_local_plugins(&session, Some(&json!({}))).expect("no-op");
        assert!(!session.join("plugins").exists());
    }
}
