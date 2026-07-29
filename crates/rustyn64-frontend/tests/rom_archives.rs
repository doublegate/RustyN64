//! Local-only check that [`romfile::read_rom`] handles archives produced by a
//! *real* archiver, not just ones this crate wrote itself.
//!
//! The unit tests in `src/romfile.rs` build their archives with `zip::ZipWriter`,
//! so they exercise a round-trip through one implementation. Real ROM-set zips
//! come from Info-ZIP / 7-Zip, at real sizes (8–64 MiB), with real deflate
//! streams — a different thing to read, and the one users will actually open.
//!
//! **The oracle is the user's own extracted copy.** Each archive is read through
//! `read_rom` and compared byte-for-byte against the already-extracted `.z64` in
//! the gitignored commercial corpus. That is an independent reference: the two
//! files were produced by different tools, so agreement is evidence rather than a
//! restatement of our own output.
//!
//! Skipped unless `RUSTYN64_ROM_ARCHIVES` names a directory of `.zip` ROM
//! archives — no path is hard-coded, and nothing here is committed. ROMs are
//! never committed (see `.gitignore` and the `no-commercial-roms` CI job).

#![cfg(not(target_arch = "wasm32"))]

use std::path::{Path, PathBuf};

use rustyn64_frontend::romfile::read_rom;

/// The extracted corpus the archives are compared against.
fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/roms/external/commercial")
}

/// Every `.z64` under the corpus, keyed by file stem, so an archive can be paired
/// with its extracted twin without hard-coding the save-type folder.
fn extracted_by_stem() -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let Ok(folders) = std::fs::read_dir(corpus_root()) else {
        return out;
    };
    for folder in folders.flatten() {
        let Ok(entries) = std::fs::read_dir(folder.path()) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "z64")
                && let Some(stem) = p.file_stem().and_then(|s| s.to_str())
            {
                out.push((stem.to_owned(), p));
            }
        }
    }
    out
}

/// **A real ROM-set archive reads back byte-identically to its extracted twin.**
///
/// Compares whole images (8–64 MiB), not a prefix: a truncated or mis-decoded
/// deflate stream typically produces a *correct-looking* beginning, so a header
/// check would pass on a broken read.
#[test]
#[ignore = "local-only: set RUSTYN64_ROM_ARCHIVES to a directory of .zip ROMs"]
fn real_archives_match_their_extracted_twins() {
    let Ok(dir) = std::env::var("RUSTYN64_ROM_ARCHIVES") else {
        // Skipping, NOT passing. Said loudly because a green tick here would
        // otherwise read as "real archives verified" when nothing was read at
        // all. It stays a skip rather than a failure so the test is runnable by
        // someone without a ROM corpus, matching `commercial_boot.rs`.
        eprintln!(
            "SKIPPED (verified nothing): RUSTYN64_ROM_ARCHIVES is unset, so no \
             archive was read. Set it to a directory of .zip ROMs to actually \
             exercise this test."
        );
        return;
    };
    let extracted = extracted_by_stem();
    assert!(
        !extracted.is_empty(),
        "no extracted .z64 found under {} — nothing to compare against, so a \
         pass here would be vacuous",
        corpus_root().display()
    );

    let Ok(entries) = std::fs::read_dir(&dir) else {
        panic!("cannot read RUSTYN64_ROM_ARCHIVES directory {dir}");
    };

    let mut checked = 0usize;
    for entry in entries.flatten() {
        let archive = entry.path();
        if archive.extension().is_none_or(|x| x != "zip") {
            continue;
        }
        // The archive is named "Title (USA).zip"; the extracted twin is
        // "Title.z64". Pair by requiring the archive stem to be the twin's stem
        // followed by a No-Intro region tag — i.e. the remainder starts with
        // " (".
        //
        // A bare `starts_with` is NOT enough, and this is not hypothetical: it
        // paired "F-1 World Grand Prix II (USA)" with "F-1 World Grand Prix.z64"
        // and reported a byte mismatch between two different games as if
        // `read_rom` had failed. Longest match wins as a second guard.
        let Some(arc_stem) = archive.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Some((_, twin)) = extracted
            .iter()
            .filter(|(stem, _)| {
                arc_stem
                    .strip_prefix(stem.as_str())
                    .is_some_and(|rest| rest.starts_with(" ("))
            })
            .max_by_key(|(stem, _)| stem.len())
        else {
            continue;
        };

        let from_archive = read_rom(&archive)
            .unwrap_or_else(|e| panic!("read_rom failed on {}: {e}", archive.display()));
        let from_disk =
            std::fs::read(twin).unwrap_or_else(|e| panic!("read {}: {e}", twin.display()));

        assert_eq!(
            from_archive.len(),
            from_disk.len(),
            "{}: extracted length differs from the twin",
            archive.display()
        );
        assert!(
            from_archive == from_disk,
            "{}: extracted bytes differ from the twin",
            archive.display()
        );
        checked += 1;
        eprintln!(
            "ok {} ({} bytes)",
            archive.file_name().unwrap().to_string_lossy(),
            from_archive.len()
        );
    }

    // Witness that work happened: an empty directory, or a naming scheme that
    // paired nothing, would otherwise pass every assertion above without reading
    // a single archive.
    assert!(
        checked > 0,
        "no archive in {dir} paired with an extracted .z64 — the test proved nothing"
    );
    eprintln!("{checked} archive(s) matched their extracted twin");
}
