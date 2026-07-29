//! SCRATCH — corpus-wide render census. Not for commit.
#![allow(missing_docs, clippy::doc_markdown)]
use std::io::Write as _;
use std::path::Path;
use rustyn64_core::System;
use rustyn64_core::vi::VI_CTRL;
use rustyn64_test_harness::rom;

#[test]
#[ignore = "local probe"]
fn r18_probe() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/roms/external/commercial");
    let mut rows = Vec::new();
    let Ok(folders) = std::fs::read_dir(&root) else { return };
    for folder in folders.flatten() {
        let Ok(entries) = std::fs::read_dir(folder.path()) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().is_none_or(|x| x != "z64") { continue }
            let Ok(image) = std::fs::read(&p) else { continue };
            let mut sys = System::new(0);
            if rom::hle_boot(&mut sys, &image).is_err() { continue }
            let mut best = 0usize;
            let mut dims = (0u32, 0u32);
            for f in 1..=420u64 {
                let t = sys.master_ticks().saturating_add(rustyn64_core::MASTER_HZ / 60);
                sys.run_until(t);
                if f % 60 != 0 { continue }
                let mut fb = vec![0u8; 720 * 576 * 4];
                let (w, h) = sys.bus.scanout_scaled(&mut fb);
                if w == 0 || h == 0 { continue }
                let lit = fb.chunks_exact(4).take((w * h) as usize)
                    .filter(|q| q[0] != 0 || q[1] != 0 || q[2] != 0).count();
                if lit > best { best = lit; dims = (w, h); }
            }
            let name = p.file_stem().unwrap().to_string_lossy().to_string();
            println!("  {:<52} rdp={:<8} {}x{} lit={best} vi=0x{:X}",
                name, sys.bus.rdp.commands_processed, dims.0, dims.1,
                sys.bus.vi.read(VI_CTRL));
            std::io::stdout().flush().ok();
            rows.push((sys.bus.rdp.commands_processed, best, name));
        }
    }
    let rendering = rows.iter().filter(|(c, l, _)| *c > 1000 && *l > 1000).count();
    println!("\n  === {} of {} titles issue >1000 RDP commands AND light >1000 px ===",
        rendering, rows.len());
    let silent: Vec<_> = rows.iter().filter(|(c, _, _)| *c == 0).map(|(_, _, n)| n.as_str()).collect();
    println!("  {} titles issue ZERO RDP commands: {}", silent.len(), silent.join(", "));
}
