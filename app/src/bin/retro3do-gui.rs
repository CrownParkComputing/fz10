//! retro3do-gui: the visual front end for the Rust 3DO core.
//!
//! egui over wgpu (Vulkan/GL) so the same code runs on Windows, Linux, macOS
//! and Android. SDL3 handles only game audio and gamepads. The console, disc
//! and pads are the real models rendered by `fz1_3d`; the box art comes
//! from the person's own RetroMedia library.

#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

#[path = "retro3do-gui/boot_logo.rs"]
mod boot_logo;
#[path = "retro3do-gui/case_art.rs"]
mod case_art;
#[path = "retro3do-gui/fz1_3d.rs"]
mod fz1_3d;
#[path = "retro3do-gui/gpu_scale.rs"]
mod gpu_scale;
#[cfg(target_os = "android")]
#[path = "retro3do-gui/android_pad.rs"]
mod android_pad;
#[cfg(not(target_os = "android"))]
#[path = "retro3do-gui/sdl_pad.rs"]
mod sdl_pad;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

use egui::{ColorImage, StrokeKind, TextureHandle, TextureOptions};
use retro3do::{PadButton, Region, Retro3do, MAX_PADS};
#[cfg(not(target_os = "android"))]
use sdl3::audio::{AudioFormat, AudioSpec, AudioStreamOwner};

/// A case spine's width over its height (a 14 mm spine on a 207 mm case,
/// as the media folder's spine scans are cut).
const SPINE_ASPECT: f32 = 146.0 / 2156.0;
/// How many spines the shelf shows at once: a compact strip, not a wall.
const SHELF_MAX_SPINES: usize = 20;
/// How long the unboxing and the box-away run, in seconds.
const UNBOX_SECS: f32 = 9.0;
const BOXAWAY_SECS: f32 = 5.0;
/// How long the machine stands alone on the table after the unboxing before
/// the shelf comes in, and how long the shelf takes to arrive. Nothing is
/// allowed over the reveal until the machine has settled.
const SHELF_WAIT: f32 = 0.45;
const SHELF_FADE: f32 = 0.8;
/// The presentation spin: the sealed carton turns once on the spot, slowly,
/// before the lid opens, so every face of the retail box is seen.
const SPIN_SECS: f32 = 6.0;
/// The whole opening show: the spin, then the unboxing.
const INTRO_SECS: f32 = SPIN_SECS + UNBOX_SECS;
/// How far into the show the unboxing starts.
const SPIN_FRAC: f32 = SPIN_SECS / INTRO_SECS;
/// The yaw the machine view sits at: a three-quarter view, and where the
/// presentation spin starts and lands.
const YAW0: f32 = 0.42;
/// The canvas a generated spine is drawn on: a real spine's proportions,
/// at a size that still reads when the shelf lifts one out.
const GEN_SPINE_W: u32 = 128;
const GEN_SPINE_H: u32 = 1890;

/// What the pad does in the launcher, shown on the shelf and on the
/// settings page so none of it has to be guessed at.
const PAD_MAP: &str =
    "pad · left/right pick · up case · A select, then boot · B back · \
     C hide or show · P play · L/R zoom · X close";

/// The same map, cut to what a narrow window has room for.
const PAD_MAP_SHORT: &str = "pad · d-pad picks · A boot · B back · C hide · X close";

/// The same map, a row each, for the settings page.
const PAD_LAUNCHER: &[(&str, &str)] = &[
    ("left / right", "along the shelf - or turn the case when one is open"),
    ("up / down", "open the case the shelf points at - or tilt it, and the machine"),
    ("A", "take the case out, then put its disc in"),
    ("B", "back: shut the case, or back to the game"),
    ("C", "hide the title, or put a hidden one back"),
    ("P", "play: the disc goes straight in"),
    ("L / R", "zoom out and in on whatever is on the table"),
    ("X", "put the machine away and close the launcher"),
];

/// How far the tiled wall drifts before it turns round and goes back.
const WALL_SPAN: f32 = 1400.0;
/// How long the overlay icons stay up after the player's last touch while a
/// game is on screen.
const CHROME_IDLE: Duration = Duration::from_secs(3);

/// One floating icon: a glyph on a dark pad, no frame and no label.
fn icon_button(ui: &mut egui::Ui, glyph: &str, hint: &str) -> bool {
    let size = egui::vec2(34.0, 34.0);
    let (spot, press) = ui.allocate_exact_size(size, egui::Sense::click());
    let hot = press.hovered();
    ui.painter().rect_filled(
        spot,
        6.0,
        egui::Color32::from_black_alpha(if hot { 220 } else { 150 }),
    );
    ui.painter().text(
        spot.center(),
        egui::Align2::CENTER_CENTER,
        glyph,
        egui::FontId::proportional(18.0),
        if hot { egui::Color32::WHITE } else { TEXT_DIM },
    );
    if hot {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    press.on_hover_text(hint).clicked()
}

/// The pair of disc marks: an arrow over the tray's bar. Eject points up and
/// out of the tray, insert points down and into it — the same mark turned
/// around, drawn rather than typed so the two are unmistakably a pair.
fn tray_icon(ui: &mut egui::Ui, insert: bool, hint: &str) -> bool {
    let size = egui::vec2(34.0, 34.0);
    let (spot, press) = ui.allocate_exact_size(size, egui::Sense::click());
    let hot = press.hovered();
    ui.painter().rect_filled(
        spot,
        6.0,
        egui::Color32::from_black_alpha(if hot { 220 } else { 150 }),
    );
    let ink = if hot { egui::Color32::WHITE } else { TEXT_DIM };
    let cx = spot.center().x;
    // The tray: a bar the arrow sits over.
    let bar = spot.max.y - 9.0;
    ui.painter().rect_filled(
        egui::Rect::from_min_max(egui::pos2(cx - 8.0, bar), egui::pos2(cx + 8.0, bar + 2.0)),
        0.0,
        ink,
    );
    // The arrow, in the same space either way round.
    let top = spot.min.y + 7.0;
    let (apex, base) = if insert {
        (bar - 2.0, bar - 8.0)
    } else {
        (top, top + 6.0)
    };
    ui.painter().add(egui::Shape::convex_polygon(
        vec![
            egui::pos2(cx, apex),
            egui::pos2(cx - 4.5, base),
            egui::pos2(cx + 4.5, base),
        ],
        ink,
        egui::Stroke::NONE,
    ));
    let stem = if insert {
        egui::Rect::from_min_max(egui::pos2(cx - 1.5, top), egui::pos2(cx + 1.5, base))
    } else {
        egui::Rect::from_min_max(egui::pos2(cx - 1.5, base), egui::pos2(cx + 1.5, bar - 2.0))
    };
    ui.painter().rect_filled(stem, 0.0, ink);
    if hot {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    press.on_hover_text(hint).clicked()
}

/// Ease in and out of 0..1.
fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
/// The drawer's travel rate (full travel per second) and how long the
/// platter spins up before the machine is switched on.
const TRAY_RATE: f32 = 0.5;
/// The case lid takes about a second and a half to swing.
const LID_RATE: f32 = 0.7;
const SPINUP_SECS: f32 = 2.6;

/// Audio the stream keeps buffered — about 93 ms at 44.1 kHz, deep enough to
/// play through a stalled emulated field without a gap, thin enough that the
/// picture and the sound stay married — and how much is pulled at a time.
#[cfg(not(target_os = "android"))]
const AUDIO_PAIRS_TARGET: i32 = 4096;
const AUDIO_PAIRS_PULL: usize = 1024;
#[cfg(not(target_os = "android"))]
const AUDIO_BYTES_PER_PAIR: i32 = 4;

/// What the core is asked to draw at when its art is being read. It is a ceiling,
/// not a promise: the emulator thread lowers it by itself rather than let
/// fields run late, and the pack still replaces art at native size.
const HD_INTERNAL_SCALE: u32 = 2;

const ACCENTS: [egui::Color32; 8] = [
    egui::Color32::from_rgb(230, 80, 120),
    egui::Color32::from_rgb(80, 200, 120),
    egui::Color32::from_rgb(120, 90, 220),
    egui::Color32::from_rgb(90, 150, 230),
    egui::Color32::from_rgb(230, 160, 60),
    egui::Color32::from_rgb(60, 190, 200),
    egui::Color32::from_rgb(200, 200, 90),
    egui::Color32::from_rgb(150, 110, 90),
];

/// The texture-cache key the extracted boot logo arrives under (see
/// `boot_logo.rs`). It is not a media path, so it is never re-asked for.
const LOGO_KEY: &str = "__boot_logo__";

/// The 3DO badge in the cache: the tile the machine view's wallpaper is
/// made of.
const WALL_KEY: &str = "__3do_mark__";

const BG: egui::Color32 = egui::Color32::from_rgb(14, 14, 20);
const PANEL: egui::Color32 = egui::Color32::from_rgb(22, 22, 32);
const CARD: egui::Color32 = egui::Color32::from_rgb(28, 28, 40);
const TEXT_DIM: egui::Color32 = egui::Color32::from_rgb(140, 140, 155);

// ---------------------------------------------------------------------------
// Paths and scanning

struct Config {
    games_dir: PathBuf,
    media_dir: PathBuf,
    bios_dir: PathBuf,
    saves_dir: PathBuf,
    /// 1-based game index to insert on launch (skips the library).
    boot: Option<usize>,
    /// Like boot, but powers the machine on straight away.
    play: Option<usize>,
    /// Pre-rendered documentation pages (see tools/fetch-docs.sh).
    docs_dir: PathBuf,
    /// The BIOS file remembered from the walkthrough.
    bios_choice: Option<String>,
    /// Whether the walkthrough has been through once.
    configured: bool,
    /// Directory of hand-authored cel replacements (`<fnv>.png`, see
    /// src/texpack.rs); overrides the computed filter where it has a match.
    texdir: Option<PathBuf>,
    /// Explicit artwork map: ROM stem and media set to artwork file.
    name_map: HashMap<(String, String), String>,
    /// Titles the person has hidden, by disc file stem. They are kept out of
    /// the shelf until "show hidden" is on.
    hidden: std::collections::HashSet<String>,
    /// Print where every game finds its artwork and stop.
    resolve: bool,
    /// The game picture filtered smoothly on the way up to the window,
    /// instead of crisp nearest-neighbour pixels.
    smooth: bool,
    /// ARM work per field as a percentage of the real machine's: 100 is the
    /// real thing, up to 150 for games the hardware itself struggled with.
    cpu_scale: u32,
    /// Which attached gamepad drives console pad 0.
    pad_primary: usize,
    /// The pad mapping: 3DO button name to SDL button name.
    pad_bind: Vec<(String, String)>,
}

/// The stock pad mapping. Android takes its pads from the Java shim, which
/// has a fixed mapping of its own, so there is nothing to bind there.
fn default_bindings() -> Vec<(String, String)> {
    #[cfg(not(target_os = "android"))]
    {
        sdl_pad::default_bindings()
    }
    #[cfg(target_os = "android")]
    {
        Vec::new()
    }
}

/// A saved mapping as one settings-file value: `A:South,B:East,...`.
fn bindings_to_text(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(three, sdl)| format!("{three}:{sdl}"))
        .collect::<Vec<_>>()
        .join(",")
}

fn bindings_from_text(text: &str) -> Vec<(String, String)> {
    text.split(',')
        .filter_map(|pair| pair.split_once(':'))
        .map(|(three, sdl)| (three.trim().to_string(), sdl.trim().to_string()))
        .collect()
}

/// Shared, user-visible storage on Android: a folder a person can fill from a
/// file manager, a card reader or `adb push`. Writability needs "All files
/// access" (MANAGE_EXTERNAL_STORAGE, in the manifest).
#[cfg(target_os = "android")]
const ANDROID_STORAGE: &str = "/sdcard/fz10";

/// What Android lets an app write with no permission at all, used when the
/// shared folder above is not writable (the permission was not granted).
#[cfg(target_os = "android")]
const ANDROID_APP_STORAGE: &str =
    "/storage/emulated/0/Android/data/com.crownpark.fz10/files/fz10";

/// The folder the launcher's own files live in: the shared one on Android,
/// `~/.config/retro3do` elsewhere.
fn config_dir() -> PathBuf {
    #[cfg(target_os = "android")]
    {
        let shared = PathBuf::from(ANDROID_STORAGE);
        if std::fs::create_dir_all(&shared).is_ok() {
            return shared;
        }
        let own = PathBuf::from(ANDROID_APP_STORAGE);
        let _ = std::fs::create_dir_all(&own);
        own
    }
    #[cfg(not(target_os = "android"))]
    {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(format!("{home}/.config/retro3do"))
    }
}

/// Where the launcher remembers its paths between runs.
fn settings_path() -> PathBuf {
    config_dir().join("launcher.conf")
}

/// The defaults for the folders a person fills: on Android everything sits
/// under one shared folder, so the walkthrough can point at it and be done.
fn default_dirs() -> (PathBuf, PathBuf, PathBuf, PathBuf, PathBuf) {
    #[cfg(target_os = "android")]
    {
        let root = config_dir();
        (
            root.join("games"),
            root.join("media"),
            root.join("bios"),
            root.join("saves"),
            root.join("docs"),
        )
    }
    #[cfg(not(target_os = "android"))]
    {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        (
            PathBuf::from(format!("{home}/3do-library/games")),
            PathBuf::from(format!("{home}/StudioProjects/RetroMedia/media/3do")),
            PathBuf::from(format!("{home}/3do-library/bios")),
            PathBuf::from(format!("{home}/3do-library/saves")),
            PathBuf::from(format!("{home}/3do-library/docs")),
        )
    }
}

/// Where a folder picker starts when a field is empty.
fn start_dir() -> PathBuf {
    config_dir()
}

impl Config {
    /// Read the remembered paths, if the walkthrough has been through once.
    /// One `key = value` a line; anything else is ignored.
    fn load_settings(&mut self) -> bool {
        let Ok(text) = std::fs::read_to_string(settings_path()) else {
            return false;
        };
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim().to_string();
            if value.is_empty() {
                continue;
            }
            match key.trim() {
                "games" => self.games_dir = PathBuf::from(&value),
                "bios" => self.bios_dir = PathBuf::from(&value),
                "saves" => self.saves_dir = PathBuf::from(&value),
                "media" => self.media_dir = PathBuf::from(&value),
                "bios_file" => self.bios_choice = Some(value),
                "smooth" => self.smooth = value == "true",
                "cpu_scale" => self.cpu_scale = value.parse().unwrap_or(100).clamp(100, 150),
                "pad_primary" => self.pad_primary = value.parse().unwrap_or(0),
                "hidden" => {
                    self.hidden = value.split('|').map(str::to_string).collect();
                }
                "pad_bind" => {
                    let bind = bindings_from_text(&value);
                    if !bind.is_empty() {
                        self.pad_bind = bind;
                    }
                }
                _ => {}
            }
        }
        true
    }

    fn save_settings(&self) -> Result<(), String> {
        let path = settings_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let text = format!(
            "games = {}\nbios = {}\nsaves = {}\nmedia = {}\nbios_file = {}\n\
             smooth = {}\ncpu_scale = {}\npad_primary = {}\npad_bind = {}\n\
             hidden = {}\n",
            self.games_dir.display(),
            self.bios_dir.display(),
            self.saves_dir.display(),
            self.media_dir.display(),
            self.bios_choice.clone().unwrap_or_default(),
            self.smooth,
            self.cpu_scale,
            self.pad_primary,
            bindings_to_text(&self.pad_bind),
            {
                let mut names: Vec<&str> = self.hidden.iter().map(String::as_str).collect();
                names.sort_unstable();
                names.join("|")
            },
        );
        std::fs::write(&path, text).map_err(|e| e.to_string())
    }

    fn with_defaults() -> Self {
        let (games_dir, media_dir, bios_dir, saves_dir, docs_dir) = default_dirs();
        Config {
            games_dir,
            media_dir,
            bios_dir,
            saves_dir,
            boot: None,
            play: None,
            docs_dir,
            bios_choice: None,
            configured: false,
            texdir: std::env::var_os("RETRO3DO_TEXDIR").map(PathBuf::from),
            name_map: HashMap::new(),
            hidden: std::collections::HashSet::new(),
            resolve: false,
            smooth: false,
            cpu_scale: 100,
            pad_primary: 0,
            pad_bind: default_bindings(),
        }
    }

    fn from_args() -> Result<Self, String> {
        let mut cfg = Self::with_defaults();
        // Remembered paths first; anything on the command line wins.
        let configured = cfg.load_settings();
        cfg.configured = configured;
        let mut args = std::env::args().skip(1);
        let mut dirs_set = false;
        let mut resolve = false;
        while let Some(arg) = args.next() {
            if arg == "--resolve" {
                resolve = true;
                continue;
            }
            let value = args.next().ok_or_else(|| format!("{arg} wants a value"))?;
            match arg.as_str() {
                "--games-dir" => {
                    cfg.games_dir = PathBuf::from(value);
                    dirs_set = true;
                }
                "--media-dir" => cfg.media_dir = PathBuf::from(value),
                "--bios-dir" => {
                    cfg.bios_dir = PathBuf::from(value);
                    dirs_set = true;
                }
                "--saves-dir" => cfg.saves_dir = PathBuf::from(value),
                "--docs-dir" => cfg.docs_dir = PathBuf::from(value),
                "--texdir" => cfg.texdir = Some(PathBuf::from(value)),
                "--boot" => {
                    cfg.boot = value
                        .parse::<usize>()
                        .ok()
                        .filter(|n| *n >= 1)
                        .map(|n| n - 1)
                }
                "--play" => {
                    cfg.play = value
                        .parse::<usize>()
                        .ok()
                        .filter(|n| *n >= 1)
                        .map(|n| n - 1)
                }
                "-h" | "--help" => {
                    return Err("usage: retro3do-gui [--games-dir D] [--media-dir D] [--bios-dir D] [--saves-dir D] [--docs-dir D] [--texdir D] [--boot N] [--play N]".to_string());
                }
                other => return Err(format!("unknown argument: {other}")),
            }
        }
        // Handed real paths on the command line counts as configured: the
        // walkthrough is for someone who has never run this before.
        cfg.configured = cfg.configured || dirs_set;
        cfg.resolve = resolve;
        cfg.name_map = load_name_map(&cfg);
        Ok(cfg)
    }
}

/// A disc image in the games directory.
struct Game {
    path: PathBuf,
    /// Every disc of the title, in order. One entry for most games; an
    /// `.m3u` beside the discs names the rest for a title that swaps.
    discs: Vec<PathBuf>,
    title: String,
    /// Media file (cover) matched from the media directory, if any.
    cover: Option<PathBuf>,
    /// The cover's decode key: its path as text.
    cover_key: Option<String>,
    /// The disc's own art: a real scan from the media `discs` directory
    /// where there is one, else the `cartridges` art.
    disc_art: Option<PathBuf>,
    disc_key: Option<String>,
}

/// Words that mark a bracket as a region, language or revision tag rather
/// than part of the title: "(USA)", "(USA, Europe)", "(En,Fr)", "(RE1)".
const TAG_WORDS: &[&str] = &[
    "usa", "europe", "japan", "korea", "asia", "australia", "brazil", "canada", "china",
    "germany", "france", "italy", "spain", "uk", "netherlands", "sweden", "norway", "finland",
    "denmark", "portugal", "poland", "russia", "czech", "republic", "greece", "turkey",
    "israel", "arabic", "hebrew", "hindi", "taiwan", "world", "intl", "international", "multi",
    "en", "fr", "de", "es", "it", "ja", "ko", "pt", "nl", "sv", "no", "da", "fi", "pl", "ru",
    "cs", "el", "tr", "zh", "ar", "he", "hi", "th", "vi", "id", "ms",
    "re", "rev", "beta", "demo", "proto", "prototype", "alpha", "prerelease", "trial", "debug",
];

/// Whether a bracketed group says only where the disc came from: every word
/// in it is a region, a language or a revision marker.
fn is_tag_group(content: &str) -> bool {
    // "RE1", "rev2", "v1.0": a revision marker with a number on it.
    let revision = |word: &str, stem: &str| {
        word.len() > stem.len()
            && word.starts_with(stem)
            && word[stem.len()..].chars().all(|c| c.is_ascii_digit())
    };
    let mut any = false;
    for word in content.split(|c: char| !c.is_alphanumeric()) {
        if word.is_empty() {
            continue;
        }
        any = true;
        let lower = word.to_ascii_lowercase();
        if TAG_WORDS.contains(&lower.as_str())
            || lower.chars().all(|c| c.is_ascii_digit())
            || revision(&lower, "re")
            || revision(&lower, "rev")
            || revision(&lower, "v")
        {
            continue;
        }
        return false;
    }
    any
}

/// Lowercase, region and revision tags dropped, punctuation folded to spaces:
/// the matching key for both a game filename and a media filename.
fn normalize(stem: &str) -> String {
    let mut text = String::with_capacity(stem.len());
    let mut rest = stem;
    loop {
        let Some(open) = rest.find(['(', '[']) else {
            text.push_str(rest);
            break;
        };
        text.push_str(&rest[..open]);
        let close = if rest[open..].starts_with('(') { ')' } else { ']' };
        let tail = &rest[open + 1..];
        let Some(end) = tail.find(close) else {
            text.push_str(tail);
            break;
        };
        let content = &tail[..end];
        // A group that is only region and revision noise goes; "(Disc 2)"
        // stays, because it tells one disc of a set from another.
        if !is_tag_group(content) {
            text.push(' ');
            text.push_str(content);
            text.push(' ');
        }
        rest = &tail[end + 1..];
    }

    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str(" and "),
            c if c.is_alphanumeric() => out.extend(c.to_lowercase()),
            _ => out.push(' '),
        }
    }
    // ", The" moves to the front in some scrapers; the media library uses it.
    // Dropping the word entirely makes "The Need for Speed" and "Need for
    // Speed, The" the same key.
    out.split_whitespace()
        .filter(|w| *w != "the")
        .collect::<Vec<_>>()
        .join(" ")
}

fn file_stem(p: &Path) -> String {
    p.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string()
}

/// A path's identity for "is this the same file?" questions: the resolved
/// path when it exists, the text as written when it does not (an m3u may name
/// a disc that has not been dumped yet).
fn path_key(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

fn scan_dir(dir: &Path, exts: &[&str]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    if let Ok(read) = std::fs::read_dir(dir) {
        for entry in read.flatten() {
            let path = entry.path();
            let is_wanted = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| exts.iter().any(|x| x.eq_ignore_ascii_case(e)));
            if path.is_file() && is_wanted {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// The explicit artwork map: "ROM stem <TAB> media set <TAB> artwork file",
/// one line each. It names the file for a game whose artwork does not carry
/// the game's own name.
fn load_name_map(cfg: &Config) -> HashMap<(String, String), String> {
    let mut map = HashMap::new();
    let mut paths = Vec::new();
    if let Some(parent) = cfg.games_dir.parent() {
        paths.push(parent.join("name-map.tsv"));
    }
    paths.push(cfg.media_dir.join("name-map.tsv"));
    for path in paths {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut parts = line.split('\t');
            let (Some(rom), Some(set), Some(file)) = (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            map.insert((rom.to_string(), set.to_string()), file.to_string());
        }
        if !map.is_empty() {
            break;
        }
    }
    map
}

/// The artwork for one game in one media set: the explicit map first, then the
/// name match. `sets` is the order the sets are tried in.
fn resolve_media(
    cfg: &Config,
    rom_stem: &str,
    sets: &[&str],
    candidates: &[PathBuf],
) -> Option<PathBuf> {
    for set in sets {
        let Some(file) = cfg.name_map.get(&(rom_stem.to_string(), (*set).to_string())) else {
            continue;
        };
        let hit = candidates.iter().find(|c| {
            c.parent()
                .and_then(|p| p.file_name())
                .map(|d| d.to_string_lossy().into_owned())
                .is_some_and(|d| d == *set)
                && c.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .is_some_and(|n| n == *file)
        });
        if hit.is_some() {
            return hit.cloned();
        }
    }
    if let Some(hit) = match_media(&normalize(rom_stem), candidates) {
        return Some(hit.clone());
    }
    // One disc of a set: the set's own artwork stands in for the disc.
    if let Some((base, _)) = disc_tag(rom_stem) {
        return resolve_media(cfg, &base, sets, candidates);
    }
    None
}

/// Print where every game finds its artwork, one line per game and media set:
/// the check that the artwork names line up with the disc names.
fn resolve_report(cfg: &Config) {
    let sets = [
        "box2d", "boxback", "spine", "discs", "cartridges", "images", "marquee", "titles",
        "fanarts", "thumbnails",
    ];
    // The library card's own pick: the first set that has the game's cover.
    let covers: Vec<PathBuf> = ["box2d", "images", "thumbnails", "titles"]
        .iter()
        .flat_map(|set| scan_dir(&cfg.media_dir.join(set), &["png", "jpg", "jpeg"]))
        .collect();
    for path in scan_dir(&cfg.games_dir, &["chd", "iso", "cue", "bin", "img"]) {
        let stem = file_stem(&path);
        println!(
            "{}\tcover\t{}",
            stem,
            resolve_media(cfg, &stem, &["box2d", "images", "thumbnails", "titles"], &covers)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| "-".to_string())
        );
        for set in sets {
            let candidates = scan_dir(&cfg.media_dir.join(set), &["png", "jpg", "jpeg"]);
            let hit = resolve_media(cfg, &stem, &[set], &candidates);
            println!(
                "{}\t{}\t{}",
                stem,
                set,
                hit.map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "-".to_string())
            );
        }
    }
}

/// The real disc scan for a game, from the `discs` media set.
///
/// A scan belongs to exactly one game, so the match is on the title alone:
/// the game's own scan, or the first disc of a set, and never a longer
/// title that merely contains it.
fn resolve_disc_scan(cfg: &Config, stem: &str, scans: &[PathBuf]) -> Option<PathBuf> {
    if let Some(file) = cfg.name_map.get(&(stem.to_string(), "discs".to_string())) {
        if let Some(hit) = scans.iter().find(|c| {
            c.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .as_deref()
                == Some(file.as_str())
        }) {
            return Some(hit.clone());
        }
    }
    let (want_base, want_number) = match disc_tag(stem) {
        Some((base, number)) => (normalize(&base), Some(number)),
        None => (normalize(stem), None),
    };
    let mut best: Option<(u8, &PathBuf)> = None;
    for scan in scans {
        let name = file_stem(scan);
        let (base, number) = match disc_tag(&name) {
            Some((base, number)) => (normalize(&base), number),
            None => (normalize(&name), 1),
        };
        if base != want_base {
            continue;
        }
        // A set wants its own disc; a single-disc game takes disc one.
        let score = match want_number {
            Some(want) if want == number => 0,
            Some(_) => continue,
            None => u8::from(number != 1),
        };
        if best.is_none_or(|(s, _)| score < s) {
            best = Some((score, scan));
        }
    }
    best.map(|(_, scan)| scan.clone())
}

/// Longest normalized media stem that appears inside the game's stem, or the
/// reverse. Earlier candidates win ties, so ordered dirs give the preference.
fn match_media<'a>(stem: &str, candidates: &'a [PathBuf]) -> Option<&'a PathBuf> {
    // Exact first: "Alone in the Dark" must not pick up "Alone in the Dark
    // 2" just because that name is longer.
    if let Some(exact) = candidates.iter().find(|c| normalize(&file_stem(c)) == stem) {
        return Some(exact);
    }
    let mut best: Option<(usize, &PathBuf)> = None;
    for cand in candidates {
        let key = normalize(&file_stem(cand));
        if key.is_empty() || stem.is_empty() {
            continue;
        }
        let hits = stem.contains(&key) || key.contains(stem);
        if hits && best.is_none_or(|(n, _)| key.len() > n) {
            best = Some((key.len(), cand));
        }
    }
    best.map(|(_, c)| c)
}

fn scan_games(cfg: &Config) -> Vec<Game> {
    let mut covers: Vec<PathBuf> = Vec::new();
    for sub in ["box2d", "images", "thumbnails", "titles"] {
        covers.extend(scan_dir(&cfg.media_dir.join(sub), &["png", "jpg", "jpeg"]));
    }
    let disc_art = scan_dir(&cfg.media_dir.join("cartridges"), &["png", "jpg", "jpeg"]);
    let disc_scans = scan_dir(&cfg.media_dir.join("discs"), &["png", "jpg", "jpeg"]);

    // A title that spans discs is one entry. An `.m3u` beside them says so
    // outright and fixes the order; without one, "Disc 2"/"CD2" and their
    // like fold into the lowest-numbered disc.
    let mut sets: Vec<(String, Vec<PathBuf>)> = Vec::new();
    let mut listed: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    for list in scan_dir(&cfg.games_dir, &["m3u"]) {
        let Ok(text) = std::fs::read_to_string(&list) else {
            continue;
        };
        let dir = list.parent().unwrap_or(&cfg.games_dir);
        let discs: Vec<PathBuf> = text
            .trim_start_matches('\u{feff}')
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(|l| {
                let l = l.trim().trim_matches(|c| c == '"' || c == '\'');
                let p = PathBuf::from(l);
                let p = if p.is_absolute() {
                    p
                } else {
                    dir.join(p)
                };
                // Resolve "./disc.iso" so the queue and the scan agree on
                // identity; a name that does not exist yet is kept as typed.
                path_key(&p)
            })
            .collect();
        if !discs.is_empty() {
            listed.extend(discs.iter().map(|p| path_key(p)));
            sets.push((file_stem(&list), discs));
        }
    }

    let mut games = Vec::new();
    for path in scan_dir(&cfg.games_dir, &["chd", "iso", "cue", "bin", "img"]) {
        // A disc an `.m3u` already names belongs to that title, not to its
        // own carousel entry.
        if listed.contains(&path_key(&path)) {
            continue;
        }
        let stem = file_stem(&path);
        let cover = resolve_media(cfg, &stem, &["box2d", "images", "thumbnails", "titles"], &covers);
        let disc = resolve_disc_scan(cfg, &stem, &disc_scans)
            .or_else(|| resolve_media(cfg, &stem, &["cartridges"], &disc_art));
        games.push(Game {
            discs: vec![path.clone()],
            title: file_stem(&path),
            cover_key: cover.as_ref().map(|c| c.to_string_lossy().into_owned()),
            cover,
            disc_key: disc.as_ref().map(|d| d.to_string_lossy().into_owned()),
            disc_art: disc,
            path,
        });
    }
    fold_disc_sets(&mut games);

    for (title, discs) in sets {
        let Some(first) = discs.first().cloned() else {
            continue;
        };
        let cover = resolve_media(cfg, &title, &["box2d", "images", "thumbnails", "titles"], &covers);
        let label = resolve_disc_scan(cfg, &title, &disc_scans)
            .or_else(|| resolve_media(cfg, &title, &["cartridges"], &disc_art));
        games.push(Game {
            discs,
            title,
            cover_key: cover.as_ref().map(|c| c.to_string_lossy().into_owned()),
            cover,
            disc_key: label.as_ref().map(|d| d.to_string_lossy().into_owned()),
            disc_art: label,
            path: first,
        });
    }
    games.sort_by(|a, b| a.title.cmp(&b.title));
    games
}

/// The disc number in a filename stem, with the title it belongs to.
///
/// Recognises the shapes multi-disc sets actually ship as: "Disc 2", "(Disc
/// 2 of 3)", "CD2", "CD 2", "Disk 2". The tag has to be a word of its own —
/// "abcd2" is not a disc — and the digits follow it, so "Disc 2 (Rev 1)"
/// reads as disc 2 rather than 21.
fn disc_tag(title: &str) -> Option<(String, usize)> {
    let lower = title.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    for word in ["disc", "disk", "cd"] {
        let mut from = 0;
        while let Some(found) = lower[from..].find(word) {
            let at = from + found;
            let before_ok = at == 0 || !bytes.get(at - 1).is_some_and(u8::is_ascii_alphanumeric);
            let after = at + word.len();
            let after_ok = !bytes.get(after).is_some_and(u8::is_ascii_alphabetic);
            if before_ok && after_ok {
                let mut i = after;
                while bytes
                    .get(i)
                    .is_some_and(|b| b.is_ascii_whitespace() || matches!(b, b'-' | b'_' | b'.' | b':'))
                {
                    i += 1;
                }
                let start = i;
                while bytes.get(i).is_some_and(u8::is_ascii_digit) {
                    i += 1;
                }
                if i > start {
                    // Two digits at most: a full number, not a year or a
                    // revision baked into the same tag.
                    if let Ok(number) = lower.get(start..i).unwrap_or_default().parse::<usize>() {
                        if (1..=99).contains(&number) {
                            let base = title
                                .get(..at)
                                .unwrap_or_default()
                                .trim()
                                .trim_end_matches(|c: char| {
                                    c.is_whitespace() || "-_.:([{,".contains(c)
                                })
                                .trim()
                                .to_string();
                            if !base.is_empty() {
                                return Some((base, number));
                            }
                        }
                    }
                }
            }
            from = at + word.len();
        }
    }
    None
}

/// Fold "Title (Disc 2)" into "Title (Disc 1)" so a multi-disc game shows
/// once. The discs ride along in number order for swapping mid-game, and the
/// lowest number is the entry's boot disc.
fn fold_disc_sets(games: &mut Vec<Game>) {
    let mut folded: Vec<Game> = Vec::new();
    let mut groups: Vec<(String, Vec<(usize, Game)>)> = Vec::new();
    for game in games.drain(..) {
        match disc_tag(&game.title) {
            Some((stem, number)) => {
                match groups
                    .iter_mut()
                    .find(|(s, _)| s.eq_ignore_ascii_case(&stem))
                {
                    Some((_, discs)) => discs.push((number, game)),
                    None => groups.push((stem, vec![(number, game)])),
                }
            }
            None => folded.push(game),
        }
    }
    for (stem, mut discs) in groups {
        discs.sort_by_key(|(number, _)| *number);
        let mut primary = discs.remove(0).1;
        primary.title = stem;
        for (_, extra) in discs {
            primary.discs.extend(extra.discs);
        }
        folded.push(primary);
    }
    *games = folded;
}

fn scan_bios(cfg: &Config) -> Vec<String> {
    scan_dir(&cfg.bios_dir, &["bin", "rom"])
        .iter()
        .map(|p| file_stem(p))
        .collect()
}

/// `.bin`/`.rom` files at least 256 KiB — the sizes real 3DO ROMs come in,
/// which is what the walkthrough checks a BIOS folder for.
fn bios_candidates(dir: &Path) -> Vec<PathBuf> {
    scan_dir(dir, &["bin", "rom"])
        .into_iter()
        .filter(|p| {
            p.metadata()
                .map(|m| m.len() >= 256 * 1024)
                .unwrap_or(false)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Audio device stream and gamepads (no video — egui owns the window).
//
// Desktop builds take both from SDL3. Android builds take pads from the Java
// shim in `android/` (Android key/generic-motion events, see android_pad.rs)
// and have no audio device yet, so the ring is drained and dropped.

#[cfg(not(target_os = "android"))]
struct SoundAndPads {
    _sdl: sdl3::Sdl,
    stream: Option<AudioStreamOwner>,
    gamepads: Option<sdl_pad::Gamepads>,
}

#[cfg(not(target_os = "android"))]
impl SoundAndPads {
    fn open() -> Option<Self> {
        let sdl = sdl3::init().ok()?;
        let audio = sdl.audio().ok();
        let spec = AudioSpec {
            freq: Some(retro3do::audio_ring::SAMPLE_RATE as i32),
            channels: Some(2),
            format: Some(AudioFormat::S16LE),
        };
        let stream = audio.and_then(|a| {
            a.open_playback_device(&spec)
                .and_then(|dev| dev.open_device_stream(Some(&spec)))
                .ok()
        });
        if let Some(s) = &stream {
            let _ = s.resume();
        }
        // The gamepad module opens the pads SDL reports and keeps up with pads
        // plugged in later; egui owns the window, so there is no SDL event loop
        // here to poll them with.
        let gamepads = sdl.gamepad().ok().map(sdl_pad::Gamepads::new);
        Some(SoundAndPads {
            _sdl: sdl,
            stream,
            gamepads,
        })
    }

    fn top_up(&self, emu: &Retro3do, buf: &mut [i16]) {
        if let Some(owner) = &self.stream {
            let queued = owner.queued_bytes().unwrap_or(0);
            let want = ((AUDIO_PAIRS_TARGET * AUDIO_BYTES_PER_PAIR - queued) / AUDIO_BYTES_PER_PAIR)
                as usize;
            if let Some(slice) = buf.get_mut(..want.min(AUDIO_PAIRS_PULL) * 2) {
                if !slice.is_empty() {
                    let _ = emu.audio().pull_interleaved(slice);
                    let _ = owner.put_data_i16(slice);
                }
            }
        }
    }

    /// The 3DO buttons held down on gamepad `index`.
    fn pad_mask(&self, index: usize) -> u32 {
        self.gamepads.as_ref().map_or(0, |g| g.mask(index))
    }

    fn count(&self) -> usize {
        self.gamepads.as_ref().map_or(0, |g| g.count())
    }

    /// The names SDL reports, for the launcher's status line.
    fn names(&self) -> Vec<String> {
        self.gamepads.as_ref().map_or_else(Vec::new, |g| g.names())
    }

    /// Looks for pads plugged in or pulled out since the last look.
    fn refresh(&mut self) {
        if let Some(g) = self.gamepads.as_mut() {
            if g.refresh() {
                eprintln!("[pad] connected: {:?}", g.names());
            }
        }
    }

    /// Put the saved mapping in force.
    fn set_bindings(&mut self, pairs: &[(String, String)]) {
        if let Some(g) = self.gamepads.as_mut() {
            g.set_bindings(pairs);
        }
    }

    /// The SDL button held down on a pad, by name: what the remap page
    /// listens for.
    fn pressed(&self, index: usize) -> Option<&'static str> {
        self.gamepads.as_ref().and_then(|g| g.pressed(index))
    }
}

/// Android pads: one physical gamepad, mapped onto console pad 0 beside the
/// keyboard, delivered by the Java shim (see `android_pad.rs`).
#[cfg(target_os = "android")]
struct SoundAndPads;

#[cfg(target_os = "android")]
impl SoundAndPads {
    fn open() -> Option<Self> {
        Some(SoundAndPads)
    }

    /// No audio device on Android yet: the ring is drained so it never holds
    /// stale samples, and nothing is played.
    fn top_up(&self, emu: &Retro3do, buf: &mut [i16]) {
        let _ = emu.audio().pull_interleaved(buf);
    }

    fn pad_mask(&self, index: usize) -> u32 {
        android_pad::pad_mask(index)
    }

    fn count(&self) -> usize {
        0
    }

    /// Android pads have no name to report.
    fn names(&self) -> Vec<String> {
        Vec::new()
    }

    /// The Java shim delivers Android pads as they happen, so there is
    /// nothing to look for here.
    fn refresh(&mut self) {}

    /// The Android shim's mapping is its own and fixed.
    fn set_bindings(&mut self, _pairs: &[(String, String)]) {}

    fn pressed(&self, _index: usize) -> Option<&'static str> {
        None
    }
}

/// The JNI shim in `android/src/lib.rs` hands the activity's key events here.
#[cfg(target_os = "android")]
pub fn android_pad_key(code: i32, down: bool) {
    android_pad::set_key(code, down);
}

/// ...and its generic motion events (sticks, d-pad hat, triggers).
#[cfg(target_os = "android")]
pub fn android_pad_axis(axis: i32, value: f32) {
    android_pad::set_axis(axis, value);
}

/// The 3DO buttons by the name the script harness and the settings page
/// use for them.
fn sdl_pad_names() -> [(&'static str, PadButton); 11] {
    [
        ("up", PadButton::Up),
        ("down", PadButton::Down),
        ("left", PadButton::Left),
        ("right", PadButton::Right),
        ("a", PadButton::A),
        ("b", PadButton::B),
        ("c", PadButton::C),
        ("play", PadButton::Play),
        ("stop", PadButton::Stop),
        ("l", PadButton::LeftShift),
        ("r", PadButton::RightShift),
    ]
}

fn keyboard_mask(ctx: &egui::Context) -> u32 {
    let mut mask = 0u32;
    ctx.input(|i| {
        for (key, pad) in [
            (egui::Key::ArrowUp, PadButton::Up),
            (egui::Key::ArrowDown, PadButton::Down),
            (egui::Key::ArrowLeft, PadButton::Left),
            (egui::Key::ArrowRight, PadButton::Right),
            (egui::Key::Z, PadButton::A),
            (egui::Key::X, PadButton::B),
            (egui::Key::C, PadButton::C),
            (egui::Key::Enter, PadButton::Play),
            (egui::Key::Space, PadButton::Play),
            (egui::Key::Backspace, PadButton::Stop),
            (egui::Key::A, PadButton::LeftShift),
            (egui::Key::S, PadButton::RightShift),
        ] {
            if i.key_down(key) {
                mask |= 1 << (pad as u32);
            }
        }
    });
    mask
}

// ---------------------------------------------------------------------------
// The application

#[derive(PartialEq, Clone, Copy)]
enum View {
    /// The machine itself: its picture when it runs, a placard when it does
    /// not. The console panel sits beside it.
    Machine,

    /// First run: where everything lives, and which BIOS to boot.
    Setup,

    /// The settings page, filling the window on its own.
    Settings,
}

/// Which part of the settings page is on show. The page is a page, not a
/// dialog: the sections live down the left and one of them is on screen.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsTab {
    Controller,
    Video,
    Paths,
    System,
}

/// The sections of the settings page, in the order they are listed.
const SETTINGS_TABS: [(SettingsTab, &str, &str); 4] = [
    (SettingsTab::Controller, "Controller", "the pad and its buttons"),
    (SettingsTab::Video, "Video", "how the picture is drawn"),
    (SettingsTab::Paths, "Paths", "where the library lives"),
    (SettingsTab::System, "System", "the BIOS and the CPU"),
];

/// The heading at the top of a settings section.
fn settings_head(ui: &mut egui::Ui, title: &str, blurb: &str) {
    ui.label(egui::RichText::new(title).size(22.0).strong());
    ui.add_space(4.0);
    ui.label(egui::RichText::new(blurb).color(TEXT_DIM));
    ui.add_space(18.0);
}

/// One card inside a settings section: its name, and whatever it holds.
fn settings_card(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::NONE
        .fill(CARD)
        .inner_margin(18)
        .corner_radius(8)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                egui::RichText::new(title)
                    .size(15.0)
                    .strong()
                    .color(TEXT_DIM),
            );
            ui.add_space(12.0);
            add(ui);
        });
}

struct GuiApp {
    cfg: Config,
    emu: Retro3do,
    io: Option<SoundAndPads>,
    games: Vec<Game>,
    bioses: Vec<String>,
    bios_choice: Option<String>,
    pal: bool,
    view: View,
    covers: HashMap<String, TextureHandle>,
    covers_rx: Receiver<(String, ColorImage)>,
    cover_tx: Sender<(String, PathBuf)>,
    /// The decode thread's answer channel, kept so the logo extraction can
    /// post its image the same way.
    result_tx: Sender<(String, ColorImage)>,
    pending_images: Vec<(String, ColorImage)>,
    frame_tex: Option<TextureHandle>,
    /// The FZ-1 in three dimensions: the model as loaded, the GPU renderer
    /// built on first paint, and what the drawer and disc are doing. The
    /// drawn console is the fallback when the model or the GPU is not there.
    /// The scene: the console first (the camera frames it), then the shelf it
    /// stands on and the pad in front of it.
    fz1_scene: Vec<fz1_3d::Model>,
    fz1: Option<fz1_3d::Fz1Renderer>,
    fz1_state: fz1_3d::Fz1State,
    fz1_tried: bool,
    /// The picture asked for the model's disc, by its file path, so it is
    /// decoded once per game rather than every frame.
    fz1_disc_art: Option<PathBuf>,
    /// Media paths already asked of the decode thread.
    art_asked: std::collections::HashSet<String>,
    /// The title the shelf last pointed at (the header names it).
    selected: Option<usize>,
    /// The spine and back-cover scans each game has, matched once per scan.
    spines: Vec<Option<PathBuf>>,
    backs: Vec<Option<PathBuf>>,
    /// How far the shelf has scrolled when it is wider than the window.
    shelf_scroll: f32,
    /// Whether hidden titles are on show (the shelf's own toggle), and
    /// whether the shelf should scroll to bring the selection into view —
    /// set when the pad moves it, since the pad cannot see where it went.
    show_hidden: bool,
    shelf_follow: bool,
    /// The pad buttons held last frame, how long the held direction has
    /// waited before repeating, and how long the on-screen pad map has left
    /// to show.
    pad_prev: u32,
    pad_hold: f32,
    pad_hint: f32,
    /// Seconds the machine has stood settled on the table. The shelf is held
    /// back until the unboxing has played out, so nothing is drawn over the
    /// reveal; see `SHELF_WAIT` and `SHELF_FADE`.
    settled_for: f32,
    /// The case being looked at in the upper view, and the game whose
    /// artwork is in the case model's slots.
    case_open: Option<usize>,
    /// Whether the case in the upper view stands open on its hinge. A case
    /// arrives shut with its wrap artwork showing; the lid only opens here.
    case_lid_open: bool,
    case_art_for: Option<usize>,
    /// Composed artwork coming back from worker threads: which slot, the
    /// key it was asked for under, and the RGBA pixels.
    slot_tx: Sender<(fz1_3d::ArtSlot, String, Vec<u8>)>,
    slot_rx: Receiver<(fz1_3d::ArtSlot, String, Vec<u8>)>,
    /// The platter picture that has actually reached the GPU.
    fz1_disc_uploaded: Option<PathBuf>,
    /// A disc on its way into the machine.
    load: Option<Load>,
    /// The opening show, 0 (sealed, about to spin) to 1 (done), and the
    /// box-away on close.
    intro: f32,
    /// Whether the presentation spin has finished, after which nothing but a
    /// hand turns the machine.
    spun: bool,
    /// When the player last touched anything: the overlay icons hide again
    /// three seconds later while a game is on screen.
    chrome_woke: Instant,
    closing: Option<f32>,
    close_now: bool,
    /// The launcher shown over a running game (which is paused under it).
    frontend: bool,
    /// The automation script (RETRO3DO_SCRIPT) and screenshots waiting to
    /// be written.
    script: Vec<(f32, String)>,
    shots: std::collections::VecDeque<String>,
    /// A pad button the script is holding down this frame (`pad:right`),
    /// taken by `pad_ui` the way a real pad's mask is.
    script_pad: u32,
    /// Opacity of the "drag to rotate" hint: 1 while it is new, draining to
    /// 0 once it has had its moment or been acted on. `orbit_seen` counts the
    /// seconds the machine has been on screen as a control, so the hint's
    /// moment is measured by when it could actually be read.
    orbit_hint: f32,
    orbit_seen: f32,
    /// Whether the machine's picture has anything in it yet: the console
    /// stays on screen until the core is actually drawing, then slides and
    /// fades out over about half a second (`console_fade` 1.0 = console
    /// fully on top, 0.0 = gone).
    picture_live: bool,
    console_fade: f32,
    /// The size of the field captured this repaint, if the core published
    /// one; `view_screen` reads it instead of asking for a frame itself.
    frame_dims: Option<(usize, usize)>,
    /// The walkthrough's step and the paths being typed into it, plus the
    /// directory picker (`picker` is the folder being browsed, `picker_step`
    /// which step's field it belongs to).
    setup_step: usize,
    setup_bios: String,
    setup_games: String,
    setup_media: String,
    picker: Option<PathBuf>,
    picker_step: usize,
    /// When the window opened.
    launched: Instant,
    /// How far the badge wallpaper has drifted, in points, and which way
    /// each axis is going: the drift turns round at `WALL_SPAN` rather than
    /// snapping back to nothing.
    wall_drift: f32,
    wall_drift_x: f32,
    wall_dir: egui::Vec2,
    /// Whether the model's drawer is out. The machine's own open/close button
    /// toggles it; powering on shuts it.
    fz1_tray_open: bool,
    /// The wheel's own zoom, kept apart from the drawer zoom so the two do
    /// not fight.
    fz1_user_zoom: f32,
    /// The disc in the machine, if any.
    game: Option<usize>,
    /// Which disc of `game`'s set is in the drive. Swapping moves this within
    /// the same title and leaves the machine running.
    disc_index: usize,
    /// When software last ejected the tray, so a game's "insert Disc 2" is
    /// answered once the tray has actually stayed open rather than in the
    /// middle of the driver's own mount-time eject.
    guest_tray_since: Option<Instant>,
    scaler: Option<gpu_scale::GpuScaler>,
    frame_rgba: Vec<u8>,
    /// Last texture presented to the view: repaints outnumber published
    /// fields, so frames without a new field keep drawing this.
    last_frame: Option<(egui::TextureId, egui::Vec2)>,
    audio_buf: Vec<i16>,
    disc_angle: f32,
    /// How fast the platter is turning, in radians a second. The spindle
    /// cuts power to stop it, so the disc coasts down instead of stopping
    /// dead.
    disc_speed: f32,
    status: String,
    last_nvram_save: Instant,
    /// Frame-interval instrumentation, kept only while RETRO3DO_FRAME_LOG is
    /// set: when the picture last reached the screen, when the machine last
    /// published a field, and the gaps gathered since the last report.
    present_last: Option<Instant>,
    field_last: Option<Instant>,
    present_gaps: Vec<f64>,
    field_gaps: Vec<f64>,
    /// Where the settings page came from, so the way back lands there, the
    /// section of it on show, and which 3DO button (an index into
    /// `sdl_pad::THREE_DO`) is waiting for a pad button to be pressed.
    settings_back: View,
    settings_tab: SettingsTab,
    rebind: Option<usize>,
    /// A scroll position the script harness has asked the settings page to
    /// jump to, taken once so the wheel still works the rest of the time.
    settings_scroll: Option<f32>,
    /// The folders being typed on the settings page, applied on the button.
    set_dirs: Option<(String, String, String)>,
}

/// Which title the platter wears: the disc that is in the drive, never the
/// one merely highlighted in the shelf. A disc that has just been ejected
/// stays on the platter while the spindle coasts down, and then rides out
/// on the open drawer, where it sits waiting to be taken or swapped.
fn platter_title(
    game: Option<usize>,
    disc_loaded: bool,
    spinning: bool,
    tray_open: bool,
) -> Option<usize> {
    game.filter(|_| disc_loaded || spinning || tray_open)
}

/// The steps of putting a disc in, each played at the machine's own pace.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum LoadStep {
    /// The drawer runs out and whatever disc was in it lifts away.
    Eject,
    /// The new disc comes down onto the platter.
    Place,
    /// The drawer shuts on it.
    Close,
    /// The platter comes up to speed; then the machine is switched on.
    SpinUp,
}

struct Load {
    game: usize,
    step: LoadStep,
    t: f32,
}

/// A spine for a title with no spine scan: the stock colour the case art
/// uses for it, and the name — without the scraper's region tags — set
/// down its length the way a shelf reads.
fn text_spine(painter: &egui::Painter, rect: egui::Rect, title: &str, alpha: f32) {
    let [r, g, b, _] = case_art::spine_paper(title).0;
    painter.rect_filled(rect, 1.0, egui::Color32::from_rgb(r, g, b).gamma_multiply(alpha));
    let name = case_art::clean_title(title).to_uppercase();
    let ink = egui::Color32::from_rgb(232, 232, 236).gamma_multiply(alpha);
    let mut size = (rect.width() * 0.74).clamp(8.0, 22.0);
    let galley = loop {
        let galley = painter.layout_no_wrap(name.clone(), egui::FontId::proportional(size), ink);
        if galley.size().x <= rect.height() - 14.0 || size <= 6.0 {
            break galley;
        }
        size -= 0.5;
    };
    // Turned a quarter clockwise: the text runs down the spine, its top
    // toward the right edge, centred both ways.
    let gs = galley.size();
    let at = egui::pos2(rect.center().x + gs.y * 0.5, rect.center().y - gs.x * 0.5);
    painter.add(egui::epaint::TextShape::new(at, galley, ink).with_angle(std::f32::consts::FRAC_PI_2));
}

/// The automation script: `secs:action[:arg]` entries split by `;`.
fn parse_script(text: &str) -> Vec<(f32, String)> {
    let mut out: Vec<(f32, String)> = text
        .split(';')
        .filter_map(|e| {
            let (at, action) = e.trim().split_once(':')?;
            Some((at.trim().parse().ok()?, action.trim().to_string()))
        })
        .collect();
    out.sort_by(|a, b| a.0.total_cmp(&b.0));
    out
}

impl GuiApp {
    fn new(cc: &eframe::CreationContext<'_>, cfg: Config) -> Self {
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = BG;
        visuals.window_fill = PANEL;
        cc.egui_ctx.set_visuals(visuals);
        // A handheld's 1080p landscape is dense in logical pixels; scale the
        // whole launcher up so tiles and text stay thumb-sized.
        #[cfg(target_os = "android")]
        cc.egui_ctx.set_pixels_per_point(1.75);
        #[cfg(not(target_os = "android"))]
        cc.egui_ctx.set_pixels_per_point(1.0);

        let games = scan_games(&cfg);
        let bioses = scan_bios(&cfg);
        let bios_choice = bioses
            .iter()
            .find(|b| b.contains("panafz10"))
            .or_else(|| bioses.first())
            .cloned();

        // Artwork decodes on one worker thread; the UI sends (key, path) jobs
        // and receives (key, image) results for the texture cache.
        let (job_tx, job_rx) = channel::<(String, PathBuf)>();
        let (tx, rx) = channel::<(String, ColorImage)>();
        let result_tx = tx.clone();
        std::thread::spawn(move || {
            for (key, path) in job_rx {
                if let Ok(img) = image::open(&path) {
                    let img = img.to_rgba8();
                    let (w, h) = (img.width() as usize, img.height() as usize);
                    if tx
                        .send((
                            key,
                            ColorImage::from_rgba_unmultiplied([w, h], img.as_raw()),
                        ))
                        .is_err()
                    {
                        return;
                    }
                }
            }
        });
        let queue_art = |job_tx: &Sender<(String, PathBuf)>, games: &[Game]| {
            for game in games {
                for pair in [
                    (&game.cover, &game.cover_key),
                    (&game.disc_art, &game.disc_key),
                ] {
                    if let (Some(p), Some(k)) = pair {
                        let _ = job_tx.send((k.clone(), p.clone()));
                    }
                }
            }
        };
        queue_art(&job_tx, &games);

        let (slot_tx, slot_rx) = channel();
        let mut app = Self {
            emu: Retro3do::new(),
            io: SoundAndPads::open(),
            games,
            bioses,
            bios_choice,
            pal: false,
            view: View::Machine,
            covers: HashMap::new(),
            covers_rx: rx,
            cover_tx: job_tx,
            result_tx,
            pending_images: Vec::new(),
            frame_tex: None,
            fz1_scene: Vec::new(),
            fz1: None,
            fz1_state: fz1_3d::Fz1State {
                zoom: 1.0,
                // A three-quarter view: the deck reads as an object, and the
                // power and eject buttons are still face on enough to hit.
                yaw: YAW0,
                pitch: 0.38,
                case_yaw: 0.35,
                case_pitch: 0.12,
                case_zoom: 1.0,
                ..Default::default()
            },
            fz1_tried: false,
            fz1_disc_art: None,
            art_asked: std::collections::HashSet::new(),
            selected: None,
            spines: Vec::new(),
            backs: Vec::new(),
            shelf_scroll: 0.0,
            show_hidden: false,
            shelf_follow: false,
            pad_prev: 0,
            pad_hold: 0.0,
            pad_hint: 0.0,
            settled_for: 0.0,
            case_open: None,
            case_lid_open: false,
            case_art_for: None,
            slot_tx,
            slot_rx,
            fz1_disc_uploaded: None,
            load: None,
            intro: 0.0,
            spun: false,
            chrome_woke: Instant::now(),
            closing: None,
            close_now: false,
            frontend: false,
            script: std::env::var("RETRO3DO_SCRIPT")
                .map(|s| parse_script(&s))
                .unwrap_or_default(),
            shots: std::collections::VecDeque::new(),
            script_pad: 0,
            orbit_hint: 1.0,
            orbit_seen: 0.0,
            picture_live: false,
            console_fade: 1.0,
            frame_dims: None,
            setup_step: 0,
            setup_bios: cfg.bios_dir.to_string_lossy().into_owned(),
            setup_games: cfg.games_dir.to_string_lossy().into_owned(),
            setup_media: cfg.media_dir.to_string_lossy().into_owned(),
            picker: None,
            picker_step: 0,
            launched: Instant::now(),
            wall_drift: 0.0,
            wall_drift_x: 0.0,
            wall_dir: egui::vec2(1.0, 1.0),
            fz1_tray_open: true,
            fz1_user_zoom: 1.0,
            game: None,
            disc_index: 0,
            guest_tray_since: None,
            scaler: None,
            frame_rgba: Vec::new(),
            last_frame: None,
            audio_buf: vec![0; AUDIO_PAIRS_PULL * 2],
            disc_angle: 0.0,
            disc_speed: 0.0,
            status: String::new(),
            last_nvram_save: Instant::now(),
            present_last: None,
            field_last: None,
            present_gaps: Vec::new(),
            field_gaps: Vec::new(),
            settings_back: View::Machine,
            settings_tab: SettingsTab::Controller,
            rebind: None,
            settings_scroll: None,
            set_dirs: None,
            cfg,
        };
        app.refresh_covers();
        app.apply_textures();
        // Powered off with an empty drawer, which is how a 3DO sits until
        // someone puts something in it.
        app.game = None;
        app.fz1_tray_open = false;
        // The saved settings put in force before anything runs.
        app.emu.set_cpu_scale_percent(app.cfg.cpu_scale);
        let bind = app.cfg.pad_bind.clone();
        if let Some(io) = app.io.as_mut() {
            io.set_bindings(&bind);
        }
        if app.bios_choice.is_none() {
            app.bios_choice = app.cfg.bios_choice.clone().or_else(|| app.bioses.first().cloned());
        }
        // The walkthrough appears when there is nothing remembered, and
        // again when what is remembered no longer has a BIOS to boot.
        if !app.cfg.configured || app.bioses.is_empty() {
            app.view = View::Setup;
        }
        // The recreated REAL card and the badge tile are baked in — no BIOS
        // needed — and the splash can hold on the card at once. The frame
        // the ROM itself draws still replaces the card when it arrives.
        app.pending_images
            .push((LOGO_KEY.to_string(), boot_logo::render()));
        app.pending_images
            .push((WALL_KEY.to_string(), boot_logo::badge()));
        // The real boot logo, pulled out of the chosen BIOS by the core
        // itself, arrives on the artwork channel like any other picture.
        app.extract_logo();
        // Nothing boots by itself: --boot and --play only point the shelf at
        // a title. The machine comes out of its box powered off and empty.
        app.selected = app.cfg.play.or(app.cfg.boot).filter(|&g| g < app.games.len());
        if app.view == View::Setup {
            app.intro = 1.0;
        }
        app
    }

    fn nvram_path(&self) -> PathBuf {
        self.cfg.saves_dir.join("nvram.bin")
    }

    fn save_nvram(&self) {
        if let Some(image) = self.emu.take_nvram() {
            let _ = std::fs::create_dir_all(&self.cfg.saves_dir);
            let _ = std::fs::write(self.nvram_path(), image);
        }
    }

    /// Re-match card artwork after the media-type dropdown changes, and queue
    /// decodes for any image not already in the texture cache.
    /// Box fronts for the library cards.
    fn refresh_covers(&mut self) {
        let exts = ["png", "jpg", "jpeg"];
        for (sub, into) in [("spine", &mut self.spines), ("boxback", &mut self.backs)] {
            let cands = scan_dir(&self.cfg.media_dir.join(sub), &exts);
            *into = self
                .games
                .iter()
                .map(|g| resolve_media(&self.cfg, &file_stem(&g.path), &[sub], &cands))
                .collect();
        }
        let cands = scan_dir(&self.cfg.media_dir.join("box2d"), &["png", "jpg", "jpeg"]);
        for g in &mut self.games {
            let m = resolve_media(&self.cfg, &file_stem(&g.path), &["box2d"], &cands);
            g.cover_key = m.as_ref().map(|c| c.to_string_lossy().into_owned());
            g.cover = m;
            if let (Some(k), Some(p)) = (&g.cover_key, &g.cover) {
                if !self.covers.contains_key(k) {
                    let _ = self.cover_tx.send((k.clone(), p.clone()));
                }
            }
        }
    }

    /// Put a disc in: the machine starts it, booting first if it is off and
    /// resetting into the new disc if it is already running.
    fn insert_disc(&mut self, index: usize) {
        self.save_nvram();
        let Some(game) = self.games.get(index) else {
            return;
        };
        let (path, title) = (game.path.clone(), game.title.clone());
        let was_running = self.emu.running();
        if was_running {
            self.emu.stop();
        }
        match self.emu.load_disc(path.to_string_lossy().as_ref()) {
            Ok(()) => {
                self.game = Some(index);
                self.disc_index = 0;
                self.guest_tray_since = None;
                self.status = format!("{title} inserted");
                self.view = View::Machine;
                self.fz1_tray_open = false;
                // A new disc is a new boot: the machine stays on show until
                // the core is drawing again.
                self.picture_live = false;
                if was_running {
                    self.emu.start();
                    self.emu.request_reset();
                }
            }
            Err(e) => {
                self.status = format!("{title}: {e}");
                if was_running {
                    self.emu.start();
                }
            }
        }
    }

    /// The discs of the title in the machine, in queue order.
    fn disc_queue(&self) -> &[PathBuf] {
        match self.game.and_then(|g| self.games.get(g)) {
            Some(game) => &game.discs,
            None => &[],
        }
    }

    /// Put disc `index` of the current title in the drive. This is a tray
    /// swap, not a boot: the machine keeps running, with its RAM, its
    /// progress and the game's own "insert Disc 2" prompt intact.
    fn swap_disc(&mut self, index: usize) {
        let Some(path) = self.disc_queue().get(index).cloned() else {
            return;
        };
        if self.game.is_none() || index == self.disc_index {
            return;
        }
        let title = self
            .game
            .and_then(|g| self.games.get(g))
            .map(|g| g.title.clone())
            .unwrap_or_default();
        let total = self.disc_queue().len();
        self.save_nvram();
        // The core's insert is eject-and-insert without a reset; the machine
        // is never stopped here, so nothing it holds is lost.
        match self.emu.load_disc(path.to_string_lossy().as_ref()) {
            Ok(()) => {
                self.disc_index = index;
                self.guest_tray_since = None;
                self.status = format!("{title} — disc {} of {total} in the drive", index + 1);
            }
            Err(e) => {
                self.status = format!("disc {}: {e}", index + 1);
            }
        }
    }

    /// The next disc in the queue, for the guest's own disc request and the
    /// next-disc control.
    fn swap_next_disc(&mut self) {
        let next = self.disc_index + 1;
        if next < self.disc_queue().len() {
            self.swap_disc(next);
        }
    }

    /// Watch the drive's tray. Software ejects it to ask for the next disc;
    /// a game that is merely mounting ejects and spins up again at once, so
    /// only a tray left open is taken as the request.
    fn poll_disc_request(&mut self) {
        if !self.emu.running() || self.game.is_none() || self.fz1_tray_open {
            self.guest_tray_since = None;
            return;
        }
        if self.emu.disc_tray_open() {
            let since = *self.guest_tray_since.get_or_insert_with(Instant::now);
            let more = self.disc_index + 1 < self.disc_queue().len();
            if since.elapsed() > Duration::from_millis(1500) && more {
                self.swap_next_disc();
            }
        } else {
            self.guest_tray_since = None;
        }
    }

    /// The drive can empty without the launcher doing it: the BIOS ejects
    /// the disc, or a guest asks for the next one of a set. What the machine
    /// shows has to follow the drive, not the last thing the launcher did,
    /// so it never claims a disc that is not in it.
    fn sync_disc_state(&mut self) {
        if self.game.is_some() && self.emu.running() && !self.emu.disc_loaded() {
            self.status = "disc out — the BIOS is waiting".to_string();
        }
    }

    fn power_on(&mut self) {
        if !self.emu.bios_loaded() {
            let Some(bios) = &self.bios_choice else {
                self.status = "no BIOS selected — pick one in the library".to_string();
                return;
            };
            let path = self.cfg.bios_dir.join(format!("{bios}.bin"));
            let path = if path.exists() {
                path
            } else {
                self.cfg.bios_dir.join(bios)
            };
            if let Err(e) = self.emu.load_bios(path.to_string_lossy().as_ref()) {
                self.status = format!("bios {bios}: {e}");
                return;
            }
            if let Ok(image) = std::fs::read(self.nvram_path()) {
                let _ = self.emu.restore_nvram(&image);
            }
        }
        if self.pal {
            self.emu.set_region(Region::Pal);
        }
        let was_running = self.emu.running();
        if !was_running {
            // A machine coming up cold starts at the reset vector. A machine
            // that was stopped resumes where it left off, and the BIOS would
            // never re-run to pick the disc in the drive up. Asked for before
            // the thread starts, so the reset lands before the first field.
            self.emu.request_reset();
        }
        self.emu.set_paused(false);
        self.emu.start();
        self.picture_live = false;
        self.frontend = false;
        self.fz1_tray_open = false;
        self.status.clear();
        self.view = View::Machine;
    }

    /// Stop the machine. The disc stays in; the panel shows it powered down.
    fn power_off(&mut self) {
        self.emu.stop();
        self.save_nvram();
        self.picture_live = false;
        self.view = View::Machine;
    }

    /// The gamepads SDL reports, for the settings page.
    fn pad_names(&self) -> Vec<String> {
        self.io.as_ref().map_or_else(Vec::new, |io| io.names())
    }

    /// The pad drives the launcher itself, not only the game: the same
    /// button mask the console is fed is read here first while the launcher
    /// is on screen, so a phone with a controller never needs the touch
    /// screen. See `PAD_MAP` for what each button does.
    fn pad_ui(&mut self, ctx: &egui::Context) {
        // Only while the launcher is what is on screen: a running game has
        // the pad to itself.
        let busy = self.load.is_some() || self.closing.is_some() || self.intro < 1.0;
        let launcher = self.view == View::Machine && (self.frontend || !self.emu.running());
        if busy || !launcher {
            self.pad_prev = 0;
            return;
        }
        let mut mask = keyboard_mask(ctx) | std::mem::take(&mut self.script_pad);
        // Enter/Space swing the lid of the case in view. They are also the
        // keyboard's PLAY, so while a case is open consume that bit for as
        // long as either key is held - otherwise the next frame reads the
        // same hold as a fresh press and launches the disc. The pad's own
        // PLAY is untouched and still puts the disc in.
        if self.case_open.is_some() {
            let lid_key = |i: &egui::InputState| {
                i.key_down(egui::Key::Enter) || i.key_down(egui::Key::Space)
            };
            if ctx.input(lid_key) {
                mask &= !(1u32 << (PadButton::Play as u32));
            }
            if ctx.input(|i| {
                i.key_pressed(egui::Key::Enter) || i.key_pressed(egui::Key::Space)
            }) {
                self.case_lid_open = !self.case_lid_open;
            }
        }
        if let Some(io) = self.io.as_mut() {
            mask |= io.pad_mask(self.cfg.pad_primary);
        }
        let dt = ctx.input(|i| i.stable_dt).clamp(0.0, 0.1);
        let bit = |b: PadButton| 1u32 << (b as u32);
        let fresh = mask & !self.pad_prev;
        self.pad_prev = mask;
        if mask != 0 {
            // Anything pressed brings the map back up for a moment.
            self.pad_hint = 4.0;
        }
        // A held direction repeats, after a pause, so a long shelf can be
        // walked without pumping the stick.
        let dirs = bit(PadButton::Left) | bit(PadButton::Right) | bit(PadButton::Up) | bit(PadButton::Down);
        let mut step = fresh & dirs;
        if mask & dirs == 0 {
            self.pad_hold = 0.0;
        } else {
            if step != 0 {
                self.pad_hold = -0.35;
            }
            self.pad_hold += dt;
            if self.pad_hold >= 0.09 {
                self.pad_hold = 0.0;
                step |= mask & dirs;
            }
        }
        let case = self.case_open;
        // Left and right: along the shelf, or round the case when one is
        // open and its own turn is what the stick is for.
        if case.is_some() {
            let spin = f32::from(step & bit(PadButton::Right) != 0)
                - f32::from(step & bit(PadButton::Left) != 0);
            let tilt = f32::from(step & bit(PadButton::Down) != 0)
                - f32::from(step & bit(PadButton::Up) != 0);
            self.fz1_state.case_yaw += spin * 0.12;
            self.fz1_state.case_pitch = (self.fz1_state.case_pitch + tilt * 0.08).clamp(-1.2, 1.2);
        } else {
            let move_by = i32::from(step & bit(PadButton::Right) != 0)
                - i32::from(step & bit(PadButton::Left) != 0);
            if move_by != 0 {
                self.move_selection(move_by);
            }
            // Up opens the case of whatever the shelf points at; down turns
            // the machine on its stand instead.
            if step & bit(PadButton::Up) != 0 {
                if let Some(i) = self.selected.or_else(|| self.visible_games().first().copied()) {
                    self.open_case(i);
                }
            }
            if step & bit(PadButton::Down) != 0 {
                self.fz1_state.pitch = (self.fz1_state.pitch + 0.08).clamp(-1.2, 1.2);
            }
        }
        // A: the case, then the disc. B: back out of it.
        if fresh & bit(PadButton::A) != 0 {
            match (case, self.selected) {
                (Some(i), _) => self.start_load(i),
                (None, Some(i)) => self.open_case(i),
                (None, None) => {
                    if let Some(i) = self.visible_games().first().copied() {
                        self.open_case(i);
                    }
                }
            }
        }
        if fresh & bit(PadButton::B) != 0 {
            if case.is_some() {
                self.case_open = None;
                self.case_lid_open = false;
            } else if self.emu.running() {
                self.frontend = false;
            }
        }
        // P: put the disc in from wherever the shelf stands.
        if fresh & bit(PadButton::Play) != 0 {
            if let Some(i) = case.or(self.selected) {
                self.start_load(i);
            }
        }
        // C: hide the title, or put it back. L and R: in and out.
        if fresh & bit(PadButton::C) != 0 {
            if let Some(i) = case.or(self.selected) {
                self.toggle_hidden(i);
                if !self.show_hidden && self.is_hidden(i) {
                    self.move_selection(1);
                }
            }
        }
        let zoom = f32::from(mask & bit(PadButton::RightShift) != 0)
            - f32::from(mask & bit(PadButton::LeftShift) != 0);
        if zoom != 0.0 {
            if case.is_some() {
                self.fz1_state.case_zoom = (self.fz1_state.case_zoom + zoom * dt * 1.4).clamp(0.5, 3.0);
            } else {
                self.fz1_user_zoom = (self.fz1_user_zoom + zoom * dt * 1.4).clamp(0.5, 3.0);
            }
        }
        // X: put the machine away and close the launcher.
        if fresh & bit(PadButton::Stop) != 0 {
            self.begin_close();
        }
        if mask != 0 {
            ctx.request_repaint();
        }
    }

    /// Walk the shelf's selection along by `by` places, wrapping at the
    /// ends, and ask the shelf to scroll until it is in view.
    fn move_selection(&mut self, by: i32) {
        let shown = self.visible_games();
        if shown.is_empty() {
            return;
        }
        let at = self
            .selected
            .and_then(|sel| shown.iter().position(|&i| i == sel))
            .map_or(0, |p| {
                (p as i32 + by).rem_euclid(shown.len() as i32) as usize
            });
        if let Some(&index) = shown.get(at) {
            self.selected = Some(index);
            self.shelf_follow = true;
            if self.case_open.is_some() {
                self.open_case(index);
            }
        }
    }

    fn push_pads(&mut self, ctx: &egui::Context) {
        let mut connected = 0u32;
        // Console pad 0 is the keyboard plus the first gamepad, so a gamepad
        // always works even without mapping keys. Extra gamepads chain behind.
        let keys = keyboard_mask(ctx);
        let mut mask = keys;
        let primary = self.cfg.pad_primary;
        if let Some(io) = self.io.as_mut() {
            io.refresh();
            mask |= io.pad_mask(primary);
            // The chosen pad is console pad 0 with the keyboard; the rest
            // chain behind it in the order SDL reports them.
            let mut idx = 1u32;
            for i in 0..io.count() {
                if i == primary || idx >= MAX_PADS {
                    continue;
                }
                self.emu.pads().set_buttons(idx, io.pad_mask(i));
                self.emu.pads().set_connected(idx, true);
                connected = idx + 1;
                idx += 1;
            }
        }
        self.emu.pads().set_buttons(0, mask);
        self.emu.pads().set_connected(0, true);
        connected = connected.max(1);
        for i in connected..MAX_PADS {
            self.emu.pads().set_connected(i, false);
        }
    }


    /// One row of the walkthrough: a label, a path that can be typed, and a
    /// browse button that opens the folder picker for this step.
    fn setup_path_row(&mut self, ui: &mut egui::Ui, label: &str, step: usize) {
        ui.horizontal(|ui| {
            ui.label(label);
            let field = match step {
                1 => &mut self.setup_bios,
                2 => &mut self.setup_games,
                _ => &mut self.setup_media,
            };
            ui.add(egui::TextEdit::singleline(field).desired_width(340.0));
            if ui.button("browse…").clicked() {
                let start = field.trim();
                let start = if start.is_empty() {
                    start_dir()
                } else {
                    PathBuf::from(start)
                };
                self.picker = Some(if start.is_dir() {
                    start
                } else {
                    start.parent().map(Path::to_path_buf).unwrap_or(start)
                });
                self.picker_step = step;
            }
        });
    }

    /// First run: welcome, then where the BIOS, the discs and the artwork
    /// live. Everything it collects is written to the settings file, so it
    /// only appears again if that file goes away or the BIOS folder empties.
    fn view_setup(&mut self, ui: &mut egui::Ui) {
        let rect = ui.available_rect_before_wrap();
        let card = egui::Rect::from_center_size(
            rect.center(),
            egui::vec2(rect.width().min(620.0), rect.height().min(440.0)),
        );
        ui.painter().rect(
            card,
            10.0,
            CARD,
            egui::Stroke::new(1.0, TEXT_DIM),
            StrokeKind::Inside,
        );
        ui.scope_builder(
            egui::UiBuilder::new().max_rect(card.shrink(20.0)),
            |ui| {
                ui.vertical(|ui| {
                    ui.heading("Set up RETRO 3DO");
                    ui.add_space(4.0);
                    let bios_dir = PathBuf::from(self.setup_bios.trim());
                    let games_dir = PathBuf::from(self.setup_games.trim());
                    let bioses = bios_candidates(&bios_dir);
                    let mut can_next = true;
                    match self.setup_step {
                        0 => {
                            ui.add_space(10.0);
                            ui.label(
                                "Welcome. A few quick questions and the machine is \
                                 yours: where your BIOS image lives, where your \
                                 discs are, and (if you like) where their artwork \
                                 is kept.",
                            );
                            ui.add_space(10.0);
                            ui.label(
                                egui::RichText::new(
                                    "You will need a real 3DO BIOS ROM — the emulator \
                                     cannot boot retail discs without one.",
                                )
                                .small()
                                .color(TEXT_DIM),
                            );
                        }
                        1 => {
                            ui.label(
                                "Where is your 3DO BIOS? It is the ROM dumped from a \
                                 real console — a .bin or .rom file around 1 MB.",
                            );
                            ui.add_space(8.0);
                            self.setup_path_row(ui, "bios folder", 1);
                            ui.add_space(6.0);
                            if bioses.is_empty() {
                                can_next = false;
                                ui.label(
                                    egui::RichText::new(
                                        "no BIOS images here — point me at the folder \
                                         that holds one",
                                    )
                                    .small()
                                    .color(egui::Color32::from_rgb(230, 110, 110)),
                                );
                            } else {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "{} BIOS image(s) found: {}",
                                        bioses.len(),
                                        bioses
                                            .iter()
                                            .map(|p| file_stem(p))
                                            .collect::<Vec<_>>()
                                            .join(", ")
                                    ))
                                    .small()
                                    .color(egui::Color32::from_rgb(110, 210, 130)),
                                );
                            }
                        }
                        2 => {
                            ui.label(
                                "Where are your games? Disc images (.chd, .iso, \
                                 .cue) are read straight from this folder.",
                            );
                            ui.add_space(8.0);
                            self.setup_path_row(ui, "games folder", 2);
                            ui.add_space(6.0);
                            if !games_dir.is_dir() {
                                can_next = false;
                                ui.label(
                                    egui::RichText::new("that folder does not exist")
                                        .small()
                                        .color(egui::Color32::from_rgb(230, 110, 110)),
                                );
                            } else {
                                let discs =
                                    scan_dir(&games_dir, &["chd", "iso", "cue", "bin", "img"])
                                        .len();
                                ui.label(
                                    egui::RichText::new(if discs > 0 {
                                        format!("{discs} disc image(s) found")
                                    } else {
                                        "folder found — no discs in it yet, that is fine"
                                            .to_string()
                                    })
                                    .small()
                                    .color(TEXT_DIM),
                                );
                            }
                        }
                        3 => {
                            ui.label(
                                "Where is the artwork? Box fronts, backs, marquees \
                                 and disc labels are read from here. Optional — \
                                 skip it and the library just stays plain.",
                            );
                            ui.add_space(8.0);
                            self.setup_path_row(ui, "artwork", 3);
                            ui.add_space(6.0);
                            let media = PathBuf::from(self.setup_media.trim());
                            ui.label(
                                egui::RichText::new(if media.join("box2d").is_dir() {
                                    "covers found"
                                } else {
                                    "no box2d folder here — artwork simply stays blank"
                                })
                                .small()
                                .color(TEXT_DIM),
                            );
                        }
                        _ => {
                            ui.label("All set. This is what the machine will use:");
                            ui.add_space(8.0);
                            for (label, path) in [
                                ("bios", bios_dir.clone()),
                                ("games", games_dir.clone()),
                                (
                                    "artwork",
                                    PathBuf::from(self.setup_media.trim()),
                                ),
                            ] {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        egui::RichText::new(format!("{label:>7}")).monospace(),
                                    );
                                    ui.label(
                                        egui::RichText::new(if path.as_os_str().is_empty() {
                                            "—".to_string()
                                        } else {
                                            path.display().to_string()
                                        })
                                        .small()
                                        .color(TEXT_DIM),
                                    );
                                });
                            }
                            if bioses.len() > 1 {
                                ui.add_space(8.0);
                                ui.label("Which BIOS should it boot?");
                                for b in &bioses {
                                    let name = file_stem(b);
                                    if ui
                                        .selectable_label(
                                            self.bios_choice.as_deref() == Some(name.as_str()),
                                            &name,
                                        )
                                        .clicked()
                                    {
                                        self.bios_choice = Some(name.clone());
                                    }
                                }
                            }
                        }
                    }

                    ui.add_space(14.0);
                    ui.horizontal(|ui| {
                        if self.setup_step > 0 && ui.button("back").clicked() {
                            self.setup_step -= 1;
                        }
                        if self.setup_step == 3 && ui.button("skip").clicked() {
                            self.setup_media.clear();
                            self.setup_step += 1;
                        }
                        let last = self.setup_step >= 4;
                        let label = if last {
                            "finish"
                        } else if self.setup_step == 0 {
                            "get started"
                        } else {
                            "next"
                        };
                        if ui
                            .add_enabled(can_next, egui::Button::new(label))
                            .clicked()
                        {
                            if last {
                                self.finish_setup();
                            } else {
                                self.setup_step += 1;
                            }
                        }
                        ui.label(
                            egui::RichText::new(format!("step {} of 5", self.setup_step + 1))
                                .small()
                                .color(TEXT_DIM),
                        );
                    });
                });
            },
        );
    }

    /// The folder browser the walkthrough opens for each path it asks for.
    /// Directories only: click one to descend into it, "use this folder"
    /// takes the one being browsed.
    fn dir_picker_overlay(&mut self, ctx: &egui::Context) {
        let Some(current) = self.picker.clone() else {
            return;
        };
        egui::Area::new(egui::Id::new("dir-picker"))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                let screen = ctx.content_rect();
                ui.painter()
                    .rect_filled(screen, 0.0, egui::Color32::from_black_alpha(190));
                let card = egui::Rect::from_center_size(
                    screen.center(),
                    egui::vec2(screen.width().min(560.0), screen.height().min(430.0)),
                );
                ui.painter().rect(
                    card,
                    10.0,
                    PANEL,
                    egui::Stroke::new(1.0, TEXT_DIM),
                    StrokeKind::Inside,
                );
                ui.scope_builder(
                    egui::UiBuilder::new().max_rect(card.shrink(16.0)),
                    |ui| {
                        ui.label(egui::RichText::new(current.display().to_string()).monospace());
                        ui.add_space(6.0);
                        let mut dirs: Vec<PathBuf> = Vec::new();
                        if let Ok(read) = std::fs::read_dir(&current) {
                            for entry in read.flatten() {
                                let path = entry.path();
                                if path.is_dir()
                                    && !entry
                                        .file_name()
                                        .to_string_lossy()
                                        .starts_with('.')
                                {
                                    dirs.push(path);
                                }
                            }
                        }
                        dirs.sort();
                        egui::ScrollArea::vertical()
                            .max_height(card.height() - 110.0)
                            .show(ui, |ui| {
                                if let Some(parent) = current.parent() {
                                    if ui.selectable_label(false, "..  (up)").clicked() {
                                        self.picker = Some(parent.to_path_buf());
                                    }
                                }
                                for dir in dirs {
                                    let name = dir
                                        .file_name()
                                        .map(|n| n.to_string_lossy().into_owned())
                                        .unwrap_or_default();
                                    if ui.selectable_label(false, name).clicked() {
                                        self.picker = Some(dir);
                                    }
                                }
                            });
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui.button("use this folder").clicked() {
                                let chosen = current.to_string_lossy().into_owned();
                                match self.picker_step {
                                    1 => self.setup_bios = chosen,
                                    2 => self.setup_games = chosen,
                                    // The settings page's own three fields.
                                    11..=13 => {
                                        if let Some(d) = self.set_dirs.as_mut() {
                                            match self.picker_step {
                                                11 => d.0 = chosen,
                                                12 => d.1 = chosen,
                                                _ => d.2 = chosen,
                                            }
                                        }
                                    }
                                    _ => self.setup_media = chosen,
                                }
                                self.picker = None;
                            }
                            if ui.button("cancel").clicked() {
                                self.picker = None;
                            }
                        });
                    },
                );
            });
    }

    /// Both covers, opened up side by side over the picture. A click puts
    /// them away again.
    /// Pull the boot logo out of the chosen BIOS on a worker thread and send
    /// it to the texture cache like any other artwork.
    fn extract_logo(&self) {
        let Some(bios) = self.bios_choice.clone() else {
            return;
        };
        let mut path = self.cfg.bios_dir.join(format!("{bios}.bin"));
        if !path.exists() {
            path = self.cfg.bios_dir.join(&bios);
        }
        let tx = self.result_tx.clone();
        std::thread::spawn(move || {
            if let Some(img) = boot_logo::extract(&path) {
                let _ = tx.send((LOGO_KEY.to_string(), img));
            }
        });
    }

    /// Remember the answers and open the machine.
    fn finish_setup(&mut self) {
        let games_dir = PathBuf::from(self.setup_games.trim());
        self.cfg.bios_dir = PathBuf::from(self.setup_bios.trim());
        self.cfg.saves_dir = games_dir
            .parent()
            .map(|p| p.join("saves"))
            .unwrap_or_else(|| games_dir.join("saves"));
        let media = PathBuf::from(self.setup_media.trim());
        self.cfg.media_dir = if media.as_os_str().is_empty() {
            games_dir
                .parent()
                .map(|p| p.join("media"))
                .unwrap_or_else(|| games_dir.join("media"))
        } else {
            media
        };
        self.cfg.games_dir = games_dir;
        self.games = scan_games(&self.cfg);
        self.bioses = scan_bios(&self.cfg);
        if self
            .bios_choice
            .as_ref()
            .is_none_or(|b| !self.bioses.contains(b))
        {
            self.bios_choice = self
                .bioses
                .iter()
                .find(|b| b.contains("panafz10"))
                .or_else(|| self.bioses.first())
                .cloned();
        }
        self.refresh_covers();
        self.extract_logo();
        self.cfg.bios_choice = self.bios_choice.clone();
        match self.cfg.save_settings() {
            Ok(()) => self.status = "setup saved".to_string(),
            Err(e) => self.status = format!("could not save setup: {e}"),
        }
        self.view = View::Machine;
    }

    /// A game's picture from one of the media sets (`box2d`, `boxback`,
    /// `marquee`, `cartridges`…), by path.
    /// The disc-label picture: the art that belongs on a disc — the game's
    /// own scan where the media folder has one.
    fn disc_label(&self, game: &Game) -> Option<PathBuf> {
        game.disc_art.clone()
    }

    /// The extracted boot logo, letterboxed at `fill` of the area's height.
    /// Returns whether it painted — it is only there once the extraction
    /// thread has sent it in.
    fn paint_logo(&self, ui: &egui::Ui, area: egui::Rect, fill: f32) -> bool {
        let Some(tex) = self.covers.get(LOGO_KEY) else {
            return false;
        };
        let size = tex.size_vec2();
        let scale = (area.height() * fill / size.y.max(1.0)).min(area.width() * 0.9 / size.x.max(1.0));
        let dst = egui::Rect::from_center_size(area.center(), size * scale);
        ui.painter().image(
            tex.id(),
            dst,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
        true
    }

    /// The texture for a media path, asking the decode thread for it the
    /// first time it is wanted.
    fn art_texture(&mut self, path: &Path) -> Option<egui::TextureId> {
        let key = path.to_string_lossy().into_owned();
        if let Some(tex) = self.covers.get(&key) {
            return Some(tex.id());
        }
        if self.art_asked.insert(key.clone()) {
            let _ = self.cover_tx.send((key, path.to_path_buf()));
        }
        None
    }

    /// What the disc on the platter wears: the game's own disc art, never
    /// its cover, or a drawn platter for a title with no disc art.
    fn platter_art(&self, index: usize) -> Option<PathBuf> {
        let game = self.games.get(index)?;
        self.disc_label(game)
            .or_else(|| Some(PathBuf::from(format!("title:{}", game.title))))
    }

    /// Ask a worker for the platter picture; it comes back on `slot_rx`.
    /// Whatever the slot showed until now is stale, so the part it dresses
    /// hides until the new picture lands.
    fn request_platter(&mut self, art: PathBuf) {
        if let Some(r) = self.fz1.as_mut() {
            r.clear_art(fz1_3d::ArtSlot::Platter);
        }
        let tx = self.slot_tx.clone();
        std::thread::spawn(move || {
            let key = art.to_string_lossy().into_owned();
            let rgba = match key.strip_prefix("title:") {
                Some(title) => case_art::disc(&case_art::CaseArt {
                    title: title.to_string(),
                    ..Default::default()
                })
                .as_raw()
                .to_vec(),
                None => {
                    // The game's own scan, cut the way the case's disc is.
                    case_art::disc(&case_art::CaseArt {
                        disc: Some(art.clone()),
                        ..Default::default()
                    })
                    .as_raw()
                    .to_vec()
                }
            };
            let _ = tx.send((fz1_3d::ArtSlot::Platter, key, rgba));
        });
    }

    /// Ask a worker to dress the case for `index`: the printed sleeve and
    /// the disc in it. The slot keeps another game's sleeve until then, so
    /// the case stays out of sight rather than wearing the wrong cover.
    fn request_case_art(&mut self, index: usize) {
        if let Some(r) = self.fz1.as_mut() {
            r.clear_art(fz1_3d::ArtSlot::Wrap);
        }
        let Some(game) = self.games.get(index) else {
            return;
        };
        let art = case_art::CaseArt {
            title: game.title.clone(),
            front: game.cover.clone(),
            back: self.backs.get(index).cloned().flatten(),
            spine: self.spines.get(index).cloned().flatten(),
            disc: self.disc_label(game),
            discs: game.discs.len().max(1),
        };
        let tx = self.slot_tx.clone();
        let key = format!("case:{index}");
        std::thread::spawn(move || {
            let wrap = case_art::wrap(&art);
            let _ = tx.send((fz1_3d::ArtSlot::Wrap, key, wrap.as_raw().to_vec()));
        });
        // The disc inside the case wears the same label as the machine's
        // platter. Dress that slot too, unless a game is actually running and
        // the platter belongs to it.
        if self.game.is_none() {
            if let Some(art) = self.platter_art(index) {
                self.fz1_disc_art = Some(art.clone());
                self.request_platter(art);
            }
        }
    }

    /// Hand finished artwork to the renderer, if it is still what is wanted.
    fn take_slot_art(&mut self, state: &eframe::egui_wgpu::RenderState) {
        while let Ok((slot, key, rgba)) = self.slot_rx.try_recv() {
            let wanted = match slot {
                fz1_3d::ArtSlot::Platter => self
                    .fz1_disc_art
                    .as_ref()
                    .is_some_and(|p| p.to_string_lossy() == key),
                _ => self.case_art_for.is_some_and(|i| key == format!("case:{i}")),
            };
            if !wanted {
                continue;
            }
            if let Some(r) = self.fz1.as_mut() {
                r.set_art(state, slot, &rgba);
                if slot == fz1_3d::ArtSlot::Platter {
                    self.fz1_disc_uploaded = self.fz1_disc_art.clone();
                }
            }
        }
    }

    /// Whether a title is hidden: the shelf leaves it out until the hidden
    /// ones are put back on show.
    fn is_hidden(&self, index: usize) -> bool {
        self.games
            .get(index)
            .is_some_and(|g| self.cfg.hidden.contains(&file_stem(&g.path)))
    }

    /// The titles the shelf shows, in shelf order.
    fn visible_games(&self) -> Vec<usize> {
        (0..self.games.len())
            .filter(|&i| self.show_hidden || !self.is_hidden(i))
            .collect()
    }

    /// Hide a title, or put it back. The flag lives with the rest of the
    /// launcher's settings, so it survives the next run.
    fn toggle_hidden(&mut self, index: usize) {
        let Some(stem) = self.games.get(index).map(|g| file_stem(&g.path)) else {
            return;
        };
        if !self.cfg.hidden.remove(&stem) {
            self.cfg.hidden.insert(stem);
            // A title that has just gone cannot stay the one on show.
            if !self.show_hidden && self.case_open == Some(index) {
                self.case_open = None;
            }
        }
        if let Err(e) = self.cfg.save_settings() {
            self.status = format!("settings not saved: {e}");
        }
    }

    /// The drawn spine for a title the media folder has no spine scan of:
    /// the generic 3DO card, composed once on a worker and kept as a
    /// texture like any other piece of artwork.
    fn drawn_spine_texture(&mut self, index: usize) -> Option<egui::TextureId> {
        let title = self.games.get(index)?.title.clone();
        let key = format!("spine:{title}");
        if let Some(tex) = self.covers.get(&key) {
            return Some(tex.id());
        }
        if self.art_asked.insert(key.clone()) {
            let tx = self.result_tx.clone();
            std::thread::spawn(move || {
                let img = case_art::generic_spine(GEN_SPINE_W, GEN_SPINE_H, &title);
                let _ = tx.send((key, img));
            });
        }
        None
    }

    /// Open a game's case in the upper view.
    fn open_case(&mut self, index: usize) {
        if self.games.get(index).is_none() {
            return;
        }
        self.selected = Some(index);
        if self.case_open != Some(index) {
            self.fz1_state.case_yaw = 0.35;
            self.fz1_state.case_pitch = 0.12;
            self.fz1_state.case_zoom = 1.0;
            // It comes off the shelf as a sealed case: the wrap is what you
            // see until the lid is opened from here.
            self.case_lid_open = false;
        }
        self.case_open = Some(index);
    }

    /// Start putting `index` in the machine: the drawer runs out, whatever is
    /// in it comes out, the new disc goes down, the drawer shuts, the platter
    /// spins up and the machine comes on. The same path whether the machine
    /// is empty or a game is already running in it.
    fn start_load(&mut self, index: usize) {
        if self.games.get(index).is_none() || self.closing.is_some() {
            return;
        }
        self.selected = Some(index);
        self.case_open = None;
        self.case_lid_open = false;
        self.frontend = false;
        // Whatever is running stops now; its disc comes out with the drawer.
        if self.emu.running() {
            self.emu.set_paused(false);
            self.emu.stop();
            self.save_nvram();
        }
        if self.emu.disc_loaded() {
            self.emu.eject_disc();
        }
        self.picture_live = false;
        self.fz1_tray_open = true;
        self.load = Some(Load {
            game: index,
            step: LoadStep::Eject,
            t: 0.0,
        });
        // Get the new disc's picture decoding while the drawer runs out.
        if let Some(art) = self.platter_art(index) {
            if self.game.is_none() && self.fz1_disc_art.as_ref() != Some(&art) {
                self.fz1_disc_art = Some(art.clone());
                self.request_platter(art);
            }
        }
    }

    /// The machine's own OPEN/CLOSE. Shutting an empty drawer is just that;
    /// opening it is a real eject, and so is pressing it while a load is
    /// still playing - in both cases the disc leaves the drive, so nothing
    /// is left booting or running from a disc that has come out.
    fn toggle_tray(&mut self) {
        if self.load.is_none() && self.fz1_tray_open {
            self.fz1_tray_open = false;
            // A disc was still waiting on the drawer and nothing is in the
            // drive: shutting it is the user taking that disc away.
            if !self.emu.disc_loaded() {
                self.clear_tray_disc();
            }
        } else {
            self.eject();
        }
    }

    /// Take the disc out, whatever stage the machine had reached. A load
    /// still running is cancelled; a game still running is stopped and saved
    /// and the launcher comes back. The spindle loses power, so the disc
    /// coasts down, and the drawer runs out with the disc still on it: it
    /// waits there to be swapped for another, or to be taken away by
    /// shutting the drawer on nothing.
    fn eject(&mut self) {
        let cancelled = self.load.take().is_some();
        if self.emu.running() {
            self.emu.set_paused(false);
            self.emu.stop();
            self.save_nvram();
        }
        if self.emu.disc_loaded() {
            self.emu.eject_disc();
        }
        self.picture_live = false;
        self.frontend = false;
        self.disc_index = 0;
        // `self.game` and its platter picture are left alone: the disc is
        // out of the drive but still on the drawer, and the scene has to go
        // on showing it sitting there.
        if self.game.is_some() {
            self.fz1_state.disc_drop = 1.0;
        }
        self.fz1_tray_open = true;
        self.view = View::Machine;
        self.status = if cancelled {
            "load cancelled - the disc came out".to_string()
        } else if self.game.is_some() {
            "ejected - the disc is waiting on the drawer".to_string()
        } else {
            String::new()
        };
    }

    /// Lift the ejected disc off the open drawer: what is left is an empty
    /// machine with an empty tray.
    fn clear_tray_disc(&mut self) {
        self.game = None;
        self.disc_index = 0;
        self.fz1_disc_art = None;
        self.fz1_state.disc_drop = 0.0;
    }

    /// Skip whatever is left of the load: straight to the machine on.
    fn finish_load(&mut self) {
        let Some(load) = self.load.take() else {
            return;
        };
        self.fz1_tray_open = false;
        self.fz1_state.tray = 0.0;
        self.fz1_state.disc_drop = 1.0;
        self.insert_disc(load.game);
        if !self.emu.running() {
            self.power_on();
        }
    }

    /// Step the load along. The drawer moves on its own clock in
    /// `animate`; each step waits for it.
    fn advance_load(&mut self, dt: f32) {
        let Some(load) = self.load.as_mut() else {
            return;
        };
        load.t += dt;
        let tray = self.fz1_state.tray;
        match load.step {
            LoadStep::Eject => {
                if tray < 0.999 {
                    load.t = 0.0;
                    return;
                }
                // The old disc lifts off the platter, then is gone.
                if self.game.is_some() {
                    self.fz1_state.disc_drop = (1.0 - load.t / 0.8).max(0.0);
                    if self.fz1_state.disc_drop > 0.0 {
                        return;
                    }
                    self.game = None;
                }
                let game = load.game;
                load.step = LoadStep::Place;
                load.t = 0.0;
                self.fz1_state.disc_drop = 0.0;
                if let Some(art) = self.platter_art(game) {
                    if self.fz1_disc_art.as_ref() != Some(&art) {
                        self.fz1_disc_art = Some(art.clone());
                        self.request_platter(art);
                    }
                }
            }
            LoadStep::Place => {
                // The disc only comes down once its picture is on it.
                if self.fz1_disc_uploaded != self.fz1_disc_art {
                    load.t = 0.0;
                    return;
                }
                let e = (load.t / 1.1).min(1.0);
                self.fz1_state.disc_drop = e;
                if e >= 1.0 && load.t > 1.4 {
                    load.step = LoadStep::Close;
                    load.t = 0.0;
                    self.fz1_tray_open = false;
                }
            }
            LoadStep::Close => {
                if tray <= 0.001 {
                    load.step = LoadStep::SpinUp;
                    load.t = 0.0;
                }
            }
            LoadStep::SpinUp => {
                if load.t >= SPINUP_SECS {
                    self.finish_load();
                }
            }
        }
    }

    /// The disc the platter carries this frame, if any.
    fn platter_game(&self) -> Option<usize> {
        match &self.load {
            Some(l) if l.step == LoadStep::Eject => self.game,
            Some(l) => Some(l.game),
            None => platter_title(
                self.game,
                self.emu.disc_loaded(),
                self.disc_speed > 0.01,
                self.fz1_tray_open,
            ),
        }
    }

    /// Move the scene on by `dt`: the drawer, the disc, the lamps, the
    /// unboxing. Runs only while the launcher is on screen.
    fn animate(&mut self, dt: f32, time: f64) {
        let running = self.emu.running();
        // A machine still in its carton keeps its drawer shut: nothing opens
        // until it is out and standing on the table.
        let boxed = self.fz1_state.unbox < 0.999;
        self.fz1_state.tray_goal = if self.fz1_tray_open && !boxed { 1.0 } else { 0.0 };
        let step = dt * TRAY_RATE;
        let gap = self.fz1_state.tray_goal - self.fz1_state.tray;
        self.fz1_state.tray += gap.clamp(-step, step);
        // Every case lies shut until its lid is opened in the case view. Only
        // `case_lid_open` drives the hinge, so the wrap shows while closed and
        // the disc on its hub shows once open.
        self.fz1_state.lid_goal = if self.case_lid_open { 1.0 } else { 0.0 };
        let lid_step = dt * LID_RATE;
        let lid_gap = self.fz1_state.lid_goal - self.fz1_state.lid;
        self.fz1_state.lid += lid_gap.clamp(-lid_step, lid_step);
        let platter = self.platter_game();
        self.fz1_state.disc_shown = platter.is_some();
        if self.load.is_none() {
            self.fz1_state.disc_drop = if platter.is_some() { 1.0 } else { 0.0 };
        }
        self.fz1_state.spin = self.disc_angle;
        self.fz1_state.lit = running;
        // The ACCESS lamp only ever shows the drive at work: spinning up,
        // the BIOS reading the disc, the drawer running.
        let spinning_up = self.load.as_ref().is_some_and(|l| l.step == LoadStep::SpinUp);
        let booting = running && !self.picture_live;
        let drawer = (self.fz1_state.tray - self.fz1_state.tray_goal).abs() > 0.001;
        self.fz1_state.access = if spinning_up || booting || (running && drawer) {
            ((time * 5.0).fract() < 0.6) as u8 as f32
        } else {
            0.0
        };
        // The unboxing, and the box-away once the drawer is shut.
        if let Some(c) = self.closing.as_mut() {
            if self.fz1_state.tray <= 0.001 {
                *c = (*c + dt / BOXAWAY_SECS).min(1.0);
            }
            self.fz1_state.unbox = 1.0 - *c;
        } else {
            let was = self.intro;
            self.intro = (self.intro + dt / INTRO_SECS).min(1.0);
            if was < 1.0 && self.intro >= 1.0 {
                self.intro_done();
            }
            // The show is the spin and then the unboxing, back to back.
            self.fz1_state.unbox = ((self.intro - SPIN_FRAC) / (1.0 - SPIN_FRAC)).clamp(0.0, 1.0);
            if !self.spun {
                let s = (self.intro / SPIN_FRAC).clamp(0.0, 1.0);
                // Eased at both ends, and a whole turn, so it lands on the
                // three-quarter view it started from.
                self.fz1_state.yaw = YAW0 + smoothstep(s) * std::f32::consts::TAU;
                self.spun = s >= 1.0;
            }
        }
        // The shelf only exists once the machine is out, standing still and
        // on its feet: while the carton turns, opens and gives the machine
        // up, the show has the window to itself.
        self.settled_for = if self.closing.is_none() && self.fz1_state.unbox >= 0.999 {
            self.settled_for + dt
        } else {
            0.0
        };
        self.fz1_state.zoom = self.fz1_user_zoom.clamp(0.45, 3.0);
    }

    /// Out of the box: an empty machine opens its drawer for a disc.
    fn intro_done(&mut self) {
        if self.game.is_none() && self.load.is_none() {
            self.fz1_tray_open = true;
        }
    }

    /// Load the scene and build the renderer, once, from egui's own wgpu
    /// device. Returns false where there is no GPU path or no model.
    fn ensure_scene(&mut self, frame: &mut eframe::Frame) -> bool {
        if !self.fz1_tried {
            self.fz1_tried = true;
            let console = fz1_3d::load_model();
            let carton = fz1_3d::load_carton();
            let case = fz1_3d::load_case();
            if let (Some(a), Some(b), Some(c)) = (console, carton, case) {
                self.fz1_scene = vec![a, b, c];
            }
        }
        if self.fz1_scene.is_empty() {
            return false;
        }
        let Some(state) = frame.wgpu_render_state() else {
            return false;
        };
        if self.fz1.is_none() {
            self.fz1 = fz1_3d::Fz1Renderer::new(state, &self.fz1_scene);
            // A fresh renderer has empty slots: everything is asked again.
            self.fz1_disc_uploaded = None;
            self.case_art_for = None;
            if let Some(art) = self.fz1_disc_art.clone() {
                self.request_platter(art);
            }
        }
        self.take_slot_art(state);
        self.fz1.is_some()
    }

    /// Let the 3D layer go entirely: a running game gets the GPU to itself.
    fn suspend_scene(&mut self, frame: &mut eframe::Frame) {
        if let Some(mut r) = self.fz1.take() {
            if let Some(state) = frame.wgpu_render_state() {
                r.release(state);
            }
        }
        // Drop any artwork still on its way; it is asked again on return.
        while self.slot_rx.try_recv().is_ok() {}
    }

    /// Paint the machine into `rect`, returning whether it drew. `fade`
    /// below 1.0 is the machine leaving as the game takes over: it sinks
    /// and goes see-through, and its controls stop answering.
    fn draw_fz1(
        &mut self,
        ui: &mut egui::Ui,
        frame: &mut eframe::Frame,
        rect: egui::Rect,
        fade: f32,
    ) -> bool {
        if !self.ensure_scene(frame) {
            return false;
        }
        let Some(state) = frame.wgpu_render_state() else {
            return false;
        };
        // The platter picture follows the disc on it.
        if self.load.is_none() {
            let want = self.platter_game().and_then(|g| self.platter_art(g));
            if want.is_some() && want != self.fz1_disc_art {
                self.fz1_disc_art = want.clone();
                if let Some(art) = want {
                    self.request_platter(art);
                }
            }
        }
        let Some(renderer) = self.fz1.as_mut() else {
            return false;
        };
        let ppt = ui.ctx().pixels_per_point();
        let w = (rect.width() * ppt).round().max(1.0) as u32;
        let h = (rect.height() * ppt).round().max(1.0) as u32;
        let boxed = self.fz1_state.unbox < 0.999;
        let which = if boxed { 0..2 } else { 0..1 };
        let Some(tex) = renderer.render_into(state, &self.fz1_scene, &self.fz1_state, (w, h), 0, which)
        else {
            return false;
        };
        let at = rect.translate(egui::vec2(0.0, (1.0 - fade) * rect.height() * 0.18));
        ui.painter().image(
            tex,
            at,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE.gamma_multiply(fade.clamp(0.0, 1.0)),
        );
        ui.ctx().request_repaint();
        if fade < 0.999 {
            return true;
        }

        let scene = ui.interact(rect, ui.id().with("fz1-orbit"), egui::Sense::click_and_drag());
        // The unboxing and the load are shows; a click skips them.
        if boxed || self.load.is_some() {
            if scene.clicked() {
                if self.closing.is_none() && boxed {
                    self.intro = 1.0;
                    self.fz1_state.unbox = 1.0;
                    self.fz1_state.yaw = YAW0;
                    self.spun = true;
                    self.intro_done();
                } else if self.load.is_some() {
                    self.finish_load();
                }
            }
            if self.load.is_some() {
                ui.painter().text(
                    egui::pos2(rect.center().x, rect.max.y - 16.0),
                    egui::Align2::CENTER_CENTER,
                    "click to skip",
                    egui::FontId::proportional(11.0),
                    TEXT_DIM,
                );
            }
            return true;
        }

        // Turn it with a drag, come closer with the wheel. Nothing turns it
        // but a hand.
        if scene.dragged() {
            let drag = scene.drag_delta();
            self.fz1_state.yaw += drag.x * 0.01;
            self.fz1_state.pitch = (self.fz1_state.pitch - drag.y * 0.01).clamp(-1.2, 1.2);
            self.orbit_hint = self.orbit_hint.min(0.99);
        }
        if scene.hovered() {
            let wheel = ui.ctx().input(|i| i.smooth_scroll_delta.y);
            if wheel != 0.0 {
                self.fz1_user_zoom = (self.fz1_user_zoom * (1.0 - wheel * 0.002)).clamp(0.45, 3.0);
            }
        }

        // The model's own buttons, wherever they have turned to.
        let mut hit: Option<&'static str> = None;
        for (index, (name, at, depth)) in renderer.screen_points().iter().enumerate() {
            let control = match name.as_str() {
                "PowerButton" => "power",
                "OpenCloseButton" => "tray",
                _ => continue,
            };
            if *depth < 0.0 || *depth > 1.0 {
                continue;
            }
            let centre = egui::pos2(
                rect.min.x + at[0] * rect.width(),
                rect.min.y + at[1] * rect.height(),
            );
            let size = (rect.width() * 0.05).clamp(14.0, 40.0);
            let spot = egui::Rect::from_center_size(centre, egui::vec2(size, size));
            // A button is several parts, so each one needs its own id.
            let press = ui.interact(spot, ui.id().with(("fz1", control, index)), egui::Sense::click());
            if press.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                ui.painter()
                    .circle_stroke(centre, size * 0.5, egui::Stroke::new(1.5, ACCENTS[3]));
            }
            if press.hovered() && press.clicked_by(egui::PointerButton::Primary) {
                hit = Some(control);
            }
        }
        match (hit, self.emu.running()) {
            (Some("power"), false) => self.power_on(),
            (Some("power"), true) => self.power_off(),
            (Some("tray"), _) => self.toggle_tray(),
            _ => {}
        }

        let dt = ui.ctx().input(|i| i.stable_dt).clamp(0.0, 0.1);
        if self.orbit_hint > 0.0 {
            self.orbit_seen += dt;
            if self.orbit_seen > 6.0 || self.orbit_hint < 1.0 {
                self.orbit_hint = (self.orbit_hint - dt / 1.2).max(0.0);
            }
            ui.painter().text(
                egui::pos2(rect.center().x, rect.max.y - 16.0),
                egui::Align2::CENTER_CENTER,
                "drag to turn · wheel to zoom · OPEN/CLOSE and POWER work",
                egui::FontId::proportional(11.0),
                TEXT_DIM.gamma_multiply(self.orbit_hint),
            );
        }
        true
    }

    /// The case of `index` in the upper view: turn it by hand to read the
    /// back, and put its disc in from here.
    fn view_case(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame, rect: egui::Rect, index: usize) {
        if !self.ensure_scene(frame) {
            self.case_open = None;
            return;
        }
        if self.case_art_for != Some(index) {
            self.case_art_for = Some(index);
            self.request_case_art(index);
        }
        let (Some(state), Some(renderer)) = (frame.wgpu_render_state(), self.fz1.as_mut()) else {
            return;
        };
        let ppt = ui.ctx().pixels_per_point();
        let w = (rect.width() * ppt).round().max(1.0) as u32;
        let h = (rect.height() * ppt).round().max(1.0) as u32;
        if let Some(tex) = renderer.render_into(state, &self.fz1_scene, &self.fz1_state, (w, h), 1, 2..3) {
            ui.painter().image(
                tex,
                rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }
        let orbit = ui.interact(rect, ui.id().with("case-orbit"), egui::Sense::click_and_drag());
        if orbit.dragged() {
            let d = orbit.drag_delta();
            self.fz1_state.case_yaw += d.x * 0.01;
            self.fz1_state.case_pitch = (self.fz1_state.case_pitch - d.y * 0.01).clamp(-1.4, 1.4);
        }
        if orbit.hovered() {
            let wheel = ui.ctx().input(|i| i.smooth_scroll_delta.y);
            if wheel != 0.0 {
                self.fz1_state.case_zoom =
                    (self.fz1_state.case_zoom * (1.0 - wheel * 0.002)).clamp(0.5, 2.5);
            }
        }
        let title = self
            .games
            .get(index)
            .map(|g| case_art::clean_title(&g.title))
            .unwrap_or_default();
        // The case fills the view, so the caption needs its own ground.
        ui.painter().rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(rect.min.x, rect.min.y),
                egui::pos2(rect.max.x, rect.min.y + 52.0),
            ),
            0.0,
            egui::Color32::from_black_alpha(150),
        );
        ui.painter().text(
            egui::pos2(rect.center().x, rect.min.y + 18.0),
            egui::Align2::CENTER_CENTER,
            &title,
            egui::FontId::proportional(18.0),
            egui::Color32::from_rgb(220, 222, 230),
        );
        ui.painter().text(
            egui::pos2(rect.center().x, rect.min.y + 40.0),
            egui::Align2::CENTER_CENTER,
            "drag to turn it over · wheel to zoom · enter to open/close",
            egui::FontId::proportional(11.0),
            TEXT_DIM,
        );
        let bar = egui::Rect::from_center_size(
            egui::pos2(rect.center().x, rect.max.y - 26.0),
            egui::vec2(390.0, 34.0),
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(bar), |ui| {
            ui.horizontal_centered(|ui| {
                let toggle = if self.case_lid_open {
                    "▣  Close case"
                } else {
                    "▣  Open case"
                };
                if ui.button(egui::RichText::new(toggle).size(15.0)).clicked() {
                    self.case_lid_open = !self.case_lid_open;
                }
                // Putting the disc in is the floating insert mark in the
                // corner now, paired with the eject one.
                if ui.button(egui::RichText::new("✕  Back").size(15.0)).clicked() {
                    self.case_open = None;
                    self.case_lid_open = false;
                }
            });
        });
        if ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
            self.case_open = None;
            self.case_lid_open = false;
        }
        ui.ctx().request_repaint();
    }

    /// The shelf: every game spine-out, side by side, in its own strip under
    /// the machine. One click opens the case; a double click puts the disc
    /// straight in.
    fn view_shelf(&mut self, ui: &mut egui::Ui, strip: egui::Rect, alpha: f32) {
        let painter = ui.painter_at(strip);
        let back = egui::Color32::from_rgb(17, 17, 24).gamma_multiply(alpha);
        painter.rect_filled(strip, 0.0, back);
        let board = egui::Rect::from_min_max(egui::pos2(strip.min.x, strip.max.y - 16.0), strip.max);
        painter.rect_filled(board, 0.0, egui::Color32::from_rgb(38, 36, 40).gamma_multiply(alpha));
        painter.line_segment(
            [board.left_top(), board.right_top()],
            egui::Stroke::new(1.0, egui::Color32::from_white_alpha((40.0 * alpha) as u8)),
        );
        let on_shelf = self.visible_games();
        if on_shelf.is_empty() {
            painter.text(
                strip.center(),
                egui::Align2::CENTER_CENTER,
                if self.games.is_empty() {
                    "no discs in the library folder"
                } else {
                    "every title is hidden - press C on the pad, or the eye below, to bring them back"
                },
                egui::FontId::proportional(13.0),
                TEXT_DIM.gamma_multiply(alpha),
            );
            self.shelf_chrome(ui, strip, alpha);
            return;
        }
        // Room above the spines for a pulled-out one and the title it names.
        let h = (strip.height() - 16.0 - 34.0).max(40.0);
        // A scan's own aspect, eased wider: a true 14 mm spine reads fine
        // on a shelf at arm's length, but a window is further away and the
        // lettering is what suffers, so each spine shows a half again as
        // wide — and never thinner than lettering can live in.
        let w = (h * SPINE_ASPECT * 1.5).max(22.0);
        let gap = 2.0;
        let total = on_shelf.len() as f32 * (w + gap);
        let room = strip.width() - 40.0;
        let hover_any = ui.rect_contains_pointer(strip);
        if total > room {
            if hover_any {
                let wheel = ui.ctx().input(|i| i.smooth_scroll_delta);
                self.shelf_scroll -= wheel.x + wheel.y;
            }
            self.shelf_scroll = self.shelf_scroll.clamp(0.0, total - room);
        } else {
            self.shelf_scroll = 0.0;
        }
        // The pad cannot reach for the shelf, so the shelf comes to it: a
        // selection moved by the pad is scrolled into view.
        if self.shelf_follow && total > room {
            if let Some(at) = self
                .selected
                .and_then(|sel| on_shelf.iter().position(|&i| i == sel))
            {
                let left = at as f32 * (w + gap);
                let want = self
                    .shelf_scroll
                    .min(left - 20.0)
                    .max(left + w + 20.0 - room);
                self.shelf_scroll = want.clamp(0.0, total - room);
            }
        }
        self.shelf_follow = false;
        let mut x = if total > room {
            strip.min.x + 20.0 - self.shelf_scroll
        } else {
            strip.center().x - total * 0.5
        };
        let mut named: Option<(egui::Pos2, String)> = None;
        let mut open: Option<usize> = None;
        let mut load: Option<usize> = None;
        // A compact shelf: at most twenty spines on show at once.
        let mut shown = 0;
        for &index in &on_shelf {
            let slot = egui::Rect::from_min_size(egui::pos2(x, board.min.y - h), egui::vec2(w, h));
            x += w + gap;
            if slot.max.x < strip.min.x || slot.min.x > strip.max.x {
                continue;
            }
            shown += 1;
            if shown > SHELF_MAX_SPINES {
                break;
            }
            let press = ui.interact(slot, ui.id().with(("spine", index)), egui::Sense::click());
            let chosen = self.case_open == Some(index);
            let picked = self.selected == Some(index);
            let lift = if press.hovered() || chosen || picked { 10.0 } else { 0.0 };
            let rect = slot.translate(egui::vec2(0.0, -lift));
            // The game's own spine scan where the folder has one, the drawn
            // 3DO spine where it has not.
            let tex = self
                .spines
                .get(index)
                .cloned()
                .flatten()
                .and_then(|p| self.art_texture(&p))
                .or_else(|| self.drawn_spine_texture(index));
            let faded = self.is_hidden(index);
            let painter = ui.painter_at(strip);
            // A hidden title on show while the hidden ones are up is drawn
            // back, so it reads as one that has been put away.
            let tint = egui::Color32::WHITE.gamma_multiply(if faded { alpha * 0.4 } else { alpha });
            match tex {
                Some(id) => {
                    painter.image(
                        id,
                        rect,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        tint,
                    );
                }
                None => {
                    let title = self.games.get(index).map(|g| g.title.clone()).unwrap_or_default();
                    text_spine(&painter, rect, &title, alpha);
                }
            }
            // A sliver of shade down each edge, so the spines read as
            // separate boxes standing side by side.
            painter.rect_stroke(
                rect,
                1.0,
                egui::Stroke::new(1.0, egui::Color32::from_black_alpha((150.0 * alpha) as u8)),
                StrokeKind::Inside,
            );
            if chosen || picked || self.game == Some(index) {
                painter.rect_filled(
                    egui::Rect::from_min_size(rect.left_bottom() + egui::vec2(0.0, 3.0), egui::vec2(w, 3.0)),
                    1.0,
                    match (chosen || picked, chosen) {
                        (true, true) => ACCENTS[3],
                        (true, false) => ACCENTS[0],
                        _ => ACCENTS[1],
                    },
                );
            }
            if press.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                let title = self
                    .games
                    .get(index)
                    .map(|g| case_art::clean_title(&g.title))
                    .unwrap_or_default();
                named = Some((egui::pos2(rect.center().x, strip.min.y + 12.0), title));
            }
            if press.double_clicked() {
                load = Some(index);
            } else if press.clicked() {
                open = Some(index);
                self.selected = Some(index);
            }
        }
        self.shelf_chrome(ui, strip, alpha);
        if let Some((at, title)) = named {
            let at = egui::pos2(at.x.clamp(strip.min.x + 120.0, strip.max.x - 120.0), at.y);
            painter.text(
                at,
                egui::Align2::CENTER_CENTER,
                title,
                egui::FontId::proportional(13.0),
                egui::Color32::from_rgb(225, 226, 232),
            );
        }
        // Mid-load or mid-unbox the shelf is scenery.
        let busy = self.load.is_some() || self.closing.is_some() || self.intro < 1.0;
        if !busy {
            if let Some(i) = load {
                self.start_load(i);
            } else if let Some(i) = open {
                self.open_case(i);
            }
        }
    }

    /// The shelf's own furniture: the eye that puts hidden titles back on
    /// show, the count of what is put away, and the pad map while it is
    /// still worth showing.
    fn shelf_chrome(&mut self, ui: &mut egui::Ui, strip: egui::Rect, alpha: f32) {
        let hidden = (0..self.games.len()).filter(|&i| self.is_hidden(i)).count();
        if hidden > 0 || self.show_hidden {
            // A word, not a glyph: the launcher's font is the one the phone
            // ships, and a word is read on any of them.
            let label = if self.show_hidden {
                format!("{hidden} hidden - put away")
            } else {
                format!("{hidden} hidden - show")
            };
            let spot = egui::Rect::from_min_size(
                egui::pos2(strip.min.x + 16.0, strip.min.y + 8.0),
                egui::vec2(170.0, 26.0),
            );
            let press = ui.interact(spot, ui.id().with("show-hidden"), egui::Sense::click());
            let painter = ui.painter_at(strip);
            painter.rect_filled(
                spot,
                6.0,
                egui::Color32::from_black_alpha(if press.hovered() { 220 } else { 150 }),
            );
            painter.text(
                spot.center(),
                egui::Align2::CENTER_CENTER,
                label,
                egui::FontId::proportional(12.0),
                if self.show_hidden { ACCENTS[3] } else { TEXT_DIM },
            );
            if press.clicked() {
                self.show_hidden = !self.show_hidden;
            }
            if press.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
        }
        // The pad map: up while the launcher is new, and again for a moment
        // whenever the pad is touched, so it is learnt rather than looked up.
        let dt = ui.ctx().input(|i| i.stable_dt).clamp(0.0, 0.1);
        self.pad_hint = (self.pad_hint - dt).max(0.0);
        let fade = if self.launched.elapsed().as_secs_f32() < 12.0 {
            1.0
        } else {
            (self.pad_hint / 1.5).min(1.0)
        };
        // The map gives way to its short form where the strip is too narrow
        // to carry it beside the hidden count.
        let room = strip.width() - if hidden > 0 || self.show_hidden { 210.0 } else { 40.0 };
        let map = if room > PAD_MAP.chars().count() as f32 * 5.6 {
            PAD_MAP
        } else {
            PAD_MAP_SHORT
        };
        if fade > 0.01 {
            ui.painter_at(strip).text(
                egui::pos2(strip.max.x - 16.0, strip.min.y + 21.0),
                egui::Align2::RIGHT_CENTER,
                map,
                egui::FontId::proportional(11.0),
                TEXT_DIM.gamma_multiply(fade * alpha),
            );
        }
        if self.pad_hint > 0.0 {
            ui.ctx().request_repaint();
        }
    }

    /// The built-in reading of the console's own art — it needs no pack and
    /// suits every title — is simply on.
    fn apply_textures(&mut self) {
        // Reading the art costs pixels to put it in, so the machine asks for
        // the higher internal scale; the emulator thread lowers it by itself
        // if the game cannot hold the field rate there.
        self.emu.set_internal_scale(HD_INTERNAL_SCALE);
        self.emu.set_texture_enhance(true);
    }

    // Decoded but not yet uploaded — the upload needs the egui context, which
    // only exists inside update().
    fn upload_pending(&mut self, ctx: &egui::Context) {
        for (key, img) in self.pending_images.drain(..) {
            // Mipmapped: spines and covers are shown far smaller than their
            // scans, and plain linear filtering shimmers there.
            let opts = TextureOptions {
                mipmap_mode: Some(egui::TextureFilter::Linear),
                ..TextureOptions::LINEAR
            };
            let tex = ctx.load_texture(key.clone(), img, opts);
            self.covers.insert(key, tex);
        }
    }
}

impl eframe::App for GuiApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        while let Ok((key, img)) = self.covers_rx.try_recv() {
            self.pending_images.push((key, img));
        }
        self.run_script(&ctx);
        // Closing the window puts the machine back in its box first; asking
        // again while it packs away closes at once.
        if ctx.input(|i| i.viewport().close_requested()) && !self.close_now {
            if self.closing.is_none() && self.view == View::Machine && self.fz1_scene_ok(frame) {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.begin_close();
            } else {
                self.save_nvram();
            }
        }
        if self.closing.is_some_and(|c| c >= 1.0) && !self.close_now {
            self.close_now = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        self.upload_pending(&ctx);
        self.pad_ui(&ctx);
        self.push_pads(&ctx);
        self.poll_frame();

        // Disc spin: full speed while the machine runs, a slow show-off turn
        // while a disc just sits in the tray. The spindle cuts power on
        // eject, so the disc coasts down instead of stopping dead. The
        // console is not drawn while the picture is up, so the spin is
        // frozen there — paused, not torn down.
        let running = self.emu.running();
        let dt = ctx.input(|i| i.stable_dt).min(0.1);
        if self.console_fade > 0.001 {
            let spin_up = self.load.as_ref().is_some_and(|l| l.step == LoadStep::SpinUp);
            let target = if spin_up {
                9.0
            } else if !self.emu.disc_loaded() || self.fz1_tray_open {
                0.0
            } else if running {
                9.0
            } else {
                0.6
            };
            // Power off is quick; a real drive takes its time to come up.
            let rate = if target < self.disc_speed { 4.0 } else { 1.6 };
            self.disc_speed += (target - self.disc_speed).clamp(-dt * rate, dt * rate);
            self.disc_angle += dt * self.disc_speed;
            if self.disc_speed > 0.01 {
                ctx.request_repaint();
            }
            // Spindle stopped and nothing in the drive: the disc is off the
            // platter. A guest asking for the next disc of a set keeps the
            // title, so the swap still knows what to put in, and so does a
            // disc the launcher has ejected onto the open drawer.
            if self.game.is_some()
                && self.load.is_none()
                && self.disc_speed <= 0.01
                && !self.emu.disc_loaded()
                && !self.emu.disc_tray_open()
                && !self.fz1_tray_open
            {
                self.clear_tray_disc();
            }
        }
        // A game asking for the next disc ejects the tray; answer it.
        self.poll_disc_request();
        // The drive can empty behind the launcher's back; what the machine
        // shows has to follow the drive, not the last thing the launcher did.
        self.sync_disc_state();

        // Audio and saves only matter while the machine runs.
        if running {
            if let Some(io) = &self.io {
                io.top_up(&self.emu, &mut self.audio_buf);
            }
            if self.last_nvram_save.elapsed() > Duration::from_secs(2) {
                self.last_nvram_save = Instant::now();
                self.save_nvram();
            }
        }

        // Everything that is not the picture lives in one column down the
        // left, so the game keeps the full width of the window.
        // The machine itself lives in a panel of its own, with the settings
        // that belong to it beside the model. Hidden, the picture has the
        // whole window.
        // The drawer being open is what "choosing a game" means: the
        // selector and the machine are on show then, and the picture has the
        // window to itself once it closes.
        // The launcher's scene only moves while it is on screen; a running
        // game suspends all of it.
        if !self.game_live() {
            let time = ctx.input(|i| i.time);
            self.animate(dt, time);
            self.advance_load(dt);
        }
        // No chrome strip at all: the window decorations carry the name, and
        // the hamburger and the cog float over whatever is on screen.
        // The status line carries messages only: a pad being plugged in is
        // not news the player needs on screen.
        if !self.status.is_empty() {
            egui::Panel::bottom("status").show(ui, |ui| {
                ui.label(egui::RichText::new(&self.status).small().color(TEXT_DIM));
            });
        }

        egui::CentralPanel::default().show(ui, |ui| match self.view {
            View::Setup => self.view_setup(ui),
            View::Machine => self.view_picture(ui, &ctx, frame),
            View::Settings => self.view_settings(ui),
        });

        // Escape is the way back off the settings page, as it is everywhere.
        if self.view == View::Settings
            && ctx.input(|i| i.key_pressed(egui::Key::Escape))
        {
            self.close_settings();
        }

        // The hamburger and the cog, side by side in the corner. While a game
        // has the window they hide, and the next touch brings them back.
        if self.view == View::Machine && self.chrome_shown(&ctx) {
            self.chrome_icons(&ctx);
        }

        if self.picker.is_some() {
            self.dir_picker_overlay(&ctx);
        }
    }
}

// ---------------------------------------------------------------------------
// Views

impl GuiApp {
    /// Grab the newest native field and note whether anything real is being
    /// drawn. Runs every repaint while the machine is on — the console's
    /// departure is keyed off `picture_live`, so waiting for the picture view
    /// to ask would mean it never happened.
    fn poll_frame(&mut self) {
        // Only consider real game rendering after the boot splash has finished.
        // The emulator may already be running during the BIOS boot logo, which
        // would otherwise set `picture_live` too early and cause the console to
        // fade out before the game starts. Guard against this by requiring the
        // splash timer to be cleared before we mark the picture as live.

        self.frame_dims = None;
        if !self.emu.running() {
            return;
        }
        let mut lit = false;
        self.emu.acquire_frame(|pixels, w, h| {
            self.frame_rgba.clear();
            self.frame_rgba
                .resize(w.saturating_mul(h).saturating_mul(4), 0);
            for (px, dst) in pixels.iter().zip(self.frame_rgba.as_chunks_mut::<4>().0) {
                let [_, r, g, b] = px.to_be_bytes(); // XRGB -> RGBA
                *dst = [r, g, b, 255];
            }
            self.frame_dims = Some((w, h));
            // Anything other than a black field counts as the game drawing.
            lit = self
                .frame_rgba
                .as_chunks::<4>()
                .0
                .iter()
                .step_by(97)
                .any(|px| {
                    px.first().copied().unwrap_or(0) as u32
                        + px.get(1).copied().unwrap_or(0) as u32
                        + px.get(2).copied().unwrap_or(0) as u32
                        > 90
                });
        });
        if lit {
            self.picture_live = true;
        }
    }

    /// The main area: the machine with its disc and pad until the core is
    /// drawing, then the machine slides away and the picture has the window.
    /// What counts as live is the core's output, disc or not — a powered-on
    /// machine with an empty tray still shows its BIOS boot and management
    /// screens, exactly as the real hardware does on the TV. An open drawer
    /// is the choosing state: the machine and the selector take the window
    /// back until it shuts.
    fn view_picture(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, frame: &mut eframe::Frame) {
        let live = self.game_live();
        let dt = ctx.input(|i| i.stable_dt).clamp(0.0, 0.1);
        let goal = if live { 0.0 } else { 1.0 };
        self.console_fade += (goal - self.console_fade).clamp(-dt / 0.5, dt / 0.5);
        if !live && self.console_fade >= 0.999 {
            self.view_machine(ui, frame);
            return;
        }
        self.view_screen(ui, ctx, frame);
        if self.console_fade <= 0.001 {
            // The game has the window: the launcher's 3D layer is let go
            // rather than drawn under the picture. The parsed models stay
            // in hand, so coming back rebuilds it.
            self.suspend_scene(frame);
        } else {
            // Mid-crossing: the machine sinks away over the new picture.
            let area = ui.available_rect_before_wrap();
            ui.scope_builder(egui::UiBuilder::new().max_rect(area), |ui| {
                self.draw_fz1(ui, frame, area, self.console_fade);
            });
            ctx.request_repaint();
        }
    }

    /// Whether a game has the window: running, drawing, and not set aside
    /// for the launcher.
    fn game_live(&self) -> bool {
        self.emu.running()
            && self.picture_live
            && !self.frontend
            && self.load.is_none()
            && self.closing.is_none()
    }

    fn fz1_scene_ok(&mut self, frame: &mut eframe::Frame) -> bool {
        frame.wgpu_render_state().is_some() && (!self.fz1_tried || !self.fz1_scene.is_empty())
    }

    /// Start packing the machine away: whatever runs stops and is saved,
    /// the drawer shuts, and the box-away plays before the window goes.
    fn begin_close(&mut self) {
        if self.emu.running() {
            self.emu.set_paused(false);
            self.emu.stop();
        }
        self.save_nvram();
        self.load = None;
        self.case_open = None;
        self.frontend = false;
        self.fz1_tray_open = false;
        self.intro = 1.0;
        self.closing = Some(0.0);
    }

    /// The launcher: the machine in the upper view (or the case being
    /// looked at), and the shelf in its own strip along the bottom.
    fn view_machine(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let area = ui.available_rect_before_wrap();
        ui.painter().rect_filled(area, 0.0, BG);
        self.wallpaper(ui, area);
        // Tall enough that a spine's title reads from a sofa: the strip takes
        // nearly half the view, and the machine above it frames a little
        // tighter. Spine art is long and thin, so height is what buys the
        // width the lettering needs.
        // A fifth taller than it was: the spines read from a sofa.
        let shelf_h = (area.height() * 0.528).clamp(360.0, 672.0).min(area.height() * 0.72);
        let strip = egui::Rect::from_min_max(egui::pos2(area.min.x, area.max.y - shelf_h), area.max);
        // The shelf comes in as the machine settles out of its box, and
        // goes as it is packed away.
        // A disc on its way in means the machine is booting, and a machine
        // that is on with a disc in it is past booting into the game: either
        // way the shelf goes away entirely. It only comes back when the
        // launcher is asked for over a running game (the choosing state) or
        // when an eject cancels the boot and drops back to the library.
        let booting = self.load.is_some() || (self.emu.running() && !self.frontend);
        let alpha = if booting {
            0.0
        } else {
            match self.closing {
                Some(c) => 1.0 - (c * 4.0).min(1.0),
                None => ((self.settled_for - SHELF_WAIT) / SHELF_FADE).clamp(0.0, 1.0),
            }
        };
        // While the carton is still turning and the machine is coming out of
        // it, there is no shelf, so the show gets the whole window: the scene
        // keeps the strip's room until the shelf arrives to claim it. Drawn
        // only as tall as the strip is empty, the reveal is never cut off
        // along a line the player cannot see the reason for.
        let scene = egui::Rect::from_min_max(
            area.min,
            egui::pos2(area.max.x, area.max.y - shelf_h * smoothstep(alpha)),
        );
        match self.case_open {
            Some(index) => self.view_case(ui, frame, scene, index),
            None => self.view_attract(ui, frame, scene),
        }
        if alpha > 0.0 {
            self.view_shelf(ui, strip, alpha);
        }
    }

    /// The 3DO mark tiled faint across the machine view, drifting slowly —
    /// a wall behind the console rather than a flat backdrop. Rows are
    /// staggered brick-style so the pattern reads as a pattern, not a grid.
    fn wallpaper(&mut self, ui: &mut egui::Ui, area: egui::Rect) {
        let Some(tex) = self.covers.get(WALL_KEY) else {
            return;
        };
        let dt = ui.ctx().input(|i| i.stable_dt).clamp(0.0, 0.1);
        // A slow diagonal, and it turns round at the bounds rather than
        // snapping back: the wall never jumps, whichever way it is going.
        self.wall_drift += dt * 8.0 * self.wall_dir.y;
        self.wall_drift_x += dt * 3.4 * self.wall_dir.x;
        if self.wall_drift.abs() > WALL_SPAN {
            self.wall_dir.y = -self.wall_dir.y;
            self.wall_drift = self.wall_drift.clamp(-WALL_SPAN, WALL_SPAN);
        }
        if self.wall_drift_x.abs() > WALL_SPAN {
            self.wall_dir.x = -self.wall_dir.x;
            self.wall_drift_x = self.wall_drift_x.clamp(-WALL_SPAN, WALL_SPAN);
        }
        let pitch_y = 190.0;
        let badge_h = pitch_y;
        let badge_w = pitch_y * tex.size_vec2().x / tex.size_vec2().y.max(1.0);
        let pitch_x = badge_w * 2.1;
        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        let tint = egui::Color32::from_white_alpha(18);
        let painter = ui.painter();
        // Tiles are placed by their own index in the wall, not by a counter
        // that restarts at the top of the view: the brick stagger then
        // belongs to the row itself, so a row crossing the edge keeps its
        // offset instead of popping over by half a tile. A tile's margin
        // either side draws over the seam.
        let (ox, oy) = (self.wall_drift_x, self.wall_drift);
        let row0 = ((area.min.y - oy) / pitch_y).floor() as i64 - 1;
        let row1 = ((area.max.y - oy) / pitch_y).ceil() as i64 + 1;
        for row in row0..=row1 {
            let y = oy + row as f32 * pitch_y;
            let stagger = pitch_x * 0.5 * row.rem_euclid(2) as f32;
            let col0 = ((area.min.x - ox - stagger) / pitch_x).floor() as i64 - 1;
            let col1 = ((area.max.x - ox - stagger) / pitch_x).ceil() as i64 + 1;
            for col in col0..=col1 {
                let x = ox + stagger + col as f32 * pitch_x;
                painter.image(
                    tex.id(),
                    egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(badge_w, badge_h)),
                    uv,
                    tint,
                );
            }
        }
        ui.ctx().request_repaint();
    }

    /// The machine in the upper view, with a word on what to do next.
    fn view_attract(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame, scene: egui::Rect) {
        let drawn = ui
            .scope_builder(egui::UiBuilder::new().max_rect(scene), |ui| {
                self.draw_fz1(ui, frame, scene, 1.0)
            })
            .inner;
        if !drawn {
            // No GPU path: no show to wait for.
            self.intro = 1.0;
            if self.load.is_some() {
                self.finish_load();
            }
            if self.closing.is_some() {
                self.closing = Some(1.0);
            }
            ui.painter().rect_filled(scene, 6.0, PANEL);
        }
        let hint = if self.closing.is_some() || self.intro < 1.0 || self.load.is_some() {
            ""
        } else if self.emu.running() && self.frontend {
            "Esc returns to the game · double-click a spine to change discs"
        } else {
            "click a spine to look at the case · double-click to play it"
        };
        ui.painter().text(
            egui::pos2(scene.center().x, scene.min.y + 14.0),
            egui::Align2::CENTER_CENTER,
            hint,
            egui::FontId::proportional(12.0),
            TEXT_DIM,
        );
        if self.emu.running() && self.frontend && self.load.is_none() {
            let spot = egui::Rect::from_min_size(
                egui::pos2(scene.max.x - 640.0, scene.min.y + 30.0),
                egui::vec2(626.0, 30.0),
            );
            ui.scope_builder(egui::UiBuilder::new().max_rect(spot), |ui| {
                ui.horizontal(|ui| {
                    let resume = ui.button("▶  Back to the game");
                    if resume.clicked() || ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
                        self.frontend = false;
                        self.emu.set_paused(false);
                    }
                    if ui.button("⟲  Reset").clicked() {
                        self.emu.request_reset();
                    }
                    if ui.button("⚙  Settings").clicked() {
                        self.open_settings();
                    }
                    // Ejecting is the floating mark in the corner now.
                    if ui.button("✕  Power off").clicked() {
                        self.power_off();
                    }
                });
            });
        }
    }

    /// The picture, once a game is drawing it.
    fn view_screen(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if !self.emu.running() {
            let rect = ui.available_rect_before_wrap();
            ui.painter().rect_filled(rect, 0.0, egui::Color32::BLACK);
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                if self.fz1_tray_open {
                    "pick a game below, then PLAY"
                } else {
                    "the drawer is shut — eject to choose another game"
                },
                egui::FontId::proportional(15.0),
                TEXT_DIM,
            );
            return;
        }
        // The newest field was already captured by `poll_frame` this repaint.
        let dims = self.frame_dims;

        // Upscale on the GPU straight to the size the frame will occupy on
        // screen, so every preset's internal scale lands on real pixels
        // instead of being rendered huge and minified back down. The core
        // already renders the preset's internal scale (up to 4x); the GPU
        // pass covers the rest of the way to the target.
        let rect = ui.available_rect_before_wrap();
        let ppt = ctx.pixels_per_point();
        let (fit_w, fit_h) = (rect.width() * ppt, rect.height() * ppt);
        let mut frame_id: Option<egui::TextureId> = None;
        let mut frame_size = egui::vec2(0.0, 0.0);
        if let (Some((w, h)), Some(state)) = (dims, frame.wgpu_render_state()) {
            if self.scaler.is_none() {
                self.scaler = Some(gpu_scale::GpuScaler::new(&state.device));
            }
            // The core draws at its own scale; the GPU pass only fits that
            // picture to the window.
            let (target_h, sharpen) = (0u32, 0.0f32);
            // Aspect-correct: fit the frame inside the area at physical size.
            let fit = (fit_w / w.max(1) as f32)
                .min(fit_h / h.max(1) as f32)
                .max(0.0);
            let out_w = ((w as f32 * fit).round() as u32).max(1) & !1;
            let out_h = ((h as f32 * fit).round() as u32).max(1);
            if let Some(scaler) = &mut self.scaler {
                frame_id = scaler.present(
                    state,
                    &self.frame_rgba,
                    w as u32,
                    h as u32,
                    out_w,
                    out_h,
                    // Crisp unless the player asked for the smooth filter.
                    target_h == 0 && !self.cfg.smooth,
                    sharpen,
                );
                frame_size = egui::vec2(out_w as f32, out_h as f32);
            }
        }
        if frame_id.is_none() && dims.is_some() {
            // New frame but no wgpu backend (e.g. a glow build): draw it as a
            // normal egui texture at native size, scaled by the backend.
            if let Some((w, h)) = dims {
                let img = ColorImage::from_rgba_unmultiplied([w, h], &self.frame_rgba);
                let opts = if self.cfg.smooth {
                    TextureOptions::LINEAR
                } else {
                    TextureOptions::NEAREST
                };
                match &mut self.frame_tex {
                    Some(tex) if tex.size() == img.size => tex.set(img, opts),
                    _ => self.frame_tex = Some(ctx.load_texture("frame", img, opts)),
                }
            }
            if let Some(tex) = &self.frame_tex {
                frame_id = Some(tex.id());
                frame_size = tex.size_vec2();
            }
        }
        // No new field this repaint (the UI redraws faster than the machine
        // produces fields): keep showing the last presented texture.
        if let (Some(id), true) = (frame_id, frame_size.x > 0.0) {
            self.last_frame = Some((id, frame_size));
        } else if let Some((id, size)) = self.last_frame {
            frame_id = Some(id);
            frame_size = size;
        }

        let painter = ui.painter();
        painter.rect_filled(rect, 0.0, egui::Color32::BLACK);
        if let Some(id) = frame_id {
            if frame_size.x > 0.0 && frame_size.y > 0.0 {
                let scale = (rect.width() / frame_size.x).min(rect.height() / frame_size.y);
                let (w, h) = (frame_size.x * scale, frame_size.y * scale);
                let dst = egui::Rect::from_center_size(rect.center(), egui::vec2(w, h));
                painter.image(
                    id,
                    dst,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            }
        } else {
            // Booting: nothing from the core yet, so hold the real logo on
            // the screen the way the machine would.
            self.paint_logo(ui, rect, 0.85);
        }

        // The picture carries no chrome but the hamburger: the controls live in
        // the menu it opens, not over the game.
        if self.chrome_shown(ctx) {
            self.disc_swap_strip(ui, rect);
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.open_frontend();
        }
        // Present on the display's own cadence. A fixed 8 ms timer beats
        // against both vsync and the 60 or 50 Hz the machine publishes at,
        // and that beat is what reaches the eye as uneven motion. A plain
        // repaint request is paced by the swapchain, and a repaint with no
        // new field costs one blit of the texture already on the GPU.
        ctx.request_repaint();
        self.frame_log(dims.is_some());
    }

    /// With RETRO3DO_FRAME_LOG set, report how evenly the picture reaches
    /// the screen: the gap between presents, and the gap between the fields
    /// the machine publishes. Off by default, and free when off.
    fn frame_log(&mut self, new_field: bool) {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if !*ON.get_or_init(|| std::env::var_os("RETRO3DO_FRAME_LOG").is_some()) {
            return;
        }
        let now = Instant::now();
        if let Some(last) = self.present_last {
            self.present_gaps.push(now.duration_since(last).as_secs_f64() * 1e3);
        }
        self.present_last = Some(now);
        if new_field {
            if let Some(last) = self.field_last {
                self.field_gaps.push(now.duration_since(last).as_secs_f64() * 1e3);
            }
            self.field_last = Some(now);
        }
        if self.present_gaps.len() < 300 {
            return;
        }
        fn report(name: &str, gaps: &mut Vec<f64>) {
            if gaps.is_empty() {
                return;
            }
            gaps.sort_by(f64::total_cmp);
            let n = gaps.len();
            let mean = gaps.iter().sum::<f64>() / n as f64;
            let at = |i: usize| gaps.get(i.min(n - 1)).copied().unwrap_or(0.0);
            println!(
                "{name}: n={n} mean={mean:.2}ms min={:.2} p50={:.2} p99={:.2} max={:.2}",
                at(0),
                at(n / 2),
                at(n * 99 / 100),
                at(n - 1)
            );
            gaps.clear();
        }
        let mut present = std::mem::take(&mut self.present_gaps);
        let mut field = std::mem::take(&mut self.field_gaps);
        report("present", &mut present);
        report("field", &mut field);
        let stats = self.emu.stats();
        println!(
            "emu: {:.2} fps, {:.2} ms/field, behind={}, resets={}, launcher3d={}",
            stats.emulated_fps,
            stats.frame_ms,
            stats.frames_behind,
            stats.deadline_resets,
            if self.fz1.is_some() { "live" } else { "suspended" }
        );
        self.present_gaps = present;
        self.field_gaps = field;
    }

    /// Play the RETRO3DO_SCRIPT automation: `secs:action` entries, where an
    /// action is `shot:NAME` (window screenshot into RETRO3DO_SHOT_DIR),
    /// `case:GAME`, `load:GAME` (GAME an index or part of a title), `lid`
    /// (swing the case's lid), `esc`, `eject`, `click` (skip the show
    /// running) or `close`. `key` pushes a
    /// key press into egui, waking the chrome the way a real touch does.
    fn run_script(&mut self, ctx: &egui::Context) {
        let shots: Vec<std::sync::Arc<ColorImage>> = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Screenshot { image, .. } => Some(image.clone()),
                    _ => None,
                })
                .collect()
        });
        let dir = std::env::var("RETRO3DO_SHOT_DIR").unwrap_or_else(|_| ".".to_string());
        for image in shots {
            if let Some(name) = self.shots.pop_front() {
                let path = format!("{dir}/{name}.png");
                let _ = image::save_buffer(
                    &path,
                    image.as_raw(),
                    image.size[0] as u32,
                    image.size[1] as u32,
                    image::ExtendedColorType::Rgba8,
                );
                println!("shot {path}");
            }
        }
        if self.script.is_empty() {
            return;
        }
        ctx.request_repaint();
        let now = self.launched.elapsed().as_secs_f32();
        while self.script.first().is_some_and(|(at, _)| *at <= now) {
            let (_, action) = self.script.remove(0);
            let (verb, arg) = action.split_once(':').unwrap_or((action.as_str(), ""));
            let game = arg.parse::<usize>().ok().or_else(|| {
                let want = arg.to_lowercase();
                self.games.iter().position(|g| g.title.to_lowercase().contains(&want))
            });
            println!("script {now:.1}s {action}");
            match verb {
                "shot" => {
                    self.shots.push_back(arg.to_string());
                    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
                }
                "case" => {
                    if let Some(g) = game {
                        self.open_case(g);
                    }
                }
                "lid" => self.case_lid_open = !self.case_lid_open,
                "load" => {
                    if let Some(g) = game {
                        self.start_load(g);
                    }
                }
                "esc" => {
                    if self.game_live() {
                        self.open_frontend();
                    } else if self.emu.running() && self.frontend {
                        self.frontend = false;
                        self.emu.set_paused(false);
                    }
                }
                "pad" => {
                    // One pad button, held for a frame: the launcher's own
                    // pad path is what the harness is exercising, so it is
                    // fed the same mask a real pad would make.
                    if let Some((_, button)) = sdl_pad_names()
                        .iter()
                        .find(|(name, _)| name.eq_ignore_ascii_case(arg))
                    {
                        self.script_pad |= 1 << (*button as u32);
                    }
                }
                "key" => {
                    // A key press pushed into egui, so the chrome's wake on
                    // input can be exercised without a person at the
                    // keyboard. It goes into the event list only: the pad
                    // reads the key state, not the events, so the game does
                    // not see it.
                    let key = match arg {
                        "escape" => egui::Key::Escape,
                        _ => egui::Key::Space,
                    };
                    ctx.input_mut(|i| {
                        i.events.push(egui::Event::Key {
                            key,
                            physical_key: None,
                            pressed: true,
                            repeat: false,
                            modifiers: Default::default(),
                        });
                    });
                }
                "click" => {
                    if self.load.is_some() {
                        self.finish_load();
                    } else {
                        self.intro = 1.0;
                        self.fz1_state.unbox = 1.0;
                        self.fz1_state.yaw = YAW0;
                        self.spun = true;
                        self.intro_done();
                    }
                }
                "eject" => self.eject(),
                "tray" => self.toggle_tray(),
                "scroll" => self.settings_scroll = arg.parse().ok(),
                "dirs" => self.apply_dirs(),
                "smooth" => {
                    self.cfg.smooth = !self.cfg.smooth;
                    let _ = self.cfg.save_settings();
                }
                "settings" => self.open_settings(),
                "settings-back" => self.close_settings(),
                "section" => {
                    if let Some((tab, _, _)) =
                        SETTINGS_TABS.iter().find(|(_, n, _)| n.eq_ignore_ascii_case(arg))
                    {
                        self.settings_tab = *tab;
                    }
                }
                // The script cannot inject pointer events the chrome would
                // see, so this is the touch that wakes it.
                "poke" => self.chrome_woke = Instant::now(),
                "menu" => self.chrome_menu(),
                "close" => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                _ => {}
            }
        }
    }

    /// Set the game aside for the launcher: it pauses under it.
    fn open_frontend(&mut self) {
        self.frontend = true;
        self.emu.set_paused(true);
    }

    /// One pad in a view of its own. Clicking it picks it up: that is the
    /// pad the keyboard then works.
    /// The other discs of the title in the machine, always in reach while it
    /// plays: one button per remaining disc, and the current one named. This
    /// is the manual half of the swap; the guest's own EJECT is the automatic
    /// half. Neither resets the machine.
    fn disc_swap_strip(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        let total = self.disc_queue().len();
        if total < 2 {
            return;
        }
        let current = self.disc_index;
        let area = egui::Rect::from_min_size(
            egui::pos2(rect.min.x + 12.0, rect.max.y - 44.0),
            egui::vec2(rect.width() - 90.0, 32.0),
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(area), |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!("disc {} of {total}", current + 1))
                        .small()
                        .color(TEXT_DIM),
                );
                for index in 0..total {
                    if index == current {
                        continue;
                    }
                    let label = format!("⏏ Disc {}", index + 1);
                    if ui
                        .small_button(label)
                        .on_hover_text("swap this disc in — the game keeps running")
                        .clicked()
                    {
                        self.swap_disc(index);
                    }
                }
            });
        });
    }

    /// Go to the settings page, with the folder fields filled from the
    /// settings in force and the way back pointed at this view.
    fn open_settings(&mut self) {
        self.set_dirs = Some((
            self.cfg.games_dir.display().to_string(),
            self.cfg.bios_dir.display().to_string(),
            self.cfg.media_dir.display().to_string(),
        ));
        self.rebind = None;
        if self.view != View::Settings {
            self.settings_back = self.view;
        }
        self.view = View::Settings;
    }

    /// Leave the settings page for the view the cog was pressed on.
    fn close_settings(&mut self) {
        self.rebind = None;
        if self.view == View::Settings {
            self.view = self.settings_back;
        }
    }

    /// The rebind table: one row a 3DO button, showing what drives it and a
    /// button that listens for a new one.
    #[cfg(not(target_os = "android"))]
    fn pad_rebind(&mut self, ui: &mut egui::Ui) {
        egui::Grid::new("rebind")
            .num_columns(3)
            .spacing([10.0, 4.0])
            .show(ui, |ui| {
                for (i, (three, _)) in sdl_pad::THREE_DO.iter().enumerate() {
                    let bound = self
                        .cfg
                        .pad_bind
                        .iter()
                        .find(|(n, _)| n == three)
                        .map_or_else(|| "-".to_string(), |(_, sdl)| sdl.clone());
                    ui.label(*three);
                    ui.label(egui::RichText::new(bound).color(TEXT_DIM).monospace());
                    let listening = self.rebind == Some(i);
                    let label = if listening { "press a button" } else { "rebind" };
                    if ui.button(label).clicked() {
                        self.rebind = if listening { None } else { Some(i) };
                    }
                    ui.end_row();
                }
            });
    }

    /// Android's pads come from the Java shim with a mapping of their own.
    #[cfg(target_os = "android")]
    fn pad_rebind(&mut self, ui: &mut egui::Ui) {
        ui.label(
            egui::RichText::new("Android pads use the system mapping.")
                .small()
                .color(TEXT_DIM),
        );
    }

    /// While a row is listening, the next pad button pressed takes it.
    /// Returns true when something was bound, so the caller can save.
    #[cfg(not(target_os = "android"))]
    fn poll_rebind(&mut self) -> bool {
        let Some(i) = self.rebind else {
            return false;
        };
        let Some((three, _)) = sdl_pad::THREE_DO.get(i) else {
            self.rebind = None;
            return false;
        };
        let primary = self.cfg.pad_primary;
        let Some(sdl) = self.io.as_ref().and_then(|io| io.pressed(primary)) else {
            return false;
        };
        // One SDL button drives one 3DO button: whatever else it was bound
        // to lets go of it.
        self.cfg.pad_bind.retain(|(n, s)| n != three && s != sdl);
        self.cfg.pad_bind.push((three.to_string(), sdl.to_string()));
        self.rebind = None;
        let bind = self.cfg.pad_bind.clone();
        if let Some(io) = self.io.as_mut() {
            io.set_bindings(&bind);
        }
        self.status = format!("{three} is {sdl}");
        true
    }

    #[cfg(target_os = "android")]
    fn poll_rebind(&mut self) -> bool {
        false
    }

    /// Point the library at other folders and read them again.
    fn apply_dirs(&mut self) {
        let Some((games, bios, media)) = self.set_dirs.clone() else {
            return;
        };
        self.cfg.games_dir = PathBuf::from(games.trim());
        self.cfg.bios_dir = PathBuf::from(bios.trim());
        self.cfg.media_dir = PathBuf::from(media.trim());
        self.games = scan_games(&self.cfg);
        self.bioses = scan_bios(&self.cfg);
        if self
            .bios_choice
            .as_ref()
            .is_none_or(|b| !self.bioses.contains(b))
        {
            self.bios_choice = self.bioses.first().cloned();
        }
        self.cfg.bios_choice = self.bios_choice.clone();
        self.selected = None;
        self.case_open = None;
        self.refresh_covers();
        self.extract_logo();
        self.status = format!(
            "{} games, {} BIOS files",
            self.games.len(),
            self.bioses.len()
        );
    }

    /// The settings page: a page of its own, never a dialog squeezed over the
    /// scene. A bar across the top carries the way back, the sections live
    /// down the left, and the section on show has the rest of the window.
    /// Every change is written to the settings file as it is made.
    fn view_settings(&mut self, ui: &mut egui::Ui) {
        let mut save = self.poll_rebind();
        let mut apply = false;
        let mut back = false;

        let page = ui.available_rect_before_wrap();
        ui.painter().rect_filled(page, 0.0, BG);

        // The top bar: the way back to wherever the cog was pressed.
        let header = egui::Rect::from_min_size(page.min, egui::vec2(page.width(), 74.0));
        ui.painter().rect_filled(header, 0.0, PANEL);
        ui.painter()
            .hline(page.x_range(), header.max.y, egui::Stroke::new(1.0, CARD));
        ui.scope_builder(
            egui::UiBuilder::new().max_rect(header.shrink2(egui::vec2(26.0, 16.0))),
            |ui| {
                ui.horizontal_centered(|ui| {
                    if ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new("\u{2039}  Back").size(15.0),
                            )
                            .min_size(egui::vec2(104.0, 36.0)),
                        )
                        .on_hover_text("Back to where you came from (Esc)")
                        .clicked()
                    {
                        back = true;
                    }
                    ui.add_space(18.0);
                    ui.label(egui::RichText::new("Settings").size(25.0).strong());
                });
            },
        );

        // The sections, down the left: one is on screen at a time, so nothing
        // has to be scrolled past to reach anything else.
        let body = egui::Rect::from_min_max(egui::pos2(page.min.x, header.max.y), page.max);
        let nav_w = (body.width() * 0.26).clamp(150.0, 230.0);
        let nav = egui::Rect::from_min_size(body.min, egui::vec2(nav_w, body.height()));
        ui.painter()
            .vline(nav.max.x, body.y_range(), egui::Stroke::new(1.0, CARD));
        ui.scope_builder(
            egui::UiBuilder::new().max_rect(nav.shrink2(egui::vec2(16.0, 22.0))),
            |ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                for (tab, name, hint) in SETTINGS_TABS {
                    let on = self.settings_tab == tab;
                    let hit = ui.add_sized(
                        [ui.available_width(), 42.0],
                        egui::Button::selectable(on, egui::RichText::new(name).size(16.0)),
                    );
                    if hit.on_hover_text(hint).clicked() {
                        self.settings_tab = tab;
                    }
                }
            },
        );

        // The section itself, with room around it however wide the window is.
        let pane = egui::Rect::from_min_max(egui::pos2(nav.max.x, body.min.y), body.max);
        ui.scope_builder(
            egui::UiBuilder::new().max_rect(pane.shrink2(egui::vec2(34.0, 24.0))),
            |ui| {
                let mut area = egui::ScrollArea::vertical().auto_shrink([false, false]);
                if let Some(at) = self.settings_scroll.take() {
                    area = area.vertical_scroll_offset(at);
                }
                area.show(ui, |ui| {
                    let measure = ui.available_width().min(720.0);
                    ui.set_max_width(measure);
                    match self.settings_tab {
                        SettingsTab::Controller => save |= self.settings_controller(ui),
                        SettingsTab::Video => save |= self.settings_video(ui),
                        SettingsTab::Paths => save |= self.settings_paths(ui, &mut apply),
                        SettingsTab::System => save |= self.settings_system(ui),
                    }
                });
            },
        );

        if back {
            self.close_settings();
        }
        if apply {
            self.apply_dirs();
            save = true;
        }
        if save {
            if let Err(e) = self.cfg.save_settings() {
                self.status = format!("could not save settings: {e}");
            }
        }
    }

    /// Controller: which pad drives the machine, and what each 3DO button is.
    fn settings_controller(&mut self, ui: &mut egui::Ui) -> bool {
        let mut save = false;
        settings_head(
            ui,
            "Controller",
            "Which pad drives the machine, and what each 3DO button answers to.",
        );
        settings_card(ui, "The pad in the launcher", |ui| {
            ui.label(
                egui::RichText::new(
                    "The launcher itself is driven from the pad, so a phone with a \
                     controller never needs the screen.",
                )
                .color(TEXT_DIM),
            );
            ui.add_space(6.0);
            for (button, does) in PAD_LAUNCHER {
                ui.horizontal(|ui| {
                    ui.add_sized(
                        [92.0, 18.0],
                        egui::Label::new(egui::RichText::new(*button).strong()),
                    );
                    ui.label(egui::RichText::new(*does).color(TEXT_DIM));
                });
            }
        });
        let pads = self.pad_names();
        settings_card(ui, "The pad", |ui| {
            if pads.is_empty() {
                ui.label(
                    egui::RichText::new("No pads detected - the keyboard drives pad 0.")
                        .color(TEXT_DIM),
                );
            }
            for (i, name) in pads.iter().enumerate() {
                if ui
                    .radio(self.cfg.pad_primary == i, format!("{i}   {name}"))
                    .clicked()
                {
                    self.cfg.pad_primary = i;
                    save = true;
                }
            }
        });
        ui.add_space(16.0);
        settings_card(ui, "Buttons", |ui| {
            ui.label(
                egui::RichText::new("Click rebind, then press the button on the pad.")
                    .small()
                    .color(TEXT_DIM),
            );
            ui.add_space(10.0);
            self.pad_rebind(ui);
            ui.add_space(12.0);
            if ui.button("stock mapping").clicked() {
                self.cfg.pad_bind = default_bindings();
                let bind = self.cfg.pad_bind.clone();
                if let Some(io) = self.io.as_mut() {
                    io.set_bindings(&bind);
                }
                save = true;
            }
        });
        save
    }

    /// Video: how the machine's picture reaches the window.
    fn settings_video(&mut self, ui: &mut egui::Ui) -> bool {
        let mut save = false;
        settings_head(
            ui,
            "Video",
            "How the machine's picture is scaled up to the window.",
        );
        settings_card(ui, "Scaling", |ui| {
            let mut smooth = self.cfg.smooth;
            if ui
                .checkbox(&mut smooth, "Smooth rendering (bilinear filter)")
                .changed()
            {
                self.cfg.smooth = smooth;
                save = true;
            }
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(
                    "Off keeps every pixel square and crisp; on blends them as the \
                     picture is scaled up to the window.",
                )
                .small()
                .color(TEXT_DIM),
            );
        });
        save
    }

    /// Paths: the folders the library, the BIOS images and the artwork are
    /// read from.
    fn settings_paths(&mut self, ui: &mut egui::Ui, apply: &mut bool) -> bool {
        settings_head(ui, "Paths", "Where the library is read from.");
        settings_card(ui, "Folders", |ui| {
            let mut browse = None;
            if let Some(dirs) = self.set_dirs.as_mut() {
                for (label, step, field) in [
                    ("Games  ", 11usize, &mut dirs.0),
                    ("BIOS   ", 12, &mut dirs.1),
                    ("Artwork", 13, &mut dirs.2),
                ] {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(label).monospace());
                        let room = (ui.available_width() - 104.0).max(140.0);
                        ui.add(egui::TextEdit::singleline(field).desired_width(room));
                        if ui.button("browse\u{2026}").clicked() {
                            browse = Some((step, field.clone()));
                        }
                    });
                    ui.add_space(8.0);
                }
            }
            if let Some((step, at)) = browse {
                self.open_picker(step, &at);
            }
        });
        ui.add_space(16.0);
        settings_card(ui, "Read them again", |ui| {
            ui.label(
                egui::RichText::new(
                    "The library is scanned when you ask: nothing is read until then.",
                )
                .small()
                .color(TEXT_DIM),
            );
            ui.add_space(10.0);
            if ui
                .add(
                    egui::Button::new("read these folders")
                        .min_size(egui::vec2(176.0, 32.0)),
                )
                .clicked()
            {
                *apply = true;
            }
        });
        false
    }

    /// System: the BIOS the machine boots and the headroom its CPU is given.
    fn settings_system(&mut self, ui: &mut egui::Ui) -> bool {
        let mut save = false;
        settings_head(
            ui,
            "System",
            "The ROM the machine boots, and the headroom its CPU is given.",
        );
        let bioses = self.bioses.clone();
        settings_card(ui, "BIOS", |ui| {
            if bioses.is_empty() {
                ui.label(
                    egui::RichText::new("No BIOS images in the BIOS folder.")
                        .color(TEXT_DIM),
                );
            }
            for name in &bioses {
                let on = self.bios_choice.as_deref() == Some(name.as_str());
                if ui.radio(on, name).clicked() {
                    self.bios_choice = Some(name.clone());
                    self.cfg.bios_choice = self.bios_choice.clone();
                    self.extract_logo();
                    save = true;
                }
            }
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(
                    "A different ROM takes the machine from the next power-on.",
                )
                .small()
                .color(TEXT_DIM),
            );
        });
        ui.add_space(16.0);
        settings_card(ui, "CPU", |ui| {
            let mut scale = self.cfg.cpu_scale;
            if ui
                .add(
                    egui::Slider::new(&mut scale, 100..=150)
                        .suffix(" %")
                        .text("overclock"),
                )
                .changed()
            {
                self.cfg.cpu_scale = scale;
                self.emu.set_cpu_scale_percent(scale);
                save = true;
            }
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(
                    "More ARM work per field, no other clock moved: it helps games \
                     the real hardware itself struggled with.",
                )
                .small()
                .color(TEXT_DIM),
            );
        });
        save
    }

    /// Open the folder picker on a path field, starting where it points.
    fn open_picker(&mut self, step: usize, at: &str) {
        let start = at.trim();
        let start = if start.is_empty() {
            start_dir()
        } else {
            PathBuf::from(start)
        };
        self.picker = Some(if start.is_dir() {
            start
        } else {
            start.parent().map(Path::to_path_buf).unwrap_or(start)
        });
        self.picker_step = step;
    }


    /// Whether the overlay chrome is on screen. A game on the window gets the
    /// picture and nothing else until the player touches something; the
    /// launcher always shows the icons.
    fn chrome_shown(&mut self, ctx: &egui::Context) -> bool {
        let touched = ctx.input(|i| {
            i.pointer.any_down()
                || i.pointer.is_moving()
                || i.events.iter().any(|e| {
                    matches!(
                        e,
                        egui::Event::PointerButton { .. }
                            | egui::Event::PointerMoved(_)
                            | egui::Event::Touch { .. }
                            | egui::Event::Key { .. }
                            | egui::Event::MouseWheel { .. }
                    )
                })
        });
        if touched {
            self.chrome_woke = Instant::now();
        }
        if !self.game_live() {
            return true;
        }
        if self.chrome_woke.elapsed() < CHROME_IDLE {
            // Come back when the three seconds are up: the picture alone would
            // keep the icons on screen until the next field arrived.
            ctx.request_repaint_after(CHROME_IDLE);
            true
        } else {
            false
        }
    }

    /// The hamburger: the shelf from a running game, and the way back into
    /// the game from the launcher over it.
    fn chrome_menu(&mut self) {
        if self.game_live() {
            self.open_frontend();
        } else if self.emu.running() && self.frontend {
            self.frontend = false;
            self.emu.set_paused(false);
        }
    }

    /// The floating corner: the hamburger (the menu — the shelf from a game,
    /// the way back into it from the launcher) and the cog (settings), side by
    /// side over everything else.
    fn chrome_icons(&mut self, ctx: &egui::Context) {
        let mut menu = false;
        let mut cog = false;
        let mut insert = false;
        let mut eject = false;
        // The case on show is the disc the insert mark puts in; the machine
        // has something in it when a disc is loaded or one is on its way.
        let case = self.case_open;
        let loaded = self.emu.disc_loaded() || self.load.is_some();
        egui::Area::new(egui::Id::new("chrome-icons"))
            .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-12.0, 12.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    if case.is_some() {
                        insert = tray_icon(ui, true, "Put the disc in");
                    }
                    if loaded {
                        eject = tray_icon(ui, false, "Take the disc out");
                    }
                    menu = icon_button(ui, "\u{2630}", "The menu");
                    cog = icon_button(ui, "\u{2699}", "Settings");
                });
            });
        if menu {
            self.chrome_menu();
        }
        if cog {
            self.open_settings();
        }
        if insert {
            if let Some(index) = case {
                self.start_load(index);
            }
        }
        if eject {
            self.eject();
        }
    }

}


#[cfg_attr(target_os = "android", allow(dead_code))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = match Config::from_args() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(if e.starts_with("usage") { 0 } else { 2 });
        }
    };
    if cfg.resolve {
        resolve_report(&cfg);
        return Ok(());
    }
    run(cfg, None)
}

/// Android's entry point: the activity arrives from `android-activity`, and
/// winit takes the native window from there. The `#[no_mangle]` export lives in
/// `android/src/lib.rs`, outside this file's `forbid(unsafe_code)`.
#[cfg(target_os = "android")]
pub fn android_main(app: winit::platform::android::activity::AndroidApp) {
    let cfg = Config::from_args().unwrap_or_else(|_| Config::with_defaults());
    let _ = run(cfg, Some(app));
}

#[cfg(target_os = "android")]
type AndroidContext = winit::platform::android::activity::AndroidApp;
#[cfg(not(target_os = "android"))]
type AndroidContext = ();

/// Hand the activity to winit's android backend; nothing to do elsewhere.
#[cfg(target_os = "android")]
fn attach_activity(options: &mut eframe::NativeOptions, app: Option<AndroidContext>) {
    options.android_app = app;
}
#[cfg(not(target_os = "android"))]
fn attach_activity(_options: &mut eframe::NativeOptions, _app: Option<AndroidContext>) {}

/// Paint the launcher: one window on a desk, the whole screen on a handheld.
fn run(cfg: Config, app: Option<AndroidContext>) -> Result<(), Box<dyn std::error::Error>> {
    let icon = image::load_from_memory(include_bytes!("../../assets/retro3do-icon-1024.png"))
        .ok()
        .map(|img| {
            let rgba = img.to_rgba8();
            egui::IconData {
                rgba: rgba.as_raw().clone(),
                width: rgba.width(),
                height: rgba.height(),
            }
        });
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Retro 3DO")
        .with_inner_size([1280.0, 800.0])
        .with_min_inner_size([720.0, 540.0]);
    if let Some(icon) = icon {
        viewport = viewport.with_icon(icon);
    }
    let mut options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    attach_activity(&mut options, app);
    eframe::run_native(
        "retro3do",
        options,
        Box::new(move |cc| Ok(Box::new(GuiApp::new(cc, cfg)))),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]
    use super::*;

    fn game(title: &str, disc: &str) -> Game {
        let path = PathBuf::from(disc);
        Game {
            discs: vec![path.clone()],
            title: title.to_string(),
            cover: None,
            cover_key: None,
            disc_art: None,
            disc_key: None,
            path,
        }
    }

    #[test]
    fn disc_tag_reads_the_shapes_sets_ship_as() {
        let cases = [
            ("Game (Disc 2)", "Game", 2),
            ("Game (Disc 2 of 3)", "Game", 2),
            ("Game [Disc 3]", "Game", 3),
            ("Game Disc 2", "Game", 2),
            ("Game CD2", "Game", 2),
            ("Game CD 2", "Game", 2),
            ("Game - Disk 4", "Game", 4),
            ("Game (CD 1) (USA)", "Game", 1),
            ("Game (Disc 10)", "Game", 10),
        ];
        for (title, stem, number) in cases {
            assert_eq!(disc_tag(title), Some((stem.to_string(), number)), "{title}");
        }
    }

    #[test]
    fn disc_tag_ignores_words_that_only_contain_disc() {
        for title in [
            "Abcd2",
            "Disco Elysium",
            "CD-i Adventure",
            "Game (Disc)",
            "Game (Disc 0)",
            "2CD Collection",
        ] {
            assert_eq!(disc_tag(title), None, "{title}");
        }
    }

    #[test]
    fn fold_keeps_one_entry_per_title_in_disc_order() {
        let mut games = vec![
            game("Game (Disc 2)", "b/d2.chd"),
            game("Game (Disc 1)", "b/d1.chd"),
            game("Other", "b/other.chd"),
            game("Game (Disc 3 of 3)", "b/d3.chd"),
        ];
        fold_disc_sets(&mut games);
        games.sort_by(|a, b| a.title.cmp(&b.title));
        assert_eq!(games.len(), 2);
        assert_eq!(games[0].title, "Game");
        assert_eq!(
            games[0].discs,
            vec![
                PathBuf::from("b/d1.chd"),
                PathBuf::from("b/d2.chd"),
                PathBuf::from("b/d3.chd")
            ]
        );
        // The lowest-numbered disc is what the entry boots.
        assert_eq!(games[0].path, PathBuf::from("b/d1.chd"));
        assert_eq!(games[1].title, "Other");
    }

    #[test]
    fn fold_of_a_set_with_no_first_disc_still_offers_its_discs() {
        let mut games = vec![game("Game (Disc 2)", "b/d2.chd"), game("Game (Disc 3)", "b/d3.chd")];
        fold_disc_sets(&mut games);
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].discs.len(), 2);
        assert_eq!(games[0].path, PathBuf::from("b/d2.chd"));
    }
    #[test]
    fn scanning_a_synthetic_multidisc_layout_makes_one_entry_per_title() {
        let root = std::env::temp_dir().join(format!(
            "retro3do-scan-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let games_dir = root.join("games");
        let media_dir = root.join("media");
        std::fs::create_dir_all(&games_dir).unwrap();
        std::fs::create_dir_all(&media_dir).unwrap();
        for name in [
            "Solo Game (USA).iso",
            "Saga (Disc 2).chd",
            "Saga (Disc 1).chd",
            "Saga (Disc 3).chd",
            "Tale CD1.iso",
            "Tale CD2.iso",
        ] {
            std::fs::write(games_dir.join(name), b"x").unwrap();
        }
        // An m3u names its own order, and the images it names must not also
        // appear as separate carousel entries — even when the list writes
        // "./" in front of them.
        std::fs::write(games_dir.join("Playlist Disc A.iso"), b"x").unwrap();
        std::fs::write(games_dir.join("Playlist Disc B.iso"), b"x").unwrap();
        std::fs::write(
            games_dir.join("Playlist.m3u"),
            "#EXTM3U\r\n./Playlist Disc A.iso\r\n\"Playlist Disc B.iso\"\r\n",
        )
        .unwrap();

        let cfg = Config {
            games_dir: games_dir.clone(),
            media_dir,
            bios_dir: root.join("bios"),
            saves_dir: root.join("saves"),
            boot: None,
            play: None,
            docs_dir: root.join("docs"),
            bios_choice: None,
            configured: true,
            texdir: None,
            name_map: HashMap::new(),
            hidden: std::collections::HashSet::new(),
            resolve: false,
            smooth: false,
            cpu_scale: 100,
            pad_primary: 0,
            pad_bind: default_bindings(),
        };
        let games = scan_games(&cfg);
        let titles: Vec<&str> = games.iter().map(|g| g.title.as_str()).collect();
        assert_eq!(
            titles,
            vec!["Playlist", "Saga", "Solo Game (USA)", "Tale"],
            "one entry per title, sorted; no disc leaked out on its own"
        );
        let saga = games.iter().find(|g| g.title == "Saga").unwrap();
        assert_eq!(
            saga.discs,
            vec![
                games_dir.join("Saga (Disc 1).chd"),
                games_dir.join("Saga (Disc 2).chd"),
                games_dir.join("Saga (Disc 3).chd")
            ]
        );
        assert_eq!(saga.path, games_dir.join("Saga (Disc 1).chd"));
        let tale = games.iter().find(|g| g.title == "Tale").unwrap();
        assert_eq!(
            tale.discs,
            vec![games_dir.join("Tale CD1.iso"), games_dir.join("Tale CD2.iso")]
        );
        let playlist = games.iter().find(|g| g.title == "Playlist").unwrap();
        assert_eq!(
            playlist.discs,
            vec![
                games_dir.join("Playlist Disc A.iso"),
                games_dir.join("Playlist Disc B.iso")
            ],
            "m3u order is the queue order"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_platter_shows_the_disc_in_the_drive() {
        // A title highlighted in the shelf is a choice, not an insertion:
        // with nothing in the drive the platter is bare.
        assert_eq!(platter_title(Some(3), false, false, false), None);
        assert_eq!(platter_title(Some(3), true, false, false), Some(3));
        // Ejected, but the spindle has not stopped yet: the disc is still
        // coasting down on the platter.
        assert_eq!(platter_title(Some(3), false, true, false), Some(3));
        // Spun down and run out: the disc waits on the open drawer.
        assert_eq!(platter_title(Some(3), false, false, true), Some(3));
        assert_eq!(platter_title(None, true, true, false), None);
        assert_eq!(platter_title(None, false, false, true), None);
    }

    /// A scratch library: a games folder and a media folder.
    fn scratch_library() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "retro3do-art-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(root.join("games")).unwrap();
        std::fs::create_dir_all(root.join("media")).unwrap();
        root
    }

    fn scratch_config(root: &Path) -> Config {
        Config {
            games_dir: root.join("games"),
            media_dir: root.join("media"),
            bios_dir: root.join("bios"),
            saves_dir: root.join("saves"),
            boot: None,
            play: None,
            docs_dir: root.join("docs"),
            bios_choice: None,
            configured: true,
            texdir: None,
            name_map: HashMap::new(),
            hidden: std::collections::HashSet::new(),
            resolve: false,
            smooth: false,
            cpu_scale: 100,
            pad_primary: 0,
            pad_bind: default_bindings(),
        }
    }

    #[test]
    fn region_and_revision_tags_go_but_the_disc_stays() {
        assert_eq!(normalize("Killing Time (USA) (RE1)"), "killing time");
        assert_eq!(normalize("PGA Tour 96 (USA) (RE-1)"), "pga tour 96");
        assert_eq!(
            normalize("Flashback - The Quest for Identity (USA) (En,Fr)"),
            "flashback quest for identity"
        );
        assert_eq!(
            normalize("Wing Commander III - Heart of the Tiger (USA, Europe) (Disc 1)"),
            "wing commander iii heart of tiger disc 1"
        );
    }

    #[test]
    fn an_exact_name_beats_a_longer_one() {
        let art = vec![
            PathBuf::from("Alone in the Dark 2 (USA).png"),
            PathBuf::from("Alone in the Dark (USA).png"),
        ];
        let stem = normalize("Alone in the Dark (USA)");
        assert_eq!(
            match_media(&stem, &art).map(|p| file_stem(p)),
            Some("Alone in the Dark (USA)".to_string())
        );
    }

    #[test]
    fn the_map_names_the_file_when_the_names_disagree() {
        let root = scratch_library();
        let media = root.join("media");
        std::fs::create_dir_all(media.join("box2d")).unwrap();
        std::fs::write(media.join("box2d").join("Odd Name.png"), b"x").unwrap();
        std::fs::write(
            root.join("name-map.tsv"),
            "Some Game (USA)\tbox2d\tOdd Name.png\n",
        )
        .unwrap();
        let mut cfg = scratch_config(&root);
        cfg.name_map = load_name_map(&cfg);
        let candidates = scan_dir(&media.join("box2d"), &["png"]);
        assert_eq!(
            resolve_media(&cfg, "Some Game (USA)", &["box2d"], &candidates),
            Some(media.join("box2d").join("Odd Name.png"))
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn one_disc_falls_back_to_the_sets_art() {
        let root = scratch_library();
        let media = root.join("media");
        std::fs::create_dir_all(media.join("images")).unwrap();
        let set = media.join("images").join(
            "Wing Commander III - Heart of the Tiger (USA) (Disc 1).png",
        );
        std::fs::write(&set, b"x").unwrap();
        let cfg = scratch_config(&root);
        let candidates = scan_dir(&media.join("images"), &["png"]);
        assert_eq!(
            resolve_media(
                &cfg,
                "Wing Commander III - Heart of the Tiger (USA, Europe) (Disc 2)",
                &["images"],
                &candidates
            ),
            Some(set)
        );
        std::fs::remove_dir_all(&root).ok();
    }
    #[test]
    fn the_tray_disc_wears_the_scan_and_never_the_cover() {
        let root = scratch_library();
        let media = root.join("media");
        std::fs::create_dir_all(media.join("discs")).unwrap();
        std::fs::create_dir_all(media.join("box2d")).unwrap();
        std::fs::write(media.join("discs").join("Some Game (USA).png"), b"x").unwrap();
        std::fs::write(media.join("box2d").join("Some Game (USA).png"), b"y").unwrap();
        let mut cfg = scratch_config(&root);
        cfg.name_map = load_name_map(&cfg);
        let scans = scan_dir(&media.join("discs"), &["png"]);
        // The disc wears its own scan, not the cover filed beside it.
        assert_eq!(
            resolve_disc_scan(&cfg, "Some Game (USA)", &scans),
            Some(media.join("discs").join("Some Game (USA).png"))
        );
        // A title with no scan has no disc art at all: what the tray gets
        // then is the plain printed platter, never the cover.
        assert!(resolve_disc_scan(&cfg, "Other Game (USA)", &scans).is_none());
        let plain = case_art::disc(&case_art::CaseArt {
            title: "Other Game".to_string(),
            front: Some(media.join("box2d").join("Some Game (USA).png")),
            ..Default::default()
        });
        assert_eq!(plain.size, [case_art::DISC_TEX as usize; 2]);
        assert_eq!(plain.pixels[0].a(), 0, "the platter's corner is clear");
    }
}
