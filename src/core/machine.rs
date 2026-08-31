//! What this Mac actually is, and what it can actually hold.
//!
//! Process spawning is confined to a few thin functions; every parser is a
//! pure `fn(&str) -> Option<T>` exercised against verbatim captured output, so
//! no test spawns a process (§8.2).
//!
//! Every number carries where it came from. `checks::effective_wired_mb`
//! reports 196608 MiB on a 256 GiB M3 Ultra where the engine reports 228065 —
//! understating the machine by 30.7 GiB — so an unlabelled number is exactly
//! the confident wrongness this tool exists to prevent.

use std::path::Path;

/// Where a number came from, most to least trustworthy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance {
    /// Reported by the engine that will actually load the model.
    EngineReported,
    /// Read from the OS as a configured fact.
    Measured,
    /// Derived by formula — correct only as far as the formula is.
    Predicted,
}

impl Provenance {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::EngineReported => "engine-reported",
            Self::Measured => "measured",
            Self::Predicted => "predicted",
        }
    }
}

/// A number that cannot be constructed without saying where it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Probed<T> {
    pub value: T,
    pub provenance: Provenance,
}

impl<T> Probed<T> {
    #[must_use]
    pub const fn new(value: T, provenance: Provenance) -> Self {
        Self { value, provenance }
    }
}

/// `  MTL0: Apple M3 Ultra (228065 MiB, 228064 MiB free)` -> (name, total, free).
#[must_use]
pub fn parse_list_devices(out: &str) -> Option<(String, u64, u64)> {
    let line = out
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("MTL") && l.contains(" MiB,"))?;
    let (_, after_colon) = line.split_once(": ")?;
    let (name, paren) = after_colon.rsplit_once(" (")?;
    let inside = paren.strip_suffix(')')?;
    let (total, free) = inside.split_once(", ")?;
    let total = total.strip_suffix(" MiB")?.trim().parse().ok()?;
    let free = free.strip_suffix(" MiB free")?.trim().parse().ok()?;
    Some((name.trim().to_owned(), total, free))
}

/// `"gpu-core-count" = 80` anywhere in `ioreg -rc AGXAccelerator` output.
#[must_use]
pub fn parse_gpu_cores(out: &str) -> Option<u32> {
    let (_, after) = out.split_once("\"gpu-core-count\"")?;
    let (_, value) = after.split_once('=')?;
    value
        .trim_start()
        .split(|c: char| !c.is_ascii_digit())
        .find(|t| !t.is_empty())?
        .parse()
        .ok()
}

/// Split a batched `sysctl -n k1 k2 …` result, refusing a short read.
///
/// A dropped key yields fewer lines — with exit 0 for some keys and exit 1 for
/// others — so the line count is the only reliable signal that the values no
/// longer line up with the keys that were asked for.
#[must_use]
pub fn parse_sysctl_batch(out: &str, keys: usize) -> Option<Vec<String>> {
    let values: Vec<String> = out.lines().map(|l| l.trim_end().to_owned()).collect();
    (values.len() == keys).then_some(values)
}

/// The GPU budget in MiB, by the first rung that answers.
///
/// `iogpu_raw` is `sysctl -n iogpu.wired_limit_mb`, where 0 means "system
/// default in effect", not zero bytes.
#[must_use]
pub fn gpu_budget(
    engine: Option<&str>,
    iogpu_raw: Option<u64>,
    memsize_bytes: Option<u64>,
) -> Option<Probed<u64>> {
    if let Some((_, total, _)) = engine.and_then(parse_list_devices) {
        return Some(Probed::new(total, Provenance::EngineReported));
    }
    // 0 is the "system default in effect" sentinel, not zero bytes.
    if let Some(mb) = iogpu_raw.filter(|&raw| raw != 0) {
        return Some(Probed::new(mb, Provenance::Measured));
    }
    let (mb, _) = crate::core::checks::effective_wired_mb(0, memsize_bytes?);
    Some(Probed::new(mb, Provenance::Predicted))
}

/// Path of the engine binary whose device list is authoritative.
#[must_use]
pub fn engine_binary(engine_dir: &Path) -> std::path::PathBuf {
    crate::core::engine::server_binary(engine_dir)
}

/// Run one command, returning stdout when it exits 0.
fn capture(bin: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(bin).args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// One sysctl key, individually. Batching is only safe when the caller checks
/// the line count (see `parse_sysctl_batch`).
#[must_use]
pub fn sysctl_one(key: &str) -> Option<String> {
    capture("sysctl", &["-n", key]).map(|v| v.trim().to_owned())
}

/// The one thermal signal macOS gives without root: `powermetrics` needs
/// sudo, and the real pressure level is a C notification API this crate
/// does not link under `forbid(unsafe_code)`.
#[must_use]
pub fn pmset_therm() -> Option<String> {
    capture("pmset", &["-g", "therm"])
}

/// Free MiB the engine reports right now — the teardown check that the last
/// model's memory was actually released before the next one loads.
#[must_use]
pub fn live_gpu_free(engine_dir: &Path) -> Option<u64> {
    let binary = engine_binary(engine_dir);
    let out = binary
        .exists()
        .then(|| capture(&binary.to_string_lossy(), &["--list-devices"]))
        .flatten()?;
    parse_list_devices(&out).map(|(_, _, free)| free)
}

/// This machine's GPU budget, by the first rung that answers.
#[must_use]
pub fn live_gpu_budget(engine_dir: &Path) -> Option<Probed<u64>> {
    let binary = engine_binary(engine_dir);
    let engine = binary
        .exists()
        .then(|| capture(&binary.to_string_lossy(), &["--list-devices"]))
        .flatten();
    let iogpu = sysctl_one("iogpu.wired_limit_mb").and_then(|v| v.parse().ok());
    let memsize = sysctl_one("hw.memsize").and_then(|v| v.parse().ok());
    gpu_budget(engine.as_deref(), iogpu, memsize)
}

/// What this machine is, for the scan report.
#[derive(Debug, Clone)]
pub struct Machine {
    pub chip: Option<String>,
    pub model: Option<String>,
    pub memsize_bytes: Option<u64>,
    pub gpu_cores: Option<u32>,
    pub perf_threads: Option<u32>,
    pub budget: Option<Probed<u64>>,
    pub macos: Option<String>,
}

#[must_use]
pub fn probe(engine_dir: &Path) -> Machine {
    Machine {
        chip: sysctl_one("machdep.cpu.brand_string"),
        model: sysctl_one("hw.model"),
        memsize_bytes: sysctl_one("hw.memsize").and_then(|v| v.parse().ok()),
        gpu_cores: capture("ioreg", &["-rc", "AGXAccelerator", "-d", "1", "-w0"])
            .as_deref()
            .and_then(parse_gpu_cores),
        perf_threads: sysctl_one("hw.perflevel0.logicalcpu").and_then(|v| v.parse().ok()),
        budget: live_gpu_budget(engine_dir),
        macos: capture("sw_vers", &["-productVersion"]).map(|v| v.trim().to_owned()),
    }
}

/// `sha256(model_id | memsize | brand | gpu_cores)`, first 12 hex chars.
///
/// Spec §4.2. `None` when ANY component is unknown — a partial identity
/// would let a bench row from another machine compare as if it were this
/// one's.
#[must_use]
pub fn machine_id(m: &Machine) -> Option<String> {
    let key = format!(
        "{}|{}|{}|{}",
        m.model.as_deref()?,
        m.memsize_bytes?,
        m.chip.as_deref()?,
        m.gpu_cores?
    );
    Some(crate::core::hash::sha256_hex(key.as_bytes())[..12].to_owned())
}

#[cfg(test)]
mod tests {
    use super::{
        Machine, Provenance, gpu_budget, machine_id, parse_gpu_cores, parse_list_devices,
        parse_sysctl_batch,
    };

    fn m3_ultra() -> Machine {
        Machine {
            chip: Some("Apple M3 Ultra".into()),
            model: Some("Mac15,14".into()),
            memsize_bytes: Some(274_877_906_944),
            gpu_cores: Some(80),
            perf_threads: Some(24),
            budget: None,
            macos: Some("27.0".into()),
        }
    }

    #[test]
    fn machine_id_is_stable_and_refuses_partial_identity() {
        let full = m3_ultra();
        let id = machine_id(&full).expect("complete identity");
        assert_eq!(id.len(), 12);
        assert_eq!(id, machine_id(&full).expect("deterministic"));
        let mut partial = m3_ultra();
        partial.chip = None;
        assert_eq!(
            machine_id(&partial),
            None,
            "an invented id would let a foreign bench row compare as this machine's"
        );
    }

    /// Verbatim from `./llama.cpp/build/bin/llama-server --list-devices` on the
    /// author's M3 Ultra.
    const LIST_DEVICES: &str = "Available devices:\n  MTL0: Apple M3 Ultra (228065 MiB, 228064 MiB free)\n  BLAS: Accelerate (0 MiB, 0 MiB free)\n";

    #[test]
    fn the_engine_device_line_yields_name_and_budget() {
        let (name, total, free) = parse_list_devices(LIST_DEVICES).expect("an MTL line");
        assert_eq!(name, "Apple M3 Ultra");
        assert_eq!(total, 228_065);
        assert_eq!(free, 228_064);
    }

    #[test]
    fn no_metal_device_is_not_a_zero_budget() {
        // Absence of any MTL line means the Metal backend is missing, which is
        // a different thing from a device with no memory.
        assert_eq!(
            parse_list_devices("Available devices:\n  BLAS: Accelerate (0 MiB, 0 MiB free)\n"),
            None
        );
    }

    #[test]
    fn gpu_cores_are_read_from_the_ioregistry() {
        let out = "  +-o AGXAcceleratorG16X  <class AGXAcceleratorG16X>\n      \"gpu-core-count\" = 80\n      \"name\" = <\"agx\">\n";
        assert_eq!(parse_gpu_cores(out), Some(80));
        assert_eq!(parse_gpu_cores("nothing here"), None);
    }

    #[test]
    fn a_short_sysctl_batch_is_refused_rather_than_misaligned() {
        let full = "Apple M3 Ultra\nMac15,14\n274877906944\n";
        assert_eq!(
            parse_sysctl_batch(full, 3).as_deref(),
            Some(
                &[
                    "Apple M3 Ultra".to_owned(),
                    "Mac15,14".to_owned(),
                    "274877906944".to_owned()
                ][..]
            )
        );
        // One key dropped: every later value would be attributed to the wrong key.
        assert_eq!(
            parse_sysctl_batch("Apple M3 Ultra\n274877906944\n", 3),
            None
        );
    }

    #[test]
    fn the_engine_budget_outranks_the_arithmetic_fallback() {
        let b = gpu_budget(Some(LIST_DEVICES), Some(0), Some(274_877_906_944)).expect("a budget");
        assert_eq!(
            b.value, 228_065,
            "the engine's own ceiling is authoritative"
        );
        assert_eq!(b.provenance, Provenance::EngineReported);
    }

    #[test]
    fn an_explicit_iogpu_limit_is_measured_not_predicted() {
        let b = gpu_budget(None, Some(187_000), Some(274_877_906_944)).expect("a budget");
        assert_eq!(b.value, 187_000);
        assert_eq!(b.provenance, Provenance::Measured);
    }

    #[test]
    fn without_the_engine_the_formula_is_labelled_predicted() {
        let b = gpu_budget(None, Some(0), Some(274_877_906_944)).expect("a budget");
        assert_eq!(b.value, 196_608, "hw.memsize * 3/4, the existing rule");
        assert_eq!(
            b.provenance,
            Provenance::Predicted,
            "this rung is 31457 MiB low on a real M3 Ultra and must say so"
        );
    }
}
