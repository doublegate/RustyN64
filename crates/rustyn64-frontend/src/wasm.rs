//! The wasm browser entry point — a canvas demo.
//!
//! Runs the emulator in the browser and blits the VI scan-out to a 2D `<canvas>`
//! through `web-sys`, on a `requestAnimationFrame` loop. This is the browser
//! entry point the Phase 6 plan calls for: a `wasm-bindgen` dependency, a
//! `#[wasm_bindgen(start)]` entry, an `index.html`, and a `trunk build` that
//! produces a working demo.
//!
//! Deliberately **not** the full winit/wgpu/egui shell: that needs async wgpu
//! init on wasm and is the roadmap for the in-browser shell. The 2D-canvas path
//! is robust and proves the emulator runs and renders in a browser.

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::prelude::*;

use crate::emu::EmuCore;

/// A committed, license-clean homebrew ROM (our own MIPS assembly) that CPU-fills
/// a framebuffer and programs the VI, so the demo shows a real rendered frame.
const DEMO_ROM: &[u8] = include_bytes!("../../../tests/roms/homebrew/render_fill.z64");

/// The self-rescheduling `requestAnimationFrame` closure, kept alive by an `Rc`.
type RafClosure = Rc<RefCell<Option<Closure<dyn FnMut()>>>>;

/// The browser entry point (wasm `start`): set up the canvas, boot the demo ROM,
/// and drive one emulated frame per animation frame, blitting the scan-out.
///
/// # Errors
/// Returns a [`JsValue`] if the page has no `#rustyn64-canvas` element or no 2D
/// context (a malformed host page).
#[wasm_bindgen(start)]
pub fn start() -> Result<(), JsValue> {
    console_error_panic_hook::set_once();

    let window = web_sys::window().ok_or("no window")?;
    let document = window.document().ok_or("no document")?;
    let canvas = document
        .get_element_by_id("rustyn64-canvas")
        .ok_or("missing <canvas id=\"rustyn64-canvas\">")?
        .dyn_into::<web_sys::HtmlCanvasElement>()?;
    let ctx = canvas
        .get_context("2d")?
        .ok_or("no 2d context")?
        .dyn_into::<web_sys::CanvasRenderingContext2d>()?;

    let mut emu = EmuCore::new(0);
    emu.load_bare_metal_demo(DEMO_ROM);
    let emu = Rc::new(RefCell::new(emu));

    // The classic wasm-bindgen requestAnimationFrame loop: a closure that
    // reschedules itself, held alive through an `Rc<RefCell<Option<..>>>`.
    let raf: RafClosure = Rc::new(RefCell::new(None));
    let raf_kick = raf.clone();
    let win = window.clone();
    *raf_kick.borrow_mut() = Some(Closure::wrap(Box::new(move || {
        {
            let mut emu = emu.borrow_mut();
            emu.run_frame();
            let (rgba, w, h) = emu.frame_rgba();
            if w > 0 && h > 0 {
                if canvas.width() != w {
                    canvas.set_width(w);
                }
                if canvas.height() != h {
                    canvas.set_height(h);
                }
                blit(&ctx, rgba, w, h);
            }
        }
        request_frame(&win, raf.borrow().as_ref());
    }) as Box<dyn FnMut()>));

    request_frame(&window, raf_kick.borrow().as_ref());
    Ok(())
}

/// Upload `rgba` (the VI scan-out) to the 2D canvas as a `w`×`h` image.
///
/// `ImageData` requires **exactly** `4 * w * h` bytes. The scan-out buffer is the
/// max-size backing store (`EmuCore` guarantees `4 * w * h <= rgba.len()`), so we
/// blit the exact `4 * w * h` prefix — and skip the frame (rather than pass an
/// undersized slice, which the engine rejects with `IndexSizeError`) if that
/// invariant is ever violated. DOM failures are logged, never swallowed.
fn blit(ctx: &web_sys::CanvasRenderingContext2d, rgba: &[u8], w: u32, h: u32) {
    let len = (w * h * 4) as usize;
    if rgba.len() < len {
        return;
    }
    match web_sys::ImageData::new_with_u8_clamped_array_and_sh(
        wasm_bindgen::Clamped(&rgba[..len]),
        w,
        h,
    ) {
        Ok(img) => {
            if let Err(e) = ctx.put_image_data(&img, 0.0, 0.0) {
                web_sys::console::error_1(&e);
            }
        }
        Err(e) => web_sys::console::error_1(&e),
    }
}

/// Schedule `f` for the next animation frame (a no-op if the closure is gone).
fn request_frame(window: &web_sys::Window, f: Option<&Closure<dyn FnMut()>>) {
    if let Some(f) = f
        && let Err(e) = window.request_animation_frame(f.as_ref().unchecked_ref())
    {
        web_sys::console::error_1(&e);
    }
}
