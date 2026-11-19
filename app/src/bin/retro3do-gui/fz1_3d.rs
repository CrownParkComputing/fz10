//! The FZ-1 console as a real 3D model, rendered with wgpu inside egui.
//!
//! The model is assets/fz1.glb: a measured replica of the Panasonic FZ-1
//! (284x268x88 mm) from the "FZ-1 Turntable" interactive page. It carries
//! one animation, `TrayOpen`, sliding `TrayRoot` (the drawer and the disc
//! on it) forward, and the Disc material is UV-mapped so a square game
//! label lands on it exactly.
//!
//! Everything draws offscreen into one texture that egui then paints like
//! the game-frame path does.

use eframe::egui_wgpu::{self, wgpu};

// --- tiny column-major mat4 helpers (glTF convention) --------------------

type Mat4 = [[f32; 4]; 4];

fn mat_identity() -> Mat4 {
    [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

fn mat_mul(a: &Mat4, b: &Mat4) -> Mat4 {
    let mut out = [[0.0f32; 4]; 4];
    for c in 0..4 {
        for r in 0..4 {
            let mut s = 0.0f32;
            for k in 0..4 {
                let av = a.get(k).and_then(|col| col.get(r)).copied().unwrap_or(0.0);
                let bv = b.get(c).and_then(|col| col.get(k)).copied().unwrap_or(0.0);
                s += av * bv;
            }
            if let Some(cell) = out.get_mut(c).and_then(|col| col.get_mut(r)) {
                *cell = s;
            }
        }
    }
    out
}

fn mat_translate(t: [f32; 3]) -> Mat4 {
    [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [t[0], t[1], t[2], 1.0],
    ]
}

fn mat_rot_y(a: f32) -> Mat4 {
    let (s, c) = a.sin_cos();
    [
        [c, 0.0, -s, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [s, 0.0, c, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

/// Right-handed perspective for wgpu's clip space, where depth runs 0..1 —
/// an OpenGL projection (-1..1) puts half the model behind the near plane
/// and the picture comes out empty.
fn mat_perspective(fov_y: f32, aspect: f32, near: f32, far: f32) -> Mat4 {
    let f = 1.0 / (fov_y * 0.5).tan();
    let nf = 1.0 / (near - far);
    [
        [f / aspect, 0.0, 0.0, 0.0],
        [0.0, f, 0.0, 0.0],
        [0.0, 0.0, far * nf, -1.0],
        [0.0, 0.0, far * near * nf, 0.0],
    ]
}

fn mat_look_at(eye: [f32; 3], at: [f32; 3], up: [f32; 3]) -> Mat4 {
    let norm = |v: [f32; 3]| {
        let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt().max(1e-9);
        [v[0] / l, v[1] / l, v[2] / l]
    };
    let cross = |a: [f32; 3], b: [f32; 3]| {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    };
    let f = norm([at[0] - eye[0], at[1] - eye[1], at[2] - eye[2]]);
    let s = norm(cross(f, up));
    let u = cross(s, f);
    [
        [s[0], u[0], -f[0], 0.0],
        [s[1], u[1], -f[1], 0.0],
        [s[2], u[2], -f[2], 0.0],
        [
            -(s[0] * eye[0] + s[1] * eye[1] + s[2] * eye[2]),
            -(u[0] * eye[0] + u[1] * eye[1] + u[2] * eye[2]),
            f[0] * eye[0] + f[1] * eye[1] + f[2] * eye[2],
            1.0,
        ],
    ]
}

// --- parsed model ---------------------------------------------------------

#[derive(Clone)]
struct Part {
    name: String,
    /// Node-local TRS.
    local: Mat4,
    /// Parent's world transform (already includes TrayRoot's rest pose for
    /// tray children, so animation applies a delta on top).
    parent: Mat4,
    color: [f32; 4],
    emissive: [f32; 3],
    textured: bool,
    /// Mean of the part's own vertices, in node-local space: the point a
    /// click target sits on once it is projected.
    centre: [f32; 3],
    verts: Vec<f32>, // pos(3) normal(3) uv(2)
    indices: Vec<u32>,
}

pub struct Model {
    parts: Vec<Part>,
    /// Where this model sits in the scene: the shelf and the pad are placed
    /// around the console rather than at the origin.
    pub place: Mat4,
    /// Rest translation of each node that can move, by node name.
    tray_rest: Option<[f32; 3]>,
    disc_rest: Option<[f32; 3]>,
    /// TrayOpen translation keyframes: (time, position).
    tray_anim: Vec<(f32, [f32; 3])>,
    center: [f32; 3],
    radius: f32,
}

impl Model {
    /// The tray's translation at anim time `t` (clamped to the clip).
    fn tray_at(&self, t: f32) -> [f32; 3] {
        let anim = &self.tray_anim;
        let Some(first) = anim.first() else {
            return self.tray_rest.unwrap_or([0.0; 3]);
        };
        if t <= first.0 {
            return first.1;
        }
        let Some(last) = anim.last() else {
            return first.1;
        };
        if t >= last.0 {
            return last.1;
        }
        for w in anim.windows(2) {
            let (t0, p0) = w.first().copied().unwrap_or((0.0, [0.0; 3]));
            let (t1, p1) = w.get(1).copied().unwrap_or((t0, p0));
            if t >= t0 && t <= t1 && t1 > t0 {
                let f = (t - t0) / (t1 - t0);
                return [
                    p0[0] + (p1[0] - p0[0]) * f,
                    p0[1] + (p1[1] - p0[1]) * f,
                    p0[2] + (p1[2] - p0[2]) * f,
                ];
            }
        }
        last.1
    }
}

fn walk_node(
    node: &gltf::Node,
    parent: &Mat4,
    buffers: &[gltf::buffer::Data],
    model: &mut Model,
    lo: &mut [f32; 3],
    hi: &mut [f32; 3],
) {
    let local: Mat4 = node.transform().matrix();
    let world = mat_mul(parent, &local);
    let name = node.name().unwrap_or("").to_string();

    if name == "TrayRoot" {
        model.tray_rest = local.get(3).map(|c| [c[0], c[1], c[2]]);
    }
    if name == "Disc" {
        model.disc_rest = local.get(3).map(|c| [c[0], c[1], c[2]]);
    }

    if let Some(mesh) = node.mesh() {
        for prim in mesh.primitives() {
            let reader = prim.reader(|b| buffers.get(b.index()).map(|d| &d.0[..]));
            let positions: Vec<[f32; 3]> = reader
                .read_positions()
                .map(|p| p.collect())
                .unwrap_or_default();
            let normals: Vec<[f32; 3]> = reader
                .read_normals()
                .map(|n| n.collect())
                .unwrap_or_default();
            let uvs: Vec<[f32; 2]> = reader
                .read_tex_coords(0)
                .map(|t| t.into_f32().collect())
                .unwrap_or_default();
            let indices: Vec<u32> = reader
                .read_indices()
                .map(|i| i.into_u32().collect())
                .unwrap_or_default();
            if positions.is_empty() || indices.is_empty() {
                continue;
            }
            // Track world-space bounds for framing.
            for p in &positions {
                let wp = transform_point(&world, *p);
                for axis in 0..3 {
                    if let (Some(v), Some(l), Some(h)) =
                        (wp.get(axis), lo.get_mut(axis), hi.get_mut(axis))
                    {
                        *l = l.min(*v);
                        *h = h.max(*v);
                    }
                }
            }
            let mut verts = Vec::with_capacity(positions.len() * 8);
            for (i, p) in positions.iter().enumerate() {
                let n = normals.get(i).copied().unwrap_or([0.0, 1.0, 0.0]);
                let uv = uvs.get(i).copied().unwrap_or([0.0, 0.0]);
                verts.extend_from_slice(&[p[0], p[1], p[2], n[0], n[1], n[2], uv[0], uv[1]]);
            }
            let mat = prim.material();
            let pbr = mat.pbr_metallic_roughness();
            let mut centre = [0.0f32; 3];
            if !positions.is_empty() {
                for p in &positions {
                    for (sum, v) in centre.iter_mut().zip(p.iter()) {
                        *sum += v;
                    }
                }
                for sum in centre.iter_mut() {
                    *sum /= positions.len() as f32;
                }
            }
            model.parts.push(Part {
                name: name.clone(),
                local,
                parent: *parent,
                color: pbr.base_color_factor(),
                emissive: mat.emissive_factor(),
                textured: pbr.base_color_texture().is_some(),
                centre,
                verts,
                indices,
            });
        }
    }
    for child in node.children() {
        walk_node(&child, &world, buffers, model, lo, hi);
    }
}

/// A point through a matrix, keeping w: clip space, where the projection's
/// perspective divide has not happened yet.
fn transform_clip(m: &Mat4, p: [f32; 3]) -> [f32; 4] {
    let mut out = [0.0f32; 4];
    for r in 0..4 {
        let mut s = m.get(3).and_then(|c| c.get(r)).copied().unwrap_or(0.0);
        for c in 0..3 {
            s += m.get(c).and_then(|col| col.get(r)).copied().unwrap_or(0.0)
                * p.get(c).copied().unwrap_or(0.0);
        }
        if let Some(cell) = out.get_mut(r) {
            *cell = s;
        }
    }
    out
}

/// The uniform scale a placement matrix carries, so a model's own radius
/// can be measured in scene units.
fn scene_place_scale(m: &Mat4) -> f32 {
    let col = m.first().copied().unwrap_or([1.0, 0.0, 0.0, 0.0]);
    (col[0] * col[0] + col[1] * col[1] + col[2] * col[2])
        .sqrt()
        .max(1e-4)
}

fn transform_point(m: &Mat4, p: [f32; 3]) -> [f32; 3] {
    let mut out = [0.0f32; 3];
    for r in 0..3 {
        let mut s = m.get(3).and_then(|c| c.get(r)).copied().unwrap_or(0.0);
        for c in 0..3 {
            s += m.get(c).and_then(|col| col.get(r)).copied().unwrap_or(0.0)
                * p.get(c).copied().unwrap_or(0.0);
        }
        if let Some(cell) = out.get_mut(r) {
            *cell = s;
        }
    }
    out
}


pub fn load_model() -> Option<Model> {
    let bytes: &[u8] = include_bytes!("../../../assets/fz1.glb");
    load_glb(bytes)
}

fn load_glb(bytes: &[u8]) -> Option<Model> {
    let (doc, buffers, _images) = gltf::import_slice(bytes).ok()?;
    let mut model = Model {
        parts: Vec::new(),
        place: mat_identity(),
        tray_rest: None,
        disc_rest: None,
        tray_anim: Vec::new(),
        center: [0.0; 3],
        radius: 0.2,
    };
    for anim in doc.animations() {
        if !anim.name().is_some_and(|n| n == "TrayOpen") {
            continue;
        }
        for ch in anim.channels() {
            if !matches!(
                ch.target().property(),
                gltf::animation::Property::Translation
            ) {
                continue;
            }
            let reader = ch.reader(|b| buffers.get(b.index()).map(|d| &d.0[..]));
            let times: Vec<f32> = reader
                .read_inputs()
                .map(|i| i.collect())
                .unwrap_or_default();
            // Only the drawer's translation track is wanted; anything else
            // the file carries is left alone.
            let values: Vec<[f32; 3]> = reader
                .read_outputs()
                .map(|o| match o {
                    gltf::animation::util::ReadOutputs::Translations(v) => v.collect(),
                    _ => Vec::new(),
                })
                .unwrap_or_default();
            model.tray_anim = times.iter().copied().zip(values.iter().copied()).collect();
        }
    }
    let mut lo = [f32::MAX; 3];
    let mut hi = [f32::MIN; 3];
    let scene = doc.scenes().next()?;
    for root in scene.nodes() {
        walk_node(
            &root,
            &mat_identity(),
            &buffers,
            &mut model,
            &mut lo,
            &mut hi,
        );
    }
    for axis in 0..3 {
        if let (Some(l), Some(h), Some(c)) =
            (lo.get(axis), hi.get(axis), model.center.get_mut(axis))
        {
            *c = (l + h) * 0.5;
        }
    }
    let dx = hi[0] - lo[0];
    let dy = hi[1] - lo[1];
    let dz = hi[2] - lo[2];
    model.radius = ((dx * dx + dy * dy + dz * dz).sqrt() * 0.5).max(0.05);
    Some(model)
}

// --- gpu renderer ----------------------------------------------------------

const SHADER: &str = r"
struct Uniforms {
    mvp: mat4x4f,
    model: mat4x4f,
    color: vec4f,
    // x: textured flag, y: emissive gain, z,w: spare
    params: vec4f,
};
@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var tex0: texture_2d<f32>;
@group(0) @binding(2) var smp0: sampler;

struct VsOut {
    @builtin(position) pos: vec4f,
    @location(0) normal: vec3f,
    @location(1) uv: vec2f,
};

@vertex
fn vs(@location(0) pos: vec3f, @location(1) normal: vec3f, @location(2) uv: vec2f) -> VsOut {
    var out: VsOut;
    out.pos = u.mvp * vec4f(pos, 1.0);
    out.normal = (u.model * vec4f(normal, 0.0)).xyz;
    out.uv = uv;
    return out;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4f {
    let n = normalize(in.normal);
    let key = normalize(vec3f(0.5, 0.8, 0.55));
    let fill = normalize(vec3f(-0.7, 0.35, 0.35));
    let rim = normalize(vec3f(0.0, 0.15, -1.0));
    let light = max(dot(n, key), 0.0) * 1.35
        + max(dot(n, fill), 0.0) * 0.55
        + max(dot(n, rim), 0.0) * 0.4;
    var base = u.color;
    if u.params.x > 0.5 {
        base = textureSample(tex0, smp0, in.uv);
    }
    // Dark plastic needs a floor, or the whole unit reads as a black hole
    // in the page.
    // Lift it out of the dark: the real unit is near-black plastic, which
    // reads as a hole on a dark page, so the material gets a floor and a
    // little sheen on top of the lighting.
    let lit = base.rgb * (0.8 + light) + vec3f(0.1) * light;
    let rgb = lit + vec3f(u.params.y) * base.rgb;
    return vec4f(rgb, base.a);
}
";

struct GpuPart {
    vb: wgpu::Buffer,
    ib: wgpu::Buffer,
    count: u32,
    ubuf: wgpu::Buffer,
    bind: wgpu::BindGroup,
}

/// One picture the renderer draws into: the machine has one, each pad has
/// its own, so a pad is its own little view rather than part of the console's
/// scene.
#[derive(Default)]
struct Target {
    color: Option<(wgpu::Texture, wgpu::TextureView)>,
    depth: Option<(wgpu::Texture, wgpu::TextureView)>,
    tex_id: Option<egui::TextureId>,
    w: u32,
    h: u32,
}

pub struct Fz1Renderer {
    pipeline: wgpu::RenderPipeline,
    /// GPU buffers per model in the scene, in the order they were given.
    parts: Vec<Vec<GpuPart>>,
    targets: Vec<Target>,
    /// Part name, where its centre landed on the image (0..1), and its depth.
    points: Vec<(String, [f32; 2], f32)>,
    disc_tex: wgpu::Texture,
    disc_view: wgpu::TextureView,
    sampler: wgpu::Sampler,
}

/// Per-frame animation state driven by the GUI.
#[derive(Default)]
pub struct Fz1State {
    /// Orbit: radians around the machine and above it, and how far back the
    /// eye sits as a multiple of the model's radius.
    pub yaw: f32,
    pub pitch: f32,
    pub zoom: f32,
    /// 0 = drawer closed, 1 = fully open. The GUI eases it toward `goal`.
    pub tray: f32,
    pub tray_goal: f32,
    /// Radians the disc has spun.
    pub spin: f32,
    /// Which way a pad view is turned, when one is being shown.
    pub pad_yaw: f32,
    pub pad_pitch: f32,
    /// Power/ready lamps lit.
    pub lit: bool,
}

const DISC_TEX_SIZE: u32 = 1024;

impl Fz1Renderer {
    pub fn new(state: &egui_wgpu::RenderState, models: &[Model]) -> Option<Self> {
        let device = &state.device;
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("fz1"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("fz1"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("fz1"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("fz1"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: 32,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x3,
                            offset: 0,
                            shader_location: 0,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x3,
                            offset: 12,
                            shader_location: 1,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x2,
                            offset: 24,
                            shader_location: 2,
                        },
                    ],
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth24Plus,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let disc_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("fz1-disc"),
            size: wgpu::Extent3d {
                width: DISC_TEX_SIZE,
                height: DISC_TEX_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        // Clear it to a neutral silver until real label art arrives.
        state.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &disc_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &[0xb0u8, 0xb0, 0xb4, 0xff],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        let disc_view = disc_tex.create_view(&Default::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("fz1-disc"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        let mut scene = Vec::new();
        for model in models {
        let mut parts = Vec::new();
        for part in &model.parts {
            let vb = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("fz1-vb"),
                size: (part.verts.len() * 4) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            state.queue.write_buffer(&vb, 0, bytemuck_f32(&part.verts));
            let ib = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("fz1-ib"),
                size: (part.indices.len() * 4) as u64,
                usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            state
                .queue
                .write_buffer(&ib, 0, bytemuck_u32(&part.indices));
            let ubuf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("fz1-u"),
                size: 160, // mvp 64 + model 64 + color 16 + params 16
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("fz1"),
                layout: &bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: ubuf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&disc_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                ],
            });
            parts.push(GpuPart {
                vb,
                ib,
                count: part.indices.len() as u32,
                ubuf,
                bind,
            });
        }
        scene.push(parts);
        }

        Some(Self {
            pipeline,
            parts: scene,
            targets: Vec::new(),
            points: Vec::new(),
            disc_tex,
            disc_view,
            sampler,
        })
    }

    /// Upload a new label image for the disc (RGBA, will be centered on a
    /// DISC_TEX_SIZE canvas letterboxed to a square).
    /// The label fills the whole disc, so the image has to be exactly the
    /// disc texture's size: the UVs map 0..1 across it, and anything smaller
    /// written into a corner shows up as art on a quarter of the disc.
    pub const ART_SIZE: u32 = DISC_TEX_SIZE;

    pub fn set_disc_art(&mut self, state: &egui_wgpu::RenderState, rgba: &[u8], w: u32, h: u32) {
        if w != DISC_TEX_SIZE || h != DISC_TEX_SIZE {
            return;
        }
        state.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.disc_tex,
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
        let _ = &self.disc_view;
        let _ = &self.sampler;
    }

    /// Draw the console into an egui texture and return its id.
    /// Where each named part's centre lands on the rendered image, in
    /// 0..1 across it: what the GUI needs to put a click target on a button
    /// that is being looked at from any angle.
    pub fn screen_points(&self) -> &[(String, [f32; 2], f32)] {
        &self.points
    }

    /// Draw `which` of the scene's models into target `target`, framed on
    /// just those models.
    pub fn render_into(
        &mut self,
        state: &egui_wgpu::RenderState,
        models: &[Model],
        anim: &Fz1State,
        size: (u32, u32),
        target: usize,
        which: std::ops::Range<usize>,
    ) -> Option<egui::TextureId> {
        let (w, h) = size;
        while self.targets.len() <= target {
            self.targets.push(Target::default());
        }
        let models: Vec<&Model> = which.clone().filter_map(|i| models.get(i)).collect();
        if models.is_empty() {
            return None;
        }
        // The console leads: the camera frames it, and the rest of the scene
        // is placed around it.
        let model = models.first()?;
        let device = &state.device;
        if w == 0 || h == 0 || w > 4096 || h > 4096 {
            return None;
        }
        let slot = self.targets.get(target)?;
        if slot.w != w || slot.h != h || slot.color.is_none() {
            let color = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("fz1-out"),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
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
            let depth = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("fz1-depth"),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Depth24Plus,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            let color_view = color.create_view(&Default::default());
            let depth_view = depth.create_view(&Default::default());
            if let Some(slot) = self.targets.get_mut(target) {
                slot.color = Some((color, color_view));
                slot.depth = Some((depth, depth_view));
                slot.w = w;
                slot.h = h;
            }
        }
        let slot = self.targets.get(target)?;
        let (_, color_view) = slot.color.as_ref()?;
        let (_, depth_view) = slot.depth.as_ref()?;

        // Camera: orbiting the whole scene — the machine, its shelf and the
        // pads in front of it — so nothing is framed out.
        let mut lo = [f32::MAX; 3];
        let mut hi = [f32::MIN; 3];
        for scene_model in models.iter() {
            let placed = transform_point(&scene_model.place, scene_model.center);
            let reach = scene_model.radius * scene_place_scale(&scene_model.place);
            for axis in 0..3 {
                if let (Some(v), Some(l), Some(h)) =
                    (placed.get(axis), lo.get_mut(axis), hi.get_mut(axis))
                {
                    *l = l.min(*v - reach);
                    *h = h.max(*v + reach);
                }
            }
        }
        let mut c = [0.0f32; 3];
        for axis in 0..3 {
            if let (Some(l), Some(h), Some(slot)) = (lo.get(axis), hi.get(axis), c.get_mut(axis)) {
                *slot = (l + h) * 0.5;
            }
        }
        let r = (0..3)
            .filter_map(|axis| Some((hi.get(axis)? - lo.get(axis)?) * 0.5))
            .fold(0.05f32, f32::max);
        let _ = model;
        let own_view = target == 0;
        let pitch = if own_view {
            anim.pitch.clamp(-1.3, 1.3)
        } else {
            // Nearly overhead by default: a pad on the table reads face up.
            anim.pad_pitch.clamp(0.15, 1.5)
        };
        // Far enough back that the whole unit sits in the frame, and wider
        // still when the viewport is tall and narrow.
        let aspect = (w as f32 / h as f32).max(0.2);
        let zoom = if own_view {
            anim.zoom.clamp(0.45, 3.0).max(0.05)
        } else {
            0.95
        };
        // Far enough that the whole thing fits the view it is given, across
        // as well as down — a fudge factor crops a wide box. The drawer
        // slides toward the viewer, so the view opens out with it as well.
        let open = if own_view { anim.tray.clamp(0.0, 1.0) } else { 0.0 };
        let half = |axis: usize| {
            (hi.get(axis).copied().unwrap_or(0.0) - lo.get(axis).copied().unwrap_or(0.0)) * 0.5
        };
        let tan = (38f32.to_radians() * 0.5).tan().max(1e-3);
        let by_height = half(1) / tan;
        let by_width = half(0).max(half(2)) / (tan * aspect);
        let dist = (by_height.max(by_width) + half(2)) * 1.12 * zoom * (1.0 + 0.25 * open);
        let _ = r;
        let (sy, cy) = if own_view {
            anim.yaw.sin_cos()
        } else {
            // Square on: a pad sits across its view, the way it sits in two
            // hands.
            anim.pad_yaw.sin_cos()
        };
        let (sp, cp) = pitch.sin_cos();
        let eye = [
            c[0] + dist * cp * sy,
            c[1] + dist * sp,
            c[2] + dist * cp * cy,
        ];
        // Aim a little forward and down while the drawer is out, so the tray
        // and the disc on it sit in the middle of the picture.
        let aim = [c[0], c[1] - r * 0.1 * open, c[2] + r * 0.22 * open];
        let view = mat_look_at(eye, aim, [0.0, 1.0, 0.0]);
        let proj = mat_perspective(38f32.to_radians(), aspect, 0.01, 10.0);
        let vp = mat_mul(&proj, &view);

        // The tray slides along the animation channel.
        let tray_t = anim.tray.clamp(0.0, 1.0);
        let tray_pos = if model.tray_anim.is_empty() {
            // No clip: fake it by sliding forward in local Z.
            let rest = model.tray_rest.unwrap_or([0.0; 3]);
            [rest[0], rest[1], rest[2] + tray_t * 0.13]
        } else {
            let span = model
                .tray_anim
                .last()
                .map(|l| l.0)
                .unwrap_or(1.0)
                .max(0.001);
            model.tray_at(tray_t * span)
        };

        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("fz1") });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("fz1"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: color_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.46,
                            g: 0.47,
                            b: 0.52,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            for (gpu_parts, model) in self.parts.iter().skip(which.start).zip(models.iter()) {
            for (gpu, part) in gpu_parts.iter().zip(model.parts.iter()) {
                // Dynamic transforms: tray children slide with the drawer,
                // the disc additionally spins about its own local Z.
                let delta = [
                    tray_pos[0] - model.tray_rest.unwrap_or(tray_pos)[0],
                    tray_pos[1] - model.tray_rest.unwrap_or(tray_pos)[1],
                    tray_pos[2] - model.tray_rest.unwrap_or(tray_pos)[2],
                ];
                let slide = mat_translate(delta);
                let mut world =
                    mat_mul(&model.place, &mat_mul(&slide, &mat_mul(&part.parent, &part.local)));
                let is_tray_child = part.name == "FZ1_Tray" || part.name == "Disc";
                if !is_tray_child {
                    world = mat_mul(&model.place, &mat_mul(&part.parent, &part.local));
                }
                if part.name == "Disc" {
                    world = mat_mul(&world, &mat_rot_y(anim.spin));
                }
                let mvp = mat_mul(&vp, &world);
                let emissive = if anim.lit { 1.5 } else { 0.0 };
                let params = [
                    if part.textured { 1.0 } else { 0.0 },
                    if part.emissive.iter().any(|e| *e > 0.01) {
                        emissive
                    } else {
                        0.0
                    },
                    0.0,
                    0.0,
                ];
                let mut udata = Vec::with_capacity(160);
                udata.extend_from_slice(bytemuck_f32(mvp.as_flattened()));
                udata.extend_from_slice(bytemuck_f32(world.as_flattened()));
                udata.extend_from_slice(bytemuck_f32(&part.color));
                udata.extend_from_slice(bytemuck_f32(&params));
                state.queue.write_buffer(&gpu.ubuf, 0, &udata);
                pass.set_vertex_buffer(0, gpu.vb.slice(..));
                pass.set_index_buffer(gpu.ib.slice(..), wgpu::IndexFormat::Uint32);
                pass.set_bind_group(0, &gpu.bind, &[]);
                pass.draw_indexed(0..gpu.count, 0, 0..1);
            }
            }
        }
        // Remember where each part landed, for the caller's click targets.
        self.points.clear();
        for model in models.iter() {
        for part in &model.parts {
            let world = mat_mul(&model.place, &mat_mul(&part.parent, &part.local));
            let centre = transform_point(&world, part.centre);
            let clip = transform_clip(&vp, centre);
            if clip[3] <= 0.0 {
                continue;
            }
            let ndc = [clip[0] / clip[3], clip[1] / clip[3], clip[2] / clip[3]];
            self.points.push((
                part.name.clone(),
                [(ndc[0] + 1.0) * 0.5, (1.0 - ndc[1]) * 0.5],
                ndc[2],
            ));
        }
        }
        state.queue.submit([encoder.finish()]);

        let mut renderer = state.renderer.write();
        let existing = self.targets.get(target).and_then(|t| t.tex_id);
        match existing {
            Some(id) => {
                renderer.update_egui_texture_from_wgpu_texture(
                    device,
                    color_view,
                    wgpu::FilterMode::Linear,
                    id,
                );
                Some(id)
            }
            None => {
                let id =
                    renderer.register_native_texture(device, color_view, wgpu::FilterMode::Linear);
                if let Some(slot) = self.targets.get_mut(target) {
                    slot.tex_id = Some(id);
                }
                Some(id)
            }
        }
    }
}

fn bytemuck_f32(v: &[f32]) -> &[u8] {
    bytemuck::cast_slice(v)
}

fn bytemuck_u32(v: &[u32]) -> &[u8] {
    bytemuck::cast_slice(v)
}
