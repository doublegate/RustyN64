//! The Bus owns everything mutable.
//!
//! RDRAM (main work RAM), the RSP, the RDP, the AI (audio), the cart (→ PI), the
//! controllers (→ SI), and the RCP interface register blocks
//! (SP / DP / VI / AI / PI / SI / RI / MI). The CPU borrows `&mut Bus` during
//! `tick()`. The RDP and AI see narrower bus traits
//! ([`rustyn64_rdp::VideoBus`], [`rustyn64_audio::AudioBus`]) which the Bus
//! implements. See `docs/architecture.md` (the load-bearing facts).
//!
//! Per the `TetaNES` postmortem (carried over from `RustyNES`): one owner for
//! all mutable state avoids the "CPU holds the RSP/RDP, but they also need the
//! CPU's memory bus"
//! borrow-checker fight. Each chip sees only the smaller trait it actually needs.

// The MI interrupt block is a row of orthogonal hardware-latch booleans that map
// 1:1 to real RCP IRQ lines; collapsing them into an enum would obscure the model.
#![allow(clippy::struct_excessive_bools)]
// Address math truncates by design when narrowing 32-bit physical addresses.
#![allow(clippy::cast_possible_truncation)]

use rustyn64_audio::{AiIrq, Audio, AudioBus, StereoSample};
use rustyn64_cart::{Cart, Cartridge, RdramBus};
use rustyn64_cpu::Bus as CpuBus;
use rustyn64_rdp::{Rdp, VideoBus};
use rustyn64_rsp::Rsp;
use serde::{Deserialize, Serialize};

use crate::vi::{self, Vi};

/// Expand a 5-bit color channel to 8 bits, replicating the high bits into the
/// low so 0x1F maps to 0xFF (not 0xF8) — the standard RGBA5551 → RGBA8 widening.
/// Masks to 5 bits first, so an out-of-range argument cannot overflow the shift.
const fn expand5(v5: u8) -> u8 {
    let v = v5 & 0x1F;
    (v << 3) | (v >> 2)
}

/// Convert a logical RGBA5551 pixel to the VI's RGB8, using the **truncating**
/// widening the VI hardware applies (the 5-bit field sits at the top of the byte
/// with the low bits zero — *not* `expand5`'s high-bit replication). Alpha (bit 0,
/// coverage) is not carried; the scan-out sets an opaque display alpha. Ledger R-5.
const fn vi_rgb5551(px: u16) -> [u8; 3] {
    [
        ((px >> 8) & 0xF8) as u8,
        ((px & 0x07C0) >> 3) as u8,
        ((px & 0x003E) << 2) as u8,
    ]
}

/// The VI's 5-bit bilinear lerp, per channel: `a + (((b - a) * frac + 16) >> 5)`
/// (`frac` a 5-bit weight 0..=31, rounding bias `+16` before the `>> 5`). A faithful
/// port of Angrylion `vi_vl_lerp` (`vi/lerp.c`). Ledger R-5.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn vi_lerp3(a: [u8; 3], b: [u8; 3], frac: i32) -> [u8; 3] {
    let mut o = [0u8; 3];
    for i in 0..3 {
        let (ai, bi) = (i32::from(a[i]), i32::from(b[i]));
        o[i] = (ai + (((bi - ai) * frac + 16) >> 5)) as u8;
    }
    o
}

/// The register-derived rules a scan-out filters every source pixel under.
///
/// Fixed for the whole of one [`Bus::scanout_scaled`] call, which is what makes a memo
/// keyed on `(x, y)` alone sound.
#[derive(Clone, Copy)]
struct ViCfg {
    /// `VI_ORIGIN`, the framebuffer base.
    origin: u32,
    /// `VI_WIDTH`, source pixels per row.
    src_stride: i32,
    /// 2 (RGBA5551) or 4 (RGBA8888).
    bpp: u32,
    /// `VI_CTRL` bits 9:8. Only 0 and 1 run the coverage filters.
    aa_mode: u32,
    /// `VI_CTRL` bit 4 — the 3-tap median on partial-coverage edges.
    divot: bool,
    /// `VI_CTRL` bit 16 — the de-dither restore on fully-covered pixels.
    dither_filter: bool,
}

/// One scan-out's source-sampling configuration, plus a two-row memo of source
/// pixels that have already been through the coverage filters.
///
/// The configuration and the memo live in **one** value on purpose. A cache keyed on
/// `(x, y)` alone is only sound while `origin`, `src_stride`, `bpp`, and the filter
/// flags are fixed, and pairing a memo with the wrong configuration would return a
/// pixel filtered under different rules — a corruption no test would localize. Making
/// them inseparable removes the possibility rather than asserting against it.
///
/// **Why memoizing is behavior-identical by construction.** [`Bus::vi_sample`] is a
/// pure function of RDRAM and the fields here; [`Bus::scanout_scaled`] takes `&self`,
/// so RDRAM cannot change while one scan-out runs. Two calls with the same `(x, y)`
/// therefore cannot disagree, and returning the first answer for the second call is
/// not an approximation.
///
/// **On the per-scan-out allocation.** `cells` is one heap allocation per frame — at
/// most 32 KB, and about 5 KB at the resolutions a real title programs. It is not
/// hoisted into `Bus` because `scanout_scaled` takes `&self`: caching there would mean
/// interior mutability in the type whose field layout *is* the save-state format
/// (ADR 0005), which is a far larger cost than one allocation. Against the ~7.8 ms of
/// filtering the memo replaces, the allocation does not appear in a profile.
///
/// **Why it is worth having.** The scan-out samples each source pixel about three
/// times: at `x_add = 512` (a 2x upscale, what Super Mario 64 programs) an even output
/// pixel samples column `sx`, the odd one samples `sx` and `sx + 1`, so every column is
/// asked for twice as a near sample and once as a far one. Each of those calls runs the
/// whole filter chain — under `aa_mode` 0 with `divot` and `dither_filter` set, three
/// divot taps of nine de-dither taps each, 27 [`Bus::vi_read_cov`] calls of three
/// `rdram_offset` lookups apiece.
struct ViSampler {
    /// The register-derived rules every sample is filtered under.
    cfg: ViCfg,
    /// First source column the memo covers.
    x_lo: i32,
    /// Columns per memo row.
    span: usize,
    /// The source row each memo row holds; `None` marks an unused row.
    ///
    /// An `Option` rather than a sentinel because a sentinel is a value the domain
    /// might one day contain, and a collision would return another row's pixels
    /// without invalidating anything.
    row_y: [Option<i32>; 2],
    /// `2 * span` filtered pixels, row-major. `None` is "not computed yet".
    cells: alloc::vec::Vec<Option<[u8; 3]>>,
}

impl ViSampler {
    /// Two rows is exactly what the vertical lerp needs: it samples `sy` and
    /// `sy + 1`, and the walk over output rows only ever moves `sy` forward.
    const ROWS: usize = 2;

    /// The widest memo this will allocate, in source columns per row.
    ///
    /// The real bound is much smaller — `VI_X_SCALE` fields are 12 bits and the
    /// output is clamped to a 640-pixel prescale line, so the walk cannot ask for
    /// more than about 3,000 columns — but that argument lives a hundred lines away
    /// in `scanout_scaled`'s register decode and would not survive someone relaxing a
    /// clamp. `x_lo`/`x_hi` are ultimately guest-controlled through VI MMIO, and an
    /// allocation sized by guest registers deserves a bound stated where the
    /// allocation happens. Past the cap the memo is simply empty and every sample
    /// takes the uncached path: slower, never wrong.
    const MAX_SPAN: usize = 4096;

    /// Build a memo covering source columns `x_lo..=x_hi` inclusive.
    fn new(cfg: ViCfg, x_lo: i32, x_hi: i32) -> Self {
        // `i64` throughout: the subtraction is on guest-derived values, and a signed
        // overflow here would be a debug-build panic in a scan-out path.
        // An empty or inverted range (`x_hi < x_lo`) and an over-wide one are the same
        // outcome — no memo — but they are written as one explicit match so neither
        // reads as an accident of `try_from` failing on a negative.
        let columns = i64::from(x_hi) - i64::from(x_lo) + 1;
        let span = match usize::try_from(columns) {
            Ok(want) if want <= Self::MAX_SPAN => want,
            _ => 0,
        };
        Self {
            cfg,
            x_lo,
            span,
            row_y: [None; Self::ROWS],
            cells: alloc::vec![None; span * Self::ROWS],
        }
    }

    /// The memo row holding source row `y`, evicting if neither does.
    ///
    /// Eviction takes the row with the smaller `y`: the scan-out walks `y` forward
    /// (`y_add` is unsigned), so the lower row is the one that will not be asked for
    /// again. A wrong choice here would only cost hit rate, never correctness.
    ///
    /// The choice is written as an explicit match rather than as a comparison of two
    /// `Option`s, because the rule it encodes — unused rows first, then the row
    /// further behind the walk — should be readable without knowing that `None` sorts
    /// below every `Some`.
    fn row_slot(&mut self, y: i32) -> usize {
        if self.row_y[0] == Some(y) {
            return 0;
        }
        if self.row_y[1] == Some(y) {
            return 1;
        }
        // Written out rather than leaning on `Option`'s derived `Ord`: an unused row
        // goes first, then the row further behind the forward walk.
        let victim = match (self.row_y[0], self.row_y[1]) {
            (None, _) => 0,
            (_, None) => 1,
            (Some(y0), Some(y1)) if y1 < y0 => 1,
            (Some(_), Some(_)) => 0,
        };
        self.row_y[victim] = Some(y);
        let base = victim * self.span;
        self.cells[base..base + self.span].fill(None);
        victim
    }
}

/// The VI's integer square root (Angrylion `vi_integer_sqrt`), used to build the
/// gamma curve. A restoring square-root: `res` accumulates the root two bits at a
/// time from the top. Ledger R-5.
const fn vi_integer_sqrt(a: u32) -> u32 {
    let mut op = a;
    let mut res = 0u32;
    let mut one = 1u32 << 30;
    while one > op {
        one >>= 2;
    }
    while one != 0 {
        if op >= res + one {
            op -= res + one;
            res += one << 1;
        }
        res >>= 1;
        one >>= 2;
    }
    res
}

/// The VI AA-edge filter's penultimate min/max over a channel's gathered values
/// (Angrylion `video_max_optimized`). Returns `(penumin, penumax)` — a specific
/// single-pass "runner-up" min/max, *not* a plain second-smallest/largest: it tracks
/// the current min/max position and its predecessor, then refines the runner-up with a
/// second partial scan. Ported verbatim so the tie-handling matches. Ledger R-5.
fn vi_video_max(pixels: &[u32]) -> (u32, u32) {
    debug_assert!(
        !pixels.is_empty(),
        "vi_video_max needs at least the center pixel"
    );
    let n = pixels.len();
    let (mut posmax, mut posmin) = (0usize, 0usize);
    let (mut curpenmax, mut curpenmin) = (pixels[0], pixels[0]);
    for i in 1..n {
        if pixels[i] > pixels[posmax] {
            curpenmax = pixels[posmax];
            posmax = i;
        } else if pixels[i] < pixels[posmin] {
            curpenmin = pixels[posmin];
            posmin = i;
        }
    }
    if curpenmax != pixels[posmax] {
        for &p in &pixels[posmax + 1..] {
            if p > curpenmax {
                curpenmax = p;
            }
        }
    }
    if curpenmin != pixels[posmin] {
        for &p in &pixels[posmin + 1..] {
            if p < curpenmin {
                curpenmin = p;
            }
        }
    }
    (curpenmin, curpenmax)
}

/// The VI gamma curve for one channel: `sqrt(v << 6) << 1` (Angrylion `gamma_table`,
/// `vi_gamma_init`). Applied when `gamma_enable` is set and `gamma_dither` is not
/// (the dithered variants are noise-based and deferred). Ledger R-5.
#[allow(clippy::cast_possible_truncation)]
const fn vi_gamma(v: u8) -> u8 {
    (vi_integer_sqrt((v as u32) << 6) << 1) as u8
}

/// The 256-entry VI gamma lookup table, built at compile time from [`vi_gamma`]
/// (Angrylion's `gamma_table`) — a table lookup per channel on the scan-out path
/// instead of recomputing the integer square root per pixel.
const GAMMA_TABLE: [u8; 256] = {
    let mut t = [0u8; 256];
    let mut i = 0usize;
    while i < 256 {
        t[i] = vi_gamma(i as u8);
        i += 1;
    }
    t
};

/// Base RDRAM size: 4 MiB (8 MiB with the Expansion Pak installed).
pub const RDRAM_SIZE: usize = 8 * 1024 * 1024;

/// Granularity of the RDRAM dirty-page map, in bytes.
///
/// 4 KiB gives 2,048 pages for 8 MiB of RDRAM — a 2 KiB flag array, L1-resident,
/// so the once-a-frame scan is free. Finer pages would send less redundant data
/// but grow the scan; coarser would send more. Not tuned: 4 KiB is also the
/// alignment `VK_EXT_external_memory_host` wants, so the two agree by
/// construction rather than by coincidence.
pub const RDRAM_PAGE: usize = 4096;

/// `log2(RDRAM_PAGE)`, so the hot-path mark is a shift rather than a divide.
const RDRAM_PAGE_SHIFT: usize = 12;

const _: () = assert!(1 << RDRAM_PAGE_SHIFT == RDRAM_PAGE);

/// A dirty map with every page set, for construction **and for `serde`**.
///
/// All-dirty is the only correct starting state in both cases: at power-on the
/// consumer has never seen this RDRAM, and loading a save-state replaces all of
/// it at once. Coming back clean would present the previous state's framebuffer
/// until something happened to overwrite it.
#[cfg(feature = "rdp-tap")]
fn all_pages_dirty() -> alloc::boxed::Box<[bool]> {
    alloc::vec![true; RDRAM_SIZE.div_ceil(RDRAM_PAGE)].into_boxed_slice()
}

/// The RCP MIPS-interface (MI) interrupt lines.
///
/// Each bit, when set and unmasked (via [`RcpRegs::mi_mask`]), drives the VR4300
/// IP2 interrupt. The mask register is implemented (`MI_MASK` set/clear pairs; it
/// gates the IP2 line).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MiInterrupt {
    /// SP (RSP) interrupt.
    pub sp: bool,
    /// SI (serial / PIF) interrupt.
    pub si: bool,
    /// AI (audio-buffer-done) interrupt.
    pub ai: bool,
    /// VI (vertical-blank) interrupt.
    pub vi: bool,
    /// PI (peripheral DMA-done) interrupt.
    pub pi: bool,
    /// DP (RDP-done) interrupt.
    pub dp: bool,
}

/// Pack the six interrupt lines into their register bit order.
///
/// `MI_INTERRUPT` and `MI_MASK` share it, which is why one packer serves both.
const fn pack_mi(l: MiInterrupt) -> u32 {
    (l.sp as u32)
        | ((l.si as u32) << 1)
        | ((l.ai as u32) << 2)
        | ((l.vi as u32) << 3)
        | ((l.pi as u32) << 4)
        | ((l.dp as u32) << 5)
}

impl MiInterrupt {
    /// `true` if any interrupt line is asserted.
    #[must_use]
    pub const fn any(self) -> bool {
        self.sp || self.si || self.ai || self.vi || self.pi || self.dp
    }
}

/// The RCP interface register state.
///
/// The SP / DP / VI / AI / PI / SI / RI / MI register blocks the CPU memory-maps
/// in `$0400_0000..$04FF_FFFF`. Skeleton: each is a placeholder for its real
/// register set (a roadmap phase).
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct RcpRegs {
    /// MI — MIPS interface (interrupt lines + mask + RCP version).
    pub mi_intr: MiInterrupt,
    /// MI interrupt mask (a line drives IP2 only when masked-in).
    pub mi_mask: MiInterrupt,
    /// `MI_MODE`'s storage bits (the repeat count and flags).
    pub mi_mode: u32,
    /// The RI (RDRAM controller) register file, `0x0470_0000..0x0470_0020`:
    /// `RI_MODE`, `RI_CONFIG`, `RI_CURRENT_LOAD`, `RI_SELECT`, `RI_REFRESH`,
    /// `RI_LATENCY`, `RI_ERROR`, `RI_BANK_STATUS` (N64brew *RDRAM Interface*
    /// §Registers).
    ///
    /// Plain storage — writes stick and reads return them. That is enough for the
    /// one thing that actually depends on it today: the cartridge's IPL3 reads
    /// `RI_SELECT` and branches on whether RDRAM has already been brought up
    /// (ledger R-18). The documented read *oddities* are deliberately NOT modeled
    /// (see R-22): `RI_CURRENT_LOAD` is write-only on hardware and its read
    /// returns a collection of bits from other registers, and `RI_ERROR` /
    /// `RI_BANK_STATUS` reflect controller state rather than the last write.
    /// Nothing exercises those yet and there is no oracle for them — the suite has
    /// no RI group — so they stay honest storage rather than invented behavior.
    pub ri: [u32; 8],
    // The SP, DP (DPC), VI, AI, PI, SI, MI, and RI register blocks are all decoded
    // (see the `is_*_register` methods + the read/write dispatch). Still undecoded:
    // the RDRAM-config registers (`0x03F0_0000`) — the per-chip Rambus device
    // registers, distinct from the RI controller block above. n64-systemtest has no
    // RI/RDRAM-register group, so neither has a suite oracle; they are validated by
    // commercial-boot progress (ledger R-18) instead.
}

/// Everything mutable lives here — the single owner.
#[derive(Serialize, Deserialize)]
pub struct Bus {
    /// Main system RDRAM (boxed slice: 8 MiB, heap-allocated without a stack
    /// temporary).
    pub rdram: alloc::boxed::Box<[u8]>,
    /// The RDRAM "hidden" bits: the 9th bit RDRAM carries per byte, used by the
    /// RDP Z-buffer for the low 2 bits of each pixel's `dz`. Two bits per 16-bit
    /// halfword, **bit-packed** four halfwords to a byte (`RDRAM_SIZE / 8` = 1 MiB
    /// for 8 MiB of RDRAM). Lazily allocated — `None` until the first hidden write,
    /// since only Z-buffered rendering touches it (reads return 0, matching the
    /// power-on state).
    pub rdram_hidden: Option<alloc::boxed::Box<[u8]>>,
    /// Every RDP command word this Bus has fed to the RDP since the last drain
    /// (ADR 0014's tap), appended whole-command in FIFO order.
    ///
    /// Exists so an out-of-core rasterizer can be handed the *same* stream the
    /// software RDP consumed. It has to be a tap rather than a re-read of
    /// RDRAM: by the time anything outside the core could look, `DPC_CURRENT`
    /// has reached `DPC_END` and the game has usually overwritten the buffer.
    ///
    /// `#[serde(skip)]` on purpose, and that is not a shortcut. This is
    /// per-frame scratch that the consumer drains, so it carries no state a
    /// save-state needs; skipping it also keeps the snapshot layout **identical**
    /// with the feature on or off, which is what keeps this out of ADR 0005's
    /// announced-in-advance format-break territory.
    ///
    /// **Private.** The only supported access is [`Bus::take_rdp_commands`],
    /// because the invariant worth protecting is that a consumer takes the whole
    /// stream: a partial drain would replay this frame's tail on top of the next
    /// frame's commands, and reordering would submit a command list the machine
    /// never issued.
    #[cfg(feature = "rdp-tap")]
    #[serde(skip)]
    rdp_tap: alloc::vec::Vec<u32>,
    /// One flag per [`RDRAM_PAGE`]-sized page, set when anything writes RDRAM.
    ///
    /// Lets an out-of-core rasterizer upload only what changed instead of all
    /// 8 MiB every frame — measured at ~2.1 ms of the ~2.4 ms a GPU frame costs.
    ///
    /// **`bool`, not a bitset, and that is the hot-path decision.** Marking is on
    /// every RDRAM store, so it must be a single store; a bitset would make it a
    /// read-modify-write. The whole array is 2 KiB for 8 MiB of RDRAM, so it sits
    /// in L1 either way, and the once-a-frame scan is nothing against the copy it
    /// replaces.
    ///
    /// `#[serde(skip)]` like the tap: it is per-frame scratch, and skipping keeps
    /// the save-state layout identical with the feature on or off.
    ///
    /// **`default` is not optional here.** `#[serde(skip)]` alone fills the field
    /// from `Default`, and `Box<[bool]>::default()` is **empty** — so the first
    /// RDRAM store after loading a save-state would index page `off >> 12` into a
    /// zero-length slice and panic. `rdp_tap` survives the same attribute only
    /// because `Vec::default()` is empty *and pushable*; an indexed map is not.
    #[cfg(feature = "rdp-tap")]
    #[serde(skip, default = "all_pages_dirty")]
    rdram_dirty: alloc::boxed::Box<[bool]>,
    /// The PI DMA engine (T-14-001), pulled forward from Phase 5 because
    /// n64-systemtest loads the rest of its own ELF through it.
    pub pi: rustyn64_cart::pi::Pi,
    // DMEM and IMEM are **not** here: the RSP owns them (`Bus::rsp`), and this
    // Bus reaches them through `Rsp::mem_read`/`mem_write`. They were a separate
    // `spmem` slice on the Bus while the RSP was a stub, which meant the CPU and
    // the RSP addressed two different memories that happened to start equal.
    /// The `ISViewer` buffer, as guest-visible memory.
    isviewer: alloc::boxed::Box<[u8]>,
    /// Text the guest has flushed through the `ISViewer` channel.
    isviewer_out: alloc::vec::Vec<u8>,
    /// Text the guest has pushed through the **EMUX** `xlog` channel.
    ///
    /// Kept separate from [`Bus::isviewer_output`] deliberately: they are two
    /// independent console paths and n64-systemtest picks whichever the
    /// emulator advertises, so merging them would hide which one is live.
    emux_out: alloc::vec::Vec<u8>,
    /// Set once the guest has issued `EMUX xioctl(EXIT)`.
    emux_exited: bool,
    /// Whether this host advertises the EMUX extensions. **Off by default**:
    /// hardware has none, and offering them changes the guest's control flow.
    emux_enabled: bool,
    /// The value a PI direct-I/O write latched, visible to every PI-bus read
    /// until the write finalizes. See [`Bus::pi_tick`].
    pi_write_latch: u32,
    /// RCP cycles remaining before the latched PI write finalizes. Zero is idle.
    pi_write_countdown: u32,
    /// `SI_DRAM_ADDR` — the RDRAM side of a PIF-RAM SI DMA.
    si_dram_addr: u32,
    /// Set when the PIF's real-PIF boot checksum verify fails: the real PIF
    /// freezes the CPU via NMI until power-off (`PIF-NUS.md` §Console startup).
    /// The scheduler stops stepping the CPU once this latches. Only the real-PIF
    /// path can set it (a genuine ROM matches and never does); always `false`
    /// under HLE and in normal operation.
    boot_nmi_halt: bool,
    /// The RSP coprocessor.
    pub rsp: Rsp,
    /// The RDP rasterizer.
    pub rdp: Rdp,
    /// The Video Interface register file (`0x0440_0000`). Scan-out and the
    /// scheduler-driven scan position are follow-up VI tickets.
    pub vi: Vi,
    /// The Audio Interface.
    pub audio: Audio,
    /// The cartridge (PI/SI + saves).
    pub cart: Cart,
    /// The RCP interface register state.
    pub rcp: RcpRegs,
    /// Controller button/stick state, 4 ports (latched by the SI joybus).
    pub controllers: [u32; 4],
    /// Count of RCP chip-steps taken (diagnostic; used by the scheduler test).
    rcp_steps: u64,
}

impl core::fmt::Debug for Bus {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Bus")
            .field("rsp", &self.rsp)
            .field("rdp", &self.rdp)
            .field("audio", &self.audio)
            .field("cart", &self.cart)
            .field("rcp", &self.rcp)
            .field("controllers", &self.controllers)
            .finish_non_exhaustive()
    }
}

impl Default for Bus {
    fn default() -> Self {
        Self {
            // `vec![..].into_boxed_slice()` allocates straight on the heap —
            // no 8 MiB stack temporary (which `Box::new([0; N])` would create).
            pi: rustyn64_cart::pi::Pi::new(),
            isviewer: alloc::vec![0u8; 0x20 + Self::ISVIEWER_LEN].into_boxed_slice(),
            isviewer_out: alloc::vec::Vec::new(),
            emux_out: alloc::vec::Vec::new(),
            emux_exited: false,
            emux_enabled: false,
            pi_write_latch: 0,
            pi_write_countdown: 0,
            si_dram_addr: 0,
            boot_nmi_halt: false,
            rdram: alloc::vec![0u8; RDRAM_SIZE].into_boxed_slice(),
            rdram_hidden: None,
            #[cfg(feature = "rdp-tap")]
            rdp_tap: alloc::vec::Vec::new(),
            // Every page starts dirty: the consumer has never seen this RDRAM, so
            // the first upload must be complete. Starting clean would hand the GPU
            // an empty framebuffer and whatever the allocator left behind.
            #[cfg(feature = "rdp-tap")]
            rdram_dirty: all_pages_dirty(),
            rsp: Rsp::new(),
            rdp: Rdp::new(),
            vi: Vi::new(),
            audio: Audio::new(),
            cart: Cart::new(),
            rcp: RcpRegs::default(),
            controllers: [0; 4],
            rcp_steps: 0,
        }
    }
}

impl Bus {
    /// Construct at power-on.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Advance the PI's asynchronous write by one RCP cycle.
    ///
    /// # Why a PI write is not immediate
    ///
    /// From N64brew *Memory map* (PI external bus):
    ///
    /// > All writes are performed **asynchronously** by the PI. Making a write
    /// > in this area will in fact just cause the PI to latch the value
    /// > internally, and release the VR4300 immediately. The write will then
    /// > happen in background. [...] While a write is ongoing, further writes
    /// > are ignored, and reads (from any address) return the 32-bit value that
    /// > is being written.
    ///
    /// The PI does not know a device is read-only, so a write into ROM follows
    /// the same path and is simply dropped by the ROM — which is why a value
    /// written to cart ROM is briefly readable and then gone.
    ///
    /// # The duration is bounded by the oracle, not derived from hardware
    ///
    /// How long finalization takes depends on the PI domain timing registers
    /// (`LAT`/`PWD`/`PGS`/`RLS`), which are not modeled. n64-systemtest bounds
    /// it only *relatively*: the latched value must still be visible after 0
    /// loop iterations and gone after 110. [`Bus::PI_WRITE_CYCLES`] sits inside
    /// those bounds; it is **not** a hardware measurement. Accuracy ledger C-9.
    pub const fn pi_tick(&mut self) {
        if self.pi_write_countdown > 0 {
            self.pi_write_countdown -= 1;
        }
    }

    /// Is a PI direct-I/O write still in flight?
    const fn pi_io_busy(&self) -> bool {
        self.pi_write_countdown > 0
    }

    /// Step the RSP.
    ///
    /// The chip stays **in place**. It used to be moved out with
    /// `core::mem::take` so that `Rsp::tick` could borrow the Bus, under a
    /// comment asserting "No allocation" — which was false: `take` needs
    /// `Default`, and constructing an `Rsp` allocates DMEM and IMEM, so every
    /// RCP step allocated and freed 8 KiB. `Rsp::tick` now *returns* what it
    /// wants done instead of borrowing its owner, so there is nothing to move.
    pub fn rsp_tick(&mut self) {
        let out = self.rsp.tick();
        if let Some(raise) = out.interrupt_change {
            self.rcp.mi_intr.sp = raise;
        }
        if let Some(dma) = out.dma {
            self.sp_dma(dma);
        }
        if let Some((off, val)) = out.dp_write {
            // The RSP's COP0 `c8`–`c15` *are* the RDP command registers; the RSP
            // crate cannot name `Rdp` (crate-graph rule), so it reports the write
            // as a DPC word offset and the Bus carries it out — the same seam the
            // CPU uses at `0x0410_0000`. This is how the rdpq microcode's
            // `mtc0 DP_END` submits a command list to the RDP.
            self.rdp.dpc_write(u32::from(off), val);
        }
        self.rcp_steps = self.rcp_steps.wrapping_add(1);
    }

    /// Step the RDP against this bus's narrow [`VideoBus`] view (split-borrow).
    ///
    /// The `take` is how the RDP borrows its owner, and it is not free — it reads the
    /// whole struct out, writes a fresh `Default` into the vacated slot, and the restore
    /// overwrites that. It used to happen on **every RCP step**; the measurements are in
    /// `docs/performance.md` §"The Bus split-borrow moves 1.35 GB a frame".
    ///
    /// So the step's bus-free half runs first. On most steps the RDP is frozen,
    /// stalling, or looking at an empty command FIFO, and answers the whole step from
    /// its own fields — in which case nothing is moved at all. The predicate lives in
    /// [`rustyn64_rdp::Rdp::tick_without_bus`] beside the early-outs it encodes, not
    /// here, so it cannot drift away from them, and it hands back a
    /// [`rustyn64_rdp::NeedsBus`] token that the bus half requires — so the two cannot
    /// be called out of order.
    pub fn rdp_tick(&mut self) {
        let Some(proof) = self.rdp.tick_without_bus() else {
            return;
        };
        let mut rdp = core::mem::take(&mut self.rdp);
        #[cfg(feature = "rdp-tap")]
        let before = rdp.cmd_current;
        rdp.tick_with_bus(proof, self);
        // Capture what was actually consumed, by DIFFING the FIFO pointer rather
        // than decoding the command again here. `tick_with_bus` already knows the
        // opcode's length and already refuses a command that is only partly
        // written; re-deriving either would be the same rule written twice, free
        // to drift, and a tap that disagrees with the RDP about where one command
        // ends is worse than no tap. An unconsumed step leaves the pointer put and
        // captures nothing.
        #[cfg(feature = "rdp-tap")]
        {
            // Iterate a COUNT, not `while addr < after`. `cmd_current` is masked
            // to `DPC_ADDR_MASK` (0x00FF_FFF8) so it cannot reach the top of the
            // address space today, and the comparison form would therefore never
            // wrap — but "cannot happen because of a mask three files away" is a
            // reason to write the loop that does not need the argument. A count
            // terminates whatever the addresses are.
            let words = rdp.cmd_current.wrapping_sub(before) / 4;
            for i in 0..words {
                self.rdp_tap
                    .push(self.rdram_read_u32(before.wrapping_add(i * 4)));
            }
        }
        self.rdp = rdp;
    }

    /// Drain the RDP command tap, leaving it empty.
    ///
    /// The consumer is expected to call this once per frame. Nothing bounds the
    /// buffer otherwise, and a consumer that stops draining would grow it without
    /// limit — which is the caller's problem to have loudly rather than this
    /// Bus's to hide by silently dropping the oldest commands.
    #[cfg(feature = "rdp-tap")]
    pub fn take_rdp_commands(&mut self) -> alloc::vec::Vec<u32> {
        core::mem::take(&mut self.rdp_tap)
    }

    /// Mark the page containing byte offset `off` as written.
    ///
    /// Compiles to nothing without `rdp-tap`, so a default build pays no hot-path
    /// cost at all — which is the only reason it is acceptable to call this from
    /// every RDRAM store.
    ///
    /// `off` is an **RDRAM byte offset**, not an address: every caller obtains it
    /// from [`Bus::rdram_offset`], which returns `Option<usize>` and yields
    /// `Some` only below [`RDRAM_SIZE`]. So the index below cannot go out of
    /// bounds, and the `debug_assert` says which fact that rests on rather than
    /// leaving a reader to rediscover it. A clamp here would be worse than the
    /// panic: it would silently mark the wrong page and stage the wrong bytes.
    #[allow(
        clippy::inline_always,
        reason = "called from every RDRAM store; a call here would cost more than the mark"
    )]
    #[cfg_attr(
        not(feature = "rdp-tap"),
        allow(
            clippy::unused_self,
            clippy::missing_const_for_fn,
            clippy::needless_pass_by_ref_mut,
            reason = "the body is empty without the feature; the signature stays uniform so the six \
                      call sites need no cfg of their own"
        )
    )]
    #[inline(always)]
    fn mark_rdram_dirty(&mut self, off: usize) {
        #[cfg(feature = "rdp-tap")]
        {
            debug_assert!(
                off < RDRAM_SIZE,
                "mark_rdram_dirty takes an RDRAM offset from rdram_offset, not an address"
            );
            self.rdram_dirty[off >> RDRAM_PAGE_SHIFT] = true;
        }
        #[cfg(not(feature = "rdp-tap"))]
        {
            let _ = off;
        }
    }

    /// As [`Bus::mark_rdram_dirty`], for a write of `len` bytes that may straddle
    /// a page boundary. The `u32` store does; the byte stores cannot.
    #[allow(
        clippy::inline_always,
        reason = "called from the u32 store fast path; see mark_rdram_dirty"
    )]
    #[cfg_attr(
        not(feature = "rdp-tap"),
        allow(
            clippy::unused_self,
            clippy::missing_const_for_fn,
            clippy::needless_pass_by_ref_mut,
            reason = "the body is empty without the feature; the signature stays uniform so the six \
                      call sites need no cfg of their own"
        )
    )]
    #[inline(always)]
    fn mark_rdram_dirty_range(&mut self, off: usize, len: usize) {
        #[cfg(feature = "rdp-tap")]
        {
            if len == 0 {
                return;
            }
            // Saturating: `off + len` would wrap for an absurd `len`, and a
            // debug-only panic in a marking helper is the worst of both worlds —
            // it fires in tests and silently marks page 0 in release. Saturation
            // clamps to the last page instead, which over-marks (costing one
            // extra page of staging) rather than under-marking (costing a stale
            // frame). Every real caller passes 1 or 4.
            let first = off >> RDRAM_PAGE_SHIFT;
            let last = off.saturating_add(len - 1) >> RDRAM_PAGE_SHIFT;
            for p in first..=last.min(self.rdram_dirty.len() - 1) {
                self.rdram_dirty[p] = true;
            }
        }
        #[cfg(not(feature = "rdp-tap"))]
        {
            let _ = (off, len);
        }
    }

    /// The per-page dirty flags, for a consumer staging RDRAM elsewhere.
    #[cfg(feature = "rdp-tap")]
    #[must_use]
    pub fn rdram_dirty_pages(&self) -> &[bool] {
        &self.rdram_dirty
    }

    /// Clear every dirty flag. The consumer calls this once it has staged them.
    ///
    /// Separate from [`Bus::rdram_dirty_pages`] on purpose: a consumer that
    /// failed part-way through must be able to leave the flags set so the next
    /// attempt re-sends what it missed, rather than losing the pages to a
    /// read-and-clear it could not complete.
    #[cfg(feature = "rdp-tap")]
    pub fn clear_rdram_dirty(&mut self) {
        self.rdram_dirty.fill(false);
    }

    /// How many command words are waiting in the tap.
    ///
    /// For observation only — a test proving the tap filled before checking that
    /// something drained it needs to look without consuming, and
    /// [`Bus::take_rdp_commands`] would make that check its own answer.
    #[cfg(feature = "rdp-tap")]
    #[must_use]
    pub const fn rdp_tap_len(&self) -> usize {
        self.rdp_tap.len()
    }

    /// Step the AI against this bus's narrow [`AudioBus`] view (split-borrow),
    /// advancing the DAC to `master_ticks` so sample emission is derived from
    /// the one canonical clock (ADR 0006) rather than an independent counter.
    /// The move is skipped on the steps that emit nothing, which at a typical
    /// ~32 kHz is about 1,949 of every 1,950: the AI is asked first, and only a
    /// `NeedsBus` buys the `take`. Same shape as [`Bus::rdp_tick`] and for the
    /// same reason — the borrow cannot be arranged without moving the chip out,
    /// so the decision has to happen before it.
    ///
    /// The bus-free half still runs every step and still mutates (it stamps
    /// `last_tick` and anchors the first sample), so this skips the *move*, never
    /// the step.
    pub fn audio_tick(&mut self, master_ticks: u64) {
        let Some(proof) = self.audio.tick_without_bus(master_ticks) else {
            return;
        };
        let mut audio = core::mem::take(&mut self.audio);
        audio.tick_with_bus(proof, self);
        self.audio = audio;
    }

    /// Drain the stereo stream the AI has emitted since the last drain — the
    /// frontend pushes it into the host ring and resamples (ADR 0004).
    pub fn drain_audio_samples(&mut self) -> alloc::vec::Vec<StereoSample> {
        self.audio.drain()
    }

    /// Diagnostic: count of RCP-chip steps taken (RSP ticks). The scheduler's
    /// fractional-divisor test reads this to assert the 3:2 ratio.
    #[must_use]
    pub const fn rcp_steps_for_test(&self) -> u64 {
        self.rcp_steps
    }

    /// Map a CPU physical address into RDRAM (`0..RDRAM_SIZE`), or `None` if it
    /// targets a memory-mapped register region instead.
    /// Base of RSP DMEM. IMEM follows at `+0x1000`.
    pub const SPMEM_BASE: u32 = 0x0400_0000;

    /// RCP cycles a PI direct-I/O write stays latched before finalizing.
    ///
    /// **This number is fitted, not measured.** Hardware finalization depends on
    /// the PI domain timing registers (`LAT`/`PWD`/`PGS`/`RLS`), which are not
    /// modeled; n64-systemtest bounds the latch only relatively (visible after
    /// 0 decay-loop iterations, gone after 110). 100 was the best of the values
    /// tried against the suite.
    ///
    /// Treat that provenance as a warning, not a credential. The suite still
    /// fails `Write32, Read32 (same location)` on its **second** read, where
    /// hardware has finalized and we have not — a gap no single constant closes,
    /// because the real duration is not constant. Modeling the domain registers
    /// is the actual fix. Accuracy ledger C-9.
    pub const PI_WRITE_CYCLES: u32 = 100;

    /// Base of the **MI** register block (`0x0430_0000`).
    pub const MI_BASE: u32 = 0x0430_0000;

    /// `MI_VERSION`, the value *"most consoles report"*.
    ///
    /// Packed `RSP:RDP:RAC:IO`. Other values exist in the wild — `0x0101_0101`
    /// and `0x0201_0202` appear in emulators and docs, iQue reports
    /// `0x0202_b0b0` — so this is a **choice among documented observations**,
    /// not a derived constant. Retail NTSC hardware is what this emulator
    /// models, so it reports what retail hardware reports.
    pub const MI_VERSION_VALUE: u32 = 0x0202_0102;

    /// Is this address in the MI register block?
    ///
    /// The block is four registers, and *"accesses beyond `0x0430 0010` are
    /// mirrored, so only the least significant four bits are taken into account
    /// for address decoding"*. The window itself runs to `0x0440_0000`, where
    /// the VI begins.
    const fn is_mi_register(addr: u32) -> bool {
        addr >= Self::MI_BASE && addr < 0x0440_0000
    }

    /// Read an MI register, after the 4-bit mirroring.
    const fn mi_read(&self, addr: u32) -> u32 {
        let i = self.rcp.mi_intr;
        let m = self.rcp.mi_mask;
        match (addr >> 2) & 3 {
            0 => self.rcp.mi_mode,
            1 => Self::MI_VERSION_VALUE,
            2 => pack_mi(i),
            _ => pack_mi(m),
        }
    }

    /// Write an MI register, after the 4-bit mirroring.
    fn mi_write(&mut self, addr: u32, val: u32) {
        match (addr >> 2) & 3 {
            0 => {
                // Only the bits that are storage are kept. `ClearDP` (bit 11)
                // is an action rather than a mode, and the repeat/EBus/Upper
                // modes are RDRAM-transfer behavior this emulator does not
                // model -- see the note in `docs/rsp.md`.
                self.rcp.mi_mode = (self.rcp.mi_mode & !0x7F) | (val & 0x7F);
                if val & (1 << 11) != 0 {
                    self.rcp.mi_intr.dp = false;
                }
            }
            // MI_VERSION is read-only, and MI_INTERRUPT is driven by the
            // devices -- a write to either does nothing.
            1 | 2 => {}
            _ => {
                // The mask uses clear/set pairs at `2n` / `2n + 1`, in the same
                // device order as the read layout. Unlike `SP_STATUS`, the wiki
                // does not state what both-bits-at-once does here, so this
                // applies clear before set rather than inventing a rule; if a
                // test ever pins it, it belongs in the ledger.
                let mut m = self.rcp.mi_mask;
                for (bit, line) in [(0u32, 0u32), (1, 1), (2, 2), (3, 3), (4, 4), (5, 5)] {
                    let clear = val & (1 << (bit * 2)) != 0;
                    let set = val & (1 << (bit * 2 + 1)) != 0;
                    let slot = match line {
                        0 => &mut m.sp,
                        1 => &mut m.si,
                        2 => &mut m.ai,
                        3 => &mut m.vi,
                        4 => &mut m.pi,
                        _ => &mut m.dp,
                    };
                    if clear {
                        *slot = false;
                    }
                    if set {
                        *slot = true;
                    }
                }
                self.rcp.mi_mask = m;
            }
        }
    }

    /// Base of the eight SP interface registers (`0x0404_0000`).
    pub const SP_REGS_BASE: u32 = 0x0404_0000;
    /// `SP_STATUS` (`0x0404_0010`), named because tests reach for it directly.
    pub const SP_STATUS: u32 = 0x0404_0010;
    /// `SP_PC` (`0x0408_0000`) — in its own window, not with the other eight.
    pub const SP_PC: u32 = 0x0408_0000;

    /// Is this one of the eight SP interface registers?
    const fn is_sp_register(addr: u32) -> bool {
        addr >= Self::SP_REGS_BASE && addr < Self::SP_REGS_BASE + 0x20
    }

    /// Base of the DP command registers (`0x0410_0000`): START, END, CURRENT,
    /// STATUS, then the (unmodeled) CLOCK/BUSY/PIPE/TMEM counters.
    pub const DP_REGS_BASE: u32 = 0x0410_0000;

    /// Is this one of the eight DP command (`DPC_*`) registers?
    const fn is_dp_register(addr: u32) -> bool {
        addr >= Self::DP_REGS_BASE && addr < Self::DP_REGS_BASE + 0x20
    }

    /// Base of the VI register block (`0x0440_0000`); the AI follows at
    /// `0x0450_0000`.
    pub const VI_REGS_BASE: u32 = 0x0440_0000;

    /// Is this address in the VI register block? The sixteen registers span
    /// `0x0440_0000..0x0440_0040`; the rest of the `0x044x_xxxx` window mirrors
    /// them (four-bit decode), which the word-offset mask in [`Vi::read`]
    /// handles.
    const fn is_vi_register(addr: u32) -> bool {
        addr >= Self::VI_REGS_BASE && addr < 0x0450_0000
    }

    /// Base of the AI register block (`0x0450_0000`); the SI/RI follow above.
    pub const AI_REGS_BASE: u32 = 0x0450_0000;

    /// Is this address in the AI register block? Six registers span
    /// `0x0450_0000..0x0450_0018`; the rest of the `0x045x_xxxx` window mirrors
    /// them (the three-bit decode `(addr >> 2) & 7` in [`Audio::read_reg`]).
    const fn is_ai_register(addr: u32) -> bool {
        addr >= Self::AI_REGS_BASE && addr < 0x0460_0000
    }

    /// Write an AI register, applying its interrupt effect to the MI: enqueuing
    /// the first buffer raises `MI_INTR.ai`; a write to `AI_STATUS` lowers it.
    fn ai_write(&mut self, addr: u32, val: u32) {
        match self.audio.write_reg((addr >> 2) & 7, val) {
            AiIrq::Raise => self.rcp.mi_intr.ai = true,
            AiIrq::Lower => self.rcp.mi_intr.ai = false,
            AiIrq::None => {}
        }
    }

    /// Base of the RI (RDRAM controller) register block.
    ///
    /// `0x0470_0000`, holding the eight registers N64brew *RDRAM Interface*
    /// §Registers enumerates: `RI_MODE`, `RI_CONFIG`, `RI_CURRENT_LOAD`,
    /// `RI_SELECT`, `RI_REFRESH`, `RI_LATENCY`, `RI_ERROR`, `RI_BANK_STATUS`.
    pub const RI_BASE: u32 = 0x0470_0000;

    /// Is this address in the RI register block? Eight registers span
    /// `0x0470_0000..0x0470_0020`; the rest of the `0x047x_xxxx` window mirrors
    /// them via the three-bit decode `(addr >> 2) & 7`, as the other RCP blocks do.
    const fn is_ri_register(addr: u32) -> bool {
        addr >= Self::RI_BASE && addr < Self::SI_BASE
    }

    /// Base of the SI register block (`0x0480_0000`).
    pub const SI_BASE: u32 = 0x0480_0000;

    /// Is this address in the SI register block (`0x0480_0000..0x0490_0000`)?
    const fn is_si_register(addr: u32) -> bool {
        addr >= Self::SI_BASE && addr < 0x0490_0000
    }

    /// Base of the PIF address space — the PIF **boot ROM** (IPL1/IPL2) window
    /// `0x1FC0_0000..0x1FC0_07C0`, mapped only during the real-PIF boot; under HLE
    /// no ROM is installed and it reads back 0 (the prior behavior).
    const PIF_ROM_BASE: u32 = 0x1FC0_0000;

    /// The 64-byte PIF RAM window (`0x1FC0_07C0..0x1FC0_0800`) — the tail of the
    /// PIF address space, where the CPU reads/writes the joybus command block.
    const PIF_RAM_BASE: u32 = 0x1FC0_07C0;

    /// Is this a PIF-block address (`0x1FC0_0000..0x1FC0_0800` — ROM then RAM)?
    const fn is_pif(addr: u32) -> bool {
        addr >= 0x1FC0_0000 && addr < 0x1FC0_0800
    }

    /// PIF-RAM command-byte offset (the last byte, `0x3F`).
    const PIF_CMD_BYTE: usize = 0x3F;

    /// Let the PIF act on a reset-mode command when the CPU write touched the
    /// command byte (real-PIF boot only; a no-op under HLE and in run mode).
    /// Latches the NMI freeze if IPL2's checksum verify fails.
    fn pif_boot_command_if_cmd(&mut self, off: usize) {
        if off == Self::PIF_CMD_BYTE && self.cart.pif_boot_command() {
            self.boot_nmi_halt = true;
        }
    }

    /// Has the PIF frozen the CPU via NMI after a failed real-PIF boot checksum?
    /// Always `false` under HLE, in run mode, and for a genuine ROM.
    #[must_use]
    pub const fn boot_nmi_halt(&self) -> bool {
        self.boot_nmi_halt
    }

    /// Warm-reset the real-PIF boot latches so a reset restarts IPL1→IPL2: clear
    /// the NMI freeze and unlock the PIF ROM (`PIF-NUS.md` §Console Reset). No-op
    /// under HLE (nothing is latched). The CPU's reset vector is restored by
    /// [`crate::System::reset`], which recreates the CPU at `0xBFC0_0000`.
    pub const fn reset_boot_latches(&mut self) {
        self.boot_nmi_halt = false;
        self.cart.pif_reset_boot();
    }

    /// Read an SI register (`SI_DRAM_ADDR` / `SI_STATUS`; the PIF-address
    /// registers are write-triggers and read back as 0).
    const fn si_read(&self, addr: u32) -> u32 {
        match (addr - Self::SI_BASE) & 0x1C {
            0x00 => self.si_dram_addr,
            // SI_STATUS at +0x18: DMA/IO idle (instant DMA); bit 12 = interrupt.
            0x18 => (self.rcp.mi_intr.si as u32) << 12,
            _ => 0,
        }
    }

    /// Write an SI register. `SI_DRAM_ADDR` latches the RDRAM side; the
    /// `PIF_AD_RD64B`/`WR64B` registers trigger the 64-byte PIF DMA; a write to
    /// `SI_STATUS` acknowledges (clears) the SI interrupt.
    fn si_write(&mut self, addr: u32, val: u32) {
        match (addr - Self::SI_BASE) & 0x1C {
            0x00 => self.si_dram_addr = val & 0x00FF_FFFF,
            // RD64B (+0x04): the PIF executes the joybus frame, then DMAs PIF
            // RAM → RDRAM. This is the read that actually runs the handshakes.
            0x04 => {
                self.cart.pif_execute(&self.controllers);
                let ram = *self.cart.pif_ram();
                for (i, &b) in ram.iter().enumerate() {
                    if let Some(off) = Self::rdram_offset(self.si_dram_addr.wrapping_add(i as u32))
                    {
                        self.rdram[off] = b;
                        self.mark_rdram_dirty(off);
                    }
                }
                self.rcp.mi_intr.si = true;
            }
            // WR64B (+0x10): DMA RDRAM → PIF RAM, then the PIF parses (on the
            // command-byte write inside `pif_load`/execute).
            0x10 => {
                let mut ram = [0u8; rustyn64_cart::pif::PIF_RAM_LEN];
                for (i, b) in ram.iter_mut().enumerate() {
                    *b = Self::rdram_offset(self.si_dram_addr.wrapping_add(i as u32))
                        .map_or(0, |off| self.rdram[off]);
                }
                self.cart.pif_load(&ram);
                self.rcp.mi_intr.si = true;
            }
            // SI_STATUS write acknowledges the interrupt.
            0x18 => self.rcp.mi_intr.si = false,
            _ => {}
        }
    }

    /// Write a VI register; a write to `VI_V_CURRENT` acknowledges the VI
    /// interrupt (`MI_INTR.vi = false`).
    const fn vi_write(&mut self, addr: u32, val: u32) {
        if self.vi.write(addr >> 2, val) {
            self.rcp.mi_intr.vi = false;
        }
    }

    /// Scan the framebuffer out into `out` as RGBA8, returning the active
    /// `(width, height)` — the presentable frame the VI would send to the DAC.
    ///
    /// Reads `VI_ORIGIN`/`VI_WIDTH`/`VI_CTRL` and derives the height from the
    /// active region `VI_V_VIDEO` (`(V_END − V_START)` half-lines → lines).
    /// Pixel formats (`VI_CTRL.TYPE`): 2 = 16-bit RGBA5551 (each 5-bit channel
    /// expanded to 8, the 1-bit alpha to 0/255), 3 = 32-bit RGBA8888 (a direct
    /// copy). `TYPE` 0/1 is blank — returns `(0, 0)` and writes nothing, the
    /// caller keeps a black frame.
    ///
    /// Returns `(0, 0)` and writes nothing when the VI is blanked, the width or
    /// height is zero, or `out` is smaller than `width * height * 4` — a caller
    /// that gets a non-zero size can trust the whole frame was written.
    ///
    /// **Scope:** a 1:1 scan (no `VI_X_SCALE`/`VI_Y_SCALE` resampling) and no
    /// AA/divot/de-dither post-filter — those are later VI work, recorded as
    /// open residual R-5 in `docs/accuracy-ledger.md`. Byte-exact for the direct
    /// framebuffer copy, which is what the FILL pipeline produces.
    #[must_use]
    pub fn scanout(&self, out: &mut [u8]) -> (u32, u32) {
        let bpp = match self.vi.read(vi::VI_CTRL) & 0x3 {
            2 => 2u32, // 16-bit RGBA5551
            3 => 4,    // 32-bit RGBA8888
            _ => return (0, 0),
        };
        let origin = self.vi.read(vi::VI_ORIGIN) & 0x00FF_FFFF;
        let width = self.vi.read(vi::VI_WIDTH) & 0xFFF;
        let v_video = self.vi.read(vi::VI_V_VIDEO);
        let height = ((v_video & 0x3FF).saturating_sub((v_video >> 16) & 0x3FF)) / 2;
        if width == 0 || height == 0 {
            return (0, 0);
        }
        // Refuse an undersized destination up front rather than write a
        // truncated frame and claim full dimensions; this also keeps the
        // per-pixel loop bounds-check-free.
        if out.len() < (width as usize) * (height as usize) * 4 {
            return (0, 0);
        }
        let stride = width * bpp;
        for y in 0..height {
            for x in 0..width {
                let src = origin.wrapping_add(y * stride).wrapping_add(x * bpp);
                let dst = ((y * width + x) * 4) as usize;
                if bpp == 2 {
                    let px = (u16::from(self.rdram_read(src)) << 8)
                        | u16::from(self.rdram_read(src.wrapping_add(1)));
                    out[dst] = expand5(((px >> 11) & 0x1F) as u8);
                    out[dst + 1] = expand5(((px >> 6) & 0x1F) as u8);
                    out[dst + 2] = expand5(((px >> 1) & 0x1F) as u8);
                    out[dst + 3] = if px & 1 == 1 { 0xFF } else { 0 };
                } else {
                    // 32-bit RGBA8888 is a direct big-endian copy.
                    out[dst..dst + 4].copy_from_slice(&self.rdram_read_u32(src).to_be_bytes());
                }
            }
        }
        (width, height)
    }

    /// Hardware-accurate VI scan-out with `VI_X_SCALE`/`VI_Y_SCALE` resampling and
    /// the real active-span/overscan geometry (ledger **R-5**, gap-analysis Stage D).
    ///
    /// This is the accurate replacement for [`Bus::scanout`]'s 1:1 copy, built up a
    /// slice at a time and validated RGB byte-for-byte against Angrylion's VI pipeline
    /// (`vi_process_full`) through the `.vivec` conformance vectors. Implemented so far:
    /// the geometry — the 2.10 fixed-point accumulator (`line_x = x_offs >> 10`, source
    /// index `stride*srcY + srcX`), the NTSC/PAL horizontal overscan (`h_start -= 108`
    /// / `128`), the 8/7-px `minhpass`/`maxhpass` crop, the `PRESCALE_WIDTH`/`HEIGHT`
    /// clamp, and the truncating RGBA5551→8 conversion the VI uses (`(px >> 8) & 0xF8`,
    /// not `expand5`'s replicating widening); the 5-bit **bilinear lerp** for **both**
    /// 16- and 32-bit sources (`aa_mode != REPLICATE` and a non-zero fraction, nearest
    /// otherwise); the **gamma** curve; and, under `aa_mode` 0/1 for **both** source
    /// formats, the coverage-gated **de-dither** (`cvg == 7`) and **AA-edge**
    /// (`cvg < 7`) filters and the **divot** median (`divot_enable`) — 16-bit reads
    /// coverage from the hidden-bits plane, 32-bit from the alpha byte
    /// (`vi_read_cov`). Alpha is `0xFF` (opaque) for display; the VI carries
    /// coverage in its output alpha, which the harness compares as RGB-only.
    ///
    /// Still to come (later slices, still substituted here): the gamma-dither
    /// variants, the coverage filters under `aa_mode == 2` (`RESAMP_ONLY` forces
    /// `cvg = 7`, so de-dither can still apply — currently gated to `aa_mode ≤ 1`),
    /// and the remaining R-6 field timing (interlace / serrate and the exact
    /// `H_TOTAL`; the PAL 50 Hz field rate itself is handled by `Vi::field_hz`, which
    /// drives the same `ispal` region split this geometry uses).
    ///
    /// **This is the live presented path.** The frontend calls it directly
    /// (`rustyn64_frontend::emu::Emu::produce_frame`), so what a user sees is this
    /// function's output, not [`Bus::scanout`]'s 1:1 copy. `Bus::scanout` is
    /// retained as the simpler unscaled reference the R-5 vectors are compared
    /// against and as the geometry contrast in the frontend's own tests.
    ///
    /// Returns `(0, 0)` (writing nothing) when the VI is blanked (`TYPE` 0/1), the
    /// computed width/height is non-positive, or `out` is too small.
    #[must_use]
    #[allow(
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation,
        clippy::cast_possible_wrap,
        clippy::useless_let_if_seq,
        clippy::too_many_lines
    )]
    pub fn scanout_scaled(&self, out: &mut [u8]) -> (u32, u32) {
        // The DAC prescale buffer bounds (Angrylion `PRESCALE_WIDTH`/`HEIGHT`).
        const PRESCALE_W: i32 = 640;
        const PRESCALE_H: i32 = 625;
        let ctrl = self.vi.read(vi::VI_CTRL);
        let bpp = match ctrl & 0x3 {
            2 => 2u32, // 16-bit RGBA5551
            3 => 4,    // 32-bit RGBA8888
            _ => return (0, 0),
        };
        // aa_mode (VI_CTRL bits 9:8): 3 = REPLICATE (nearest); anything else enables
        // the bilinear resample when a fraction is non-zero. aa_mode 0/1 additionally
        // reads real coverage and runs the de-dither / AA-edge filters (32-bit path).
        let aa_mode = (ctrl >> 8) & 0x3;
        // Gamma (VI_CTRL bit 3) applies the sqrt curve to the final RGB; the dithered
        // variants (bit 2 set) are noise-based and deferred, so plain gamma is applied
        // only when gamma_enable is set and gamma_dither is not (bit 3 set, bit 2 clear).
        let gamma = (ctrl & 0x0C) == 0x08;
        // The de-dither / AA-edge coverage path (aa_mode 0/1); `dither_filter`
        // (VI_CTRL bit 16) enables the de-dither restore on fully-covered pixels, and
        // `divot` (VI_CTRL bit 4) the 3-tap median on partial-coverage edges.
        let dither_filter = (ctrl >> 16) & 1 != 0;
        let divot = (ctrl >> 4) & 1 != 0;
        let origin = self.vi.read(vi::VI_ORIGIN) & 0x00FF_FFFF;
        let src_stride = (self.vi.read(vi::VI_WIDTH) & 0xFFF) as i32; // source pixels/row
        let h_video = self.vi.read(vi::VI_H_VIDEO);
        let v_video = self.vi.read(vi::VI_V_VIDEO);
        let x_scale = self.vi.read(vi::VI_X_SCALE);
        let y_scale = self.vi.read(vi::VI_Y_SCALE);
        let v_sync = self.vi.read(vi::VI_V_TOTAL) & 0x3FF;

        // Register decode (Angrylion `n64video_update_screen`; 2.10 fixed point).
        let h_start_raw = ((h_video >> 16) & 0x3FF) as i32;
        let h_end = (h_video & 0x3FF) as i32;
        let v_start_raw = ((v_video >> 16) & 0x3FF) as i32;
        let v_end = (v_video & 0x3FF) as i32;
        let mut hres = h_end - h_start_raw;
        let mut vres = (v_end - v_start_raw) >> 1;
        let x_add = (x_scale & 0xFFF) as i32;
        let mut x_start = ((x_scale >> 16) & 0xFFF) as i32;
        let y_add = (y_scale & 0xFFF) as i32;
        let mut y_start = ((y_scale >> 16) & 0xFFF) as i32;

        // Active-span adjust: NTSC/PAL horizontal overscan, then left/top clamps that
        // fold the cropped offset back into the scale accumulator start.
        let ispal = v_sync > vi::VI_PAL_V_TOTAL_THRESHOLD;
        let mut h_start = h_start_raw - if ispal { 128 } else { 108 };
        let mut h_start_clamped = false;
        if h_start < 0 {
            x_start += x_add * (-h_start);
            hres += h_start;
            h_start = 0;
            h_start_clamped = true;
        }
        let vstartoffset = if ispal { 44 } else { 34 };
        let mut v_start = (v_start_raw - vstartoffset) / 2;
        if v_start < 0 {
            y_start += y_add * (-v_start);
            v_start = 0;
        }
        let mut hres_clamped = false;
        if hres + h_start > PRESCALE_W {
            hres = PRESCALE_W - h_start;
            hres_clamped = true;
        }
        if vres + v_start > PRESCALE_H {
            vres = PRESCALE_H - v_start;
        }
        // Horizontal overscan crop; vertical is handled by the `v_start` origin.
        let minhpass = if h_start_clamped { 0 } else { 8 };
        let maxhpass = if hres_clamped { hres } else { hres - 7 };
        // Interlace/serrate (`VI_CTRL` bit 6) is deferred to R-6: this slice models
        // only the progressive field, so the height is `vres`. Angrylion doubles it
        // (`vres << serrate`) and doubles the source walk per field — modeling only
        // the height doubling here would fabricate a half-rate double-height frame,
        // which is worse than not modeling interlace at all, so serrate is ignored
        // until R-6 lands the field cadence and a vector for it.
        let width = (maxhpass - minhpass).max(0);
        let height = vres.max(0);
        if width == 0 || height == 0 {
            return (0, 0);
        }
        let (w, h) = (width as u32, height as u32);
        if out.len() < (w as usize) * (h as usize) * 4 {
            return (0, 0);
        }

        // The source columns the walk below can ask for. `x_add` is unsigned, so `sx`
        // is monotonically non-decreasing in `ox`: the first and last output pixels
        // bracket it, and the `+ 1` covers the far bilinear column. Derived from the
        // loop bounds rather than guessed, because a memo whose range is short by one
        // silently falls back to the uncached path and reads as "the optimization did
        // not help".
        // `i64` because every term is guest-controlled through VI MMIO: `x_start` and
        // `x_add` come from `VI_X_SCALE`, and `width` from the `VI_H_VIDEO` span. The
        // product is small in practice, but "small in practice" is not a property that
        // survives a clamp being relaxed, and the failure mode would be a debug-build
        // overflow panic in the scan-out.
        let x_span_end =
            i64::from(x_start) + (i64::from(minhpass) + i64::from(width) - 1) * i64::from(x_add);
        let x_first =
            i32::try_from((i64::from(x_start) + i64::from(minhpass) * i64::from(x_add)) >> 10)
                .unwrap_or(0);
        let x_last = i32::try_from((x_span_end >> 10) + 1).unwrap_or(x_first);
        let mut sampler = ViSampler::new(
            ViCfg {
                origin,
                src_stride,
                bpp,
                aa_mode,
                divot,
                dither_filter,
            },
            x_first,
            x_last,
        );

        for oy in 0..height {
            let curry = y_start + oy * y_add;
            let sy = curry >> 10;
            let yfrac = (curry >> 5) & 0x1F;
            for ox in 0..width {
                let x_offs = x_start + (minhpass + ox) * x_add;
                let sx = x_offs >> 10;
                let xfrac = (x_offs >> 5) & 0x1F;
                let dst = ((oy * width + ox) * 4) as usize;
                // Bilinear when aa_mode isn't REPLICATE and a fraction is non-zero
                // (Angrylion `lerping`): four texels, vertical lerp per column then
                // horizontal between them. Otherwise the exact nearest sample.
                //
                // Both zero-weight cases are skipped rather than computed and
                // multiplied by zero — `xfrac == 0` here and `yfrac == 0` inside
                // [`Bus::vi_column`]. Under the configuration Super Mario 64 programs
                // (`x_add` 512, so `xfrac` alternates 0 / 16; `y_add` 1024, so `yfrac`
                // is always 0) that is the difference between 2.5 filter chains per
                // output pixel and 1.5.
                let mut rgb = if aa_mode != 3 && (xfrac != 0 || yfrac != 0) {
                    let col = self.vi_column(&mut sampler, sx, sy, yfrac);
                    if xfrac == 0 {
                        col
                    } else {
                        let ncol = self.vi_column(&mut sampler, sx + 1, sy, yfrac);
                        vi_lerp3(col, ncol, xfrac)
                    }
                } else {
                    self.vi_sample(&mut sampler, sx, sy)
                };
                // Gamma is the final RGB stage (after scale, before write) — a table
                // lookup per channel (the LUT is `vi_gamma` precomputed).
                if gamma {
                    rgb = rgb.map(|c| GAMMA_TABLE[usize::from(c)]);
                }
                out[dst..dst + 3].copy_from_slice(&rgb);
                out[dst + 3] = 0xFF; // opaque display alpha (VI coverage is not shown)
            }
        }
        (w, h)
    }

    /// One source pixel as the scan-out wants it — filtered under `aa_mode` 0/1,
    /// plain under 2/3 — served from the memo when it is there.
    ///
    /// Only the filtered path is memoized. Under `aa_mode` 2/3 a sample is two RDRAM
    /// reads and a format convert, which is cheaper than the row bookkeeping, so
    /// caching it would be a pessimization dressed as an optimization. The filters
    /// themselves have exactly one implementation either way; this chooses whether to
    /// consult a cache before calling it.
    fn vi_sample(&self, s: &mut ViSampler, x: i32, y: i32) -> [u8; 3] {
        if s.cfg.aa_mode > 1 {
            return self.vi_sample_direct(s, x, y);
        }
        // `checked_sub` rather than `-`: both operands trace back to guest-controlled
        // VI registers, and the miss path is a fall-through, not an error — so an
        // extreme pair should decline the memo, not panic a debug build.
        let Some(idx) = x
            .checked_sub(s.x_lo)
            .and_then(|offset| usize::try_from(offset).ok())
        else {
            return self.vi_sample_direct(s, x, y);
        };
        if idx >= s.span {
            return self.vi_sample_direct(s, x, y);
        }
        let cell = s.row_slot(y) * s.span + idx;
        if let Some(hit) = s.cells[cell] {
            return hit;
        }
        let computed = self.vi_sample_direct(s, x, y);
        s.cells[cell] = Some(computed);
        computed
    }

    /// [`Bus::vi_sample`] without the memo: the actual filter dispatch.
    ///
    /// Under `aa_mode` 0/1 the coverage path (de-dither / AA-edge / divot) runs for
    /// both formats — 16-bit reads coverage from the hidden-bits plane, 32-bit from the
    /// alpha byte ([`Bus::vi_read_cov`]). Under `aa_mode` 2/3 (`RESAMP_ONLY` / REPLICATE)
    /// coverage is forced full, so it is a plain format-dispatched fetch.
    fn vi_sample_direct(&self, s: &ViSampler, x: i32, y: i32) -> [u8; 3] {
        let ViCfg {
            origin,
            src_stride,
            bpp,
            aa_mode,
            divot,
            dither_filter,
        } = s.cfg;
        if aa_mode <= 1 {
            if divot {
                self.vi_divot(origin, src_stride, x, y, dither_filter, bpp)
            } else {
                self.vi_fetch_coverage(origin, src_stride, x, y, dither_filter, bpp)
            }
        } else if bpp == 2 {
            self.vi_fetch16(origin, src_stride, x, y)
        } else {
            self.vi_fetch32(origin, src_stride, x, y)
        }
    }

    /// One column's vertical lerp, or its single upper sample when `yfrac` weights the
    /// lower row at zero.
    ///
    /// A function rather than two inline copies so the `sx` and `sx + 1` columns cannot
    /// drift apart — this is a correctness-critical path pinned by the VI conformance
    /// vectors.
    ///
    /// The `yfrac == 0` case skips a tap whose weight is zero: `vi_lerp3(a, b, 0)` is
    /// `a + (((b - a) * 0 + 16) >> 5)` = `a + 0` = `a`, so the far sample is discarded
    /// and not fetching it cannot change the result.
    fn vi_column(&self, s: &mut ViSampler, x: i32, sy: i32, yfrac: i32) -> [u8; 3] {
        if yfrac == 0 {
            return self.vi_sample(s, x, sy);
        }
        let upper = self.vi_sample(s, x, sy);
        let lower = self.vi_sample(s, x, sy + 1);
        vi_lerp3(upper, lower, yfrac)
    }

    /// Fetch a 16-bit RGBA5551 source pixel at `(x, y)` (stride `src_stride`, base
    /// `origin`) and convert to the VI's truncating RGB8 (`vi_rgb5551`). Reads
    /// big-endian through `rdram_read`, which returns 0 for an out-of-range address,
    /// so an out-of-bounds sample cannot panic. Ledger R-5 (VI scale resample).
    fn vi_fetch16(&self, origin: u32, src_stride: i32, x: i32, y: i32) -> [u8; 3] {
        let idx = src_stride.wrapping_mul(y).wrapping_add(x);
        // `wrapping_add_signed` adds the (possibly negative) signed byte offset to the
        // unsigned base without a sign-losing cast; `rdram_read` bounds-checks.
        let byte = origin.wrapping_add_signed(idx.wrapping_mul(2));
        let px = (u16::from(self.rdram_read(byte)) << 8)
            | u16::from(self.rdram_read(byte.wrapping_add(1)));
        vi_rgb5551(px)
    }

    /// Fetch a 32-bit RGBA8888 source pixel at `(x, y)` as RGB8 (the big-endian
    /// R/G/B bytes; the alpha byte carries coverage, not shown). Reads big-endian
    /// through `rdram_read_u32`, bounds-safe like `vi_fetch16`. Ledger R-5.
    fn vi_fetch32(&self, origin: u32, src_stride: i32, x: i32, y: i32) -> [u8; 3] {
        let idx = src_stride.wrapping_mul(y).wrapping_add(x);
        let byte = origin.wrapping_add_signed(idx.wrapping_mul(4));
        let w = self.rdram_read_u32(byte).to_be_bytes();
        [w[0], w[1], w[2]]
    }

    /// Read the raw 32-bit RGBA8888 source word at `(x, y)` (for coverage + the
    /// filter neighbor taps). Big-endian, bounds-safe. Ledger R-5.
    fn vi_read32(&self, origin: u32, src_stride: i32, x: i32, y: i32) -> u32 {
        let idx = src_stride.wrapping_mul(y).wrapping_add(x);
        let byte = origin.wrapping_add_signed(idx.wrapping_mul(4));
        self.rdram_read_u32(byte)
    }

    /// Read the raw 16-bit RGBA5551 source halfword at `(x, y)` (big-endian,
    /// bounds-safe), the 16-bit counterpart to [`Bus::vi_read32`]. Ledger R-5.
    fn vi_read16(&self, origin: u32, src_stride: i32, x: i32, y: i32) -> u16 {
        let idx = src_stride.wrapping_mul(y).wrapping_add(x);
        let byte = origin.wrapping_add_signed(idx.wrapping_mul(2));
        (u16::from(self.rdram_read(byte)) << 8) | u16::from(self.rdram_read(byte.wrapping_add(1)))
    }

    /// Read one source pixel as **raw** RGB8 (no filters) plus its 3-bit coverage,
    /// dispatching on the framebuffer format (`bpp` = 2 or 4). This is the sole
    /// format-specific primitive of the coverage path — every downstream filter
    /// (de-dither, AA-edge, divot) then operates on 8-bit channels regardless of
    /// source depth (Angrylion `vi_fetch_filter16`/`32`). Ledger R-5.
    ///
    /// - **32-bit RGBA8888:** channels are the top three big-endian bytes; coverage
    ///   is alpha bits 7:5 (`(px >> 5) & 7`).
    /// - **16-bit RGBA5551:** channels are the truncating [`vi_rgb5551`] expansion;
    ///   coverage combines the pixel's bit 0 (MSB) with the two **hidden bits** of
    ///   the 9-bit RDRAM plane (`((px & 1) << 2) | rdram_hidden`), so `cvg == 7`
    ///   requires bit 0 set **and** hidden bits `0b11`. The hidden read takes the
    ///   pixel byte address (its own halfword index is derived internally).
    fn vi_read_cov(
        &self,
        origin: u32,
        src_stride: i32,
        x: i32,
        y: i32,
        bpp: u32,
    ) -> ([u8; 3], u32) {
        // Callers derive `bpp` from `VI_CTRL.TYPE` mapped to 2 (RGBA5551) or 4
        // (RGBA8888); any other value would silently take the 32-bit branch.
        debug_assert!(bpp == 2 || bpp == 4, "vi_read_cov: bpp must be 2 or 4");
        if bpp == 2 {
            // The hidden-bits halfword shares the color pixel's byte address.
            let idx = src_stride.wrapping_mul(y).wrapping_add(x);
            let byte = origin.wrapping_add_signed(idx.wrapping_mul(2));
            let px = self.vi_read16(origin, src_stride, x, y);
            let cvg = ((u32::from(px) & 1) << 2) | u32::from(self.rdram_read_hidden(byte));
            (vi_rgb5551(px), cvg)
        } else {
            let px = self.vi_read32(origin, src_stride, x, y);
            (
                [(px >> 24) as u8, (px >> 16) as u8, (px >> 8) as u8],
                (px >> 5) & 7,
            )
        }
    }

    /// A source fetch for the coverage path (`aa_mode` 0/1), format-generic over
    /// `bpp` (2 = RGBA5551, 4 = RGBA8888). Reads the pixel's coverage via
    /// [`Bus::vi_read_cov`]; a fully-covered pixel (`cvg == 7`) gets the **de-dither**
    /// restore filter when `dither_filter` is set, otherwise the raw color. A partial
    /// pixel (`cvg < 7`) takes the **AA-edge** filter ([`Bus::vi_video_filter`]).
    /// Ledger R-5.
    ///
    /// De-dither (Angrylion `restore_filter16`/`32`): over the 8 taps of the 3×3
    /// neighborhood minus the center, each channel is nudged ±1 toward the neighbor
    /// (comparing the top-5-bit values `rgb8 >> 3` — the stored 5-bit channel in both
    /// formats), the noise-removing correction; the result is truncated to `u8`
    /// (Angrylion stores it into a `u8` field unmasked).
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn vi_fetch_cov(
        &self,
        origin: u32,
        src_stride: i32,
        x: i32,
        y: i32,
        dither_filter: bool,
        bpp: u32,
    ) -> ([u8; 3], u32) {
        // The 3×3 neighborhood minus the center (restore.c tap layout).
        const TAPS: [(i32, i32); 8] = [
            (-1, -1),
            (0, -1),
            (1, -1),
            (-1, 1),
            (0, 1),
            (1, 1),
            (-1, 0),
            (1, 0),
        ];
        let (center, cvg) = self.vi_read_cov(origin, src_stride, x, y, bpp);
        if cvg < 7 {
            // Partial coverage → the AA edge filter (video_filter16/32).
            return (
                self.vi_video_filter(origin, src_stride, x, y, center, cvg, bpp),
                cvg,
            );
        }
        if !dither_filter {
            return (center, cvg); // fully covered without dither → raw color
        }
        let center5 = [center[0] >> 3, center[1] >> 3, center[2] >> 3]; // top 5 bits
        let mut acc = [
            i32::from(center[0]),
            i32::from(center[1]),
            i32::from(center[2]),
        ];
        for (dx, dy) in TAPS {
            let (nb, _) = self.vi_read_cov(origin, src_stride, x + dx, y + dy, bpp);
            let nb5 = [nb[0] >> 3, nb[1] >> 3, nb[2] >> 3];
            for c in 0..3 {
                acc[c] += match center5[c].cmp(&nb5[c]) {
                    core::cmp::Ordering::Less => 1,
                    core::cmp::Ordering::Greater => -1,
                    core::cmp::Ordering::Equal => 0,
                };
            }
        }
        ([acc[0] as u8, acc[1] as u8, acc[2] as u8], cvg)
    }

    /// The filtered coverage-path color ([`Bus::vi_fetch_cov`] without the
    /// coverage — for the non-divot path, which only needs the RGB).
    fn vi_fetch_coverage(
        &self,
        origin: u32,
        src_stride: i32,
        x: i32,
        y: i32,
        dither_filter: bool,
        bpp: u32,
    ) -> [u8; 3] {
        self.vi_fetch_cov(origin, src_stride, x, y, dither_filter, bpp)
            .0
    }

    /// The **divot** filter (Angrylion `divot_filter`), format-generic over `bpp`: the
    /// per-channel median of a pixel and its two horizontal neighbors (all
    /// post-de-dither/AA-edge, via [`Bus::vi_fetch_cov`]). It is **skipped** (the center
    /// passes through) when all three are fully covered
    /// (`cen_cvg & left_cvg & right_cvg == 7`), so it only touches partial-coverage
    /// edges. Ledger R-5.
    fn vi_divot(
        &self,
        origin: u32,
        src_stride: i32,
        x: i32,
        y: i32,
        dither_filter: bool,
        bpp: u32,
    ) -> [u8; 3] {
        let (cen, cen_cvg) = self.vi_fetch_cov(origin, src_stride, x, y, dither_filter, bpp);
        let (left, left_cvg) = self.vi_fetch_cov(origin, src_stride, x - 1, y, dither_filter, bpp);
        let (right, right_cvg) =
            self.vi_fetch_cov(origin, src_stride, x + 1, y, dither_filter, bpp);
        if (cen_cvg & left_cvg & right_cvg) == 7 {
            return cen; // all fully covered → no divot
        }
        // Branch-expanded median-of-3 per channel (divot.c), matching its tie-handling.
        let median = |lv: u8, cv: u8, rv: u8| {
            if (lv >= cv && rv >= lv) || (lv >= rv && cv >= lv) {
                lv
            } else if (rv >= cv && lv >= rv) || (rv >= lv && cv >= rv) {
                rv
            } else {
                cv
            }
        };
        [
            median(left[0], cen[0], right[0]),
            median(left[1], cen[1], right[1]),
            median(left[2], cen[2], right[2]),
        ]
    }

    /// The AA-edge filter for a partial-coverage pixel (Angrylion
    /// `video_filter16`/`32`), format-generic over `bpp`. Gathers the fully-covered
    /// pixels (`cvg == 7`) among the 6 taps — the up/down diagonals and the two-away
    /// left/right — via [`Bus::vi_read_cov`], plus the center, takes the per-channel
    /// penultimate min/max (`vi_video_max`), and pulls the center toward their midpoint
    /// weighted by `(7 - cvg)`:
    /// `center + (((penmin + penmax - 2*center) * (7 - cvg)) + 4 >> 3)`, masked to 8
    /// bits (the intermediate is unsigned two's-complement, so wrapping). Ledger R-5.
    #[allow(clippy::cast_possible_truncation, clippy::too_many_arguments)]
    fn vi_video_filter(
        &self,
        origin: u32,
        src_stride: i32,
        x: i32,
        y: i32,
        center: [u8; 3],
        cvg: u32,
        bpp: u32,
    ) -> [u8; 3] {
        // Up/down diagonals + two-away left/right (video.c `dirs`).
        const TAPS: [(i32, i32); 6] = [(-1, -1), (1, -1), (-2, 0), (2, 0), (-1, 1), (1, 1)];
        let mut back = [[0u32; 7]; 3]; // per channel, center at index 0
        for c in 0..3 {
            back[c][0] = u32::from(center[c]);
        }
        let mut n = 1usize;
        for (dx, dy) in TAPS {
            let (nb, nb_cvg) = self.vi_read_cov(origin, src_stride, x + dx, y + dy, bpp);
            if nb_cvg == 7 {
                back[0][n] = u32::from(nb[0]);
                back[1][n] = u32::from(nb[1]);
                back[2][n] = u32::from(nb[2]);
                n += 1;
            }
        }
        let coeff = 7 - cvg;
        let mut out = [0u8; 3];
        for c in 0..3 {
            let (penmin, penmax) = vi_video_max(&back[c][..n]);
            let ctr = u32::from(center[c]);
            let col = penmin
                .wrapping_add(penmax)
                .wrapping_sub(ctr << 1)
                .wrapping_mul(coeff)
                .wrapping_add(4)
                >> 3;
            out[c] = (col.wrapping_add(ctr) & 0xFF) as u8;
        }
        out
    }

    /// Apply a write to the SP register block, performing whatever it starts.
    ///
    /// Two effects can come from one write and they are collected separately:
    /// a length write starts a DMA, and a `SP_STATUS` write can raise or
    /// acknowledge the MI's SP line. Folding them into one return value would
    /// imply they are alternatives, and `SP_STATUS` is reachable by both.
    fn sp_register_write(&mut self, addr: u32, val: u32) {
        let index = (addr >> 2) & 7;
        if index == rustyn64_rsp::sp::reg::STATUS
            && let Some(raise) = rustyn64_rsp::sp::SpRegs::interrupt_change(val)
        {
            self.rcp.mi_intr.sp = raise;
        }
        if let Some(dma) = self.rsp.sp.write(index, val) {
            self.sp_dma(dma);
        }
    }
    /// DMEM + IMEM, 4 KiB each.
    pub const SPMEM_LEN: usize = 0x2000;

    /// End of the SP memory window — where the SP *registers* begin.
    ///
    /// The 8 KiB of real storage repeats for this whole range rather than
    /// ending at `0x0400_2000`; see [`rustyn64_rsp::Rsp::mem_read`] and
    /// accuracy ledger **C-30**, which records the provenance of the mirroring.
    pub const SPMEM_WINDOW_END: u32 = 0x0404_0000;

    /// Is this address in the RSP DMEM/IMEM window?
    const fn is_spmem(addr: u32) -> bool {
        addr >= Self::SPMEM_BASE && addr < Self::SPMEM_WINDOW_END
    }

    /// Is this address handled by a device on the RCP's **internal** bus?
    ///
    /// `0x0400_0000-0x04FF_FFFF`, the range N64brew *Memory map* describes as
    /// dispatched inside the RCP without going to an external bus. What matters
    /// here is the shared consequence: every device in it ignores the access
    /// size (see [`CpuBus::write_sized`]).
    ///
    /// The PI and SI external-bus windows share that size-blindness on hardware
    /// and are deliberately **not** included — the PI already models its own
    /// bus quirks separately, and folding both into one rule without the cart
    /// tests to check it against would be a change made blind. Phase 5.
    const fn is_rcp_internal(addr: u32) -> bool {
        matches!(addr, 0x0400_0000..=0x04FF_FFFF)
    }

    /// Base of the **`ISViewer`** debug window, in cart address space.
    ///
    /// Not real N64 hardware — it is a flashcart/emulator convention that
    /// n64-systemtest uses to report results (`ref-proj/n64-systemtest/src/isviewer.rs`).
    /// The suite probes for it by writing a magic word to the buffer and reading
    /// it back; if the round-trip fails it falls back to a framebuffer console
    /// we cannot read. So this window is what turns "the suite runs" into "the
    /// suite reports".
    pub const ISVIEWER_BASE: u32 = 0x13FF_0000;
    /// Writing this register flushes `len` bytes from the buffer.
    pub const ISVIEWER_WRITE_LEN: u32 = 0x13FF_0014;
    /// The text buffer.
    pub const ISVIEWER_BUF: u32 = 0x13FF_0020;
    /// Bytes of buffer modeled — the suite writes in `0x200` chunks.
    pub const ISVIEWER_LEN: usize = 0x1000;

    /// Is this address inside the `ISViewer` window?
    const fn is_isviewer(addr: u32) -> bool {
        addr >= Self::ISVIEWER_BASE && addr < Self::ISVIEWER_BASE + 0x20 + Self::ISVIEWER_LEN as u32
    }

    /// The raw `ISViewer` backing memory, for diagnostics.
    #[must_use]
    pub fn isviewer_raw(&self) -> &[u8] {
        &self.isviewer
    }

    /// Everything the guest has written to the `ISViewer` channel.
    #[must_use]
    pub fn isviewer_output(&self) -> &[u8] {
        &self.isviewer_out
    }

    /// Text the guest has pushed through the EMUX `xlog` channel.
    #[must_use]
    pub fn emux_output(&self) -> &[u8] {
        &self.emux_out
    }

    /// Offer the EMUX extensions to the guest.
    ///
    /// Opt-in, because hardware has none: enabling this changes which console
    /// backend n64-systemtest selects and therefore the instructions it
    /// executes. Worth it for a test harness (the `xlog` console needs no PI or
    /// `ISViewer` emulation and runs ~9x faster); wrong for anything claiming to
    /// reproduce a real console.
    pub const fn enable_emux(&mut self) {
        self.emux_enabled = true;
    }

    /// Has the guest requested termination via `EMUX xioctl(EXIT)`?
    #[must_use]
    pub const fn emux_exited(&self) -> bool {
        self.emux_exited
    }

    /// Is this address on the **PI external bus** — the memory-mapped window
    /// through which the CPU reaches cart ROM, SRAM and `FlashRAM`?
    ///
    /// Ranges from N64brew *Memory map*: `0x0500_0000-0x1FBF_FFFF` and
    /// `0x1FD0_0000-0x7FFF_FFFF`. Addresses outside them are DMA-only.
    const fn is_pi_bus(addr: u32) -> bool {
        matches!(addr, 0x0500_0000..=0x1FBF_FFFF | 0x1FD0_0000..=0x7FFF_FFFF)
    }

    /// Map a PI-bus address through the **16-bit-bus off-by-two**.
    ///
    /// The PI external bus is 16 bits wide and the RCP ignores access size, so
    /// every VR4300 read becomes two 16-bit bus reads: the MSB at the CPU's
    /// address with bit 0 ignored, then the LSB at `address + 2`. The RCP thus
    /// returns the word starting at `addr & !1`, while the CPU selects its byte
    /// lane assuming a word at `addr & !3`. **That two-byte disagreement is the
    /// bug**, and it is hardware behavior, not an approximation:
    ///
    /// > effectively a 16-bit read at `0x1000'0002` returns the 16-bit word at
    /// > `0x1000'0004`
    /// > — N64brew, *Memory map*, PI external bus
    ///
    /// Working it through, `byte = (addr & !1) + (addr & 3)`, which collapses to
    /// "add two when bit 1 is set". A halfword load needs no special case
    /// because it is issued as two byte reads and both land correctly; a **word**
    /// load must bypass this entirely, which is why [`Bus::read_u32`] reads the
    /// PI window raw.
    const fn pi_bus_byte(addr: u32) -> u32 {
        if addr & 2 != 0 {
            addr.wrapping_add(2)
        } else {
            addr
        }
    }

    /// Is this address in the PI register block?
    const fn is_pi_register(addr: u32) -> bool {
        addr >= rustyn64_cart::pi::PI_BASE && addr < rustyn64_cart::pi::PI_BASE + 0x34
    }

    /// Carry out an SP DMA the register file has programmed.
    ///
    /// The engine lives in `rustyn64-rsp` and returns a description; the copy
    /// happens **here**, because the RSP does not own RDRAM and a chip reaching
    /// back into its owner is the dependency cycle `docs/architecture.md` exists
    /// to prevent. The PI works the same way.
    ///
    /// `skip` applies to the RDRAM side only. The SP side is contiguous and
    /// **wraps within its own 4 KiB bank** — a single transfer never spans DMEM
    /// and IMEM (N64brew *RSP Interface*: *"if the transfer hits the end of
    /// either memory area, it wraps around to the beginning of it"*).
    pub fn sp_dma(&mut self, dma: rustyn64_rsp::sp::Dma) {
        // Bit 12 selects the bank and is held fixed for the whole transfer;
        // only the 12-bit offset advances, so it wraps inside that bank.
        let bank = dma.sp_addr & 0x1000;
        let mut mem = dma.sp_addr & 0xFFF;
        let mut dram = dma.ram_addr;

        for _ in 0..dma.rows {
            for _ in 0..dma.row_len {
                let m = bank | (mem & 0xFFF);
                if let Some(off) = Self::rdram_offset(dram) {
                    if dma.to_dram {
                        self.rdram[off] = self.rsp.mem_read(m);
                        self.mark_rdram_dirty(off);
                    } else {
                        self.rsp.mem_write(m, self.rdram[off]);
                    }
                }
                mem = mem.wrapping_add(1);
                dram = dram.wrapping_add(1);
            }
            // The RDRAM pointer steps over the gap between rows; the SP side
            // does not.
            dram = dram.wrapping_add(dma.skip);
        }

        // Hardware leaves the pointers past the data, and the length field at
        // `0xFF8`. Instantaneous for now: the transfer is a value, so charging
        // it real time later is a scheduling change rather than a rewrite.
        self.rsp
            .sp
            .complete_dma(bank | (mem & 0xFFF), dram & 0x00FF_FFFF);
    }

    /// Write a PI register and perform any transfer it starts.
    ///
    /// The copy happens **here**, not in the PI engine, because the PI does not
    /// own RDRAM — the Bus does. Having the engine reach back into its owner is
    /// the cycle this architecture exists to avoid, so the engine returns a
    /// description of the transfer and the owner carries it out.
    pub fn pi_write_word(&mut self, addr: u32, val: u32) {
        let started = self.pi.write(addr, val);
        // Mirror the PI's interrupt state into the MI on EVERY write, not only
        // on completion. A `PI_STATUS` write that clears the interrupt starts no
        // transfer, so an early return here left the MI line asserted -- `IP2`
        // stuck high forever, hanging any interrupt-driven loader.
        self.rcp.mi_intr.pi = self.pi.interrupt();
        let Some(t) = started else {
            return;
        };
        // Instantaneous for now. The transfer is a value, so charging it real
        // time later is a scheduling change rather than a rewrite -- which is
        // the same reason `SysAD` is a state machine rather than a function.
        for i in 0..t.len {
            if t.to_dram {
                let b = self.cart.pi_read(t.cart.wrapping_add(i));
                if let Some(off) = Self::rdram_offset(t.dram.wrapping_add(i)) {
                    self.rdram[off] = b;
                    self.mark_rdram_dirty(off);
                }
            } else {
                let b = Self::rdram_offset(t.dram.wrapping_add(i)).map_or(0, |off| self.rdram[off]);
                self.cart.pi_write(t.cart.wrapping_add(i), b);
            }
        }
        self.pi.complete();
        // Completion raises the PI line into the MI, which the CPU sees as IP2.
        self.rcp.mi_intr.pi = self.pi.interrupt();
    }

    const fn rdram_offset(addr: u32) -> Option<usize> {
        // KSEG0/KSEG1 are stripped by the (future) TLB; the physical RDRAM
        // window is `$0000_0000..$007F_FFFF`.
        let phys = (addr & 0x1FFF_FFFF) as usize;
        if phys < RDRAM_SIZE { Some(phys) } else { None }
    }
}

// --- The CPU's view of the whole machine. ---
use rustyn64_cart::pi;

impl CpuBus for Bus {
    fn read_u8(&mut self, addr: u32) -> u8 {
        if let Some(off) = Self::rdram_offset(addr) {
            return self.rdram[off];
        }
        if Self::is_pi_register(addr) {
            // PI registers are 32-bit; a byte read selects within the word.
            let mut w = self.pi.read(addr);
            // `IOBUSY` covers the asynchronous direct-I/O write as well as DMA;
            // software polls it to know when a cart write has landed.
            if addr & !3 == rustyn64_cart::pi::PI_STATUS && self.pi_io_busy() {
                w |= rustyn64_cart::pi::STATUS_IO_BUSY;
            }
            return (w >> (8 * (3 - (addr & 3)))) as u8;
        }
        if Self::is_spmem(addr) {
            return self.rsp.mem_read(addr - Self::SPMEM_BASE);
        }
        // The SP interface registers. Word-granular behind a byte read: the
        // whole block lives on the RCP's internal bus, which returns the
        // aligned word and lets the CPU select within it.
        if Self::is_sp_register(addr) {
            let w = self.rsp.sp.read((addr >> 2) & 7);
            return (w >> (8 * (3 - (addr & 3)))) as u8;
        }
        if Self::is_mi_register(addr) {
            return (self.mi_read(addr) >> (8 * (3 - (addr & 3)))) as u8;
        }
        if Self::is_dp_register(addr) {
            return (self.rdp.dpc_read((addr >> 2) & 7) >> (8 * (3 - (addr & 3)))) as u8;
        }
        if Self::is_vi_register(addr) {
            return (self.vi.read(addr >> 2) >> (8 * (3 - (addr & 3)))) as u8;
        }
        if Self::is_ai_register(addr) {
            return (self.audio.read_reg((addr >> 2) & 7) >> (8 * (3 - (addr & 3)))) as u8;
        }
        if Self::is_ri_register(addr) {
            return (self.rcp.ri[((addr >> 2) & 7) as usize] >> (8 * (3 - (addr & 3)))) as u8;
        }
        if Self::is_si_register(addr) {
            return (self.si_read(addr) >> (8 * (3 - (addr & 3)))) as u8;
        }
        if Self::is_pif(addr) {
            return if addr >= Self::PIF_RAM_BASE {
                self.cart.pif_read((addr - Self::PIF_RAM_BASE) as usize)
            } else {
                // PIF boot ROM (IPL1/IPL2): mapped only on the real-PIF path; 0
                // under HLE (no ROM installed).
                self.cart
                    .pif_boot_rom_read((addr - Self::PIF_ROM_BASE) as usize)
            };
        }
        if addr & !3 == Self::SP_PC {
            return (self.rsp.sp.pc() >> (8 * (3 - (addr & 3)))) as u8;
        }
        if Self::is_isviewer(addr) {
            // Readable as ordinary memory, which is what makes the suite's
            // write-magic-then-read-back probe succeed and select this channel
            // instead of the framebuffer console. Bounds-checked for the same
            // reason as the write path: the address is guest-controlled.
            return self
                .isviewer
                .get((addr - Self::ISVIEWER_BASE) as usize)
                .copied()
                .unwrap_or(0);
        }
        if Self::is_pi_bus(addr) {
            // A write in flight shadows the whole bus: reads from ANY address
            // return the value being written, not the device's data.
            if self.pi_io_busy() {
                return self.pi_write_latch.to_be_bytes()[(addr & 3) as usize];
            }
            return self.cart.pi_read(Self::pi_bus_byte(addr));
        }
        // TODO(T-CORE-01): decode the remaining RCP register windows.
        //
        // SP, DP, VI, AI, SI, RI, MI and the PIF ROM/RAM are all decoded above —
        // this comment listed every one of them as outstanding long after they
        // landed. What is genuinely still undecoded is the **RDRAM device
        // register** block (`0x03F0_0000`), the per-chip Rambus registers, which
        // is distinct from the RI controller block.
        self.cart.pi_read(addr)
    }

    /// Read an aligned big-endian word.
    ///
    /// Overridden for the **PI external bus** only. The default composes four
    /// [`Bus::read_u8`] calls, which would apply the 16-bit-bus off-by-two to
    /// each byte independently and mangle bytes 2 and 3 of every word. A word
    /// access puts its own address on the bus, so `addr & !1 == addr` and the
    /// word is simply the four bytes there.
    fn read_u32(&mut self, addr: u32) -> u32 {
        // ISViewer lives INSIDE the PI bus range and is claimed first, exactly
        // as it is on the byte path. Letting the cart branch win here routes the
        // debug channel's read-back to ROM and breaks the detection handshake
        // the suite uses to select it.
        if Self::is_pi_bus(addr) && !Self::is_isviewer(addr) {
            if self.pi_io_busy() {
                return self.pi_write_latch;
            }
            return u32::from_be_bytes([
                self.cart.pi_read(addr),
                self.cart.pi_read(addr.wrapping_add(1)),
                self.cart.pi_read(addr.wrapping_add(2)),
                self.cart.pi_read(addr.wrapping_add(3)),
            ]);
        }
        // Registers with a **side effect on read** must be read exactly once.
        //
        // `SP_SEMAPHORE` takes the mutex when read, so composing a word out of
        // four byte reads took it four times: the first byte saw 0 and the rest
        // saw 1, and the assembled word came back as 1 where hardware returns 0.
        // n64-systemtest's `SP Semaphore Register (CPU only)` catches exactly
        // that. On hardware the RCP returns the whole aligned word for one
        // access regardless of size, so one access is the correct model.
        if Self::is_sp_register(addr) {
            return self.rsp.sp.read((addr >> 2) & 7);
        }
        if Self::is_mi_register(addr) {
            return self.mi_read(addr);
        }
        if Self::is_dp_register(addr) {
            return self.rdp.dpc_read((addr >> 2) & 7);
        }
        if Self::is_vi_register(addr) {
            return self.vi.read(addr >> 2);
        }
        if Self::is_ai_register(addr) {
            return self.audio.read_reg((addr >> 2) & 7);
        }
        if Self::is_ri_register(addr) {
            return self.rcp.ri[((addr >> 2) & 7) as usize];
        }
        if Self::is_si_register(addr) {
            return self.si_read(addr);
        }
        if Self::is_pif(addr) {
            if addr >= Self::PIF_RAM_BASE {
                let off = (addr - Self::PIF_RAM_BASE) as usize;
                return u32::from_be_bytes([
                    self.cart.pif_read(off),
                    self.cart.pif_read(off + 1),
                    self.cart.pif_read(off + 2),
                    self.cart.pif_read(off + 3),
                ]);
            }
            // PIF boot ROM (IPL1/IPL2): mapped only on the real-PIF path (0 under
            // HLE). This is the CPU instruction-fetch path from the reset vector.
            let off = (addr - Self::PIF_ROM_BASE) as usize;
            return u32::from_be_bytes([
                self.cart.pif_boot_rom_read(off),
                self.cart.pif_boot_rom_read(off + 1),
                self.cart.pif_boot_rom_read(off + 2),
                self.cart.pif_boot_rom_read(off + 3),
            ]);
        }
        u32::from_be_bytes([
            self.read_u8(addr),
            self.read_u8(addr.wrapping_add(1)),
            self.read_u8(addr.wrapping_add(2)),
            self.read_u8(addr.wrapping_add(3)),
        ])
    }

    fn write_u8(&mut self, addr: u32, val: u8) {
        if let Some(off) = Self::rdram_offset(addr) {
            self.rdram[off] = val;
            self.mark_rdram_dirty(off);
            return;
        }
        if Self::is_pi_register(addr) {
            // PI registers are **32-bit only**, and a byte write to one is not
            // something real code does. Assembling a word by read-modify-write
            // is actively wrong for two of them:
            //
            //   * the length registers *trigger* on write, so a byte-wise RMW
            //     starts a DMA per byte with a partly assembled length;
            //   * `PI_STATUS`'s read bits (busy, interrupt) do not correspond to
            //     its write bits (reset, clear-interrupt), so reading it back to
            //     fill in the other three bytes fabricates command strobes from
            //     status flags.
            //
            // Only the address registers can be safely assembled, so only they
            // are. A byte write to anything else is dropped rather than guessed
            // at -- an explicit nothing beats a plausible wrong action.
            if matches!(addr & !3, pi::PI_DRAM_ADDR | pi::PI_CART_ADDR) {
                let shift = 8 * (3 - (addr & 3));
                let w = (self.pi.read(addr) & !(0xFF << shift)) | (u32::from(val) << shift);
                self.pi_write_word(addr, w);
            }
            return;
        }
        if Self::is_spmem(addr) {
            self.rsp.mem_write(addr - Self::SPMEM_BASE, val);
            return;
        }
        if Self::is_isviewer(addr) {
            if let Some(b) = self.isviewer.get_mut((addr - Self::ISVIEWER_BASE) as usize) {
                *b = val;
            }
            return;
        }
        if Self::is_pif(addr) {
            if addr >= Self::PIF_RAM_BASE {
                let off = (addr - Self::PIF_RAM_BASE) as usize;
                self.cart.pif_write(off, val);
                self.pif_boot_command_if_cmd(off);
            }
            return;
        }
        // TODO(T-CORE-01): decode + dispatch the remaining RCP register windows —
        // specifically the RDRAM device registers (`0x03F0_0000`). Note this is
        // the **byte**-write path: RCP register blocks are reached through
        // `write_sized`, which funnels to `write_u32`, so their absence here is by
        // design rather than an omission.
        self.cart.pi_write(addr, val);
    }

    /// Model the RCP's **size-blind** write path.
    ///
    /// Everything on the RCP's internal bus latches the whole 32-bit word the
    /// VR4300 put on `SysAD`, ignoring both the access size and the low two
    /// address bits (N64brew *Memory map* §Physical Memory Map accesses). The
    /// VR4300 has already shifted the source register into the byte lane the
    /// address selects, so a narrow store writes that shifted register —
    /// **including the bits above the stored byte**, which is why the effect
    /// looks like zero-fill rather than a partial update.
    ///
    /// n64-systemtest states the rule outright in its own header comment
    /// (`src/tests/sp_memory/mod.rs`): *"SH/SB are broken: they overwrite the
    /// whole 32 bit, filling everything that isn't written with zeroes. SD is
    /// broken: it only writes the upper 32 bit of the value, touching only 4
    /// bytes."* With `$3 = 0x1234_5678`, `SB $3, 5(spmem)` leaves `0x5678_0000`
    /// in the word at offset 4 — the register shifted left 16, not the byte
    /// `0x78`.
    ///
    /// RDRAM is excluded because the RI passes the low address bits and the
    /// access size on to the RDRAM devices, which build a real byte mask from
    /// them; only the RCP's internal path throws that information away.
    fn write_sized(&mut self, addr: u32, width: u64, value: u64) {
        // Unsupported widths do nothing, matching the default `write_sized`.
        // `StoreKind::width` only ever yields 1/2/4/8 so nothing reaches this
        // today, but without the guard the internal-bus arm below would accept
        // any width and *store* -- so the two paths would disagree about what a
        // width of 3 means, which is exactly the kind of divergence that is
        // discovered years later through a corrupted byte lane.
        if !matches!(width, 1 | 2 | 4 | 8) {
            return;
        }
        if !Self::is_rcp_internal(addr) {
            // RDRAM, the PI/SI external buses and the ISViewer keep byte-exact
            // semantics -- see `is_rcp_internal` for why the external buses are
            // not folded in here yet.
            match width {
                1 => self.write_u8(addr, value as u8),
                2 => {
                    self.write_u8(addr, (value >> 8) as u8);
                    self.write_u8(addr.wrapping_add(1), value as u8);
                }
                4 => self.write_u32(addr, value as u32),
                8 => {
                    self.write_u32(addr, (value >> 32) as u32);
                    self.write_u32(addr.wrapping_add(4), value as u32);
                }
                _ => {}
            }
            return;
        }
        let word = match width {
            // 64-bit: the two words go out MSB-first and the RCP takes the
            // first, dropping the second entirely -- so a `SD` touches four
            // bytes, not eight.
            8 => (value >> 32) as u32,
            4 => value as u32,
            // Narrow: the register as the VR4300 placed it on the bus.
            //
            // Saturating, not because the invariant is in doubt but because it
            // is enforced somewhere else. MIPS requires natural alignment and
            // the CPU raises `AddressError` before a misaligned store ever
            // reaches the bus, so `width + (addr & 3) <= 4` holds for every
            // access that gets here — but this is a public trait method, and a
            // caller that breaks the invariant should get a defined byte lane
            // rather than an underflow that panics in debug and silently
            // becomes an over-wide shift in release.
            w => {
                let lane = 4u32.saturating_sub(w as u32).saturating_sub(addr & 3);
                (value as u32) << (8 * lane)
            }
        };
        self.write_u32(addr & !3, word);
    }

    fn write_u32(&mut self, addr: u32, val: u32) {
        // SP DMA registers. Handled here, at word granularity, for the same
        // reason as the PI: the default byte-wise path would fire four DMAs for
        // one `sw` to a length register.
        // A PI direct-I/O write latches and returns immediately; the transfer
        // finalizes in the background. Further writes while one is in flight are
        // ignored -- not queued.
        if Self::is_pi_bus(addr) && !Self::is_isviewer(addr) {
            if !self.pi_io_busy() {
                self.pi_write_latch = val;
                self.pi_write_countdown = Self::PI_WRITE_CYCLES;
                // A direct-I/O write must reach a *writable* PI device: SRAM, the
                // FlashRAM page buffer, or the FlashRAM Command register. The cart
                // ignores writes to the read-only ROM window, so this is safe to
                // call for any PI-bus address. (The latch above models the
                // read-back-while-busy timing; this performs the actual store.)
                self.cart.pi_write_word(addr, val);
            }
            return;
        }
        if Self::is_sp_register(addr) {
            self.sp_register_write(addr, val);
            return;
        }
        if Self::is_mi_register(addr) {
            self.mi_write(addr, val);
            return;
        }
        if Self::is_dp_register(addr) {
            self.rdp.dpc_write((addr >> 2) & 7, val);
            return;
        }
        if Self::is_vi_register(addr) {
            self.vi_write(addr, val);
            return;
        }
        if Self::is_ai_register(addr) {
            self.ai_write(addr, val);
            return;
        }
        if Self::is_ri_register(addr) {
            self.rcp.ri[((addr >> 2) & 7) as usize] = val;
            return;
        }
        if Self::is_si_register(addr) {
            self.si_write(addr, val);
            return;
        }
        if Self::is_pif(addr) {
            if addr >= Self::PIF_RAM_BASE {
                let off = (addr - Self::PIF_RAM_BASE) as usize;
                for (i, b) in val.to_be_bytes().into_iter().enumerate() {
                    self.cart.pif_write(off + i, b);
                }
                // IPL2 writes the command *word* at PIF-RAM 0x3C (`sw` to
                // 0xBFC007FC), so a word store is the usual boot-command path.
                self.pif_boot_command_if_cmd(off + 3);
            }
            return;
        }
        if addr & !3 == Self::SP_PC {
            self.rsp.sp.set_pc(val);
            return;
        }
        if addr == Self::ISVIEWER_WRITE_LEN {
            // Flushing is triggered by the LENGTH write, not by the buffer
            // writes -- so the guest assembles a whole line and then publishes
            // it. Capturing on buffer writes instead would interleave partial
            // lines and make the output unreadable.
            let n = (val as usize).min(Self::ISVIEWER_LEN);
            let base = (Self::ISVIEWER_BUF - Self::ISVIEWER_BASE) as usize;
            let bytes = &self.isviewer[base..base + n];
            self.isviewer_out.extend_from_slice(bytes);
            return;
        }
        if Self::is_isviewer(addr) {
            // Bounds-checked, not indexed. `addr` comes from guest code, so a
            // word write starting in the last three bytes of the window --
            // which `is_isviewer` accepts -- would index past the slice and
            // **panic the emulator**. A guest must never be able to do that.
            let off = (addr - Self::ISVIEWER_BASE) as usize;
            if let Some(dst) = self.isviewer.get_mut(off..off + 4) {
                dst.copy_from_slice(&val.to_be_bytes());
            }
            return;
        }
        // **A PI register write must be a single WORD write.**
        //
        // The default `write_u32` composes four `write_u8` calls, and PI
        // registers were handled byte-wise -- so a normal guest `sw` to
        // `PI_WR_LEN` started **four DMAs**, one per byte, each with a partly
        // assembled length. Every PI transfer was wrong, and the failure looks
        // like memory corruption rather than a DMA bug.
        if Self::is_pi_register(addr) {
            self.pi_write_word(addr, val);
            return;
        }
        if let Some(off) = Self::rdram_offset(addr) {
            // The fast path, avoiding four bounds checks for the common case.
            let b = val.to_be_bytes();
            if off + 3 < self.rdram.len() {
                self.rdram[off..=off + 3].copy_from_slice(&b);
                self.mark_rdram_dirty_range(off, 4);
                return;
            }
        }
        let b = val.to_be_bytes();
        for (i, byte) in b.iter().enumerate() {
            self.write_u8(addr.wrapping_add(i as u32), *byte);
        }
    }

    fn emux_enabled(&self) -> bool {
        self.emux_enabled
    }

    fn emux_log(&mut self, bytes: &[u8]) {
        self.emux_out.extend_from_slice(bytes);
    }

    fn emux_exit(&mut self) {
        self.emux_exited = true;
    }

    fn poll_irq(&mut self) -> bool {
        // IP2 asserts when an unmasked MI line is set. The run-cycle gate and the
        // DC-stage sampling point live in the CPU pipeline (ADR 0007); this only
        // reports the level.
        let i = self.rcp.mi_intr;
        let m = self.rcp.mi_mask;
        (i.sp && m.sp)
            || (i.si && m.si)
            || (i.ai && m.ai)
            || (i.vi && m.vi)
            || (i.pi && m.pi)
            || (i.dp && m.dp)
    }
}

// --- The shared RDRAM bus (used by the RDP/RSP/AI DMA paths). ---
impl RdramBus for Bus {
    fn rdram_read(&self, addr: u32) -> u8 {
        Self::rdram_offset(addr).map_or(0, |off| self.rdram[off])
    }

    fn rdram_write(&mut self, addr: u32, val: u8) {
        if let Some(off) = Self::rdram_offset(addr) {
            self.rdram[off] = val;
            self.mark_rdram_dirty(off);
        }
    }

    fn rdram_read_hidden(&self, addr: u32) -> u8 {
        // Two bits per 16-bit halfword, packed four halfwords to a byte. `None`
        // (never written) reads 0.
        match (&self.rdram_hidden, Self::rdram_offset(addr)) {
            (Some(hidden), Some(off)) => {
                let halfword = off >> 1;
                let shift = (halfword & 3) * 2;
                (hidden[halfword >> 2] >> shift) & 0x3
            }
            _ => 0,
        }
    }

    fn rdram_write_hidden(&mut self, addr: u32, val: u8) {
        if let Some(off) = Self::rdram_offset(addr) {
            let hidden = self
                .rdram_hidden
                .get_or_insert_with(|| alloc::vec![0u8; RDRAM_SIZE / 8].into_boxed_slice());
            let halfword = off >> 1;
            let shift = (halfword & 3) * 2;
            let byte = &mut hidden[halfword >> 2];
            *byte = (*byte & !(0x3 << shift)) | ((val & 0x3) << shift);
        }
    }
}

// --- The RDP's narrow view. ---
impl VideoBus for Bus {
    fn raise_dp_interrupt(&mut self) {
        self.rcp.mi_intr.dp = true;
    }
}

// --- The RSP's narrow view. ---
// --- The AI's narrow view. ---
impl AudioBus for Bus {
    fn ai_dma_read_u32(&self, addr: u32) -> u32 {
        <Self as RdramBus>::rdram_read_u32(self, addr)
    }
    fn raise_ai_interrupt(&mut self) {
        self.rcp.mi_intr.ai = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rdram_round_trips_through_cpu_view() {
        let mut bus = Bus::new();
        CpuBus::write_u8(&mut bus, 0x0000_1234, 0xAB);
        assert_eq!(CpuBus::read_u8(&mut bus, 0x0000_1234), 0xAB);
    }

    #[test]
    fn rdram_hidden_bits_lazy_round_trip_and_masked() {
        let mut bus = Bus::new();
        // Unallocated at power-on; reads back clear.
        assert!(bus.rdram_hidden.is_none());
        assert_eq!(bus.rdram_read_hidden(0x1000), 0);
        // First write allocates and stores the 2-bit value.
        bus.rdram_write_hidden(0x1000, 0x3);
        assert!(bus.rdram_hidden.is_some(), "allocated on first write");
        assert_eq!(bus.rdram_read_hidden(0x1000), 0x3);
        // Bit-packed four halfwords to a byte: the adjacent halfword shares the
        // byte but not the bits, so writing it must not clobber the first.
        assert_eq!(bus.rdram_read_hidden(0x1002), 0);
        bus.rdram_write_hidden(0x1002, 0x2);
        assert_eq!(bus.rdram_read_hidden(0x1002), 0x2);
        assert_eq!(bus.rdram_read_hidden(0x1000), 0x3, "neighbor unchanged");
        // Only the low 2 bits are kept.
        bus.rdram_write_hidden(0x1000, 0x5);
        assert_eq!(bus.rdram_read_hidden(0x1000), 0x1);
        assert_eq!(bus.rdram_read_hidden(0x1002), 0x2, "still independent");
    }

    #[test]
    fn dp_interrupt_sets_mi_line() {
        let mut bus = Bus::new();
        VideoBus::raise_dp_interrupt(&mut bus);
        assert!(bus.rcp.mi_intr.dp);
        assert!(bus.rcp.mi_intr.any());
    }

    #[test]
    fn masked_irq_drives_ip2() {
        let mut bus = Bus::new();
        bus.rcp.mi_intr.ai = true;
        bus.rcp.mi_mask.ai = true;
        assert!(CpuBus::poll_irq(&mut bus));
    }

    /// **The AI register block is CPU-addressable and drives audio end to end.**
    /// **Stepping the AI one master tick at a time still emits every sample.**
    ///
    /// [`Bus::audio_tick`] skips the `core::mem::take` on the steps whose bus-free
    /// half reports nothing due — the overwhelming majority. This drives the real
    /// per-tick cadence rather than one large jump, so every skip is exercised, and
    /// asserts the emitted stream is exactly what an unskipped run produces.
    ///
    /// The oracle is the RDRAM the test wrote, not another run of this path: both
    /// sample values are asserted against the words placed at `0x2000`, and the
    /// count against the 8-byte `AI_LENGTH`. Mutation-checked — forcing the take to
    /// be skipped unconditionally turns this red on the count, which is a clearer
    /// signal than the index-out-of-bounds panic that the only other covering test
    /// produced.
    #[test]
    fn skipping_the_take_never_skips_a_sample() {
        let mut bus = Bus::new();
        CpuBus::write_u32(&mut bus, 0x0000_2000, 0x1111_2222);
        CpuBus::write_u32(&mut bus, 0x0000_2004, 0x3333_4444);
        CpuBus::write_u32(&mut bus, Bus::AI_REGS_BASE + 0x10, 1103); // AI_DACRATE
        CpuBus::write_u32(&mut bus, Bus::AI_REGS_BASE + 0x08, 1); // AI_CONTROL
        CpuBus::write_u32(&mut bus, Bus::AI_REGS_BASE, 0x2000); // AI_DRAM_ADDR
        CpuBus::write_u32(&mut bus, Bus::AI_REGS_BASE + 0x04, 8); // AI_LENGTH

        let period = rustyn64_audio::MASTER_HZ / u64::from(bus.audio.sample_rate());
        // Two periods, one master tick at a time: thousands of skipped takes and a
        // handful of real ones.
        for now in 1..=(period * 2) {
            bus.audio_tick(now);
        }
        let stepped = bus.drain_audio_samples();

        assert_eq!(
            stepped.len(),
            2,
            "both buffered samples must come out of the per-tick cadence"
        );
        assert_eq!(
            stepped[0],
            StereoSample {
                left: 0x1111,
                right: 0x2222
            }
        );
        assert_eq!(
            stepped[1],
            StereoSample {
                left: 0x3333,
                right: 0x4444
            }
        );
    }

    /// Programming `AI_DACRATE`/`AI_CONTROL`/`AI_DRAM_ADDR`/`AI_LENGTH` through
    /// the memory-mapped path at `0x0450_0000` starts a transfer that raises
    /// `MI_INTR.ai` on enqueue, mirrors `AI_LENGTH` on the write-only registers,
    /// acknowledges on an `AI_STATUS` write, and emits the RDRAM samples as the
    /// derived-timing DAC advances.
    #[test]
    fn ai_registers_drive_audio_through_the_cpu_bus() {
        let mut bus = Bus::new();
        // Two stereo pairs at RDRAM 0x2000.
        CpuBus::write_u32(&mut bus, 0x0000_2000, 0x1111_2222);
        CpuBus::write_u32(&mut bus, 0x0000_2004, 0x3333_4444);
        // Program the AI: ~44 kHz, DMA enabled, buffer at 0x2000, 8 bytes.
        CpuBus::write_u32(&mut bus, Bus::AI_REGS_BASE + 0x10, 1103); // AI_DACRATE
        CpuBus::write_u32(&mut bus, Bus::AI_REGS_BASE + 0x08, 1); // AI_CONTROL
        CpuBus::write_u32(&mut bus, Bus::AI_REGS_BASE, 0x2000); // AI_DRAM_ADDR
        CpuBus::write_u32(&mut bus, Bus::AI_REGS_BASE + 0x04, 8); // AI_LENGTH
        assert!(
            bus.rcp.mi_intr.ai,
            "enqueuing the first buffer raises the AI line"
        );
        // Write-only registers read back the AI_LENGTH mirror (remaining bytes).
        assert_eq!(CpuBus::read_u32(&mut bus, Bus::AI_REGS_BASE + 0x10), 8);
        // AI_STATUS reports BUSY and ENABLED.
        let status = CpuBus::read_u32(&mut bus, Bus::AI_REGS_BASE + 0x0C);
        assert_ne!(status & (1 << 30), 0, "BUSY");
        assert_ne!(status & (1 << 25), 0, "ENABLED");
        // A write to AI_STATUS acknowledges the interrupt.
        CpuBus::write_u32(&mut bus, Bus::AI_REGS_BASE + 0x0C, 0);
        assert!(!bus.rcp.mi_intr.ai, "an AI_STATUS write acks the interrupt");
        // Advance the DAC and drain the emitted samples.
        let period = rustyn64_audio::MASTER_HZ / u64::from(bus.audio.sample_rate());
        bus.audio_tick(period * 2);
        let samples = bus.drain_audio_samples();
        assert_eq!(
            samples[0],
            StereoSample {
                left: 0x1111,
                right: 0x2222
            }
        );
        assert_eq!(
            samples[1],
            StereoSample {
                left: 0x3333,
                right: 0x4444
            }
        );
    }

    /// **A `Sync Full` command drives the DP interrupt through to the CPU.** A
    /// `0x29` command word placed in RDRAM and consumed by `rdp_tick` raises
    /// `MI_INTR.dp`; once the DP line is masked in it asserts IP2, which is how
    /// the CPU comes to service the RDP-done interrupt. This is the end-to-end
    /// path for Phase 3's `Sync Full` — the RDP dispatcher, the `VideoBus` seam,
    /// the MI line, and the mask, together.
    #[test]
    fn a_sync_full_command_drives_the_dp_interrupt_to_ip2() {
        let mut bus = Bus::new();
        // A Sync Full command (opcode 0x29 in bits 61:56) at RDRAM 0x100.
        bus.rdram[0x100] = 0x29;
        // Point the DP FIFO at it: a single 8-byte command.
        bus.rdp.dpc_write(0, 0x100); // DPC_START (sets START_VALID)
        bus.rdp.dpc_write(1, 0x108); // DPC_END  (copies START -> CURRENT)

        assert!(!bus.rcp.mi_intr.dp, "DP line clear before the command runs");
        bus.rdp_tick();
        assert!(bus.rcp.mi_intr.dp, "Sync Full raised the DP line");

        bus.rcp.mi_mask.dp = true;
        assert!(CpuBus::poll_irq(&mut bus), "the masked DP line asserts IP2");
    }

    /// **VI registers round-trip through the CPU bus, and a `VI_V_CURRENT` write
    /// acknowledges the VI interrupt.** The block is at `0x0440_0000`; a write to
    /// `VI_V_CURRENT` (+0x10) clears `MI_INTR.vi`, the interrupt-ack path.
    #[test]
    fn vi_registers_round_trip_and_v_current_acks_the_interrupt() {
        let mut bus = Bus::new();
        // VI_ORIGIN (+0x04) is an ordinary latch.
        CpuBus::write_u32(&mut bus, Bus::VI_REGS_BASE + 0x04, 0x0010_0000);
        assert_eq!(
            CpuBus::read_u32(&mut bus, Bus::VI_REGS_BASE + 0x04),
            0x0010_0000,
            "VI_ORIGIN round-trips"
        );
        // A pending VI interrupt is cleared by writing VI_V_CURRENT (+0x10).
        bus.rcp.mi_intr.vi = true;
        CpuBus::write_u32(&mut bus, Bus::VI_REGS_BASE + 0x10, 0x42);
        assert!(
            !bus.rcp.mi_intr.vi,
            "writing VI_V_CURRENT acks the interrupt"
        );
        // ... and the write did not latch into V_CURRENT.
        assert_eq!(CpuBus::read_u32(&mut bus, Bus::VI_REGS_BASE + 0x10), 0);
    }

    /// **VI registers latch the whole word regardless of store size, and the
    /// block mirrors every 16 words.** The VI is on the RCP-internal bus, so
    /// `write_sized` routes 8-/16-/64-bit stores through `write_u32`; a byte read
    /// recovers the addressed lane; a mirrored address decodes to the same
    /// register; and a narrow `VI_V_CURRENT` write still acks without latching.
    #[test]
    fn vi_accesses_are_size_blind_and_mirrored() {
        let mut bus = Bus::new();
        // An 8-bit store to VI_WIDTH (+0x08) latches the shifted register across
        // the whole word (RCP-internal, size-blind).
        CpuBus::write_sized(&mut bus, Bus::VI_REGS_BASE + 0x08, 1, 0x44);
        assert_eq!(
            CpuBus::read_u32(&mut bus, Bus::VI_REGS_BASE + 0x08),
            0x4400_0000,
            "SB latches the shifted register across the whole word"
        );
        // A word store (VI_WIDTH = 320 = 0x0000_0140), then a byte read recovers
        // the addressed lane — the low byte at +0x0B is 0x40.
        CpuBus::write_sized(&mut bus, Bus::VI_REGS_BASE + 0x08, 4, 320);
        assert_eq!(CpuBus::read_u8(&mut bus, Bus::VI_REGS_BASE + 0x0B), 0x40);
        // A 64-bit store writes its upper word to VI_ORIGIN (+0x04).
        CpuBus::write_sized(&mut bus, Bus::VI_REGS_BASE + 0x04, 8, 0x0010_0000_DEAD_BEEF);
        assert_eq!(
            CpuBus::read_u32(&mut bus, Bus::VI_REGS_BASE + 0x04),
            0x0010_0000
        );
        // The block mirrors every 16 words: +0x40 decodes to VI_CTRL (offset 0).
        CpuBus::write_u32(&mut bus, Bus::VI_REGS_BASE + 0x40, 0x3);
        assert_eq!(CpuBus::read_u32(&mut bus, Bus::VI_REGS_BASE), 0x3);
        // A narrow (8-bit) VI_V_CURRENT write still acks without latching.
        bus.rcp.mi_intr.vi = true;
        CpuBus::write_sized(&mut bus, Bus::VI_REGS_BASE + 0x10, 1, 0x99);
        assert!(!bus.rcp.mi_intr.vi, "a narrow VI_V_CURRENT write acks");
        assert_eq!(CpuBus::read_u32(&mut bus, Bus::VI_REGS_BASE + 0x10), 0);
    }

    /// **Scan-out converts the framebuffer to RGBA8.** 32-bit RGBA8888 is a
    /// direct copy; 16-bit RGBA5551 expands each 5-bit channel to 8 and the
    /// 1-bit alpha to 0/255. Height comes from `VI_V_VIDEO`'s active half-lines.
    #[test]
    fn scanout_converts_32bit_and_16bit_framebuffers() {
        let mut bus = Bus::new();
        let fb = 0x100usize;
        // A 2x2 32-bit framebuffer, row-major.
        let px32 = [0xAABB_CCDDu32, 0x1122_3344, 0x5566_7788, 0x99AA_BBCC];
        for (i, p) in px32.iter().enumerate() {
            bus.rdram[fb + i * 4..fb + i * 4 + 4].copy_from_slice(&p.to_be_bytes());
        }
        // VI: 32-bit, origin 0x100, width 2, V_VIDEO active = 4 half-lines (h=2).
        bus.vi.regs[vi::VI_CTRL as usize] = 3;
        bus.vi.regs[vi::VI_ORIGIN as usize] = fb as u32;
        bus.vi.regs[vi::VI_WIDTH as usize] = 2;
        bus.vi.regs[vi::VI_V_VIDEO as usize] = 4; // start 0, end 4 -> 2 lines
        let mut out = alloc::vec![0u8; 2 * 2 * 4];
        assert_eq!(bus.scanout(&mut out), (2, 2));
        assert_eq!(&out[0..4], &[0xAA, 0xBB, 0xCC, 0xDD], "direct 32-bit copy");
        assert_eq!(&out[12..16], &[0x99, 0xAA, 0xBB, 0xCC]);

        // 16-bit RGBA5551, non-uniform channels so component order and the
        // field shifts are exercised: 0x0887 -> R=1,G=2,B=3,A=1 = [08,10,18,FF];
        // 0x0886 is the same color with alpha 0 = [08,10,18,00].
        bus.rdram[fb..fb + 2].copy_from_slice(&0x0887u16.to_be_bytes());
        bus.rdram[fb + 2..fb + 4].copy_from_slice(&0x0886u16.to_be_bytes());
        bus.vi.regs[vi::VI_CTRL as usize] = 2;
        bus.vi.regs[vi::VI_V_VIDEO as usize] = 2; // h = 1
        let mut out16 = alloc::vec![0u8; 2 * 4];
        assert_eq!(bus.scanout(&mut out16), (2, 1));
        assert_eq!(
            &out16[0..4],
            &[0x08, 0x10, 0x18, 0xFF],
            "distinct channels, A=1"
        );
        assert_eq!(&out16[4..8], &[0x08, 0x10, 0x18, 0x00], "same color, A=0");
    }

    /// **A blanked VI scans out nothing.** With a non-zero sentinel in the
    /// destination, both blank types (`TYPE == 0`, the power-on default, and
    /// `TYPE == 1`) leave it untouched — a zero-filled buffer would pass even if
    /// the blank path erroneously wrote zeroes.
    #[test]
    fn scanout_is_blank_when_the_vi_is_off() {
        for blank_type in [0u32, 1] {
            let mut bus = Bus::new();
            bus.vi.regs[vi::VI_CTRL as usize] = blank_type;
            bus.vi.regs[vi::VI_WIDTH as usize] = 2;
            bus.vi.regs[vi::VI_V_VIDEO as usize] = 4;
            let mut out = alloc::vec![0xA5u8; 16];
            assert_eq!(bus.scanout(&mut out), (0, 0), "TYPE {blank_type}: no frame");
            assert!(out.iter().all(|&b| b == 0xA5), "sentinel untouched");
        }
    }

    /// **An undersized destination is refused up front.** Rather than write a
    /// truncated frame and claim full dimensions, `scanout` returns `(0, 0)` and
    /// writes nothing when `out` cannot hold `width * height * 4` bytes.
    #[test]
    fn scanout_refuses_an_undersized_buffer() {
        let mut bus = Bus::new();
        bus.vi.regs[vi::VI_CTRL as usize] = 3; // 32-bit
        bus.vi.regs[vi::VI_WIDTH as usize] = 2;
        bus.vi.regs[vi::VI_V_VIDEO as usize] = 4; // h = 2 -> needs 2*2*4 = 16 bytes
        let mut out = alloc::vec![0xFFu8; 8]; // too small (< 16)
        assert_eq!(bus.scanout(&mut out), (0, 0), "undersized: refused");
        assert!(out.iter().all(|&b| b == 0xFF), "and left untouched");
    }

    /// The memo must be invisible: a scan-out that consults it has to produce exactly
    /// what one that never does would.
    ///
    /// This is the test that would catch a wrong key, a stale row, or an eviction that
    /// keeps the wrong row — none of which the geometry tests above can see, because
    /// they read a single pixel. It recomputes every output pixel from
    /// [`Bus::vi_sample_direct`], which bypasses the memo entirely, and compares the
    /// whole buffer.
    ///
    /// The framebuffer is filled with a pattern that varies per pixel in *both* axes
    /// and sets coverage bits unevenly, so a sample taken from the wrong column, the
    /// wrong row, or the wrong coverage class differs in its bytes. A flat fill would
    /// pass with the memo returning any pixel at all.
    #[test]
    fn memoized_scanout_matches_uncached_recomputation() {
        for &(ctrl, x_scale, y_scale) in &[
            // The coverage path with divot + de-dither, at the 2x horizontal upscale
            // that makes the memo worth having (what Super Mario 64 programs).
            (0x0001_3016u32, 0x0000_0200u32, 0x0000_0400u32),
            // Same filters, a vertical fraction as well, so both memo rows are live.
            (0x0001_3016, 0x0000_0200, 0x0000_0300),
            // Coverage path without divot, 1:1.
            (0x0001_3006, 0x0000_0400, 0x0000_0400),
            // Downscale: consecutive output pixels skip source columns, so the memo
            // mostly misses and the eviction path runs hot.
            (0x0001_3016, 0x0000_0900, 0x0000_0400),
        ] {
            let mut bus = Bus::new();
            let fb = 0x2000usize;
            let stride = 64u32;
            for i in 0..(stride as usize * 64) {
                // Vary both axes and leave coverage bit 0 set on only some pixels.
                let px = ((i * 7919) % 0xFFFF) as u16;
                let off = fb + i * 2;
                bus.rdram[off..off + 2].copy_from_slice(&px.to_be_bytes());
            }
            bus.vi.regs[vi::VI_CTRL as usize] = ctrl;
            bus.vi.regs[vi::VI_ORIGIN as usize] = fb as u32;
            bus.vi.regs[vi::VI_WIDTH as usize] = stride;
            bus.vi.regs[vi::VI_V_TOTAL as usize] = 525;
            bus.vi.regs[vi::VI_H_VIDEO as usize] = (108 << 16) | 0x94;
            bus.vi.regs[vi::VI_V_VIDEO as usize] = (34 << 16) | 0x54;
            bus.vi.regs[vi::VI_X_SCALE as usize] = x_scale;
            bus.vi.regs[vi::VI_Y_SCALE as usize] = y_scale;

            let mut memoized = alloc::vec![0u8; 640 * 64 * 4];
            let (w, h) = bus.scanout_scaled(&mut memoized);
            assert!(
                w > 0 && h > 0,
                "ctrl {ctrl:#x}: scan-out produced no pixels"
            );

            // The same walk with the memo disabled. `span == 0` makes every lookup
            // fall through to `vi_sample_direct`, which is the uncached path.
            let cfg = ViCfg {
                origin: bus.vi.read(vi::VI_ORIGIN) & 0x00FF_FFFF,
                src_stride: i32::try_from(bus.vi.read(vi::VI_WIDTH) & 0xFFF).expect("12-bit"),
                bpp: 2,
                aa_mode: (ctrl >> 8) & 0x3,
                divot: (ctrl >> 4) & 1 != 0,
                dither_filter: (ctrl >> 16) & 1 != 0,
            };
            let mut bypass = ViSampler::new(cfg, 0, -1);
            assert_eq!(bypass.span, 0, "the bypass sampler must cache nothing");

            let x_add = i32::try_from(x_scale & 0xFFF).expect("12-bit field");
            let y_add = i32::try_from(y_scale & 0xFFF).expect("12-bit field");
            let x_start = i32::try_from((x_scale >> 16) & 0xFFF).expect("12-bit field");
            let y_start = i32::try_from((y_scale >> 16) & 0xFFF).expect("12-bit field");
            let (wi, hi) = (
                i32::try_from(w).expect("width fits i32"),
                i32::try_from(h).expect("height fits i32"),
            );
            for oy in 0..hi {
                let curry = y_start + oy * y_add;
                let (sy, yfrac) = (curry >> 10, (curry >> 5) & 0x1F);
                for ox in 0..wi {
                    // `8` is `minhpass`: `VI_H_VIDEO`'s start is 108, the NTSC
                    // overscan adjust takes it to `h_start = 0`, and an unclamped
                    // `h_start` crops 8 columns. Output column 0 samples source
                    // column 8.
                    let x_offs = x_start + (8 + ox) * x_add;
                    let (sx, xfrac) = (x_offs >> 10, (x_offs >> 5) & 0x1F);
                    let want = if xfrac != 0 || yfrac != 0 {
                        let col = bus.vi_column(&mut bypass, sx, sy, yfrac);
                        if xfrac == 0 {
                            col
                        } else {
                            let ncol = bus.vi_column(&mut bypass, sx + 1, sy, yfrac);
                            vi_lerp3(col, ncol, xfrac)
                        }
                    } else {
                        bus.vi_sample(&mut bypass, sx, sy)
                    };
                    let dst = usize::try_from((oy * wi + ox) * 4).expect("non-negative index");
                    assert_eq!(
                        &memoized[dst..dst + 3],
                        &want,
                        "ctrl {ctrl:#x} x_scale {x_scale:#x}: output ({ox}, {oy}) \
                         disagrees with the uncached recomputation"
                    );
                }
            }
        }
    }

    /// Eviction keeps the row the walk is still using, and an unused row goes first.
    #[test]
    fn vi_sampler_evicts_the_lower_row() {
        let cfg = ViCfg {
            origin: 0,
            src_stride: 8,
            bpp: 2,
            aa_mode: 0,
            divot: false,
            dither_filter: false,
        };
        let mut s = ViSampler::new(cfg, 0, 7);
        assert_eq!(s.row_y, [None, None], "a fresh memo holds no rows");
        assert_eq!(s.row_slot(5), 0, "the first unused row is taken");
        assert_eq!(s.row_slot(6), 1, "then the second");
        assert_eq!(s.row_slot(5), 0, "an existing row is found, not re-taken");
        assert_eq!(s.row_slot(7), 0, "row 5 is evicted, not row 6");
        assert_eq!(s.row_y, [Some(7), Some(6)]);
    }

    /// A column outside the memo's range must fall through rather than index it, and
    /// an over-wide range must disable the memo rather than allocate for it.
    #[test]
    fn vi_sampler_falls_through_outside_its_range() {
        let cfg = ViCfg {
            origin: 0,
            src_stride: 8,
            bpp: 2,
            aa_mode: 0,
            divot: false,
            dither_filter: false,
        };
        let bus = Bus::new();
        let mut s = ViSampler::new(cfg, 10, 12);
        assert_eq!(s.span, 3);
        // Left of, right of, and inside the range all answer; only the last is cached.
        let _ = bus.vi_sample(&mut s, 9, 0);
        let _ = bus.vi_sample(&mut s, 13, 0);
        let _ = bus.vi_sample(&mut s, 11, 0);
        assert_eq!(
            s.cells.iter().filter(|c| c.is_some()).count(),
            1,
            "only the in-range column is memoized"
        );

        let huge = ViSampler::new(cfg, 0, i32::MAX);
        assert_eq!(huge.span, 0, "an over-wide range disables the memo");
        assert!(huge.cells.is_empty(), "and allocates nothing for it");
    }

    /// The other side of the skip: when the RDP *does* have a queued command,
    /// `rdp_tick` must still take the bus and retire it.
    ///
    /// The stall test above proves the skip path fires; on its own that is satisfied by
    /// a predicate that always skips, which would be a dead RDP. This pins the positive
    /// case end to end through `Bus::rdp_tick` — a `Sync Pipe` (0x27) placed in RDRAM,
    /// consumed, `cmd_current` advanced past it, and the documented stall applied.
    #[test]
    fn a_queued_command_is_retired_through_the_bus_half() {
        let mut bus = Bus::new();
        let fifo = 0x1000u32;
        // Sync Pipe: opcode 0x27 in the top byte, one 64-bit word, no operands.
        bus.rdram[fifo as usize] = 0x27;
        bus.rdp.cmd_current = fifo;
        bus.rdp.cmd_end = fifo + 8;

        let before = bus.rdp.commands_processed;
        bus.rdp_tick();

        assert_eq!(
            bus.rdp.commands_processed,
            before + 1,
            "the command must be consumed, not skipped"
        );
        assert_eq!(
            bus.rdp.cmd_current,
            fifo + 8,
            "and the FIFO pointer advanced past it"
        );
        assert!(
            bus.rdp.stall > 0,
            "Sync Pipe applies its documented pipeline stall"
        );
    }

    /// A stalling RDP still burns exactly one GCLK per RCP step through the skip path.
    ///
    /// The stall is the only thing the skip path **mutates**, so an ordering mistake
    /// lands here: checking the FIFO before the stall would leave a stalled RDP with
    /// nothing queued counting down forever, which no vector notices because it changes
    /// *when* a command retires rather than whether it matches.
    #[test]
    fn a_stalled_rdp_decrements_exactly_once_per_rcp_step() {
        let mut bus = Bus::new();
        bus.rdp.stall = 3;
        for expected in [2u32, 1, 0] {
            bus.rdp_tick();
            assert_eq!(
                bus.rdp.stall, expected,
                "one GCLK burned per step, never two"
            );
        }
        // And the step after the stall expires must reach the FIFO check rather
        // than wrapping the counter.
        bus.rdp_tick();
        assert_eq!(bus.rdp.stall, 0, "an expired stall stays expired");
    }

    /// **`scanout_scaled` geometry + truncating convert (R-5).** A hand-computed
    /// 1:1 case: `VI_H_VIDEO = 108..148` (NTSC overscan `-108` → `h_start = 0`,
    /// `minhpass = 8`, so output column 0 samples **source column 8**), one active
    /// line. Source column 8 holds `0x1234`; the truncating RGBA5551→8 conversion
    /// (`(px>>8)&0xF8`, `(px&0x7C0)>>3`, `(px&0x3E)<<2`) gives `[10,40,D0]` with an
    /// opaque display alpha. A `expand5`-style replicating conversion or a wrong
    /// overscan offset would change the bytes, so this pins both.
    #[test]
    fn scanout_scaled_geometry_and_truncating_convert() {
        let mut bus = Bus::new();
        let fb = 0x2000usize;
        // Source column 8 (byte fb + 8*2), row 0, of a 48-wide framebuffer.
        bus.rdram[fb + 16..fb + 18].copy_from_slice(&0x1234u16.to_be_bytes());
        bus.vi.regs[vi::VI_CTRL as usize] = 0x0302; // 16-bit, aa_mode=REPLICATE
        bus.vi.regs[vi::VI_ORIGIN as usize] = fb as u32;
        bus.vi.regs[vi::VI_WIDTH as usize] = 48;
        bus.vi.regs[vi::VI_V_TOTAL as usize] = 525; // NTSC (< 550)
        bus.vi.regs[vi::VI_H_VIDEO as usize] = (108 << 16) | 0x94; // hres 40 -> width 25
        bus.vi.regs[vi::VI_V_VIDEO as usize] = (34 << 16) | 0x24; // vres 1 -> height 1
        bus.vi.regs[vi::VI_X_SCALE as usize] = 0x0000_0400; // 1:1
        bus.vi.regs[vi::VI_Y_SCALE as usize] = 0x0000_0400;
        let mut out = alloc::vec![0u8; 25 * 4];
        assert_eq!(
            bus.scanout_scaled(&mut out),
            (25, 1),
            "overscan-cropped geometry"
        );
        assert_eq!(
            &out[0..4],
            &[0x10, 0x40, 0xD0, 0xFF],
            "column 0 samples source column 8, truncating 5551->8, opaque alpha"
        );
    }

    /// **`scanout_scaled` blanks and refuses an undersized buffer.** `TYPE` 0/1
    /// returns `(0, 0)` writing nothing; an `out` too small for `width*height*4`
    /// is refused rather than truncated.
    #[test]
    fn scanout_scaled_blanks_and_refuses_undersized() {
        let mut bus = Bus::new();
        // Blank (TYPE == 0), a valid geometry otherwise.
        bus.vi.regs[vi::VI_ORIGIN as usize] = 0x2000;
        bus.vi.regs[vi::VI_WIDTH as usize] = 48;
        bus.vi.regs[vi::VI_V_TOTAL as usize] = 525;
        bus.vi.regs[vi::VI_H_VIDEO as usize] = (108 << 16) | 0x94;
        bus.vi.regs[vi::VI_V_VIDEO as usize] = (34 << 16) | 0x24;
        bus.vi.regs[vi::VI_X_SCALE as usize] = 0x0000_0400;
        bus.vi.regs[vi::VI_Y_SCALE as usize] = 0x0000_0400;
        let mut out = alloc::vec![0xA5u8; 25 * 4];
        assert_eq!(bus.scanout_scaled(&mut out), (0, 0), "TYPE 0: blank");
        assert!(out.iter().all(|&b| b == 0xA5), "sentinel untouched");
        // Now enable it but give a too-small buffer.
        bus.vi.regs[vi::VI_CTRL as usize] = 0x0302;
        let mut small = alloc::vec![0xA5u8; 8]; // < 25*1*4
        assert_eq!(
            bus.scanout_scaled(&mut small),
            (0, 0),
            "undersized: refused"
        );
        assert!(small.iter().all(|&b| b == 0xA5), "and left untouched");
    }

    /// **`vi_lerp3` — the 5-bit bilinear lerp with `+16 >> 5` rounding (R-5).**
    /// `frac = 16` is a 50 % blend; `frac = 0` is an exact passthrough of `a`; and
    /// `frac = 2` with a diff of 8 exercises the rounding: `(8*2 + 16) >> 5 = 1`
    /// (vs `0` without the `+16`), so the result is `0x29`, not `0x28`.
    #[test]
    fn vi_lerp3_blends_and_rounds() {
        assert_eq!(
            vi_lerp3([0x20, 0, 0x20], [0x28, 0, 0x28], 16),
            [0x24, 0, 0x24]
        );
        assert_eq!(
            vi_lerp3([0x10, 0x20, 0x30], [0xFF, 0xFF, 0xFF], 0),
            [0x10, 0x20, 0x30],
            "frac 0 is an exact passthrough of a"
        );
        assert_eq!(
            vi_lerp3([0x28, 0, 0], [0x30, 0, 0], 2),
            [0x29, 0, 0],
            "the +16 rounding rounds 0x28 up to 0x29"
        );
    }

    /// **`vi_gamma` — the VI sqrt gamma curve (R-5).** `gamma(v) = sqrt(v << 6) << 1`:
    /// `gamma(0) = 0`, `gamma(0x40) = sqrt(0x1000) << 1 = 64 << 1 = 0x80`,
    /// `gamma(0x48) = sqrt(0x1200) << 1 = 67 << 1 = 0x86`, `gamma(0xFF) = sqrt(0x3FC0)
    /// << 1 = 127 << 1 = 0xFE`. The whole curve is then checked exhaustively against an
    /// **independent** floor-sqrt (`u32::isqrt`, a different implementation than
    /// `vi_integer_sqrt`), which also pins the precomputed `GAMMA_TABLE` to `vi_gamma`.
    /// Dropping the `<< 1` fails the anchor cases.
    #[test]
    fn vi_gamma_curve() {
        assert_eq!(vi_gamma(0), 0);
        assert_eq!(vi_gamma(0x40), 0x80);
        assert_eq!(vi_gamma(0x48), 0x86);
        assert_eq!(vi_gamma(0xFF), 0xFE);
        for v in 0..=255u8 {
            let reference = ((u32::from(v) << 6).isqrt() << 1) as u8;
            assert_eq!(vi_gamma(v), reference, "vi_gamma({v}) vs isqrt reference");
            assert_eq!(GAMMA_TABLE[usize::from(v)], vi_gamma(v), "LUT entry {v}");
        }
    }
}

#[cfg(test)]
mod pi_tests {
    use super::*;
    use rustyn64_cart::pi::{PI_CART_ADDR, PI_DRAM_ADDR, PI_STATUS, PI_WR_LEN};

    /// A `PI_WR_LEN` write must copy **cart → RDRAM**, `len + 1` bytes, and
    /// raise the PI interrupt line into the MI.
    ///
    /// This is the path n64-systemtest uses to load the rest of its own ELF, so
    /// it is the difference between the suite reporting a number and not
    /// starting at all.
    #[test]
    fn a_pi_wr_len_write_copies_cart_to_rdram_and_raises_the_interrupt() {
        let mut bus = Bus::new();
        // A cart whose ROM is a recognizable ramp.
        let mut rom = alloc::vec![0u8; 0x100];
        rom[..4].copy_from_slice(&[0x80, 0x37, 0x12, 0x40]); // .z64 magic
        for (i, b) in rom.iter_mut().enumerate().skip(0x40) {
            *b = i as u8;
        }
        bus.cart = rustyn64_cart::Cart::load(&rom).expect("loadable");

        bus.pi_write_word(PI_DRAM_ADDR, 0x1000);
        bus.pi_write_word(PI_CART_ADDR, 0x1000_0040);
        bus.pi_write_word(PI_WR_LEN, 15); // 16 bytes

        for i in 0..16u32 {
            assert_eq!(
                bus.rdram[(0x1000 + i) as usize],
                (0x40 + i) as u8,
                "byte {i} of the DMA"
            );
        }
        assert_eq!(bus.rdram[0x1000 + 16], 0, "and exactly 16, not 17");
        assert!(bus.rcp.mi_intr.pi, "completion raises the PI line");
        assert_eq!(
            bus.pi.read(PI_STATUS) & rustyn64_cart::pi::STATUS_DMA_BUSY,
            0,
            "and the DMA is no longer busy"
        );
    }

    /// `len + 1`: a length write of 0 moves **one** byte. Off by one here
    /// corrupts the last byte of every block, which presents as memory
    /// corruption rather than as a DMA bug.
    #[test]
    fn a_zero_length_write_transfers_exactly_one_byte() {
        let mut bus = Bus::new();
        let mut rom = alloc::vec![0u8; 0x80];
        rom[..4].copy_from_slice(&[0x80, 0x37, 0x12, 0x40]);
        rom[0x40] = 0xAB;
        rom[0x41] = 0xCD;
        bus.cart = rustyn64_cart::Cart::load(&rom).expect("loadable");

        bus.pi_write_word(PI_DRAM_ADDR, 0x2000);
        bus.pi_write_word(PI_CART_ADDR, 0x1000_0040);
        bus.pi_write_word(PI_WR_LEN, 0);

        assert_eq!(bus.rdram[0x2000], 0xAB, "one byte moved");
        assert_eq!(bus.rdram[0x2001], 0x00, "and only one");
    }

    /// The PI registers are reachable through the ordinary CPU bus, which is how
    /// guest code drives them.
    #[test]
    fn the_pi_registers_are_reachable_from_the_cpu_bus() {
        let mut bus = Bus::new();
        // Word-wise, as real code does. Note the value read back is rounded
        // DOWN to a doubleword -- the DRAM side ignores bits 2:0.
        bus.pi_write_word(PI_DRAM_ADDR, 0x1234);
        assert_eq!(bus.read_u32(PI_DRAM_ADDR), 0x1230, "doubleword-aligned");
        // An already-aligned value survives untouched.
        bus.pi_write_word(PI_DRAM_ADDR, 0x1238);
        assert_eq!(bus.read_u32(PI_DRAM_ADDR), 0x1238);
        // And byte reads select within the word.
        assert_eq!(bus.read_u8(PI_DRAM_ADDR + 3), 0x38);
        assert_eq!(bus.read_u8(PI_DRAM_ADDR + 2), 0x12);
    }

    /// **A guest `sw` to a length register must start exactly ONE DMA.**
    ///
    /// The default `write_u32` composes four `write_u8` calls. With PI registers
    /// handled byte-wise, a normal word store started **four** transfers, each
    /// with a partly assembled length — so every PI transfer was wrong, and the
    /// symptom was memory corruption rather than anything that looked like DMA.
    #[test]
    fn a_word_store_to_a_length_register_starts_exactly_one_dma() {
        let mut bus = Bus::new();
        let mut rom = alloc::vec![0u8; 0x200];
        rom[..4].copy_from_slice(&[0x80, 0x37, 0x12, 0x40]);
        for (i, b) in rom.iter_mut().enumerate().skip(0x40) {
            *b = i as u8;
        }
        bus.cart = rustyn64_cart::Cart::load(&rom).expect("loadable");

        bus.write_u32(PI_DRAM_ADDR, 0x1000);
        bus.write_u32(PI_CART_ADDR, 0x1000_0040);
        // The write that matters: through the ordinary CPU word path.
        bus.write_u32(PI_WR_LEN, 7); // 8 bytes

        for i in 0..8u32 {
            assert_eq!(
                bus.rdram[(0x1000 + i) as usize],
                (0x40 + i) as u8,
                "byte {i}"
            );
        }
        assert_eq!(
            bus.rdram[0x1000 + 8],
            0,
            "exactly 8 bytes -- a per-byte trigger would have run four transfers \
             with lengths 0x07000000+1, 0x00070000+1, ... and scribbled far past here"
        );
    }

    /// **Clearing the PI interrupt must lower the MI line.** Only a completion
    /// used to update it, and a `PI_STATUS` clear starts no transfer — so the
    /// line stayed asserted, `IP2` stuck high, and any interrupt-driven loader
    /// hung forever.
    #[test]
    fn clearing_the_pi_interrupt_lowers_the_mi_line() {
        let mut bus = Bus::new();
        let mut rom = alloc::vec![0u8; 0x80];
        rom[..4].copy_from_slice(&[0x80, 0x37, 0x12, 0x40]);
        bus.cart = rustyn64_cart::Cart::load(&rom).expect("loadable");

        bus.write_u32(PI_WR_LEN, 0);
        assert!(bus.rcp.mi_intr.pi, "completion raised it");

        bus.write_u32(PI_STATUS, rustyn64_cart::pi::STATUS_W_CLR_INTR);
        assert!(!bus.pi.interrupt(), "the PI cleared its own flag");
        assert!(
            !bus.rcp.mi_intr.pi,
            "and the MI line must follow -- otherwise IP2 stays high forever"
        );
    }

    /// **A direct-I/O write to the DOM2 window persists to the SRAM save and
    /// reads back.** SRAM lives on the PI bus at `0x0800_0000`; a `SW` there must
    /// store into the save backing (the read-only ROM window ignores writes).
    /// Reads during the write's busy window return the latch, so the test ticks
    /// past `PI_WRITE_CYCLES` before reading the persisted value.
    #[test]
    fn a_direct_io_write_persists_to_the_sram_save() {
        let mut bus = Bus::new();
        *bus.cart.save_device_mut() =
            rustyn64_cart::save::SaveDevice::new(rustyn64_cart::SaveType::Sram);

        CpuBus::write_u32(&mut bus, 0x0800_1000, 0xDEAD_BEEF);
        for _ in 0..Bus::PI_WRITE_CYCLES {
            bus.pi_tick();
        }
        assert_eq!(
            CpuBus::read_u32(&mut bus, 0x0800_1000),
            0xDEAD_BEEF,
            "the SRAM store must survive the direct-I/O finalization"
        );
        // And it is visible in the persistable backing (for the host save file).
        assert_eq!(
            &bus.cart.save()[0x1000..0x1004],
            &0xDEAD_BEEFu32.to_be_bytes()
        );
    }

    /// **A controller read runs end to end through the SI joybus.** The CPU
    /// stages a joybus frame in RDRAM, DMAs it to PIF RAM (`SI_PIF_AD_WR64B`),
    /// then triggers the read (`SI_PIF_AD_RD64B`) — which makes the PIF execute
    /// the handshakes and DMA the replies back. The port-0 controller word must
    /// appear at the frame's reply bytes, and the SI interrupt must fire.
    #[test]
    fn a_controller_read_runs_through_the_si_joybus() {
        const SI_DRAM_ADDR: u32 = Bus::SI_BASE;
        const SI_RD64B: u32 = Bus::SI_BASE + 0x04;
        const SI_WR64B: u32 = Bus::SI_BASE + 0x10;
        const SI_STATUS: u32 = Bus::SI_BASE + 0x18;
        const FRAME: u32 = 0x2000;

        let mut bus = Bus::new();
        bus.controllers[0] = 0x8000_1234; // A pressed, stick (0x12, 0x34)

        // Joybus frame: channel 0 = { TX=1, RX=4, cmd=0x01, 4 reply bytes };
        // the command byte (PIF RAM 0x3F) bit 0 = "run".
        bus.rdram[FRAME as usize] = 0x01; // TX len
        bus.rdram[FRAME as usize + 1] = 0x04; // RX len
        bus.rdram[FRAME as usize + 2] = 0x01; // 0x01 Controller State
        bus.rdram[FRAME as usize + 0x3F] = 0x01; // command byte: run

        CpuBus::write_u32(&mut bus, SI_DRAM_ADDR, FRAME);
        CpuBus::write_u32(&mut bus, SI_WR64B, 0); // DMA RDRAM → PIF RAM
        assert!(bus.rcp.mi_intr.si, "the WR64B DMA raises the SI interrupt");
        CpuBus::write_u32(&mut bus, SI_STATUS, 0); // ack
        CpuBus::write_u32(&mut bus, SI_RD64B, 0); // execute + DMA PIF → RDRAM

        // The reply bytes (frame offset 3..7) hold the packed port-0 word.
        assert_eq!(
            &bus.rdram[FRAME as usize + 3..FRAME as usize + 7],
            &[0x80, 0x00, 0x12, 0x34],
            "the controller state reached RDRAM through the joybus"
        );
        assert!(
            bus.rcp.mi_intr.si,
            "the RD64B execution raises the SI interrupt"
        );
    }

    /// A **byte** write to a trigger or status register is dropped, not
    /// assembled. `PI_STATUS`'s read bits (busy, interrupt) do not correspond to
    /// its write bits (reset, clear-interrupt), so reading it back to fill in
    /// the other three bytes fabricates command strobes out of status flags.
    #[test]
    fn byte_writes_to_the_trigger_and_status_registers_are_dropped() {
        let mut bus = Bus::new();
        let mut rom = alloc::vec![0u8; 0x80];
        rom[..4].copy_from_slice(&[0x80, 0x37, 0x12, 0x40]);
        bus.cart = rustyn64_cart::Cart::load(&rom).expect("loadable");

        bus.write_u8(PI_WR_LEN + 3, 0xFF);
        assert!(!bus.rcp.mi_intr.pi, "no DMA was started by a byte write");

        // Raise the interrupt, then confirm a byte write to STATUS cannot
        // fabricate a clear-interrupt strobe out of the busy/interrupt bits.
        bus.write_u32(PI_WR_LEN, 0);
        assert!(bus.rcp.mi_intr.pi);
        bus.write_u8(PI_STATUS + 3, 0x00);
        assert!(bus.rcp.mi_intr.pi, "a byte write to STATUS did nothing");

        // The address registers CAN be assembled, since they only latch.
        bus.write_u8(PI_DRAM_ADDR + 3, 0x18);
        assert_eq!(bus.read_u32(PI_DRAM_ADDR) & 0xFF, 0x18);
    }

    /// **A guest must not be able to panic the emulator.** A word write starting
    /// in the last three bytes of the `ISViewer` window is accepted by the range
    /// check but would index past the backing slice.
    ///
    /// The address and value both come from guest code, so this is reachable by
    /// any ROM, not just a malformed one.
    #[test]
    fn a_word_write_at_the_end_of_the_isviewer_window_does_not_panic() {
        let mut bus = Bus::new();
        let last = Bus::ISVIEWER_BASE + 0x20 + Bus::ISVIEWER_LEN as u32 - 1;
        for addr in [last - 3, last - 2, last - 1, last] {
            bus.write_u32(addr, 0xDEAD_BEEF);
            let _ = bus.read_u32(addr);
        }
        // ...and reads just past the window are 0 rather than a panic.
        assert_eq!(bus.read_u8(last), 0xEF, "the aligned tail write landed");
    }

    /// The `ISViewer` window must round-trip a written word, because that is
    /// exactly the probe n64-systemtest uses to decide whether the channel
    /// exists — `isviewer::detect()` writes `0x12345678` and reads it back. If
    /// it fails, the suite falls back to a framebuffer console we cannot read.
    #[test]
    fn the_isviewer_window_round_trips_the_detection_magic() {
        let mut bus = Bus::new();
        bus.write_u32(Bus::ISVIEWER_BUF, 0x1234_5678);
        assert_eq!(
            bus.read_u32(Bus::ISVIEWER_BUF),
            0x1234_5678,
            "detect() must succeed or the suite picks the framebuffer instead"
        );
    }

    /// Text is captured on the **length** write, not on the buffer writes, so a
    /// whole line is published at once. Capturing per buffer write would
    /// interleave partial lines and make the output unreadable.
    #[test]
    fn text_is_captured_on_the_length_write_not_the_buffer_writes() {
        let mut bus = Bus::new();
        // "OK!\n" packed big-endian, as `isviewer::pack` does.
        bus.write_u32(Bus::ISVIEWER_BUF, u32::from_be_bytes(*b"OK!\n"));
        assert!(
            bus.isviewer_output().is_empty(),
            "nothing published until the length write"
        );
        bus.write_u32(Bus::ISVIEWER_WRITE_LEN, 4);
        assert_eq!(bus.isviewer_output(), b"OK!\n");

        // And a second line appends rather than replacing.
        bus.write_u32(Bus::ISVIEWER_BUF, u32::from_be_bytes(*b"two\n"));
        bus.write_u32(Bus::ISVIEWER_WRITE_LEN, 4);
        assert_eq!(bus.isviewer_output(), b"OK!\ntwo\n");
    }

    /// A length longer than the buffer is clamped rather than panicking — the
    /// value comes from guest code and must not be trusted.
    #[test]
    fn an_oversized_length_write_is_clamped() {
        let mut bus = Bus::new();
        bus.write_u32(Bus::ISVIEWER_WRITE_LEN, 0xFFFF_FFFF);
        assert_eq!(bus.isviewer_output().len(), Bus::ISVIEWER_LEN);
    }

    /// **The RI register block round-trips.** Eight registers at
    /// `0x0470_0000..0x0470_0020` (N64brew *RDRAM Interface* §Registers). Before
    /// this block was decoded every RI address read back `0`, which is not inert:
    /// the cartridge's IPL3 opens by reading `RI_SELECT` (`0x0470_000C`) to decide
    /// whether RDRAM has already been brought up, so an undecoded block silently
    /// forced the cold-init path on every boot (ledger R-18).
    ///
    /// Each register is given a *distinct* value so a decode that collapses them
    /// onto one another — or drops the low address bits — fails rather than
    /// passing on a shared zero.
    #[test]
    fn the_ri_register_block_round_trips() {
        let mut bus = Bus::new();
        for i in 0..8u32 {
            CpuBus::write_u32(&mut bus, Bus::RI_BASE + i * 4, 0x1234_0000 + i);
        }
        for i in 0..8u32 {
            assert_eq!(
                CpuBus::read_u32(&mut bus, Bus::RI_BASE + i * 4),
                0x1234_0000 + i,
                "RI register {i} must read back what was written"
            );
        }
    }

    /// **A narrow store to an RI register takes the size-blind RCP path**, like
    /// every other RCP block — it is NOT dropped, and it does not need a
    /// per-block arm in [`Bus::write_u8`].
    ///
    /// Pinned because "sub-word writes to RI fall through or are silently
    /// dropped" is a reasonable-sounding worry that is wrong here, and only a test
    /// settles it. Narrow CPU stores reach the bus through `write_sized`, which
    /// funnels them to `write_u32(addr & !3, word)` after shifting the register
    /// into its byte lane — the RCP latches the whole word and ignores the access
    /// size (N64brew *Memory map* §Physical Memory Map accesses). So a byte store
    /// of `0x12` at `RI_SELECT + 3` must leave `0x0000_0012`, not `0x12` merged
    /// into a previous value and not nothing at all.
    #[test]
    fn a_narrow_store_to_ri_latches_the_whole_word() {
        let mut bus = Bus::new();
        CpuBus::write_u32(&mut bus, 0x0470_000C, 0xFFFF_FFFF);
        // Byte lane 3 (the low byte of the word).
        bus.write_sized(0x0470_000F, 1, 0x12);
        assert_eq!(
            CpuBus::read_u32(&mut bus, 0x0470_000C),
            0x0000_0012,
            "the RCP latches the whole shifted word, zeroing the untouched bytes"
        );
        // ... and a halfword store into the upper lane behaves the same way.
        CpuBus::write_u32(&mut bus, 0x0470_000C, 0xFFFF_FFFF);
        bus.write_sized(0x0470_000C, 2, 0xABCD);
        assert_eq!(
            CpuBus::read_u32(&mut bus, 0x0470_000C),
            0xABCD_0000,
            "a halfword store shifts into its lane and zero-fills the rest"
        );
    }

    /// **`RI_SELECT` specifically reads back**, since it is the one RI register a
    /// real boot depends on: IPL3 branches on it. Asserted separately from the
    /// round-trip above so the intent survives if that test is ever narrowed.
    #[test]
    fn ri_select_reads_back_what_ipl3_writes() {
        let mut bus = Bus::new();
        // The value IPL3 configures: TSEL = 0b0001, RSEL = 0b0100 (N64brew
        // *RDRAM Interface* §RI_SELECT, "Extra Details").
        CpuBus::write_u32(&mut bus, 0x0470_000C, 0x14);
        assert_eq!(CpuBus::read_u32(&mut bus, 0x0470_000C), 0x14);
    }

    /// **The RSP powers up halted.** Reading `SP_STATUS` as zero claims a
    /// running RSP, which is false; n64-systemtest's `StartupTest` reads `0x1`.
    #[test]
    fn sp_status_reports_the_rsp_halted_at_power_on() {
        let mut bus = Bus::new();
        assert_eq!(
            bus.read_u32(Bus::SP_STATUS) & rustyn64_rsp::sp::STATUS_HALTED,
            rustyn64_rsp::sp::STATUS_HALTED,
            "the RSP idles halted until the CPU clears it"
        );
    }

    /// **`SP_RD_LEN` moves RDRAM into SPMEM**, and the length word is not a
    /// plain byte count: bits 11:0 are bytes-per-row minus one.
    #[test]
    fn an_sp_dma_moves_rdram_into_spmem() {
        let mut bus = Bus::new();
        for (i, b) in bus.rdram[0x100..0x108].iter_mut().enumerate() {
            *b = 0xA0 + i as u8;
        }
        bus.write_u32(Bus::SP_REGS_BASE + 4, 0x100);
        bus.write_u32(Bus::SP_REGS_BASE, 0);
        bus.write_u32(Bus::SP_REGS_BASE + 8, 7); // 8 bytes, one row
        assert_eq!(
            core::array::from_fn::<u8, 8, _>(|i| bus.rsp.mem_read(i as u32)),
            [0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7]
        );
    }

    /// `SP_WR_LEN` is the other direction — SPMEM into RDRAM.
    #[test]
    fn an_sp_dma_moves_spmem_into_rdram() {
        let mut bus = Bus::new();
        for i in 0..8u32 {
            bus.rsp.mem_write(i, 0x50 + i as u8);
        }
        bus.write_u32(Bus::SP_REGS_BASE + 4, 0x200);
        bus.write_u32(Bus::SP_REGS_BASE, 0);
        bus.write_u32(Bus::SP_REGS_BASE + 12, 7);
        assert_eq!(
            &bus.rdram[0x200..0x208],
            &[0x50, 0x51, 0x52, 0x53, 0x54, 0x55, 0x56, 0x57]
        );
    }

    /// **`count` and `skip` are real fields, not padding.** A 2D block copy
    /// moves `count + 1` rows and steps the RDRAM pointer by `skip` between
    /// them, while SPMEM stays contiguous. Reading only bits 11:0 silently
    /// drops every row after the first.
    #[test]
    fn an_sp_dma_honors_the_count_and_skip_fields() {
        let mut bus = Bus::new();
        // Two rows of 8, separated by an 8-byte gap in RDRAM.
        for i in 0..8 {
            bus.rdram[0x300 + i] = 0x10 + i as u8;
            bus.rdram[0x310 + i] = 0x20 + i as u8;
        }
        bus.write_u32(Bus::SP_REGS_BASE + 4, 0x300);
        bus.write_u32(Bus::SP_REGS_BASE, 0);
        // length = 7 (8 bytes), count = 1 (two rows), skip = 8.
        bus.write_u32(Bus::SP_REGS_BASE + 8, 7 | (1 << 12) | (8 << 20));
        assert_eq!(
            core::array::from_fn::<u8, 8, _>(|i| bus.rsp.mem_read(i as u32)),
            [0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17]
        );
        assert_eq!(
            core::array::from_fn::<u8, 8, _>(|i| bus.rsp.mem_read(8 + i as u32)),
            [0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27],
            "the second row must land contiguously in SPMEM"
        );
    }

    /// `SP_MEM_ADDR` bit 12 selects **IMEM**, and the 12-bit offset wraps within
    /// whichever half was chosen rather than spilling across into the other.
    #[test]
    fn sp_mem_addr_bit_12_selects_imem() {
        let mut bus = Bus::new();
        bus.rdram[0x400] = 0x99;
        bus.write_u32(Bus::SP_REGS_BASE + 4, 0x400);
        bus.write_u32(Bus::SP_REGS_BASE, 0x1000); // IMEM
        bus.write_u32(Bus::SP_REGS_BASE + 8, 7);
        assert_eq!(bus.rsp.mem_read(0x1000), 0x99, "landed in IMEM");
        assert_eq!(bus.rsp.mem_read(0), 0, "and NOT in DMEM");
    }

    /// Read a word out of SPMEM the way the CPU does, for the tests below.
    fn spmem_word(bus: &mut Bus, off: u32) -> u32 {
        bus.read_u32(Bus::SPMEM_BASE + off)
    }

    /// **A byte store to the RCP's internal bus writes 32 bits.**
    ///
    /// The values are n64-systemtest's, not ours (`sp_memory::SB`): with
    /// `$3 = 0x1234_5678`, storing a byte at offsets 0, 5, 10 and 15 leaves the
    /// register *shifted into the addressed lane* in each of the four words,
    /// wiping the rest. Byte-exact semantics would leave `0x7800_0000`,
    /// `0x0078_0000`, `0x0000_7800`, `0x0000_0078` instead -- so this test fails
    /// in all four words if the size-blind path is lost.
    #[test]
    fn a_byte_store_to_spmem_writes_the_whole_shifted_word() {
        let mut bus = Bus::new();
        for (i, off) in [0u32, 5, 10, 15].iter().enumerate() {
            bus.write_sized(Bus::SPMEM_BASE + off, 1, 0x1234_5678);
            let _ = i;
        }
        assert_eq!(spmem_word(&mut bus, 0), 0x7800_0000);
        assert_eq!(spmem_word(&mut bus, 4), 0x5678_0000);
        assert_eq!(spmem_word(&mut bus, 8), 0x3456_7800);
        assert_eq!(spmem_word(&mut bus, 12), 0x1234_5678);
    }

    /// The same rule for halfwords, and it **destroys the untouched half** --
    /// `sp_memory::SH` presets `0xDEAD_BEEF`/`0xBADD_ECAF` and expects both gone.
    #[test]
    fn a_halfword_store_to_spmem_writes_the_whole_shifted_word() {
        let mut bus = Bus::new();
        bus.write_u32(Bus::SPMEM_BASE, 0xDEAD_BEEF);
        bus.write_u32(Bus::SPMEM_BASE + 4, 0xBADD_ECAF);

        bus.write_sized(Bus::SPMEM_BASE, 2, 0x1234_5678);
        bus.write_sized(Bus::SPMEM_BASE + 6, 2, 0x1234_5678);

        assert_eq!(spmem_word(&mut bus, 0), 0x5678_0000);
        assert_eq!(spmem_word(&mut bus, 4), 0x1234_5678);
    }

    /// **A 64-bit store touches four bytes, not eight.** The RCP takes the first
    /// word off the bus and drops the second (`sp_memory::SD`), so the preset
    /// second word must survive intact -- which is what distinguishes this from
    /// a plain 64-bit write.
    #[test]
    fn a_doubleword_store_to_spmem_writes_only_the_upper_word() {
        let mut bus = Bus::new();
        bus.write_u32(Bus::SPMEM_BASE, 0xDEAD_BEEF);
        bus.write_u32(Bus::SPMEM_BASE + 4, 0xBADD_ECAF);

        bus.write_sized(Bus::SPMEM_BASE, 8, 0xABCD_EF98_7654_3210);

        assert_eq!(spmem_word(&mut bus, 0), 0xABCD_EF98);
        assert_eq!(
            spmem_word(&mut bus, 4),
            0xBADD_ECAF,
            "the low word is dropped on the floor, not stored"
        );
    }

    /// **RDRAM is not size-blind.** The RI passes the low address bits and the
    /// access size to the RDRAM devices, which build a real byte mask. Without
    /// this the size-blind rule would corrupt every ordinary narrow store, so
    /// the exclusion is load-bearing rather than an optimization.
    #[test]
    fn a_byte_store_to_rdram_writes_one_byte() {
        let mut bus = Bus::new();
        bus.write_u32(0x100, 0xDEAD_BEEF);
        bus.write_sized(0x101, 1, 0x1234_5678);
        assert_eq!(bus.read_u32(0x100), 0xDE78_BEEF);
    }

    /// The 8 KiB of DMEM+IMEM **repeats** up to `0x0404_0000`, where the SP
    /// registers begin. n64-systemtest writes at `0x3E000` and reads the result
    /// back at offset 0 (`sp_memory::SW (out of bounds)`).
    #[test]
    fn the_spmem_window_repeats_every_8_kib() {
        let mut bus = Bus::new();
        bus.write_u32(Bus::SPMEM_BASE, 0x0123_4567);
        bus.write_u32(Bus::SPMEM_BASE + 0x1000, 0x89AB_CDEF);
        bus.write_u32(Bus::SPMEM_BASE + 0x3E000, 0x7654_3210);

        assert_eq!(
            spmem_word(&mut bus, 0),
            0x7654_3210,
            "0x3E000 is offset 0 seen for the 31st time"
        );
        assert_eq!(spmem_word(&mut bus, 0x1000), 0x89AB_CDEF, "IMEM untouched");
        assert_eq!(spmem_word(&mut bus, 0x3E000), 0x7654_3210);
    }

    /// **`SP_SEMAPHORE` is taken once per access, not once per byte.**
    ///
    /// The register has a side effect on read, so composing a word from four
    /// byte reads took the mutex four times and returned 1 where hardware
    /// returns 0 — n64-systemtest's `SP Semaphore Register (CPU only)` fails on
    /// exactly that. The suite also checks that the value written is irrelevant.
    #[test]
    fn reading_the_semaphore_as_a_word_takes_it_exactly_once() {
        const SEMAPHORE: u32 = Bus::SP_REGS_BASE + 0x1C;
        for written in [0u32, 1, 0xFFFF_FFFF] {
            let mut bus = Bus::new();
            bus.write_u32(SEMAPHORE, written);
            assert_eq!(bus.read_u32(SEMAPHORE), 0, "the first word read acquires");
            assert_eq!(bus.read_u32(SEMAPHORE), 1, "and it stays taken");
        }
    }

    /// A `SP_STATUS` write raises and acknowledges the **MI's SP line**, and the
    /// guest can see it in `MI_INTERRUPT`.
    #[test]
    fn sp_status_drives_the_mi_interrupt_line() {
        const SET_INTR: u32 = 1 << 4;
        const CLR_INTR: u32 = 1 << 3;
        const MI_INTERRUPT: u32 = Bus::MI_BASE + 0x08;
        let mut bus = Bus::new();

        bus.write_u32(Bus::SP_STATUS, SET_INTR);
        assert_eq!(bus.read_u32(MI_INTERRUPT) & 1, 1, "SP line raised");
        bus.write_u32(Bus::SP_STATUS, CLR_INTR);
        assert_eq!(bus.read_u32(MI_INTERRUPT) & 1, 0, "and acknowledged");

        // Set and clear together leaves it alone, as for every other flag.
        bus.write_u32(Bus::SP_STATUS, SET_INTR);
        bus.write_u32(Bus::SP_STATUS, SET_INTR | CLR_INTR);
        assert_eq!(bus.read_u32(MI_INTERRUPT) & 1, 1, "unchanged");
    }

    /// `MI_MASK` writes as clear/set pairs and reads back as a flag word, and
    /// only a masked-in line reaches `IP2`.
    #[test]
    fn the_mi_mask_gates_the_interrupt_line() {
        const MI_MASK: u32 = Bus::MI_BASE + 0x0C;
        const SET_SP: u32 = 1 << 1;
        const CLR_SP: u32 = 1 << 0;
        let mut bus = Bus::new();

        bus.write_u32(Bus::SP_STATUS, 1 << 4); // raise SP
        assert!(!bus.poll_irq(), "an unmasked line must not reach IP2");

        bus.write_u32(MI_MASK, SET_SP);
        assert_eq!(bus.read_u32(MI_MASK) & 1, 1, "the mask reads back");
        assert!(bus.poll_irq(), "masked in, so IP2 asserts");

        bus.write_u32(MI_MASK, CLR_SP);
        assert!(!bus.poll_irq(), "masked out again");
    }

    /// The MI block is four registers **mirrored** on the low four address bits,
    /// so `MI_VERSION` is readable at every `+0x10` step.
    #[test]
    fn the_mi_registers_mirror_every_sixteen_bytes() {
        let mut bus = Bus::new();
        assert_eq!(bus.read_u32(Bus::MI_BASE + 0x04), Bus::MI_VERSION_VALUE);
        assert_eq!(
            bus.read_u32(Bus::MI_BASE + 0x14),
            Bus::MI_VERSION_VALUE,
            "mirrored one block up"
        );
        assert_eq!(bus.read_u32(Bus::MI_BASE + 0x1004), Bus::MI_VERSION_VALUE);
    }

    /// **The PI external bus is 16 bits wide and the RCP ignores access size**,
    /// so a byte or halfword read returns data two bytes further on than the
    /// address asked for, while a word read does not. This is a hardware bug we
    /// must reproduce, not an approximation.
    ///
    /// n64-systemtest pins all three against a ROM beginning
    /// `01 23 45 67 89 AB CD EF`.
    #[test]
    fn a_pi_bus_sub_word_read_lands_two_bytes_late() {
        let mut bus = Bus::new();
        // A `.z64` header, then a byte-index pattern from 0x40 on.
        let mut rom = alloc::vec![0u8; 0x1000];
        rom[0..4].copy_from_slice(&[0x80, 0x37, 0x12, 0x40]);
        for (i, b) in rom.iter_mut().enumerate().skip(0x40) {
            *b = (i & 0xFF) as u8;
        }
        bus.cart = rustyn64_cart::Cart::load(&rom).expect("valid z64");
        let base = 0x1000_0040u32;

        // A WORD read is unaffected: the access puts its own address on the bus.
        assert_eq!(
            bus.read_u32(base),
            0x4041_4243,
            "a word read is the four bytes at its own address"
        );

        // A BYTE read at offset 2 returns the byte at offset 4.
        assert_eq!(bus.read_u8(base + 2), 0x44, "offset 2 reads byte 4");
        assert_eq!(bus.read_u8(base + 3), 0x45, "offset 3 reads byte 5");
        // ...but offsets 0 and 1 are unaffected: bit 1 is clear.
        assert_eq!(bus.read_u8(base), 0x40);
        assert_eq!(bus.read_u8(base + 1), 0x41);

        // A HALFWORD needs no special case -- it is two byte reads, and both
        // land correctly by the same rule.
        let hi = (u16::from(bus.read_u8(base + 2)) << 8) | u16::from(bus.read_u8(base + 3));
        assert_eq!(hi, 0x4445, "halfword at offset 2 reads offset 4");
    }

    /// The quirk is confined to the PI window. RDRAM must be untouched, or every
    /// ordinary load in the machine shifts by two bytes.
    #[test]
    fn the_pi_off_by_two_does_not_leak_into_rdram() {
        let mut bus = Bus::new();
        for (i, b) in bus.rdram[0..8].iter_mut().enumerate() {
            *b = i as u8;
        }
        assert_eq!(bus.read_u8(0x0000_0002), 2, "RDRAM is NOT shifted");
        assert_eq!(bus.read_u32(0x0000_0000), 0x0001_0203);
    }

    /// **A PI direct-I/O write latches and shadows the whole bus.** While it is
    /// in flight, reads from *any* PI address return the value being written --
    /// including from ROM, which the PI has no way of knowing is read-only.
    #[test]
    fn a_pi_write_is_latched_and_shadows_reads_until_it_finalizes() {
        let mut bus = Bus::new();
        let mut rom = alloc::vec![0u8; 0x1000];
        rom[0..4].copy_from_slice(&[0x80, 0x37, 0x12, 0x40]);
        for (i, b) in rom.iter_mut().enumerate().skip(0x40) {
            *b = (i & 0xFF) as u8;
        }
        bus.cart = rustyn64_cart::Cart::load(&rom).expect("valid z64");
        let base = 0x1000_0040u32;
        assert_eq!(bus.read_u32(base), 0x4041_4243, "ROM before the write");

        bus.write_u32(base, 0xBADC_0FFE);
        assert_eq!(
            bus.read_u32(base),
            0xBADC_0FFE,
            "the latched value is read back"
        );
        assert_eq!(
            bus.read_u32(base + 0x100),
            0xBADC_0FFE,
            "and shadows a DIFFERENT address too -- it is the bus, not the cell"
        );

        // ...and it decays: the ROM value returns once the write finalizes.
        for _ in 0..Bus::PI_WRITE_CYCLES {
            bus.pi_tick();
        }
        assert_eq!(
            bus.read_u32(base),
            0x4041_4243,
            "ROM is back; ROM ignored the write"
        );
    }

    /// `PI_STATUS.IOBUSY` reports the asynchronous write, which is how software
    /// knows when a cart write has landed.
    #[test]
    fn a_pi_write_sets_io_busy_until_it_finalizes() {
        let mut bus = Bus::new();
        let st = rustyn64_cart::pi::PI_STATUS;
        assert_eq!(bus.read_u32(st) & rustyn64_cart::pi::STATUS_IO_BUSY, 0);
        bus.write_u32(0x1000_0000, 0xDEAD_BEEF);
        assert_ne!(
            bus.read_u32(st) & rustyn64_cart::pi::STATUS_IO_BUSY,
            0,
            "IOBUSY is set while the write is in flight"
        );
        for _ in 0..Bus::PI_WRITE_CYCLES {
            bus.pi_tick();
        }
        assert_eq!(bus.read_u32(st) & rustyn64_cart::pi::STATUS_IO_BUSY, 0);
    }

    /// A second write while one is in flight is **ignored**, not queued.
    #[test]
    fn a_pi_write_during_another_is_ignored() {
        let mut bus = Bus::new();
        bus.write_u32(0x1000_0000, 0xAAAA_AAAA);
        bus.write_u32(0x1000_0000, 0xBBBB_BBBB);
        assert_eq!(
            bus.read_u32(0x1000_0000),
            0xAAAA_AAAA,
            "the FIRST write still owns the bus"
        );
    }
}

#[cfg(all(test, feature = "rdp-tap"))]
mod rdp_tap_tests {
    use super::Bus;

    /// Load a command list at `addr` and drain the DP FIFO over it.
    fn run(words: &[u32]) -> Bus {
        let mut bus = Bus::new();
        let addr = 0x0010_0000u32;
        for (i, w) in words.iter().enumerate() {
            let a = addr as usize + i * 4;
            bus.rdram[a..a + 4].copy_from_slice(&w.to_be_bytes());
        }
        let end = addr + (words.len() * 4) as u32;
        bus.rdp.dpc_write(0, addr);
        bus.rdp.dpc_write(1, end);
        for _ in 0..(words.len() * 8 + 64) {
            bus.rdp_tick();
        }
        bus
    }

    /// A `Sync Full` (2 words) and a `Set Fill Color` (2 words).
    const TWO_COMMANDS: [u32; 4] = [0x2900_0000, 0, 0x3700_0000, 0xFF00_00FF];

    /// The tap must reproduce the consumed stream **exactly**, in order.
    ///
    /// Mutation-checked: dropping the `while addr < after` capture entirely
    /// leaves this empty, and advancing `addr` by 8 instead of 4 drops every
    /// second word.
    #[test]
    fn the_tap_reproduces_the_consumed_stream() {
        let mut bus = run(&TWO_COMMANDS);
        assert_eq!(bus.take_rdp_commands(), TWO_COMMANDS.to_vec());
    }

    /// Draining must actually empty it, or a per-frame consumer replays every
    /// earlier frame's commands on top of its own.
    #[test]
    fn draining_empties_the_tap() {
        let mut bus = run(&TWO_COMMANDS);
        assert!(!bus.take_rdp_commands().is_empty(), "nothing was captured");
        assert!(
            bus.take_rdp_commands().is_empty(),
            "a second drain returned commands, so the first did not consume them"
        );
    }

    /// A command the FIFO never consumed must not be captured.
    ///
    /// The tap diffs the FIFO pointer, so it inherits `tick_with_bus`'s refusal
    /// to consume a partly-written command rather than restating it. This is the
    /// test that the inheritance is real and not merely intended.
    ///
    /// **A Fill Triangle, not a 2-word command, and that is the whole point.**
    /// The first version of this test used a 2-word command with `DPC_END` set
    /// one word in — but `DPC_ADDR_MASK` is `0x00FF_FFF8`, so `addr + 4` masks
    /// back down to `addr`, leaving `cmd_end == cmd_current`. The FIFO was
    /// *empty*, `tick_without_bus` early-returned, and the test passed without
    /// the partial-command path ever running. It survived a mutation that
    /// captured unconditionally. A triangle is 4 u64 words (32 bytes), so
    /// `addr + 8` is both 8-byte aligned and genuinely short.
    #[test]
    fn a_partial_command_is_not_captured() {
        let mut bus = Bus::new();
        let addr = 0x0010_0000u32;
        // Fill Triangle (0x08): 32 bytes.
        bus.rdram[addr as usize..addr as usize + 4].copy_from_slice(&0x0800_0000u32.to_be_bytes());
        bus.rdp.dpc_write(0, addr);
        bus.rdp.dpc_write(1, addr + 8);
        assert_eq!(
            bus.rdp.dpc_read(1),
            addr + 8,
            "DPC_END was masked away; the FIFO would be empty rather than partial"
        );
        for _ in 0..64 {
            bus.rdp_tick();
        }
        assert_eq!(
            bus.rdp.commands_processed, 0,
            "the RDP consumed the partial command, so this tests nothing about the tap"
        );
        assert!(
            bus.take_rdp_commands().is_empty(),
            "the tap captured a command the RDP never consumed"
        );
    }

    /// The tap must not perturb what the RDP does.
    ///
    /// The whole feature is supposed to be observation only, and "it does not
    /// change behavior" is a claim like any other. `commands_processed` is
    /// retained state the RDP increments itself, so it witnesses the work rather
    /// than being derived from the clock.
    #[test]
    fn the_tap_does_not_change_what_the_rdp_consumes() {
        let bus = run(&TWO_COMMANDS);
        assert_eq!(
            bus.rdp.commands_processed, 2,
            "the RDP consumed a different number of commands with the tap compiled in"
        );
    }
}

#[cfg(all(test, feature = "rdp-tap"))]
mod rdram_dirty_tests {
    use super::{Bus, RDRAM_PAGE, RdramBus};
    use crate::cpu::Bus as CpuBus;

    /// KSEG0 address of the first byte of RDRAM page `p`.
    const fn page_addr(p: usize) -> u32 {
        0x8000_0000 + (p * RDRAM_PAGE) as u32
    }

    fn dirty(bus: &Bus) -> alloc::vec::Vec<usize> {
        bus.rdram_dirty_pages()
            .iter()
            .enumerate()
            .filter(|(_, d)| **d)
            .map(|(i, _)| i)
            .collect()
    }

    /// A fresh Bus starts **all** dirty.
    ///
    /// The consumer has never seen this RDRAM, so the first upload must be
    /// complete. Starting clean would hand the GPU an empty framebuffer and
    /// whatever the allocator left behind — and it would look right, because the
    /// second frame would repair it.
    #[test]
    fn a_fresh_bus_starts_entirely_dirty() {
        let bus = Bus::new();
        assert_eq!(
            dirty(&bus).len(),
            bus.rdram_dirty_pages().len(),
            "a page started clean, so the first upload would be incomplete"
        );
    }

    /// A byte write marks its page and **only** its page.
    #[test]
    fn a_write_marks_exactly_one_page() {
        let mut bus = Bus::new();
        bus.clear_rdram_dirty();
        CpuBus::write_u8(&mut bus, page_addr(3) + 17, 0xAB);
        assert_eq!(dirty(&bus), alloc::vec![3]);
    }

    /// The `u32` fast path writes four bytes and can straddle a page boundary.
    ///
    /// This is the case a point-mark would get wrong, and it is not hypothetical:
    /// the assertion that was supposed to guard the write sites caught it,
    /// because `write_u32` is `self.rdram[off..=off + 3]` rather than a single
    /// indexed store.
    #[test]
    fn a_straddling_word_write_marks_both_pages() {
        let mut bus = Bus::new();
        bus.clear_rdram_dirty();
        // Two bytes in page 5, two in page 6.
        CpuBus::write_u32(&mut bus, page_addr(6) - 2, 0xDEAD_BEEF);
        assert_eq!(dirty(&bus), alloc::vec![5, 6]);
    }

    /// Clearing actually clears, or the consumer re-sends everything forever and
    /// the whole feature is a no-op with extra steps.
    #[test]
    fn clearing_empties_the_map() {
        let mut bus = Bus::new();
        CpuBus::write_u8(&mut bus, page_addr(1), 1);
        bus.clear_rdram_dirty();
        assert!(dirty(&bus).is_empty());
        // And a later write still marks — clearing must not disable tracking.
        CpuBus::write_u8(&mut bus, page_addr(9), 1);
        assert_eq!(dirty(&bus), alloc::vec![9]);
    }

    /// **Every** CPU store width marks, including the ones with no dedicated
    /// `Bus` method.
    ///
    /// A reviewer asked whether `SH`/`SD`/`SWL`/`SDR` bypass the map, since only
    /// `write_u8` and `write_u32` carry marks. They do not: `write_sized`
    /// decomposes every non-RCP-internal width into those two, and the unaligned
    /// family reaches the bus through `Pipeline::write_width` at width 4 or 8, so
    /// it decomposes the same way. Nothing *enforces* that delegation, though —
    /// a future width-specific override would silently bypass the marks and
    /// produce a stale frame with every test still green. Hence this test rather
    /// than a reply saying the code is fine today.
    #[test]
    fn every_store_width_marks() {
        // Width, page, and the byte the store must leave at the page's base so
        // the marking assertion cannot pass on a store that did nothing.
        // Big-endian: the byte at the base is the register's *most* significant
        // byte of that width.
        for (width, page, value, expect) in [
            (1u64, 10usize, 0x00_00_00_00_00_00_00_A1u64, 0xA1u8),
            (2, 11, 0x0000_0000_0000_A2B2, 0xA2),
            (4, 12, 0x0000_0000_A3B3_C3D3, 0xA3),
            (8, 13, 0xA4B4_C4D4_E4F4_0414, 0xA4),
        ] {
            let mut bus = Bus::new();
            bus.clear_rdram_dirty();
            CpuBus::write_sized(&mut bus, page_addr(page), width, value);
            assert_eq!(
                bus.rdram[page * RDRAM_PAGE],
                expect,
                "a width-{width} store did not land, so its marking assertion is vacuous"
            );
            assert_eq!(
                dirty(&bus),
                alloc::vec![page],
                "a width-{width} store left its page clean"
            );
        }
    }

    /// `RdramBus::rdram_write` marks — the RDP/RSP/AI DMA paths use it.
    ///
    /// One of three routes the first version of this module left unguarded: the
    /// mutation check only removed the SP DMA mark, so deleting this one, SI's or
    /// PI's would have gone unnoticed. Caught in review of #245.
    #[test]
    fn the_shared_rdram_bus_write_marks() {
        let mut bus = Bus::new();
        bus.clear_rdram_dirty();
        RdramBus::rdram_write(&mut bus, page_addr(7) & 0x00FF_FFFF, 0x5A);
        assert_eq!(dirty(&bus), alloc::vec![7]);
        assert_eq!(
            bus.rdram[7 * RDRAM_PAGE],
            0x5A,
            "the write did not land, so the marking assertion is vacuous"
        );
    }

    /// PI DMA (cart to RDRAM) marks.
    #[test]
    fn pi_dma_marks_the_pages_it_writes() {
        use rustyn64_cart::pi::{PI_CART_ADDR, PI_DRAM_ADDR, PI_WR_LEN};
        let mut bus = Bus::new();
        // A cart whose first bytes are recognizable. `reattach_rom` is the
        // supported way to give a `Cart` an image without parsing a header.
        let mut rom = alloc::vec![0u8; 0x1000];
        rom[0] = 0xC5;
        bus.cart.reattach_rom(rom);
        bus.clear_rdram_dirty();

        bus.pi_write_word(PI_DRAM_ADDR, page_addr(8) & 0x00FF_FFFF);
        bus.pi_write_word(PI_CART_ADDR, 0x1000_0000);
        // WR_LEN loads INTO RDRAM; the value is length-1.
        bus.pi_write_word(PI_WR_LEN, 7);

        assert_eq!(
            bus.rdram[8 * RDRAM_PAGE],
            0xC5,
            "the PI transfer did not land, so the marking assertion is vacuous"
        );
        assert!(
            dirty(&bus).contains(&8),
            "a PI DMA to page 8 left it clean; dirty = {:?}",
            dirty(&bus)
        );
    }

    /// SI DMA (PIF RAM to RDRAM) marks.
    #[test]
    fn si_dma_marks_the_pages_it_writes() {
        let mut bus = Bus::new();
        bus.clear_rdram_dirty();
        // SI_DRAM_ADDR, then the RD64B write that runs the joybus frame and DMAs
        // PIF RAM into RDRAM.
        //
        // PHYSICAL addresses: the Bus matches MMIO on raw physical ranges
        // (`is_si_register` is `addr >= 0x0480_0000`), so a KSEG1 address never
        // matches and the write silently lands nowhere. That is what the first
        // version of this test did, and it read as "the mark is missing".
        CpuBus::write_u32(&mut bus, Bus::SI_BASE, page_addr(9) & 0x00FF_FFFF);
        CpuBus::write_u32(&mut bus, Bus::SI_BASE + 0x04, 0);
        assert!(
            dirty(&bus).contains(&9),
            "an SI DMA to page 9 left it clean; dirty = {:?}",
            dirty(&bus)
        );
    }

    /// DMA writes mark too.
    ///
    /// The CPU store path is the obvious one; a DMA that did not mark would
    /// leave the consumer with stale textures, which is the failure that
    /// presents as a rendering bug rather than as an error. Driven through
    /// `Bus::sp_dma` directly rather than through the SP registers, because what
    /// is under test is the marking in the transfer loop, not the register
    /// plumbing that reaches it.
    #[test]
    fn sp_dma_marks_the_pages_it_writes() {
        let mut bus = Bus::new();
        bus.rsp.mem_write(0, 0x5A);
        bus.clear_rdram_dirty();
        bus.sp_dma(rustyn64_rsp::sp::Dma {
            sp_addr: 0,
            ram_addr: page_addr(4) & 0x00FF_FFFF,
            row_len: 8,
            rows: 1,
            skip: 0,
            to_dram: true,
        });
        assert_eq!(
            dirty(&bus),
            alloc::vec![4],
            "an SP DMA to page 4 did not mark it"
        );
        assert_eq!(
            bus.rdram[4 * RDRAM_PAGE],
            0x5A,
            "the DMA did not actually transfer, so the marking assertion is vacuous"
        );
    }
}

#[cfg(all(test, feature = "rdp-tap"))]
mod rdram_dirty_savestate_tests {
    use super::{Bus, RDRAM_PAGE, RDRAM_SIZE};
    use crate::cpu::Bus as CpuBus;

    /// A Bus restored from a save-state must still be able to take a store.
    ///
    /// **This is a crash, not a cosmetic gap.** `#[serde(skip)]` fills the field
    /// from `Default`, and `Box<[bool]>::default()` is **empty** — so the first
    /// RDRAM store after a load indexes page `off >> 12` into a zero-length slice
    /// and panics. `rdp_tap` gets away with the same attribute only because
    /// `Vec::default()` is empty *and pushable*; an indexed map is not.
    ///
    /// Caught in review of #245, not by any gate here, because nothing had
    /// round-tripped a Bus with the feature on.
    #[test]
    fn a_deserialized_bus_can_still_take_a_store() {
        let mut bus = Bus::new();
        CpuBus::write_u8(&mut bus, 0x8000_1000, 0x11);
        let bytes = bincode::serialize(&bus).expect("serialize");
        let mut restored: Bus = bincode::deserialize(&bytes).expect("deserialize");

        assert_eq!(
            restored.rdram_dirty_pages().len(),
            bus.rdram_dirty_pages().len(),
            "the dirty map came back a different size, so the next store is a panic"
        );
        // The store itself is the assertion: without a `serde(default)` this
        // panics with an out-of-bounds index.
        CpuBus::write_u8(&mut restored, 0x8000_2000, 0x22);
        assert!(
            restored.rdram_dirty_pages()[2],
            "the restored Bus did not track the store"
        );
    }

    /// A restored Bus starts **entirely** dirty.
    ///
    /// Its consumer is a GPU backend that has never seen this machine's RDRAM —
    /// loading a save-state is exactly the moment every page is new. Coming back
    /// clean would present the previous state's framebuffer until something
    /// happened to overwrite it.
    #[test]
    fn a_deserialized_bus_starts_entirely_dirty() {
        let mut bus = Bus::new();
        bus.clear_rdram_dirty();
        let bytes = bincode::serialize(&bus).expect("serialize");
        let restored: Bus = bincode::deserialize(&bytes).expect("deserialize");
        // Length FIRST: `.all()` on an empty iterator is vacuously true, and the
        // first version of this test passed that way against the very bug it was
        // written for.
        assert_eq!(
            restored.rdram_dirty_pages().len(),
            RDRAM_SIZE.div_ceil(RDRAM_PAGE),
            "the dirty map came back the wrong size"
        );
        assert!(
            restored.rdram_dirty_pages().iter().all(|d| *d),
            "a restored Bus came back with clean pages the GPU has never seen"
        );
    }
}
