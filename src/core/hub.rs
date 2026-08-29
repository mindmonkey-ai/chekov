//! Hugging Face hub access behind a mockable seam (§8.2).
//!
//! All metadata goes through `HttpClient`, so tests inject fakes. Only the
//! shard download itself touches the network, over blocking `ureq`, and is
//! untested by design.

use serde::Deserialize;

use crate::core::pullspec::RepoId;
use crate::error::ChekovError;

/// One JSON POST: url, body, optional bearer token.
#[derive(Debug, Clone)]
pub struct JsonRequest {
    pub url: String,
    pub body: String,
    pub bearer: Option<String>,
}

/// The HTTP boundary (§C.6). Implemented by `UreqClient` for real use and by
/// canned fakes in tests — no test ever touches the network.
pub trait HttpClient {
    fn get(&self, url: &str) -> Result<String, ChekovError>;
    fn post_json(&self, req: &JsonRequest) -> Result<String, ChekovError>;
}

/// Production client: blocking `ureq` (no async runtime, prompt §2.1).
pub struct UreqClient;

impl HttpClient for UreqClient {
    fn get(&self, url: &str) -> Result<String, ChekovError> {
        let mut response = ureq::get(url)
            .call()
            .map_err(|e| ChekovError::HubRequestFailed {
                url: url.to_owned(),
                reason: e.to_string(),
            })?;
        response
            .body_mut()
            .read_to_string()
            .map_err(|e| ChekovError::HubRequestFailed {
                url: url.to_owned(),
                reason: e.to_string(),
            })
    }

    fn post_json(&self, req: &JsonRequest) -> Result<String, ChekovError> {
        // http_status_as_error(false): ureq's default renders a non-2xx as
        // "http status: 400" and DISCARDS the body — but llama-server puts the
        // real cause there ("Failed to initialize samplers", a context
        // overflow). Taking the status ourselves keeps the explanation, as the
        // proxy's own upstream call already does.
        let mut builder = ureq::post(&req.url)
            .config()
            .http_status_as_error(false)
            .build()
            .header("Content-Type", "application/json");
        if let Some(token) = &req.bearer {
            builder = builder.header("Authorization", &format!("Bearer {token}"));
        }
        let response = builder
            .send(&req.body)
            .map_err(|e| ChekovError::EndpointDown {
                url: req.url.clone(),
                reason: e.to_string(),
            })?;
        let status = response.status().as_u16();
        let mut body = response.into_body();
        let text = body
            .read_to_string()
            .map_err(|e| ChekovError::EndpointDown {
                url: req.url.clone(),
                reason: e.to_string(),
            })?;
        crate::core::proxy::serve::answered(&req.url, status, text)
    }
}

/// HF model-info API response — only the fields chekov reads.
///
/// `deny_unknown_fields` deliberately NOT set here: this is a third-party API
/// whose schema grows fields routinely; §C.7 targets our own config surface.
#[derive(Debug, Clone, Deserialize)]
pub struct RepoSnapshot {
    pub sha: String,
    #[serde(rename = "siblings")]
    pub files: Vec<RepoFile>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RepoFile {
    pub rfilename: String,
    /// Byte size from the `?blobs=true` API — the adoption/skip verifier.
    #[serde(default)]
    pub size: Option<u64>,
}

/// One file the plan will fetch, with its expected size when the API knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanFile {
    pub path: String,
    pub size: Option<u64>,
}

/// What `pull` will do for one spec: which files, which shard loads first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullPlan {
    pub quant: String,
    pub files: Vec<PlanFile>,
    pub first_shard: String,
}

/// One quant tag offered by a repo, with its shards summed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuantOption {
    pub tag: String,
    /// Total shard bytes, or `None` when the API omitted a size for any shard
    /// — a partial sum would understate the download and is never shown.
    pub bytes: Option<u64>,
}

/// What to plan a pull for, plus the memory budget the choices are judged
/// against. Bundled because §3.4 caps `plan_pull` at three arguments.
pub struct PullTarget<'a> {
    pub repo: &'a RepoId,
    pub quant: Option<&'a str>,
    /// Effective `iogpu.wired_limit_mb`; `None` drops the verdict column.
    pub wired_mb: Option<u64>,
}

/// Fetch repo metadata (sha + file list) at `revision` (default `main`).
pub fn fetch_snapshot(
    http: &dyn HttpClient,
    repo: &RepoId,
    revision: Option<&str>,
) -> Result<RepoSnapshot, ChekovError> {
    let rev = revision.unwrap_or("main");
    // ?blobs=true adds per-file sizes — required for verified adoption/skip.
    let url = format!("https://huggingface.co/api/models/{repo}/revision/{rev}?blobs=true");
    let body = http.get(&url)?;
    serde_json::from_str(&body).map_err(|e| ChekovError::HubRequestFailed {
        url,
        reason: format!("unexpected response shape: {e}"),
    })
}

/// Decide what to download: requires a quant tag (no silent default), errors
/// with the available choices when it is absent or wrong (§4.1).
pub fn plan_pull(
    snapshot: &RepoSnapshot,
    target: &PullTarget<'_>,
) -> Result<PullPlan, ChekovError> {
    let available = render_quant_table(&quant_options(snapshot), target.wired_mb);
    let Some(quant) = target.quant else {
        return Err(ChekovError::NoQuantSpecified {
            repo: target.repo.to_string(),
            available,
        });
    };
    let files: Vec<PlanFile> = snapshot
        .files
        .iter()
        .filter(|f| derived_quant(&f.rfilename).as_deref() == Some(quant))
        .map(|f| PlanFile {
            path: f.rfilename.clone(),
            size: f.size,
        })
        .collect();
    if files.is_empty() {
        return Err(ChekovError::QuantNotFound {
            quant: quant.to_owned(),
            repo: target.repo.to_string(),
            available,
        });
    }
    let first_shard = files
        .iter()
        .find(|f| f.path.contains("-00001-of-"))
        .unwrap_or(&files[0])
        .path
        .clone();
    Ok(PullPlan {
        quant: quant.to_owned(),
        files,
        first_shard,
    })
}

/// Distinct quant tags present in a repo's GGUF files (subdir- and
/// flat-filename styles).
#[must_use]
pub fn available_quants(snapshot: &RepoSnapshot) -> Vec<String> {
    quant_options(snapshot)
        .into_iter()
        .map(|opt| opt.tag)
        .collect()
}

/// Every quant tag in the repo with its shards summed, smallest first.
///
/// Size order is half the answer to "which one fits". Tags whose sizes the
/// API withheld sort last, since an unknown total cannot be compared.
#[must_use]
pub fn quant_options(snapshot: &RepoSnapshot) -> Vec<QuantOption> {
    let mut totals: std::collections::BTreeMap<String, (Option<u64>, bool)> =
        std::collections::BTreeMap::new();
    for file in &snapshot.files {
        let Some(tag) = derived_quant(&file.rfilename) else {
            continue;
        };
        let entry = totals.entry(tag).or_insert((Some(0), false));
        match file.size {
            Some(bytes) => entry.0 = entry.0.map(|sum| sum + bytes),
            None => entry.1 = true,
        }
    }
    let mut options: Vec<QuantOption> = totals
        .into_iter()
        .map(|(tag, (sum, partial))| QuantOption {
            tag,
            bytes: if partial { None } else { sum },
        })
        .collect();
    options.sort_by_key(|opt| (opt.bytes.is_none(), opt.bytes, opt.tag.clone()));
    options
}

/// Weights-only bytes at which a quant stops leaving room for the KV cache
/// and compute buffers that load on top of it.
const TIGHT_FRACTION_PCT: u64 = 85;

/// Render the choice list carried in `NoQuantSpecified` / `QuantNotFound`.
///
/// Sizes are **weights only**: the KV cache and compute buffers come on top,
/// and their size needs GGUF header geometry chekov does not have before the
/// download. Naming that in the header beats inventing a number.
#[must_use]
pub fn render_quant_table(options: &[QuantOption], wired_mb: Option<u64>) -> String {
    if options.is_empty() {
        return "(none — this repo exposes no .gguf files)".to_owned();
    }
    let header = wired_mb.map_or_else(
        || "weights only; KV cache and compute buffers come on top".to_owned(),
        |mb| format!("weights only; KV cache and compute buffers come on top of these — wired limit {mb} MB"),
    );
    let width = options.iter().map(|o| o.tag.len()).max().unwrap_or(0);
    let rows = options.iter().map(|opt| {
        let size = opt
            .bytes
            .map_or_else(|| "size unknown".to_owned(), format_gib);
        let verdict = verdict_for(opt.bytes, wired_mb);
        format!("\n  {:<width$}  {size:>12}{verdict}", opt.tag)
    });
    format!("{header}:{}", rows.collect::<String>())
}

fn format_gib(bytes: u64) -> String {
    #[expect(
        clippy::cast_precision_loss,
        reason = "display-only: GiB with one decimal, exactness is irrelevant"
    )]
    let gib = bytes as f64 / (1024.0 * 1024.0 * 1024.0);
    format!("{gib:.1} GiB")
}

/// `fits` / `tight` / `exceeds` against the wired limit. Empty when the
/// budget or the size is unknown — a guess would be worse than a blank.
fn verdict_for(bytes: Option<u64>, wired_mb: Option<u64>) -> String {
    let (Some(bytes), Some(mb)) = (bytes, wired_mb) else {
        return String::new();
    };
    let limit = mb * 1024 * 1024;
    let word = if bytes > limit {
        "exceeds"
    } else if bytes * 100 >= limit * TIGHT_FRACTION_PCT {
        "tight"
    } else {
        "fits"
    };
    format!("   {word}")
}

/// The quant tag a GGUF path belongs to: its directory when subdir-style
/// (`UD-Q5_K_XL/model-....gguf`), else the tag embedded in the filename
/// (`model-Q8_0.gguf`). One source of truth for matching AND listing, so an
/// ambiguous spec like `Q5_K_XL` can never select `UD-Q5_K_XL` files.
fn derived_quant(path: &str) -> Option<String> {
    let stem = path.strip_suffix(".gguf")?;
    if !is_weights(path) {
        return None;
    }
    if let Some((dir, _)) = path.split_once('/') {
        return quant_like(dir).then(|| dir.to_owned());
    }
    let stem = strip_shard_suffix(stem);
    stem.char_indices()
        .filter(|&(_, c)| c == '-')
        .map(|(i, _)| &stem[i + 1..])
        .find(|cand| quant_like(cand))
        .map(ToOwned::to_owned)
}

/// True for a file that is actually model weights.
///
/// A vision projector is named `mmproj-F16.gguf`, so the tag heuristic below
/// reads `F16` out of it and offers a 1 GiB "quant" of a 100 GiB model — the
/// cheapest-looking row in the table, and not a runnable model at all.
/// `docs/HOWTOS.md` already tells readers `mmproj` "is **not** the model";
/// this teaches the code the same thing.
fn is_weights(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let dir = path.split('/').next().unwrap_or("");
    // Vision projectors, calibration data, multi-token-prediction layers and
    // draft models all ship as .gguf beside real quants. Summing them inflates
    // a quant's size; offering them as one hands the user a non-model.
    let junk_prefix = ["mmproj", "imatrix", "mtp-", "dspark-"];
    !junk_prefix.iter().any(|p| name.starts_with(p)) && !name.contains("-mmproj-") && dir != "MTP"
}

/// Heuristic for quant-tag tokens: `UD-Q5_K_XL`, `IQ4_XS`, `Q8_0`, `BF16`…
fn quant_like(token: &str) -> bool {
    let core = token.strip_prefix("UD-").unwrap_or(token);
    let q_digit = |t: &str| {
        t.strip_prefix("IQ")
            .or_else(|| t.strip_prefix('Q'))
            .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
    };
    q_digit(core) || matches!(core, "BF16" | "F16" | "F32")
}

/// Parameters for a revision-pinned shard download.
pub struct DownloadSpec<'a> {
    pub repo: &'a str,
    pub revision: &'a str,
    pub dest: &'a std::path::Path,
    /// `--model-loc` root to adopt pre-downloaded files from (hard links).
    pub adopt_from: Option<&'a std::path::Path>,
}

/// Where a pre-downloaded copy of `rfilename` may live under a `--model-loc`
/// root: hf-cli layout (`<loc>/<repo-tail>/<rfilename>`) first, then flat
/// (`<loc>/<rfilename>`).
#[must_use]
pub fn adoption_candidates(
    model_loc: &std::path::Path,
    repo: &str,
    rfilename: &str,
) -> Vec<std::path::PathBuf> {
    let repo_tail = repo.split_once('/').map_or(repo, |(_, tail)| tail);
    vec![
        model_loc.join(repo_tail).join(rfilename),
        model_loc.join(rfilename),
    ]
}

/// Hard-link `src` to `dest` after verifying its size against `expected`.
///
/// `Ok(true)` = linked; `Ok(false)` = size mismatch, caller must download
/// instead (loudly — a truncated shard is never adopted silently).
pub fn link_verified(
    src: &std::path::Path,
    dest: &std::path::Path,
    expected: Option<u64>,
) -> Result<bool, ChekovError> {
    let meta = std::fs::metadata(src)
        .map_err(|e| ChekovError::io(format!("inspecting {}", src.display()), e))?;
    if expected != Some(meta.len()) {
        return Ok(false);
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ChekovError::io(format!("creating {}", parent.display()), e))?;
    }
    // Same-volume hard link is instant and free; copy is the cross-volume
    // fallback. Either way the source stays untouched.
    std::fs::hard_link(src, dest)
        .or_else(|_| std::fs::copy(src, dest).map(|_| ()))
        .map_err(|e| ChekovError::io(format!("linking {}", src.display()), e))?;
    Ok(true)
}

/// Stream one file to `dest`, via a `.part` sibling so an interrupted transfer
/// never leaves a short file that `file_matches` would have to catch later.
///
/// Network path — exercised only by real pulls, never by tests (prompt §2.4).
fn fetch_to(url: &str, dest: &std::path::Path) -> Result<(), ChekovError> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ChekovError::io(format!("creating {}", parent.display()), e))?;
    }
    let res = ureq::get(url)
        .call()
        .map_err(|e| ChekovError::io(format!("requesting {url}"), std::io::Error::other(e)))?;
    let part = dest.with_extension("part");
    let mut out = std::fs::File::create(&part)
        .map_err(|e| ChekovError::io(format!("creating {}", part.display()), e))?;
    std::io::copy(&mut res.into_body().into_reader(), &mut out)
        .map_err(|e| ChekovError::io(format!("writing {}", part.display()), e))?;
    out.sync_all()
        .map_err(|e| ChekovError::io(format!("flushing {}", part.display()), e))?;
    std::fs::rename(&part, dest)
        .map_err(|e| ChekovError::io(format!("renaming {} into place", part.display()), e))
}

/// The revision-pinned download URL for one file.
///
/// Xet-backed repos redirect to a CAS bridge that serves the bytes over
/// ordinary HTTPS with a normal content-length, so a plain GET works there too
/// — verified against unsloth/MiniMax-M2.7-GGUF, which is Xet-backed.
fn resolve_url(repo: &str, revision: &str, path: &str) -> String {
    format!("https://huggingface.co/{repo}/resolve/{revision}/{path}")
}

/// Download every planned file into the model directory, revision-pinned.
///
/// Network path — exercised only by real pulls, never by tests (prompt §2.4).
pub fn download_plan(spec: &DownloadSpec<'_>, plan: &PullPlan) -> Result<(), ChekovError> {
    let failed = |reason: String| ChekovError::DownloadFailed {
        repo: spec.repo.to_owned(),
        reason,
    };
    if !spec.repo.contains('/') {
        return Err(failed("repo id is missing the org/ prefix".to_owned()));
    }
    for file in &plan.files {
        let target = spec.dest.join(&file.path);
        if file_matches(&target, file.size) {
            println!("{} already present (size verified) — skipping", file.path);
            continue;
        }
        if try_adopt(spec, file)? {
            println!(
                "{} adopted from local copy (size verified, hard link)",
                file.path
            );
            continue;
        }
        println!("downloading {} …", file.path);
        fetch_to(&resolve_url(spec.repo, spec.revision, &file.path), &target)
            .map_err(|e| failed(format!("{}: {e}", file.path)))?;
    }
    Ok(())
}

/// True when `path` exists and matches the expected size (unknown size never
/// matches — presence alone is not verification).
fn file_matches(path: &std::path::Path, expected: Option<u64>) -> bool {
    match (std::fs::metadata(path), expected) {
        (Ok(meta), Some(size)) => meta.len() == size,
        _ => false,
    }
}

/// Try each adoption candidate under `adopt_from`; link the first verified
/// one. `Ok(false)` = nothing adoptable, download instead.
fn try_adopt(spec: &DownloadSpec<'_>, file: &PlanFile) -> Result<bool, ChekovError> {
    let Some(loc) = spec.adopt_from else {
        return Ok(false);
    };
    for candidate in adoption_candidates(loc, spec.repo, &file.path) {
        if !candidate.exists() {
            continue;
        }
        if link_verified(&candidate, &spec.dest.join(&file.path), file.size)? {
            return Ok(true);
        }
        eprintln!(
            "warning: {} exists but its size does not match the repo — downloading instead",
            candidate.display()
        );
    }
    Ok(false)
}

/// Strip a trailing `-00001-of-00004` shard marker if present.
fn strip_shard_suffix(stem: &str) -> &str {
    let Some(idx) = stem.len().checked_sub(15) else {
        return stem;
    };
    let tail = &stem[idx..];
    let shardish = tail.starts_with('-')
        && tail[1..6].bytes().all(|b| b.is_ascii_digit())
        && &tail[6..10] == "-of-"
        && tail[10..].bytes().all(|b| b.is_ascii_digit());
    if shardish { &stem[..idx] } else { stem }
}

#[cfg(test)]
mod tests {
    use super::resolve_url;

    #[test]
    fn a_download_url_pins_the_revision_not_main() {
        let url = resolve_url(
            "unsloth/MiniMax-M2.7-GGUF",
            "d2a05ccf69491b03db0cc40b335aec14bdaf7198",
            "UD-Q5_K_XL/MiniMax-M2.7-UD-Q5_K_XL-00001-of-00005.gguf",
        );
        assert_eq!(
            url,
            "https://huggingface.co/unsloth/MiniMax-M2.7-GGUF/resolve/\
             d2a05ccf69491b03db0cc40b335aec14bdaf7198/\
             UD-Q5_K_XL/MiniMax-M2.7-UD-Q5_K_XL-00001-of-00005.gguf"
                .replace(' ', ""),
            "a pull must fetch the pinned revision, never whatever main is now"
        );
    }
    use super::{
        HttpClient, JsonRequest, PullTarget, QuantOption, RepoSnapshot, available_quants,
        fetch_snapshot, plan_pull, quant_options, render_quant_table,
    };
    use crate::core::pullspec::{PullSpec, RepoId};
    use crate::error::ChekovError;

    fn repo() -> RepoId {
        PullSpec::parse("unsloth/MiniMax-M2.7-GGUF")
            .expect("spec")
            .repo
    }

    struct FakeHttp {
        body: String,
    }

    impl HttpClient for FakeHttp {
        fn get(&self, url: &str) -> Result<String, ChekovError> {
            assert!(url.contains("api/models"), "unexpected url: {url}");
            Ok(self.body.clone())
        }

        fn post_json(&self, _req: &JsonRequest) -> Result<String, ChekovError> {
            unreachable!("hub metadata never POSTs")
        }
    }

    const API_JSON: &str = r#"{
        "sha": "0123456789abcdef0123456789abcdef01234567",
        "modelId": "unsloth/MiniMax-M2.7-GGUF",
        "siblings": [
            {"rfilename": "README.md"},
            {"rfilename": "UD-Q5_K_XL/MiniMax-M2.7-UD-Q5_K_XL-00001-of-00004.gguf", "size": 8237824},
            {"rfilename": "UD-Q5_K_XL/MiniMax-M2.7-UD-Q5_K_XL-00002-of-00004.gguf", "size": 49986025344},
            {"rfilename": "UD-Q4_K_XL/MiniMax-M2.7-UD-Q4_K_XL-00001-of-00003.gguf"},
            {"rfilename": "MiniMax-M2.7-Q8_0.gguf"}
        ]
    }"#;

    fn snapshot_from(json: &str) -> RepoSnapshot {
        let fake = FakeHttp { body: json.into() };
        fetch_snapshot(&fake, &repo(), None).expect("parse fixture")
    }

    fn snapshot() -> RepoSnapshot {
        snapshot_from(API_JSON)
    }

    #[test]
    fn fetch_parses_sha_and_files() {
        let snap = snapshot();
        assert!(snap.sha.starts_with("0123456789ab"));
        assert_eq!(snap.files.len(), 5);
    }

    #[test]
    fn plan_selects_quant_files_and_first_shard() {
        let snap = snapshot();
        let plan = plan_pull(
            &snap,
            &PullTarget {
                repo: &repo(),
                quant: Some("UD-Q5_K_XL"),
                wired_mb: None,
            },
        )
        .expect("quant exists");
        assert_eq!(plan.files.len(), 2);
        assert_eq!(plan.files[0].size, Some(8_237_824), "sizes must ride along");
        assert!(
            plan.first_shard.ends_with("00001-of-00004.gguf"),
            "{}",
            plan.first_shard
        );
    }

    #[test]
    fn adoption_candidates_prefer_hf_cli_layout() {
        let cands = super::adoption_candidates(
            std::path::Path::new("/Volumes/external/models"),
            "unsloth/MiniMax-M2.7-GGUF",
            "UD-Q5_K_XL/x.gguf",
        );
        assert_eq!(
            cands[0],
            std::path::PathBuf::from(
                "/Volumes/external/models/MiniMax-M2.7-GGUF/UD-Q5_K_XL/x.gguf"
            )
        );
        assert_eq!(
            cands[1],
            std::path::PathBuf::from("/Volumes/external/models/UD-Q5_K_XL/x.gguf")
        );
    }

    #[test]
    fn link_verified_links_matching_and_rejects_mismatch() {
        let dir = std::env::temp_dir().join("chekov-test-adopt");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        let src = dir.join("src.gguf");
        std::fs::write(&src, vec![7u8; 100]).expect("write");
        let good = dir.join("sub/good.gguf");
        assert!(super::link_verified(&src, &good, Some(100)).expect("link"));
        assert_eq!(std::fs::metadata(&good).expect("linked").len(), 100);
        let bad = dir.join("sub/bad.gguf");
        assert!(!super::link_verified(&src, &bad, Some(999)).expect("mismatch is not an error"));
        assert!(!bad.exists(), "mismatched file must never be adopted");
    }

    #[test]
    fn plan_without_quant_lists_choices() {
        let msg = plan_pull(
            &snapshot(),
            &PullTarget {
                repo: &repo(),
                quant: None,
                wired_mb: None,
            },
        )
        .expect_err("no silent default")
        .to_string();
        assert!(
            msg.contains("UD-Q5_K_XL") && msg.contains("Q8_0"),
            "choices missing: {msg}"
        );
    }

    #[test]
    fn plan_with_wrong_quant_lists_choices() {
        let msg = plan_pull(
            &snapshot(),
            &PullTarget {
                repo: &repo(),
                quant: Some("Q2_K"),
                wired_mb: None,
            },
        )
        .expect_err("unknown quant")
        .to_string();
        assert!(
            msg.contains("Q2_K") && msg.contains("UD-Q4_K_XL"),
            "bad message: {msg}"
        );
    }

    #[test]
    fn quants_cover_subdir_and_flat_styles() {
        let quants = available_quants(&snapshot());
        assert!(quants.contains(&"UD-Q5_K_XL".to_owned()), "{quants:?}");
        assert!(quants.contains(&"UD-Q4_K_XL".to_owned()), "{quants:?}");
        assert!(quants.contains(&"Q8_0".to_owned()), "{quants:?}");
    }

    #[test]
    fn calibration_and_draft_artifacts_are_not_weights() {
        // Every one of these lives beside real quants in popular repos and
        // would otherwise be summed into a quant's size, or offered as one.
        for junk in [
            "imatrix_unsloth.gguf",
            "imatrix.gguf",
            "MTP/GLM-5.3-Flash-MTP-Q8_0.gguf",
            "mtp-Q4_K_M.gguf",
            "dspark-draft-Q4_0.gguf",
            "mmproj-F16.gguf",
        ] {
            assert_eq!(
                super::derived_quant(junk),
                None,
                "{junk} is not model weights"
            );
        }
        // …while a real shard in a quant folder still resolves.
        assert_eq!(
            super::derived_quant("UD-Q4_K_XL/GLM-5.3-Flash-UD-Q4_K_XL-00001-of-00006.gguf")
                .as_deref(),
            Some("UD-Q4_K_XL")
        );
    }

    #[test]
    fn a_vision_projector_is_not_offered_as_a_quant() {
        // Verified live against unsloth/GLM-5.3-Flash-GGUF, whose real quants
        // are 86-186 GiB: `mmproj-F16.gguf` was listed as an `F16` of 1.1 GiB
        // and sorted to the TOP of the fit table, so it read as the cheapest
        // option. Pulling it registered a projector as a runnable model.
        assert_eq!(super::derived_quant("mmproj-F16.gguf"), None);
        assert_eq!(super::derived_quant("mmproj-BF16.gguf"), None);
        assert_eq!(super::derived_quant("GLM-5.3-Flash-mmproj-F16.gguf"), None);
        // Real weights must still resolve, including the genuine BF16 folder
        // whose total the projector was being summed into.
        assert_eq!(
            super::derived_quant("Qwen3.8-27B-UD-Q4_K_M.gguf").as_deref(),
            Some("UD-Q4_K_M")
        );
        assert_eq!(
            super::derived_quant("BF16/Qwen3.8-27B-BF16-00001-of-00002.gguf").as_deref(),
            Some("BF16")
        );
    }

    #[test]
    fn quant_options_sums_shards_per_tag() {
        let opts = quant_options(&snapshot());
        let sized = opts
            .iter()
            .find(|o| o.tag == "UD-Q5_K_XL")
            .expect("tag present");
        assert_eq!(
            sized.bytes,
            Some(8_237_824 + 49_986_025_344),
            "shards must sum, not max"
        );
    }

    #[test]
    fn quant_options_withholds_partial_totals() {
        let opts = quant_options(&snapshot());
        for tag in ["UD-Q4_K_XL", "Q8_0"] {
            let opt = opts.iter().find(|o| o.tag == tag).expect("tag present");
            assert_eq!(opt.bytes, None, "{tag} has an unsized shard");
        }
    }

    const SIZED_JSON: &str = r#"{
        "sha": "0123456789abcdef0123456789abcdef01234567",
        "siblings": [
            {"rfilename": "Q8_0/m-Q8_0.gguf", "size": 300},
            {"rfilename": "UD-Q4_K_XL/m-UD-Q4_K_XL.gguf", "size": 100},
            {"rfilename": "UD-Q5_K_XL/m-UD-Q5_K_XL.gguf", "size": 200}
        ]
    }"#;

    #[test]
    fn quant_options_sorted_by_size_then_unsized_last() {
        let tags: Vec<String> = quant_options(&snapshot_from(SIZED_JSON))
            .into_iter()
            .map(|o| o.tag)
            .collect();
        assert_eq!(tags, ["UD-Q4_K_XL", "UD-Q5_K_XL", "Q8_0"], "size order");
        let mixed: Vec<Option<u64>> = quant_options(&snapshot())
            .into_iter()
            .map(|o| o.bytes)
            .collect();
        assert!(
            mixed.last().expect("options").is_none(),
            "unsized tags sort last: {mixed:?}"
        );
    }

    fn option(tag: &str, gib: u64) -> QuantOption {
        QuantOption {
            tag: tag.to_owned(),
            bytes: Some(gib * 1024 * 1024 * 1024),
        }
    }

    #[test]
    fn render_marks_exceeds_at_wired_limit() {
        // 100 GiB budget: 85 GiB is exactly the tight threshold, 100 GiB is
        // exactly at the limit (still tight), 101 GiB is over.
        let opts = [
            option("A", 84),
            option("B", 85),
            option("C", 100),
            option("D", 101),
        ];
        let table = render_quant_table(&opts, Some(100 * 1024));
        let verdict = |tag: &str| {
            table
                .lines()
                .find(|l| l.trim_start().starts_with(tag))
                .expect("row")
                .split_whitespace()
                .last()
                .expect("verdict")
                .to_owned()
        };
        assert_eq!(verdict("A"), "fits");
        assert_eq!(verdict("B"), "tight");
        assert_eq!(verdict("C"), "tight", "exactly at the limit is not 'fits'");
        assert_eq!(verdict("D"), "exceeds");
    }

    #[test]
    fn render_without_budget_omits_verdict_column() {
        let table = render_quant_table(&[option("A", 84)], None);
        assert!(table.contains("84.0 GiB"), "size missing: {table}");
        for word in ["fits", "tight", "exceeds", "wired limit"] {
            assert!(!table.contains(word), "{word} leaked without a budget");
        }
    }

    #[test]
    fn render_names_unknown_sizes_rather_than_guessing() {
        let opts = [QuantOption {
            tag: "Q8_0".to_owned(),
            bytes: None,
        }];
        let table = render_quant_table(&opts, Some(100 * 1024));
        assert!(table.contains("size unknown"), "{table}");
        assert!(
            !table.contains("fits"),
            "no verdict without a size: {table}"
        );
    }
}
