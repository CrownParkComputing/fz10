//! Render the launcher's 3D game case offscreen, to check a game's artwork
//! lands on the right faces the right way up without opening a window.
//!
//! `case_shot OUT_DIR [W H]`
//!
//! Two cases are drawn: one for a title with no artwork at all (so the
//! generated cover, spine and platter are on show) and one dressed in the
//! sample sleeve and disc scans in assets/dvd-case/textures.
#[allow(dead_code)]
#[path = "../src/bin/retro3do-gui/fz1_3d.rs"]
mod fz1_3d;

#[allow(dead_code)]
#[path = "../src/bin/retro3do-gui/case_art.rs"]
mod case_art;

use std::path::PathBuf;
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
    let out = args.get(1).ok_or("usage: case_shot OUT_DIR [W H]")?.clone();
    let w: u32 = args.get(2).and_then(|a| a.parse().ok()).unwrap_or(900);
    let h: u32 = args.get(3).and_then(|a| a.parse().ok()).unwrap_or(700);
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

    let scene = vec![fz1_3d::load_case().ok_or("case.glb")?];
    let mut r = fz1_3d::Fz1Renderer::new(&state, &scene).ok_or("renderer")?;

    let textures = PathBuf::from("assets/dvd-case/textures");
    let games = [
        (
            "drawn",
            case_art::CaseArt {
                title: "Alone in the Dark".to_string(),
                discs: 2,
                ..Default::default()
            },
        ),
        (
            "sides",
            case_art::CaseArt {
                title: "Which Way Round".to_string(),
                front: Some(PathBuf::from("assets/cpc-mark.png")),
                back: Some(PathBuf::from("assets/fz1-poster.jpg")),
                discs: 1,
                ..Default::default()
            },
        ),
        (
            "scanned",
            case_art::CaseArt {
                title: "Sample Disc".to_string(),
                front: Some(textures.join("cover-front-sample.jpg")),
                back: Some(textures.join("cover-back-sample.jpg")),
                disc: Some(textures.join("disc-sample.jpg")),
                discs: 1,
                ..Default::default()
            },
        ),
    ];
    // name, yaw, pitch, how far the lid stands open.
    let views: &[(&str, f32, f32, f32)] = &[
        ("front", 0.0, 0.06, 0.0),
        ("three-quarter", 0.45, 0.22, 0.0),
        ("spine", std::f32::consts::FRAC_PI_2, 0.06, 0.0),
        ("spine-left", -std::f32::consts::FRAC_PI_2, 0.06, 0.0),
        ("back", std::f32::consts::PI, 0.06, 0.0),
        ("top", 0.0, 1.0, 0.0),
        ("open", 0.30, 0.30, 1.0),
        ("half-open", 0.30, 0.30, 0.5),
    ];

    for (game, art) in &games {
        let wrap = case_art::wrap(art);
        r.set_art(&state, fz1_3d::ArtSlot::Wrap, wrap.as_raw());
        // The disc in the case wears the same art as the machine's platter.
        let disc = case_art::disc(art);
        r.set_art(&state, fz1_3d::ArtSlot::Platter, disc.as_raw());
        for &(name, yaw, pitch, lid) in views {
            let anim = fz1_3d::Fz1State {
                case_yaw: yaw,
                case_pitch: pitch,
                case_zoom: if lid > 0.0 { 1.95 } else { 0.92 },
                lid,
                unbox: 1.0,
                ..Default::default()
            };
            r.render_into(&state, &scene, &anim, (w, h), 1, 0..1)
                .ok_or("render")?;
            let tex = r.target_texture(1).ok_or("target")?;
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
            let data = buf
                .slice(..)
                .get_mapped_range()
                .map_err(|e| format!("{e:?}"))?;
            let mut img = image::RgbImage::new(w, h);
            for y in 0..h {
                for x in 0..w {
                    let i = (y * row + x * 4) as usize;
                    let a = data[i + 3] as f32 / 255.0;
                    let px = |c: u8| (c as f32 * a + 70.0 * (1.0 - a)) as u8;
                    img.put_pixel(x, y, image::Rgb([px(data[i]), px(data[i + 1]), px(data[i + 2])]));
                }
            }
            drop(data);
            let path = format!("{out}/{game}-{name}.png");
            img.save(&path).map_err(|e| e.to_string())?;
            println!("{path}");
        }
    }
    Ok(())
}
