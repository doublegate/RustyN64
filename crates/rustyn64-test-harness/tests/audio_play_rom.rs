//! Phase 4 — the first *real ROM* produces audio through the AI.
//!
//! Boots the committed, license-clean `audio_play.z64` (our own MIPS assembly —
//! see `tests/roms/homebrew/audio_play.s`) through the harness direct-load path,
//! runs the **real VR4300** until the AI has DMA'd the ROM's PCM buffer out to
//! the DAC, and pins the emitted stereo stream byte-for-byte against the buffer
//! the ROM wrote. This is the CPU → RDRAM → AI DMA path end to end, with **no RSP
//! audio microcode** — the de-risking rung that proves the AI against a real ROM
//! before the mixer-microcode gate.
//!
//! Nothing here is synthesised: the CPU executes the ROM's own instructions,
//! which generate the PCM waveform and program the AI registers; the scheduler's
//! `audio_tick` then drains the buffer at the derived DAC rate.
#![allow(
    clippy::doc_markdown,
    reason = "narrative test docs name VR4300/RDRAM/AI/DMA in prose"
)]
// The ramp indices are all < 128, so the i16 casts provably neither truncate
// nor wrap; the values are audio samples reinterpreted as signed.
#![allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]

use rustyn64_core::System;
use rustyn64_core::audio::StereoSample;
use rustyn64_test_harness::rom;

/// The committed AI ROM, relative to this crate's manifest dir.
const AUDIO_PLAY_Z64: &[u8] = include_bytes!("../../../tests/roms/homebrew/audio_play.z64");

/// The ROM writes this many stereo sample-pairs (512 bytes / 4).
const PAIRS: usize = 128;

/// Master-tick budget. At ~44.1 kHz the DAC period is ~4248 master ticks, so
/// 128 pairs drain in ~544k ticks; this leaves generous headroom.
const BUDGET_TICKS: u64 = 1_500_000;

/// Retired-instruction floor proving the ROM actually ran: ~128 generate-loop
/// iterations × ~8 instructions + the AI setup ≈ 1050, so a run below this
/// slipped past the loop rather than executing it.
const MIN_RETIRED: u64 = 1000;

/// The waveform the ROM generates: pair `i` has `left = i*256`, `right =
/// (127-i)*256` — a rising/falling stereo ramp (see `audio_play.s`).
const fn expected(i: usize) -> StereoSample {
    StereoSample {
        left: (i as i16) * 256,
        right: ((PAIRS - 1 - i) as i16) * 256,
    }
}

#[test]
fn a_real_rom_plays_pcm_through_the_ai() {
    let entry = rom::entry_point(AUDIO_PLAY_Z64).expect("readable header");
    assert_eq!(
        entry, 0xFFFF_FFFF_8000_1000,
        "audio_play.z64's entry point, sign-extended"
    );

    let mut sys = System::new(0);
    rom::load_direct(&mut sys, AUDIO_PLAY_Z64, entry).expect("loadable");

    // Run the real CPU + scheduler; the AI DMAs the buffer as the DAC advances.
    // Collect the emitted stream and watch for the AI interrupt (raised when the
    // first buffer starts).
    let mut samples: Vec<StereoSample> = Vec::new();
    let mut irq_seen = false;
    let deadline = sys.master_ticks() + BUDGET_TICKS;
    while sys.master_ticks() < deadline {
        sys.step_to_next_edge();
        if sys.bus.rcp.mi_intr.ai {
            irq_seen = true;
        }
        samples.extend(sys.bus.drain_audio_samples());
        if samples.len() >= PAIRS {
            break;
        }
    }

    // Non-vacuity: the ROM ran its generate loop, the AI interrupt fired, and a
    // full buffer's worth of samples was emitted.
    assert!(
        sys.cpu.retired > MIN_RETIRED,
        "only {} instructions retired; the ROM cannot have generated the buffer",
        sys.cpu.retired
    );
    assert!(
        irq_seen,
        "the AI interrupt never fired — the buffer never started playing"
    );
    assert!(
        samples.len() >= PAIRS,
        "the AI emitted only {} of {PAIRS} pairs before the budget expired",
        samples.len()
    );

    // The emitted stream matches the ROM's PCM buffer byte-for-byte. A constant
    // or stuck DMA could not reproduce the per-sample rising/falling ramp.
    for (i, &sample) in samples.iter().take(PAIRS).enumerate() {
        assert_eq!(
            sample,
            expected(i),
            "sample {i} differs: the AI DMA did not read the buffer in order"
        );
    }
}

/// Determinism (ADR 0004): the same seed + ROM produces a bit-identical emitted
/// stream. A second boot must reproduce the first exactly.
#[test]
fn the_emitted_stream_is_deterministic() {
    let run = || {
        let entry = rom::entry_point(AUDIO_PLAY_Z64).unwrap();
        let mut sys = System::new(0);
        rom::load_direct(&mut sys, AUDIO_PLAY_Z64, entry).unwrap();
        let mut samples: Vec<StereoSample> = Vec::new();
        let deadline = sys.master_ticks() + BUDGET_TICKS;
        while sys.master_ticks() < deadline {
            sys.step_to_next_edge();
            samples.extend(sys.bus.drain_audio_samples());
            if samples.len() >= PAIRS {
                break;
            }
        }
        samples.truncate(PAIRS);
        samples
    };
    assert_eq!(
        run(),
        run(),
        "same seed + ROM ⇒ bit-identical AI output stream"
    );
}
