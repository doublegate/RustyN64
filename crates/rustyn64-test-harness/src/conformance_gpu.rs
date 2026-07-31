//! Replay the `.rvec` conformance corpus through the **GPU** RDP (ADR 0014).
//!
//! Its shape is deliberate: it grades the GPU
//! backend against **Angrylion's** golden framebuffers — the same independent
//! oracle the software rasterizer is graded against in
//! [`crate::conformance`] — and *not* against the software rasterizer's own
//! output. Two implementations agreeing tells you nothing about either; two
//! implementations independently matching a third does.
//!
//! So a disagreement here is diagnosable rather than merely alarming. There are
//! **four** outcomes for a vector and they mean different things — one per
//! bucket of [`Census`], which is the point of there being four buckets:
//!
//! | software | GPU | reading |
//! | --- | --- | --- |
//! | matches | matches | the feature is right on both paths |
//! | matches | differs | a GPU-backend or plumbing defect |
//! | differs | matches | a software-rasterizer gap the GPU already covers |
//! | differs | differs | the **vector or the oracle** is suspect, not either rasterizer |
//!
//! The third row is expected to be non-empty. parallel-rdp is a far more
//! complete RDP than this project's software rasterizer, so "the GPU has all the
//! features of the software one" was never the interesting question — the
//! interesting one is which vectors each path gets right, and that is what
//! [`census`] reports.
//!
//! # The byte-order hazard
//!
//! parallel-rdp stores RDRAM in **native little-endian 32-bit word order** —
//! `vram8.data[index ^ 3]` throughout its compute shaders. RustyN64's `Bus`
//! stores RDRAM as **big-endian bytes**, and the `.rvec` goldens are in that same
//! layout. Every transfer across this boundary therefore reverses each aligned
//! 4-byte group ([`swap_words_into`]).
//!
//! Getting this wrong does not fail loudly: it renders a picture with the right
//! shapes and wrong colors, or the right colors in the wrong pixels. It is
//! pinned by [`tests::word_swap_is_its_own_inverse`] and, more usefully, by the
//! fact that a whole corpus would go red at once rather than one vector.

#![allow(
    clippy::doc_markdown,
    reason = "narrative module docs name RustyN64/RDRAM/Angrylion in prose, as `conformance` does"
)]

use crate::conformance::Vector;
use rustyn64_rdp_gpu::GpuRdp;

/// 8 MiB — the retail N64 with the Expansion Pak, and what `Bus::new` allocates.
///
/// A multiple of 4096, which [`GpuRdp::new`] requires (it rejects rather than
/// rounds, so a size that is not stays a loud failure).
pub const RDRAM_SIZE: usize = 8 * 1024 * 1024;

/// parallel-rdp's "hidden" RDRAM: the 9th bit per byte, one byte per halfword.
pub const HIDDEN_RDRAM_SIZE: usize = RDRAM_SIZE / 2;

/// Copy `src` into `dst`, reversing each aligned 4-byte group.
///
/// This converts between RustyN64's big-endian RDRAM bytes and parallel-rdp's
/// native word order, in **either** direction — the transform is its own
/// inverse, which is why one function serves both.
///
/// # Panics
///
/// Panics if the lengths differ or are not a multiple of 4. A partial trailing
/// group has no defined mapping, and silently leaving it unswapped would corrupt
/// exactly the last pixels of a framebuffer, where nobody looks.
pub fn swap_words_into(dst: &mut [u8], src: &[u8]) {
    assert_eq!(dst.len(), src.len(), "word-swap length mismatch");
    assert_eq!(src.len() % 4, 0, "word-swap needs whole 32-bit words");
    for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
        d[0] = s[3];
        d[1] = s[2];
        d[2] = s[1];
        d[3] = s[0];
    }
}

/// Write `src` into RDRAM at logical address `addr`, in parallel-rdp's layout.
///
/// Per byte, using the `^ 3` mapping directly, so it holds for **any** address
/// and **any** length — unlike [`swap_words_into`], which needs whole words at a
/// word-aligned base. That generality is not theoretical: a committed vector
/// carries a 2-byte texture preload.
fn write_region(rdram: &mut [u8], addr: usize, src: &[u8]) {
    for (i, &b) in src.iter().enumerate() {
        rdram[(addr + i) ^ 3] = b;
    }
}

/// Read `out.len()` bytes from RDRAM at logical address `addr`, converting back
/// to RustyN64's layout. The inverse of [`write_region`].
fn read_region(rdram: &[u8], addr: usize, out: &mut [u8]) {
    for (i, b) in out.iter_mut().enumerate() {
        *b = rdram[(addr + i) ^ 3];
    }
}

/// Why a GPU replay could not run. Distinct from "it ran and disagreed".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unavailable {
    /// No usable Vulkan device. The honest outcome on a headless CI runner.
    NoDevice,
    /// A device exists but RDRAM could not be mapped for host access.
    RdramUnmappable,
}

/// Replay `v`'s command stream through the GPU RDP.
///
/// Returns the rendered framebuffer region in **RustyN64's** byte order, so the
/// result is directly comparable to `v.golden_fb` and to
/// [`crate::conformance::replay`]'s output — the caller does not have to know
/// about the swap.
///
/// # Errors
///
/// [`Unavailable::NoDevice`] when there is no usable Vulkan device — which is
/// not a failure of the backend, see the module docs — or
/// [`Unavailable::RdramUnmappable`] when a device exists but its RDRAM could not
/// be mapped for host access.
///
/// # Panics
///
/// Panics if the vector's addresses do not fit in RDRAM, matching
/// [`crate::conformance::replay`].
pub fn replay(v: &Vector<'_>) -> Result<Vec<u8>, Unavailable> {
    let Some(mut gpu) = GpuRdp::new(RDRAM_SIZE, HIDDEN_RDRAM_SIZE) else {
        return Err(Unavailable::NoDevice);
    };

    let base = v.cmd_addr as usize;
    let fb = v.fb_addr as usize;
    // Widen before multiplying: `Vector`'s fields are all public, so a
    // directly-constructed one can overflow `u32` here and silently produce a
    // bogus length. Mirrors `conformance::replay`.
    let fb_len = (v.width as usize) * (v.height as usize) * (v.bpp as usize);
    let pre = v.preload_addr as usize;
    let fits =
        |start: usize, len: usize| start.checked_add(len).is_some_and(|end| end <= RDRAM_SIZE);
    assert!(
        fits(base, v.cmds.len()) && fits(fb, fb_len) && fits(pre, v.preload.len()),
        "vector addresses exceed RDRAM ({RDRAM_SIZE} bytes)"
    );

    // Write the preload and the command list STRAIGHT into the mapped RDRAM.
    //
    // An earlier version staged an 8 MiB copy in RustyN64's layout and swapped
    // that across: two full passes and a 16 MiB allocation per vector, 43 times
    // over. The equivalent fusion in the frontend measured **3.3x** faster
    // (`docs/performance.md`), so this is not hypothetical tidiness.
    //
    // Per-byte through [`write_region`] rather than [`swap_words_into`], because
    // a region is not necessarily a whole number of words — a committed vector
    // carries a **2-byte** preload, and the word-wise version asserted on it. The
    // comment here previously claimed every region was 4-byte aligned; the assert
    // is what proved otherwise.
    if gpu
        .with_rdram_mut(|rdram| {
            rdram.fill(0);
            write_region(rdram, pre, v.preload);
            write_region(rdram, base, v.cmds);
        })
        .is_none()
    {
        return Err(Unavailable::RdramUnmappable);
    }

    // parallel-rdp does NOT read the command list out of RDRAM the way the
    // hardware DP FIFO does — `enqueue_command` takes the decoded words, one
    // command per call, and the handler reads however many the opcode implies.
    // So the stream has to be split here, by the same length table the software
    // path uses (`rustyn64_rdp::command`), rather than handed over whole.
    for cmd in split_commands(v.cmds) {
        assert!(
            gpu.enqueue_command(&cmd),
            "the GPU backend refused a command"
        );
    }
    assert!(gpu.idle(), "the GPU backend failed to drain");

    // Read back through the coherency handshake, not through any pointer of our
    // own: when the host-import path is unavailable the device renders into its
    // own buffer and the shim's allocation is stale.
    //
    // Only the framebuffer region, not all 8 MiB — the rest is not compared.
    let mut out = vec![0u8; fb_len];
    if gpu
        .with_rdram(|rdram| read_region(rdram, fb, &mut out))
        .is_none()
    {
        return Err(Unavailable::RdramUnmappable);
    }
    Ok(out)
}

/// Split a big-endian command stream into individual commands.
///
/// Uses `rustyn64_rdp::command::command_len_words`, the same table the software
/// RDP consumes the FIFO with, so the two paths cannot disagree about where one
/// command ends and the next begins. A trailing partial command is dropped, as
/// `Rdp::tick_with_bus` also refuses to consume one.
fn split_commands(cmds: &[u8]) -> Vec<Vec<u32>> {
    /// `command_len_words` counts 64-bit RDP command doublewords; the shim takes
    /// 32-bit words.
    const U32_PER_DOUBLEWORD: usize = 2;

    let words: Vec<u32> = cmds
        .chunks_exact(4)
        .map(|c| u32::from_be_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < words.len() {
        let opcode = rustyn64_rdp::command::opcode_of(words[i]);
        let len = rustyn64_rdp::command::command_len_words(opcode) as usize * U32_PER_DOUBLEWORD;
        if len == 0 || i + len > words.len() {
            break;
        }
        out.push(words[i..i + len].to_vec());
        i += len;
    }
    out
}

/// How one vector fared on each path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Verdict {
    /// The software rasterizer reproduced the Angrylion golden.
    pub software_matches: bool,
    /// The GPU backend reproduced the Angrylion golden.
    pub gpu_matches: bool,
}

/// Grade one vector on both paths against the Angrylion golden.
///
/// # Errors
///
/// Returns [`Unavailable`] when the GPU replay could not run at all, so a
/// caller can report "not verified" rather than "passed".
///
/// # Panics
///
/// Panics if `bytes` is not a well-formed `.rvec` container.
pub fn grade(bytes: &[u8]) -> Result<Verdict, Unavailable> {
    let v = crate::conformance::parse(bytes);
    let software = crate::conformance::replay(&v);
    let gpu = replay(&v)?;
    Ok(Verdict {
        software_matches: software == v.golden_fb,
        gpu_matches: gpu == v.golden_fb,
    })
}

/// The result of grading the whole registered corpus.
#[derive(Debug, Default, Clone)]
pub struct Census {
    /// Vectors where both paths match the golden.
    pub both: Vec<&'static str>,
    /// Vectors only the software rasterizer gets right — a GPU-side defect.
    pub software_only: Vec<&'static str>,
    /// Vectors only the GPU gets right — a software-rasterizer gap.
    pub gpu_only: Vec<&'static str>,
    /// Vectors neither path reproduces.
    pub neither: Vec<&'static str>,
}

impl Census {
    /// Total vectors graded.
    #[must_use]
    pub fn total(&self) -> usize {
        self.both.len() + self.software_only.len() + self.gpu_only.len() + self.neither.len()
    }
}

/// Grade every registered vector on both paths.
///
/// One `GpuRdp` per vector, deliberately: a shared context would carry TMEM,
/// tile state and combiner state from one vector into the next, and a vector
/// that only passes because its predecessor happened to leave the right state
/// behind is not evidence of anything. It costs a device init per vector and
/// buys independence.
///
/// # Errors
///
/// Returns [`Unavailable`] if the GPU backend is unusable, without grading
/// anything — a partial census read as a complete one would be exactly the
/// vacuous pass this corpus exists to prevent.
pub fn census() -> Result<Census, Unavailable> {
    let mut c = Census::default();
    for (name, bytes) in crate::conformance::RDP_VECTORS {
        let v = grade(bytes)?;
        match (v.software_matches, v.gpu_matches) {
            (true, true) => c.both.push(name),
            (true, false) => c.software_only.push(name),
            (false, true) => c.gpu_only.push(name),
            (false, false) => c.neither.push(name),
        }
    }
    Ok(c)
}

#[cfg(test)]
mod tests {
    use super::swap_words_into;

    /// The swap must be its own inverse, which is what lets one function serve
    /// both directions.
    ///
    /// Mutation-checked: changing any one of the four index assignments in
    /// `swap_words_into` fails this.
    #[test]
    fn word_swap_is_its_own_inverse() {
        let src: Vec<u8> = (0u8..=251).collect();
        let mut once = vec![0u8; src.len()];
        let mut twice = vec![0u8; src.len()];
        swap_words_into(&mut once, &src);
        swap_words_into(&mut twice, &once);
        assert_eq!(twice, src, "the word swap is not an involution");
        assert_ne!(once, src, "the word swap did nothing");
    }

    /// Pin the exact permutation, not just that it round-trips — a swap that
    /// reversed 8-byte groups would also be an involution.
    #[test]
    fn word_swap_reverses_each_aligned_four_bytes() {
        let mut out = [0u8; 8];
        swap_words_into(&mut out, &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(out, [4, 3, 2, 1, 8, 7, 6, 5]);
    }
}
