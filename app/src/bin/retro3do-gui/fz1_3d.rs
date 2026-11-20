//! The launcher's 3D scene, rendered with wgpu inside egui: the FZ-1, the
//! retail carton it comes out of, and a game's case.
//!
//! The models are generated (docs/make-fz1-model.py, docs/make-case-model.py):
//!
//! * assets/fz1.glb — a measured replica of the Panasonic FZ-1 (284x268x88
//!   mm). It carries one animation, `TrayOpen`, sliding `TrayRoot` (the
//!   drawer and the disc on it) forward, and the Disc material is UV-mapped
//!   so a square picture lands on the platter exactly.
//! * assets/fz1-box.glb — the carton, with a hinged `BoxLid` and two dust
//!   flaps, which `carton_pose` opens and closes.
//! * assets/dvd-case/case.glb — the game case, its `Wrap` UV-mapped as one
//!   printed sleeve (see case_art.rs).
//!
//! Baked artwork in the files is uploaded once; the three pictures that
//! change with the game (platter, sleeve, case disc) are slots the GUI fills.
//! Everything draws offscreen into a texture that egui then paints.

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

fn mat_rot_x(a: f32) -> Mat4 {
    let (s, c) = a.sin_cos();
    [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, c, s, 0.0],
        [0.0, -s, c, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

fn mat_rot_z(a: f32) -> Mat4 {
    let (s, c) = a.sin_cos();
    [
        [c, s, 0.0, 0.0],
        [-s, c, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
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
    /// The material's name: the parts whose picture changes with the game
    /// are found by it.
    material: String,
    /// The model's own baked image this part wears, if any.
    image: Option<usize>,
    /// Mean of the part's own vertices, in node-local space: the point a
    /// click target sits on once it is projected.
    centre: [f32; 3],
    verts: Vec<f32>, // pos(3) normal(3) uv(2)
    indices: Vec<u32>,
}

/// What a model is, which decides how it moves.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Console,
    Carton,
    Case,
}

pub struct Model {
    pub kind: Kind,
    parts: Vec<Part>,
    /// Baked images, RGBA8: (width, height, pixels).
    images: Vec<(u32, u32, Vec<u8>)>,
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
    /// The model's own box, before `place`.
    lo: [f32; 3],
    hi: [f32; 3],
}

impl Model {
    /// Work the framing box out again from the parts that are left, after
    /// some have been dropped.
    fn refit(&mut self) {
        let mut lo = [f32::MAX; 3];
        let mut hi = [f32::MIN; 3];
        for part in &self.parts {
            let world = mat_mul(&part.parent, &part.local);
            for v in part.verts.as_chunks::<8>().0 {
                let wp = transform_point(&world, [v[0], v[1], v[2]]);
                for ((l, h), p) in lo.iter_mut().zip(hi.iter_mut()).zip(wp) {
                    *l = l.min(p);
                    *h = h.max(p);
                }
            }
        }
        if lo.iter().zip(hi).any(|(l, h)| *l > h) {
            return;
        }
        for ((c, l), h) in self.center.iter_mut().zip(lo).zip(hi) {
            *c = (l + h) * 0.5;
        }
        let span: f32 = lo.iter().zip(hi).map(|(l, h)| (h - l) * (h - l)).sum();
        self.radius = (span.sqrt() * 0.5).max(0.05);
        self.lo = lo;
        self.hi = hi;
    }

    #[allow(dead_code)]
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
                material: mat.name().unwrap_or("").to_string(),
                image: pbr
                    .base_color_texture()
                    .map(|t| t.texture().source().index()),
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
    load_glb(include_bytes!("../../../assets/fz1.glb"), Kind::Console)
}

/// The retail carton the machine is unboxed from.
pub fn load_carton() -> Option<Model> {
    load_glb(include_bytes!("../../../assets/fz1-box.glb"), Kind::Carton)
}

/// The game case. In the file it lies flat (height along +X, width along
/// +Z, front cover facing +Y); stood up it faces the camera. The file also
/// carries a disc beside the box, which is dropped here: the preview shows
/// the sleeve alone, and a disc is only ever seen going into the drive.
pub fn load_case() -> Option<Model> {
    let mut m = load_glb(include_bytes!("../../../assets/dvd-case/case.glb"), Kind::Case)?;
    m.parts
        .retain(|p| !matches!(p.name.as_str(), "CaseDisc" | "DiscHub" | "DiscRing"));
    m.refit();
    m.place = mat_mul(&mat_rot_z(std::f32::consts::FRAC_PI_2), &mat_rot_x(std::f32::consts::FRAC_PI_2));
    Some(m)
}

fn rgba8(img: &gltf::image::Data) -> Option<(u32, u32, Vec<u8>)> {
    use gltf::image::Format;
    let px = match img.format {
        Format::R8G8B8A8 => img.pixels.clone(),
        Format::R8G8B8 => img
            .pixels
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|[r, g, b]| [*r, *g, *b, 255])
            .collect(),
        _ => return None,
    };
    Some((img.width, img.height, px))
}

fn load_glb(bytes: &[u8], kind: Kind) -> Option<Model> {
    let (doc, buffers, images) = gltf::import_slice(bytes).ok()?;
    let mut model = Model {
        kind,
        images: images
            .iter()
            .map(|i| rgba8(i).unwrap_or((1, 1, vec![255; 4])))
            .collect(),
        parts: Vec::new(),
        place: mat_identity(),
        tray_rest: None,
        disc_rest: None,
        tray_anim: Vec::new(),
        center: [0.0; 3],
        radius: 0.2,
        lo: [0.0; 3],
        hi: [0.0; 3],
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
    model.lo = lo;
    model.hi = hi;
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
    var lit = base.rgb * (0.8 + light) + vec3f(0.1) * light;
    if u.params.z > 0.5 {
        // Printed card and a pressed disc are not dark plastic: plain
        // lighting, so the artwork keeps its own colours.
        lit = base.rgb * (0.35 + 0.65 * light);
    }
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

/// One picture the renderer draws into: the machine's view and the case
/// view each have their own.
/// How far the drawer runs out, in metres. The platter's rear edge sits
/// 32 mm behind the model's origin and the console's front face is 112 mm in
/// front of it, so the disc only comes fully clear of the case after 144 mm
/// of travel - then a CD can be dropped onto the tray flat from above.
pub const TRAY_TRAVEL: f32 = 0.150;

#[derive(Default)]
struct Target {
    color: Option<(wgpu::Texture, wgpu::TextureView)>,
    /// The multisampled picture the pass draws into, resolved into `color`.
    msaa: Option<wgpu::TextureView>,
    depth: Option<(wgpu::Texture, wgpu::TextureView)>,
    tex_id: Option<egui::TextureId>,
    w: u32,
    h: u32,
}

/// The pictures that change with the game.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ArtSlot {
    /// What the disc on the FZ-1's tray wears: the game's own disc art.
    Platter,
    /// The case's printed sleeve: back, spine and front on one sheet.
    Wrap,
}

impl ArtSlot {
    /// The canvas size the slot takes, which the art has to match exactly:
    /// the UVs map 0..1 across it.
    pub const fn size(self) -> (u32, u32) {
        match self {
            ArtSlot::Platter => (DISC_TEX_SIZE, DISC_TEX_SIZE),
            ArtSlot::Wrap => (WRAP_TEX_W, WRAP_TEX_H),
        }
    }

    fn of_material(name: &str) -> Option<Self> {
        match name {
            "Disc" => Some(ArtSlot::Platter),
            "Wrap" => Some(ArtSlot::Wrap),
            _ => None,
        }
    }
}

pub struct Fz1Renderer {
    pipeline: wgpu::RenderPipeline,
    /// GPU buffers per model in the scene, in the order they were given.
    parts: Vec<Vec<GpuPart>>,
    targets: Vec<Target>,
    /// Part name, where its centre landed on the image (0..1), and its depth.
    points: Vec<(String, [f32; 2], f32)>,
    /// The game-dependent pictures, by `ArtSlot` order, and every baked
    /// image the models carry (kept alive for their bind groups).
    slots: Vec<wgpu::Texture>,
    _baked: Vec<wgpu::Texture>,
}

/// Per-frame animation state driven by the GUI.
#[derive(Default)]
pub struct Fz1State {
    /// Orbit: radians around the machine and above it, and how far back the
    /// eye sits as a multiple of what fits the frame.
    pub yaw: f32,
    pub pitch: f32,
    pub zoom: f32,
    /// 0 = drawer closed, 1 = fully open. The GUI eases it toward `goal`.
    pub tray: f32,
    pub tray_goal: f32,
    /// Radians the disc has spun.
    pub spin: f32,
    /// The case view's own orbit and distance.
    pub case_yaw: f32,
    pub case_pitch: f32,
    pub case_zoom: f32,
    /// Whether there is a disc on the platter at all. A disc is something the
    /// launcher puts in once a game has been picked, so the idle machine has
    /// an empty tray rather than a blank disc prop sitting in it.
    pub disc_shown: bool,
    /// How far a disc being put in has come down onto the spindle, 0 to 1.
    pub disc_drop: f32,
    /// Power/ready lamp lit.
    pub lit: bool,
    /// The ACCESS lamp's level, 0..1.
    pub access: f32,
    /// How far the machine has come out of its carton: 0 sealed, 1 out on
    /// the table with the carton gone. Played backwards when the app closes.
    pub unbox: f32,
}

const DISC_TEX_SIZE: u32 = 1024;
/// Edges are antialiased 4x: the machine's long straight edges stair-step
/// badly without it.
const MSAA: u32 = 4;
/// The sleeve sheet: 284 x 190 mm.
pub const WRAP_TEX_W: u32 = 2048;
pub const WRAP_TEX_H: u32 = 1370;

fn smooth(a: f32, b: f32, u: f32) -> f32 {
    let t = ((u - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Where everything stands `u` of the way through the unboxing.
#[derive(Clone, Copy, Debug)]
pub struct CartonPose {
    /// Lid and dust flaps, radians open.
    pub lid: f32,
    pub flaps: f32,
    /// How high the machine and the carton sit, metres.
    pub console_y: f32,
    pub carton_y: f32,
    /// The machine's pitch about its own middle: -90 degrees while it stands
    /// on its rear panel the way it is packed, 0 once it has been laid flat.
    pub tilt: f32,
    /// How much of the framing is still the carton's (1) rather than the
    /// machine's alone (0).
    pub frame: f32,
}

/// The unboxing, as one curve: the sealed carton holds for a moment, the lid
/// swings back, the dust flaps fold out, the machine rises straight up out of
/// its foam still standing on its rear panel with the drawer's face to the
/// sky, clears the rim, swings down onto its feet and settles on the table as
/// the carton drops away.
pub fn carton_pose(u: f32) -> CartonPose {
    let u = u.clamp(0.0, 1.0);
    // Straight up first, then the turn, then down onto the table: the phases
    // barely overlap so each one reads on its own.
    let rise = smooth(0.56, 0.75, u);
    let turn = smooth(0.74, 0.90, u);
    let settle = smooth(0.88, 1.0, u);
    CartonPose {
        // The lid hinges along the top of the back wall and stands up and
        // back; the dust flaps fold straight out, flat.
        lid: smooth(0.15, 0.42, u) * 160f32.to_radians(),
        flaps: smooth(0.38, 0.56, u) * 180f32.to_radians(),
        // The carton stands 290 mm tall and the machine is 272 mm from its
        // rear edge to its drawer's face, so on edge it only just fits: it
        // has to come most of its own height up again to clear the rim.
        console_y: (NEST_Y + (CLEAR_Y - NEST_Y) * rise) * (1.0 - settle),
        carton_y: -0.9 * smooth(0.80, 0.95, u),
        tilt: -(1.0 - turn) * std::f32::consts::FRAC_PI_2,
        frame: 1.0 - smooth(0.74, 0.98, u),
    }
}

/// The machine's own middle, measured up from its feet: it pitches about this
/// point, so the point is what `console_y` places. Half the shell's 86.5 mm,
/// which also centres the machine's height across the carton's depth when it
/// stands on its rear edge.
const PIVOT_Y: f32 = 0.0433;
/// Where `console_y` sits while the machine is nested in the carton, standing
/// on its rear edge on the 10 mm foam bed. On edge the machine's rear pods
/// reach 134 mm below that point and its drawer's buttons stand 135.8 mm
/// above it, so this rests the pods on the bed — the cable stub's extra 2 mm
/// denting the foam — with the drawer's face 8 mm under the lid and 10 mm
/// under the carton's 290 mm rim.
const NEST_Y: f32 = 0.1007;
/// And where it sits once the machine is clear of the 290 mm rim: its corner
/// radius on edge is 143 mm, so the pivot has to pass 433 mm for the swing
/// down to miss the walls.
const CLEAR_Y: f32 = 0.42;

type Bounds = ([f32; 3], [f32; 3]);

fn bounds_of(models: &[(&Model, Mat4)]) -> Bounds {
    let mut lo = [f32::MAX; 3];
    let mut hi = [f32::MIN; 3];
    for (m, at) in models {
        let place = mat_mul(at, &m.place);
        for corner in 0..8 {
            let pick = |bit: usize, axis: usize| {
                let src = if corner >> bit & 1 == 0 { &m.lo } else { &m.hi };
                src.get(axis).copied().unwrap_or(0.0)
            };
            let p = transform_point(&place, [pick(0, 0), pick(1, 1), pick(2, 2)]);
            for ((l, h), v) in lo.iter_mut().zip(hi.iter_mut()).zip(p) {
                *l = l.min(v);
                *h = h.max(v);
            }
        }
    }
    (lo, hi)
}

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
            multisample: wgpu::MultisampleState {
                count: MSAA,
                ..Default::default()
            },
            multiview_mask: None,
            cache: None,
        });

        let upload = |label: &str, w: u32, h: u32, rgba: &[u8]| {
            let tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
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
            if rgba.len() as u64 == u64::from(w) * u64::from(h) * 4 {
                write_rgba(&state.queue, &tex, rgba, w, h);
            }
            tex
        };
        // Game pictures start as neutral grey until the GUI fills them.
        let slots: Vec<wgpu::Texture> = [ArtSlot::Platter, ArtSlot::Wrap]
            .iter()
            .map(|s| {
                let (w, h) = s.size();
                let grey: Vec<u8> = std::iter::repeat_n([120u8, 120, 126, 255], (w * h) as usize)
                    .flatten()
                    .collect();
                upload("fz1-slot", w, h, &grey)
            })
            .collect();
        let white = upload("fz1-white", 1, 1, &[255, 255, 255, 255]);
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("fz1"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            ..Default::default()
        });

        let mut baked = vec![white];
        let mut scene = Vec::new();
        for model in models {
            let first = baked.len();
            for (w, h, px) in &model.images {
                baked.push(upload("fz1-baked", *w, *h, px));
            }
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
                state.queue.write_buffer(&ib, 0, bytemuck_u32(&part.indices));
                let ubuf = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("fz1-u"),
                    size: 160, // mvp 64 + model 64 + color 16 + params 16
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                let tex = match ArtSlot::of_material(&part.material) {
                    Some(slot) => slots.get(slot as usize),
                    None => part
                        .image
                        .and_then(|i| baked.get(first + i))
                        .or_else(|| baked.first()),
                };
                let view = tex?.create_view(&Default::default());
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
                            resource: wgpu::BindingResource::TextureView(&view),
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
            slots,
            _baked: baked,
        })
    }

    /// Put a game's picture in a slot. It has to be exactly the slot's size.
    pub fn set_art(&mut self, state: &egui_wgpu::RenderState, slot: ArtSlot, rgba: &[u8]) {
        let (w, h) = slot.size();
        if rgba.len() as u64 != u64::from(w) * u64::from(h) * 4 {
            return;
        }
        if let Some(tex) = self.slots.get(slot as usize) {
            write_rgba(&state.queue, tex, rgba, w, h);
        }
    }

    /// Where each named part's centre lands on the rendered image, in 0..1
    /// across it: what the GUI needs to put a click target on a button that
    /// is being looked at from any angle.
    pub fn screen_points(&self) -> &[(String, [f32; 2], f32)] {
        &self.points
    }

    /// Draw `which` of the scene's models into target `target`. Target 0 is
    /// the machine's view, framed on the machine (and on the carton while it
    /// is there); any other target is the case view.
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
        let first = which.start;
        let models: Vec<&Model> = which.filter_map(|i| models.get(i)).collect();
        let model = *models.first()?;
        let device = &state.device;
        if w == 0 || h == 0 || w > 4096 || h > 4096 {
            return None;
        }
        let slot = self.targets.get(target)?;
        if slot.w != w || slot.h != h || slot.color.is_none() {
            let make = |label, format, usage, samples| {
                device.create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width: w,
                        height: h,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: samples,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage,
                    view_formats: &[],
                })
            };
            let color = make(
                "fz1-out",
                wgpu::TextureFormat::Rgba8Unorm,
                wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
                1,
            );
            let msaa = make(
                "fz1-msaa",
                wgpu::TextureFormat::Rgba8Unorm,
                wgpu::TextureUsages::RENDER_ATTACHMENT,
                MSAA,
            );
            let depth = make(
                "fz1-depth",
                wgpu::TextureFormat::Depth24Plus,
                wgpu::TextureUsages::RENDER_ATTACHMENT,
                MSAA,
            );
            let color_view = color.create_view(&Default::default());
            let depth_view = depth.create_view(&Default::default());
            if let Some(slot) = self.targets.get_mut(target) {
                slot.color = Some((color, color_view));
                slot.msaa = Some(msaa.create_view(&Default::default()));
                slot.depth = Some((depth, depth_view));
                slot.w = w;
                slot.h = h;
            }
        }
        let slot = self.targets.get(target)?;
        let (_, color_view) = slot.color.as_ref()?;
        let (_, depth_view) = slot.depth.as_ref()?;
        let msaa_view = slot.msaa.as_ref()?;

        // Where each model stands this frame: the unboxing moves the machine
        // and the carton as a whole.
        let pose = carton_pose(anim.unbox);
        let unboxing = anim.unbox < 0.999;
        let lift = |m: &Model| match m.kind {
            // The machine rides up on edge and pitches about its own middle,
            // so the turn is a rotation around a point PIVOT_Y above its feet.
            Kind::Console if unboxing => mat_mul(
                &mat_translate([0.0, pose.console_y + PIVOT_Y, 0.0]),
                &mat_mul(&mat_rot_x(pose.tilt), &mat_translate([0.0, -PIVOT_Y, 0.0])),
            ),
            Kind::Carton => mat_translate([0.0, pose.carton_y, 0.0]),
            _ => mat_identity(),
        };

        // Framing: the machine alone, or — while the carton is on show — the
        // carton around it, blended so the camera eases from one to the other.
        let own_view = target == 0;
        let (lo, hi) = {
            // Each view frames what it shows: the machine view frames the
            // machine, the case view the case alone.
            let lead: Vec<(&Model, Mat4)> = models
                .iter()
                .filter(|m| matches!((own_view, m.kind), (true, Kind::Console) | (false, Kind::Case)))
                .map(|m| (*m, lift(m)))
                .collect();
            let (mut lo, mut hi) = bounds_of(&lead);
            let carton: Vec<(&Model, Mat4)> = models
                .iter()
                .filter(|m| m.kind == Kind::Carton)
                .map(|m| (*m, mat_identity()))
                .collect();
            if !carton.is_empty() && pose.frame > 0.0 {
                let (clo, chi) = bounds_of(&carton);
                for (l, cl) in lo.iter_mut().zip(clo) {
                    *l += (cl.min(*l) - *l) * pose.frame;
                }
                for (h, ch) in hi.iter_mut().zip(chi) {
                    *h += (ch.max(*h) - *h) * pose.frame;
                }
            }
            (lo, hi)
        };
        let c = [
            (lo[0] + hi[0]) * 0.5,
            (lo[1] + hi[1]) * 0.5,
            (lo[2] + hi[2]) * 0.5,
        ];
        let half = |axis: usize| {
            (hi.get(axis).copied().unwrap_or(0.0) - lo.get(axis).copied().unwrap_or(0.0)) * 0.5
        };
        let r = half(0).max(half(1)).max(half(2)).max(0.05);
        let (yaw, pitch, zoom) = if own_view {
            (anim.yaw, anim.pitch.clamp(-1.3, 1.3), anim.zoom.clamp(0.45, 3.0))
        } else {
            (
                anim.case_yaw,
                anim.case_pitch.clamp(-1.4, 1.4),
                anim.case_zoom.clamp(0.5, 2.5),
            )
        };
        let aspect = (w as f32 / h as f32).max(0.2);
        // Far enough that the whole thing fits the view it is given, across
        // as well as down. The drawer slides toward the viewer, so the view
        // opens out with it.
        let open = if own_view && !unboxing { anim.tray.clamp(0.0, 1.0) } else { 0.0 };
        // The box's bounding sphere fits the narrower of the two fields of
        // view, whichever way round it is turned.
        let tan = (38f32.to_radians() * 0.5).tan().max(1e-3) * aspect.min(1.0);
        let sphere = (half(0).powi(2) + half(1).powi(2) + half(2).powi(2)).sqrt();
        let dist = sphere / tan * 0.92 * zoom * (1.0 + 0.24 * open);
        let (sy, cy) = yaw.sin_cos();
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
        let proj = mat_perspective(38f32.to_radians(), aspect, 0.01, 20.0);
        let vp = mat_mul(&proj, &view);

        // The tray slides along the animation channel.
        // A machine still in its carton has its drawer shut, whatever the
        // launcher would otherwise be asking for.
        let tray_t = if unboxing { 0.0 } else { anim.tray.clamp(0.0, 1.0) };
        let tray_pos = {
            let rest = model.tray_rest.unwrap_or([0.0; 3]);
            // The clip says which way the drawer runs; TRAY_TRAVEL says how
            // far, so the tray comes fully clear of the case whatever the
            // asset's own clip was authored at.
            let end = model
                .tray_anim
                .last()
                .map(|l| l.1)
                .unwrap_or([rest[0], rest[1], rest[2] + 1.0]);
            let d = [end[0] - rest[0], end[1] - rest[1], end[2] - rest[2]];
            let n = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt().max(1e-6);
            let s = tray_t * TRAY_TRAVEL / n;
            [rest[0] + d[0] * s, rest[1] + d[1] * s, rest[2] + d[2] * s]
        };

        // Every part's world transform, worked out once for drawing and for
        // the click targets.
        let world_of = |m: &Model, part: &Part| -> Option<Mat4> {
            let base = mat_mul(&lift(m), &m.place);
            let mut world = mat_mul(&base, &mat_mul(&part.parent, &part.local));
            match (m.kind, part.name.as_str()) {
                (Kind::Console, "FZ1_Tray" | "Disc") => {
                    let rest = m.tray_rest.unwrap_or(tray_pos);
                    let slide = mat_translate([
                        tray_pos[0] - rest[0],
                        tray_pos[1] - rest[1],
                        tray_pos[2] - rest[2],
                    ]);
                    world = mat_mul(&base, &mat_mul(&slide, &mat_mul(&part.parent, &part.local)));
                    if part.name == "Disc" {
                        if !anim.disc_shown {
                            return None;
                        }
                        // Coming down onto the spindle as it is put in.
                        let drop = 1.0 - anim.disc_drop.clamp(0.0, 1.0);
                        world = mat_mul(&mat_translate([0.0, drop * 0.09, 0.0]), &world);
                        world = mat_mul(&world, &mat_rot_y(anim.spin));
                    }
                }
                (Kind::Carton, _) if !unboxing => return None,
                (Kind::Carton, "BoxLid") => world = mat_mul(&world, &mat_rot_x(-pose.lid)),
                (Kind::Carton, "FlapLeft") => world = mat_mul(&world, &mat_rot_z(pose.flaps)),
                (Kind::Carton, "FlapRight") => world = mat_mul(&world, &mat_rot_z(-pose.flaps)),
                _ => {}
            }
            Some(world)
        };

        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("fz1") });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("fz1"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: msaa_view,
                    resolve_target: Some(color_view),
                    ops: wgpu::Operations {
                        // Clear transparent: the GUI's own backdrop shows
                        // through wherever nothing is drawn.
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Discard,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            for (gpu_parts, m) in self.parts.iter().skip(first).zip(models.iter()) {
                for (gpu, part) in gpu_parts.iter().zip(m.parts.iter()) {
                    let Some(world) = world_of(m, part) else {
                        continue;
                    };
                    let mvp = mat_mul(&vp, &world);
                    // The lamps are driven by name: READY holds while the
                    // machine is powered, ACCESS follows the level the GUI
                    // sets.
                    let gain = match part.name.as_str() {
                        "ReadyGlow" => {
                            if anim.lit {
                                2.0
                            } else {
                                0.0
                            }
                        }
                        "AccessGlow" => anim.access.clamp(0.0, 1.0) * 3.0,
                        _ => 0.0,
                    };
                    let params = [
                        if part.textured || ArtSlot::of_material(&part.material).is_some() {
                            1.0
                        } else {
                            0.0
                        },
                        if part.emissive.iter().any(|e| *e > 0.01) {
                            gain
                        } else {
                            0.0
                        },
                        if m.kind == Kind::Case { 1.0 } else { 0.0 },
                        0.0,
                    ];
                    // A lit lamp shows its own colour, not the dark lens.
                    let color = if params[1] > 0.0 {
                        [part.emissive[0], part.emissive[1], part.emissive[2], 1.0]
                    } else {
                        part.color
                    };
                    let mut udata = Vec::with_capacity(160);
                    udata.extend_from_slice(bytemuck_f32(mvp.as_flattened()));
                    udata.extend_from_slice(bytemuck_f32(world.as_flattened()));
                    udata.extend_from_slice(bytemuck_f32(&color));
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
        for m in models.iter() {
            for part in &m.parts {
                let Some(world) = world_of(m, part) else {
                    continue;
                };
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
        match self.targets.get(target).and_then(|t| t.tex_id) {
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

    /// The colour texture a target was last drawn into (the offscreen
    /// checks in examples/ read it back).
    #[allow(dead_code)]
    pub fn target_texture(&self, target: usize) -> Option<&wgpu::Texture> {
        self.targets.get(target)?.color.as_ref().map(|(t, _)| t)
    }

    /// Let go of the egui textures the targets registered, before the
    /// renderer is dropped, so a suspended scene leaves nothing behind.
    pub fn release(&mut self, state: &egui_wgpu::RenderState) {
        let mut renderer = state.renderer.write();
        for t in self.targets.drain(..) {
            if let Some(id) = t.tex_id {
                renderer.free_texture(&id);
            }
        }
    }
}

fn write_rgba(queue: &wgpu::Queue, tex: &wgpu::Texture, rgba: &[u8], w: u32, h: u32) {
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: tex,
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
}

fn bytemuck_f32(v: &[f32]) -> &[u8] {
    bytemuck::cast_slice(v)
}

fn bytemuck_u32(v: &[u32]) -> &[u8] {
    bytemuck::cast_slice(v)
}
