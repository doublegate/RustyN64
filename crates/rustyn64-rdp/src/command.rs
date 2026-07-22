//! RDP command-stream decoding.
//!
//! The front of the DP FIFO: given a command's first word, how many 64-bit
//! words does the whole command occupy? Getting this right for every opcode
//! `0x00`–`0x3F` is what keeps `DPC_CURRENT` aligned, so a multi-word primitive
//! (a triangle, a texture rectangle) is consumed whole and the stream never
//! desyncs the pointer mid-command.
//!
//! Reference: `n64brew_wiki/markdown/Reality Display Processor/Commands.md`
//! (the full opcode map). This module decodes *length* only — dispatching each
//! opcode to a rasterizer handler is later Phase 3 work.

/// The opcode field of an RDP command: bits 61:56 of the command's first 64-bit
/// word, i.e. bits 29:24 of that word's high half. Six bits, `0x00`–`0x3F`.
#[must_use]
pub const fn opcode_of(word0_hi: u32) -> u8 {
    ((word0_hi >> 24) & 0x3F) as u8
}

/// The length, in 64-bit (8-byte) words, of the RDP command with opcode
/// `opcode` (as returned by [`opcode_of`]). Includes the header word itself.
///
/// Every command is a single word except:
///
/// - **Fill Triangle** (`0x08`–`0x0F`): a 4-word base plus optional coefficient
///   blocks. The opcode's low three bits *are* the enable flags — bit 2 shade,
///   bit 1 texture, bit 0 z-buffer (the very bits 58/57/56 the wiki also lists
///   by name in word 0) — appending 8, 8, and 2 words respectively, in that
///   order. So `0x08` (plain) is 4 words and `0x0F` (shade+texture+z) is 22.
/// - **Texture Rectangle** / **Texture Rectangle Flip** (`0x24`/`0x25`):
///   2 words.
///
/// Every other opcode — including the no-operation ranges (`0x00`–`0x07`,
/// `0x10`–`0x23`, `0x31`) and any not-yet-handled command — is a single word,
/// so an unrecognised command consumes exactly its header and the FIFO keeps
/// its alignment.
#[must_use]
pub const fn command_len_words(opcode: u8) -> u32 {
    match opcode & 0x3F {
        0x08..=0x0F => {
            // The low three opcode bits select the appended coefficient blocks.
            let shade = ((opcode >> 2) & 1) as u32;
            let texture = ((opcode >> 1) & 1) as u32;
            let zbuffer = (opcode & 1) as u32;
            4 + shade * 8 + texture * 8 + zbuffer * 2
        }
        0x24 | 0x25 => 2,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `opcode_of` reads bits 61:56 of the 64-bit command word (bits 29:24 of
    /// the high half), masking off everything above the 6-bit field.
    #[test]
    fn opcode_of_reads_bits_61_to_56() {
        assert_eq!(opcode_of(0x3F << 24), 0x3F);
        assert_eq!(opcode_of(0x08 << 24), 0x08);
        // Upper bits (63:62) are not part of the opcode and are ignored.
        assert_eq!(opcode_of(0xFF00_0000), 0x3F);
    }

    /// The eight triangle forms `0x08`–`0x0F` decode to the exact lengths the
    /// N64brew command map gives: 4-word base, +8 shade, +8 texture, +2 z, with
    /// the appended blocks selected by the opcode's low three bits.
    #[test]
    fn triangle_lengths_match_the_command_map() {
        assert_eq!(command_len_words(0x08), 4, "Fill Triangle (base)");
        assert_eq!(command_len_words(0x09), 6, "Fill Triangle (Z): +2");
        assert_eq!(command_len_words(0x0A), 12, "Fill Triangle (T): +8");
        assert_eq!(command_len_words(0x0B), 14, "Fill Triangle (TZ): +8+2");
        assert_eq!(command_len_words(0x0C), 12, "Fill Triangle (S): +8");
        assert_eq!(command_len_words(0x0D), 14, "Fill Triangle (SZ): +8+2");
        assert_eq!(command_len_words(0x0E), 20, "Fill Triangle (ST): +8+8");
        assert_eq!(command_len_words(0x0F), 22, "Fill Triangle (STZ): +8+8+2");
    }

    /// The texture-rectangle pair is two words; every other opcode across the
    /// whole `0x00`–`0x3F` map — no-ops, syncs, set-state, load, fill — is a
    /// single word. Exhaustive so a wrong length anywhere is caught.
    #[test]
    fn every_non_triangle_opcode_has_its_documented_length() {
        for opcode in 0x00u8..=0x3F {
            let expected = match opcode {
                0x08..=0x0F => continue, // covered above
                0x24 | 0x25 => 2,
                _ => 1,
            };
            assert_eq!(
                command_len_words(opcode),
                expected,
                "opcode {opcode:#04x} length"
            );
        }
    }

    /// The high two bits of the opcode byte (which are not part of the 6-bit
    /// field) never change the decoded length.
    #[test]
    fn length_ignores_bits_above_the_opcode_field() {
        for opcode in 0x00u8..=0x3F {
            assert_eq!(
                command_len_words(opcode),
                command_len_words(opcode | 0xC0),
                "opcode {opcode:#04x} masks to six bits"
            );
        }
    }
}
