//! The **SP interface** register file (T-21-002).
//!
//! Eight registers at `0x0404_0000` plus `SP_PC` at `0x0408_0000`, memory-mapped
//! into the VR4300's address space and simultaneously exposed to the RSP itself
//! as COP0 registers `c0`–`c7`. There is one set of physical registers behind
//! both views (N64brew *RSP Interface* §RSP Internal Registers), which is why
//! this module holds them rather than either side owning a copy.
//!
//! # Why the write layout differs from the read layout
//!
//! `SP_STATUS` reads as a flag word and writes as a list of **set/clear
//! commands** — two bits per flag. That is not an encoding quirk to normalise
//! away: it exists so either processor can change one flag with a single store,
//! without the read-modify-write that would race the other. Collapsing the two
//! layouts into one would reintroduce exactly the race the hardware design
//! removes.
//!
//! The corollary is the rule that catches naive implementations: writing a
//! flag's **set and clear bits together leaves it unchanged**. n64-systemtest
//! checks this for every flag it can reach.

/// `SP_STATUS.HALTED` — the RSP is paused and fetches nothing.
pub const STATUS_HALTED: u32 = 1 << 0;
/// `SP_STATUS.BROKE` — a `BREAK` has executed since this was last cleared.
pub const STATUS_BROKE: u32 = 1 << 1;
/// `SP_STATUS.DMA_BUSY` — a transfer is in progress.
pub const STATUS_DMA_BUSY: u32 = 1 << 2;
/// `SP_STATUS.DMA_FULL` — a second transfer is queued behind the current one.
pub const STATUS_DMA_FULL: u32 = 1 << 3;
/// `SP_STATUS.IO_BUSY`.
pub const STATUS_IO_BUSY: u32 = 1 << 4;
/// `SP_STATUS.SSTEP` — single-step mode.
pub const STATUS_SSTEP: u32 = 1 << 5;
/// `SP_STATUS.INTBREAK` — raise the MI interrupt when `BREAK` executes.
pub const STATUS_INTBREAK: u32 = 1 << 6;
/// `SP_STATUS.SIG0` — the first of eight software-defined signal bits.
///
/// `SIG<n>` is bit `7 + n`, so the eight occupy bits 7..=14.
pub const STATUS_SIG0: u32 = 1 << 7;

/// Register indices, shared by the CPU's `0x0404_00xx` window and the RSP's
/// COP0 `c0`–`c7`.
pub mod reg {
    /// `SP_DMA_SPADDR` — the DMEM/IMEM side of a transfer.
    pub const DMA_SPADDR: u32 = 0;
    /// `SP_DMA_RAMADDR` — the RDRAM side.
    pub const DMA_RAMADDR: u32 = 1;
    /// `SP_DMA_RDLEN` — writing it starts an RDRAM to DMEM/IMEM transfer.
    pub const DMA_RDLEN: u32 = 2;
    /// `SP_DMA_WRLEN` — writing it starts a DMEM/IMEM to RDRAM transfer.
    pub const DMA_WRLEN: u32 = 3;
    /// `SP_STATUS`.
    pub const STATUS: u32 = 4;
    /// `SP_DMA_FULL` — a read-only mirror of `SP_STATUS.DMA_FULL`.
    pub const DMA_FULL: u32 = 5;
    /// `SP_DMA_BUSY` — a read-only mirror of `SP_STATUS.DMA_BUSY`.
    pub const DMA_BUSY: u32 = 6;
    /// `SP_SEMAPHORE` — the hardware-assisted mutex bit.
    pub const SEMAPHORE: u32 = 7;
}

/// A programmed DMA, latched from the address and length registers.
///
/// Returned to the Bus to execute rather than performed here: the RSP does not
/// own RDRAM, and a chip reaching back into its owner is the dependency cycle
/// `docs/architecture.md` exists to prevent. The PI engine returns a transfer
/// description for the same reason.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Dma {
    /// DMEM/IMEM byte offset, with bit 12 selecting IMEM.
    pub sp_addr: u32,
    /// RDRAM byte address.
    pub ram_addr: u32,
    /// Bytes per row. Already rounded **up** to a multiple of 8.
    pub row_len: u32,
    /// Number of rows.
    pub rows: u32,
    /// Bytes skipped in RDRAM between rows. The SP side stays contiguous.
    pub skip: u32,
    /// `true` for DMEM/IMEM to RDRAM (`SP_DMA_WRLEN`).
    pub to_dram: bool,
}

/// The SP interface registers.
#[derive(Clone, Copy, Debug)]
pub struct SpRegs {
    /// The flag word, in its **read** layout.
    status: u32,
    /// `SP_PC`, 12 bits.
    pc: u32,
    /// The semaphore bit.
    ///
    /// Starts **taken**. A read returns the current value and then sets it, so
    /// the sequence the hardware produces is: write (any value) clears it, the
    /// next read returns 0 and takes it, and every read after that returns 1.
    /// n64-systemtest states exactly that in its own words — *"If Semaphore is
    /// written to (value doesn't matter), the next read will return 0.
    /// Otherwise it returns 1"* — and checks five consecutive reads.
    semaphore: bool,
    /// The SP-side address of the current (or last completed) transfer.
    sp_addr: u32,
    /// The RDRAM-side address of the current (or last completed) transfer.
    ram_addr: u32,
    /// The length word as it reads back.
    ///
    /// Not the value written: after a transfer completes, the length field
    /// reads `0xFF8`, because the hardware decrements it by 8 per 64-bit word
    /// and stops at `-8`. `SP_DMA_RDLEN` and `SP_DMA_WRLEN` return the *same*
    /// data regardless of the direction that was programmed.
    len: u32,
    /// The address/length values a write has staged but not yet started.
    pending_sp_addr: u32,
    /// Staged RDRAM address.
    pending_ram_addr: u32,
}

impl Default for SpRegs {
    fn default() -> Self {
        Self::new()
    }
}

impl SpRegs {
    /// Power-on state: **halted**, semaphore free.
    ///
    /// The RSP comes out of reset halted and idles until the CPU clears the bit;
    /// n64-systemtest's `StartupTest` reads `SP_STATUS` expecting exactly `0x1`.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            status: STATUS_HALTED,
            pc: 0,
            semaphore: false,
            sp_addr: 0,
            ram_addr: 0,
            len: 0,
            pending_sp_addr: 0,
            pending_ram_addr: 0,
        }
    }

    /// The `SP_STATUS` flag word, as read.
    #[must_use]
    pub const fn status(&self) -> u32 {
        self.status
    }

    /// Is the RSP halted?
    #[must_use]
    pub const fn halted(&self) -> bool {
        self.status & STATUS_HALTED != 0
    }

    /// Halt or release the RSP from within the chip (a `BREAK`, or single-step).
    pub const fn set_halted(&mut self, halted: bool) {
        if halted {
            self.status |= STATUS_HALTED;
        } else {
            self.status &= !STATUS_HALTED;
        }
    }

    /// `SP_PC`.
    #[must_use]
    pub const fn pc(&self) -> u32 {
        self.pc
    }

    /// Set `SP_PC`, masked to the 12 bits IMEM has.
    pub const fn set_pc(&mut self, pc: u32) {
        self.pc = pc & 0xFFC;
    }

    /// Read a register by index. `mi_sp` is not touched here — see [`Self::write`].
    #[must_use]
    pub const fn read(&mut self, index: u32) -> u32 {
        match index & 7 {
            reg::DMA_SPADDR => self.sp_addr,
            reg::DMA_RAMADDR => self.ram_addr,
            // Both length registers report the same transfer, whichever
            // direction it was programmed in.
            reg::DMA_RDLEN | reg::DMA_WRLEN => self.len,
            reg::STATUS => self.status,
            reg::DMA_FULL => (self.status & STATUS_DMA_FULL != 0) as u32,
            reg::DMA_BUSY => (self.status & STATUS_DMA_BUSY != 0) as u32,
            // The read that takes the mutex: the previous value is returned and
            // the bit is set, so a reader that sees 0 has just acquired it.
            _ => {
                let was = self.semaphore;
                self.semaphore = true;
                was as u32
            }
        }
    }

    /// Write a register by index.
    ///
    /// Returns a [`Dma`] when the write started one — the Bus performs it, for
    /// the reason given on that type.
    ///
    /// The SP interrupt line is **not** updated here. `SP_STATUS` can both raise
    /// and acknowledge it, so the caller reads [`Self::interrupt_change`]
    /// afterwards; returning it through the same channel as a DMA would conflate
    /// two independent effects of one write.
    pub const fn write(&mut self, index: u32, val: u32) -> Option<Dma> {
        match index & 7 {
            // The address registers latch as *pending* and only become visible
            // when a transfer starts. Reads keep returning the ongoing or last
            // completed transfer's values until then.
            reg::DMA_SPADDR => {
                self.pending_sp_addr = val & 0x1FF8;
                None
            }
            reg::DMA_RAMADDR => {
                self.pending_ram_addr = val & 0x00FF_FFF8;
                None
            }
            reg::DMA_RDLEN => Some(self.start_dma(val, false)),
            reg::DMA_WRLEN => Some(self.start_dma(val, true)),
            reg::STATUS => {
                self.write_status(val);
                None
            }
            // The two mirrors are read-only, and the semaphore ignores the
            // value written -- writing it *releases* the mutex whatever the
            // operand, which is why the suite writes 0, 1 and 0xFFFFFFFF and
            // expects the same result from each.
            reg::SEMAPHORE => {
                self.semaphore = false;
                None
            }
            _ => None,
        }
    }

    /// Apply a `SP_STATUS` write, in its set/clear-command layout.
    const fn write_status(&mut self, val: u32) {
        /// `CLR_HALT` is bit 0 and `SET_HALT` bit 1; every later flag follows the
        /// same clear-then-set pairing.
        const CLR_HALT: u32 = 1 << 0;
        const SET_HALT: u32 = 1 << 1;
        const CLR_BROKE: u32 = 1 << 2;
        const CLR_INTR: u32 = 1 << 3;
        const SET_INTR: u32 = 1 << 4;
        const CLR_SSTEP: u32 = 1 << 5;
        const SET_SSTEP: u32 = 1 << 6;
        const CLR_INTBREAK: u32 = 1 << 7;
        const SET_INTBREAK: u32 = 1 << 8;

        self.apply(val, CLR_HALT, SET_HALT, STATUS_HALTED);
        // BROKE has a clear command and no set: it is a latch the hardware owns.
        if val & CLR_BROKE != 0 {
            self.status &= !STATUS_BROKE;
        }
        self.apply(val, CLR_SSTEP, SET_SSTEP, STATUS_SSTEP);
        self.apply(val, CLR_INTBREAK, SET_INTBREAK, STATUS_INTBREAK);

        // The eight signal bits, at CLR = 9 + 2n and SET = 10 + 2n. Signals have
        // no hardware meaning -- they exist purely so the two processors can
        // hand-shake -- so they are pure storage, but they obey the same
        // set-and-clear-together rule as everything else.
        let mut n = 0;
        while n < 8 {
            let clr = 1 << (9 + 2 * n);
            let set = 1 << (10 + 2 * n);
            self.apply(val, clr, set, STATUS_SIG0 << n);
            n += 1;
        }

        // INTR is deliberately absent: it is not a `SP_STATUS` flag at all but
        // the MI's SP line, and `take_interrupt_change` reports it.
        let _ = (CLR_INTR, SET_INTR);
    }

    /// Apply one clear/set command pair to one flag.
    ///
    /// **Both bits set means no change** — the rule the whole layout exists for.
    /// Implementing this as "clear then set" instead would silently make the
    /// combination equivalent to a set, which n64-systemtest checks for every
    /// reachable flag.
    const fn apply(&mut self, val: u32, clr: u32, set: u32, flag: u32) {
        let clearing = val & clr != 0;
        let setting = val & set != 0;
        if clearing && setting {
            return;
        }
        if clearing {
            self.status &= !flag;
        } else if setting {
            self.status |= flag;
        }
    }

    /// What a `SP_STATUS` write did to the MI's SP interrupt line.
    ///
    /// `Some(true)` raises it, `Some(false)` acknowledges it, `None` leaves it
    /// alone. Separate from [`Self::write`] because the line lives in the MI,
    /// not here, and because one write can start a DMA *and* touch the line.
    #[must_use]
    pub const fn interrupt_change(val: u32) -> Option<bool> {
        const CLR_INTR: u32 = 1 << 3;
        const SET_INTR: u32 = 1 << 4;
        match (val & CLR_INTR != 0, val & SET_INTR != 0) {
            // Set and clear together: unchanged, exactly as for the flags.
            (true, true) | (false, false) => None,
            (true, false) => Some(false),
            (false, true) => Some(true),
        }
    }

    /// Latch a length write and describe the transfer it starts.
    const fn start_dma(&mut self, len_word: u32, to_dram: bool) -> Dma {
        // The length field is bytes-minus-one and the engine works in 64-bit
        // words, so it rounds **up**: "writing 0 (or any value up to and
        // including 7) starts a transfer of exactly 8 bytes". Rounding down --
        // `(len + 1) & !7` -- turns a 12-byte request into 8 and silently drops
        // the tail.
        let row_len = ((len_word & 0xFFF) | 7) + 1;
        let rows = ((len_word >> 12) & 0xFF) + 1;
        let skip = (len_word >> 20) & 0xFFF;

        // The pending address latches become the visible ones as the transfer
        // starts; until now, reads returned the previous transfer's values.
        self.sp_addr = self.pending_sp_addr;
        self.ram_addr = self.pending_ram_addr;

        Dma {
            sp_addr: self.sp_addr,
            ram_addr: self.ram_addr,
            row_len,
            rows,
            skip,
            to_dram,
        }
    }

    /// Record where a completed transfer left the address and length registers.
    ///
    /// Hardware leaves the pointers **past** the data it moved, and the length
    /// field at `0xFF8` — it is decremented by 8 per word and ends at `-8`.
    /// `COUNT` resets to 0 and `SKIP` is preserved, which is why only the low
    /// field is rewritten here.
    pub const fn complete_dma(&mut self, sp_addr: u32, ram_addr: u32) {
        self.sp_addr = sp_addr;
        self.ram_addr = ram_addr;
        self.len = (self.len & 0xFFF0_0000) | 0xFF8;
        self.status &= !(STATUS_DMA_BUSY | STATUS_DMA_FULL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **Set and clear together leaves the flag alone.** The rule the read/write
    /// asymmetry exists for, and the one a "clear then set" implementation gets
    /// wrong in a way that looks like a set.
    #[test]
    fn setting_and_clearing_a_flag_together_changes_nothing() {
        const CLR_SSTEP: u32 = 1 << 5;
        const SET_SSTEP: u32 = 1 << 6;
        let mut sp = SpRegs::new();

        sp.write(reg::STATUS, SET_SSTEP);
        assert_ne!(sp.status() & STATUS_SSTEP, 0, "set on its own works");
        sp.write(reg::STATUS, SET_SSTEP | CLR_SSTEP);
        assert_ne!(
            sp.status() & STATUS_SSTEP,
            0,
            "both bits together must preserve the prior state (set)"
        );

        sp.write(reg::STATUS, CLR_SSTEP);
        assert_eq!(sp.status() & STATUS_SSTEP, 0, "clear on its own works");
        sp.write(reg::STATUS, SET_SSTEP | CLR_SSTEP);
        assert_eq!(
            sp.status() & STATUS_SSTEP,
            0,
            "and must preserve the prior state when clear, too"
        );
    }

    /// All eight signal bits, at `SIG<n>` = bit `7 + n`, driven by commands at
    /// `9 + 2n` / `10 + 2n`. Tested across the whole range because an off-by-one
    /// in the pairing works for `SIG0` and fails for the rest.
    #[test]
    fn every_signal_bit_sets_and_clears_independently() {
        for n in 0..8 {
            let mut sp = SpRegs::new();
            let clr = 1 << (9 + 2 * n);
            let set = 1 << (10 + 2 * n);
            let flag = STATUS_SIG0 << n;

            sp.write(reg::STATUS, set);
            assert_eq!(sp.status() & flag, flag, "SIG{n} did not set");
            assert_eq!(
                sp.status() & !flag & !STATUS_HALTED,
                0,
                "SIG{n}'s command disturbed another flag"
            );
            sp.write(reg::STATUS, set | clr);
            assert_eq!(sp.status() & flag, flag, "SIG{n} changed on set+clear");
            sp.write(reg::STATUS, clr);
            assert_eq!(sp.status() & flag, 0, "SIG{n} did not clear");
        }
    }

    /// **The semaphore's first read after a write is 0; every later one is 1.**
    ///
    /// Quoted from n64-systemtest's own header comment, and it checks five
    /// consecutive reads. The value written is irrelevant — the suite writes 0,
    /// 1 and `0xFFFF_FFFF` and expects identical behaviour from each.
    #[test]
    fn the_semaphore_is_taken_by_reading_it() {
        for written in [0u32, 1, 0xFFFF_FFFF] {
            let mut sp = SpRegs::new();
            sp.write(reg::SEMAPHORE, written);
            assert_eq!(sp.read(reg::SEMAPHORE), 0, "first read acquires it");
            for _ in 0..4 {
                assert_eq!(sp.read(reg::SEMAPHORE), 1, "and it stays taken");
            }
        }
    }

    /// Writing twice without reading is the same as writing once — the write
    /// sets a state, it does not queue.
    #[test]
    fn writing_the_semaphore_twice_is_the_same_as_once() {
        let mut sp = SpRegs::new();
        sp.write(reg::SEMAPHORE, 6);
        sp.write(reg::SEMAPHORE, 6);
        assert_eq!(sp.read(reg::SEMAPHORE), 0);
        assert_eq!(sp.read(reg::SEMAPHORE), 1);
    }

    /// `SP_STATUS`'s interrupt commands drive the **MI line**, not a status
    /// flag, and obey the same set-and-clear-together rule.
    #[test]
    fn the_interrupt_commands_report_a_line_change_not_a_flag() {
        const CLR_INTR: u32 = 1 << 3;
        const SET_INTR: u32 = 1 << 4;
        assert_eq!(SpRegs::interrupt_change(SET_INTR), Some(true));
        assert_eq!(SpRegs::interrupt_change(CLR_INTR), Some(false));
        assert_eq!(
            SpRegs::interrupt_change(SET_INTR | CLR_INTR),
            None,
            "both together must leave the line alone"
        );
        assert_eq!(SpRegs::interrupt_change(0), None);

        // And it must not leak into the status word.
        let mut sp = SpRegs::new();
        sp.write(reg::STATUS, SET_INTR);
        assert_eq!(
            sp.status(),
            STATUS_HALTED,
            "SET_INTR is not a SP_STATUS flag"
        );
    }

    /// **The length field rounds up to a multiple of 8, never down.**
    ///
    /// "Writing 0 (or any value up to and including 7) starts a transfer of
    /// exactly 8 bytes". Rounding down turns n64-systemtest's `length = 11`
    /// case (12 bytes requested) into 8 and drops the tail.
    #[test]
    fn the_dma_length_rounds_up_to_a_multiple_of_eight() {
        let mut sp = SpRegs::new();
        for (written, want) in [(0u32, 8u32), (7, 8), (8, 16), (11, 16), (15, 16)] {
            let dma = sp.write(reg::DMA_RDLEN, written).expect("a length write");
            assert_eq!(dma.row_len, want, "length field {written}");
            assert_eq!(dma.rows, 1, "count 0 is a single row");
        }
    }

    /// `COUNT` and `SKIP` are real fields. Reading only the low 12 bits moves
    /// one row and silently drops the rest, which is the failure mode for
    /// anything transferring a 2D block.
    #[test]
    fn the_dma_word_carries_count_and_skip() {
        let mut sp = SpRegs::new();
        let dma = sp
            .write(reg::DMA_RDLEN, 7 | (1 << 12) | (8 << 20))
            .expect("a length write");
        assert_eq!(dma.row_len, 8);
        assert_eq!(dma.rows, 2, "count is rows minus one");
        assert_eq!(dma.skip, 8);
    }

    /// The address registers stage as **pending** and only become readable when
    /// a transfer starts; until then reads report the previous transfer.
    #[test]
    fn the_address_registers_are_double_buffered() {
        let mut sp = SpRegs::new();
        sp.write(reg::DMA_SPADDR, 0x40);
        sp.write(reg::DMA_RAMADDR, 0x100);
        assert_eq!(sp.read(reg::DMA_SPADDR), 0, "still pending, not visible");
        assert_eq!(sp.read(reg::DMA_RAMADDR), 0);

        sp.write(reg::DMA_RDLEN, 7);
        assert_eq!(sp.read(reg::DMA_SPADDR), 0x40, "visible once it starts");
        assert_eq!(sp.read(reg::DMA_RAMADDR), 0x100);
    }

    /// After completion the pointers sit past the data and the length field
    /// reads `0xFF8` — the hardware's `-8` after decrementing per word. Both
    /// length registers report it, whichever direction was programmed.
    #[test]
    fn a_completed_dma_leaves_the_registers_past_the_transfer() {
        let mut sp = SpRegs::new();
        sp.write(reg::DMA_SPADDR, 0x50);
        sp.write(reg::DMA_RAMADDR, 0x10);
        sp.write(reg::DMA_WRLEN, 15);
        sp.complete_dma(0x60, 0x20);

        assert_eq!(sp.read(reg::DMA_SPADDR), 0x60);
        assert_eq!(sp.read(reg::DMA_RAMADDR), 0x20);
        assert_eq!(sp.read(reg::DMA_RDLEN), 0xFF8);
        assert_eq!(
            sp.read(reg::DMA_WRLEN),
            0xFF8,
            "both registers report the same transfer"
        );
    }

    /// Power-on is halted, and `SP_STATUS` reads exactly `0x1` — the value
    /// n64-systemtest's startup check expects.
    #[test]
    fn power_on_is_halted_and_nothing_else() {
        let sp = SpRegs::new();
        assert_eq!(sp.status(), 0x1);
        assert!(sp.halted());
    }
}
