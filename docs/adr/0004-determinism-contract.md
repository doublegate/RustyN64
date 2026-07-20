# ADR 0004 — The determinism contract

## Status

Accepted.

## Context

Save-states, regression tests, TAS replay, and netplay rollback all require reproducibility.

## Decision

Same seed + ROM + input ⇒ bit-identical framebuffer + audio. Power-on phase alignment is a
SEEDED PRNG; reset preserves it. No system time / thread scheduling / OS RNG in the core.
Rate control + run-ahead live in the frontend (a resampler stage / snapshot-restore
orchestration), never the core synthesis.
