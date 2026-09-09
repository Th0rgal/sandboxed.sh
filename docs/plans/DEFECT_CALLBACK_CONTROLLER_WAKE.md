# Defect: callback-to-controller wake gap

Filed: 2026-09-09
Severity: P2
Status: open

## Problem

`patches/hermes/mission_status_route.py:46` defines the wake prompt for
mission-complete callbacks routed into the operator chat. That prompt
forbids tools (`"do not run tools"`), so the woken assistant can only
summarise the completion — it cannot trigger the project controller to
process the finished mission.

The hourly controller cron is currently the only mechanism that picks up
completions. Between hourly ticks, finished missions sit idle.

## Constraint

The operator chat is a single-writer session. Allowing the wake prompt
to run tools would create two concurrent writers (the operator and the
wake-triggered assistant), which risks interleaving, lock contention on
the session DB, and duplicate work.

## Required fix

A deterministic, deduped wake of the **owner controller** — not the
operator chat. When a terminal callback arrives:

1. Identify the owning project's controller cron job.
2. Schedule an immediate (or near-immediate) one-shot wake of that
   controller, deduped so multiple callbacks within the same window
   collapse into one wake.
3. The controller processes completions in its own single-writer session.

### What NOT to do

- Do **not** enable tools in the operator wake prompt (two writers).
- Do **not** increase hourly polling frequency (wasteful, still has a
  latency window, and scales poorly with project count).

## Affected paths

- `patches/hermes/mission_status_route.py` — callback routing and wake
- Controller cron scheduling (needs a "wake now" trigger)
- Possibly `src/api/control/mod.rs` — controller dispatch
