# Changelog

All notable changes to chekov are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[SemVer](https://semver.org/).

## [Unreleased]

### Added
- `capability bench NAME --runtime <name>@<version> [--upstream <url>]` — an
  OpenAI-compatible server you already started (MTPLX, MLX, anything on the
  same wire) becomes a first-class bench subject. `--runtime` is
  `UseRunning`-only for the subject: chekov never launches, installs, or
  tears down a foreign server, and refuses before any measurement
  (`RuntimeNeedsRunningServer`) if the named model isn't already serving —
  `--judge` still launches chekov's own local llama.cpp judge exactly as
  today, though `--runtime` together with `--judge` is refused by the
  existing memory-budget gate (a foreign server chekov did not launch never
  comes down, so the judge has nowhere to load beside it). Readiness is one
  plain `GET /v1/models`; the served ids are PRINTED, never asserted
  (`chekov: runtime mtplx 0.4.1 serves: <id>...`), because chekov cannot know
  how a foreign server names its own weights. `Stamp` gains a `runtime` field
  (`#[serde(default)]` to `llama.cpp`, so every stored run reads exactly as
  before) immediately ahead of `engine_build_commit`, which for a foreign run
  holds the declared version verbatim rather than a probed commit; launch
  flags chekov cannot observe are stamped with fixed sentinels (`ctx`/
  `n_parallel` `0`; `kv_unified`/`n_batch`/`n_ubatch`/`type_k`/`type_v`/
  `flash_attn` `"unmanaged"` — a third spelling, never `"engine-default"`)
  instead of invented. The `BenchStampMismatch` message is now engine-neutral
  (`bench stamp mismatch on '{field}' ({a} vs {b}) — results are comparable
  only inside one pinned configuration (runtime, build, flags and sampling
  all held constant); re-bench under a matching stamp and compare those
  runs`); the llama.cpp float-associativity rationale moved to `Stamp`'s own
  doc comment. Codebase mode's FIM crossing rides `/infill` for llama.cpp and
  a deterministic chat-completions instruction for a runtime with none
  (`prompt_set_hash` covers the chat template only when a run actually rode
  it), and the report names which: `fim transport: /infill` or
  `fim transport: chat`. `capability compare A B --cross-runtime` permits
  exactly `runtime`, `engine_build_commit`, the eight unmanaged fields and
  `prompt_set_hash` to differ, opens with a loud banner
  (`cross-runtime comparison: <a> vs <b>` … `this measures the runtimes, not
  the model.`) naming every differing allow-listed field, and still refuses
  on anything else — plain `compare` (no flag) refuses a cross-runtime pair
  on `runtime` like any other first-differing field. Foreign throughput and
  codebase chat-FIM crossings are now timed by chekov's own wall clock over
  the streamed response instead of requiring llama.cpp's `timings` object: a
  new `HttpClient::post_json_stream_timed` (default implementation refuses
  with `ForeignTimingsUnsupported`; `UreqClient` overrides it) captures two
  windows — request written → first SSE data frame, first data frame →
  stream end — and a new `timings_from_stream` derives
  `prompt_per_second`/`predicted_per_second` from the OpenAI `usage`
  object's token counts (decode divides by n−1 tokens, since the first
  token lands at the first-data mark; `cache_n` is recorded `0`, unknowable
  through a foreign server). This is honest client-side timing — it includes
  wire and translator overhead (negligible on localhost) and the first-data
  mark only approximates end-of-prefill because these servers stream tokens
  as they are generated — and it fails loudly per probe
  (`ForeignTimingsUnsupported { runtime, reason }`) when a reply gives it
  nothing to time: no `usage` frame, fewer than 2 completion tokens, or a
  zero-length window. `Stamp` gains `timing_source` (`#[serde(default)]` to
  `server-reported`, so every stored run reads unaffected; a foreign run
  stamps `chekov-streamed`), and the report prints `timing source:
  chekov-streamed (client wall-clock over SSE; includes wire overhead)` only
  when it isn't `server-reported`. `capability compare --cross-runtime` now
  also permits `timing_source` to differ (the allow-list grows to 12).
  Agentic and fixture suites are not yet on the timed path — their
  foreign-run row failures name the runtime and the exact reason instead of
  `BenchNoTimings`'s engine-rebuild advice, but extending them onto the same
  streamed mechanism is a recorded follow-up. Live verification against a
  real MLX/MTPLX server is approval-gated and has not run yet; the plumbing
  is unit-tested against fakes on the existing `HttpClient` seam.
- `capability bench NAME --runtime ... --served-model <id>` names which of a
  foreign server's served ids is the bench subject on the request wire's
  OpenAI `model` field — chekov's own registry name never reaches it on a
  foreign run. Absent the flag, a single served id is used automatically; a
  server listing zero or several ids without one refuses loudly
  (`RuntimeServedModelRequired { count }`) rather than guessing. Fixes a live
  finding: llama.cpp ignores the `model` field, but mlx-lm routes on it and
  404s trying to download chekov's registry name from Hugging Face. The
  registry name still names the run directory, the stamp's weights identity,
  and the report header.

### Fixed
- **A dead bench/tune candidate no longer reads as alive for the whole
  readiness budget, and a cooperative teardown no longer burns the full
  grace period on a corpse.** `spawn_daemon_with_env` never reaped its
  child (correct for `chekov run`, where chekov exits and launchd adopts
  it), so a candidate that died while loading became a zombie — and the
  signal-0 liveness probe reports a zombie as alive forever. bench and tune
  stay resident, so a candidate that died at load timed out after the full
  600-poll readiness budget instead of reporting "died" in seconds, and
  every teardown's SIGTERM looked "ignored" (the server was already dead,
  but its zombie stayed visible), escalating to a SIGKILL that hit nothing.
  `server::child_alive` reaps via `waitpid(WNOHANG)` before answering,
  falling back to the signal-0 probe for a pid this process did not spawn
  (e.g. a pidfile written by another chekov invocation), and now backs
  `bench::runner::wait_ready`'s pid-watch and `server::stop_pid`'s
  grace-period poll.
- `tune`'s `fa` stage no longer launches a doomed `fa off` candidate under
  a quantized KV incumbent (most commonly the `kv` stage's own `q8_0`
  winner). `fa off` requires the incumbent's `-ctv`/`--cache-type-v` to be
  unquantized, and llama.cpp exits at load naming exactly that when it
  isn't ("quantized V cache requires flash_attn to be enabled"). The trial
  is now skipped before any spawn, mirroring the existing kv-skip rule in
  the other direction and naming the incumbent's actual V-cache spelling
  in the reason.

### Changed
- `[limits] wired_limit_mb` no longer has a built-in value. The old default,
  187000 MB, was one 256 GB desk's number: on any Mac below ~250 GB a fresh
  install refused every model as "unreachable" before looking at it, `setup`
  ended incomplete demanding a sysctl the machine could never satisfy, and
  `status` said "need 187000 MB". Absent — the default — the model is the
  requirement: `run` sizes the model (weights on disk + KV cache at its
  effective context + the ~3 GiB of compute buffers every `graph` cell
  reserves) against the live GPU budget and refuses only a model that does
  not fit, with `ModelExceedsBudget` naming the levers that exist
  (a smaller quant, a lower `ctx_size`, `capability recommend`) and never a
  sysctl; a tight fit proceeds and says so; an unreadable header proceeds and
  says it did not check. `setup` completes on any readable budget and prints
  it with its provenance; `status` shows the budget and `(no floor
  configured; run checks the model's footprint)`. A configured floor keeps
  every previous behaviour exactly. A test now pins that no production path
  decides anything with this desk's numbers.
- The parts of that sizing the three commands share now live once, in
  `core::footprint`: the weights sum, the 3 GiB overhead constant, the total,
  and the q8 rule. Two consequences for `capability recommend`: its total
  now includes the same overhead `graph` always reserved (a model within
  3 GiB of the budget reads as not fitting in both, not just one), and it
  reads `q8_0` from the flags the model is actually launched with (defaults
  plus its own `extra_flags`) instead of the defaults alone — before, a
  model that set `q8_0` per-entry was sized at f16 by `recommend` and at q8
  by `graph` and `run`.
- `capability compare`'s codebase section shows a `cross_file_first` task's
  two arms apart — `cross_file_first` and `cross_file_first+extra`, paired by
  full task id as the report does — instead of one line blending both, and
  gains a `context lift` group: the two runs' per-task lifts (extra −
  no_extra) on tiers 1-5, compile and test, through the same paired sign
  test as every tier. A task both runs touched but one of them measured on
  one arm only is dropped from the lift and counted on its own drop line
  (`measured on one arm only in one run`); the existing drop line now names
  its reason (`unavailable in one run`) the same way.
- `Stamp` gains a 21st field, `judge: Option<JudgeStamp>` — absent on every
  run recorded before slice C, filled at plan time by `--judge` or adopted on
  `--resume --judge <name>` when the rest of the stamp still matches. `compare`
  is not affected by its presence or absence; `--resume` refuses a run whose
  stamp names a different judge.
- `capability bench --codebase` keeps a file that contains `#[cfg(test)]` and
  cuts the test items out of it, instead of excluding the whole file. Idiomatic
  Rust keeps its unit tests inline, so the old rule left 7 of chekov's own 63
  source files eligible — the benchmark was sampling the repository's
  leftovers. Each `#[cfg(test)]`-attributed item is now cut from its attribute
  line through the matching `}` or terminating `;`, literal-aware (a brace
  inside a string in the test module cannot end the cut early, and the
  attribute inside a string or a comment is not a cut point), before masking
  and before the repo symbol set is built. Nothing goes quiet: every row
  carries its file's `excluded.cfg_test_lines`, the report header adds
  `; tests elided: L lines in F files`, and the dry-run plan line adds
  `, tests elided in F files`. Any repository with inline tests now yields a
  different task set, so its set hash — and so its `corpus_id` — changes; the
  stamp records which one a run used.
- `capability graph`'s inputs legend says what the second character encodes
  — `#  kv measured   ·  kv predicted` — and ends with what it does not
  cover, derived from the cells: `overhead is a flat predicted 3.0 GiB in
  every cell`. Before, `#` read as "measured" for a sum with a guessed
  summand. The glyphs are unchanged; both renderers share the legend.
- A request the server ANSWERED with a non-2xx is `UpstreamRefused` — "the
  server at <url> answered HTTP <status> instead of a result (<the server's
  own words>) — it is up and reachable; the request is what
  to fix" — instead of `EndpointDown`'s "not answering … restart", which sent
  a diagnosis the wrong way. `EndpointDown` keeps its meaning: connect, send,
  and read failures, readiness timeouts. The bench's forced-pass latch fires
  only on a real refusal now, never on a dead socket.

### Added
- `chekov tune [NAME] [--dry-run] [--yes] [--apply] [--stages fa,kv,batch,ubatch]`
  measures, on this machine, whether any of a small set of launch flags beats
  what the model launches with today, and says **`defaults won`** when
  nothing does — the honest-verdict pattern `mtplx tune` uses. A four-stage
  descent starts from the model's own current flags as the baseline and, one
  stage at a time, rewrites `--flash-attn` (`fa`, judged on **decode**), then
  `--cache-type-k`/`--cache-type-v` together (`kv`, judged on **decode**; a
  `q8_0` candidate under an FA-off incumbent is skipped), then `--batch-size`
  (`batch`, judged on **prefill**), then `--ubatch-size` (`ubatch`, judged on
  **prefill**, candidates capped at the incumbent batch). A candidate wins its
  stage only when `stats::compare` at `[bench] significance_pct` says `Faster`
  on the stage's own metric **and** not `Slower` on the other; a degenerate
  trial (the server never came up, the probe returned no timings, too few
  samples survived warmup, or the measured prompt was truncated) is recorded
  with its reason and can never win or be saved, and a degenerate baseline
  stops the run as `TuneBaselineDegenerate` rather than comparing against
  nothing. `--dry-run` prints the stage plan, an upper bound on launches
  (`≤ N launches`, counted against the widest batch the descent can actually
  reach so the bound holds even when a stage's winner grows the next one) and
  a wall-clock estimate; a run without `--yes` confirms once against that
  same estimate. Every run writes `tune/<utc>-<model>.json` — every trial's
  argv, flag stamp, decode/prefill summaries, thermal readings (`pmset -g
  therm`'s `CPU_Speed_Limit`, honestly `None` where macOS reports nothing
  without root) and the `significance_pct` its verdicts were reached under —
  so the record and the printed report always agree on the threshold even
  when `[bench] significance_pct` is not 5, and the `defaults won` line names
  it. `--apply` prints the exact `extra_flags` diff for the winner's
  `models.toml` entry and, after a confirm (`--yes` covers it), writes it
  through `Registry::save`; a `defaults won` run has nothing to apply and
  refuses (`TuneNothingToApply`) rather than silently doing nothing.
  Configured under a new `[tune]` section (`depth`, `flash_attn`,
  `cache_types`, `batch_sizes`, `ubatch_sizes`); `repetitions`, `max_tokens`
  and `significance_pct` are shared with `[bench]` — one definition of "how
  many samples" and "what is significant" for every measurement chekov makes.
  The launch/ready/teardown lifecycle `bench` already used moves into
  `core::bench::candidate` (`launch`, `ensure_ready`, `teardown`, unchanged in
  behaviour) so `bench` and `tune` share one implementation and cannot drift.
- `capability bench` gains `--judge <NAME>`, a registered `role = "judge"`
  model (set by hand on its `models.toml` entry — `pull` never writes the
  field; any other value is refused at registry load, naming the one accepted
  value; `chekov list` marks the entry carrying it in a new `ROLE` column) of
  a different architecture family from every candidate. Resolution
  happens at plan time, before any launch: an unregistered name, a judge
  missing `role = "judge"`, a same-family judge and candidate, a judge named
  among `--models`, and a running server that bench did not start are each a
  refusal before the run is spent, and `--resume` of a run stamped with a
  different judge is refused too — a run is never spent to report a judge
  outcome as unavailable at the end. The judge runs as its own phase, launched
  once after every candidate has landed on disk and torn down, and shared
  across every run in the invocation. For each eligible `function_body`
  crossing — a stored row with a non-empty prediction, and, under
  `--allow-exec`, a tier-6 verdict other than a hard `0.0` — it asks one
  binary question twice, gold and prediction position-swapped, through
  chekov's own translator on the forced wire with a `json_schema`-grammar
  request; the reply's `content` is parsed into a `deny_unknown_fields`
  struct rather than trusted, because llama.cpp can leave the grammar
  inactive while a model is thinking. A reply that is not exactly the schema,
  or one truncated at `[bench] judge_max_tokens` (default 512), is a counted,
  printed abstention, never a verdict; agreement between the two orders is the
  verdict, disagreement is undecided. Verdicts land as their own resumable
  `judge` suite in `results.jsonl`, keyed so `--resume` skips what is already
  judged. The report's `function_body` line gains an `equiv` cell (the mean
  over judged rows, with the judged/undecided counts named), the header gains
  a clause naming the judge, its quant/revision/architecture, the rubric
  hash, and the swap-consistency rate, and the trailer prints the
  identical/called/undecided/skipped tally plus a "reply was not the schema"
  warning when the grammar went unenforced. Below `[bench]
  judge_min_consistency_pct` (default 70) the `equiv` column is voided for
  that run, both numbers printed, and the raw rows stay. `capability compare`
  gains an `equiv` row under `function_body` with the same paired sign test as
  every other tier, or `equiv: not compared (judge differs: …)` when the two
  runs' judge stamps disagree, or `equiv: not compared (no crossing judged in
  both runs)` when they share a judge but no crossing reached a verdict in
  both — nothing else in the comparison changes. A third new `[bench]` knob
  joins the two named above: `judge_reasoning_effort` (`none|low|medium|high`,
  default `low`, forwarded to the judge's wire only — gpt-oss needs it,
  Gemma's template ignores it).
  The 2026-08-30 probe recommends `gpt-oss-20b` (Apache-2.0, 96% swap
  consistency); Gemma 3 12B instruct also clears the 100%-parse/70%-
  consistency gate.
- `capability bench --codebase` gains `--allow-exec` and, behind it, the two
  tiers that say whether a fill is code rather than plausible text. Tier 6
  splices the fill (trimmed to the gold's line count — the same text tiers 1-4
  grade) into the worktree's copy of the file, runs `cargo check
  --message-format=json --offline`, and passes when the stream carries no
  `error` anywhere in the workspace; the exit status is not the verdict,
  because cargo exits non-zero for things it also reports and the diagnostics
  are the auditable record. Tier 7 runs the repository's own covering tests
  for the masked symbol — the enclosing function plus the cross-file symbol,
  the nearest `Cargo.toml` with a `[package] name`, up to five `#[test]`
  functions naming it as a whole word outside literals (`tests/*.rs`
  included), each through `cargo test -p <crate> --offline -- <t> --exact` —
  and passes only when all of them pass. The bounds are stated and enforced:
  the detached worktree is the only place written, one `cargo fetch` before
  the loop is the only networked step, `CARGO_TARGET_DIR` is
  `eval/.scratch/target-<head12>`, 120 s per check and 300 s per test run with
  a process-group kill, and every crossing is reverted and byte-compared
  before the next — a worktree that will not restore raises
  `ExecWorktreeDirty` and stops the run, with the rows written so far intact
  and resumable. Nothing degrades silently: no toolchain, an offline registry,
  a timeout, a span outside every function, a crate with no covering test are
  each a counted reason, printed in the block's trailer by reason, and
  excluded from the `compile` mean rather than averaged in as zeros. The row
  gains `exec` (`#[serde(default)]`; pre-B2 rows load as `None`), the stamp
  gains `allow_exec`, `cargo_version` and `exec_target` — so `compare` refuses
  across a run that executed the repository and one that did not — and the
  report gains two cells per tier line, two lift columns and the timing
  trailer. The task set is unchanged, so `corpus_id` is unchanged. Only Rust;
  `--judge` is slice C.
- `capability bench --codebase` adds the `cross_file_first` tier: the mask is
  the first use in a file of a symbol declared in exactly one **other** file,
  found over the elided texts with a declaration index — never an ambiguous
  name (declared in two or more files), never a name the file declares itself,
  never a `use` line, and never a hit inside a string or a comment. The
  default 24 tasks now split 12 / 6 / 6 rather than 16 / 8. Each cross-file
  task is crossed **twice**: once with nothing but its own file, once with the
  defining file sent as llama.cpp's `input_extra` — capped at 32 KiB and
  otherwise windowed on the declaration line, with `truncated` and the exact
  bytes on the row. The report prints both arms and the `context lift` between
  them over the tasks answered in both, with what was sent (files, KiB,
  truncated) and what the leakage filter's rule (b) withheld — every other
  file whose text contains the answer verbatim, never the defining file,
  without which the tier is unanswerable. The two arms take distinct task ids
  (`<id>` and `<id>+extra`), so `--resume` skips per arm and an arm that
  failed is one unavailable row. Because the set hash covers the new tier's
  ids, any repository that yields cross-file tasks gets a new `corpus_id`:
  runs recorded before this slice are not comparable with runs after it, and
  `compare` refuses them by that field, as it should.

  Two rules were tightened before the slice shipped, and both change numbers.
  **Precision:** a candidate's defining file is accepted only when the calling
  file names that file's module — in a `use` statement, or before a `::`. Name
  alone matched a bare `x.next()` to whichever file declared `fn next`, which
  is not a file the caller reads; `mod` also left the index, since a module
  declares a file rather than a callable symbol, and the declaration scans now
  run over literal-blanked text so `/// the fn build` is prose. The skipped
  names are counted in the tier's shortfall. **Scoring:** tiers 1–4 grade the
  prediction trimmed to the gold's line count. `n_predict` is 36 tokens per
  gold line floored at 64, so a model that answered a one-line span correctly
  and then wrote on was being graded on the run-on — the token budget, not the
  answer. Tier 5 still reads the whole prediction. Stored predictions are
  untouched, so this changes the rendered `in_file`, `function_body` and
  cross-file numbers of runs **already on disk**: the report recomputes tiers
  1–4 from stored text, and its header now says
  `tiers 1-4 score the first gold_lines lines of each fill`.
- `capability compare <A> <B>` compares the agentic and codebase sections too,
  not throughput alone. Tonight's pushkin run had to be held up against its
  predecessor by eye, two reports open side by side, which is exactly how a
  case that flipped gets missed. The agentic block prints the report's own
  figures side by side — counted by the store's helpers, so the comparison and
  the report can never disagree about what 8/10 means — over the cases both
  runs graded, keyed by suite, task id AND door. Under them go the
  disagreements: the cases where exactly one run passed, named with the losing
  side's own reason, since a case both runs failed separates nothing. Cases
  graded in only one run are listed by name rather than dropped, and
  unavailable rows leave every count with the exclusion printed. The codebase
  block pairs tasks by id and prints, per tier group and ladder tier, both
  means with a signed delta and the per-task win counts, judged by an exact
  two-sided binomial sign test at p < 0.05 — a five-nil sweep of six tasks is
  p = 0.0625 and is reported as no significant difference, which a normal
  approximation at that n would have called a win. Verdicts name the model,
  never "A" or "B", and a section one run never measured says so instead of
  vanishing.
- `chekov pull` shows a per-shard progress line while a file is in flight:
  `  shard 2/5  12.3 / 39.9 GB  31%  97 MiB/s  ETA 4m29s`, with the rate
  measured over the last five seconds so a stall shows up as it happens
  rather than being averaged away. It goes to **stderr** and stdout is
  untouched, so a script still reads one line per shard. On a terminal the
  line is redrawn in place and padded so no tail of a longer line survives;
  when stderr is redirected it becomes one plain line per 10% plus a final
  one, because a log file has no use for carriage returns. A resumed shard
  says where it picked up. Before this, a 40 GB shard was a silent hour.
- `chekov pull` resumes a partial shard instead of starting the file again.
  Each transfer already went to a `.part` sibling; now the next run asks for
  the rest of it with `Range: bytes=<n>-`. The bytes are checked before any
  of them are written: a `206` must start at exactly the offset already on
  disk and describe a file of the size the API published, or the shard is
  refused with all three numbers named — writing at the wrong offset would
  corrupt a shard that no later size check could catch. A `200` means the
  server ignored the range, so the `.part` is truncated and restarted from
  zero and says so; a `416` restarts only when no size was published, and is
  refused against a known size, because that means the server's file is not
  the one the plan was built from. A `.part` longer than the file can be is
  discarded rather than appended to. When the stream ends, the part's length
  is compared to the published size once more: a short file is never renamed
  into place, and its `.part` is kept so the next run can resume it.
- `capability graph --svg` under `--metric tok-s` draws the measured
  throughput: a "decode tok/s (measured)" panel under the grid, on the grid's
  own ctx columns, with a filled dot at each measured cell's decode median, a
  p10–p90 whisker, and the number and row name beside it — nothing for a cell
  without a run, and no panel at all without a measurement, so sparse dots
  are the honest picture rather than empty axes. Labels flip to a dot's left
  when they would run across the next dot. Predicted throughput is stated as
  not drawn (no validated model), never left to be inferred.
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
- `chekov capability bench --codebase <PATH>` — a private codebase is the
  only corpus a local user has that is guaranteed not to be in any model's
  training data. Slice A builds the whole pipeline over the narrowest task
  shape that already discriminates: same-file infill in Rust. It covers the
  gate, the worktree, deterministic task sampling, honest masking, the
  `/infill` crossing, storage, tiers 1–5 of the deterministic scoring ladder,
  and the report. Left to later slices, and said in every report so nothing
  is over-claimed: slice B (`cross_file_first` tasks with `input_extra`
  context, and tiers 6–7 behind `--allow-exec`), slice C (`--judge`), other
  languages behind the same `MaskSource` trait, and any composite score.
  The report's header reads `{n} tasks from {files} files ({a} in_file,
  {b} function_body) — boundary-scanned (not AST); context: same-file (engine
  window ≤ n_batch)`: chekov sends and grades the whole file, but llama.cpp's
  `/infill` windows the prompt at its batch size, so a long file reaches the
  model only in part and the header says so rather than implying otherwise.
  `N/A — infill unsupported by this model` is reserved for a run where NOTHING
  was answered and every crossing recorded that verdict as it happened (never
  inferred later from an error's wording — a refusal names the URL it was
  refused at, and that URL ends in `/infill`, so an outage would read as a
  missing capability); a run that failed for another reason reports that
  reason, and a run where only some tasks failed excludes
  them from every mean and counts them in the header (`(k unavailable,
  excluded)`). A task nobody answered stores no tier-5 score at all — the
  symbols cell reads `n/a` rather than averaging in a zero.

### Fixed
- **`pull` reads a folder-per-quant repo whose folders are named after the
  model rather than the tag.** bartowski publishes
  `Model-IQ3_M/Model-IQ3_M-00001-of-00005.gguf`: the folder is not itself a
  quant tag, so deriving the tag from the folder alone yielded nothing for
  every file in the repo and the whole repo was reported as "exposes no .gguf
  files" — a layout difference presented as an empty repository. The tag is now
  derived from the shard's own filename when the folder does not carry one.
  unsloth's `UD-*/` folders and flat `Model-Q8_0.gguf` names derive exactly as
  before.
- **`pull` reads lowercase and dot-separated quant tags.** `Qwen/*-GGUF` repos
  publish `qwen2.5-0.5b-instruct-q4_k_m.gguf` and mradermacher publishes
  `Model.Q4_K_M.gguf`; both derived to nothing and reported "exposes no .gguf
  files". A tag is now recognised in any case and after a `.` as well as a
  `-`, a spec matches case-insensitively, and the registry records the repo's
  own spelling — it is also the download path. If one repo carries two
  spellings of the same tag, both are listed and an inexact spec is refused
  (`QuantAmbiguous`) naming them, never resolved by a silent pick; an exact
  spelling always wins.
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
