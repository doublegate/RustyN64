//! Scratch: which VI path is hot for a real title?
use rustyn64_frontend::emu::EmuCore;
use rustyn64_frontend::input::{N64Buttons, bit};

#[test]
#[ignore]
fn vi_state() {
    let path = std::env::var("RUSTYN64_PROBE_ROM").unwrap();
    let raw = std::fs::read(&path).unwrap();
    let mut core = EmuCore::new(0);
    core.load_rom(&raw).unwrap();
    for f in 0..520 {
        let mut b = N64Buttons::default();
        match (f / 8) % 4 { 0 => b.set(bit::START, true), 2 => b.set(bit::A, true), _ => {} }
        core.set_controllers([b.pack(), 0, 0, 0]);
        core.run_frame();
    }
    let bus = &core.system().bus;
    let r = |o: u32| rustyn64_core::cpu::Bus::read_u32_dbg(bus, 0x0440_0000 + o);
    let ctrl = r(0x00);
    let xs = r(0x30); let ys = r(0x34);
    println!("VI_CTRL   = {ctrl:#010x}");
    println!("  type/bpp   = {}", ctrl & 3);
    println!("  aa_mode    = {}  (3=REPLICATE/nearest, 0/1=coverage path)", (ctrl >> 8) & 3);
    println!("  divot      = {}", (ctrl >> 4) & 1);
    println!("  dither_flt = {}", (ctrl >> 16) & 1);
    println!("  gamma      = {}", (ctrl >> 3) & 1);
    println!("VI_X_SCALE = {xs:#010x}  x_add = {}  (1024 = 1 src px/out px)", xs & 0xFFF);
    println!("VI_Y_SCALE = {ys:#010x}  y_add = {}", ys & 0xFFF);
}
