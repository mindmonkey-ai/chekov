# Foreign-runtime bench — design

Date: 2026-08-31. Status: approved in chat 2026-08-31; this document is the
binding spec. Idea: IDEAS.md "Bench a foreign runtime: MTPLX and MLX servers
as first-class candidates (2026-08-30)".

## 1. Purpose

chekov benches models through its own Anthropic↔OpenAI translator against a
llama-server it launches. MTPLX and MLX servers speak the same OpenAI wire and
claim large decode speedups on the same hardware; today chekov cannot referee
that claim because the stamp assumes a llama.cpp engine commit, readiness
asserts llama-server's `/props`, and codebase mode rides llama-server's
`/infill`. This design makes a foreign OpenAI-compatible server a first-class
bench candidate — declared, never guessed; measured, never managed — and gives
`compare` an explicit cross-runtime mode so two runs of the same corpus on two
runtimes can be read side by side with the incomparabilities named out loud.

Two slices, one plan, in order: slice 1 (runtime identity, foreign
candidates, FIM chat fallback) then slice 2 (`compare --cross-runtime`).

## 2. CLI surface

```
chekov capability bench NAME --runtime <name>@<version> [--upstream <url>] [existing bench flags]
chekov capability compare A B --cross-runtime [existing compare flags]
```

- `--runtime` declares the runtime serving the subject model. Format:
  `<name>@<version>`, split on the LAST `@`. `name` must match
  `[a-z0-9][a-z0-9._-]*` (lowercase); `version` must be non-empty with no
  whitespace. Anything else is refused at parse with
  `RuntimeFlagInvalid { value, reason }` (§8). Absent `--runtime`, every
  current behaviour is exactly unchanged and the stamp records
  `runtime = "llama.cpp"`.
- `--runtime` is `UseRunning`-only for the SUBJECT: the user starts the
  foreign server; chekov never launches, installs, builds, or tears down a
  foreign runtime. If the bench plan would need to launch the subject (the
  named model is not already served), the run refuses with
  `RuntimeNeedsRunningServer { runtime }` before any measurement. The judge
  candidate is exempt: `--judge` still launches chekov's own local llama.cpp
  judge exactly as today — the restriction is on the subject only.
- `--upstream <url>` overrides the server base URL for this run (foreign
  servers pick their own ports). Valid only together with `--runtime`;
  `--upstream` without `--runtime` is refused by clap (`requires`). Absent,
  the configured chekov endpoint is used as today.
- `--cross-runtime` on `compare` is slice 2 (§7). Without it, `compare`
  refuses two runs whose `runtime` differs, naming the field (§3) — plain
  compare never silently blends runtimes.

## 3. Stamp: the `runtime` field

`Stamp` gains one field:

```rust
#[serde(default = "default_runtime")]  // "llama.cpp"
pub runtime: String,                   // e.g. "llama.cpp", "mtplx 0.4.1"
```

- Stored value: for llama.cpp runs, the literal `llama.cpp`; for foreign runs,
  `<name> <version>` from `--runtime` (single space join — `@` stays a CLI
  spelling, not a stored one).
- Position: immediately AFTER `machine_id` and BEFORE `engine_build_commit`.
  Field order is comparison order, so any cross-runtime pair mismatches on
  `runtime` before commit noise can produce a llama.cpp-flavoured message.
- Serde default keeps every stored JSONL run readable: absent field
  deserializes to `llama.cpp`, which is true of every run written before this
  change.
- `engine_build_commit` for a foreign run holds the declared `<version>`
  verbatim (it is the runtime's build identity string; for llama.cpp it stays
  the git commit). It is never probed from the server.
- `first_mismatch` walks the new 22-field order. The
  `BenchStampMismatch` message is rewritten engine-neutral:

```
bench stamp mismatch on '{field}' ({a} vs {b}) — results are comparable only
inside one pinned configuration (runtime, build, flags and sampling all held
constant); re-bench under a matching stamp and compare those runs
```

The llama.cpp float-associativity lecture moves to the `Stamp` doc comment,
where it remains true, instead of being printed for a `corpus_id` mismatch.

## 4. Foreign readiness

For a foreign runtime the llama-server checks do not exist, so readiness is:

- One plain `GET <upstream>/v1/models` (chekov's `HttpClient` GET seam carries
  no bearer, and `/v1/models` is unauthenticated on the servers this targets;
  a server that requires auth fails loudly here, which is correct). Success is
  HTTP 200 with a JSON body containing a `data` array. Anything else fails as
  the existing `EndpointDown` with the URL.
- The served model ids from `data[].id` are PRINTED, never asserted:
  `chekov: runtime mtplx 0.4.1 serves: <id>[, <id>...]` — chekov cannot know
  how a foreign server names the weights, so it reports and lets the human
  read. An empty `data` array still prints (`serves: (none listed)`) and
  proceeds; the first probe will fail loudly if nothing is really served.
- No `/health` poll, no `/props` ctx assertion, no pid watching (there is no
  pid). The llama.cpp path is byte-for-byte unchanged.

## 5. Unmanaged stamp fields

Launch flags chekov cannot observe on a server it did not launch are stamped
with fixed sentinels — recorded honestly as "not managed by chekov", never
invented:

| field | foreign value |
|---|---|
| `ctx` (u32) | `0` |
| `n_parallel` (u32) | `0` |
| `kv_unified`, `n_batch`, `n_ubatch` (flag strings) | `"unmanaged"` |
| `type_k`, `type_v`, `flash_attn` (flag strings) | `"unmanaged"` |

(The six flag fields are the stamp's argv-sourced strings whose absent-flag
value is `"engine-default"`; `"unmanaged"` is a third, distinct spelling —
"chekov did not launch this server", never "the engine's default".) The
run head's `launch_args` records the empty list — chekov passed none.

Everything chekov does control or know stays real: `machine_id`, weights
identity (`weights_revision`, `quant` from the registry entry — an MTPLX
community build is registered in `models.toml` like any model; `UseRunning`
needs no pulled weights on disk), `seed` and `temperature_milli` (chekov sets
sampling in every request body), `allow_exec`, `exec_target`, `cargo_version`,
`chekov_version`, `corpus_id`, `prompt_set_hash` (§6), `judge`.

Two foreign runs of the same declared runtime and model therefore compare
cleanly under plain `compare`; a foreign run can never silently match a
llama.cpp run because `runtime` differs first.

## 6. Codebase mode: FIM over chat completions

`cross_infill` gains a second transport. Selection is by runtime — `llama.cpp`
uses `/infill` exactly as today; a foreign runtime uses chat completions. The
existing `InfillOutcome::Unsupported` detection stays as the safety net on the
`/infill` arm.

The chat arm packs the same task content into one deterministic user message
crossing the same translator path the agentic probes use (Anthropic-shaped in,
OpenAI out), `temperature 0`, `top_k 1`, `max_tokens` = the infill arm's
`n_predict`. Template (verbatim; `{...}` are the only substitutions):

```
You are completing code. Output ONLY the missing code between PREFIX and
SUFFIX. No explanation, no code fences, no repetition of the prefix or
suffix.

{for each extra chunk}
FILE {filename}:
{text}
{end}

PREFIX:
{input_prefix}

SUFFIX:
{input_suffix}

MIDDLE:
```

Reply normalization, in order, before grading: (1) if the entire reply is a
single fenced code block, strip the fences and any language tag; (2) trim one
trailing newline. Nothing else — the graders read the result as text, exactly
as they read `/infill` content today.

`prompt_set_hash` for a run whose codebase suite used the chat arm is
`hash(existing input ‖ the template string above)`. llama.cpp runs hash
exactly as today. So a template edit is a NAMED stamp change between foreign
runs, and the two transports never carry the same hash (§7 handles the
cross-runtime consequence).

The codebase report section header names the transport once per run:
`fim transport: /infill` or `fim transport: chat`.

## 7. Slice 2 — `compare --cross-runtime`

Plain `compare` (no flag) refuses on `runtime` like any first-differing field.
`--cross-runtime` permits EXACTLY this allow-list to differ, and nothing else:

- `runtime`, `engine_build_commit`
- the eight §5 unmanaged fields (`ctx`, `n_parallel`, `kv_unified`,
  `n_batch`, `n_ubatch`, `type_k`, `type_v`, `flash_attn`)
- `prompt_set_hash` (the transports' prompts genuinely differ — that IS part
  of what a runtime is)

Everything else — `machine_id`, `allow_exec`, `cargo_version`, `exec_target`,
`seed`, `temperature_milli`, `chekov_version`, `corpus_id` — must still
match; a mismatch outside the allow-list refuses with the same
`BenchStampMismatch`, so `--cross-runtime` never becomes "compare anything".
(`weights_revision`, `quant` and `judge` are already masked by plain
`compare` as the comparison's subject, not its precondition — that stays
exactly as it is.)

Output opens with a banner, before any section:

```
cross-runtime comparison: <runtime A> vs <runtime B>
determinism does not hold across runtimes; differing fields:
<field>: <a> vs <b>          (one line per allow-listed field that differs)
this measures the runtimes, not the model.
```

Below the banner, every existing compare section runs unchanged — including
the codebase tiers and the B2 exec tiers, which is the referee's point: if the
foreign runtime's "output distribution unchanged" claim is true, its fills
should compile and pass at the same rate on the same corpus. When both runs
have the same runtime, `--cross-runtime` is accepted, the banner still prints
(with zero differing-field lines under it), and behaviour is otherwise plain
compare.

## 8. Errors

New `ChekovError` variants, both exit 1 like the rest:

- `RuntimeFlagInvalid { value: String, reason: String }` —
  `--runtime '{value}' is not <name>@<version> — {reason}`.
- `RuntimeNeedsRunningServer { runtime: String }` —
  `--runtime {runtime} benches a server you started — chekov cannot launch a
  {runtime} server; start it, then re-run (the subject must already be
  serving)`.

Existing errors reused: `EndpointDown` for a foreign upstream that does not
answer `/v1/models`; `BenchStampMismatch` (message rewritten, §3) for both
plain and cross-runtime refusals.

## 9. Testing

Unit-level against fakes on the existing `HttpClient` seam; no foreign server
needed to ship:

- `--runtime` parse: valid, missing `@`, empty version, uppercase name,
  whitespace — exact `RuntimeFlagInvalid` reasons.
- Subject-launch refusal fires before any HTTP; judge launch is exempt.
- Foreign readiness: 200-with-data passes and prints ids; empty data prints
  `(none listed)`; non-200 and connect failure are `EndpointDown`.
- Sentinel stamping: a foreign run's stamp records exactly the §5 table; a
  default run still records real values and `runtime = "llama.cpp"`.
- Serde back-compat: a stamp JSON without `runtime` deserializes to
  `llama.cpp`.
- `first_mismatch` ordering: cross-runtime pair mismatches on `runtime`
  before `engine_build_commit`.
- Chat-FIM body shape (template verbatim, temperature 0, top_k 1), both
  normalization rules, and transport selection by runtime.
- `prompt_set_hash` divergence between the two transports, stability within
  each.
- `--cross-runtime` allow-list: each allow-listed field may differ; each
  non-listed field still refuses; banner lines list exactly the differing
  fields; same-runtime `--cross-runtime` prints the banner with no field
  lines.

Live verification (approval-gated, not required to ship): an `mlx-lm` server
on this machine serving a registered model, one codebase-tier bench through
the chat arm, and one `--cross-runtime` compare against an existing llama.cpp
run of the same corpus.

## 10. Out of scope

- Launching, installing, building, or version-probing foreign runtimes.
- MTP-head bench rows (separate IDEAS entry; waits on upstream llama.cpp).
- Streaming probes over foreign runtimes (buffered only, as codebase mode is
  today).
- Any registry schema change — foreign models are ordinary `models.toml`
  entries.
- `tune` over foreign runtimes (`tune` is launch-flag search; foreign flags
  are unmanaged by definition).
