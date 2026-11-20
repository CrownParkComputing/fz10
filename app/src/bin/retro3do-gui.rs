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
#[path = "retro3do-gui/fz1_3d.rs"]
mod fz1_3d;
#[path = "retro3do-gui/gpu_scale.rs"]
mod gpu_scale;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

use egui::{ColorImage, StrokeKind, TextureHandle, TextureOptions};
use retro3do::{PadButton, Region, Retro3do, MAX_PADS};
use sdl3::audio::{AudioFormat, AudioSpec, AudioStreamOwner};
use sdl3::gamepad::{Axis, Button, Gamepad};

const TILE_W: f32 = 190.0;
const TILE_H: f32 = 108.0;

/// Audio the stream keeps buffered, and how much is pulled at a time.
const AUDIO_PAIRS_TARGET: i32 = 2048;
const AUDIO_PAIRS_PULL: usize = 1024;
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
}

/// Where the launcher remembers its paths between runs.
fn settings_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(format!("{home}/.config/retro3do/launcher.conf"))
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
            "games = {}\nbios = {}\nsaves = {}\nmedia = {}\nbios_file = {}\n",
            self.games_dir.display(),
            self.bios_dir.display(),
            self.saves_dir.display(),
            self.media_dir.display(),
            self.bios_choice.clone().unwrap_or_default(),
        );
        std::fs::write(&path, text).map_err(|e| e.to_string())
    }

    fn from_args() -> Result<Self, String> {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let mut cfg = Config {
            games_dir: PathBuf::from(format!("{home}/3do-library/games")),
            media_dir: PathBuf::from(format!("{home}/StudioProjects/RetroMedia/media/3do")),
            bios_dir: PathBuf::from(format!("{home}/3do-library/bios")),
            saves_dir: PathBuf::from(format!("{home}/3do-library/saves")),
            boot: None,
            play: None,
            docs_dir: PathBuf::from(format!("{home}/3do-library/docs")),
            bios_choice: None,
            configured: false,
            texdir: std::env::var_os("RETRO3DO_TEXDIR").map(PathBuf::from),
        };
        // Remembered paths first; anything on the command line wins.
        let configured = cfg.load_settings();
        cfg.configured = configured;
        let mut args = std::env::args().skip(1);
        let mut dirs_set = false;
        while let Some(arg) = args.next() {
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
    /// Disc label art matched from the media `cartridges` directory, if any.
    disc_art: Option<PathBuf>,
    disc_key: Option<String>,
}

/// Lowercase, region tags removed, punctuation folded to spaces: the matching
/// key for both a game filename and a media filename.
fn normalize(stem: &str) -> String {
    let mut out = String::with_capacity(stem.len());
    let mut depth = 0i32;
    for ch in stem.chars() {
        match ch {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            _ if depth > 0 => {}
            '&' => out.push_str(" and "),
            c if c.is_alphanumeric() => out.extend(c.to_lowercase()),
            _ => out.push(' '),
        }
    }
    // ", The" moves to the front in some scrapers; the media library uses it.
    // Dropping the word entirely makes "The Need for Speed" and "Need for
    // Speed, The" the same key.
    let collapsed = out
        .split_whitespace()
        .filter(|w| *w != "the")
        .collect::<Vec<_>>()
        .join(" ");
    collapsed
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

/// Longest normalized media stem that appears inside the game's stem, or the
/// reverse. Earlier candidates win ties, so ordered dirs give the preference.
fn match_media<'a>(stem: &str, candidates: &'a [PathBuf]) -> Option<&'a PathBuf> {
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
        let stem = normalize(&file_stem(&path));
        let cover = match_media(&stem, &covers);
        let disc = match_media(&stem, &disc_art);
        games.push(Game {
            discs: vec![path.clone()],
            title: file_stem(&path),
            cover_key: cover.map(|c| c.to_string_lossy().into_owned()),
            cover: cover.cloned(),
            disc_key: disc.map(|d| d.to_string_lossy().into_owned()),
            disc_art: disc.cloned(),
            path,
        });
    }
    fold_disc_sets(&mut games);

    for (title, discs) in sets {
        let Some(first) = discs.first().cloned() else {
            continue;
        };
        let stem = normalize(&title);
        let cover = match_media(&stem, &covers);
        let label = match_media(&stem, &disc_art);
        games.push(Game {
            discs,
            title,
            cover_key: cover.map(|c| c.to_string_lossy().into_owned()),
            cover: cover.cloned(),
            disc_key: label.map(|d| d.to_string_lossy().into_owned()),
            disc_art: label.cloned(),
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
// SDL3: audio device stream and gamepads (no video — egui owns the window)

struct SoundAndPads {
    _sdl: sdl3::Sdl,
    stream: Option<AudioStreamOwner>,
    pads: Vec<Gamepad>,
}

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
        let gamepad = sdl.gamepad().ok();
        let mut pads = Vec::new();
        if let Some(g) = &gamepad {
            if let Ok(ids) = g.gamepads() {
                for id in ids.into_iter().take(MAX_PADS as usize - 1) {
                    if g.is_gamepad(id) {
                        if let Ok(p) = g.open(id) {
                            pads.push(p);
                        }
                    }
                }
            }
        }
        Some(SoundAndPads {
            _sdl: sdl,
            stream,
            pads,
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

    /// Pads after the keyboard: SDL pad i is console pad i+1.
    fn pad_mask(&self, index: usize) -> u32 {
        let mut mask = 0u32;
        if let Some(p) = self.pads.get(index) {
            for (button, pad) in [
                (Button::DPadUp, PadButton::Up),
                (Button::DPadDown, PadButton::Down),
                (Button::DPadLeft, PadButton::Left),
                (Button::DPadRight, PadButton::Right),
                (Button::South, PadButton::A),
                (Button::East, PadButton::B),
                (Button::West, PadButton::C),
                (Button::North, PadButton::C),
                (Button::Start, PadButton::Play),
                (Button::Back, PadButton::Stop),
                (Button::LeftShoulder, PadButton::LeftShift),
                (Button::RightShoulder, PadButton::RightShift),
            ] {
                if p.button(button) {
                    mask |= 1 << (pad as u32);
                }
            }
            const EDGE: i16 = 16000;
            if p.axis(Axis::LeftX) < -EDGE {
                mask |= 1 << (PadButton::Left as u32);
            }
            if p.axis(Axis::LeftX) > EDGE {
                mask |= 1 << (PadButton::Right as u32);
            }
            if p.axis(Axis::LeftY) < -EDGE {
                mask |= 1 << (PadButton::Up as u32);
            }
            if p.axis(Axis::LeftY) > EDGE {
                mask |= 1 << (PadButton::Down as u32);
            }
            if p.axis(Axis::TriggerLeft) > EDGE {
                mask |= 1 << (PadButton::LeftShift as u32);
            }
            if p.axis(Axis::TriggerRight) > EDGE {
                mask |= 1 << (PadButton::RightShift as u32);
            }
        }
        mask
    }

    fn count(&self) -> usize {
        self.pads.len()
    }
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
    /// The artwork currently on the model's disc, by its file path, so it is
    /// uploaded once per game rather than every frame, and whether anything
    /// has been put on the platter yet.
    fz1_disc_art: Option<PathBuf>,
    fz1_disc_set: bool,
    /// Media paths already asked of the decode thread.
    art_asked: std::collections::HashSet<String>,
    /// Where the selector is pointing: slot 0 is the empty tray, and the
    /// games follow it.
    selected: usize,
    /// The box art opened up, front and back side by side.
    art_zoom: bool,
    /// Whether the machine and the pads turn on their own.
    console_spin: bool,
    /// The slow turn the machine makes on launch, before anyone has taken
    /// hold of it. The first drag ends it for good; the ⟳ control's spin is
    /// a separate, asked-for thing.
    auto_spin: bool,
    /// Opacity of the "drag to rotate" hint: 1 while it is new, draining to
    /// 0 once it has had its moment or been acted on. `orbit_seen` counts the
    /// seconds the machine has been on screen as a control, so the hint's
    /// moment is measured by when it could actually be read.
    orbit_hint: f32,
    orbit_seen: f32,
    /// A game waiting for the drawer to shut before it goes in.
    pending_start: Option<usize>,
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
    /// When the window opened, and until when the boot-logo splash holds the
    /// screen once the extracted artwork has arrived.
    launched: Instant,
    splash_until: Option<Instant>,
    /// How far the badge wallpaper has drifted, in points.
    wall_drift: f32,
    /// Whether the model's drawer is out. The machine's own open/close button
    /// toggles it; powering on shuts it.
    fz1_tray_open: bool,
    /// How far the drawer zoom has come in, 0 to 1: eased toward the tray
    /// state so the eye closes on the drawer as it opens.
    drawer_zoom: f32,
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
    status: String,
    last_nvram_save: Instant,
}

impl GuiApp {
    fn new(cc: &eframe::CreationContext<'_>, cfg: Config) -> Self {
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = BG;
        visuals.window_fill = PANEL;
        cc.egui_ctx.set_visuals(visuals);
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
                yaw: 0.42,
                pitch: 0.38,
                pad_yaw: 0.0,
                pad_pitch: 1.25,
                ..Default::default()
            },
            fz1_tried: false,
            fz1_disc_art: None,
            fz1_disc_set: false,
            art_asked: std::collections::HashSet::new(),
            selected: 0,
            art_zoom: false,
            console_spin: false,
            auto_spin: true,
            orbit_hint: 1.0,
            orbit_seen: 0.0,
            pending_start: None,
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
            splash_until: None,
            wall_drift: 0.0,
            fz1_tray_open: true,
            drawer_zoom: 0.0,
            fz1_user_zoom: 0.65,
            game: None,
            disc_index: 0,
            guest_tray_since: None,
            scaler: None,
            frame_rgba: Vec::new(),
            last_frame: None,
            audio_buf: vec![0; AUDIO_PAIRS_PULL * 2],
            disc_angle: 0.0,
            status: String::new(),
            last_nvram_save: Instant::now(),
            cfg,
        };
        app.refresh_covers();
        app.apply_textures();
        // Powered off with an empty drawer, which is how a 3DO sits until
        // someone puts something in it.
        app.game = None;
        app.fz1_tray_open = false;
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
        if app.cfg.boot.is_none() && app.cfg.play.is_none() {
            app.splash_until = Some(app.launched + Duration::from_millis(1800));
        }
        // The real boot logo, pulled out of the chosen BIOS by the core
        // itself, arrives on the artwork channel like any other picture.
        app.extract_logo();
        if let Some(b) = app.cfg.boot {
            if b < app.games.len() {
                app.insert_disc(b);
            }
        }
        if let Some(p) = app.cfg.play {
            if p < app.games.len() {
                app.insert_disc(p);
                app.power_on();
            }
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
        let cands = scan_dir(&self.cfg.media_dir.join("box2d"), &["png", "jpg", "jpeg"]);
        for g in &mut self.games {
            let stem = normalize(&file_stem(&g.path));
            let m = match_media(&stem, &cands);
            g.cover_key = m.map(|c| c.to_string_lossy().into_owned());
            g.cover = m.cloned();
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

    /// Take the disc out. The machine keeps running and falls back to the
    /// BIOS, which is what the console does with an empty tray.
    fn eject_disc(&mut self) {
        self.save_nvram();
        let running = self.emu.running();
        if running {
            self.emu.stop();
        }
        self.emu.eject_disc();
        self.game = None;
        self.disc_index = 0;
        self.guest_tray_since = None;
        self.fz1_tray_open = true;
        self.pending_start = None;
        self.picture_live = false;
        self.status = "disc out — the BIOS is waiting".to_string();
        self.view = View::Machine;
        if running {
            self.emu.start();
            self.emu.request_reset();
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
        self.emu.start();
        self.picture_live = false;
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

    fn push_pads(&self, ctx: &egui::Context) {
        let mut connected = 0u32;
        // Pad 0 is the keyboard plus SDL pad 0 so a gamepad always works even
        // without mapping keys. Extra SDL pads chain behind.
        let keys = keyboard_mask(ctx);
        let mut mask = keys;
        if let Some(io) = &self.io {
            mask |= io.pad_mask(0);
            for i in 0..io.count() {
                let idx = (i + 1) as u32;
                if idx < MAX_PADS {
                    self.emu.pads().set_buttons(idx, io.pad_mask(i + 1));
                    self.emu.pads().set_connected(idx, true);
                    connected = idx + 1;
                }
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
                let start = PathBuf::from(if start.is_empty() {
                    std::env::var("HOME").unwrap_or_else(|_| "/".to_string())
                } else {
                    start.to_string()
                });
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
                                let field = match self.picker_step {
                                    1 => &mut self.setup_bios,
                                    2 => &mut self.setup_games,
                                    _ => &mut self.setup_media,
                                };
                                *field = current.to_string_lossy().into_owned();
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
    fn art_overlay(&mut self, ctx: &egui::Context) {
        let screen = ctx.content_rect();
        egui::Area::new(egui::Id::new("art-zoom"))
            .fixed_pos(screen.min)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                let (rect, press) =
                    ui.allocate_exact_size(screen.size(), egui::Sense::click());
                ui.painter()
                    .rect_filled(rect, 0.0, egui::Color32::from_black_alpha(230));
                let pad = 24.0;
                let half = (rect.width() - pad * 3.0) * 0.5;
                let height = rect.height() - pad * 2.0;
                let Some(game) = self.selected_game() else {
                    self.art_zoom = false;
                    return;
                };
                for (i, sub) in ["box2d", "boxback"].iter().enumerate() {
                    let at = egui::Rect::from_min_size(
                        egui::pos2(rect.min.x + pad + (half + pad) * i as f32, rect.min.y + pad),
                        egui::vec2(half, height),
                    );
                    let path = self.games.get(game).and_then(|g| self.media_for(g, sub));
                    if let Some(id) = path.and_then(|p| self.art_texture(p.as_path())) {
                        ui.painter().image(
                            id,
                            at,
                            egui::Rect::from_min_max(
                                egui::pos2(0.0, 0.0),
                                egui::pos2(1.0, 1.0),
                            ),
                            egui::Color32::WHITE,
                        );
                    }
                }
                if press.clicked() {
                    self.art_zoom = false;
                }
            });
    }

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
    fn media_for(&self, game: &Game, sub: &str) -> Option<PathBuf> {
        let candidates = scan_dir(&self.cfg.media_dir.join(sub), &["png", "jpg", "jpeg"]);
        let stem = normalize(&file_stem(&game.path));
        match_media(&stem, &candidates).cloned()
    }

    /// The disc-label picture: the art that belongs on a disc.
    fn disc_label(&self, game: &Game) -> Option<PathBuf> {
        self.media_for(game, "cartridges")
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

    /// The game selector: seven marquees at a time with the chosen one in
    /// the middle. Clicking a neighbour walks to it; clicking the middle one
    /// puts it in and closes the drawer.
    fn game_selector(&mut self, ui: &mut egui::Ui, height: f32) {
        if self.games.is_empty() {
            ui.label(
                egui::RichText::new("no discs in the library folder")
                    .small()
                    .color(TEXT_DIM),
            );
            return;
        }
        // Slot 0 is an empty tray: nothing chosen, which is where the
        // machine starts.
        let count = self.games.len() + 1;
        self.selected = self.selected.min(count - 1);
        let room = ui.available_width();
        let mut shown = 7usize.min(count);
        while shown > 1 && (TILE_W + 6.0) * shown as f32 > room {
            shown -= 2;
        }
        let half = (shown / 2) as isize;
        let width = TILE_W;
        let row_w = (width + 6.0) * shown as f32;
        let margin = ((room - row_w) * 0.5).max(0.0);
        ui.horizontal(|ui| {
            ui.add_space(margin);
            for slot in -half..=half {
                let index = (self.selected as isize + slot).rem_euclid(count as isize) as usize;
                let middle = slot == 0;
                let path = index
                    .checked_sub(1)
                    .and_then(|g| self.games.get(g))
                    .and_then(|g| self.media_for(g, "marquee"));
                let tex = path.and_then(|p| self.art_texture(p.as_path()));
                let size = egui::vec2(width, if middle { height } else { height * 0.82 });
                let (rect, press) = ui.allocate_exact_size(size, egui::Sense::click());
                let painter = ui.painter();
                painter.rect_filled(rect, 4.0, if middle { CARD } else { PANEL });
                if let Some(id) = tex {
                    painter.image(
                        id,
                        rect.shrink(2.0),
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        egui::Color32::WHITE,
                    );
                } else {
                    let label = match index.checked_sub(1).and_then(|g| self.games.get(g)) {
                        Some(game) => game.title.clone(),
                        None => "no disc".to_string(),
                    };
                    painter.text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        label,
                        egui::FontId::proportional(11.0),
                        TEXT_DIM,
                    );
                }
                painter.rect_stroke(
                    rect,
                    4.0,
                    egui::Stroke::new(
                        if middle { 2.0 } else { 1.0 },
                        if middle {
                            egui::Color32::WHITE
                        } else if press.hovered() {
                            TEXT_DIM
                        } else {
                            PANEL
                        },
                    ),
                    StrokeKind::Inside,
                );
                if press.clicked() && self.fz1_tray_open {
                    // A neighbour walks the shelf; the middle tile is the
                    // chosen one already, so its click opens the art instead.
                    if middle && self.selected_game().is_some() {
                        self.art_zoom = true;
                    } else {
                        self.selected = index;
                    }
                }
            }
        });
        // The arrow keys walk the row. Nothing else moves it — a stray
        // wheel event anywhere else in the window must not spin the shelf.
        let step = if self.fz1_tray_open {
            ui.ctx().input(|i| {
                i.key_pressed(egui::Key::ArrowRight) as i32
                    - i.key_pressed(egui::Key::ArrowLeft) as i32
            })
        } else {
            0
        };
        if step != 0 {
            self.selected =
                (self.selected as isize + step as isize).rem_euclid(count as isize) as usize;
        }
    }

    /// Work the drawer. Shutting it on a chosen disc loads that disc, the
    /// way the machine does; powering on is still the user's to press.
    /// The game the selector is on, if it is on one at all: slot 0 is an
    /// empty tray.
    fn selected_game(&self) -> Option<usize> {
        self.selected.checked_sub(1)
    }

    fn toggle_tray(&mut self) {
        self.fz1_tray_open = !self.fz1_tray_open;
        if !self.fz1_tray_open {
            self.pending_start = self.selected_game();
        }
    }

    /// Drive the start the drawer is holding up, once it has closed.
    fn advance_start(&mut self) {
        if self.fz1_state.tray > 0.02 {
            return;
        }
        if let Some(index) = self.pending_start.take() {
            self.insert_disc(index);
            // Shutting the drawer on a disc starts it: a machine that was
            // off comes on by itself rather than waiting to be noticed.
            if !self.emu.running() {
                self.power_on();
            }
        }
    }

    /// Paint the FZ-1 model into `rect`, returning whether it drew. The model
    /// loads on first use and the renderer is built from egui's own wgpu
    /// device, so it shares the window's surface. `fade` below 1.0 is the
    /// console leaving as the game takes over: it sinks and goes see-through,
    /// and its controls stop answering.
    fn draw_fz1(
        &mut self,
        ui: &mut egui::Ui,
        frame: &mut eframe::Frame,
        rect: egui::Rect,
        fade: f32,
    ) -> bool {
        if !self.fz1_tried {
            self.fz1_tried = true;
            if let Some(console) = fz1_3d::load_model() {
                self.fz1_scene.push(console);
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
        }

        // The disc wears the disc's own label art, not the box cover, and
        // falls back to whatever the gallery has if a title has no label.
        let art = match self.view {
            View::Setup => None,
            View::Machine => {
                let game = self.selected_game().and_then(|g| self.games.get(g));
                game.and_then(|g| self.disc_label(g))
                    .or_else(|| game.and_then(|g| g.cover.clone()))
            }
        };
        let Some(renderer) = self.fz1.as_mut() else {
            return false;
        };
        if art != self.fz1_disc_art || !self.fz1_disc_set {
            self.fz1_disc_art = art.clone();
            self.fz1_disc_set = true;
            if art.is_none() {
                // Nothing chosen: a blank platter in the drawer's own colour,
                // rather than the tray showing through where a disc goes.
                let size = fz1_3d::Fz1Renderer::ART_SIZE;
                let blank: Vec<u8> = std::iter::repeat_n([38u8, 38, 43, 255], (size * size) as usize)
                    .flatten()
                    .collect();
                renderer.set_disc_art(state, &blank, size, size);
            }
            if let Some(image) = art.as_ref().and_then(|p| image::open(p).ok()) {
                // Square, at exactly the disc texture's size, so the label
                // covers the disc instead of a corner of it.
                let size = fz1_3d::Fz1Renderer::ART_SIZE;
                let image = image
                    .resize_exact(size, size, image::imageops::FilterType::Lanczos3)
                    .to_rgba8();
                renderer.set_disc_art(state, image.as_raw(), size, size);
            }
        }

        // The drawer follows the open/close button, and shuts itself when the
        // machine runs. The disc turns on the same angle the drawn console
        // uses.
        let running = self.emu.running();
        self.fz1_state.tray_goal = if self.fz1_tray_open { 1.0 } else { 0.0 };
        let step = ui.ctx().input(|i| i.stable_dt).clamp(0.0, 0.1) * 2.0;
        let gap = self.fz1_state.tray_goal - self.fz1_state.tray;
        self.fz1_state.tray += gap.clamp(-step, step);

        // The eye comes in as the drawer opens and backs off as it shuts:
        // about 0.4 s each way, smoothstepped so it eases at both ends.
        // `zoom` is a distance multiplier, so in is below 1.0.
        let dt = ui.ctx().input(|i| i.stable_dt).clamp(0.0, 0.1);
        let zoom_goal = if self.fz1_tray_open { 1.0 } else { 0.0 };
        self.drawer_zoom += (zoom_goal - self.drawer_zoom).clamp(-dt / 0.4, dt / 0.4);
        let e = self.drawer_zoom * self.drawer_zoom * (3.0 - 2.0 * self.drawer_zoom);
        self.fz1_state.zoom = (self.fz1_user_zoom * (1.0 - 0.38 * e)).clamp(0.45, 3.0);
        self.fz1_state.spin = self.disc_angle;
        let turn = ui.ctx().input(|i| i.stable_dt).clamp(0.0, 0.1);
        if self.console_spin {
            self.fz1_state.yaw += turn * 0.7;
        } else if self.auto_spin {
            self.fz1_state.yaw += turn * 0.3;
        }
        self.fz1_state.lit = running;
        // The ACCESS lamp only ever shows disc access: it blinks while the
        // BIOS is still coming up and again while the drawer runs out.
        let booting = running && !self.picture_live;
        let ejecting =
            running && self.fz1_state.tray_goal > 0.5 && self.fz1_state.tray < 0.98;
        let t = ui.ctx().input(|i| i.time);
        self.fz1_state.access = if booting || ejecting {
            ((t * 5.0).fract() < 0.6) as u8 as f32
        } else {
            0.0
        };

        let ppt = ui.ctx().pixels_per_point();
        let w = (rect.width() * ppt).round().max(1.0) as u32;
        let h = (rect.height() * ppt).round().max(1.0) as u32;
        let Some(tex) = renderer.render_into(state, &self.fz1_scene, &self.fz1_state, (w, h), 0, 0..1)
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
        if fade < 0.999 {
            // Leaving or arriving: the machine is scenery, not a control.
            ui.ctx().request_repaint();
            return true;
        }

        // Turn it with a drag, come closer with the wheel.
        let turn = ui.interact(
            rect,
            ui.id().with("fz1-orbit"),
            egui::Sense::click_and_drag(),
        );
        if turn.dragged() {
            let drag = turn.drag_delta();
            self.fz1_state.yaw += drag.x * 0.01;
            self.fz1_state.pitch = (self.fz1_state.pitch - drag.y * 0.01).clamp(-1.2, 1.2);
            // Taking hold of the machine ends the launch turn for good.
            self.auto_spin = false;
        }
        if turn.hovered() {
            let wheel = ui.ctx().input(|i| i.smooth_scroll_delta.y);
            if wheel != 0.0 {
                self.fz1_user_zoom =
                    (self.fz1_user_zoom * (1.0 - wheel * 0.002)).clamp(0.45, 3.0);
            }
        }

        // The model's own buttons, wherever they have turned to: each part is
        // projected to the picture, and the ones that are controls get a hit
        // target there.
        let mut hit: Option<&'static str> = None;
        for (name, at, depth) in renderer.screen_points() {
            let control = match name.as_str() {
                n if n.contains("Power") => "power",
                n if n.contains("Eject") || n.contains("Open") => "tray",
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
            let press = ui.interact(spot, ui.id().with(("fz1", control)), egui::Sense::click());
            if press.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                ui.painter()
                    .circle_stroke(centre, size * 0.5, egui::Stroke::new(1.5, ACCENTS[3]));
            }
            // Only a deliberate press on the part being pointed at: a click
            // buffered from focusing the window must not eject the disc.
            if press.hovered() && press.clicked_by(egui::PointerButton::Primary) {
                hit = Some(control);
            }
        }
        if let Some(control) = hit {
            match (control, self.emu.running()) {
                ("power", false) => self.power_on(),
                ("power", true) => self.power_off(),
                // The machine's own open/close button works the drawer both
                // ways; EJECT is what gives the disc back.
                ("tray", _) => self.toggle_tray(),
                _ => {}
            }
        }

        // The orbit hint: a few seconds once the machine is on screen, or
        // until the first drag shows it was not needed, then it fades and
        // does not come back.
        if self.orbit_hint > 0.0 {
            self.orbit_seen += dt;
            if self.orbit_seen > 6.0 || !self.auto_spin {
                self.orbit_hint = (self.orbit_hint - dt / 1.2).max(0.0);
            }
            if self.orbit_hint > 0.0 {
                ui.painter().text(
                    egui::pos2(rect.center().x, rect.max.y - 34.0),
                    egui::Align2::CENTER_CENTER,
                    "drag to rotate · wheel to zoom",
                    egui::FontId::proportional(11.0),
                    TEXT_DIM.gamma_multiply(self.orbit_hint),
                );
            }
        }

        self.spin_handle(ui, rect);
        ui.ctx().request_repaint();
        true
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
            let tex = ctx.load_texture(key.clone(), img, TextureOptions::LINEAR);
            self.covers.insert(key, tex);
        }
    }
}

impl eframe::App for GuiApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        while let Ok((key, img)) = self.covers_rx.try_recv() {
            if key == LOGO_KEY
                && self.splash_until.is_none()
                && self.launched.elapsed() < Duration::from_secs(4)
                && self.cfg.boot.is_none()
                && self.cfg.play.is_none()
            {
                self.splash_until = Some(Instant::now() + Duration::from_millis(1800));
            }
            self.pending_images.push((key, img));
        }
        self.upload_pending(&ctx);
        self.push_pads(&ctx);
        self.poll_frame();

        // Disc spin: full speed while the machine runs, a slow show-off turn
        // while a disc just sits in the tray.
        let running = self.emu.running();
        let dt = ctx.input(|i| i.stable_dt).min(0.1);
        if self.emu.disc_loaded() {
            self.disc_angle += dt * if running { 9.0 } else { 0.6 };
            ctx.request_repaint();
        }
        // A game asking for the next disc ejects the tray; answer it.
        self.poll_disc_request();

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
        self.advance_start();
        // A header the whole time: the emulator's name, and what is in the
        // machine.
        egui::Panel::top("header").exact_size(44.0).show(ui, |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                ui.label(
                    egui::RichText::new("RETRO 3DO")
                        .strong()
                        .size(18.0)
                        .color(egui::Color32::from_rgb(210, 212, 220)),
                );
                let title = self
                    .games
                    .get(self.selected.saturating_sub(1))
                    .map(|g| g.title.clone())
                    .unwrap_or_default();
                let discs = self
                    .selected_game()
                    .and_then(|g| self.games.get(g))
                    .map_or(1, |g| g.discs.len());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(14.0);
                    ui.label(
                        egui::RichText::new(if discs > 1 {
                            format!("{title}  ·  {discs} discs")
                        } else {
                            title
                        })
                        .color(TEXT_DIM),
                    );
                });
            });
        });
        if self.art_zoom {
            self.art_overlay(&ctx);
        }

        if !self.status.is_empty() {
            egui::Panel::bottom("status").show(ui, |ui| {
                ui.label(egui::RichText::new(&self.status).small().color(TEXT_DIM));
            });
        }

        egui::CentralPanel::default().show(ui, |ui| match self.view {
            View::Setup => self.view_setup(ui),
            View::Machine => self.view_picture(ui, &ctx, frame),
        });

        if self.picker.is_some() {
            self.dir_picker_overlay(&ctx);
        }
        if let Some(until) = self.splash_until {
            if Instant::now() < until {
                egui::Area::new(egui::Id::new("boot-splash"))
                    .order(egui::Order::Foreground)
                    .show(&ctx, |ui| {
                        let screen = ctx.content_rect();
                        ui.painter()
                            .rect_filled(screen, 0.0, egui::Color32::BLACK);
                        self.paint_logo(ui, screen, 0.85);
                    });
                ctx.request_repaint();
            } else {
                self.splash_until = None;
            }
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
        // Only set picture_live after the boot splash has cleared.
        if lit && self.splash_until.is_none() {
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
        let live = self.emu.running() && self.picture_live && !self.fz1_tray_open;
        let dt = ctx.input(|i| i.stable_dt).clamp(0.0, 0.1);
        let goal = if live { 0.0 } else { 1.0 };
        self.console_fade += (goal - self.console_fade).clamp(-dt / 0.5, dt / 0.5);
        if !live && self.console_fade >= 0.999 {
            self.view_machine(ui, frame);
            return;
        }
        self.view_screen(ui, ctx, frame);
        if self.console_fade > 0.001 {
            // Mid-crossing: the machine sinks away over the new picture.
            let area = ui.available_rect_before_wrap();
            ui.scope_builder(egui::UiBuilder::new().max_rect(area), |ui| {
                self.draw_fz1(ui, frame, area, self.console_fade);
            });
            ctx.request_repaint();
        }
    }

    /// The machine, filling the window at one size whatever is going on,
    /// with the selector along the bottom while the drawer is out.
    fn view_machine(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let area = ui.available_rect_before_wrap();
        ui.painter().rect_filled(area, 0.0, BG);
        self.wallpaper(ui, area);
        let tiles = if self.fz1_tray_open { TILE_H + 18.0 } else { 0.0 };
        let scene = egui::Rect::from_min_max(
            area.min,
            egui::pos2(area.max.x, area.max.y - tiles),
        );
        self.view_attract(ui, frame, scene);
        if self.fz1_tray_open {
            self.selector_strip(ui, area, tiles);
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
        self.wall_drift += dt * 8.0;
        let pitch_y = 190.0;
        let badge_h = pitch_y;
        let badge_w = pitch_y * tex.size_vec2().x / tex.size_vec2().y.max(1.0);
        let pitch_x = badge_w * 2.1;
        let uv =
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        let tint = egui::Color32::from_white_alpha(18);
        let painter = ui.painter();
        let mut row = 0usize;
        let mut y = area.min.y - pitch_y + self.wall_drift % pitch_y;
        while y < area.max.y {
            let mut x = area.min.x - pitch_x + pitch_x * 0.5 * (row % 2) as f32;
            while x < area.max.x {
                painter.image(
                    tex.id(),
                    egui::Rect::from_min_size(
                        egui::pos2(x, y),
                        egui::vec2(badge_w, badge_h),
                    ),
                    uv,
                    tint,
                );
                x += pitch_x;
            }
            y += pitch_y;
            row += 1;
        }
        ui.ctx().request_repaint();
    }

    /// The selector on its card along the bottom.
    fn selector_strip(&mut self, ui: &mut egui::Ui, area: egui::Rect, tiles: f32) {
        let strip = egui::Rect::from_min_max(
            egui::pos2(area.min.x, area.max.y - tiles),
            area.max,
        );
        ui.painter().rect(
            strip.shrink2(egui::vec2(10.0, 4.0)),
            8.0,
            CARD,
            egui::Stroke::new(1.0, PANEL),
            StrokeKind::Inside,
        );
        ui.scope_builder(
            egui::UiBuilder::new().max_rect(strip.shrink2(egui::vec2(16.0, 10.0))),
            |ui| {
                self.game_selector(ui, TILE_H);
            },
        );
    }

    /// Nothing on the screen yet: the machine is the whole view, drawn to
    /// fill the scene while keeping its shape, with only a quiet word on
    /// what to press floated along the bottom.
    fn view_attract(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame, scene: egui::Rect) {
        let drawn = ui
            .scope_builder(egui::UiBuilder::new().max_rect(scene), |ui| {
                self.draw_fz1(ui, frame, scene, 1.0)
            })
            .inner;
        if !drawn {
            ui.painter().rect_filled(scene, 6.0, PANEL);
        }
        ui.painter().text(
            egui::pos2(scene.center().x, scene.max.y - 12.0),
            egui::Align2::CENTER_CENTER,
            if self.fz1_tray_open {
                "pick a disc below, then shut the drawer"
            } else {
                "press OPEN/CLOSE on the machine to choose a disc"
            },
            egui::FontId::proportional(12.0),
            TEXT_DIM,
        );
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
            if !self.fz1_tray_open {
                self.eject_corner(ui, rect);
            }
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
                    target_h == 0,
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
                match &mut self.frame_tex {
                    Some(tex) if tex.size() == img.size => tex.set(img, TextureOptions::NEAREST),
                    _ => {
                        self.frame_tex =
                            Some(ctx.load_texture("frame", img, TextureOptions::NEAREST))
                    }
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

        // Overlay chrome appears when the pointer moves near the top, and keys
        // still work: Esc powers off to the console.
        let pointer_y = ctx.input(|i| i.pointer.latest_pos().map_or(f32::MAX, |p| p.y));
        let show_bar = pointer_y < rect.min.y + 64.0;
        if show_bar {
            let bar = egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x, rect.min.y + 44.0));
            painter.rect_filled(bar, 0.0, egui::Color32::from_black_alpha(170));
            ui.scope_builder(
                egui::UiBuilder::new().max_rect(bar.shrink2(egui::vec2(8.0, 6.0))),
                |ui| {
                    ui.horizontal(|ui| {
                        if ui.button("⏸").on_hover_text("pause").clicked() {
                            self.emu.set_paused(!self.emu.paused());
                        }
                        if ui.button("⟲").on_hover_text("reset").clicked() {
                            self.emu.request_reset();
                        }
                        if ui.button("✕").on_hover_text("power off").clicked() {
                            self.power_off();
                        }
                        let stats = self.emu.stats();
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                egui::RichText::new(format!("{:.0} fps", stats.emulated_fps))
                                    .small()
                                    .color(TEXT_DIM),
                            );
                        });
                    });
                },
            );
        }
        if !self.fz1_tray_open {
            self.disc_swap_strip(ui, rect);
            self.eject_corner(ui, rect);
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.fz1_tray_open = true;
        }
        ctx.request_repaint_after(Duration::from_millis(8));
    }

    /// The turn handle in a view's top right: a click sets it spinning, a
    /// second click stops it. Dragging the view still turns it by hand.
    fn spin_handle(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        let size = 20.0;
        let spot = egui::Rect::from_min_size(
            egui::pos2(rect.max.x - size - 6.0, rect.min.y + 6.0),
            egui::vec2(size, size),
        );
        let press = ui.interact(spot, ui.id().with("spin"), egui::Sense::click());
        let on = self.console_spin;
        let painter = ui.painter();
        painter.rect_filled(spot, 4.0, egui::Color32::from_black_alpha(140));
        painter.text(
            spot.center(),
            egui::Align2::CENTER_CENTER,
            "⟳",
            egui::FontId::proportional(14.0),
            if on || press.hovered() {
                egui::Color32::WHITE
            } else {
                TEXT_DIM
            },
        );
        if press.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if press.clicked() {
            self.console_spin = !self.console_spin;
        }
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

    /// The eject corner: the way back to choosing a game from the picture.
    fn eject_corner(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        let size = 34.0;
        let spot = egui::Rect::from_min_size(
            egui::pos2(rect.max.x - size - 12.0, rect.min.y + 12.0),
            egui::vec2(size, size),
        );
        let press = ui.interact(spot, ui.id().with("eject-corner"), egui::Sense::click());
        let painter = ui.painter();
        painter.rect_filled(
            spot,
            6.0,
            egui::Color32::from_black_alpha(if press.hovered() { 220 } else { 150 }),
        );
        painter.text(
            spot.center(),
            egui::Align2::CENTER_CENTER,
            "⏏",
            egui::FontId::proportional(18.0),
            if press.hovered() {
                egui::Color32::WHITE
            } else {
                TEXT_DIM
            },
        );
        if press.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if press.clicked() {
            self.eject_disc();
        }
    }

}


fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = match Config::from_args() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(if e.starts_with("usage") { 0 } else { 2 });
        }
    };

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
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
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
}
