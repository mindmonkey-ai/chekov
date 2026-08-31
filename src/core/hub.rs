//! Hugging Face hub access behind a mockable seam (§8.2).
//!
//! All metadata goes through `HttpClient`, so tests inject fakes. Only the
//! shard download itself touches the network, over blocking `ureq`, and is
//! untested by design.

use serde::Deserialize;

use crate::core::progress::{CountingReader, Progress, Shard, Sink, format_size};
use crate::core::pullspec::RepoId;
use crate::error::ChekovError;

/// One JSON POST: url, body, optional bearer token.
#[derive(Debug, Clone)]
pub struct JsonRequest {
    pub url: String,
    pub body: String,
    pub bearer: Option<String>,
}

/// Client-measured stream timing: request-written → first SSE data frame,
/// and first data frame → stream end. Durations, not instants — the math
/// needs only the two windows.
#[derive(Debug, Clone, Copy)]
pub struct StreamMarks {
    pub to_first_data: std::time::Duration,
    pub first_to_done: std::time::Duration,
}

/// The HTTP boundary (§C.6). Implemented by `UreqClient` for real use and by
/// canned fakes in tests — no test ever touches the network.
pub trait HttpClient {
    fn get(&self, url: &str) -> Result<String, ChekovError>;
    fn post_json(&self, req: &JsonRequest) -> Result<String, ChekovError>;

    /// POST and read the response as a stream, timing it. Returns the full
    /// body plus the two durations the timing math needs. The default
    /// refuses: a client that cannot stream-time must say so, never fake
    /// marks around a buffered read.
    fn post_json_stream_timed(
        &self,
        _req: &JsonRequest,
    ) -> Result<(String, StreamMarks), ChekovError> {
        Err(ChekovError::ForeignTimingsUnsupported {
            runtime: "unknown".to_owned(),
            reason: "this HTTP client cannot stream-time responses".to_owned(),
        })
    }
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

    fn post_json_stream_timed(
        &self,
        req: &JsonRequest,
    ) -> Result<(String, StreamMarks), ChekovError> {
        // Same request shape as post_json; the read is incremental so the
        // first data frame can be timestamped. Thin network I/O — untested
        // by design, like the hub's shard download; the pure parts
        // (saw_first_data, the timing math) carry the tests.
        let started = std::time::Instant::now();
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
        if !(200..300).contains(&status) {
            let text = body
                .read_to_string()
                .map_err(|e| ChekovError::EndpointDown {
                    url: req.url.clone(),
                    reason: e.to_string(),
                })?;
            return Err(status_error(&req.url, status, text));
        }
        read_stream_timed(body.into_reader(), started, &req.url)
    }
}

/// The error `answered` reports for a non-2xx status — it always errors
/// when `status` is outside 200..300, so the fallback stays panic-free
/// without assuming that invariant holds forever.
fn status_error(url: &str, status: u16, text: String) -> ChekovError {
    let fallback = ChekovError::EndpointDown {
        url: url.to_owned(),
        reason: format!("unexpected status {status}"),
    };
    crate::core::proxy::serve::answered(url, status, text)
        .err()
        .unwrap_or(fallback)
}

/// Reads the streamed body incrementally, marking the first SSE data frame
/// and the point the stream ends. Thin network I/O — untested by design,
/// like the hub's shard download; `saw_first_data` and the timing math
/// carry the tests.
fn read_stream_timed(
    mut reader: impl std::io::Read,
    started: std::time::Instant,
    url: &str,
) -> Result<(String, StreamMarks), ChekovError> {
    let mut buffer = String::new();
    let mut first_data: Option<std::time::Duration> = None;
    let mut chunk = [0_u8; 8192];
    loop {
        let n = reader
            .read(&mut chunk)
            .map_err(|e| ChekovError::EndpointDown {
                url: url.to_owned(),
                reason: e.to_string(),
            })?;
        if n == 0 {
            break;
        }
        buffer.push_str(&String::from_utf8_lossy(&chunk[..n]));
        if first_data.is_none() && saw_first_data(&buffer) {
            first_data = Some(started.elapsed());
        }
    }
    let to_first_data = first_data.ok_or_else(|| ChekovError::EndpointDown {
        url: url.to_owned(),
        reason: "stream ended with no data frame".to_owned(),
    })?;
    Ok((
        buffer,
        StreamMarks {
            to_first_data,
            first_to_done: started.elapsed().saturating_sub(to_first_data),
        },
    ))
}

/// True once `buffer` holds a `data:` line with at least one non-whitespace
/// payload byte — the first real SSE data frame, not just the marker.
fn saw_first_data(buffer: &str) -> bool {
    buffer
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .any(|payload| !payload.trim().is_empty())
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
    if target.quant.is_none() {
        return Err(ChekovError::NoQuantSpecified {
            repo: target.repo.to_string(),
            available,
        });
    }
    let quant = resolve_quant(snapshot, target, available)?;
    let files: Vec<PlanFile> = snapshot
        .files
        .iter()
        .filter(|f| derived_quant(&f.rfilename).as_deref() == Some(quant.as_str()))
        .map(|f| PlanFile {
            path: f.rfilename.clone(),
            size: f.size,
        })
        .collect();
    let first_shard = files
        .iter()
        .find(|f| f.path.contains("-00001-of-"))
        .unwrap_or(&files[0])
        .path
        .clone();
    Ok(PullPlan {
        quant,
        files,
        first_shard,
    })
}

/// The spelling `plan_pull` will use, or the error that names the choices:
/// `QuantNotFound` with the table when nothing matches, `QuantAmbiguous` with
/// every spelling when more than one does.
fn resolve_quant(
    snapshot: &RepoSnapshot,
    target: &PullTarget<'_>,
    available: String,
) -> Result<String, ChekovError> {
    let quant = target.quant.unwrap_or_default();
    match select_spelling(snapshot, quant) {
        Ok(spelling) => Ok(spelling),
        Err(spellings) if spellings.is_empty() => Err(ChekovError::QuantNotFound {
            quant: quant.to_owned(),
            repo: target.repo.to_string(),
            available,
        }),
        Err(spellings) => Err(ChekovError::QuantAmbiguous {
            quant: quant.to_owned(),
            repo: target.repo.to_string(),
            spellings: spellings.join(", "),
        }),
    }
}

/// The repo's own spelling of the tag the user asked for.
///
/// An exact match wins outright. Otherwise the match is case-insensitive
/// (`Q4_K_M` selects a repo's `q4_k_m`) — and if that matches more than one
/// spelling, the caller refuses with all of them rather than picking one:
/// `Err` carries every spelling matched, empty when there is none.
fn select_spelling(snapshot: &RepoSnapshot, quant: &str) -> Result<String, Vec<String>> {
    let tags = available_quants(snapshot);
    if tags.iter().any(|t| t == quant) {
        return Ok(quant.to_owned());
    }
    let mut matched: Vec<String> = tags
        .into_iter()
        .filter(|t| t.eq_ignore_ascii_case(quant))
        .collect();
    match matched.len() {
        1 => Ok(matched.remove(0)),
        _ => Err(matched),
    }
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
///
/// A directory that is not itself a tag (`Model-IQ3_M/Model-IQ3_M-00001-of-
/// 00005.gguf`, the bartowski layout) falls through to the filename: the tag
/// is in the shard's own name, and reading nothing out of it would present a
/// full repo as "no .gguf files".
fn derived_quant(path: &str) -> Option<String> {
    let stem = path.strip_suffix(".gguf")?;
    if !is_weights(path) {
        return None;
    }
    if let Some((dir, _)) = path.split_once('/')
        && quant_like(dir)
    {
        return Some(dir.to_owned());
    }
    let stem = stem.rsplit('/').next().unwrap_or(stem);
    let stem = strip_shard_suffix(stem);
    // `-` and `.` both separate a tag from the model name: `Model-Q8_0` and
    // `Model.Q4_K_M` (the mradermacher layout). A version dot (`Qwen2.5-…`)
    // yields a candidate starting with a digit, which is not tag-like.
    stem.char_indices()
        .filter(|&(_, c)| c == '-' || c == '.')
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
///
/// Case-insensitive: `Qwen/*-GGUF` repos publish `q4_k_m`, and a tag is a
/// tag whichever case its publisher typed. The token keeps its own spelling
/// everywhere it is stored — it is also the download path.
fn quant_like(token: &str) -> bool {
    let upper = token.to_ascii_uppercase();
    let core = upper.strip_prefix("UD-").unwrap_or(&upper);
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

/// What to do with a `.part` once the server has answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeAction {
    /// Keep the existing bytes and write after them.
    Append,
    /// Truncate and write from zero — the server is sending the whole file.
    Restart,
}

/// The server's answer to a resume attempt, as the numbers that decide it.
/// Bundled because §3.4 caps `resume_action` at three arguments.
#[derive(Debug, Clone, Copy)]
pub struct RangeAnswer<'a> {
    /// Bytes already on disk in the `.part`.
    pub part_len: u64,
    pub status: u16,
    pub content_range: Option<&'a str>,
    /// The API's byte size for the file, when it published one.
    pub expected: Option<u64>,
}

/// Decide whether a resumed transfer may append. Pure, so every arm is tested
/// without a network.
///
/// Refuses rather than guesses: writing at the wrong offset would corrupt a
/// shard silently, which no later size check would catch.
pub fn resume_action(answer: &RangeAnswer<'_>) -> Result<ResumeAction, String> {
    match (answer.status, answer.expected) {
        (206, _) => appendable(answer),
        // 200: the server sent the whole file instead of the tail, so the
        // part is worthless but the transfer is fine. 416 with no published
        // size: the part may simply be the whole file — start over and find
        // out, because there is no number to check it against.
        (200, _) | (416, None) => Ok(ResumeAction::Restart),
        // A 416 for a range that starts inside a file the API says is longer
        // means the server's copy is not the copy that was planned.
        (416, Some(total)) => Err(format!(
            "server answered 416 to bytes={}- but the repo lists {total} bytes — \
             the server's file is not the one the plan was built from",
            answer.part_len
        )),
        (status, _) => Err(format!(
            "server answered {status} to bytes={}-",
            answer.part_len
        )),
    }
}

/// The 206 arm: append only when the tail starts exactly where the `.part`
/// ends and describes the file the plan expects.
fn appendable(answer: &RangeAnswer<'_>) -> Result<ResumeAction, String> {
    let Some(header) = answer.content_range else {
        return Err(format!(
            "server answered 206 to bytes={}- without a content-range header",
            answer.part_len
        ));
    };
    let Some((start, total)) = parse_content_range(header) else {
        return Err(format!(
            "server answered 206 to bytes={}- with an unreadable content-range {header:?}",
            answer.part_len
        ));
    };
    let expected = answer.expected.unwrap_or(total);
    if start != answer.part_len || total != expected {
        return Err(format!(
            "server answered 206 as bytes {start}-/{total} for a {}-byte partial file of an \
             expected {expected} bytes — refusing to write at the wrong offset",
            answer.part_len
        ));
    }
    Ok(ResumeAction::Append)
}

/// `bytes 100-199/200` → `(100, 200)`. `None` for `*` totals and anything
/// else the header may carry.
fn parse_content_range(header: &str) -> Option<(u64, u64)> {
    let (range, total) = header.trim().strip_prefix("bytes ")?.split_once('/')?;
    let (start, _end) = range.split_once('-')?;
    Some((start.trim().parse().ok()?, total.trim().parse().ok()?))
}

/// GET `url`, asking only for the tail when `from` is past zero.
///
/// `http_status_as_error(false)` so a 416 or a 200-instead-of-206 reaches
/// `resume_action` as a number to classify rather than as an opaque failure.
fn send(url: &str, from: u64) -> Result<ureq::http::Response<ureq::Body>, ChekovError> {
    let mut request = ureq::get(url).config().http_status_as_error(false).build();
    if from > 0 {
        request = request.header("Range", &format!("bytes={from}-"));
    }
    request.call().map_err(|e| ChekovError::HubRequestFailed {
        url: url.to_owned(),
        reason: e.to_string(),
    })
}

/// Classify the answer: `Restart` writes the `.part` from zero, `Append`
/// continues it. A fresh download only ever accepts a 200.
fn decide(
    res: &ureq::http::Response<ureq::Body>,
    url: &str,
    progress: &Progress,
) -> Result<ResumeAction, ChekovError> {
    let status = res.status().as_u16();
    let refused = |reason: String| ChekovError::HubRequestFailed {
        url: url.to_owned(),
        reason,
    };
    let from = progress.resumed_from();
    if from == 0 {
        return if status == 200 {
            Ok(ResumeAction::Restart)
        } else {
            Err(refused(format!("server answered {status}")))
        };
    }
    let answer = RangeAnswer {
        part_len: from,
        status,
        content_range: res
            .headers()
            .get("content-range")
            .and_then(|v| v.to_str().ok()),
        expected: progress.total(),
    };
    resume_action(&answer).map_err(refused)
}

/// Copy the body into the `.part` — appending when the shard resumed —
/// reporting bytes on stderr as they land. Returns the part's new length.
fn stream(
    part: &std::path::Path,
    res: ureq::http::Response<ureq::Body>,
    progress: &mut Progress,
) -> Result<u64, ChekovError> {
    let from = progress.resumed_from();
    let mut out = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(from > 0)
        .truncate(from == 0)
        .open(part)
        .map_err(|e| ChekovError::io(format!("opening {}", part.display()), e))?;
    let mut screen = std::io::stderr();
    let mut reader = CountingReader::new(res.into_body().into_reader(), from, |done| {
        drop(progress.emit(&mut screen, done));
    });
    let copied = std::io::copy(&mut reader, &mut out)
        .map_err(|e| ChekovError::io(format!("writing {}", part.display()), e))?;
    drop(reader);
    drop(progress.finish(&mut screen, from + copied));
    out.sync_all()
        .map_err(|e| ChekovError::io(format!("flushing {}", part.display()), e))?;
    Ok(from + copied)
}

/// Stream one file to `dest`, via a `.part` sibling so an interrupted transfer
/// never leaves a short file that `file_matches` would have to catch later.
///
/// Network path — exercised only by real pulls, never by tests (prompt §2.4).
fn fetch_to(url: &str, dest: &std::path::Path, progress: &mut Progress) -> Result<(), ChekovError> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ChekovError::io(format!("creating {}", parent.display()), e))?;
    }
    let part = dest.with_extension("part");
    let res = send(url, progress.resumed_from())?;
    if decide(&res, url, progress)? == ResumeAction::Restart && progress.resumed_from() > 0 {
        eprintln!(
            "server ignored Range — restarting {} from zero",
            progress.label()
        );
        progress.restart();
    }
    let written = stream(&part, res, progress)?;
    verify_length(written, progress.total()).map_err(|reason| ChekovError::HubRequestFailed {
        url: url.to_owned(),
        reason,
    })?;
    finalize(&part, dest)
}

/// A shard that ended short of its published size is never renamed into
/// place; the `.part` stays so the next run resumes it.
fn verify_length(written: u64, expected: Option<u64>) -> Result<(), String> {
    let Some(expected) = expected.filter(|expected| *expected != written) else {
        return Ok(());
    };
    Err(format!(
        "transfer ended at {written} bytes but the repo lists {expected} — \
         the partial file is kept so the next run can resume it"
    ))
}

/// fsync the finished `.part` and move it onto the real name.
fn finalize(part: &std::path::Path, dest: &std::path::Path) -> Result<(), ChekovError> {
    std::fs::File::open(part)
        .and_then(|handle| handle.sync_all())
        .map_err(|e| ChekovError::io(format!("flushing {}", part.display()), e))?;
    std::fs::rename(part, dest)
        .map_err(|e| ChekovError::io(format!("renaming {} into place", part.display()), e))
}

/// How many bytes of `part` may be resumed. Zero when there is none, and zero
/// after discarding one longer than the file can be — a corrupt part is never
/// kept and never appended to.
fn prepare_part(
    part: &std::path::Path,
    total: Option<u64>,
    label: &str,
) -> Result<u64, ChekovError> {
    let part_len = std::fs::metadata(part).map_or(0, |meta| meta.len());
    if let Some(total) = total
        && part_len > total
    {
        eprintln!(
            "{label}: partial file is {part_len} bytes but the repo lists {total} — \
             discarding it and starting from zero"
        );
        std::fs::remove_file(part)
            .map_err(|e| ChekovError::io(format!("removing {}", part.display()), e))?;
        return Ok(0);
    }
    Ok(part_len)
}

/// Fetch one shard: finish a complete `.part`, resume a partial one, or start
/// fresh — announcing which on stdout before the stderr progress begins.
fn download_shard(spec: &DownloadSpec<'_>, shard: Shard, sink: Sink) -> Result<(), ChekovError> {
    let target = spec.dest.join(&shard.label);
    let part = target.with_extension("part");
    let resume_at = prepare_part(&part, shard.total, &shard.label)?;
    if shard.total == Some(resume_at) {
        println!("{} already complete — finishing", shard.label);
        return finalize(&part, &target);
    }
    if resume_at > 0 {
        println!(
            "downloading {} … (resuming at {})",
            shard.label,
            format_size(resume_at)
        );
    } else {
        println!("downloading {} …", shard.label);
    }
    let url = resolve_url(spec.repo, spec.revision, &shard.label);
    fetch_to(&url, &target, &mut Progress::new(shard, resume_at, sink))
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
    let sink = Sink::for_stderr();
    for (index, file) in plan.files.iter().enumerate() {
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
        let shard = Shard {
            index: index + 1,
            count: plan.files.len(),
            label: file.path.clone(),
            total: file.size,
        };
        download_shard(spec, shard, sink).map_err(|e| failed(format!("{}: {e}", file.path)))?;
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
    use super::{RangeAnswer, ResumeAction, resolve_url, resume_action};

    /// A resume of a 100-byte `.part` of a 200-byte file, answered as asked.
    fn answered(status: u16, content_range: Option<&str>) -> RangeAnswer<'_> {
        RangeAnswer {
            part_len: 100,
            status,
            content_range,
            expected: Some(200),
        }
    }

    #[test]
    fn a_206_at_the_expected_offset_appends() {
        let answer = answered(206, Some("bytes 100-199/200"));
        assert_eq!(resume_action(&answer), Ok(ResumeAction::Append));
    }

    #[test]
    fn a_206_without_a_known_size_appends_on_the_offset_alone() {
        let answer = RangeAnswer {
            expected: None,
            ..answered(206, Some("bytes 100-199/200"))
        };
        assert_eq!(resume_action(&answer), Ok(ResumeAction::Append));
    }

    #[test]
    fn a_206_at_the_wrong_offset_names_all_three_numbers() {
        let answer = answered(206, Some("bytes 40-199/200"));
        let reason = resume_action(&answer).unwrap_err();
        for number in ["100", "40", "200"] {
            assert!(reason.contains(number), "{reason}");
        }
    }

    #[test]
    fn a_206_over_a_different_total_names_all_three_numbers() {
        let answer = answered(206, Some("bytes 100-998/999"));
        let reason = resume_action(&answer).unwrap_err();
        for number in ["100", "999", "200"] {
            assert!(reason.contains(number), "{reason}");
        }
    }

    #[test]
    fn a_206_without_a_content_range_is_refused() {
        let reason = resume_action(&answered(206, None)).unwrap_err();
        assert!(reason.contains("content-range"), "{reason}");
    }

    #[test]
    fn a_200_means_the_server_ignored_the_range_and_restarts() {
        let answer = answered(200, None);
        assert_eq!(resume_action(&answer), Ok(ResumeAction::Restart));
    }

    #[test]
    fn a_416_restarts_only_when_no_size_was_published() {
        let answer = RangeAnswer {
            expected: None,
            ..answered(416, None)
        };
        assert_eq!(resume_action(&answer), Ok(ResumeAction::Restart));
    }

    #[test]
    fn a_416_against_a_published_size_is_refused() {
        let reason = resume_action(&answered(416, None)).unwrap_err();
        assert!(reason.contains("416"), "{reason}");
        assert!(reason.contains("200"), "{reason}");
    }

    #[test]
    fn any_other_status_is_refused_by_name() {
        let reason = resume_action(&answered(503, None)).unwrap_err();
        assert!(reason.contains("503"), "{reason}");
    }

    /// A scratch directory holding one `.part` of `len` bytes.
    fn scratch_part(name: &str, len: usize) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("chekov-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        let part = dir.join("shard.part");
        std::fs::write(&part, vec![7u8; len]).expect("write");
        part
    }

    #[test]
    fn a_partial_file_is_resumed_from_its_current_length() {
        let part = scratch_part("resume-partial", 40);
        assert_eq!(
            super::prepare_part(&part, Some(100), "shard.gguf").expect("prepare"),
            40
        );
        assert!(part.exists(), "a resumable part is kept");
    }

    #[test]
    fn a_missing_part_resumes_from_zero() {
        let part = scratch_part("resume-missing", 0);
        std::fs::remove_file(&part).expect("remove");
        assert_eq!(
            super::prepare_part(&part, Some(100), "shard.gguf").expect("prepare"),
            0
        );
    }

    #[test]
    fn a_part_longer_than_the_file_is_discarded_not_appended_to() {
        let part = scratch_part("resume-overlong", 140);
        assert_eq!(
            super::prepare_part(&part, Some(100), "shard.gguf").expect("prepare"),
            0
        );
        assert!(!part.exists(), "a corrupt part is never kept");
    }

    #[test]
    fn a_short_transfer_is_refused_with_both_numbers() {
        let reason = super::verify_length(90, Some(100)).unwrap_err();
        assert!(reason.contains("90") && reason.contains("100"), "{reason}");
        assert!(super::verify_length(100, Some(100)).is_ok());
        assert!(super::verify_length(90, None).is_ok());
    }

    #[test]
    fn a_complete_part_is_renamed_into_place() {
        let part = scratch_part("resume-complete", 100);
        let dest = part.with_extension("gguf");
        super::finalize(&part, &dest).expect("finalize");
        assert!(!part.exists() && dest.exists());
    }

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

    /// A client with no override must say so, never fake marks around a
    /// buffered read (spec §2's default body).
    #[test]
    fn the_default_stream_timed_post_refuses_honestly() {
        let http = FakeHttp {
            body: String::new(),
        };
        let req = JsonRequest {
            url: "http://x".to_owned(),
            body: "{}".to_owned(),
            bearer: None,
        };
        let err = http
            .post_json_stream_timed(&req)
            .expect_err("the default must refuse, never fabricate marks");
        assert!(err.to_string().contains("cannot stream-time"), "{err}");
    }

    #[test]
    fn the_first_data_scan_fires_on_a_payload_byte_and_not_before() {
        assert!(!super::saw_first_data("event: x\n"));
        assert!(!super::saw_first_data("data:"));
        assert!(super::saw_first_data("data: {"));
        assert!(super::saw_first_data("event: x\ndata: 1\n"));
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

    const LOWER_AND_DOTTED_JSON: &str = r#"{
        "sha": "abcdef0123456789abcdef0123456789abcdef01",
        "siblings": [
            {"rfilename": "qwen2.5-0.5b-instruct-q4_k_m.gguf", "size": 10},
            {"rfilename": "Mistral-7B-v0.3.Q4_K_M.gguf", "size": 20},
            {"rfilename": "Model.i1-IQ3_XS.gguf", "size": 30},
            {"rfilename": "ud-q5_k_xl/Model-ud-q5_k_xl-00001-of-00002.gguf", "size": 40},
            {"rfilename": "ud-q5_k_xl/Model-ud-q5_k_xl-00002-of-00002.gguf", "size": 40}
        ]
    }"#;

    #[test]
    fn lowercase_and_dot_separated_tags_derive_in_the_repos_own_spelling() {
        let tags = available_quants(&snapshot_from(LOWER_AND_DOTTED_JSON));
        for expected in ["q4_k_m", "Q4_K_M", "IQ3_XS", "ud-q5_k_xl"] {
            assert!(
                tags.contains(&expected.to_owned()),
                "{expected} missing from {tags:?}"
            );
        }
    }

    #[test]
    fn a_spec_matches_a_tag_case_insensitively_and_records_the_repo_spelling() {
        let snap = snapshot_from(LOWER_AND_DOTTED_JSON);
        let target = PullTarget {
            repo: &repo(),
            quant: Some("IQ3_xs"),
            wired_mb: None,
        };
        let plan = plan_pull(&snap, &target).expect("case-insensitive match");
        assert_eq!(
            plan.quant, "IQ3_XS",
            "the repo's spelling is what gets stored"
        );
        assert_eq!(plan.files.len(), 1);
        assert_eq!(plan.files[0].path, "Model.i1-IQ3_XS.gguf");
    }

    #[test]
    fn two_spellings_of_one_tag_are_both_listed_and_an_inexact_spec_is_refused() {
        let snap = snapshot_from(
            r#"{"sha": "abcdef0123456789abcdef0123456789abcdef01", "siblings": [
                {"rfilename": "Model-Q4_K_M.gguf", "size": 10},
                {"rfilename": "Model.q4_k_m.gguf", "size": 10}
            ]}"#,
        );
        let tags = available_quants(&snap);
        assert!(
            tags.contains(&"Q4_K_M".to_owned()) && tags.contains(&"q4_k_m".to_owned()),
            "{tags:?}"
        );
        let ask = |quant: &'static str| {
            plan_pull(
                &snap,
                &PullTarget {
                    repo: &repo(),
                    quant: Some(quant),
                    wired_mb: None,
                },
            )
        };
        let err = ask("q4_K_m").expect_err("two spellings match");
        assert!(matches!(err, ChekovError::QuantAmbiguous { .. }), "{err}");
        assert!(
            err.to_string().contains("Q4_K_M") && err.to_string().contains("q4_k_m"),
            "{err}"
        );
        let exact = ask("q4_k_m").expect("an exact spelling wins outright");
        assert_eq!(exact.files[0].path, "Model.q4_k_m.gguf");
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
    fn a_named_folder_per_quant_derives_the_tag_from_the_shard_name() {
        // bartowski layout: the folder repeats the model name, the tag is in the file.
        let tag = |p: &str| super::derived_quant(p);
        assert_eq!(
            tag("Ornith-1.5-397B-IQ3_M/Ornith-1.5-397B-IQ3_M-00001-of-00005.gguf").as_deref(),
            Some("IQ3_M")
        );
        assert_eq!(
            tag("Ornith-1.5-397B-Q3_K_XL/Ornith-1.5-397B-Q3_K_XL-00003-of-00005.gguf").as_deref(),
            Some("Q3_K_XL")
        );
        assert_eq!(
            tag("UD-Q5_K_XL/Model-UD-Q5_K_XL-00001-of-00006.gguf").as_deref(),
            Some("UD-Q5_K_XL")
        );
        assert_eq!(
            tag("Ornith-1.5-397B-imatrix.gguf"),
            None,
            "calibration data is not a quant"
        );
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
