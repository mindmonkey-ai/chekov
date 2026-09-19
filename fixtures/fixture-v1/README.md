# fixture-v1 — a graded Rust probe for the capability-spec `--fixture` gate

A small event-sourced ledger crate that grades a candidate's ability to
integrate cross-file context, honour a stated invariant, and choose the correct
of two near-identical APIs. It is a **content slice**, not production chekov
code and embedded into the chekov binary by `build.rs` and run by
`chekov capability bench --fixture --allow-exec` (`docs/capability-spec.md`
§9).

## Layout

```
fixture-v1/
  src/            the materialized crate (compiled-in, shown to the candidate)
    domain/       entities + invariants stated only in comments
    store/        a trait and two impls with deliberately different failure
                  semantics, plus replay helpers
    projection/   a fold over a sliding window
    api/          the command dispatcher containing the near-miss API pair
  hidden/         held-out assertions — NEVER materialized into any prompt
  manifest.toml   the grading contract: names the hidden set
  Cargo.toml.in   `panic = "abort"` in dev, matching chekov's release policy —
                  named `.in` so this tree is not a nested cargo package, which
                  would drop it out of the published `.crate`; `build.rs`
                  embeds it under the key `Cargo.toml` and the materializer
                  writes it back out under that name
```

The crate is self-contained — no network, no download, no new dependency — but
it is not buildable in place: there is no `Cargo.toml` here to build against.
Run it through `chekov capability bench --fixture --allow-exec`, or materialize
it (see `fixture::materialize`) and `cargo test` in the materialized tree.

## The four anti-saturation devices

These are what make the probe discriminate rather than saturate. Each device has
a masked body the candidate writes and a held-out assertion in `hidden/` that the
grader injects:

| # | Device | Masked source | Held-out file | Tier |
|---|--------|---------------|---------------|------|
| 2 | Near-miss API (central discriminator) | `src/api/mod.rs` `handle_credit` | `hidden/near_miss_api.rs` | 7 |
| 3 | Invariant trap — money is `i128` cents, never `f64` | `src/domain/money.rs` `from_str` | `hidden/invariant_exact.rs` | 7 |
| 5 | Integer-remainder split conserves money | `src/domain/money.rs` `split_evenly` | `hidden/split_conserves.rs` | 7 |
| 6 | Exact overdraft boundary | `src/domain/ledger.rs` `debit_allowed` | `hidden/debit_boundary.rs` | 7 |

**Device 2** — `apply_entry` and `append_entry` both compile and both return
`Ok`; only `apply_entry` advances the projection. Choosing the wrong one fails
only the hidden balance assertion. This defeats models that pattern-match on
plausible-looking code; it cannot be passed by formatting mimicry.

**Device 3** — the tempting `s.parse::<f64>() * 100.0 as i128` path compiles and
passes `from_str("0.01")` but underflows the cents by one on `2499.95`
(`249994` via float, `249995` via str-path). The invariant is stated only in a
comment in `domain/mod.rs`.

**Device 5** — integer division alone drops indivisible cents. The contract
requires every share to sum back to the exact total, including below zero, with
the Euclidean remainder distributed to the earliest shares. The tempting
`total / parts` body compiles but fails the held-out conservation cases. Its
body is followed only by an elided test module, preventing FIM suffix runaway.

**Device 6** — an overdraft is strictly greater than the current balance. The
tempting `< balance` predicate compiles but refuses a debit equal to the
balance. `debit_allowed` is a short exhaustive match at end of file, with no
similar-signature method for an infill model to continue into.

## Scoring tiers (capability-spec §9)

This slice uses tier 7, after every fill passes tier 6:

- **Tier 6 — compile gate:** `cargo check` (JSON diagnostics). A body the
  candidate writes must still compile before a held-out test can grade it.
- **Tier 7 — test gate:** run only the specific covering test. All four devices
  are graded here.

Tiers 1–2 (whitespace exact match, edit similarity) are line-level and would
punish semantically-correct alternative implementations and reward formatting
mimicry, so they are deliberately not assigned to any body-level task.

## How the grader assembles and grades this slice

`--fixture` materializes `fixture-v1` into `$CHEKOV_HOME/eval/fixture-v1/`.
The grader:

1. copies the workspace to a scratch dir,
2. writes the `hidden/` assertions into it,
3. runs them against the model's patch.

The patch is the canonical evaluated body, not a reference-length prefix. The
grader preserves the raw reply, then lexically stops before the first unmatched
closing brace outside literals and nested comments; a reply that never crosses
that boundary is kept in full. All semantic, symbol, judge, compile, and test
consumers use those same bytes. Function bodies receive a fixed 1440-token
budget, independent of the gold body.

`manifest.toml` is consulted by the context assembler — not a glob that someone
can later edit — so every `hidden` file named there is **excluded from every
prompt by construction**. Three properties follow: the model can never read the
tests (physically absent from disk, not merely filtered); the tasks cannot be
answered by pattern-matching a visible assertion; and the leakage filter from
`capability-spec.md` §8 runs over the fixture identically, with its exclusion
count printed, so the fixture's honesty is auditable by the same mechanism as a
user repo's.

## Release-gate status — NOT cleared

Per `IDEAS.md` and `capability-spec.md` §9, this compiled-in slice remains
**release-gated**: it ships, but no published capability number rests on it
until three models of clearly different capability produce a real spread.

- **Angle A (release gate) remains unmet.** Two campaigns on older corpus ids
  failed acceptance: the original devices saturated, and the first hardening
  attempt measured FIM suffix runaway rather than capability. The campaign on
  `fixture-v1:068b719a1d81` was also flat at one pass per model. Device 6
  saturated; two syntactically complete but longer device-5 answers were cut
  to the gold body's line budget and failed compilation, so that apparent
  difference is not a capability signal. A counterfactual replay under the
  lexical ruling makes both answers compile, but both then fail the negative
  remainder assertion. The corrected taxonomy is semantic test failure, while
  the absolute result remains one pass per model.
- **The post-ruling campaign is also flat.** Runs
  `20260919T235139Z-qwen3.5-9b`, `20260919T235200Z-qwen3.8-27b`, and
  `20260919T235259Z-ornith-1.5-35b-a3b` share grading identity
  `f93fc06b2db4` and again score 1/4 each. Device 5's two longer alternatives
  are retained, compile, and fail only the hidden negative-total case; device 6
  passes all three models. The gate therefore remains closed for lack of a
  genuine separator, not because of a grading artifact.
- **Angle B (runtime detector)** will be applied at run time: when every
  candidate scores above 90% or below 10% on a tier, the tier is reported, not
  ranked.
- **Versioning.** `fixture-v1`'s id and content hash go into every `stamp.json`;
  a future `fixture-v2` can never be silently compared against a v1 run. Ten
  task slots are held in reserve so v1 can be hardened without renumbering.
- **Licensing (spec open decision #10).** `fixture-v1` ships inside a public
  repo, so its content becomes training data — the held-test design mitigates
  leakage of the *answers*, not the *prompts*. Codebase mode (a private repo) is
  the durable signal; the fixture is the convenient one.

## Verification

- In the materialized tree: clippy is warning-free and 21 unit tests pass.
- All four held-out assertions compile against the real public API and pass
  against the correct implementation.
- Devices 5 and 6 are exercised end to end by the real-cargo gated test: each
  plausible wrong body compiles, then fails only its hidden assertion; each
  gold body passes.
- Prompt assembly resolves all four devices in order and withholds each gold
  body and every checked held-out answer literal.

## Scope

The content remains isolated from chekov's production behavior, but it is
compiled into the binary by `build.rs`. The manifest-driven fixture pipeline
materializes it into a clean scratch repository, assembles prompts through
codebase mode, and injects exactly one held-out test for each tier-7 crossing.
See `capability-spec.md` §9 and `IDEAS.md` for the release-gate record.
