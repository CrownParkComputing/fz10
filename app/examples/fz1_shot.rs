//! Render the GUI's 3D FZ-1 (and its carton) offscreen, to check the model
//! against the reference photographs without opening a window.
//!
//! `fz1_shot OUT_DIR [W H]`
//!
//! Writes front, three-quarter, top, back, side, underside and drawer-open
//! views of the console, and the carton opening at a few points of the intro.
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

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    let out = args.get(1).ok_or("usage: fz1_shot OUT_DIR [W H]")?.clone();
    let w: u32 = args.get(2).and_then(|a| a.parse().ok()).unwrap_or(1200);
    let h: u32 = args.get(3).and_then(|a| a.parse().ok()).unwrap_or(800);
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;

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

    let mut scene = vec![fz1_3d::load_model().ok_or("fz1.glb")?];
    scene.push(fz1_3d::load_carton().ok_or("fz1-box.glb")?);
    let mut r = fz1_3d::Fz1Renderer::new(&state, &scene).ok_or("renderer")?;
    // A directional label on the disc — a red half on the left, a green
    // half on the right, a white band across the top — so a mirrored or
    // flipped UV map cannot hide in the render.
    let size = fz1_3d::ArtSlot::Platter.size().0;
    let mut label = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let px = if y < size / 8 {
                [235, 235, 235, 255]
            } else if x < size / 2 {
                [200, 60, 60, 255]
            } else {
                [60, 170, 80, 255]
            };
            label.extend_from_slice(&px);
        }
    }
    r.set_art(&state, fz1_3d::ArtSlot::Platter, &label);

    let views: &[(&str, f32, f32, f32, f32, bool, f32)] = &[
        // name, yaw, pitch, tray, intro, lit, zoom
        ("front", 0.0, 0.08, 0.0, 1.0, true, 1.0),
        ("front-top", 0.0, 0.62, 0.0, 1.0, true, 1.0),
        ("three-quarter", 0.42, 0.38, 0.0, 1.0, true, 1.0),
        ("top", 0.0, 1.29, 0.0, 1.0, false, 1.0),
        ("back", std::f32::consts::PI, 0.25, 0.0, 1.0, false, 1.0),
        ("back-quarter", 2.5, 0.35, 0.0, 1.0, false, 1.0),
        ("side", std::f32::consts::FRAC_PI_2, 0.12, 0.0, 1.0, false, 1.0),
        ("under", 0.3, -1.1, 0.0, 1.0, false, 1.0),
        ("open", 0.35, 0.45, 1.0, 1.0, true, 1.0),
        // The drawer has to come fully clear of the case, so a CD can be
        // dropped onto it flat from straight above.
        ("open-top", 0.0, 1.3, 1.0, 1.0, true, 1.0),
        ("open-side", std::f32::consts::FRAC_PI_2, 0.06, 1.0, 1.0, true, 1.0),
        ("buttons", 0.0, 0.05, 0.0, 1.0, true, 0.55),
        ("intro-00", 0.42, 0.38, 0.0, 0.0, false, 1.0),
        ("intro-30", 0.42, 0.38, 0.0, 0.30, false, 1.0),
        ("intro-45", 0.42, 0.38, 0.0, 0.45, false, 1.0),
        ("intro-65", 0.42, 0.38, 0.0, 0.65, false, 1.0),
        ("intro-70", 0.42, 0.38, 1.0, 0.70, false, 1.0),
        ("intro-78", 0.42, 0.38, 1.0, 0.78, false, 1.0),
        ("intro-80", 0.42, 0.38, 0.0, 0.80, false, 1.0),
        ("intro-85", 0.42, 0.38, 0.0, 0.85, false, 1.0),
        ("intro-90", 0.42, 0.38, 0.0, 0.90, false, 1.0),
        ("intro-95", 0.42, 0.38, 0.0, 0.95, false, 1.0),
        // Looking down on the machine as it rides up on its rear panel: the
        // drawer's face must be the one pointing at the sky.
        ("intro-rise-top", 0.0, 1.15, 0.0, 0.72, false, 1.0),
        ("intro-back", 3.0, 0.4, 0.0, 0.0, false, 1.0),
        ("intro-top", 0.0, 1.25, 0.0, 0.0, false, 1.0),
    ];
    for &(name, yaw, pitch, tray, intro, lit, zoom) in views {
        let anim = fz1_3d::Fz1State {
            yaw,
            pitch,
            zoom,
            tray,
            tray_goal: tray,
            unbox: intro,
            lit,
            access: if lit { 1.0 } else { 0.0 },
            disc_shown: true,
            disc_drop: 1.0,
            ..Default::default()
        };
        let which = if intro < 1.0 { 0..2 } else { 0..1 };
        r.render_into(&state, &scene, &anim, (w, h), 0, which)
            .ok_or("render")?;
        let tex = r.target_texture(0).ok_or("target")?;
        let row = (w * 4).div_ceil(256) * 256;
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: (row * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            tex.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        queue.submit([enc.finish()]);
        buf.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| e.to_string())?;
        let data = buf.slice(..).get_mapped_range().map_err(|e| format!("{e:?}"))?;
        // Over a mid grey, the way the GUI's page sits behind it.
        let mut img = image::RgbImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let i = (y * row + x * 4) as usize;
                let a = data[i + 3] as f32 / 255.0;
                let bg = 70.0;
                let px = |c: u8| (c as f32 * a + bg * (1.0 - a)) as u8;
                img.put_pixel(x, y, image::Rgb([px(data[i]), px(data[i + 1]), px(data[i + 2])]));
            }
        }
        drop(data);
        let path = format!("{out}/{name}.png");
        img.save(&path).map_err(|e| e.to_string())?;
        println!("{path}");
    }
    Ok(())
}
