# Sprint 1 — Workspace, CI, and the architecture skeleton

**Phase:** Phase 0 — Foundation
**Sprint goal:** a green `cargo build --workspace` plus the full quality-gate matrix on
Linux/macOS/Windows, with all ten crates present and the Bus + fractional scheduler carrying the
architecture the later phases fill in.
**Estimated duration:** 2 weeks

## Tickets

### T-01-001 — Cargo workspace, lints, and the pinned toolchain

**Description:** the virtual workspace manifest listing all ten crates, `[workspace.package]`
(edition 2024, MSRV 1.96, dual licence), `[workspace.lints]` with clippy `pedantic` + `nursery`
at warn plus `missing_docs` and `unsafe_code`, the release profile, and `rust-toolchain.toml`
pinning 1.96 with the wasm and embedded targets.

**Acceptance criteria:**

- [x] All ten crates listed as members. *(cpu, rsp, rdp, audio, cart, core, frontend,
      test-harness, netplay, cheevos.)*
- [x] `edition = "2024"`, `rust-version = "1.96"`, `license = "MIT OR Apache-2.0"`.
- [x] Workspace lints applied and clean at `-D warnings`.
- [x] `cargo metadata --format-version 1` succeeds.

**Dependencies:** none
**Reference:** `docs/architecture.md` §crate graph
**Estimated complexity:** S

---

### T-01-002 — The Bus: single ownership of mutable state

**Description:** `rustyn64-core::Bus` owning RDRAM (8 MiB, base plus Expansion Pak), the RSP,
RDP, AI, cart, controllers, and the MI interrupt lines, with the five narrow per-chip traits and
the `core::mem::take` split-borrow stepping.

**Acceptance criteria:**

- [x] `Bus` owns every mutable subsystem; the CPU borrows `&mut Bus`. *(`bus.rs`.)*
- [x] Five narrow traits implemented: `CpuBus`, `RdramBus`, `VideoBus`, `RspBus`, `AudioBus`.
- [x] Chips are stepped via split-borrow with no allocation and no `Rc`/`RefCell`.

**Dependencies:** T-01-001
**Reference:** `docs/architecture.md` fact 2
**Estimated complexity:** L

---

### T-01-003 — The fractional master-clock scheduler

**Description:** `System::tick_one_unit` advancing one VR4300 cycle per master tick with the RCP
on a 2/3 accumulator, plus seeded power-on phase alignment from SplitMix64 and a reset that
preserves it.

**Acceptance criteria:**

- [x] `MASTER_HZ = 93_750_000`, `RCP_HZ = 62_500_000`, `RCP_NUM = 2`, `RCP_DEN = 3`.
- [x] Over any 3 master ticks the RCP advances exactly 2. *(`fractional_divisor_holds_3_to_2`.)*
- [x] Power-on phase is seeded, and reset preserves alignment. *(`reset_preserves_phase`.)*
- [x] No wall-clock, OS entropy, or thread scheduling anywhere in the core.

**Dependencies:** T-01-002
**Reference:** `docs/scheduler.md`; `docs/adr/0001-master-clock-lockstep-scheduler.md`
**Estimated complexity:** L

---

### T-01-004 — ROM-format detection and header parsing

**Description:** detect and normalise `.z64` (big-endian), `.n64` (little-endian), and `.v64`
(byte-swapped) by magic, parse the cartridge header, and expose the `RomFormat`, `SaveType`, and
`Cic` enums.

**Acceptance criteria:**

- [x] All three byte orders detected by magic and normalised to native. *(`rustyn64-cart`.)*
- [x] Header fields parsed; round-trip tests cover each format.
- [x] The enums exist and are exhaustive for the backends Phase 5 will implement.

**Dependencies:** T-01-001
**Reference:** `docs/cartridge-format.md`
**Estimated complexity:** M

---

### T-01-005 — CI: the quality-gate matrix

**Description:** `.github/workflows/ci.yml` running fmt, clippy, test, rustdoc, the `no_std`
cross-build, and the `test-roms` battery, split light/full so ordinary PRs get the fast gates and
release-class events get the full matrix.

**Acceptance criteria:**

- [x] fmt, clippy `-D warnings`, test, and rustdoc `-D warnings` all gate.
- [x] `cargo build -p rustyn64-core --target thumbv7em-none-eabihf --no-default-features` gates.
- [x] The matrix covers ubuntu, macOS, and Windows on full runs.
- [x] Linux jobs install the alsa/udev headers the frontend needs. *(via
      `.github/actions/linux-build-deps`; without it every Linux job dies in a build script.)*
- [x] The matrix-selection job passes event and ref through env rather than expression
      interpolation, avoiding script injection.

**Dependencies:** T-01-001
**Reference:** `docs/testing-strategy.md` §CI gates
**Estimated complexity:** M

---

### T-01-006 — Release and Pages automation

**Description:** a `v*` tag builds all three targets, packages archives with licences, generates
`SHA256SUMS`, and publishes a GitHub Release; pushes to `main` publish rustdoc to Pages under
`/api/`.

**Acceptance criteria:**

- [x] Per-target archives contain the binary plus both licences, `NOTICE`, `README`, `CHANGELOG`.
- [x] The tag is checked against the workspace version before anything publishes.
- [x] Re-running an existing release re-uploads assets rather than erroring.
- [x] Pages deploys and serves rustdoc. *(live; `/` redirects to `/api/`.)*
- [ ] A real tag has been cut and the release path exercised end to end. **DEFERRED:** no tag
      exists yet; the workflow is written but has never run.

**Dependencies:** T-01-005
**Reference:** `docs/STATUS.md` §project infrastructure
**Estimated complexity:** M

---

## Sprint review checklist

- [x] All tickets checked off or explicitly deferred (with reason).
- [x] CI green on the main branch across all three platforms.
- [x] CHANGELOG.md updated.
- [x] `cargo build --workspace` works on a fresh clone with only Rust 1.96 installed.
