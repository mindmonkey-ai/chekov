# `chekov capability` — Final Specification

**Synthesis note.** Three judges named three different winners, so I resolved the spine on aggregate: **Angle A** scored 27/24/25 (76) against B's 24/20/23 (67) and C's 24/23/22 (69) — it won the feasibility lens outright and led on points in the other two lenses despite the declared winners. Angle A is therefore the spine (`chekov capability`, provenance-as-a-type, zero-dep terminal-native, phased). Angle C's ClaudeFacade probe harness is grafted in wholesale as the bench core; Angle B's statistics, append-only store, and compiler-enforced provenance are grafted in; every fatal flaw the judges named is fixed below, and three of them I re-measured on this machine today rather than trusting the design docs.

**What I verified live (2026-08-25, this Mac Studio), because the judges disputed it:**

```
$ ./llama.cpp/build/bin/llama-server --list-devices
Available devices:
  MTL0: Apple M3 Ultra (228065 MiB, 228064 MiB free)
  BLAS: Accelerate (0 MiB, 0 MiB free)
$ sysctl -n iogpu.wired_limit_mb   →  0
$ sysctl -n hw.memsize             →  274877906944
```

`228065 MiB` is real and reproducible right now. `src/core/checks.rs:79-81` computes `memsize_bytes / 4 * 3 / (1024 * 1024)` = **196608 MiB** — a **31457 MiB (30.72 GiB) understatement**. The judge who could not reproduce it was looking at the wrong path; the binary exists at `llama.cpp/build/bin/llama-server` and `CHEKOV_HOME` is the repo root itself.

**And the sysctl trap that all three designs got wrong.** I ran both failure modes:

```
$ sysctl -n hw.memsize hw.cpufrequency hw.pagesize
274877906944
16384
exit=0                       # 3 keys → 2 stdout lines, EXIT ZERO, no signal at all

$ sysctl -n hw.memsize nosuch.key hw.pagesize
274877906944
sysctl: unknown oid 'nosuch.key'     ← stderr
16384
exit=1                       # 3 keys → 2 stdout lines, error on stderr
```

Angle A said "exits 1 and the whole batch is lost" — wrong on both counts. Angle B said "treat a short/blank field as Unknown" — there is no blank; the line is *dropped*, so B's parser would map `16384` onto `hw.cpufrequency`. Only Angle C's `lines.len() == keys.len()` assert survives contact with either mode, and even C's rationale ("exits non-zero") is only half right. **The rule in this spec: assert stdout line count against key count; treat exit status as advisory only.**

---

## 1. Name, pitch, and the IDEAS.md entry

### `chekov capability`

`chekov capability` is doctor's twin for the machine rather than the server. It probes what this Mac actually is (chip, P/E core split, GPU cores, the *engine-reported* Metal working-set budget, live memory pressure, per-volume disk headroom, macOS and llama.cpp build state), computes exact GGUF fit arithmetic from real header geometry rather than file-size folklore, renders the resulting frontier — model × quantization × context, with fits/tight/exceeds regions and tok/s contours — as a terminal grid and an optional self-contained SVG, recommends specific `org/repo:QUANT` candidates with every step of the sizing math printed, and benchmarks them by pushing Anthropic-shaped probes through **chekov's own `ClaudeFacade` translator** so a passing model has demonstrably survived the exact code path every Claude Code turn crosses. Every number it prints carries a provenance tag from a closed set of six, and the type system makes a bare unlabelled number a compile-time inconvenience rather than a review-time judgement call. It exists because chekov is currently wrong about its own machine by 30.72 GiB, and because "will this model survive an agent loop" is the only question a chekov user is actually asking.

### IDEAS.md entry (file this first — charter N13, `AGENTS.md:45`)

Appended to `IDEAS.md` in that file's own documented format:

```markdown
## Machine capability scan, frontier graph, recommendations and agent bench (2026-08-25)
`chekov capability {scan,graph,recommend,explain,bench,compare}` — probe the machine
(sysctl / ioreg / `llama-server --list-devices` / df+mount), render an ASCII+SVG frontier of
model × quant × ctx with fits/tight/exceeds and predicted-vs-measured tok/s, recommend
candidates with the sizing math shown, and benchmark them through chekov's own
Anthropic→OpenAI translator against a built-in fixture or the user's own repo.
Motivated by a measured defect: `checks::effective_wired_mb` reports 196608 MiB on this
machine where the engine reports 228065 MiB — chekov understates its own budget by 30.7 GiB.
Supersedes the arithmetic in `references/model-fit-sizing.md` (see "Model-fit sizing", above).
Proposed 2026-08-25 — status: OPEN
```

Note the cross-reference: `IDEAS.md` already carries **"Model-fit sizing (2026-08-21)"**, which cites "the machine's 182.62 GiB wired budget". I traced that number — it is not a third measurement, it is `config.toml`'s `wired_limit_mb = 187000` read as MiB (187000 / 1024 = 182.617 GiB). So there are exactly **two** competing budget figures in the tree (196608 computed, 228065 engine-reported) plus **one configured floor** (187000). This spec reconciles all three; §13 asks the human to confirm the reconciliation.

---

## 2. Command surface

One new `Cmd` variant carrying a nested `#[command(subcommand)]` action enum. The six-touchpoint subcommand tax (`AGENTS.md`) is paid **once** for six verbs instead of six times, and `dispatch()` (`src/cli.rs:76-93`, an exhaustive match — a missing arm fails to compile) gains exactly one line.

```rust
// src/cli.rs, inside `enum Cmd` (currently src/cli.rs:23-60)
/// Scan this machine, graph its model frontier, recommend and benchmark models
Capability(commands::capability::CapabilityCmd),

// src/cli.rs, inside `dispatch` (currently src/cli.rs:76-93)
Cmd::Capability(c) => c.run(ctx),
```

```rust
#[derive(Debug, clap::Args)]
pub struct CapabilityCmd {
    #[command(subcommand)]
    pub action: Option<CapAction>,   // None ⇒ CapAction::Scan(ScanArgs::default())
}
```

### 2.1 Full flag table

| Command | Flag | Type | Default | Meaning |
|---|---|---|---|---|
| `capability [scan]` | `--json` | bool | `false` | Emit the full `MachineFile` incl. every provenance tag |
| | `--no-engine-probe` | bool | `false` | Skip `llama-server --list-devices` (budget falls to rung c, loudly) |
| | `--save` | bool | `true` | Write `$CHEKOV_HOME/machine.toml`; `--no-save` to suppress |
| `capability graph` | `--ctx <N>` | `Vec<u32>`, repeatable | `[8192,16384,32768,65536,98304,131072,262144]` (config `ctx_ladder`) | X axis |
| | `--metric <M>` | `fit` \| `tok-s` | `fit` | Which value the first cell glyph encodes |
| | `--candidates <C>` | `registered` \| `catalog` \| `name,name` | `registered` | Y axis source |
| | `--width <COLS>` | `u16` | `$COLUMNS`, else `100` | Grid width |
| | `--svg [PATH]` | `Option<Option<PathBuf>>` | flag absent = no file; flag bare = `$CHEKOV_HOME/reports/frontier-<utc_compact_now>.svg` | Self-contained SVG; **path is printed, never opened** |
| | `--measured-only` | bool | `false` | Blank every cell whose inputs are predicted |
| | `--json` | bool | `false` | Emit the `Frontier` verbatim |
| `capability recommend` | `--ctx <N>` | `u32` | `98304` (registry `defaults.ctx_size`, `src/core/registry.rs:33`) | Context the fit is computed at |
| | `--role <R>` | `agent` \| `chat` | `agent` | `agent` enables the tool-parser hard gate |
| | `--top <N>` | `usize` | `8` | Rows printed after gating |
| | `--refresh` | bool | `false` | **The only networked path in the feature** |
| | `--allow-untooled` | bool | `false` | Admit candidates that fail the tool-parser gate, still marked |
| | `--json` | bool | `false` | Structured output incl. rejected rows and reasons |
| `capability explain <SPEC>` | `<SPEC>` | positional `String` | required | `org/repo:QUANT[@rev]` or a registered model name |
| | `--ctx <N>` | `u32` | model's effective ctx, else `98304` | |
| | `--json` | bool | `false` | |
| `capability bench` | `--models <LIST>` | `Vec<String>` comma | active model | Registered names only |
| | `--suite <S>` | `agentic`\|`fim`\|`longctx`\|`throughput`\|`all` | `agentic` | |
| | `--fixture` | bool | `true` (implied) | Built-in fixture corpus |
| | `--codebase <PATH>` | `Option<PathBuf>` | none | Mutually exclusive with `--fixture` |
| | `--repeats <N>` | `u8` | `5` (config `bench_repeats`) | Per throughput point; first is dropped as warmup |
| | `--depths <LIST>` | `Vec<u32>` comma | `[512, ctx/4, ctx*3/4]` clamped to fit | Throughput depth ladder |
| | `--judge <NAME>` | `Option<String>` | none | Registered model, binary tie-break only |
| | `--allow-exec` | bool | `false` | **Single gate** on every path that runs repo/fixture code |
| | `--resume <RUN_ID>` | `Option<String>` | none | Skip tasks already in that run's JSONL |
| | `--dry-run` | bool | `false` | Print the `Vec<BenchStep>` plan + wall-clock estimate, run nothing |
| | `--yes` | bool | `false` | Skip the `confirm` gate (`src/commands/mod.rs:90`) |
| | `--materialize-fixture <DIR>` | `Option<PathBuf>` | none | Write the fixture out for inspection and exit |
| | `--json` | bool | `false` | |
| `capability compare <A> <B>` | `<A> <B>` | positional run ids | required | Refuses on stamp mismatch |
| | `--json` | bool | `false` | |

`--json` is **per-action, never global**. A global `--json` would be silently ignored on actions that cannot honour it, which the creed forbids; `chekov capability compare --json` parses, `chekov capability --json compare` is a clap error.

### 2.2 Example invocations

**(1) The scan — the first honest answer about this machine**

```
$ chekov capability
machine 8d41f0c2a917 — Apple M3 Ultra (Mac15,14), macOS 27.0 (26A5416b)

  CPU           32 logical  ·  24 Performance / 8 Efficiency        measured  sysctl hw.perflevel*
  GPU           80 cores                                            measured  ioreg AGXAccelerator
  RAM           256.0 GiB (274877906944 B)                          measured  sysctl hw.memsize
  GPU budget    228065 MiB (222.72 GiB)                    engine-reported  llama-server --list-devices
    · iogpu.wired_limit_mb = 0 (system default in effect)  measured  sysctl
    · arithmetic fallback would say 196608 MiB — 31457 MiB (30.7 GiB) LOWER.
      chekov used the engine-reported figure. `chekov run` now gates on the same number.
  Single-buffer cap  ~167.0 GiB (hw.memsize × 0.6525)               predicted  one-machine formula
  Bandwidth     819.2 GB/s                                             table  (M3 Ultra, 80 cores) high
  Config floor  187000 MiB                                            config  [limits] wired_limit_mb
  Engine        built · dda1b0d67 · Metal backend present (MTL0)   measured  git + --list-devices

  VOLUME          FS     FREE        TOTAL      MODELS HERE  CONTAINER
  /               apfs   200.3 GiB   926.3 GiB  1            disk3s
  /Volumes/jane   apfs   1.29 TiB    7.28 TiB   3            disk9s

  note: /Volumes/Recovery shares APFS container disk3s with / — its free space is the
        same 200.3 GiB, not additional. Deduped by container, not by mount point.

  5 probes measured, 1 predicted, 0 unavailable.
```

**(2) The graph** — see §5 for the full mock.

**(3) Pre-download recommendation**

```
$ chekov capability recommend --ctx 131072 --role agent --refresh
catalog: fetched snapshot, generated 2026-08-22 (3 days old)   [layer 2]
budget:  228065 MiB engine-reported   ·   ctx 131072   ·   KV q8_0/q8_0, flash-attn on

  #  REPO:QUANT                                   TOTAL     BUDGET%  ~tok/s@0  PARSER
  1  unsloth/Qwen3.8-27B-GGUF:UD-Q6_K_XL         26.9 GiB    11.8%   ~22.1     qwen3-coder-xml
  2  ornith-ai/Ornith-1.5-35B-A3B-GGUF:Q8_0      38.5 GiB    17.3%   ?         qwen3-coder-xml
  3  unsloth/gemma-4-31B-it-GGUF:UD-Q6_K_XL       ?           ?      ?         gemma4
  ...
  refused (4):
    OBLITERATUS/Qwen3.8-27B-OBLITERATED:Q4_K_M  — chat template (506 chars) falls through to the
        generic autoparser; no tool markup. `--role chat` or `--allow-untooled` to include.
    unsloth/Kimi-K3-GGUF:UD-Q2_K_XL             — 861.3 GiB exceeds any Mac.
    unsloth/DeepSeek-V4-Pro-0813-GGUF:UD-Q4_K_XL— 849.7 GiB exceeds any Mac.
    <repo>:UD-IQ1_S                             — effective bpw 2.37 below the 3.5 agent floor.
  `?` in TOTAL means the GGUF header has not been read; run `chekov capability explain <spec>`.
```

**(4) The math, printed line by line**

```
$ chekov capability explain ornith-1.5-35b-a3b --ctx 262144
ornith-ai/Ornith-1.5-35B-A3B-GGUF:Q8_0 @fbbaed45c2f0

  weights                                                      37802149120 B   measured  local file
  ctx requested                                                     262144
  C = PAD256(262144)                                                262144      predicted  formula
  n_layer  = block_count 41 − nextn_predict_layers 1                    40      measured  gguf header
  kv_layers= n_layer 40 / full_attention_interval 4                     10      measured  gguf header
  ek = ev  = key_length 256 × head_count_kv 2                          512      measured  gguf header
  bk = bv  = q8_0 = 34/32                                           1.0625      predicted  type table
  kv = 10 × (512×262144×1.0625)×2                               2852126720 B   predicted  formula
  overhead = 512×(4×262144 + 96×2048) + 1×248320×4 + 64MiB       705636352 B   predicted  formula
  ─────────────────────────────────────────────────────────────────────────
  total                                                        41359912192 B  = 38.52 GiB
  budget                                                      239143780352 B  = 222.72 GiB  engine-reported
  ratio 17.3%  <  85%                                            VERDICT: fits
  headroom                                                    197783868160 B  = 184.2 GiB

  decode tok/s: ? — this file's expert/shared tensor split has not been read,
                so active bytes per token are unknown. `chekov capability bench
                --models ornith-1.5-35b-a3b --suite throughput` to measure.
```

**(5) Benchmark, dry run first**

```
$ chekov capability bench --models qwen3.8-27b,ornith-1.5-35b-a3b --suite agentic --dry-run
plan: 2 candidates × 1 flag config = 2 server loads

  [1] load  qwen3.8-27b UD-Q6_K_XL  (25.9 GB from /Volumes/jane)    est  95s
      probes tool_emit(30) grammar_gap(30) diff_fidelity(12) tool_loop(6)
             think_leak(8) instruction_adherence(40) hallucination(all)   est 24m
      teardown + Metal residency drain                                    est  10s
  [2] load  ornith-1.5-35b-a3b Q8_0  (37.8 GB from /)                est 140s
      … same suite …                                                      est 31m
  ─────────────────────────────────────────────────────────────
  estimated wall clock 59m ± 15m.  Results append to
  $CHEKOV_HOME/eval/20260825T140312Z/results.jsonl after every task (--resume safe).
  run without --dry-run to execute.
```

**(6) Comparing two runs refuses when the stamps differ**

```
$ chekov capability compare 20260824T191203Z 20260825T140312Z
error: benchmark stamp mismatch on field `engine.build_commit`
       run 20260824T191203Z: a91f4c2  ·  run 20260825T140312Z: dda1b0d
       llama.cpp does not guarantee bit-identical logits across builds, so these
       runs are not comparable.
       re-run: chekov capability bench --models qwen3.8-27b --suite agentic
```

---

## 3. Capability scan — the probe table

Six probe groups, ~85 ms total, all through `std::process::Command` (the pattern `src/core/checks.rs:89-104` and `src/core/engine.rs:106-112` already establish). Every parser is a pure `fn(&str) -> Option<T>`; **no test spawns a process**, honouring `AGENTS.md`'s "no network, no real llama.cpp anywhere".

| # | What is read | Exact command | Yields | When unavailable |
|---|---|---|---|---|
| 1 | CPU identity, RAM, page size, core totals | `sysctl -n machdep.cpu.brand_string hw.model hw.memsize hw.memsize_usable hw.pagesize hw.logicalcpu hw.physicalcpu hw.nperflevels` (one process, ~5 ms) | 8 values, newline-separated **in argument order**. Verified here: `Apple M3 Ultra`, `Mac15,14`, `274877906944`, `16384`, `32`, `32`, `2` | **Parser asserts `stdout_lines.len() == keys.len()`.** On mismatch the *whole batch* becomes `Unavailable{why:"sysctl returned N lines for M keys — a key was dropped"}` and each key is retried individually via `sysctl_one`. Verified live: a dropped key produces fewer lines with **exit 0** (`hw.cpufrequency`) or **exit 1 with the error on stderr** (`nosuch.key`) — the line count is the only reliable signal. |
| 2 | P/E core split | `sysctl -n hw.perflevelN.name hw.perflevelN.logicalcpu` for `N in 0..hw.nperflevels`, **individually, never batched** | Verified here: `Performance`/`24`, `Efficiency`/`8`. Never assume index 0 is Performance — read `.name` | Level absent (single-tier chip) ⇒ `Unavailable`, not an error. Thread-count default falls back to `hw.logicalcpu` with the substitution named. |
| 3 | GPU core count | `ioreg -rc AGXAccelerator -d 1 -w0`, scan for `"gpu-core-count" = <N>` (~24 ms) | Verified here: `80`. Chosen over `system_profiler SPDisplaysDataType` because it is 13× faster (24 ms vs 308 ms) **and reads the IORegistry, so it works headless** — the exact machine class chekov targets | `Unavailable`. Consequence: the bandwidth table cannot be keyed and tok/s becomes `?`. Never interpolated — `Apple M3 Max` alone is ambiguous between 307.2 and 409.6 GB/s. |
| 4a | Configured wired limit | `sysctl -n iogpu.wired_limit_mb` | Verified here: `0` = "system default in effect", **not zero bytes**. Non-zero ⇒ that value **is** the budget, in **MiB (1048576 B, not 1e6)** — at 256 GiB the MB/MiB confusion is ~11 GiB | Fall to 4b. |
| 4b | **Authoritative GPU budget** | `<engine_dir>/build/bin/llama-server --list-devices` via `engine::server_binary` (`src/core/engine.rs:89-91`), ~53 ms | Verified here verbatim: `MTL0: Apple M3 Ultra (228065 MiB, 228064 MiB free)`. Regex `MTL\d+:\s+(.+?)\s+\((\d+)\s+MiB,\s+(\d+)\s+MiB free\)`. This is ggml's read of Apple's `recommendedMaxWorkingSetSize` — the ceiling llama.cpp's own fitter fits under. **Absence of any `MTL` line is a Metal-backend FAIL**, which no current doctor check can see | Fall to 4c, with the reason and `chekov setup` named. |
| 4c | Arithmetic fallback | `hw.memsize * 3 / 4` (the existing `checks::effective_wired_mb`) | 196608 MiB here — **measurably 31457 MiB low** | This is the last rung. It is stamped `Predicted{formula:"hw.memsize × 0.75"}` and **stamps the entire artifact** (§5). |
| 4d | Single-buffer cap | `hw.memsize * 0.6525` | ~167.0 GiB here. A single MTLBuffer above this fails even when the working-set budget is ample; raising `iogpu.wired_limit_mb` does not obviously lift it | Always `Predicted`. Verified on exactly one machine (M3 Ultra / macOS 27), so it carries `Confidence::Low` and is reported as a *separate* ceiling, never folded into the fit sum. |
| 5 | Live pressure | `sysctl -n vm.swapusage` + `vm_stat` | Reported as a subtraction from the budget with the holder named. A 256 GB Mac with Chrome and Docker open is not a 256 GB Mac | Reported as `Unavailable`; **never fails a check on its own** — on a 32 GB support-floor Mac another app's paging would make chekov blame the model, a loud failure pointing at the wrong remediation. |
| 6a | Disk free per volume | `df -Pk <path>...` (POSIX-stable `1024-blocks Used Available Capacity Mounted-on`) | Verified here: `/` 209989308 KiB free, `/Volumes/jane` 1388204564 KiB free | `Unavailable` per volume; the recommender then cannot gate on download space and says so. |
| 6b | Filesystem type | `mount`, first token inside the parens | Verified here: `/dev/disk3s1s1 on / (apfs, sealed, local, read-only, journaled)`. macOS `df` has **no `-T`**, and `stat -f "%T"` does **not** return fs type (it returns `/`) | Type renders `?`. exFAT detection matters — a 160 GB shard exceeds exFAT's practical comfort. |
| 6c | **Container dedupe** | Derived: strip the trailing `sN` from the `df` device (`disk3s1s1` → `disk3s`) | **Verified trap none of the three designs caught:** `df -Pk / /Volumes/*` here returns `/` *twice* (firmlink) plus `/Volumes/Recovery` on `disk3s3` reporting the *same* 209989308 KiB — APFS volumes in one container share free space. Summing them triple-counts 200 GiB | Fall back to dedupe by mount point, and print `container unknown — free space may be shared`. |
| 7 | macOS / build state | `sw_vers -productVersion`, `sw_vers -buildVersion`, `git -C <engine_dir> rev-parse --short HEAD`, `<engine>/build/bin/llama-server --version` | `27.0` / `26A5416b` / `dda1b0d67` here | Each independently `Unavailable`. Engine commit missing ⇒ bench refuses to record a stamp. |
| 8 | Metal family (enrichment, `--json` only) | `system_profiler SPDisplaysDataType -json` → `spdisplays_mtlgpufamilysupport` | `spdisplays_metal4` here. 308 ms, so it is never on the hot path. **`sppci_cores` is a JSON *string* `"80"`** — deserialize as `String` then `parse::<u32>()` | `Unavailable`. chekov never touches `SPHardwareDataType` at all, because it leaks `serial_number` / `platform_UUID` / `provisioning_UDID`. |

---

## 4. The capability model

### 4.1 Provenance — the closed set, and how it is enforced

Angle B's most honest sentence was *"PROVENANCE DISCIPLINE IS A CONVENTION, NOT A COMPILER GATE"*, and Angle A's counter-claim (*"a number with no provenance cannot be constructed"*) overstated what a struct field buys you. This is the honest merge of the two:

```rust
// src/core/machine.rs

/// Where a number came from. Six variants, closed. There is deliberately no
/// variant meaning "roughly" or "assumed": absence is `Unavailable`, and a
/// formula's output is `Predicted` no matter how confident the formula is.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum Provenance {
    /// Read directly off this machine (sysctl / ioreg / df / mount / git).
    Measured { probe: &'static str },
    /// Reported by the built engine (`llama-server --list-devices`, `/props`).
    EngineReported { probe: &'static str },
    /// Read from config.toml or models.toml.
    Config { key: &'static str },
    /// Compiled-in table keyed on hardware identity.
    Table { key: String, confidence: Confidence },
    /// Computed by a named formula from other values.
    Predicted { formula: &'static str },
    /// Not obtainable. `fix` is the remediation command when one exists.
    Unavailable { why: String, fix: Option<String> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence { High, Medium, Low }

impl Provenance {
    /// The single character every renderer appends. `·` predicted, `#` measured,
    /// `?` unknown. One function, so the grid, the table, the SVG and the JSON
    /// legend can never disagree about what a marker means.
    #[must_use]
    pub const fn marker(&self) -> char {
        match self {
            Self::Measured { .. } | Self::EngineReported { .. } | Self::Config { .. } => '#',
            Self::Table { .. } | Self::Predicted { .. } => '·',
            Self::Unavailable { .. } => '?',
        }
    }
}
```

```rust
/// A number that cannot be printed without saying where it came from.
///
/// Implements neither `Display` nor `Deref` nor `ToString`, and exposes no
/// public field. `render_with` is the only formatting API and always appends
/// `provenance().marker()`. Arithmetic goes through `value()`, which yields
/// `Option<&T>` — usable in math, a compile error inside a format string.
///
/// This is a strong nudge, not a proof: a caller can still `match p.value()`
/// and format the inner `T` by hand. That path is greppable
/// (`grep -n 'value()' src/ | grep format!`) and is a CI check, which is the
/// honest second line of defence rather than a claimed first one.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Probed<T> {
    value: Option<T>,
    provenance: Provenance,
}

impl<T> Probed<T> {
    #[must_use] pub fn known(value: T, provenance: Provenance) -> Self {
        Self { value: Some(value), provenance }
    }
    #[must_use] pub fn missing(why: impl Into<String>, fix: Option<String>) -> Self {
        Self { value: None, provenance: Provenance::Unavailable { why: why.into(), fix } }
    }
    #[must_use] pub fn value(&self) -> Option<&T> { self.value.as_ref() }
    #[must_use] pub fn provenance(&self) -> &Provenance { &self.provenance }

    /// The only rendering path. `?` when absent; otherwise `<formatted><marker>`.
    #[must_use] pub fn render_with(&self, fmt: impl Fn(&T) -> String) -> String {
        self.value.as_ref().map_or_else(
            || "?".to_owned(),
            |v| format!("{}{}", fmt(v), self.provenance.marker()),
        )
    }
}
```

### 4.2 The machine

```rust
#[derive(Debug, Clone, serde::Serialize)]
pub struct Machine {
    pub id: MachineId,
    pub brand: Probed<String>,          // "Apple M3 Ultra"
    pub model_id: Probed<String>,       // "Mac15,14"
    pub macos: Probed<OsVersion>,
    pub memsize_bytes: Probed<u64>,
    pub page_size: Probed<u32>,
    pub cores: Probed<CoreTopology>,
    pub gpu_cores: Probed<u32>,
    pub gpu_budget_bytes: Probed<u64>,      // the ladder, §4.3
    pub single_buffer_cap_bytes: Probed<u64>,
    pub bandwidth_bytes_per_sec: Probed<u64>,
    pub swap_used_bytes: Probed<u64>,
    pub volumes: Vec<Volume>,
    pub engine: EngineState,
}

/// sha256(model_id | memsize | brand | gpu_cores)[..12]. Newtype so a bench row
/// from another machine can never be compared as if it were this one's.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MachineId(String);

#[derive(Debug, Clone, serde::Serialize)]
pub struct CoreTopology { pub logical: u32, pub performance: u32, pub efficiency: u32 }

#[derive(Debug, Clone, serde::Serialize)]
pub struct Volume {
    pub mount_point: std::path::PathBuf,
    pub device: String,                 // "disk9s1"
    pub container: Option<String>,      // "disk9s" — free space is shared within one
    pub fs_type: Probed<String>,
    pub free_bytes: Probed<u64>,
    pub total_bytes: Probed<u64>,
    pub models_here: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct EngineState {
    pub binary_present: bool,
    pub commit: Probed<String>,
    pub metal_backend: Probed<bool>,    // an MTL* line appeared in --list-devices
}
```

The persisted form is a separate struct, because it is deserialized:

```rust
// $CHEKOV_HOME/machine.toml — chekov's own format, so §C.7 applies in full.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineFile {
    pub schema_version: u16,
    pub generated_at: String,           // clock::utc_compact_now()
    pub chekov_version: String,
    pub machine: MachineRecord,
}
```

**The `ModelEntry` trap** (Angle C caught this; A and B both missed it): `src/core/registry.rs:63` is `#[serde(deny_unknown_fields)]` *without* a container-level `default`, and `Registry::load` (`registry.rs:93-103`) turns any unknown key into `RegistryCorrupt`. So every field this feature adds to `ModelEntry` **must** carry `#[serde(default, skip_serializing_if = ...)]` or `chekov list` breaks on the author's own four-model `models.toml` on the very first run:

```rust
// src/core/registry.rs, appended to ModelEntry
/// GGUF header geometry, cached at pull time. Optional forever.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub geometry: Option<crate::core::sizing::Geometry>,
/// Latest bench run id for this entry, if any.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub bench_run: Option<String>,
```

### 4.3 The GPU budget ladder

```rust
/// Three rungs, tried in order. Never blends, never averages, never guesses.
pub fn gpu_budget(probes: &SysctlBatch, engine: &EngineState) -> Probed<u64> {
    if let Some(mib) = probes.iogpu_wired_limit_mb.value().copied().filter(|&m| m != 0) {
        return Probed::known(mib * 1_048_576, Provenance::Measured {
            probe: "sysctl iogpu.wired_limit_mb",
        });
    }
    if let Some(mib) = engine.mtl0_total_mib.value().copied() {
        return Probed::known(mib * 1_048_576, Provenance::EngineReported {
            probe: "llama-server --list-devices",
        });
    }
    match probes.memsize_bytes.value().copied() {
        Some(bytes) => Probed::known(bytes / 4 * 3, Provenance::Predicted {
            formula: "hw.memsize × 0.75 — folklore; measured 0.87 on macOS 27 / 256 GiB",
        }),
        None => Probed::missing(
            "neither sysctl nor the engine reported a GPU budget",
            Some("chekov setup".to_owned()),
        ),
    }
}
```

`src/core/checks.rs:77-83`'s `effective_wired_mb` keeps its signature and its unit tests, is demoted to rung (c), and its doc comment is corrected from *"~75% of physical RAM"* to *"~75% of physical RAM — folklore. Measured 87% (228065 MiB of 262144) on macOS 27 / M3 Ultra / 256 GiB, 2026-08-25. Use `machine::gpu_budget()`; this is the last rung."*

### 4.4 The fit and speed model

```rust
// src/core/sizing.rs — pure, zero I/O, no unwrap, every function ≤3 args via
// frozen dataclasses (§4→§5; note doctor.rs:31 `run_checks(http, cfg, eff)` is
// ALREADY at the clippy.toml cap of 3, so this is a live constraint).

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Geometry {
    pub arch: String,
    pub block_count: u32,
    pub nextn_predict_layers: u32,           // MTP blocks, subtracted
    pub full_attention_interval: Option<u32>,// hybrid/linear-attention models
    pub sliding_window: Option<u32>,         // iSWA models
    pub key_length: u32,
    pub value_length: u32,
    pub key_length_mla: Option<u32>,         // presence ⇒ MLA ⇒ no V tensor
    pub head_count: u32,
    pub head_count_kv: u32,
    pub embedding_length: u32,
    pub vocab_size: u32,
    pub expert_count: Option<u32>,
    pub expert_used_count: Option<u32>,
}

#[derive(Debug, Clone, Copy)] pub struct CacheTypes { pub k: GgmlType, pub v: GgmlType }
#[derive(Debug, Clone)] pub struct RunShape {
    pub ctx: u32, pub n_ubatch: u32, pub n_parallel: u32, pub kv_unified: bool, pub flash_attn: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum Fit { Fits, Tight, Exceeds, Unknown(FootnoteId) }

#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "basis", rename_all = "snake_case")]
pub enum Speed {
    /// Bandwidth-bound prior. Always a band, never a point.
    Predicted { lo: f64, hi: f64, formula: &'static str },
    /// Refit from ≥3 measured depths in this machine's own store.
    Measured { median: f64, p10: f64, p90: f64, reps: u8, run_id: String },
    Unknown(FootnoteId),
}
```

#### Cell count and the per-slot window

```
C        = PAD256(ctx)
C_slot   = if kv_unified { C } else { PAD256(C / n_parallel) }
```

This is llama.cpp's exact formula (`src/llama-context.cpp:288-304`). **The stored project memory note `llama-server-unified-kv-slots.md` is now inverted and must be corrected**: omitting `-np` resolves to `n_parallel = 4` **and** `kv_unified = true` *set together*, and the unified branch takes `n_ctx_seq = n_ctx`, so all four auto slots report the *full* context. The division happens only on an **explicit** `-np N` (N>1), which does *not* set `kv_unified`. chekov's `-np 1` default (`src/core/registry.rs:53-54`, verified in the live `models.toml`) remains correct — it is the one setting where `C_slot == C` on either branch — but the comment justifying it at `registry.rs:48-52` states the wrong mechanism and must be rewritten.

#### KV cache

```
n_layer   = block_count − nextn_predict_layers
kv_layers = full_attention_interval.map_or(n_layer, |i| n_layer / i)
ek        = key_length   × head_count_kv
ev        = value_length × head_count_kv
if key_length_mla.is_some():   ek = key_length ;  ev = 0        // MLA allocates K only
if sliding_window == Some(w):  SWA layers use PAD256(min(C, w + n_ubatch)) cells
bk, bv    = type_size / blck_size      // f16 = 2.0 ; q8_0 = 34/32 = 1.0625 (NOT 0.5)

kv_bytes  = Σ over kv_layers of ( ek × cells × bk  +  ev × cells × bv )
```

Every shortcut here is wrong for a real architecture already in this repo's `models.toml`. Using `block_count` as the layer count over-estimates `ornith-1.5-35b-a3b` by **4×** (41 blocks − 1 MTP = 40, ÷ `full_attention_interval` 4 = 10 KV layers). Treating `key_length_mla` as a cache dimension gets DeepSeek wrong by **71×**. Both errors refuse configurations that fit.

#### Compute reserve

```
overhead = n_ubatch × (4×C + 96×E)  +  n_parallel × V × 4  +  64 MiB
if !flash_attn:  overhead += head_count × C × n_ubatch × 4   // materialized f32 KQ
```

`4×C` covers the f16 KQ mask on the Metal and host sides; `96×E` upper-bounds the measured per-token scratch (40×E dense, 69×E MoE/hybrid); 64 MiB absorbs the fixed term and allocator slack. The flash-attention-off term is not a rounding error — 1024 MiB measured on a 16-head model at 32K/ub512, and it scales linearly in all three factors, so `-fa off` at 128K with 64 heads exceeds 100 GB. **`-ctv q8_0` with `-fa off` is a hard llama-server startup failure** (`model returns nullptr`), and chekov's own defaults pair `--flash-attn on` with `--cache-type-v q8_0`, so the validator must refuse that override rather than let the server die.

#### Fit

```
total   = weights + kv_bytes + overhead
Fits    if total × 100 <  budget × TIGHT_FRACTION_PCT     // 85
Tight   if total       <= budget
Exceeds otherwise
```

`TIGHT_FRACTION_PCT` is promoted from private (`src/core/hub.rs:217`) to `pub(crate)`, together with `verdict_for` (`hub.rs:255`) and `format_gib` (`hub.rs:244`), so the frontier, the recommender **and the existing quant table** (`render_quant_table`, `hub.rs:225-242`) are governed by **one** named tunable rather than four copies (§6).

#### Speed

```
active_bytes  = weights                                              (dense)
              = W_shared + (expert_used / expert_count) × W_expert   (MoE)
                where W_expert is the summed byte size of tensors matching
                blk.*.ffn_{gate,down,up}_exps.weight from the GGUF tensor-info

tok_s(0)      = η_shallow × BW / active_bytes                        η_shallow = 0.70
1/tok_s(d)    = a + b·d      a = 1/tok_s(0),  b = kv_bytes_per_token / (η·BW)
```

Once the store holds **≥3 distinct depths** for a `(machine, model, quant, flags)` tuple, `a` and `b` are refit by least squares from measurement and the basis flips `Predicted → Measured`. With fewer than 3 depths chekov prints the raw measured points and says *"insufficient depths to fit a curve"* — it does not extrapolate from two.

### 4.5 Worked numbers — 256 GB M3 Ultra

Model: `ornith-ai/Ornith-1.5-35B-A3B-GGUF:Q8_0` (the author's active model). Weights measured on disk: **37,802,149,120 B**. Geometry verified against a live `llama-cli -v` log: `block_count 41`, `nextn_predict_layers 1`, `full_attention_interval 4`, `ek = ev = 512`, `E = 2048`, `V = 248320`. Registry `ctx_size = 262144`, flags `--flash-attn on --cache-type-k q8_0 --cache-type-v q8_0 -np 1`.

```
C          = PAD256(262144)                              = 262144
kv_layers  = (41 − 1) / 4                                = 10
kv_bytes   = 10 × 2 × 512 × 262144 × 1.0625              = 2,852,126,720 B   (2.66 GiB)
overhead   = 512 × (4×262144 + 96×2048)                  =   637,534,208
           + 1 × 248320 × 4                              =       993,280
           + 64 MiB                                      =    67,108,864
                                                          = 705,636,352 B   (0.66 GiB)
total      = 37,802,149,120 + 2,852,126,720 + 705,636,352
           = 41,359,912,192 B                            = 38.52 GiB = 39,443 MiB

budget (EngineReported)  228065 MiB = 239,143,780,352 B  → ratio 17.29%  → FITS
budget (Predicted 0.75)  196608 MiB = 206,158,430,208 B  → ratio 20.06%  → FITS
headroom on the engine-reported budget                   = 184.2 GiB
```

**The case where the rung changes the verdict.** `unsloth/MiniMax-M2.7-GGUF:UD-Q5_K_XL` at `ctx_size = 163840` (the live registry entry), whose components `config.example.toml` already documents: 157.81 GiB weights + 20.59 GiB q8_0 KV + ~2.6 GiB compute = **181.00 GiB = 185,344 MiB**.

```
vs 228065 MiB (engine-reported)  → 81.27%  <85%   → FITS
vs 196608 MiB (0.75 folklore)    → 94.27%  ≥85%   → TIGHT
```

Same model, same arithmetic, **two different verdicts** depending on which rung answered. This is precisely why the budget ladder is the spine of the feature and why the artifact is stamped when rung (c) fires.

Decode speed, calibrated on this machine against `qwen3.8-27b` UD-Q6_K_XL (25,924,152,384 B, dense, 819.2 GB/s):

```
predicted  tok_s(0) = 0.70 × 819.2e9 / 25.924e9 = 22.12 tok/s   band 18.8 – 25.4
measured   23.1 tok/s shallow, 15.7 tok/s at 65536      ← inside the band ✓
refit      a = 1/23.1 = 0.043290 ;  b = (1/15.7 − a)/65536 = 3.113e-7
predicted at 131072 = 1 / (0.043290 + 3.113e-7 × 131072) = 11.9 tok/s   [Measured basis]
```

For `ornith-1.5-35b-a3b` the tok/s cell renders **`?`** with a footnote until the GGUF tensor-info is read, because the shared/expert byte split is what `active_bytes` needs and chekov does not have it yet. Inventing an "A3B ⇒ 8.5% active" number from the model *name* would be exactly the silent guess this design exists to refuse.

### 4.6 Worked numbers — 36 GB "M4 Pro"

*Inline caveat, as instructed:* Apple does not ship a 36 GB M4 Pro — M4 Pro configurations are 24/48/64 GB, and 36 GB is an M3 Pro / M4 Max tier. I have computed the numbers for a 36 GiB machine using M4 Pro bandwidth (273 GB/s at 20 GPU cores) as specified, and flag that the bandwidth row would render `?` on a real machine whose `(brand, gpu_cores)` pair is not in the table.

The engine cannot be probed on a machine I do not have, so this example deliberately runs at **rung (c)** — which is also the honest demonstration of what the artifact looks like when the ceiling is a guess.

```
hw.memsize = 36 GiB           = 38,654,705,664 B
budget (Predicted 0.75)       = 28,991,029,248 B = 27,648 MiB = 27.0 GiB
                                ^ stamped "CEILING PREDICTED — build the engine
                                  with `chekov setup` for a measured budget"
```

Candidate: `ornith-ai/Ornith-1.5-35B-A3B-GGUF:Q4_K_M`, weights **21,700,000,000 B** (summed from the HF tree endpoint 2026-08-25; re-summed at recommend time, never hardcoded).

| ctx | C | kv_bytes | overhead | total | ratio | verdict |
|---|---|---|---|---|---|---|
| 32768 | 32768 | 356,515,840 | 235,874,304 | 22,292,390,144 (20.76 GiB) | **76.9%** | Fits |
| 131072 | 131072 | 1,426,063,360 | 437,200,896 | 23,563,264,256 (21.94 GiB) | **81.3%** | Fits |
| 262144 | 262144 | 2,852,126,720 | 705,636,352 | 25,257,763,072 (23.52 GiB) | **87.1%** | **Tight** |

That is a real frontier row with a real boundary between 131072 and 262144 — the ctx ladder is not decorative.

Speed on the same machine for a dense candidate where the math is unambiguous, `unsloth/gemma-4-12b-it-GGUF:Q8_0` (12,700,000,000 B):

```
tok_s(0) = 0.70 × 273e9 / 12.7e9 = 15.0 tok/s        band 12.8 – 17.3   [Predicted]
```

Two honesty notes carried in the output: the efficiency factor η is **not** constant across chips (measured 77–78% on an M2 10-core, 73% on an M3 Pro 18-core, but only **59%** on an M3 Max 40-core — wide-GPU parts go compute-starved before bandwidth-starved), so the ±15% band is labelled *an unvalidated model, not a tolerance*, until refit from this machine's own measurements; and the whole 36 GB example must be recomputed on the real hardware because rung (c) is 12 points low on the one machine where both rungs are observable.

---

## 5. The graph

### 5.1 Data model — computed once, rendered three ways

```rust
// src/core/frontier.rs
#[derive(Debug, Clone, serde::Serialize)]
pub struct Frontier {
    pub machine: MachineId,
    pub budget: Probed<u64>,
    pub cache_types: CacheTypes,
    pub flash_attn: bool,
    pub n_parallel: u32,
    pub engine_commit: Probed<String>,
    pub ctx_ladder: Vec<u32>,                 // X axis
    pub candidates: Vec<Candidate>,           // Y axis, ascending weight bytes
    pub cells: Vec<FrontierCell>,             // row-major, len == candidates × ladder
    pub footnotes: Vec<Footnote>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct FrontierCell {
    pub weights_bytes: Probed<u64>,
    pub kv_bytes: Probed<u64>,
    pub overhead_bytes: Probed<u64>,
    pub total_bytes: Probed<u64>,
    pub fit: Fit,
    pub speed: Speed,
}
```

Three renderers over **one** model — `render_ascii`, `render_svg`, `to_json` — so no two views can disagree. `render_ascii` delegates to `budget_header`, `axis_line`, `cell_row`, `legend`, `footnotes`, each 15–25 LOC, matching the shape `hub::render_quant_table` (`hub.rs:225-242`) already uses to stay under the 40-LOC cap.

### 5.2 Predicted vs measured — the two-character cell

Angle B's insight, adopted: one glyph cannot carry two orthogonal facts. Every cell is **two characters**:

- **char 1 — the fit verdict** (always arithmetic): `#` fits · `+` tight · `.` exceeds · `?` unknown
- **char 2 — the provenance of the *inputs***: `#` measured (GGUF header read, weights on disk) · `·` predicted (bpw table, no header) · `?` unknown

So a cell is never visually ambiguous *before* the reader consults the legend, and the legend is mandatory — there is no flag that suppresses it. Under `--metric tok-s` char 1 becomes a band digit 1–9 (deciles of decode rate, edges printed in the legend) and char 2 keeps its meaning; a cell with unknown bandwidth or unknown geometry stays `??` and never becomes a digit.

### 5.3 Terminal mock

```
$ chekov capability graph --ctx 32768 --ctx 131072 --ctx 262144

  chekov capability frontier                              machine 8d41f0c2a917
  GPU budget   228065 MiB (222.72 GiB)     engine-reported · llama-server --list-devices
  KV types     q8_0 / q8_0    flash-attn on    slots 1 (n_ctx_seq == n_ctx either branch)
  engine       dda1b0d67                  candidates: 4 registered

                                            ctx →     32K     131K     262K
    ornith-1.5-35b-a3b        Q8_0     35.2 GiB        ##       ##       ##
    qwen3.8-27b         UD-Q6_K_XL     24.1 GiB        ##       ##       +#
    gpt-oss-120b               F16     59.2 GiB        #·       #·       ??  [2]
    minimax-m2.7        UD-Q5_K_XL    157.8 GiB        #·       +·       .·  [1]

    fit      #  fits (<85% of budget)   +  tight (85-100%)   .  exceeds   ?  unknown
    inputs   #  measured (gguf header read)   ·  predicted (bpw table)    ?  unknown

    [1] geometry predicted — this file's GGUF header has not been parsed.
        run `chekov capability explain minimax-m2.7` to read it (local, no network).
    [2] gpt-oss ships MXFP4 natively; the header parser has no expert/shared split
        for arch `gpt-oss` yet, so KV at 262144 is Unknown — not large, unknown.

  at ctx 32768:
    NAME                QUANT        WEIGHTS       KV  OVERHEAD      TOTAL  BUDGET%    tok/s
    ornith-1.5-35b-a3b  Q8_0         35.2 GiB#  0.33G#    0.22G·   35.8 GiB#   16.1%   41.2#
    qwen3.8-27b         UD-Q6_K_XL   24.1 GiB#  0.61G#    0.22G·   25.0 GiB#   11.2%   ~22.1·
    gpt-oss-120b        F16          59.2 GiB#      ?·        ?·        ?·        ?        ?
    minimax-m2.7        UD-Q5_K_XL  157.8 GiB#      ?·        ?·        ?·        ?        ?

    tok/s: bare = measured median (run 20260824T191203Z, 5 reps, p10-p90 41.0-41.5)
           ~    = predicted, ±15% band, bandwidth-bound prior η=0.70 — an unvalidated
                  model, not a tolerance. `chekov capability bench --suite throughput`
                  measures it. 3 depths are required before a curve is fitted.
```

### 5.4 The rules the graph must never break

Each is a unit test, not a guideline.

1. **No `Fits` from an incomplete cell.** `Fit::Unknown(id)` is a first-class variant. A test asserts no code path constructs `Fits`/`Tight`/`Exceeds` when any of `weights_bytes`/`kv_bytes`/`overhead_bytes` has `value() == None`.
2. **Unknown is blank-plus-footnote, never interpolated.** Never rounded from a nearby quant, never inherited from a neighbouring ctx, never taken from another machine's row.
3. **Reason text lives in numbered footnotes below the grid, never inside a cell.** `doctor` already stuffs multi-sentence errors into a DETAIL column that never wraps; this does not repeat that.
4. **An estimated ceiling stamps the whole artifact** (Angle C's rule, which beats per-cell labelling): when rung (c) fires the header reads `GPU budget 196608 MiB — CEILING PREDICTED from hw.memsize × 0.75. This is folklore; the measured ratio on macOS 27 / 256 GiB is 0.87. Build the engine with 'chekov setup' for a measured budget.` and every `Fits` in the grid is re-worded to `fits against a predicted ceiling`. A whole chart drawn on a guessed axis is a lie no per-cell marker can repair.
5. **Predicted and measured are never blended in one column.** A `--metric tok-s` grid built from a bench run labels the grid `measured` and any cell without a measurement stays `??`.
6. **Never interpolate bandwidth.** An unrecognised `(brand, gpu_cores)` pair yields `Unavailable`, never a nearest-neighbour guess. The binning traps make this mandatory: M3 Max is 307.2 GB/s at 30 cores but 409.6 at 40; M4 Max is 410 at 32 but 546 at 40.
7. **The ctx axis is labelled `configured --ctx-size`.** When a candidate's flags carry an explicit `-np N` (N>1) the row gains a `!` suffix and a footnote giving the real per-slot window from `C_slot`.
8. **Stale measurements are named, never carried silently.** When `machine.engine.commit` differs from the commit stamped on a measured cell, the footer reads `measured cells are from build <old>; the engine is now at <new>. Re-run 'chekov capability bench' to revalidate.`
9. **No quality axis on the graph.** Its two axes are memory and speed — both things chekov can measure here. Quality lives only in the bench table with its own provenance, because a single quality scalar per quant is the mistake every competing tool makes.

### 5.5 The exported version

`--svg [PATH]` writes a **self-contained** SVG (hand-emitted `String`, no dependency, no CDN, no JS) to `$CHEKOV_HOME/reports/frontier-<utc_compact_now>.svg` using `clock::utc_compact_now` (`src/core/clock.rs:37`), and **prints the path**. It does not spawn `open`: launching a GUI from a CLI is an unrequested side effect that both surviving designs independently refused, and a printed path composes with the user's own tooling.

The SVG carries: the frontier as `<rect>` cells, **solid fill for measured inputs and a 45° hatch `<pattern>` for predicted** (so the distinction survives greyscale printing and colour-blind viewers, which colour alone does not); the tight band 85–100% as a **shaded band, not a line** — even oobabooga's symbolic regression over 19,517 measurements and >10⁹ candidate formulas lands at 365 MiB median absolute error, so three states are the honest resolution chekov's arithmetic supports; measured throughput points as filled dots with p10–p90 whiskers and predicted ones as hollow dots with their ±15% range; a per-cell `<title>` tooltip printing that cell's actual arithmetic (`weights + kv + overhead = total vs budget`); and a footer listing every probe with its value and provenance tag. The legend text is generated by the *same* `legend()` function the ASCII renderer calls.

---

## 6. Recommendation engine

### 6.1 Candidate sourcing — three layers

One layer alone fails a documented way in each direction. A pure pass-through (llama.cpp ships no catalog; `docs/models.md` just links HF trending) never goes stale and gives zero recommendation value. A vendored static list rots visibly: ramalama's `shortnames.conf` still maps `codellama` to TheBloke repos with no upload since **2024-01-31**, and leaves its own selection criteria as an unfilled `[Document your criteria here]` placeholder.

**Layer 1 — compiled-in floor.** `include_str!("catalog/seed.toml")`, ~20 hand-verified entries spanning the memory tiers (16–24 / 32–48 / 64–96 / 128 / 192+ GB). Each carries `repo`, `quant`, **measured byte total from the tree endpoint**, `arch`, dense-vs-MoE derived from `expert_count` (never from card prose — a WebFetch summary of the MiniMax-M2.7 card called it "dense"; `config.json` shows `num_local_experts=256, num_experts_per_tok=8`), tool-parser label, native ctx, license, and `verified_at`. This makes `recommend` work on a cold, offline, first-run machine. It is never the only source and its age is always visible.

**Layer 2 — published snapshot, the layer that actually fixes staleness because it moves without a chekov release.** A `catalog.toml` attached to chekov's GitHub releases, fetched by `--refresh`, written atomically to `$CHEKOV_HOME/cache/catalog.toml` via **temp + `sync_all` + rename, with the pid in the temp name**. That last detail is a deliberate deviation from `Registry::save` (`registry.rs:106-116`), which uses a *fixed shared* `path.with_extension("toml.tmp")` and no fsync — a lost-update and torn-file hazard this feature declines to copy. The payload carries `schema_version`, `generated_at`, `min_chekov_version`; a snapshot demanding a newer binary is **refused** with `CatalogSchemaTooNew` naming `cargo install chekov-mac`, never partially parsed. Validation rejects the whole payload on duplicate or empty repo keys.

**Layer 3 — live HF query, only under `--refresh`.**

```
GET https://huggingface.co/api/models
      ?filter=gguf&filter=text-generation&expand[]=gguf&expand[]=downloads
      &sort=downloads&direction=-1&limit=100
GET https://huggingface.co/api/models/{repo}/tree/main?recursive=true
```

`expand[]=gguf` on the **list** endpoint is the efficiency win: one request returns architecture, `context_length`, parameter total **and the full chat template** for a whole page, so the tool-parser cascade runs across every candidate with no per-repo follow-up. Note `expand[]` is **not** supported on `GET /api/models/{repo}` — it returns an error there; chekov's existing revision-pinned `?blobs=true` call (`src/core/hub.rs:126`) is correct and stays exactly as it is.

**Sizing exclusions (mandatory, or sizes are wrong by multiples).** Never `gguf.totalFileSize` — it is one arbitrary file, measured as BF16 (71 GB) for ornith-ai, IQ2_M (12.5 GB) for bartowski, Q4_0 (16 GB) for unsloth on comparable repos. Never sum every `.gguf` — `unsloth/Qwen3.8-27B-GGUF` totals 472 GB across 30 files for a 16–29 GB model. Shards are grouped by stripping `-000NN-of-000NN.gguf`, and `mmproj-*`, `imatrix*`, `MTP/`, `mtp-*`, `dspark-*` are excluded (vision projectors, calibration data, multi-token-prediction layers, draft models). `mmproj-*` presence is separately recorded as the machine-readable multimodal signal.

**Weights are measured, never computed.** `hub::quant_options` (`hub.rs:191-213`) already sums shards per tag and — verified above — sets `bytes: None` when *any* shard's size was withheld, rather than publishing a partial sum. That refusal is reused unchanged. Never `params × nominal_bpw`: a quant name is a per-tensor **mixture**. Qwen3-30B-A3B Q4_K_M measures 76.9% Q4_K + 22.9% Q6_K + 0.3% F32 = **4.8606** effective bpw, not 4.5 — an 8% underestimate that turns a no-fit into a claimed fit. Unsloth `UD-*` deviates up to 50% and *model-dependently* (UD-IQ1_S is 2.370 bpw on a MoE, 1.890 on a dense model of the same family). The nominal bpw table exists only for sizing a quant that has not been published, and everything it produces is stamped `Predicted`.

**Staleness is loud, never automatic.** Every `recommend` prints the layer and the age (`catalog: fetched snapshot, generated 2026-08-22 (3 days old)`). Past `catalog_stale_days` (default 30) the line becomes a banner naming the exact fix. **chekov never auto-refreshes** — an offline-first tool that silently reaches for the network on an ordinary invocation is a surprise, and a recommendation that changed because a background fetch landed is not reproducible. Rate limits are read from the headers, not guessed: anonymous HF is `500 requests / 300 s` (`ratelimit-policy: "fixed window";"api";q=500;w=300`), remaining is in `ratelimit: "api";r=…;t=…`, and pagination follows the `link: <…cursor=…>; rel="next"` header rather than synthesising offsets.

### 6.2 Ranking — gate first, then sort

**Step 1 — hard gates. A rejected candidate is PRINTED with its reason, never dropped.**

| Gate | Rule | Evidence |
|---|---|---|
| Tool parser | Replay llama.cpp's `common_chat_try_specialized_template` substring cascade (`common/chat.cpp` ~3433-3552) in Rust against `gguf.chat_template` from the list endpoint. Falling through to the generic PEG autoparser is a **refusal** under `--role agent` unless `--allow-untooled` | `OBLITERATUS/Qwen3.8-27B-OBLITERATED` — 389,747 downloads, #1 trending GGUF text-generation, **506-character template with zero tool markup**, vs unsloth's 9,993-character one that resolves to Qwen3-Coder-XML. **Never rank by downloads or trendingScore** |
| Quality floor | effective bpw < 3.5 | Q3_K_S loses 5.7% aggregate on Llama-3.1-8B; the ppl delta jumps 13× from Q4_K_M to Q3_K_M |
| Agent floor | 1-bit dynamic quants | Vendor-documented tool-call failure — chekov's primary use case |
| Fit | `Fit::Exceeds` at the requested ctx | §4 |
| Size class | "exceeds any Mac" | Kimi-K3's smallest is 861.3 GB; Qwen3.8-2.4T-A95B's is 1310.9 GB. Download-sorted queries surface these to a 256 GB machine constantly |
| Disk | destination volume free bytes < download size (after container dedupe) | §3 probe 6c |
| Config | quantized `-ctv` with `-fa off`; asymmetric `-ctk`/`-ctv` on MLA/DeepSeek; quantized KV where `ek % 32 != 0` | Each returns `nullptr` at model load — catchable here instead of as a server death |

**Step 2 — sort key**, lexicographic then numeric so it is debuggable: `(fit_class, −quality_band, −predicted_tok_s)` where `quality_band` buckets effective bpw (`≥8` lossless, `≥5.5` high, `≥4.5` standard, `≥3.5` economy). This encodes "bigger model at lower quant wins, down to about Q4" as a **bounded** rule; the rule's supporting evidence is weaker than its ubiquity, it inverts below Q3, and Unsloth explicitly documents 1-bit dynamic quants failing at tool calling — the exact workload chekov feeds Claude Code. **Step 3 — tie-break** on smaller weights (leaves KV headroom).

**No composite score for un-benched candidates.** Angle C proposed a z-score composite; judge 3's objection is correct and adopted: z-scoring across the comparison set makes a model's number change when a *different peer* is benched, and single-run probes with ~30 prompts carry roughly ±9pp binomial noise. So `recommend` orders un-benched candidates by fit headroom and predicted tok/s only, and the table header says so. A composite appears **only** in `bench` output, **only** when every axis has ≥`repeats` samples, **only** with per-axis intervals printed above it, and normalized against **fixed absolute anchors** rather than the peer set (§7.5).

**Per-repo exception encoded as data, not code:** `gpt-oss` ships natively in MXFP4, so every quant lands at ~63 GB (Q4_K_M 62.8, Q8_0 63.4, F16 65.4). The seed entry carries `quant_choice = "no-op"` and the recommender says the quality delta is ~zero rather than emitting a technically-true, practically-useless "upgrade your quant".

---

## 7. Bench mode

### 7.1 The architectural move — probes go through chekov's own translator

This is Angle C's contribution and the reason bench is worth building at all. `chekov doctor`'s check 2 posts to `cfg.base_url() + "/v1/messages"` (`src/commands/doctor.rs:89-94`) — that is **llama-server's own** Anthropic door. But `chekov launch claude` points the agent at the in-process proxy: `set("ANTHROPIC_BASE_URL", format!("http://127.0.0.1:{proxy_port}"))` (`src/core/launch.rs:56-58`). **doctor has never tested the door Claude Code actually walks through.**

Every probe is therefore constructed as a `proxy::http::HttpRequest`, routed through `ClaudeFacade::route`, forwarded through the existing `HttpClient` seam, and translated back:

```rust
// src/core/bench/runner.rs — signatures verified against the tree today
let facade = ClaudeFacade::new(&eff.name);                       // proxy/claude.rs:28
let req = HttpRequest {                                          // proxy/http.rs:19-23
    method: "POST".to_owned(),
    path: "/v1/messages".to_owned(),
    body: probe.anthropic_body().into_bytes(),
};
match facade.route(&req)? {                                      // proxy/claude.rs:55
    Action::Forward(fwd) => {                                    // proxy/mod.rs:22-30
        let body = String::from_utf8(fwd.body)
            .map_err(|e| ChekovError::ProxyBadRequest { reason: e.to_string() })?;
        let upstream = ctx.http.post_json(&JsonRequest {         // hub.rs:20-23
            url: format!("{}{}", cfg.base_url(), fwd.path),
            body,
            bearer: Some(cfg.file.server.api_key.clone()),
        })?;
        let anthropic = facade.translate_response(&upstream)?;    // proxy/claude.rs:81
        grade(&anthropic)
    }
    Action::Reply(res) => grade_local(&res),
}
```

`Forward.body` is `Vec<u8>` (`proxy/mod.rs:28`), so the `from_utf8` conversion needs a `ChekovError` mapping that does not exist today — that is one small addition, not a hand-wave.

Streaming probes use the same seam: `stream: true`, buffered `post_json`, split on `data:` lines and fed through `facade.stream_translator()` (`proxy/claude.rs:86`) exactly as `serve::relay` does (`proxy/serve.rs:120-136`), then `finish()`. **No new HttpClient method, no async, no touched test fakes.** This is what makes streaming-only defects reachable — interleaved parallel tool-call deltas, an upstream error frame swallowed into a fake `end_turn`, an unterminated `<think>` eating the turn.

**Honest scope limit, narrowing Angle C's own claim:** this exercises **translation**, not the socket. It calls `facade.route()` directly and bypasses `proxy/serve.rs` and the hand-rolled HTTP/1.1 framing in `proxy/http.rs`, so no chunked-encoding, header, or connection-lifecycle bug is reachable. That is far more of the real path than doctor covers, and the report says exactly this rather than claiming "the exact code every Claude Code turn crosses".

### 7.2 The probes

| Probe | N | What it measures | Grading (all deterministic) |
|---|---|---|---|
| `tool_emit` | 30 | Native tool-call emission, unconstrained decoding | BFCL-style AST match on the `tool_use` block: name + sorted arg keys + values vs a golden record via `serde_json`. Includes **5 abstention** cases (no tool should fire) and **3 missing-function** cases (must not fabricate one) |
| `grammar_gap` | 30 | The same set with `json_schema` forced. **`gap = forced − unconstrained`** | The single best anti-self-deception device in any of the three designs: measuring only with the grammar on makes every model score ~100%, and **Claude Code does not force grammars**. A large gap means "works only with a babysitter" |
| `diff_fidelity` | 12 | Can it emit an applicable edit | Ladder: `git apply --check` succeeds → patched file parses → compiles (`cargo check --message-format=json`) |
| `tool_loop` | 6 | Multi-turn read→edit→verify against an in-process mock tool server with fully canned responses | Reached terminal state within K turns. Deterministic because the environment is canned |
| `think_leak` | 8 | Does `<think>` / `<\|channel\|>` text reach `content` in the **translated** body | String scan on the Anthropic side. **Live finding: all four entries in this repo's `models.toml` pass `--reasoning-format none`, which per `common/arg.cpp` means "leaves thoughts unparsed in message.content" — the setting that *causes* leakage, not the one that suppresses it.** If that is deliberate for the MiniMax lineage the reason belongs in a comment; if not, dropping the flag (`auto`) or using `deepseek` is the fix |
| `instruction_adherence` | 40 | IFEval-style verifiable coding constraints ("fenced rust only, no prose", "no `unwrap()`", "≤40 lines", "call exactly this function") | Regex / brace-depth. Strict **and** loose accuracy reported separately; the gap is a chattiness metric for an agent backend |
| `long_ctx_trace` | 16 | RULER-style 2-hop chains (const A → fn B → value C) planted in corpus text at 4 depths × 4 lengths | Exact match. Single-needle retrieval saturates and yields a flat useless column; multi-hop discriminates. Output: the largest length holding ≥90% — **fed back as a recommended `ctx_size` for `models.toml`**, a decision chekov already owns |
| `hallucination` | all | Every identifier and import in every snippet across the run, checked against the corpus symbol set plus declared dependencies (`Cargo.toml` / `package.json` / `pyproject.toml`) | Fraction that exist. Best value-per-LOC probe in the plan: no execution, no judge, and it measures the failure mode that actually breaks agent sessions |
| `throughput` | depths × repeats | Prefill and decode tok/s at depth | §7.4 |

### 7.3 Per-candidate lifecycle

Sequential by necessity — `minimax-m2.7` at UD-Q5_K_XL is ~181 GiB resident and cannot co-reside with anything.

1. **`run::preflight(ctx, &eff)`** (`src/commands/run.rs:27`, promoted to `pub(crate)`) — the same four refusal gates as `chekov run`. A model chekov would refuse to run is refused identically here, not benchmarked through a back door.
2. **Flag hygiene assertion** — emitted argv is checked against the built binary's own `--help` before spawning. chekov tracks tip-of-master with no pin, and upstream has *removed* (not deprecated) `--draft-max`/`--draft-min` behind an `arg_removed()` handler that **terminates startup**; `--mlock`/`--no-mmap`/`--direct-io` now warn in favour of `--load-mode`; `--defrag-thold` is a complete no-op whose handler discards the value. *Verified good news:* chekov's current defaults (`--jinja --flash-attn on --cache-type-k q8_0 --cache-type-v q8_0 -np 1`) and every `extra_flags` entry in the live `models.toml` are clean today. The assertion exists so a routine `chekov update --engine` cannot break bench silently.
3. **`server::spawn_daemon`** (`src/core/server.rs:165`) with `GGML_METAL_RESIDENCY_KEEP_ALIVE_S=5` in the child's environment. Metal keeps GPU memory wired for **3 minutes** after use by default, which makes a sequential sweep OOM nondeterministically on the *second* model and makes the failure vanish when runs are spaced out.
4. **Readiness gate** — poll `GET /health` every 500 ms via `std::thread::sleep` until 200, with a deadline, **while also polling the pid**. The endpoint is public (no API key) and 503 means "still loading", not "failed". `/health`, `/props`, `/models`, `/metrics` are all exempt from resetting the idle timer, so chekov can poll them freely. A server that died during load is reported as **died**, not as slow — closing the gap where `spawn_daemon` succeeds if the binary is merely executable.
5. **Config assertion** — `GET /props`, read `default_generation_settings.n_ctx` (which is the **per-slot** window, sourced from `meta.slot_n_ctx`, *not* the `-c` value) and `total_slots`. If per-slot ctx is below the longest probe's requirement, **abort loudly** rather than silently truncating and publishing a fake "this model can't do 32K" result. Cross-check against `C_slot` from §4.4.
6. **Probes** via §7.1, sampling pinned `temperature: 0, top_k: 1, seed: <config bench_seed, 42>`.
7. **Throughput** read from the `timings` object llama-server returns on **every** completion: `prompt_per_second` (prefill), `predicted_per_second` (decode), `cache_n` (prompt-cache reuse). This needs no `--metrics` flag and is already in hand — doctor currently discards exactly this data. Wall clock alone is never used: it conflates prefill with decode, a ~40% error at long context.
8. **Teardown** — `server::stop` + `clear_run_state`, then verify released budget via `--list-devices` before the next candidate.

**Explicitly out of scope: `llama-bench`.** Angle B wanted it; Angle A refused; judge 3 endorsed the refusal and I adopt it. It would require adding a fourth entry to `BUILD_TARGETS` (`src/core/engine.rs:11`, verified `[&str; 3]`), forcing every existing user through a multi-minute llama.cpp rebuild they did not ask for, in exchange for a metric that **explicitly excludes tokenization and sampling time** and therefore is systematically higher than real serving. Two columns that must never be averaged is a correctness burden with no user payoff here. If it is ever added, B's rule stands: separate columns forever, never averaged, never sharing a graph axis.

### 7.4 Variance — the part Angle A was missing

- `--repeats` default **5**. The **first repetition is dropped** as warmup and the drop count is recorded in the row.
- Report **median with p10/p90**, never mean±stddev. Throughput is right-skewed by thermal events; a mean flatters a run that hit one thermal stall.
- **"No significant difference" is defined and printed** rather than resolved into a false winner: two configurations are indistinguishable when their p10–p90 intervals overlap **and** the median difference is under `significance_pct` (default 5). Output reads `no significant difference (22.9 vs 23.4 tok/s medians, intervals overlap)`.
- **Comparison refuses across stamps.** `Stamp` = `{machine_id, engine_build_commit, gguf_sha256, quant, ctx, n_parallel, kv_unified, n_batch, n_ubatch, type_k, type_v, flash_attn, seed, temperature, chekov_version, prompt_set_hash, corpus_id}`. `compare` raises `BenchStampMismatch` naming the **first** differing field. This is not pedantry: llama.cpp does not guarantee bit-identical logits across batch sizes, because GPU reduction kernels pick different accumulation orders and float addition is non-associative. Greedy decoding removes *sampler* nondeterminism but not *kernel* nondeterminism, so determinism holds only inside one pinned configuration and the stamp is what pins it. The error message says exactly that, so a legitimate refusal does not read as an excuse.

### 7.5 Storage — append-only JSONL

`$CHEKOV_HOME/eval/<run_id>/results.jsonl`, one object per task, appended with `O_APPEND` in a single write after **every** task, so `--resume` loses at most one task. JSONL rather than TOML precisely because the file grows without bound and appending needs no read-modify-write — the lost-update hazard in `Registry::save`'s fixed shared temp path does not exist for a single-write row.

```json
{"schema":1,
 "run_id":"20260825T140312Z","seq":47,
 "machine":{"id":"8d41f0c2a917","brand":"Apple M3 Ultra","gpu_cores":80,"budget_mib":228065,
            "budget_source":"engine_reported"},
 "engine":{"build_commit":"dda1b0d67","backend":"Metal"},
 "model":{"name":"qwen3.8-27b","repo":"unsloth/Qwen3.8-27B-GGUF","quant":"UD-Q6_K_XL",
          "revision":"f1bfb127c64f7072bdd2cad55f258b9c8b2910fe","gguf_sha256":"a3f1…"},
 "config":{"ctx":131072,"n_ctx_seq":131072,"n_parallel":1,"kv_unified":false,
           "n_batch":2048,"n_ubatch":512,"type_k":"q8_0","type_v":"q8_0","flash_attn":true,
           "seed":42,"temperature":0.0,"top_k":1},
 "corpus":{"kind":"fixture","id":"fixture-v1","excluded_files":7},
 "task":{"suite":"agentic","probe":"tool_emit","id":"te-014","position_swap":null},
 "route":{"through":"claude_facade","translated":true,"stream":false},
 "measure":{"source":"serving","prompt_per_second":362.1,"predicted_per_second":21.8,
            "cache_n":0,"prompt_n":1841,"predicted_n":96,"warmup_dropped":1,"reps":5,
            "median":21.8,"p10":21.2,"p90":22.4},
 "grade":{"tier":"ast_match","pass":true,"detail":"name+args exact","judge":null},
 "stamp_hash":"7c2b…","prompt_set_hash":"e19a…"}
```

Plus one `stamp.json` per run holding the full `Stamp` and the complete `server::launch_args` array (`src/core/server.rs:96-111`).

`capability bench --json` and a future `export --redact` strip `machine.id`, hostnames and absolute paths. Imported foreign rows are tagged `"foreign": true`: they may inform a prior, but they can **never** render as this machine's measurement and can never satisfy the ≥3-depths curve-fit requirement.

**Composite scoring, when it appears at all:** per-axis table always printed **above** it; each axis normalized to [0,1] against **fixed documented anchors** (e.g. `tool_emit`: 0 = 0% AST match, 1 = 100%), never z-scored against the peer set; weights `agentic 0.45 (tool_emit .15, diff_fidelity .12, tool_loop .10, abstention+hallucination .08) · repo-FIM 0.25 · instruction 0.10 · long-ctx 0.10 · speed headroom 0.10`; classic pass@1 weighted **0**; and the composite is **withheld entirely** if any axis lacks `repeats` samples or is `N/A`, with the withholding reason printed.

### 7.6 How long a full run takes

Measured inputs from this machine: model load ≈ 95 s for 25.9 GB from `/Volumes/jane`, ≈ 140 s for 37.8 GB from `/`; decode ≈ 22 tok/s shallow, ≈ 12 tok/s at 131072; cold 65K prefill measured at **4.2 minutes**.

| Suite | Per candidate |
|---|---|
| `throughput` (3 depths × 5 reps) | ~8 min + load |
| `agentic` (probes 1–6, 8) | ~24–31 min + load |
| `fim` (§8/§9, 60 tasks) | ~18 min + load, +compile/test tiers |
| `longctx` (16 points, includes a cold deep prefill) | ~22 min + load |
| `all` | ~70–85 min + load |

A 3-model × `agentic` sweep is therefore **~1.5–2.5 hours**. `bench` prints that estimate and requires `super::confirm` (`src/commands/mod.rs:90`) before starting unless `--yes`, and `--dry-run` shows the plan as data first.

---

## 8. Codebase mode

Status 2026-08-30: slices A (Rust, same-file, tiers 1–5), B1 (`cross_file_first` with `input_extra`, two arms and the context lift), B2 (tiers 6–7 behind `--allow-exec`) and C (`--judge`, position-swapped binary judge as its own phase) shipped — see `docs/superpowers/specs/2026-08-29-codebase-mode-slice-a-design.md`, `…-slice-b1-design.md`, `2026-08-30-codebase-mode-slice-b2-design.md` and `2026-08-30-codebase-mode-slice-c-design.md`.

`chekov capability bench --codebase <PATH>` turns the user's own repository into graded tasks. A private codebase is the only guaranteed-uncontaminated corpus a local user has — which is exactly what LiveCodeBench's rolling-cutoff design provides and what no vendored public benchmark can.

**Safety gate first.** Refuse a dirty working tree (`WorkingTreeDirty { path }`). All work happens in a `git worktree` copy under `$CHEKOV_HOME/eval/<run_id>/tree/`, never the user's checkout. Nothing from the repo executes unless **`--allow-exec`** — the single gate on every code-running path in the whole feature (Angle C's consolidation, cleaner than scattering per-tier opt-ins). Without it the ladder stops at the parse tier and the report names which tier it reached and why. Every execution runs under a wall-clock timeout with a process-group kill. Implemented for Rust in slice B2 (`2026-08-30-codebase-mode-slice-b2-design.md`): one `cargo fetch` then `--offline`, a scratch `CARGO_TARGET_DIR`, 120 s/300 s wall clocks with a process-group kill, and a revert verified byte for byte after every crossing — `ExecWorktreeDirty` stops the run.

**Task generation — three tiers** (RepoBench taxonomy):

- `in_file` — mask a span inside a function whose dependencies are all local.
- `cross_file_first` — mask the **first** use of a symbol defined in another file. Hardest and most diagnostic: a model that did not read the other file cannot recover the signature. This is the tier where models actually separate; in-file completion saturates.
- `function_body` — mask an entire body at its boundary, never a random line range.

Sampling is deterministic: a seeded RNG keyed on `git rev-parse HEAD`, so the same commit always yields the same task set, and `compare` refuses across differing HEADs. Otherwise users watch a model "get worse" when their code changed.

**Mask selection without a parser, stated plainly.** The reference design (SAFIM) picks masks at AST node boundaries with tree-sitter. chekov ships a `MaskSource` trait with a `BraceBalanceMasker` implementation: brace-balance for C-family/Rust/Go/TS, indentation for Python, plus a language-specific signature regex. It is **cruder than AST boundaries and the report says so** — every codebase-mode task set is labelled `boundary-scanned (not AST)`. Masks failing a balance check are **discarded, not approximated**, so the set is smaller but never malformed. If tree-sitter is later approved it slots in behind the same trait with no caller changes (§11).

**Leakage filter — mandatory, and the single most important correctness control.** "Future context leakage" is a documented critical flaw in repo-derived benchmarks: after removing a function's definition, its test file, its callers and its docstring still reveal it. Before assembling context for masked symbol S, chekov drops:

(a) every file matching test globs — `tests/`, `*_test.*`, `test_*.py`, `*.spec.*`. A file containing `#[cfg(test)]` is **kept**: each `#[cfg(test)]`-attributed item is cut from its text (attribute through the matching `}` or the terminating `;`, literal-aware) before masking and before the symbol set is built, and the report prints how many lines were elided (amended 2026-08-29 — excluding the whole file left 7 of 63 files eligible on an idiomatic Rust repo);
(b) every file whose text contains S's identifier;
(c) the doc-comment or docstring immediately preceding the masked span;
(d) any README/docs file naming S.

**The exclusion count is written into every task record and printed per task**, so the user can audit that the mask was honest. An unauditable filter is indistinguishable from no filter. The design states plainly that this is a mitigation, not a proof — a symbol reachable only through a re-export, a macro, or a generated file can still leak.

**FIM delegation.** Infill tasks go to `POST /infill` with `input_prefix`, `input_suffix`, and cross-file context in `input_extra` (an array of `{filename, text}`). chekov **never** hand-rolls the sentinel tokens: families differ (`<PRE>/<SUF>/<MID>` vs `<|fim_prefix|>`, PSM vs SPM ordering), and llama.cpp resolves them from GGUF metadata. A "model does not support infill" error is recorded as a first-class **capability** result, scored **N/A**, excluded from the composite **with the renormalization printed** — never zero. Zeroing it would rank an excellent instruct model, exactly the kind chekov wants for Claude Code, below a mediocre base model.

**Deterministic scoring ladder** — cheapest to strongest, highest available tier awarded, **every tier reported separately** (never collapsed, because tier 6 is unavailable for most repos):

1. whitespace-normalized exact match
2. edit similarity `1 − lev(pred, gold) / max(len)` — hand-rolled two-row DP, ~25 LOC
3. identifier-set F1 — identifiers via `[A-Za-z_][A-Za-z0-9_]*` minus a per-language keyword list; catches API hallucination with no toolchain
4. parse gate — brace/indent balance, free
5. **repo-symbol existence** — the fraction of referenced identifiers that exist in the repo's own symbol set plus its declared dependencies. Best value-per-LOC probe in the design
6. compile gate — `cargo check --message-format=json` / `tsc --noEmit` / `python -m py_compile`, only when the toolchain exists — Rust shipped in slice B2; the JSON diagnostics are the verdict, not the exit status, and an `error` anywhere in the workspace counts.
7. test gate — run only the specific test covering the masked symbol, hard timeout, process-group kill, `--allow-exec` only — shipped in slice B2: the enclosing function (plus the cross-file symbol), the nearest `[package]`, up to five `#[test]` functions naming it as a whole word, `tests/*.rs` included.

Tiers 6 and 7 report **`Skipped`** when the toolchain or covering test is absent — never `Pass`. That is `doctor.rs:1-2`'s stated contract ("Skipped checks are reported as skipped, never as passed") applied verbatim. Tiers 1–2 are applied to **line-level tasks only**: on function-body masks they punish semantically-correct alternative implementations and reward formatting mimicry, so body-level tasks lean on tiers 3–7.

---

## 9. Fixture mode

`--fixture` (the default) materializes `fixture-v1` into `$CHEKOV_HOME/eval/fixture-v1/` from `include_str!` templates — no network, no download, no new dependency. It exists for the user with no suitable repo, and for cross-machine comparability, which the repo-derived suite cannot offer.

**Language: Rust.** chekov's own toolchain is guaranteed present on the target machine, so the compile and test tiers are **always available** rather than usually skipped — which is not true of any other language.

**Content: ~1,800 LOC across 9 files, a small event-sourced ledger.**

```
fixture-v1/
  domain/      entities plus invariants stated only in comments
  store/       a trait and two impls with deliberately different failure semantics
  projection/  a fold over a sliding window (off-by-one prone by construction)
  api/         a command dispatcher containing the near-miss API pair
  hidden/      held-out assertions — NEVER materialized into any prompt context
  manifest.toml
```

**The four anti-saturation devices** — this is what makes it discriminate rather than saturate:

1. **Cross-file first-use masks (6 tasks).** The masked span is the first call site of a symbol defined two modules away. A model that read only the local file cannot produce the right call.

2. **The near-miss API (the central discriminator).** `apply_entry` and `append_entry` coexist with different semantics — one folds into the projection, the other only appends to the log. Choosing the wrong one **compiles cleanly** and passes the obvious behavioural assertion; it fails only the hidden call-count assertion against an instrumented fake. This defeats models that pattern-match on plausible-looking code, and it cannot be passed by formatting mimicry.

   *Concrete task:* `api/dispatch.rs` line 61, body masked —
   ```rust
   fn handle_credit(&mut self, cmd: CreditCommand) -> Result<Receipt, LedgerError> {
       // MASKED — 4 lines. Must update the running balance projection.
   }
   ```
   Both `self.log.append_entry(e)` and `self.log.apply_entry(e)` compile and return `Ok`. Only `apply_entry` advances `projection.balance`. Hidden assertion: `assert_eq!(fake.apply_calls(), 1)` and `assert_eq!(ledger.balance(), -4_250)`.

3. **Invariant traps (3 masked bodies).** The correct implementation must honour an invariant stated only in a comment two files away: *balances are `i128` cents and must never be constructed from an `f64`*. Plausible, compiling, idiomatic code that rounds through a float fails the hidden assertion. This measures whether the model integrated context or merely completed locally.

   *Concrete task:* `domain/money.rs` — masked constructor. `Cents::from_major(f64)` compiles and passes `assert_eq!(Cents::from_major(0.1 + 0.2).0, 30)`? No: `0.1 + 0.2 = 0.30000000000000004`, `× 100 = 30.000000000000004`, `as i128 = 30` — it *passes*. The hidden assertion uses `Cents::from_major_str("8014.35")`, where the float path yields `801434` instead of `801435`.

4. **A generic + lifetime knot (1 task).** One masked signature that type-checks exactly one way. Cheap to grade (the compile gate settles it) and it discriminates hard on Rust competence specifically.

   *Concrete task:* `store/replay.rs` — masked signature for a function returning an iterator borrowing from `&'a self` and filtered by a closure capturing `&'a Filter`. `impl Iterator<Item = &'a Entry> + 'a` compiles; the four plausible near-misses do not.

**Hidden grading — the mechanism, not just the intent.** The materialized copy ships **without** the hidden tests; their content lives only in the binary. `manifest.toml` names the hidden set, and the context assembler consults the manifest — files named there are excluded from every prompt **by construction**, not by a glob that might later be edited. Grading copies the workspace to a scratch dir, writes the hidden tests in, and runs them against the model's patch. Three properties follow: the model can never read the tests (physically absent from disk, not merely filtered); the tasks cannot be answered by pattern-matching a visible assertion; and the leakage filter from §8 runs over the fixture identically with its exclusion count printed, so the fixture's honesty is auditable by the same mechanism as a user repo's.

**Discrimination — a release gate and a runtime detector, both.**

- *Release gate (Angle A):* fixture-v1 does not ship until it has been measured against **three models of clearly different capability** and shown a real spread. If the spread is flat, the fixture is the problem, not the models, and the phase-4 notes publish the spread rather than assuming the traps work.
- *Runtime detector (Angle B):* when every candidate in a run scores above 90% or below 10% on a tier, chekov prints `the fixture is not discriminating on this candidate set (all scored <band>) — this tier is reported, not ranked` instead of publishing a meaningless ordering.

**Versioning.** `fixture-v1`'s id and content hash go into every `stamp.json`; a future `fixture-v2` can never be silently compared against a v1 run. **Ten task slots are held in reserve, unreleased**, so v1 can be hardened without renumbering existing tasks. The honest long-run caveat: a fixture that ships in a public repo becomes training data — codebase mode (a private repo) is the durable signal and the fixture is the convenient one.

---

## 10. Where a small local model is used

**The deterministic function does the entire required path.** Scan, graph, all sizing math, fit verdicts, tok/s prediction and curve fitting, catalog ranking including the tool-parser cascade (pure substring matching), throughput measurement and statistics, all three renderers, and grading tiers 1–7 plus the tool-call AST comparison, the diff-apply gate, the symbol-existence check and the IFEval-style constraint checks involve **zero model calls** beyond the candidates under test. The feature ships complete, correct, and offline with no small model at all. A recommendation engine whose answer depends on a model's opinion cannot be unit-tested, cannot be reproduced, and cannot be audited — and it would put an LLM in the path of a memory-safety-adjacent decision that is simple arithmetic.

**The one place a small model earns its place:** `--judge <NAME>` on `bench`, as a **binary tie-breaker only**, in exactly two situations where no programmatic oracle exists — (a) the fixture's near-miss and invariant traps, where a candidate compiles and passes the obvious assertion but a human would say "it reimplemented the helper"; and (b) codebase-mode function-body masks where candidate and reference differ textually but may be semantically equivalent, exactly where exact-match and edit-similarity punish a correct alternative implementation.

**Which model.** Any registered model carrying `role = "judge"`; the recommended pull is a 7–14B instruct model of a **different architecture family** from every candidate. Evidence for the placement: a 4B–14B local judge scores ~88–90% on closed-ended binary correctness against a programmatic oracle (Phi-4-14B 89.55%, a 4B model 87.81% at a 10-token budget). The *same* models collapse on graded quality (Spearman 0.520–0.620 on SummEval, 0.215–0.570 on MT-Bench) and the **rankings invert** between paradigms — the best binary judge drops to rank 9 open-ended. So binary competence must never be extrapolated into a 1–5 score.

**The constraints, all mandatory and all enforced in code:**

- **Binary verdicts only.** `{"winner":"A"|"B"|"tie"}` or `{"pass":bool}`. Never a graded score, never a ranking across candidates, never any other probe.
- **Grammar-forced** via llama.cpp's `json_schema`, so parsing cannot fail and scoring is trivially deterministic.
- **10–20 token budget.** Extended reasoning matched or lost to a 10-token verdict on 8 of 13 judges for closed-ended questions.
- **Position swap is not optional.** Every call runs twice with A/B reversed; agreement is the verdict, disagreement is a **tie**. Judges pick the first slot ~68% of the time even against clear human preference; swapping raises human agreement from 65% to 77% and within-judge consistency from ~60% to ~85%. A single-order pairwise judge is close to a coin flip.
- **Self-consistency gates the score.** The swap-agreement rate is printed; below `judge_min_consistency_pct` (default 70) the judge sub-score is **voided entirely**, not down-weighted.
- **Frozen, versioned, hashed rubric prompt.** Never a user-tunable knob — a "Lenient" persona shifted a weak judge by 10.20% where the strongest judges varied under 0.55%. The prompt hash goes into `stamp.json` so scores from different chekov versions are never silently mixed.
- **Family refusal.** The judge's `general.architecture` is read from its GGUF and compared against every candidate; a match is refused with `JudgeFamilyConflict { judge, candidate }` naming a different judge. Self-preference and same-family preference are documented; a model judging its sibling produces a rigged table.
- **No ensembles, no debate.** Three-judge majority measured **+0.06%** over the best single judge, and multi-agent debate consistently *degraded* accuracy. That is 3× compute for noise.
- **Verbosity control.** Only extracted code blocks are compared, prose stripped, both sides truncated to an equal token budget.
- **Capped at ≤5% of the composite**, and it can only order a tie the deterministic score already declared — it can never overturn a deterministic result.
- **Scheduling cost is stated up front.** On a 256 GB box running a ~181 GiB model there is no room for a co-resident judge, so the judge pass runs as a separate phase after the candidate server stops. `--dry-run` shows the extra reload cost rather than discovering it as an OOM.

**The deterministic fallback, which is the default path.** With no `--judge`, or when the named judge is not registered, or on a family conflict, or when swap self-consistency falls below the floor, the axis is reported `N/A (judge unavailable: <specific reason>)`, the composite is recomputed over the remaining axes, **and the renormalization is printed**. The tie **stands as a tie** — never silently broken by a coin flip, never resolved by falling back to an arbitrary sort, never quietly zeroed. Any output touched by a judge names the judge model, its quant and revision, the rubric hash, the swap-consistency rate, and the exact weight it contributed.

`--judge` shipped as slice C, `docs/superpowers/specs/2026-08-30-codebase-mode-slice-c-design.md` — its §9 states four departures from this section's wording (a plan-time refusal instead of `N/A`, no composite so no 5% cap, `function_body` crossings only, and a `same_behavior` verdict rather than `winner`).

---

## 11. Charter compliance, dependencies, module plan

### 11.1 Charter walkthrough

**macOS / Apple Silicon only.** Every probe is a macOS-specific binary (`sysctl` OIDs, `ioreg AGXAccelerator`, `sw_vers`, `df`+`mount`). No cross-platform abstraction is invented for a single-platform tool.

**No async runtime.** Every hardware probe is a blocking `Command::output()`. All HTTP goes through the existing blocking `HttpClient` seam (`hub.rs:20-23`, `UreqClient` over ureq 3.4). Readiness polling is `std::thread::sleep(Duration::from_millis(500))`, the primitive `server::stop_pid` already uses. Streaming probes deliberately use the buffered `post_json` and re-split on `data:` lines rather than widening the trait with a streaming method. The bench sweep is **sequential**, which costs nothing because it must be: two large models cannot co-reside, and Metal's 3-minute residency keep-alive means even back-to-back loads need spacing. The no-async constraint and the hardware constraint agree.

**`#![forbid(unsafe_code)]`.** No FFI anywhere: no `libc`, no objc, no Metal linkage, no Swift. The GPU budget comes from parsing `--list-devices`, which is *why* that probe was chosen over calling Metal directly.

**40-LOC / 3-arg / depth-3** (`clippy.toml`, verified: `too-many-arguments-threshold = 3`, `too-many-lines-threshold = 40`, `excessive-nesting-threshold = 4`). This is a live constraint, not a formality — `run_checks(http, cfg, eff)` (`doctor.rs:31`) is **already at the 3-arg cap** and `DoctorCmd::run` is 30 of the allowed 40 lines. Concretely: `kv_bytes(&Geometry, &CacheTypes, cells)` is exactly 3 because the natural six-argument form becomes frozen dataclasses (the §4→§5 fix, not a workaround); every bench runner takes **one** argument, a frozen `BenchRequest`; the sweep is **data** (`plan_sweep(&BenchRequest) -> Vec<BenchStep>`, mirroring `EngineStep` at `engine.rs:13-20`) so the executor is a short loop shaped like `run_steps` (`engine.rs:95-104`, 10 LOC) and `--dry-run` inspects exactly what would run; `Machine::probe` is six named calls and a struct literal; `render_ascii` delegates to five ~20-LOC helpers; `CapabilityCmd::run` matches and delegates, nothing else. The frontier cell loop is a flat iterator chain producing `Vec<FrontierCell>` with a computed row-major index, not a nested double loop.

**thiserror in the library, anyhow only in `main.rs`.** Eight new `ChekovError` variants, each naming its remediation per the contract at `src/error.rs:1-2` ("Every variant's Display message must state what failed AND the exact remediation command — enforced by tests"): `MachineProbeUnavailable{probe,why}`, `EngineNotBuilt` → `chekov setup`, `CatalogStale{days}` → `chekov capability recommend --refresh`, `CatalogSchemaTooNew{needs}` → `cargo install chekov-mac`, `BenchStampMismatch{field,a,b}` → re-run command, `JudgeFamilyConflict{judge,candidate}`, `ToolCallingUnsupported{repo,template_len}`, `WorkingTreeDirty{path}`.

**No `unwrap()`/`expect()` outside tests.** Release is `panic = "abort"`, so a surviving unwrap is a process kill. Every probe returns `Option`/`Result`; `Probed::missing` is the *typed* representation of absence, so the code never holds an `Option` it is tempted to unwrap. Option combinators over `is_some()` + `unwrap()`.

**`deny_unknown_fields` on every externally-deserialized struct.** `CapabilitySection` (in the `DoctorSection` idiom, `config.rs:56-75`), `MachineFile`, the catalog snapshot and seed, and the bench stamp — all chekov's own formats. **Not** applied to HF API responses or to llama-server's `/props` and `timings`, matching the explicit exception already documented at `hub.rs:66-69` for third-party APIs whose schemas grow fields routinely. **This is the resolution of Angle B's charter deviation**: B filed a decision record asking to carve out upstream wire formats; the carve-out already exists as precedent in the tree and needs no new exception. GGUF metadata is likewise unknown-key-tolerant by necessity.

**The `ModelEntry` trap** (§4.2) — new registry fields carry `#[serde(default, skip_serializing_if = ...)]` or `Registry::load` fails on the author's own `models.toml` on first run.

**Six-touchpoint discipline, paid once:** `src/commands/capability.rs` → `pub mod capability;` in `commands/mod.rs:13-27` → `Capability(...)` variant in `cli.rs:23-60` → `Cmd::Capability(c) => c.run(ctx)` in `dispatch` → doc-comment help text → README command table → regenerated completions via the hidden `chekov completions` subcommand (`cli.rs:54-59`) that `make install` runs.

**Tests: no network, no real llama.cpp.** Hardware probes are tested as pure parsers against the verbatim strings captured in §3 — including `MTL0: Apple M3 Ultra (228065 MiB, 228064 MiB free)`, the exact `df -Pk` rows, the `mount` line with `(apfs, sealed, local, read-only, journaled)`, and **both** sysctl short-output cases. No test spawns a process, so CI on any machine passes. Bench and catalog inject fakes at the `HttpClient` boundary exactly as `SeqHttp` (`doctor.rs:216-220`) and `FakeHttp` (`hub.rs:451`) already do. Sizing carries the measured regression vectors: 28 layers / ek=ev=1024 / C=4096 / f16 → 469,762,048 B; same at C=32768 q8_0 → 998,244,352 B per side; 10 layers / ek=ev=512 / C=32768 / f16 → 671,088,640 B; MLA L=61 / ek=576 / ev=0 / C=32768 / f16 → 2,303,721,472 B; plus the two worked totals from §4.5–4.6.

**Product creed — nothing degrades silently.** Enforced by the `Probed`/`Provenance` types on every number; `Fit::Unknown` renders `??` and cannot be constructed from a complete-looking cell; an estimated ceiling stamps the whole artifact; grading tiers whose toolchain is absent report `Skipped`; the catalog never auto-refreshes and states its age; rejected candidates print their reason; a missing capability scores `N/A` with the renormalization printed; `compare` refuses on stamp mismatch naming the field; the judge's failure modes all fall back to a printed tie. And `-c` is **always** passed explicitly, precisely because llama.cpp's automatic memory fitting defaults **on** with a 4096 floor and will silently shrink an unset context — an explicit `-c N` (N>0) is documented as never modified, so this one rule eliminates the whole silent-shrink class.

### 11.2 Dependency decisions

**ZERO new dependencies in the shipped design. `Cargo.toml` is unchanged.** Each avoidance is justified with the alternative that was considered and why it was rejected.

| Need | Decision | Alternative considered, and why rejected |
|---|---|---|
| Hardware probes | `std::process::Command` on sysctl / ioreg / df / mount / git, plus the already-built `llama-server --list-devices` | **`libc` 0.2.189** — already in `Cargo.lock` transitively via hf-hub → xet-runtime, so `libc::sysctlbyname` costs one line and zero new compilation and saves ~5 ms of fork. **Rejected:** chekov already shells out to llama-server (`server.rs:156-161`), git (`engine.rs:31-40`) and sysctl (`checks.rs:89-96`), so a shelled probe reviews the same way as the rest of the tool; the total probe budget is ~85 ms on a deliberate command; `sysctlbyname` cannot read GPU core count or the Metal budget anyway, so it would eliminate only two of eight probes; and it introduces an FFI boundary requiring an `unsafe` wrapper under `#![forbid(unsafe_code)]` |
| Machine facts | same | **`sysinfo` 0.38.4** — also already transitively present, offers `total_memory`/`Disks`. **Rejected:** it cannot answer GPU core count, `recommendedMaxWorkingSetSize`, or `iogpu.wired_limit_mb`, so it would add a direct dependency *and* still require every shell probe |
| JSON | `serde_json` — **already a direct dependency**, justified in `Cargo.toml` for the HF API and `/v1` endpoints | — |
| HTTP | existing `ureq 3.4` behind `HttpClient` | — |
| TOML | existing `toml 1.1` | — |
| SVG | hand-rolled `String`, ~80 LOC | **the `svg` crate.** Rejected on `core/clock.rs:1-5`'s own stated reasoning for declining chrono for two format sites — one renderer of a fixed grid of `<rect>` and `<text>` does not justify a dependency |
| Levenshtein | hand-rolled two-row DP, ~25 LOC | **`strsim`.** Rejected: single call site, textbook algorithm, must clear `cargo deny` for no benefit |
| Percentiles | ~15 LOC over a sorted `Vec<f64>` | any stats crate — same reasoning |
| Terminal width | `$COLUMNS`, fallback 100 | **`terminal_size`.** Rejected: `--width` is an explicit override and the fallback is never harmfully wrong |
| GGUF headers | hand-rolled, ~300 LOC over `std::fs` | a gguf crate. **Rejected:** chekov needs ~15 metadata keys and the tensor-info dimensions, not a general reader, and v3 is a stable documented binary layout. **The one real hazard, called out:** every length, count, dimension and offset is `u64` — a `u32` parser silently corrupts above 4 GB, which is every file chekov handles |
| HTTP server | existing hand-rolled `proxy/http.rs` | — |

**Named, not smuggled, and requiring human approval if wanted:** `tree-sitter` + `tree-sitter-{rust,python,typescript,go}` (5 crates, exact pins, must clear `cargo deny`). This is the one place a dependency would materially improve the design — SAFIM-style AST-boundary masks are strictly better than brace-balance heuristics, and exact identifier extraction would strengthen the hallucination probe. It is **not** added. The `BraceBalanceMasker` ships behind the `MaskSource` trait, the limitation is labelled in every report, and if AST-quality masks are wanted that is a separate proposal to the human.

**Seam change, budgeted deliberately (Angle B's catch, which Angle C hand-waved).** Pre-download GGUF headers need HTTP Range reads and the cheap staleness probe needs response headers. The current trait is exactly:

```rust
pub trait HttpClient {
    fn get(&self, url: &str) -> Result<String, ChekovError>;
    fn post_json(&self, req: &JsonRequest) -> Result<String, ChekovError>;
}
```

No request headers in, no bytes or response headers out. So Slice 4 lands **both** methods in one wave rather than churning the fakes twice:

```rust
fn get_range(&self, url: &str, first: u64, last: u64) -> Result<Vec<u8>, ChekovError>;
fn head_commit(&self, url: &str) -> Result<Option<String>, ChekovError>;  // x-repo-commit
```

Blast radius, named: `FakeHttp` (`hub.rs:451`), `SeqHttp` (`doctor.rs:216-220`), and the `tests/pull_dry_run.rs` fixture. Slices 1–3 confine `gguf.rs` to local files via `std::fs` so the seam is untouched until it must move.

### 11.3 Module plan

| File | Status | LOC | Owns |
|---|---|---|---|
| `src/core/machine.rs` | new | 330 | `Provenance`, `Confidence`, `Probed<T>`, `MachineId`, `Machine`, `Volume`, `EngineState`; six spawn functions; the matching pure parsers (`parse_sysctl_batch` with the line-count assert, `parse_gpu_core_count`, `parse_list_devices`, `parse_df_pk`, `parse_mount_fstypes`, `parse_sw_vers`, `parse_swapusage`); `gpu_budget()`; container dedupe |
| `src/core/bandwidth.rs` | new | 90 | `&[(&str, u32, f64, Confidence)]` keyed on **(brand, gpu_cores)**; unknown pair → `None`, never interpolated |
| `src/core/sizing.rs` | new | 380 | `TYPE_TRAITS` (24 ggml types, exact block bytes), `Geometry`, `CacheTypes`, `RunShape`, `pad256`, `kv_bytes`, `overhead_bytes`, `total_bytes`, `fit_verdict`, `tok_s_prior`, `fit_reciprocal_linear`; all regression vectors as tests |
| `src/core/gguf.rs` | new | 300 | GGUF v3 header reader (all lengths `u64`), metadata KV block, tensor-info; the ~15 keys sizing needs; expert/shared byte split; 32 MiB bounded read. Local `std::fs` in slice 3, Range-backed in slice 4 behind one `parse_header(&[u8])` |
| `src/core/frontier.rs` | new | 320 | `Frontier`, `FrontierCell`, `Fit`, `Speed`, `Footnote`; `render_ascii` (+ `budget_header`/`axis_line`/`cell_row`/`legend`/`footnotes`), `render_svg`, `to_json` |
| `src/core/catalog.rs` + `catalog/seed.toml` | new | 340 + data | Three layers; atomic pid-named temp + `sync_all` + rename; `schema_version`/`generated_at`/`min_chekov_version`; `template_parser()` cascade; quant-string decoder (`UD-`/`i1-` prefix, bits, K vs IQ, tier); artifact exclusion filter; rate-limit header parsing and cursor pagination |
| `src/core/recommend.rs` | new | 260 | `gate()` → `Result<Gated, Rejected>`, `rank()`, `Vec<ExplainLine>` per row so `explain` is data not a second code path |
| `src/core/bench/mod.rs` | new | 60 | `BenchRequest` frozen dataclass (the 3-arg fix), re-exports |
| `src/core/bench/store.rs` | new | 180 | `BenchRow`, `Stamp`, `Stamp::first_difference`, O_APPEND JSONL writer, streaming reader, `--resume` |
| `src/core/bench/stats.rs` | new | 120 | `median`, `percentile`, `drop_warmup`, `compare() -> Verdict::{Faster,Slower,NoSignificantDifference}` |
| `src/core/bench/sweep.rs` | new | 140 | `plan_sweep(&BenchRequest) -> Vec<BenchStep>` grouped by reload boundary; wall-clock estimate |
| `src/core/bench/runner.rs` | new | 260 | The ClaudeFacade round trip; `/health` + pid readiness gate; `/props` assertion; flag-hygiene assertion; `timings` capture; teardown with residency drain |
| `src/core/bench/probes.rs` | new | 350 | The nine probes and their golden records as `const` tables, hashed into the stamp |
| `src/core/bench/grade.rs` | new | 280 | Tiers 1–7, AST tool-call match, `git apply --check` gate, IFEval constraint checks, think-leak scan on the translated body |
| `src/core/bench/corpus.rs` | new | 300 | `MaskSource` trait + `BraceBalanceMasker`, leakage filter with printed exclusion counts, HEAD-seeded sampler, worktree isolation |
| `src/core/bench/fixture.rs` + templates | new | 200 + data | fixture-v1 materialization, `manifest.toml`, hidden-test injection, discrimination-band detector |
| `src/core/bench/judge.rs` | new | 180 | Grammar-forced binary verdicts, position swap, frozen hashed rubric, family refusal, consistency void, disclosure footer |
| `src/commands/capability.rs` | new | 330 | `CapabilityCmd`, `CapAction`, one thin handler per action; no math, no parsing — the shape `doctor.rs:158-189` uses over `core/checks.rs` |
| `src/core/checks.rs` | changed | +40 | `effective_wired_mb` demoted to rung (c), doc comment corrected; `slot_ctx_expected(&RunShape)` added |
| `src/core/hub.rs` | changed | +80 | `TIGHT_FRACTION_PCT`, `verdict_for`, `format_gib` → `pub(crate)`; `get_range` + `head_commit` on the trait (slice 4) |
| `src/commands/run.rs` | changed | +8 | `preflight` → `pub(crate)`; wired-limit arm (`run.rs:51-58`) rewired onto `machine::gpu_budget()` |
| `src/core/config.rs` | changed | +45 | `CapabilitySection` (`ctx_ladder`, `efficiency_shallow_pct` 70, `efficiency_deep_pct` 50, `catalog_stale_days` 30, `bench_seed` 42, `bench_repeats` 5, `significance_pct` 5, `judge_min_consistency_pct` 70); `reports_dir()`, `cache_dir()`, `eval_dir()` beside `logs_dir()` |
| `src/core/registry.rs` | changed | +25 | `geometry`/`bench_run` with mandatory `#[serde(default)]` |
| `src/error.rs` | changed | +65 | Eight variants with remediation text |
| `src/cli.rs`, `commands/mod.rs`, `README.md`, completions | changed | +15 | The six touchpoints |
| `config.example.toml` | changed | +8 | **Correct the "75% of RAM (192 GiB)" comment**, which is factually wrong on this machine |
| `IDEAS.md` | changed | +12 | The entry from §1 |

**Total: ~3,920 new LOC + ~300 changed against a verified 7,403-LOC crate — roughly 53% growth.** That is the largest feature in the tool by a wide margin, and phasing is a correctness control here, not project-management garnish. Every slice below is independently shippable and useful.

---

## 12. Phased delivery

### Slice 1 — `chekov capability scan` (~470 LOC, zero deps, zero network)

`machine.rs` + `bandwidth.rs` + the Scan action + `machine.toml` + `--json`. **Critically, this slice also rewires `run::preflight`'s wired-limit arm (`run.rs:51-58`) onto `machine::gpu_budget()`.** Shipping the scan without it would create a tool that reports 228065 MiB while `chekov run` gates against 196608 MiB — two surfaces contradicting each other is worse than either defect alone.

**This is a gate-loosening change and must be announced as such.** `run.rs:51-58` is a *floor assertion* (`Some((actual, _)) if actual < required => Err(WiredLimitLow)`) against `[limits] wired_limit_mb`, not a fit ceiling. With the code default of 200_000 and the computed 196608, `chekov run` currently **fails**; with the author's `config.toml` value of 187000 it currently **passes**. Swapping in 228065 makes it pass in both cases. The CHANGELOG entry says "loosens the wired-limit floor check by using the engine-reported budget", not "fixes reporting".

*Acceptance test:* `cargo test` passes with parsers exercised against the verbatim strings in §3 including **both** sysctl short-output modes; a test asserts `gpu_budget` returns `EngineReported` when `--list-devices` output is injected and `iogpu` is 0, `Measured` when `iogpu` is non-zero, and `Predicted` when both are absent; a test asserts `run::preflight` and `capability scan` resolve through the *same* function (single call site for `machine::gpu_budget`); manual: `chekov capability` prints `228065 MiB engine-reported` and names the 31457 MiB delta on this machine.

### Slice 2 — `chekov capability graph`, ASCII only (~480 LOC, still no network)

`sizing.rs` with the regression vectors, `frontier.rs` with the data model and terminal renderer, over **registered models only**. Weights come from files already on disk; KV renders as an explicitly labelled reserve band and every cell with unknown geometry is `??` with a footnote. `TIGHT_FRACTION_PCT` promotion lands here so the existing `render_quant_table` shares the constant.

*Acceptance test:* all five sizing regression vectors pass to the byte; a test asserts no code path constructs `Fit::Fits` from a cell with any `None` component; a test asserts that when the budget's provenance is `Predicted`, `budget_header` contains the string `CEILING PREDICTED` and every `fits` in the rendered grid legend reads `fits against a predicted ceiling`; manual: `chekov capability graph` renders the four registered models.

### Slice 3 — exact sizing + `explain` (~640 LOC, still no network)

`gguf.rs` reading **local** files via `std::fs` (no seam change), the layer-count ladder (MTP subtraction → `full_attention_interval` → SWA split → MLA branch), the expert/shared split, and `capability explain` printing the math line by line. This replaces slice 2's reserve band with real KV numbers and turns most `??` cells into verdicts.

*Acceptance test:* `chekov capability explain ornith-1.5-35b-a3b --ctx 262144` reproduces §4.5 exactly — `kv_layers = 10`, `kv_bytes = 2,852,126,720`, `total = 41,359,912,192`; a test asserts a `deepseek2` header **without** `key_length_mla` triggers a loud warning naming "re-pull a newer conversion"; a test asserts a `u32`-truncation guard on every GGUF length field.

### Slice 4 — `chekov capability recommend` (~700 LOC, first networked path, behind `--refresh` only)

The `HttpClient` seam change (`get_range` + `head_commit`, both at once) with its three fakes updated; `catalog.rs` with the compiled-in seed and atomic snapshot; the tool-parser cascade as an agent-role hard gate; `recommend.rs` with printed rejection reasons; the staleness banner; rate-limit and cursor handling.

*Acceptance test:* against a canned `FakeHttp`, the cascade classifies the recorded `OBLITERATUS` 506-char template as autoparser-fallthrough and **refuses** it under `--role agent` with its reason printed, while classifying the unsloth 9,993-char template as `qwen3-coder-xml`; a test asserts `gguf.totalFileSize` is never read; a test asserts shard sums exclude `mmproj-*`, `imatrix*`, `MTP/`, `mtp-*`, `dspark-*`; a test asserts a snapshot whose `min_chekov_version` exceeds the binary raises `CatalogSchemaTooNew` rather than parsing.

### Slice 5 — `chekov capability bench --fixture` (~1,400 LOC)

`runner.rs` (ClaudeFacade round trip, `/health`+pid readiness, `/props` assertion, flag hygiene, `timings`), `store.rs`, `stats.rs`, `sweep.rs`, `probes.rs`, `grade.rs`, `fixture.rs`, `compare`. The `--metric tok-s` grid upgrades from predicted to measured.

*Acceptance test:* a test injects a canned OpenAI response through `ClaudeFacade::route` → `post_json` → `translate_response` and asserts the graded artifact is the **Anthropic** body; a test asserts `compare` refuses on a differing `engine.build_commit` naming that field; a test asserts a two-sample throughput point reports "insufficient depths to fit a curve" rather than extrapolating; a test asserts overlapping p10–p90 intervals with <5% median delta print `no significant difference`. **Release gate:** fixture-v1 does not ship until measured against three models of clearly different capability with the spread published in the slice notes.

### Slice 6 — `--codebase`, `--svg`, `--judge` (~830 LOC)

Worktree isolation and the `--allow-exec` gate, the leakage filter with printed exclusion counts, HEAD-seeded sampling, `/infill` delegation with `N/A` capability recording, the SVG renderer, and the position-swapped grammar-forced binary judge with its consistency floor and printed tie fallback. Deliberately last: largest, most likely to need iteration, and every earlier slice is useful without it.

*Acceptance test:* a test asserts the leakage filter drops a file containing the masked identifier, cuts a `#[cfg(test)]` block out of a kept file, and drops the preceding doc-comment, and that the exclusion counts appear in the task record; a test asserts an `/infill`-unsupported model scores `N/A` and the printed composite states the renormalization; a test asserts a judge sharing `general.architecture` with a candidate raises `JudgeFamilyConflict`; a test asserts swap disagreement records a tie and sub-70% run consistency voids the axis.

---

## 13. Open questions — only the human can answer these

1. **Does the IDEAS.md entry get approved at all?** Charter N13 (`AGENTS.md:45`) is unambiguous and `IDEAS.md`'s own header says "Nothing here is implemented until it is approved". Filing the entry is the first action; nothing below it is written before approval.

2. **Is loosening the `run` wired-limit floor acceptable?** Slice 1 changes a live refusal gate from 196608 to 228065 MiB. That is correct — the engine's number is what llama.cpp actually fits under — but it makes `chekov run` start models it previously refused. Confirm, and confirm whether `[limits] wired_limit_mb = 187000` should stay as a floor at all now that the real budget is probed, or become a *reserve* (budget minus N) instead.

3. **`wired_limit_mb` units.** The config comment, `IDEAS.md`'s "Model-fit sizing" entry, and `hub.rs`'s `render_quant_table` header all treat the key inconsistently between MB and MiB (`hub.rs:232` prints "wired limit {mb} MB" while the arithmetic at `hub.rs:259` multiplies by `1024*1024`). The sysctl is genuinely MiB. Should the key be renamed `wired_limit_mib` with a back-compat alias, or is the existing name kept and only the comments corrected?

4. **Who maintains the layer-2 catalog?** Layer 2 is what actually fixes staleness, but it needs someone to publish a refreshed `catalog.toml` to GitHub releases. If nobody will, say so now and I will cut layer 2 and make the staleness banner on layer 1 correspondingly louder — shipping a layer-2 mechanism nobody feeds is exactly ramalama's failure mode with extra code.

5. **Is `--reasoning-format none` on all four `models.toml` entries deliberate?** Per `common/arg.cpp` it means "leaves thoughts unparsed in `message.content`" — the setting that *causes* `<think>` leakage, not the one that suppresses it. If it is deliberate for the MiniMax-M2 lineage (which wants thinking preserved verbatim across history), the reason belongs in a comment and the `think_leak` probe should treat leakage as expected for those entries. If it was chosen to hide thinking, three entries need fixing and that is a separate change.

6. **Correct the stored memory note now or as part of slice 2?** `llama-server-unified-kv-slots.md` records that auto `-np` splits `--ctx-size` across 4 slots. Upstream now resolves auto as `n_parallel = 4` **and** `kv_unified = true` together, and the unified branch takes `n_ctx_seq = n_ctx` — the inverse. The `-np 1` default stays correct; the comment at `registry.rs:48-52` justifying it does not.

7. **tree-sitter: yes or no?** Five crates, exact pins, `cargo deny` clearance. It upgrades codebase-mode masks from brace-balance to AST-node boundaries and makes identifier extraction exact. The design ships and works without it, with the weaker method labelled in every report. This is the only dependency worth asking for.

8. **Should `bench` ever be allowed to write back into `models.toml`?** `long_ctx_trace` produces a defensible recommended `ctx_size` and the throughput fit produces `a`/`b` coefficients. Writing them into the registry closes a real loop; it also means a bench run mutates config. Default in this spec is **print, never write**. Confirm or reverse.

9. **Is the ~85 ms scan cost acceptable on `run`/`doctor`, or scan-only?** Rewiring `preflight` onto `machine::gpu_budget()` adds the `--list-devices` probe (~53 ms) to every `chekov run`. Options: pay it, cache `machine.toml` with an engine-commit invalidation, or keep `run` on the cheap sysctl path and accept that it disagrees with `capability` when the engine is present. This spec pays it; the cached variant is a two-line change if 53 ms is judged too expensive.

10. **Fixture licensing and publication.** fixture-v1 ships inside a public repo, which means it becomes training data. The hidden-test design mitigates this and the discrimination detector reports the failure, but the honest long-run answer is that codebase mode is the durable signal. Accept that trade, or keep the fixture in a private sibling repo fetched on demand (which reintroduces a network dependency the rest of the design avoids)?