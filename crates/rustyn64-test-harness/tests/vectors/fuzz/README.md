# Seeded-fuzz conformance corpus

Each `.rvec` here is a **curated** candidate from the reproducible Angrylion
vector generator. Only candidates that already reproduce the Angrylion golden
byte-for-byte are committed, so a config that trips an RDP corner RustyN64 does
not yet model is dropped rather than baked in. The whole directory is replayed by
the `fuzz_corpus_matches_angrylion` gate in `../../rdp_conformance.rs`.

## Reproducing

The generator lives in `../../../vectors-gen/driver.c` (`--fuzz` mode). It is
seeded (SplitMix64), so a `(seed, count)` pair plus the generator source fully
determines every emitted vector.

```sh
cd crates/rustyn64-test-harness/vectors-gen
make ANGRYLION_CORE=../../../ref-proj/parallel-rdp/angrylion-rdp-plus/src/core
./driver --fuzz /tmp/cand <seed> <count>
```

then curate (replay against RustyN64, keep only the passers):

```sh
RUSTYN64_FUZZ_DIR=/tmp/cand cargo test -p rustyn64-test-harness \
  --test rdp_conformance -- --ignored --nocapture curate_fuzz_candidates
```

## Committed batches

| Prefix | Family | Seed | Count | Notes |
| --- | --- | --- | --- | --- |
| `fz_fill_` | FILL-mode `Fill Rectangle` (16-bit) | `0x1234` | 48 | Sweeps fill colour, image size, and rectangle position (scissor is the full image). Found the R-3 inclusive-lower-right edge bug. Generator family `fillrect`. |
| `fz_scis_` | FILL `Fill Rectangle` + independent scissor sub-rect (16-bit) | `0x5c15` | 48 | Varies the scissor so it clips the rectangle on each edge. Found the R-15 asymmetric scissor clip (inclusive X, exclusive Y, `allover` guard). Generator family `scissor`. |

Regenerating batch `fz_fill_` from its seed is byte-identical to what is
committed here.
