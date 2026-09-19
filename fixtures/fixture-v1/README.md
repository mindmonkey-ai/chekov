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
                  semantics, plus the replay/lifetime helper
    projection/   a fold over a sliding window
    api/          the command dispatcher containing the near-miss API pair
  hidden/         held-out assertions — NEVER materialized into any prompt
  manifest.toml   the grading contract: names the hidden set
  Cargo.toml      `panic = "abort"` in dev, matching chekov's release policy
```

The crate is self-contained: build with `cargo build`, test with
`cargo test`. No network, no download, no new dependency.

## The four anti-saturation devices

These are what make the probe discriminate rather than saturate. Each device has
a masked body the candidate writes and a held-out assertion in `hidden/` that the
grader injects:

| # | Device | Masked source | Held-out file | Tier |
|---|--------|---------------|---------------|------|
| 1 | Cross-file first-use mask + capacity | `src/store/mod.rs` `LimitedStore::record` | `hidden/store_limited_full.rs` | 7 |
| 2 | Near-miss API (central discriminator) | `src/api/mod.rs` `handle_credit` | `hidden/near_miss_api.rs` | 7 |
| 3 | Invariant trap — money is `i128` cents, never `f64` | `src/domain/money.rs` `from_str` | `hidden/invariant_exact.rs` | 7 |
| 4 | Generic + lifetime knot | `src/store/replay.rs` `replay_filtered` | `hidden/lifetime_knot.rs` | 6 |

**Device 1** — the masked capacity check is the first use of `StoreError::Full`
and the only place a `LedgerEntry` triggers a hard rejection; `LedgerEntry` and
`StoreError` are defined two modules away, so reading only this file cannot
resolve them. A candidate that unconditionally `push`es passes the obvious
assertion but leaves the balance of a full store wrong.

**Device 2** — `apply_entry` and `append_entry` both compile and both return
`Ok`; only `apply_entry` advances the projection. Choosing the wrong one fails
only the hidden balance assertion. This defeats models that pattern-match on
plausible-looking code; it cannot be passed by formatting mimicry.

**Device 3** — the tempting `s.parse::<f64>() * 100.0 as i128` path compiles and
passes `from_str("0.01")` but underflows the cents by one on `2499.95`
(`249994` via float, `249995` via str-path). The invariant is stated only in a
comment in `domain/mod.rs`.

> Note: the capability-spec's concrete example `8014.35` does **not** trap under
> `as i128` (both paths give `801435`). `2499.95` is the real invariant
> candidate and is what the held-out assertion pins.

**Device 4** — the returned iterator must borrow `entries` for `'a`, and the
filter closure must capture `filter` for that same `'a`. A near-miss borrow
signature fails to compile, so this is graded by the **compile gate** (tier 6),
not the test gate.

## Scoring tiers (capability-spec §9)

Tiers 6 and 7 are the ones this slice uses:

- **Tier 6 — compile gate:** `cargo check` (JSON diagnostics). A body the
  candidate writes must still compile. Device 4 is graded here.
- **Tier 7 — test gate:** run only the specific covering test. Devices 1–3 are
  graded here.

Tiers 1–2 (whitespace exact match, edit similarity) are line-level and would
punish semantically-correct alternative implementations and reward formatting
mimicry, so they are deliberately not assigned to any body-level task.

## How the grader assembles and grades this slice

`--fixture` materializes `fixture-v1` into `$CHEKOV_HOME/eval/fixture-v1/`.
The grader:

1. copies the workspace to a scratch dir,
2. writes the `hidden/` assertions into it,
3. runs them against the model's patch.

`manifest.toml` is consulted by the context assembler — not a glob that someone
can later edit — so every `hidden` file named there is **excluded from every
prompt by construction**. Three properties follow: the model can never read the
tests (physically absent from disk, not merely filtered); the tasks cannot be
answered by pattern-matching a visible assertion; and the leakage filter from
`capability-spec.md` §8 runs over the fixture identically, with its exclusion
count printed, so the fixture's honesty is auditable by the same mechanism as a
user repo's.

## Release-gate status — NOT cleared

Per `IDEAS.md` (2026-09-16) and `capability-spec.md` §9, this slice is
**release-gated** and the gate is **unmet**. It does not ship compiled-in until
it has been measured against **three models of clearly different capability**
with a real spread published.

This work authors the content slice that makes the gate *measurable in the
future*; it does not clear the gate. Concretely:

- **Angle A (release gate) is unmet.** No three-model campaign has run on this
  fixture. The preflight trio in `IDEAS.md` listed `gpt-oss-120b`, which is
  **gone** from `models/` — only `gpt-oss-20b` remains — so even the preflight
  set can no longer be reproduced as-is.
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

## Verified before shipping

- `cargo build` — warning-free; `cargo test` — 20 unit tests pass.
- All four held-out assertions compile against the real public API and pass
  against the correct implementation.
- Each device confirmed to discriminate: a plausible wrong body for every device
  fails its hidden assertion (devices 1–3 by value, device 4 by failing to
  compile).

## Scope

Content only. Nothing here is wired into production `chekov` (`src/`). The
`manifest.toml` + `hidden/` mechanism is a contract for the not-yet-built
materialization/grading pipeline; it names the pieces rather than implementing
the plumbing. See `capability-spec.md` §9 and `IDEAS.md` for the release-gate
reconciliation.
