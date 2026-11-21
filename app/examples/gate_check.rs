//! The render-order rule, checked offscreen: a case or disc is never drawn
//! before its artwork is bound. It must be invisible while its slot is
//! empty or stale, and appear the frame the art lands.
//!
//! `gate_check` — prints PASS/FAIL lines, exits nonzero on any failure.
#[allow(dead_code)]
#[path = "../src/bin/retro3do-gui/fz1_3d.rs"]
mod fz1_3d;

use std::sync::Arc;
use std::task::{Context, Poll, Waker};

use eframe::egui_wgpu::{self, wgpu};

fn block<F: std::future::Future>(fut: F) -> F::Output {
    let mut fut = std::pin::pin!(fut);
    let mut cx = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(v) = fut.as_mut().poll(&mut cx) {
            return v;
        }
        std::thread::yield_now();
    }
}

fn has_part(r: &fz1_3d::Fz1Renderer, name: &str) -> bool {
    r.screen_points().iter().any(|(n, _, _)| n == name)
}

fn main() -> Result<(), String> {
    let instance = wgpu::Instance::default();
    let adapter = block(instance.request_adapter(&wgpu::RequestAdapterOptions::default()))
        .map_err(|e| e.to_string())?;
    let (device, queue) = block(adapter.request_device(&wgpu::DeviceDescriptor::default()))
        .map_err(|e| e.to_string())?;
    let renderer = egui_wgpu::Renderer::new(
        &device,
        wgpu::TextureFormat::Rgba8Unorm,
        egui_wgpu::RendererOptions::default(),
    );
    let state = egui_wgpu::RenderState {
        adapter: adapter.clone(),
        available_adapters: vec![adapter],
        instance,
        device: device.clone(),
        queue: queue.clone(),
        target_format: wgpu::TextureFormat::Rgba8Unorm,
        renderer: Arc::new(egui::mutex::RwLock::new(renderer)),
        surface_config: egui_wgpu::SurfaceConfig::LOW_LATENCY,
    };

    // The scene order the GUI uses: console, carton, case.
    let scene = vec![
        fz1_3d::load_model().ok_or("fz1.glb")?,
        fz1_3d::load_carton().ok_or("fz1-box.glb")?,
        fz1_3d::load_case().ok_or("case.glb")?,
    ];
    let mut r = fz1_3d::Fz1Renderer::new(&state, &scene).ok_or("renderer")?;
    let size = (900u32, 700u32);
    let anim = fz1_3d::Fz1State {
        unbox: 1.0,
        tray: 1.0,
        disc_shown: true,
        disc_drop: 1.0,
        ..Default::default()
    };

    let mut fails = 0;
    let mut check = |name: &str, ok: bool| {
        println!("{} {}", if ok { "PASS" } else { "FAIL" }, name);
        if !ok {
            fails += 1;
        }
    };

    // The disc: shown on the tray but nothing bound yet — it must not draw.
    r.render_into(&state, &scene, &anim, size, 0, 0..1);
    check("tray disc hidden until its art is bound", !has_part(&r, "Disc"));

    let (dw, dh) = fz1_3d::ArtSlot::Platter.size();
    let disc_art = vec![200u8; (dw * dh * 4) as usize];
    r.set_art(&state, fz1_3d::ArtSlot::Platter, &disc_art);
    r.render_into(&state, &scene, &anim, size, 0, 0..1);
    check("tray disc draws once its art is bound", has_part(&r, "Disc"));

    r.clear_art(fz1_3d::ArtSlot::Platter);
    r.render_into(&state, &scene, &anim, size, 0, 0..1);
    check("stale platter art hides the disc again", !has_part(&r, "Disc"));

    // The case: same rule — nothing on screen until the sleeve is bound.
    r.render_into(&state, &scene, &anim, size, 1, 2..3);
    check("case hidden until its wrap is bound", !has_part(&r, "Wrap"));

    let (ww, wh) = fz1_3d::ArtSlot::Wrap.size();
    let wrap_art = vec![200u8; (ww * wh * 4) as usize];
    r.set_art(&state, fz1_3d::ArtSlot::Wrap, &wrap_art);
    r.render_into(&state, &scene, &anim, size, 1, 2..3);
    check("case draws once its wrap is bound", has_part(&r, "Wrap"));

    r.clear_art(fz1_3d::ArtSlot::Wrap);
    r.render_into(&state, &scene, &anim, size, 1, 2..3);
    check("stale wrap art hides the case again", !has_part(&r, "Wrap"));

    if fails > 0 {
        return Err(format!("{fails} gate check(s) failed"));
    }
    Ok(())
}
