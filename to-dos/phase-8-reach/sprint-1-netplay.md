# Sprint 1 — Rollback netplay on the determinism contract

**Phase:** Phase 8 — Reach
**Sprint goal:** GGPO-style rollback netplay for 2-4 players, built on the existing snapshot and
restore path — the feature that most directly audits ADR 0004, because any nondeterminism
anywhere becomes a desync.
**Estimated duration:** 3 weeks

## Tickets

### T-81-001 — Fill in `rustyn64-netplay`

**Description:** turn the one-line placeholder crate into the rollback orchestration layer:
input prediction, a confirmed-frame window, and rollback-and-resimulate over the core's
snapshot/restore.

**Acceptance criteria:**

- [ ] The crate exposes predict, advance, and rollback over the existing snapshot API, with no
      new core surface added for its convenience.
- [ ] Resimulation from a restored snapshot is bit-identical to the original run.
- [ ] Rollback depth is bounded and configurable.
- [ ] The crate stays frontend-side; the core never learns netplay exists.

**Dependencies:** Phase 6 Sprint 2 (save-states), T-11-007 (the determinism test)
**Reference:** `docs/frontend.md`; `docs/adr/0004-determinism-contract.md`
**Estimated complexity:** L

---

### T-81-002 — The transport and session lifecycle

**Description:** implement the UDP transport, session negotiation, and peer liveness, with input
delay negotiated per session.

**Acceptance criteria:**

- [ ] Two to four peers connect, agree a start frame, and exchange inputs.
- [ ] Peer loss is detected by timeout and surfaced, not silently hung.
- [ ] Input delay is configurable and negotiated at session start.
- [ ] Nothing about the transport can influence core output — it only supplies inputs.

**Dependencies:** T-81-001
**Reference:** `docs/frontend.md` §netplay
**Estimated complexity:** L

---

### T-81-003 — Desync detection and adverse-network proof

**Description:** add periodic state hashing to detect divergence, and prove bit-identical
resimulation under latency, jitter, and packet loss.

**Acceptance criteria:**

- [ ] Peers exchange periodic state hashes and report a desync with the frame it began.
- [ ] A simulated adverse network (latency, jitter, loss) produces no desync over a long run.
- [ ] A deliberately introduced nondeterminism *is* caught, proving the detector works.
- [ ] Desync reports name the frame and the diverging component, not just "desync".

**Dependencies:** T-81-002
**Reference:** `docs/adr/0004-determinism-contract.md`
**Estimated complexity:** L

---

### T-81-004 — Byte-identity with the feature off

**Description:** prove that enabling the netplay feature does not change the default build's
output, so the accuracy numbers survive the feature.

**Acceptance criteria:**

- [ ] With `netplay` off, the golden corpus is byte-identical to the pre-Phase-8 baseline.
- [ ] The check runs in CI rather than being asserted in a document.
- [ ] `docs/STATUS.md`'s byte-identity claim is updated to cite the check.

**Dependencies:** T-81-003
**Reference:** `docs/STATUS.md` §version policy
**Estimated complexity:** M

---

## Sprint review checklist

- [ ] All tickets checked off or explicitly deferred (with reason).
- [ ] A 2-player session survives an adverse network without desync.
- [ ] The default build is proven byte-identical with the feature off.
- [ ] CHANGELOG.md updated.
