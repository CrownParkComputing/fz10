//! GPU-side presentation upscaling for the playing view.
//!
//! The core hands out a native frame (640x480). Uploading that to the GPU is
//! cheap; upscaling it on the CPU every frame (a 78 MB buffer at 8x) is what
//! made video stutter. This module keeps a small render pipeline that samples
//! the native frame with a Catmull-Rom bicubic and renders it into a texture
//! at the chosen preset resolution, which egui then draws like any image.
//! The preset applies the very next frame and costs the CPU almost nothing.

use eframe::egui_wgpu::{self, wgpu};

const SHADER: &str = r"
struct VsOut {
    @builtin(position) pos: vec4f,
    @location(0) uv: vec2f,
};

@vertex
fn vs(@builtin(vertex_index) i: u32) -> VsOut {
    var out: VsOut;
    let p = vec2f(f32((i << 1u) & 2u), f32(i & 2u));
    out.pos = vec4f(p * 2.0 - 1.0, 0.0, 1.0);
    out.uv = vec2f(p.x, 1.0 - p.y);
    return out;
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var smp: sampler;

struct Params {
    // x: unsharp-mask strength applied in source-pixel units, so the
    // sharpening survives the final draw's minification.
    v: vec4f,
};
@group(0) @binding(2) var<uniform> params: Params;

// Catmull-Rom cubic weights for the four taps around f.
fn weights(f: f32) -> vec4f {
    let f2 = f * f;
    let f3 = f2 * f;
    return vec4f(
        -0.5 * f3 + f2 - 0.5 * f,
        1.5 * f3 - 2.5 * f2 + 1.0,
        -1.5 * f3 + 2.0 * f2 + 0.5 * f,
        0.5 * f3 - 0.5 * f2,
    );
}

// Bicubic in four bilinear fetches: each pair of taps on an axis is combined
// into one hardware-linear sample at a weighted position.
@fragment
fn fs(in: VsOut) -> @location(0) vec4f {
    let size = vec2f(textureDimensions(src));
    let tc = in.uv * size - 0.5;
    let base = floor(tc);
    let f = tc - base;
    let wx = weights(f.x);
    let wy = weights(f.y);
    let gx = max(vec2f(wx.x + wx.y, wx.z + wx.w), vec2f(1e-6));
    let gy = max(vec2f(wy.x + wy.y, wy.z + wy.w), vec2f(1e-6));
    let ux = (base.x + vec2f(-0.5 + wx.y / gx.x, 1.5 + wx.w / gx.y)) / size.x;
    let uy = (base.y + vec2f(-0.5 + wy.y / gy.x, 1.5 + wy.w / gy.y)) / size.y;
    let c00 = textureSampleLevel(src, smp, vec2f(ux.x, uy.x), 0.0);
    let c10 = textureSampleLevel(src, smp, vec2f(ux.y, uy.x), 0.0);
    let c01 = textureSampleLevel(src, smp, vec2f(ux.x, uy.y), 0.0);
    let c11 = textureSampleLevel(src, smp, vec2f(ux.y, uy.y), 0.0);
    let cubic = (c00 * gx.x + c10 * gx.y) * gy.x + (c01 * gx.x + c11 * gx.y) * gy.y;
    // Bilinear at the same point is a ~1-source-pixel blur. v.x mixes around
    // the bicubic: negative values soften toward the blur, positive ones
    // sharpen past it, so every preset has a distinct look on any display.
    let blur = textureSampleLevel(src, smp, in.uv, 0.0);
    if abs(params.v.x) < 0.001 {
        return cubic;
    }
    return clamp(cubic + params.v.x * (cubic - blur), vec4f(0.0), vec4f(1.0));
}
";

struct Tex {
    _tex: wgpu::Texture,
    view: wgpu::TextureView,
    w: u32,
    h: u32,
}

pub struct GpuScaler {
    pipeline: wgpu::RenderPipeline,
    bgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    src: Option<Tex>,
    src_bind: Option<wgpu::BindGroup>,
    dst: Option<Tex>,
    dst_id: Option<egui::TextureId>,
    dst_filter: wgpu::FilterMode,
    params: wgpu::Buffer,
    last_sharpen: f32,
}

impl GpuScaler {
    pub fn new(device: &wgpu::Device) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("upscale"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("upscale-src"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("upscale"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("upscale"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("upscale-src"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("upscale-params"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            pipeline,
            bgl,
            sampler,
            src: None,
            src_bind: None,
            dst: None,
            dst_id: None,
            dst_filter: wgpu::FilterMode::Linear,
            params,
            last_sharpen: f32::NAN,
        }
    }

    /// Upload `rgba` (src_w*src_h*4), upscale it into the preset-size texture
    /// and return the egui texture id to draw this frame. `crisp` picks
    /// nearest sampling for the final egui draw (the "Original" preset).
    #[expect(clippy::too_many_arguments)]
    pub fn present(
        &mut self,
        state: &egui_wgpu::RenderState,
        rgba: &[u8],
        w: u32,
        h: u32,
        out_w: u32,
        out_h: u32,
        crisp: bool,
        sharpen: f32,
    ) -> Option<egui::TextureId> {
        let device = &state.device;
        if w == 0 || h == 0 || out_w == 0 || out_h == 0 {
            return None;
        }

        let src_new = self.src.as_ref().is_none_or(|t| t.w != w || t.h != h);
        if src_new {
            let tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("frame-src"),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = tex.create_view(&Default::default());
            self.src = Some(Tex {
                _tex: tex,
                view,
                w,
                h,
            });
            self.src_bind = None;
        }
        let src = self.src.as_ref()?;
        let need = w.saturating_mul(4) as u64;
        if (rgba.len() as u64) >= need.saturating_mul(h as u64) {
            state.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &src._tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                rgba,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(w.saturating_mul(4)),
                    rows_per_image: Some(h),
                },
                wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
            );
        } else {
            return None;
        }
        if self.src_bind.is_none() {
            self.src_bind = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("upscale-src"),
                layout: &self.bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&src.view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: self.params.as_entire_binding(),
                    },
                ],
            }));
        }
        if sharpen != self.last_sharpen {
            self.last_sharpen = sharpen;
            let v = [
                sharpen.to_ne_bytes(),
                0f32.to_ne_bytes(),
                0f32.to_ne_bytes(),
                0f32.to_ne_bytes(),
            ]
            .concat();
            state.queue.write_buffer(&self.params, 0, &v);
        }

        let dst_new = self
            .dst
            .as_ref()
            .is_none_or(|t| t.w != out_w || t.h != out_h);
        if dst_new {
            let tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("frame-dst"),
                size: wgpu::Extent3d {
                    width: out_w,
                    height: out_h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let view = tex.create_view(&Default::default());
            self.dst = Some(Tex {
                _tex: tex,
                view,
                w: out_w,
                h: out_h,
            });
        }
        let dst = self.dst.as_ref()?;
        let bind = self.src_bind.as_ref()?;

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("upscale"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("upscale"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &dst.view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, bind, &[]);
            pass.draw(0..3, 0..1);
        }
        state.queue.submit([encoder.finish()]);

        // Register (or re-register after a resize/filter change) the output
        // texture so egui can draw it like any other image.
        let filter = if crisp {
            wgpu::FilterMode::Nearest
        } else {
            wgpu::FilterMode::Linear
        };
        let mut renderer = state.renderer.write();
        match (self.dst_id, dst_new || filter != self.dst_filter) {
            (None, _) => {
                self.dst_id = Some(renderer.register_native_texture(device, &dst.view, filter));
            }
            (Some(id), true) => {
                renderer.update_egui_texture_from_wgpu_texture(device, &dst.view, filter, id);
            }
            (Some(_), false) => {}
        }
        self.dst_filter = filter;
        self.dst_id
    }
}
