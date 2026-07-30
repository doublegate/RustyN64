//! Reading a ROM image off the host filesystem — plain, or from inside a `.zip`.
//!
//! ROM sets are almost always distributed as one-`.z64`-per-`.zip`, so requiring
//! the user to unpack first is friction with no upside. This module takes a path
//! and returns the ROM bytes, transparently unwrapping a zip container.
//!
//! **Byte order is not this module's problem.** `.z64` (big-endian), `.n64`
//! (little-endian) and `.v64` (byte-swapped) are normalized downstream by
//! `rustyn64_cart`'s `RomFormat` detection, which sniffs the header magic rather
//! than trusting a file extension. This module only has to produce the bytes.
//!
//! **The archive is untrusted input** (module 60): it is sniffed by magic rather
//! than by extension, its declared sizes are bounds-checked *before* any
//! allocation, decompression is hard-capped so a lying header or a zip bomb
//! cannot exhaust memory, and nothing is ever written to disk — so the classic
//! zip-slip path-traversal class does not arise here at all.

use std::fmt;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Extensions recognized as a ROM image inside an archive. Matched
/// case-insensitively; the *outer* file is identified by magic, not extension.
const ROM_EXTENSIONS: [&str; 3] = ["z64", "n64", "v64"];

/// The largest ROM this will decompress, in bytes.
///
/// The cartridge domain-1 address space is `0x1000_0000..=0x1FBF_FFFF`, so no
/// real N64 cartridge exceeds 64 MiB (the largest retail ROM, *Resident Evil 2*,
/// is 64 MiB). Anything larger is either not a ROM or is a decompression bomb, so
/// it is refused rather than allocated.
pub const MAX_ROM_BYTES: u64 = 64 * 1024 * 1024;

/// The local-file-header magic every non-empty zip archive starts with.
const ZIP_MAGIC: [u8; 4] = [b'P', b'K', 0x03, 0x04];

/// Why a ROM file could not be read.
#[derive(Debug)]
pub enum RomFileError {
    /// The file could not be opened or read.
    Io(std::io::Error),
    /// The file starts with the zip magic but is not a readable archive.
    Archive(String),
    /// The archive holds no member with a `.z64`/`.n64`/`.v64` extension.
    NoRomMember,
    /// The archive holds more than one ROM member, so which to boot is
    /// ambiguous. Carries the candidate names, sorted, so the message can name
    /// them — guessing (say, "the largest") would silently boot the wrong game.
    AmbiguousMembers(Vec<String>),
    /// The image is larger than [`MAX_ROM_BYTES`], so it is not a plausible N64
    /// ROM. Carries the offending size.
    TooLarge(u64),
}

impl fmt::Display for RomFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "cannot read ROM file: {e}"),
            Self::Archive(e) => write!(f, "cannot read zip archive: {e}"),
            Self::NoRomMember => write!(f, "the archive contains no .z64/.n64/.v64 ROM"),
            Self::AmbiguousMembers(names) => write!(
                f,
                "the archive contains {} ROMs ({}); extract the one you want",
                names.len(),
                names.join(", ")
            ),
            Self::TooLarge(n) => write!(
                f,
                "ROM is {n} bytes, larger than the {MAX_ROM_BYTES}-byte maximum for an N64 cartridge",
            ),
        }
    }
}

impl std::error::Error for RomFileError {}

impl From<std::io::Error> for RomFileError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// Read a ROM image from `path`, transparently unwrapping a zip archive.
///
/// The container is identified by **magic, not extension**: a zip named
/// `game.z64` is still unwrapped, and a raw ROM named `game.zip` is still read
/// verbatim. That matters because ROM sets are inconsistently named, and trusting
/// the extension would turn a mislabeled file into a confusing parse error much
/// further downstream.
///
/// # Errors
///
/// Returns [`RomFileError`] if the file cannot be read, the archive is malformed,
/// it holds no ROM member or more than one, or the image exceeds
/// [`MAX_ROM_BYTES`].
pub fn read_rom(path: &Path) -> Result<Vec<u8>, RomFileError> {
    let mut file = File::open(path)?;
    let mut magic = [0u8; 4];
    // A file shorter than the magic cannot be an archive; rewind and let the
    // plain path read (and the ROM parser reject) whatever it actually is.
    let is_zip = match file.read_exact(&mut magic) {
        Ok(()) => magic == ZIP_MAGIC,
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => false,
        Err(e) => return Err(e.into()),
    };
    file.seek(SeekFrom::Start(0))?;

    if is_zip {
        read_from_zip(file)
    } else {
        read_plain(file)
    }
}

/// Read at most `cap` bytes from `reader`, refusing a stream that exceeds it.
///
/// **Both** the plain and the zip path go through here, deliberately. `declared`
/// is whatever the source *claims* the size is — file metadata, or the zip entry
/// header — and is only ever a hint: it lets an obviously-oversized input be
/// refused before a byte is read, and sizes the buffer. The **read itself is
/// capped independently**, because `declared` cannot be trusted:
///
/// - A zip entry header is attacker-controlled and can understate the real
///   decompressed length (the classic bomb).
/// - `File::metadata().len()` reports **0** for a FIFO, a character device, or a
///   pipe, so a declared-size check alone passes trivially and an unbounded
///   `read_to_end` on `/dev/urandom` never returns.
/// - The file can grow between the `metadata()` call and the read.
///
/// `take(cap + 1)` is the enforcement: if the reader yields that extra byte, the
/// stream is genuinely over-long whatever it declared.
fn read_capped<R: Read>(reader: R, declared: u64, cap: u64) -> Result<Vec<u8>, RomFileError> {
    if declared > cap {
        return Err(RomFileError::TooLarge(declared));
    }
    // `declared` is bounded by `cap` here, so the cast cannot lose information.
    let mut buf = Vec::with_capacity(usize::try_from(declared).unwrap_or(0));
    let read = reader.take(cap + 1).read_to_end(&mut buf)?;
    if read as u64 > cap {
        return Err(RomFileError::TooLarge(read as u64));
    }
    Ok(buf)
}

/// Read a bare ROM image, refusing anything implausibly large before allocating.
fn read_plain(file: File) -> Result<Vec<u8>, RomFileError> {
    let declared = file.metadata()?.len();
    read_capped(file, declared, MAX_ROM_BYTES)
}

/// Extract the single ROM member from a zip archive.
fn read_from_zip(file: File) -> Result<Vec<u8>, RomFileError> {
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| RomFileError::Archive(e.to_string()))?;

    // Collect candidates by index so the archive is only borrowed briefly, and
    // sort by name so the ambiguity message (and any future tie-break) is
    // deterministic rather than dependent on central-directory order.
    let mut candidates: Vec<(String, usize)> = Vec::new();
    for i in 0..archive.len() {
        // A malformed entry header is REPORTED, not skipped. Swallowing it would
        // turn "this archive is corrupt" into "this archive contains no ROM",
        // which sends the user looking for the wrong problem — and if the corrupt
        // entry were the only ROM, the real cause would never surface.
        let entry = archive
            .by_index_raw(i)
            .map_err(|e| RomFileError::Archive(format!("entry {i}: {e}")))?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_owned();
        let Some(ext) = Path::new(&name)
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
        else {
            continue;
        };
        if ROM_EXTENSIONS.contains(&ext.as_str()) {
            candidates.push((name, i));
        }
    }
    candidates.sort_by(|a, b| a.0.cmp(&b.0));

    let (_, index) = match candidates.len() {
        0 => return Err(RomFileError::NoRomMember),
        1 => candidates.remove(0),
        _ => {
            return Err(RomFileError::AmbiguousMembers(
                candidates.into_iter().map(|(n, _)| n).collect(),
            ));
        }
    };

    let entry = archive
        .by_index(index)
        .map_err(|e| RomFileError::Archive(e.to_string()))?;
    let declared = entry.size();
    read_capped(entry, declared, MAX_ROM_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Build a zip in memory containing `members`, each stored uncompressed.
    fn zip_with(members: &[(&str, &[u8])]) -> Vec<u8> {
        zip_with_method(members, zip::CompressionMethod::Stored)
    }

    /// Build a zip in memory using an explicit compression method.
    fn zip_with_method(members: &[(&str, &[u8])], method: zip::CompressionMethod) -> Vec<u8> {
        let mut cursor = std::io::Cursor::new(Vec::new());
        {
            let mut w = zip::ZipWriter::new(&mut cursor);
            let opts = zip::write::SimpleFileOptions::default().compression_method(method);
            for (name, body) in members {
                w.start_file(*name, opts).expect("start_file");
                w.write_all(body).expect("write member");
            }
            w.finish().expect("finish");
        }
        cursor.into_inner()
    }

    /// Write `bytes` to a uniquely-named scratch file and return its path.
    fn scratch(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("rustyn64-romfile-tests");
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let path = dir.join(name);
        std::fs::write(&path, bytes).expect("write scratch");
        path
    }

    /// **A bare ROM file is returned verbatim.** The baseline: the archive path
    /// must not perturb the ordinary case.
    #[test]
    fn a_plain_rom_is_read_verbatim() {
        let body = [0x80, 0x37, 0x12, 0x40, 0xDE, 0xAD, 0xBE, 0xEF];
        let path = scratch("plain.z64", &body);
        assert_eq!(read_rom(&path).expect("read"), body);
    }

    /// **A ROM inside a zip is extracted.** Asserts the *contents*, not merely
    /// that the call succeeded — a stub returning an empty vec would satisfy a
    /// success-only check.
    #[test]
    fn a_rom_inside_a_zip_is_extracted() {
        let body: Vec<u8> = (0u8..=255).cycle().take(4096).collect();
        let path = scratch("game.zip", &zip_with(&[("Some Game (USA).z64", &body)]));
        assert_eq!(read_rom(&path).expect("read"), body);
    }

    /// **A DEFLATE-compressed member decompresses correctly.** Real ROM-set
    /// archives are deflated, not stored, so a `Stored`-only test would leave the
    /// entire decompression path unexercised — and the body here is deliberately
    /// compressible (a repeating pattern) so the stored and deflated streams
    /// genuinely differ.
    #[test]
    fn a_deflated_member_is_decompressed() {
        let body: Vec<u8> = (0u8..16).cycle().take(64 * 1024).collect();
        let archive = zip_with_method(
            &[("Some Game (USA).z64", &body)],
            zip::CompressionMethod::Deflated,
        );
        // Guard the premise: if this were not smaller, the member was not
        // actually compressed and the test would silently degrade to `Stored`.
        assert!(
            archive.len() < body.len(),
            "the member must really be deflated ({} vs {} bytes)",
            archive.len(),
            body.len()
        );
        let path = scratch("deflated.zip", &archive);
        assert_eq!(read_rom(&path).expect("read"), body);
    }

    /// **The container is identified by magic, not extension.** Both directions:
    /// a zip named `.z64` is unwrapped, and a raw ROM named `.zip` is not.
    #[test]
    fn the_container_is_detected_by_magic_not_extension() {
        let body = [0x80, 0x37, 0x12, 0x40, 1, 2, 3, 4];
        let zipped = scratch("mislabeled.z64", &zip_with(&[("real.z64", &body)]));
        assert_eq!(read_rom(&zipped).expect("zip named .z64"), body);

        let bare = scratch("mislabeled.zip", &body);
        assert_eq!(read_rom(&bare).expect("ROM named .zip"), body);
    }

    /// **A member's extension is matched case-insensitively**, since ROM-set
    /// archives are inconsistent about it.
    #[test]
    fn member_extensions_match_case_insensitively() {
        let body = [1u8, 2, 3, 4];
        let path = scratch("upper.zip", &zip_with(&[("GAME.Z64", &body)]));
        assert_eq!(read_rom(&path).expect("read"), body);
    }

    /// **A non-ROM member is ignored**, so the common "ROM + readme" archive
    /// still resolves to exactly one candidate.
    #[test]
    fn non_rom_members_are_ignored() {
        let body = [9u8, 8, 7, 6];
        let path = scratch(
            "withreadme.zip",
            &zip_with(&[("readme.txt", b"notes"), ("game.n64", &body)]),
        );
        assert_eq!(read_rom(&path).expect("read"), body);
    }

    /// **An archive with no ROM is an error, not an empty image.** Booting zero
    /// bytes would fail much later with an unrelated message.
    #[test]
    fn an_archive_without_a_rom_is_rejected() {
        let path = scratch("empty.zip", &zip_with(&[("readme.txt", b"notes")]));
        assert!(matches!(read_rom(&path), Err(RomFileError::NoRomMember)));
    }

    /// **Two ROMs in one archive is an error naming both.** Picking one (the
    /// first, the largest) would silently boot a game the user did not choose.
    #[test]
    fn a_multi_rom_archive_is_ambiguous_and_names_the_candidates() {
        let path = scratch(
            "two.zip",
            &zip_with(&[("b.z64", &[1, 2]), ("a.z64", &[3, 4])]),
        );
        let Err(RomFileError::AmbiguousMembers(names)) = read_rom(&path) else {
            panic!("expected an ambiguity error");
        };
        // Sorted, so the message is deterministic rather than archive-order.
        assert_eq!(names, ["a.z64", "b.z64"]);
    }

    /// **An over-large plain image is refused before it is allocated.** Pinned at
    /// one byte past the cap so the boundary itself is tested, not merely a value
    /// far beyond it.
    #[test]
    fn an_oversized_plain_image_is_refused() {
        let path = scratch("huge.z64", &[]);
        // Extend to one byte past the cap without materializing it in memory.
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("open");
        f.set_len(MAX_ROM_BYTES + 1).expect("set_len");
        drop(f);
        assert!(matches!(
            read_rom(&path),
            Err(RomFileError::TooLarge(n)) if n == MAX_ROM_BYTES + 1
        ));
    }

    /// **A ROM exactly at the cap is accepted**, so the limit is inclusive and
    /// the largest retail cartridge (64 MiB) still loads. Without this, the test
    /// above would pass just as well with an off-by-one that rejects every 64 MiB
    /// ROM.
    #[test]
    fn an_image_exactly_at_the_cap_is_accepted() {
        let path = scratch("atcap.z64", &[]);
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("open");
        f.set_len(MAX_ROM_BYTES).expect("set_len");
        drop(f);
        let rom = read_rom(&path).expect("a 64 MiB ROM is valid");
        assert_eq!(rom.len() as u64, MAX_ROM_BYTES);
    }

    /// **A stream that exceeds the cap is refused even when it DECLARES a small
    /// size.** This is the actual anti-bomb control — the branch the module's
    /// whole security argument rests on — and it had no coverage: a zip written
    /// by `ZipWriter` always declares its size honestly, so no archive fixture
    /// can reach it.
    ///
    /// Testing [`read_capped`] directly with a small cap does reach it, and is
    /// the *same* code path both callers use. A declared size of 0 also covers
    /// the FIFO / character-device case, where `metadata().len()` reports 0 and a
    /// declared-size check alone would pass trivially.
    #[test]
    fn a_stream_longer_than_the_cap_is_refused_whatever_it_declares() {
        let body = vec![0xAAu8; 100];
        // Declares 10 bytes, really 100: the header lies.
        assert!(matches!(
            read_capped(body.as_slice(), 10, 32),
            Err(RomFileError::TooLarge(n)) if n == 33
        ));
        // Declares nothing at all — what a FIFO or /dev/urandom looks like.
        assert!(matches!(
            read_capped(body.as_slice(), 0, 32),
            Err(RomFileError::TooLarge(n)) if n == 33
        ));
        // And an honest stream at exactly the cap still succeeds, so the guard
        // is not simply rejecting everything.
        let exact = vec![0xBBu8; 32];
        assert_eq!(
            read_capped(exact.as_slice(), 32, 32).expect("at the cap"),
            exact
        );
    }

    /// **An oversized DECLARED size is refused before the stream is read.** The
    /// cheap first gate: the reader here would panic if touched, so this fails if
    /// the declared-size check is ever removed in favor of the read cap alone.
    #[test]
    fn an_oversized_declared_size_is_refused_without_reading() {
        struct Exploding;
        impl Read for Exploding {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                panic!("the stream must not be read when the declared size is over the cap");
            }
        }
        assert!(matches!(
            read_capped(Exploding, 1000, 32),
            Err(RomFileError::TooLarge(n)) if n == 1000
        ));
    }

    /// **A file too short to hold the magic is read as a plain image**, not
    /// treated as a truncated archive.
    #[test]
    fn a_file_shorter_than_the_magic_is_read_plainly() {
        let path = scratch("tiny.z64", &[0x80, 0x37]);
        assert_eq!(read_rom(&path).expect("read"), [0x80, 0x37]);
    }
}
