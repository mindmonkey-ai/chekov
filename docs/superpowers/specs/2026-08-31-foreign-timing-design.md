# Foreign-runtime timing measurement — design

Date: 2026-08-31. Status: approved in chat 2026-08-31; this document is the
binding spec. Follow-up recorded by the foreign-runtime bench
(`docs/superpowers/specs/2026-08-31-foreign-runtime-bench-design.md`, PR #56):
a foreign server reports no llama.cpp `timings` object, so today every timed
probe on a foreign run fails loudly with `ForeignTimingsUnsupported`.

## 1. Purpose

chekov's throughput numbers are 100% server-reported: `timings_from`
(runner.rs) requires llama.cpp's four fields (`prompt_n`,
`prompt_per_second`, `predicted_n`, `predicted_per_second`), all-or-nothing.
A foreign OpenAI-compatible server reports none of them, so the speed claim
chekov exists to referee cannot be measured. This design adds a SECOND timing
source — chekov's own wall clock over a streamed response — used only on
foreign runs, named on the stamp and in the report, and never blended
silently with server-reported numbers. llama.cpp runs are byte-for-byte
unchanged.

The measurement is honest about what it is: client-side timestamps around an
SSE stream include wire and translator overhead (microseconds on localhost,
negligible against token times), and the first-data-frame timestamp
approximates end-of-prefill only because these servers stream tokens as they
are generated. The report says so.

## 2. The timed streaming seam

`HttpClient` (core/hub.rs) gains ONE method with a DEFAULT implementation:

```rust
/// POST and read the response as a stream, timing it. Returns the full
/// body plus the two durations the timing math needs. The default refuses:
/// a client that cannot stream-time must say so, never fake marks around
/// a buffered read.
fn post_json_stream_timed(
    &self,
    req: &JsonRequest,
) -> Result<(String, StreamMarks), ChekovError> {
    Err(ChekovError::ForeignTimingsUnsupported {
        runtime: "unknown".to_owned(),
        reason: "this HTTP client cannot stream-time responses".to_owned(),
    })
}
```

```rust
/// Client-measured stream timing: request-written → first SSE data frame,
/// and first data frame → stream end. Durations, not instants — the math
/// needs only the two windows.
pub struct StreamMarks {
    pub to_first_data: std::time::Duration,
    pub first_to_done: std::time::Duration,
}
```

- The default implementation is what keeps the pushkin-protected integration
  test (`tests/bench_streamed_probe_crosses_the_translator.rs`, which
  implements `HttpClient` and is unwritable) compiling. Test fakes in `src/`
  override it with canned bodies and canned marks.
- `UreqClient` overrides it: write the request, then read the response
  incrementally; `to_first_data` ends at the first byte of the first
  `data:` line's payload; `first_to_done` ends when the stream is fully
  read. Monotonic clock (`std::time::Instant`) only.
- `get` and `post_json` are untouched.

## 3. Derived timings

A pure function beside `timings_from` (runner.rs):

```rust
/// llama.cpp-shaped Timings from chekov's own measurement: OpenAI `usage`
/// token counts + the stream marks. Loud on anything it cannot derive.
fn timings_from_stream(usage: &StreamUsage, marks: &StreamMarks)
    -> Result<Timings, ChekovError>
```

with `StreamUsage { prompt_tokens: u64, completion_tokens: u64 }` parsed
from the LAST SSE data frame carrying a `usage` object (the translator
already asks upstream for streamed usage — claude.rs's
`streaming_request_asks_upstream_for_usage`; OpenAI servers deliver it in
the terminal chunk).

Derivation:

- `prompt_n = usage.prompt_tokens`
- `prompt_per_second = prompt_n / to_first_data` (seconds, f64)
- `predicted_n = usage.completion_tokens`
- `predicted_per_second = (predicted_n − 1) / first_to_done` — the first
  token lands AT the first-data mark, so the decode window carries n−1
  tokens.
- `cache_n = 0` — unknowable through a foreign server; consistent with the
  unmanaged philosophy, never invented.

Guards — each fails as `ForeignTimingsUnsupported { runtime, reason }` with
the reason naming exactly what was missing (the caller supplies the
runtime):

- no `usage` frame in the stream → reason `no usage object in the stream`
- `prompt_tokens == 0` → `usage.prompt_tokens is 0`
- `completion_tokens < 2` → `fewer than 2 completion tokens — no decode
  window to time`
- either duration is zero → `zero-length timing window`

## 4. The timed crossing

A sibling of `cross_streaming` (runner.rs) — `cross_stream_timed` — that:

1. sets `stream: true` on the Anthropic-shaped request exactly as
   `cross_streaming` does;
2. sends via `post_json_stream_timed`;
3. reuses the EXISTING SSE machinery verbatim — `data_lines` to split,
   `wire.facade.stream_translator()` per frame, `assemble` for the
   Anthropic body (no parallel parser);
4. extracts `StreamUsage` from the frames and derives timings via
   `timings_from_stream`;
5. returns the same `ProbeArtifact { anthropic_body, timings }` every other
   crossing returns — downstream (sweep, rows, grading, stats, SVG,
   compare) needs zero changes.

## 5. Selection — who rides the timed path

Selection is by runtime, exactly like `FimTransport` (threaded as
parameters/bundle fields, never a `ProbeWire` field — the Task-3 ruling
stands):

- **Throughput sweep**: on a foreign run, `measure_depth`'s exec closure
  crosses via `cross_stream_timed`. llama.cpp runs keep today's path.
- **Codebase chat-FIM**: on a foreign run, `cross_fim_chat` sends its one
  user message through `cross_stream_timed` (same pins: temperature 0,
  top_k 1, gold-bounded max_tokens; the reply text is extracted from the
  assembled Anthropic body with the existing first-text-block rule, then
  normalized as today). Foreign codebase rows therefore grade the fill AND
  carry real timings. The llama.cpp `/infill` arm is untouched.
- **Agentic and fixture suites**: NOT timed on foreign runs in this design
  — their crossings still require timings and still fail per-row, but §7
  fixes their failure text. Extending them onto the timed path is a
  recorded follow-up riding the same mechanism.

## 6. Stamp, report, compare

- `Stamp` gains field 23, immediately AFTER `runtime`:

```rust
#[serde(default = "default_timing_source")]  // "server-reported"
pub timing_source: String,  // "server-reported" | "chekov-streamed"
```

  `server-reported` is true of every stored run; foreign runs written under
  this design stamp `chekov-streamed`. `first_mismatch` walks the 23-field
  order with `timing_source` third.
- Constants: `pub const TIMING_SERVER: &str = "server-reported";`
  `pub const TIMING_CHEKOV_STREAMED: &str = "chekov-streamed";`
- **Report**: when (and only when) the stamp's `timing_source` is not
  `server-reported`, the run render prints one line in the header area:
  `timing source: chekov-streamed (client wall-clock over SSE; includes
  wire overhead)`. llama.cpp run output is byte-identical to today.
- **Compare**: `CROSS_RUNTIME_ALLOWED` grows to 12 with `timing_source` —
  under `--cross-runtime` it may differ (that IS the comparison) and the
  banner names it like any differing allow-listed field. Plain compare
  refuses on it via normal field order (two foreign runs of the same
  runtime both stamp `chekov-streamed` and compare cleanly).

## 7. Foreign failure text everywhere (the PR #56 parked residual)

`ForeignTimingsUnsupported` gains the `reason` field (§3) — message:

```
the server declared as runtime '{runtime}' gave chekov nothing to time
({reason}) — a foreign run is stream-timed from `usage` token counts, and
this reply had none to work with
```

and on a foreign run EVERY per-row FAIL reason derived from a
`BenchNoTimings` failure — including the agentic and fixture suites, whose
`failed_probe`/`append_probe` path swallows errors into row reasons — reads
the runtime-aware text, never `BenchNoTimings`' engine-rebuild advice. The
recast threads through the suite-pass bundles (`AgenticPass` gains the
runtime); llama.cpp rows keep today's text exactly.

## 8. Errors

- `ForeignTimingsUnsupported { runtime, reason }` — reshaped (was
  `{ runtime }`), message per §7. All existing raise-sites supply a reason
  (`the reply carried no llama.cpp timings object` where nothing more
  specific is known).
- No other new variants. `EndpointDown`, `UpstreamRefused`,
  `BenchStreamFailed` keep their meanings on the streamed path.

## 9. Testing

Unit-level, no live server (fakes override `post_json_stream_timed`):

- `timings_from_stream`: the derivation math on known counts/durations
  (exact expected f64s), and each §3 guard's reason string.
- `cross_stream_timed`: a canned SSE body (frames + terminal usage frame) +
  canned marks yields an artifact whose body is the assembled text and
  whose timings match the math; a stream with no usage frame fails with
  the right reason; the request body carries `stream: true` and the pins.
- Selection: foreign throughput exec uses the timed crossing; foreign
  chat-FIM rides it; llama.cpp paths are unchanged (existing tests keep
  passing untouched).
- Stamp: serde back-compat (absent field → `server-reported`);
  `timing_source` mismatches third, after `runtime`.
- Compare: `timing_source` differing passes only under `--cross-runtime`
  and appears in the banner; the allow-list is exactly 12.
- Report: the header line prints for `chekov-streamed`, absent for
  `server-reported`.
- §7: a foreign agentic/fixture row failure reason contains the
  runtime-aware text and not `chekov update --engine`.
- The default trait implementation refuses with the §2 error.

Live verification (separate, approval-gated, unchanged from PR #56's
follow-up): an `mlx-lm` server on this machine, one throughput depth and
one codebase tier, then a `--cross-runtime` compare against a llama.cpp run
of the same corpus.

## 10. Out of scope

- TTFT as its own reported metric (it exists internally as
  `to_first_data`; reporting it is a later decision).
- Per-token inter-arrival timing (the two-window split is the honest limit
  of a buffered SSE read).
- Foreign `tune`, foreign agentic/fixture timing (recorded follow-up, §5).
- Any change to llama.cpp-path timing, rows, or report output.
- Launching or installing foreign runtimes.
