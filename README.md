<!-- markdownlint-disable MD033 MD041 -->
<div align="center">

# RustyN64

**A cycle-accurate Nintendo 64 emulator in Rust (LLE RSP/RDP).**

</div>

RustyN64 is a cycle-accurate Nintendo 64 emulator in Rust, architected at the Mesen2 / ares / higan
accuracy bar (a master-clock lockstep scheduler, a Bus that owns everything mutable, a
one-directional `no_std + alloc` chip-crate graph, a hard determinism contract, test-ROM-is-spec).

## Crates

- `rustyn64-cpu` — NEC VR4300 (MIPS R4300i)
- `rustyn64-rsp` — RSP (Reality Signal Processor)
- `rustyn64-rdp` — RDP (Reality Display Processor)
- `rustyn64-audio` — AI + RSP audio microcode
- `rustyn64-cart` — PI cart + PIF/CIC + saves
- `rustyn64-core` — the Bus + scheduler tie crate
- `rustyn64-frontend` — the `winit + wgpu + cpal + egui` shell (binary `rustyn64`)
- `rustyn64-test-harness` — the AccuracyCoin-equivalent oracle

## Build / test

```bash
cargo check --workspace
cargo test --workspace
cargo test --workspace --features test-roms
cargo run --release -p rustyn64-frontend -- path/to/rom
```

## License

RustyN64 is dual-licensed under **MIT OR Apache-2.0**. See `LICENSE-MIT` and `LICENSE-APACHE`.
