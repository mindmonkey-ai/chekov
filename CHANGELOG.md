# Changelog

All notable changes to chekov are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[SemVer](https://semver.org/).

## [Unreleased]

### Changed
- A request the server ANSWERED with a non-2xx is `UpstreamRefused` — "the
  server at <url> answered HTTP <status> instead of a result (<the server's
  own words>) — it is up and reachable; the request is what
  to fix" — instead of `EndpointDown`'s "not answering … restart", which sent
  a diagnosis the wrong way. `EndpointDown` keeps its meaning: connect, send,
  and read failures, readiness timeouts. The bench's forced-pass latch fires
  only on a real refusal now, never on a dead socket.

### Added
- `chekov stop --if-running` — opt-in: with no server running it prints
  "nothing to stop" and exits 0, so a teardown script can call it
  idempotently. Without the flag, stopping a stopped server is the loud
  failure it always was; a stale pidfile is still cleaned and reported, and
  a stop that actually fails still fails, flag or not.
- `doctor` gains a sixth check, **context loaded (server /props)**: the
  per-slot `n_ctx` the server actually loaded must equal the effective
  `ctx_size` — the same assertion the bench makes before recording a run, so
  the two can never disagree. The fifth row only ever compared `models.toml`
  to `config.toml` and could PASS with the server down; this is the row that
  speaks for the server. A mismatch fails naming both numbers; an unreachable
  `/props` fails like the other server checks — never SKIP, never PASS.
- The bench's grammar-forced pass asks the engine to extract reasoning
  (`reasoning_format: deepseek`) on that wire only, so `grammar_gap` is
  measurable on thinking-prefill templates (ornith, the Qwen/Hermes family)
  that llama.cpp's specialized chat handler otherwise refuses with `Failed to
  initialize samplers` — validated live. The unconstrained and streamed wires
  are byte-identical to before. The run head records the mode and the
  `grammar_gap` line prints `forced pass ran with reasoning extracted
  (deepseek)`: the one extra difference from the unconstrained arm is named,
  never hidden. Runs recorded before the field load unchanged.
- `setup` and `update --engine` verify the binary they just built: a fourth
  engine step runs `llama-server --version`, so a llama.cpp change that
  breaks the build's output fails right there as `EngineStepFailed` naming
  the step — before the commit is recorded as built — instead of surfacing
  later as a failed `run`. Printed under `--dry-run` like every other step.
  No auto-rollback: `logs/chekov.engine` names the commit to go back to.
- `[engine] git_ref` in `config.toml` — pin the llama.cpp engine to a branch,
  tag, or commit. Weights were already revision-pinned; the binary that runs
  them was whatever upstream HEAD was on the day of `setup` / `update
  --engine`. Absent, nothing changes. Pinned, `setup` and `update --engine`
  run `git fetch origin <ref>` and `git checkout --detach FETCH_HEAD` instead
  of a fast-forward pull (no `--branch` on the clone, so a sha works), print
  the steps under `--dry-run` like every other step, and `update --engine`
  reports `engine: <old> → <new> (pinned to <ref>)`. A ref git would read as
  an option (`-…`) or that would split into several arguments is refused at
  config load, naming the key.
- Bench probes cross the STREAMING translator too — the door Claude Code
  actually takes. Every unconstrained agentic case (`tool_emit`,
  `instruction`) now runs through both doors: buffered, and streamed with
  `stream: true` on the Anthropic request, the SSE body pumped through a fresh
  `stream_translator()` exactly as the proxy's relay does, and the agent-side
  events reassembled into the message an SDK client holds at `message_stop`,
  so the same graders read it. Rows record their `transport` (older rows load
  as `buffered`; schema unchanged), `--resume` keys on the door too, and the
  report adds `streamed …` lines beside the unchanged buffered ones plus the
  finding this exists for: `asymmetry <suite> <case>: buffered PASS, streamed
  FAIL — <reason>` for every case that answered differently through the two
  doors, or `asymmetry none` when they all agree. An upstream `error` frame
  mid-stream fails the crossing as `BenchStreamFailed` — recorded
  unavailable, never graded, never a forged `end_turn`. The throughput sweep
  and the grammar-forced pass stay buffered by design; the socket itself
  (HTTP framing) is still not exercised, as spec §7.1 states.
- `chekov capability graph --metric tok-s` — the grid's first character
  becomes a band digit 1–9 of the MEASURED decode median from stored bench
  runs (`eval/`), and every cell without a run stays `??`: predicted and
  measured are never blended in one column. Band edges are fixed and printed
  in the legend (5/10/15/20/30/40/60/80 tok/s), not deciles of the peer set,
  so a digit cannot move because a different model was benched. A run applies
  to a cell only on an exact match of model, quant, configured ctx AND machine
  id — never another machine's row; among several matches the latest is shown
  and the choice is a numbered footnote. The headline per run is the median at
  the deepest depth that can be summarised, and the legend says so. A measured
  cell from a build other than the installed engine is shown AND named:
  `measured cells are from build <old>; the engine is now at <new>. Re-run
  'chekov capability bench' to revalidate.` An unreadable run directory is a
  footnote, not a crash and not a silent skip. The SVG carries the same digit,
  the same footer, and the median with p10–p90, depth and run id in each
  cell's tooltip. Live on this machine: qwen3.8-27b `5#` at 128K (20.5
  tok/s), ornith-1.5-35b-a3b `8#` at 256K (68 tok/s), `??` elsewhere.
- `chekov capability graph --svg [PATH]` — a self-contained SVG of the same
  frontier the terminal prints, the first piece of slice 6. Hand-emitted: no
  dependency, no CDN, no script, no external reference, so it opens offline
  and travels in a bug report. Bare, it lands in `reports/`; the path is
  **printed, never opened**. The legend is generated by the same function the
  ASCII renderer calls, so the two views cannot disagree; a predicted ceiling
  says `CEILING PREDICTED` here too; measured-vs-predicted inputs are carried
  by a 45° hatch as well as fill (colour alone fails greyscale and
  colour-blind viewers), fills are luminance-ordered, and every cell prints
  its glyph, so all three states survive with no colour at all. Each cell
  carries a tooltip with its own arithmetic — `weights + kv + overhead = total
  vs budget` — and each part's provenance. The spec's throughput dots are
  deliberately absent: no measured speed reaches the frontier model yet, and
  drawing points from unmeasured data is the failure this subsystem exists to
  prevent.
- `launch`: local-directory marketplace plugins (`extraKnownMarketplaces`
  with `source = "directory"`) are mirrored into the session config dir so
  `enabledPlugins` resolves; `extraKnownMarketplaces` is now a carried key.

### Fixed
- **The non-streaming translator keeps extracted reasoning, as the streaming
  one always has.** With `--reasoning-format auto|deepseek` — or the bench's
  forced pass asking for it per request — the engine puts the reasoning in
  `message.reasoning_content`; the streaming path turned it into a `thinking`
  block and the buffered path silently dropped it. It is now the same
  `thinking` block, ahead of the text and `tool_use` blocks, so a client sees
  the same content by either transport. Empty or absent adds nothing.
- **The non-streaming translator now strips the thinking span, as the
  streaming one always has.** `to_anthropic_response` passed `<think>…</think>`
  through verbatim while `ClaudeStream` dropped it, so the two halves of the
  same translator disagreed about what the agent receives; any non-streaming
  Anthropic client saw reasoning the streaming one never shows it. Claude Code
  streams, so it was unaffected — this was a latent asymmetry, found by the
  bench's own first agentic run, where instruction adherence scored strict 1/12
  and every failure was `fenced_rust_only` against a reply beginning `<think>`.
  With the fix the same model scores **strict 11/12** on the same cases: the
  suite had been measuring the leak, not the model. A span that never closes
  yields no text rather than raw reasoning — the model spent its budget
  thinking and never produced an answer, and reporting the reasoning as the
  answer would be inventing one. This does not contradict
  `--reasoning-format none` or doctor's think-tag retention check: retention is
  required at the `OpenAI` door, segregation happens at the Anthropic one.
- **An unmeasurable bench axis reports `N/A`, never a zero.** The first agentic
  run published `grammar_gap 0/7 forced — unconstrained 6/7 (gap -85%)` when
  all seven were engine refusals (HTTP 400) and the model was never asked.
  Grade rows gain an `unavailable` state: such a task is never listed as a
  failure, an axis whose every task is unavailable renders as `N/A` with the
  engine's own reason, and the run stops after the first refusal instead of
  firing six more doomed requests.
- `hub::post_json` keeps the server's explanation on a non-2xx instead of
  discarding the body — the proxy's own upstream call already did this, so the
  bench was losing `Failed to initialize samplers` behind a bare
  `http status: 400`.
- An agentic-only run no longer prints `insufficient depths to fit a curve`,
  which claimed a failed fit for a suite that never ran.

### Added
- `chekov capability bench --suite agentic` — the corpus-free §7.2 probe
  suites, seed set v0. `tool_emit` (7 call + 2 abstention + 1
  missing-function cases) crosses the translator's real tool mapping and
  grades the translated `tool_use` block BFCL-style (name + arguments as
  parsed JSON); `grammar_gap` re-runs the call cases with the case's own
  `oneOf` schema forced via `response_format` on the wire, and the summary
  prints forced-vs-unconstrained ON THE SAME CASES — the anti-self-deception
  device: a large gap means "works only with a babysitter"; `instruction`
  (12 IFEval-style cases) reports strict and loose separately with the
  chattiness gap. Failures are listed individually, passes counted, and the
  probe-set TOML's hash rides in `prompt_set_hash`, so an edited case makes
  old runs incomparable by construction. `--suite` defaults to `throughput`
  (a stated deviation from the spec's `agentic` default, held until the set
  reaches the spec's 30/40 counts). Deferred with their reasons recorded:
  `diff_fidelity`/`tool_loop`/`long_ctx_trace`/`hallucination` (need the
  §8/§9 corpora), `think_leak` (waits on the `--reasoning-format none`
  question).
- `chekov capability bench --models a,b [--dry-run] [--yes]` — the §7.3
  per-candidate lifecycle. Bench launches each candidate itself behind `run`'s
  own preflight gates, checks the argv against the binary's own `--help`
  before spawning (upstream removes flags behind a handler that terminates
  startup), carries `GGML_METAL_RESIDENCY_KEEP_ALIVE_S=5` into the child so a
  sequential sweep does not OOM on the second model, and tears down with a
  verification that the GPU budget actually came back before the next load.
  The server-use rule is explicit: the one running server is reused when it
  IS the single request; otherwise a live server is a refusal — bench never
  stops a server it did not start. Any launch step requires confirmation;
  `--dry-run` prints the plan and a rough wall-clock estimate as data.
- Bench rows record `cache_n` (prompt tokens served from KV cache) — observed
  live: a warm rerun's depth-1024 `prompt_n` fell 1055 → 516 because the
  shared prefix was cached, and without `cache_n` that read as a shallower
  measurement.
- `chekov capability bench [--fixture <path>] [--resume <run-id>]` — slice 5's
  harness. Measures the running server THROUGH chekov's own Anthropic↔OpenAI
  translator: `/health`+pid readiness (a server that dies while loading fails
  as "died", not as a timeout), a `/props` assertion that the loaded per-slot
  `n_ctx` matches the config's intent, and a depth sweep with sampling pinned
  on the wire (`temperature 0, top_k 1`, seeded). Every run lands in
  `eval/<run_id>/` as `stamp.json` — a 17-field configuration stamp plus the
  exact launch argv — and `results.jsonl`, one flushed append per task, so a
  crash loses at most one task and `--resume` skips what a run already holds
  (resuming under a changed stamp is refused). Raw samples are stored;
  summaries are recomputed on read, so a stored median can never drift.
  Optional graded probes come from a user-supplied TOML fixture; there is
  deliberately no compiled-in fixture — fixture-v1 is release-gated on a
  three-model measurement campaign.
- `chekov capability compare <run-a> <run-b>` — refuses on the FIRST differing
  stamp field, because llama.cpp does not guarantee bit-identical results
  across configurations (GPU reduction kernels pick different accumulation
  orders; float addition is not associative). The subject fields
  (`weights_revision`, `quant`) are exempt — they are what is being compared —
  and differing task sets (`prompt_set_hash`, `corpus_id`) always refuse.
  `no significant difference` is a first-class printed outcome, never resolved
  into a winner.
- `[bench]` config section: sweep depths, repetitions, probe `max_tokens`,
  the significance threshold, the readiness poll budget, and the sampling
  seed.
- Hand-rolled SHA-256 (`core::hash`, NIST-vector-verified) for the machine
  identity and prompt-set hashes — house style, no new dependency.
- `core::stats` — the statistical honesty slice 5's bench rests on. Median with
  p10/p90, never mean ± stddev, because decode rate is right-skewed by thermal
  events and a mean flatters a run that hit one stall. The first repetition is
  dropped as warmup and the drop is recorded rather than absorbed. Two
  configurations are called indistinguishable — and printed as such rather than
  resolved into a winner — when their p10-p90 intervals overlap OR the medians
  differ by less than the significance threshold. Fewer than three distinct
  depths refuses to fit a curve instead of extrapolating from a line.
- `chekov capability recommend --refresh` — queries the Hugging Face list
  endpoint for candidates, classifies each repo's tool parser from the chat
  template it returns, and sizes it through the same `quant_options` path
  `pull` uses, so a repo withholding a shard's size yields no number rather
  than a partial sum. **The only networked path**: without `--refresh` chekov
  ranks registered models and never reaches out, because a recommendation that
  changed due to a background fetch is not reproducible. Download order is used
  only to bound which repos are worth a size lookup, never to rank them.
- `pull` excludes calibration and draft artifacts from quant sizing:
  `imatrix*`, `MTP/`, `mtp-*` and `dspark-*` join `mmproj-*`. Each ships as a
  `.gguf` beside real quants and would otherwise inflate a quant's size or be
  offered as one.
- `chekov capability recommend [--ctx N] [--role agent|chat]` — ranks the
  registered models for this machine. Rejected candidates are printed with
  their reason, never silently dropped. A model whose template has no dedicated
  llama.cpp tool parser is **downranked with a note under `--role agent`, not
  refused** — the refusal the spec called for would have rejected
  `minimax-m2.7`, which falls through and works. Under `--role chat` the tool
  parser is ignored entirely and the largest fitting model wins.
- `core::toolparser` — replays llama.cpp's `common_chat_try_specialized_template`
  substring cascade so chekov can tell which tool-call parser a chat template
  resolves to, or that it falls through to the generic autoparser. Verified
  against live templates: `OBLITERATUS/Qwen3.8-27B-OBLITERATED` (509k downloads,
  506 chars, zero tool markup) falls through, while
  `unsloth/Qwen3.8-27B-GGUF` (9993 chars) resolves to Qwen3-Coder — so ranking
  candidates by downloads recommends the one that cannot call a tool.
- `chekov capability explain [name] [--ctx N]` — slice 3 of the capability
  spec. Reads the model's real GGUF header from local disk and prints the fit
  arithmetic line by line. `capability graph` now uses those numbers, so cells
  backed by a readable header report `measured` inputs instead of the coarse
  reserve.
  The layer ladder is the point: `kv_layers` is **not** `block_count`. An MTP
  block is subtracted, then a hybrid model caches one layer in every
  `full_attention_interval` — 41 blocks becomes 10 cached layers on
  ornith-1.5-35b-a3b, and using `block_count` would over-estimate KV by 4x and
  refuse configurations that fit. `q8_0` is 17/16 bytes per element, not 1/2.
- `chekov capability graph [--ctx N]...` — slice 2 of the capability spec. A
  grid of registered models against context lengths. Each cell is two
  characters (fit verdict, then input provenance) because one glyph cannot
  carry two orthogonal facts. A cell with any unknown component renders `?`,
  never a fit; when the GPU ceiling itself is predicted the header says
  `CEILING PREDICTED` and the legend reads "fits against a predicted ceiling"
  rather than promising a plain fit. KV and overhead are a labelled reserve
  until the GGUF header reader lands in slice 3.
- `chekov capability [--json]` — slice 1 of the capability spec. Reports chip,
  model, memory, GPU cores, performance threads, macOS, and the GPU budget with
  its provenance. On the author's M3 Ultra it prints
  `228065 MiB (engine-reported) — 31457 MiB more than the 196608 MiB formula
  would predict`.

### Changed
- **The wired-limit gate is loosened**, deliberately. `run`'s refusal and
  `status`'s report now resolve the GPU budget through the same
  `machine::live_gpu_budget` ladder the new scan prints: the engine's own
  `--list-devices` figure first, then an explicit `iogpu.wired_limit_mb`, and
  only then the 75%-of-RAM formula. The formula is measurably 31457 MiB low on
  a 256 GiB M3 Ultra — verified against
  `MTL0: Apple M3 Ultra (228065 MiB, 228064 MiB free)` — so this makes `run`
  accept models it previously refused between those two figures. It is a
  correctness fix that happens to loosen a gate, not a relaxation for
  convenience; when the engine is not built the ladder falls back to the old
  formula and behaviour is unchanged.

### Added
- `setup` and `update --engine` record the llama.cpp commit they built to
  `logs/chekov.engine`, and `chekov status` shows it — an unrecorded engine
  says so and names the command that records one, rather than being guessed.
- `update --engine` now says when the running server is still the previous
  engine, mirroring what `use` already does for models.

### Fixed
- `chekov show` no longer prints the server API key. The invocation line
  carried `--api-key <key>` verbatim, and that output is exactly what people
  paste into bug reports. The value is withheld positionally, so a stray
  `--api-key` in `extra_flags` is covered too; `launch_args` — the thing that
  actually executes — is untouched.
- `doctor`'s context-floor row is renamed "context floor (config, not the
  server)". It compares `models.toml` to `config.toml` and nothing else, so it
  is the one row that can report PASS while the server is down — beside four
  FAILs, an unqualified PASS reads as evidence the server is healthy, which
  this check cannot know.
- `pull` no longer offers a vision projector as a quant. `mmproj-F16.gguf`
  matched the tag heuristic, so `unsloth/GLM-5.3-Flash-GGUF` listed an "F16" of
  1.1 GiB beside real quants of 86-186 GiB — sorted to the top of the table as
  the cheapest-looking option — and pulling it registered a projector as a
  runnable model with `first_shard = mmproj-F16.gguf`. It was also summed into
  the genuine `BF16` total (Qwen3.8-27B read 51.8 GiB against real weights of
  50.9). Found by running the tool against live repos, not by reading it.
- README's `[defaults]` block omitted `-np 1`, so anyone following the docs to
  hand-edit `models.toml` silently reintroduced the shared-KV-slot context
  split. Two new tests parse README's own TOML fences and compare them against
  `Defaults::default()` and `FileConfig::default()`, so this class of drift now
  fails the build instead of shipping.
- README's KV-cache rule of thumb used the model's layer count and treated q8_0
  as one byte per element. Modern MoE and sliding-window models cache only a
  fraction of their blocks (a 4-5× overestimate), and q8_0 is 34 bytes per 32
  elements. Both corrected, with a pointer to the measured value in the server
  log.
- `docs/HOWTOS.md` claimed a 4-bit quant costs 0.5 GB per billion parameters,
  contradicting its own table on the next line (which implies ~0.6 GB/B).
  The table was right: llama.cpp promotes `output.weight` and the embeddings,
  so a "4-bit" Q4_K_M realises about 4.8 bits per weight. Also states that the
  table is weights-only, matching what `render_quant_table` already says.
- Confirmation prompts no longer report a phantom decline when there is no
  terminal. `confirm` read EOF from a non-tty stdin and reported the user as
  having declined, with a remediation ("re-run and answer 'y'") that cannot be
  followed in cron or launchd. It now checks for a tty first and fails with
  `ConfirmationRequiresTerminal`, which states the actual situation.
- proxy: a non-2xx from llama-server no longer collapses to `http status: 400`
  with the body discarded. ureq's `http_status_as_error` is turned off so the
  status is taken directly and the server's own `error.message` — the only
  thing that says *why*, e.g. a context overflow — reaches the user, bounded so
  a runaway body cannot become a 20 KB log line.
- proxy: inbound headers are bounded. The body had a 64 MB ceiling but the
  header loop had no count cap and no per-line cap, so a local peer could hold
  a proxy thread open indefinitely or grow a single line without limit. Now
  100 headers and 16 KB per line, and an over-long line reports the ceiling
  instead of the misleading "client closed the connection".
- `update --model`: the "old revision kept on disk" notice told the user to run
  `chekov rm <name>`, but `repoint` has already moved the registry entry to the
  NEW revision — following that advice deleted the weights just downloaded and
  orphaned the old directory permanently (nothing enumerates the models dir, so
  an unreferenced revision is invisible to `list`). The notice now names the
  stale directory by path and warns against `rm` explicitly.
- `restart` with no argument targets the active model, which may not be the one
  running. It now says so before stopping — resolving run-state before the stop
  clears it — and names `chekov restart <running>` for keeping the loaded model.
  Unloading and reloading 100+ GB is never silent.
- `integrate hermes`: a `model:` header carrying a trailing comment
  (`model:  # active`) was not recognised, so the merge prepended a SECOND
  top-level `model:` key and left the old block intact — ambiguous YAML in a
  live Hermes config. The block header is now matched on its key, not the
  whole line.
- `integrate hermes`: a `providers:` block indented with anything other than
  two spaces is now refused with a named remediation instead of silently
  gaining a mis-nested duplicate `chekov:` entry beside the stale one. The
  module is contractually forbidden from clobbering this file, so an
  un-editable shape is a loud refusal, not a guess.
- `launch`: `settings.json` and `.claude.json` carry the server API key and are
  now created 0600 inside a 0700 session dir (created with the mode, not
  chmod'd afterwards, so there is no world-readable window).
- The llama.cpp build no longer passes a bare `cmake -j`, which reached make
  with no job count — unbounded compile jobs with the jobserver disabled, on a
  machine that may be holding a 158 GiB model resident. Now one job per
  logical core.
- CI, release and the Makefile pass `--locked` on every cargo invocation that
  resolves dependencies, so a `Cargo.toml`/`Cargo.lock` drift fails the build
  instead of silently re-resolving inside the runner.
- `CARGO_REGISTRY_TOKEN` is scoped to the publish step instead of the whole
  `crates-io` job, where it was also live during `actions/checkout` and
  `dtolnay/rust-toolchain@master`.
- Documentation: `README.md` no longer claims "No async runtime" without
  qualification, and the `hf-hub` dependency comment no longer states that
  `blocking` avoids tokio — upstream defines that feature as `["tokio/rt"]`.
- `plugins`: a `plugin.json` `name` is no longer trusted as a path component.
  It was joined onto the marketplace cache dir and that path was then removed
  and recreated, so `"name": "../.."` reached outside the cache and an
  absolute name replaced it entirely. Unsafe names now fall back, loudly.
- `plugins`: `installed_plugins.json` and `known_marketplaces.json` are written
  atomically (process-unique temp + rename, mirroring `Registry::save`) rather
  than truncate-in-place — these are another tool's live state files.
- release: `shell/chekov.zsh` ships under `shell/` in the tarball instead of at
  the root. It derives `CHEKOV_HOME` from its own depth, so the flattened
  layout resolved one level too high and every tarball install had a wrong
  `CHEKOV_HOME`, a wrong `PATH`, and unreachable completions.
- `launch` now runs the same four refusal gates as `run` before starting a
  server, instead of spawning blind and surfacing an opaque connection error.
- `launch` refuses to adopt a running server that is serving a different model
  (`ServerModelMismatch`) or one whose identity is unrecorded
  (`ServerModelUnknown`) — it no longer advertises a model and context window
  the upstream is not actually serving.
- The proxy translates an upstream `error` frame into an Anthropic `error`
  event and suppresses the terminal `end_turn`, so a failed turn is no longer
  reported to the agent as a clean, complete one.
- A mid-stream upstream failure now emits a terminating envelope instead of
  dropping the socket, which the SDK reported as a protocol error.

### Changed
- Downloads no longer go through `hf-hub`. It was reached from exactly one call
  site and pulled 190 of the tree's 256 crates, including `tokio` — which the
  README said chekov does not use — plus the `xet` stack, and a yank anywhere
  in it could break `cargo deny` without a line of chekov changing (it did).
  Replaced with a streaming `ureq` GET against the same revision-pinned
  `resolve/` URL hf-hub was calling. **256 crates -> 66, and tokio is gone.**
  Measured first: the repos are Xet-backed, but the Xet bridge serves plain
  HTTPS, and a single stream already saturates the link at ~54 MB/s while
  parallel streams do not beat it — so Xet's chunked parallelism had nothing to
  win here. Downloads now land via a `.part` file and a rename.
- `deny.toml` evaluates the graph for the Apple targets chekov actually ships
  for, and its seven unversioned duplicate-skips are gone. Each of those
  disarmed duplicate detection for a crate at *every* version, forever; with
  the hf-hub tree removed the graph unifies on its own.
- `[limits] wired_limit_mb` default is 187000 (was 200000). The old value
  exceeded macOS's own 75%-of-RAM default on every shipping Apple Silicon Mac,
  so a fresh install refused to `run` until the user hand-wrote a config file.
  README and `config.example.toml` now agree with the code.
- A wired-limit requirement above physical RAM reports `WiredLimitUnreachable`,
  naming `config.toml`, instead of printing a `sudo sysctl` command the machine
  can never satisfy.
- Default root is `~/.chekov` (was `~/personal_dev/chekov`); `CHEKOV_HOME`
  still overrides, and `shell/chekov.zsh` exports it for source installs.
- README: "Finding a model" / "Adding a model" guides; machine-specific
  paths removed.

## [0.1.0] — 2026-08-21

First release.

### Added
- Model lifecycle: `pull`, `list`, `use`, `rm`, `show` against a TOML registry
  with revision-pinned Hugging Face downloads, size-verified adoption of
  existing weights (`--model-loc`), and license snapshot + provenance per pull.
- Server lifecycle: `run` (background by default), `stop`, `restart`, `status`
  with pidfile handling and wired-memory verification before start.
- `doctor`: five offline-verifiable health checks (OpenAI door, Anthropic door,
  think-tag retention, NaN canary, context floor).
- `setup` / `update --engine|--model|--all`: llama.cpp clone + Metal build,
  gated model re-pointing with a license-diff stop.
- Integrations: `integrate hermes` (surgical config merge), `integrate claude`
  (`cclocal` launcher), `env`, and `launch claude` with an in-process
  Anthropic→OpenAI protocol translator and a generated `CLAUDE_CONFIG_DIR`.
- zsh completions and a `chekov.zsh` shell shim.
