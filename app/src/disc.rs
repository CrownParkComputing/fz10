#![forbid(unsafe_code)]
//! Disc images.
//!
//! This layer knows nothing about the 3DO. It turns a file into "give me
//! logical block N as 2048 bytes of user data", plus a track list, and it is
//! the part that has to understand the several ways a CD can be stored in a
//! file: cooked 2048-byte sectors, raw 2352-byte ones in mode 1 or mode 2, cue
//! sheets naming a binary image, and CHD archives.
//!
//! The sector-stride problem
//! -------------------------
//! A CD sector is 2352 bytes: sync, header, user data, error correction. Only
//! some of that is payload, and where the payload starts depends on the mode.
//! A dump may hold whole raw sectors or only the cooked 2048-byte payload, and
//! the extension does not say which. Reading a raw image as though it were
//! cooked *almost* works — the first 2048 bytes of sector zero really are data
//! — and then every later sector is off by 304 bytes and looks like a corrupt
//! disc rather than a misread one. So the layout is always detected from the
//! sync pattern, never assumed from the name.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use crate::chd::{
    probe_chd, ChdFile, ChdInfo, CDROM_TRACK_METADATA2_TAG, CDROM_TRACK_METADATA_TAG,
    CD_MAX_SECTOR_DATA, CD_TRACK_PADDING,
};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A CD sector's user area is always this, whatever the container.
pub const SECTOR_USER_BYTES: u32 = 2048;
/// A whole raw sector.
pub const RAW_SECTOR_BYTES: u32 = 2352;

/// Every raw CD sector starts with this. Finding it is how a raw image is told
/// from a cooked one.
const SYNC_PATTERN: [u8; 12] = [
    0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00,
];

/// How sectors are laid out in a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SectorLayout {
    /// User data only.
    #[default]
    Cooked2048,
    /// sync(12) + header(4) + user(2048) + ecc(288)
    Raw2352Mode1,
    /// sync(12) + header(4) + subheader(8) + user(2048) + edc(280)
    Raw2352Mode2,
    /// subheader(8) + user(2048) + edc(280)
    Raw2336Mode2,
}

impl SectorLayout {
    /// Bytes each sector takes in the file.
    pub fn bytes_per_sector(self) -> u32 {
        match self {
            SectorLayout::Cooked2048 => 2048,
            SectorLayout::Raw2352Mode1 | SectorLayout::Raw2352Mode2 => 2352,
            SectorLayout::Raw2336Mode2 => 2336,
        }
    }

    /// Where the 2048 bytes of user data start within a sector.
    pub fn user_offset(self) -> u32 {
        match self {
            SectorLayout::Cooked2048 => 0,
            // sync(12) + header(4)
            SectorLayout::Raw2352Mode1 => 16,
            // sync(12) + header(4) + subheader(8). Missing this eight-byte
            // subheader is what makes a mode 2 disc read as almost-right
            // garbage.
            SectorLayout::Raw2352Mode2 => 24,
            SectorLayout::Raw2336Mode2 => 8,
        }
    }
}

/// Bytes each sector of `layout` takes in the file.
pub fn bytes_per_sector(layout: SectorLayout) -> u32 {
    layout.bytes_per_sector()
}

/// Where the user data starts within a sector of `layout`.
pub fn user_offset_in_sector(layout: SectorLayout) -> u32 {
    layout.user_offset()
}

/// One track of a disc.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Track {
    pub number: u32,
    pub is_audio: bool,
    pub start_lba: u32,
    pub length_sectors: u32,
}

/// A CHD being read as a disc: the geometry that turns a logical block into
/// a place inside a hunk, and the decoded hunks a worker keeps ahead.
///
/// A CD hunk is eight frames, and decompressing one costs an LZMA or FLAC
/// decode — several milliseconds, occasionally far more on a slow disk.
/// Done on the emulation thread inside `read_sector` that cost lands
/// mid-field and judders the frame the sound and picture are waiting on, so
/// the file lives on a worker thread that decodes a window ahead of the
/// reader instead.
struct ChdReader {
    prefetch: Arc<ChdPrefetch>,
    worker: Option<std::thread::JoinHandle<()>>,
    unit_bytes: u32,
    frames_per_hunk: u32,
    /// Where the first data track's frames begin inside the CHD. Tracks are
    /// stored one after another, each padded to a multiple of four frames, so
    /// this is not always zero.
    data_track_frame: u32,
    /// Where a sector's user data begins inside its frame. A MODE1 track
    /// holds the user data alone, while a MODE1_RAW track holds the whole
    /// sector with twelve bytes of sync and a four-byte header in front of
    /// it; taking the payload from offset zero in both makes the raw one
    /// yield sync bytes where the filesystem should be. Most 3DO dumps in
    /// circulation are MODE1_RAW.
    payload_offset: u32,
    /// The hunk the last frame came from: a hit repeats eight times.
    current: Option<(u32, Arc<Vec<u8>>)>,
}

/// How far past the reader's position the worker decodes, in hunks. Eight
/// hunks is sixty-four sectors — over a second of double-speed streaming —
/// while the drive's sector pacing gives the worker far longer than a decode
/// takes to stay ahead.
const PREFETCH_HUNKS: u32 = 8;

/// The worker's side of the reader: decoded hunks waiting to be consumed.
struct ChdPrefetch {
    state: Mutex<PrefetchState>,
    ready: Condvar,
}

struct PrefetchState {
    /// The hunk the reader most recently asked for; the window starts here.
    want: u32,
    /// Hunk -> decoded bytes. `None` marks a read that failed, so the reader
    /// learns it instead of waiting for a hunk that will never land.
    hunks: BTreeMap<u32, Option<Arc<Vec<u8>>>>,
    stop: bool,
}

/// Decodes hunks ahead of the reader until told to stop. Owns the file:
/// every CHD read — including the reader's own misses — happens here.
fn prefetch_loop(file: &mut ChdFile<File>, hunk_bytes: usize, shared: &ChdPrefetch) {
    loop {
        let target = {
            let mut state = lock(&shared.state);
            loop {
                if state.stop {
                    return;
                }
                // The lowest undecoded hunk in the window ahead of the reader.
                let end = state.want.saturating_add(PREFETCH_HUNKS);
                match (state.want..end).find(|h| !state.hunks.contains_key(h)) {
                    Some(hunk) => break hunk,
                    None => state = shared.ready.wait(state).unwrap_or_else(PoisonError::into_inner),
                }
            }
        };
        let mut hunk = vec![0u8; hunk_bytes];
        let decoded = file.read_hunk(target, &mut hunk).ok().map(|_| Arc::new(hunk));
        let mut state = lock(&shared.state);
        state.hunks.insert(target, decoded);
        // Nothing below the reader's position is coming back into use.
        let want = state.want;
        state.hunks.retain(|hunk, _| *hunk >= want);
        shared.ready.notify_all();
    }
}

impl ChdReader {
    /// The decoded hunk `index`, waiting on the worker if it has to. `None`
    /// when the read failed or the worker is gone.
    fn hunk(&mut self, index: u32) -> Option<Arc<Vec<u8>>> {
        let mut state = lock(&self.prefetch.state);
        state.want = index;
        self.prefetch.ready.notify_one();
        loop {
            if let Some(decoded) = state.hunks.get(&index) {
                return decoded.clone();
            }
            // Stopped, or the worker died holding the other reference: a hunk
            // that will never land is a failed read, not a hang.
            if state.stop || Arc::strong_count(&self.prefetch) < 2 {
                return None;
            }
            state = self
                .prefetch
                .ready
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Copies 2352 bytes of a stored frame, starting at the payload, into
    /// `out`.
    fn read_frame(&mut self, frame: u32, out: &mut [u8]) -> bool {
        let index = frame / self.frames_per_hunk;
        let within = (frame % self.frames_per_hunk) as usize * self.unit_bytes as usize;
        if self.current.as_ref().map(|(hunk, _)| *hunk) != Some(index) {
            self.current = self.hunk(index).map(|decoded| (index, decoded));
        }
        let Some((_, hunk)) = &self.current else {
            return false;
        };
        let start = within + self.payload_offset as usize;
        let length = CD_MAX_SECTOR_DATA as usize;
        match (hunk.get(start..start + length), out.get_mut(..length)) {
            (Some(src), Some(dst)) => {
                dst.copy_from_slice(src);
                true
            }
            _ => false,
        }
    }
}

impl Drop for ChdReader {
    fn drop(&mut self) {
        lock(&self.prefetch.state).stop = true;
        self.prefetch.ready.notify_one();
        // At most one in-flight decode: quick, and it frees the file on the
        // way out rather than some later moment nobody can see.
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

enum Source {
    Closed,
    Image(File),
    Chd(Box<ChdReader>),
}

/// A disc image open for reading.
pub struct Disc {
    source: Source,
    path: String,
    last_error: String,
    layout: SectorLayout,
    chd: ChdInfo,
    sector_count: u32,
    tracks: Vec<Track>,
}

impl Default for Disc {
    fn default() -> Self {
        Disc::new()
    }
}

/// How far into a frame chdman's track types put the user data.
fn chd_payload_offset(track_type: &str) -> u32 {
    match track_type {
        // Twelve bytes of sync, then a four-byte header.
        "MODE1_RAW" | "MODE1/2352" => 16,
        // Sync and header, then an eight-byte subheader.
        "MODE2_RAW" | "MODE2/2352" | "MODE2_FORM_MIX" => 24,
        // Subheader only.
        "MODE2" | "MODE2/2336" => 8,
        // MODE1, MODE1/2048, MODE2_FORM1, MODE2_FORM2 and AUDIO all begin at
        // the payload.
        _ => 0,
    }
}

fn lowercase_extension(path: &str) -> String {
    path.rfind('.')
        .map(|dot| path.get(dot..).unwrap_or("").to_ascii_lowercase())
        .unwrap_or_default()
}

fn directory_of(path: &str) -> &str {
    path.rfind(['/', '\\'])
        .and_then(|slash| path.get(..=slash))
        .unwrap_or("")
}

fn file_size(file: &mut File) -> u64 {
    let here = file.stream_position().unwrap_or(0);
    let size = file.seek(SeekFrom::End(0)).unwrap_or(0);
    let _ = file.seek(SeekFrom::Start(here));
    size
}

/// Whitespace as C's `isspace` sees it.
fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

fn trimmed(text: &[u8]) -> &[u8] {
    let start = text
        .iter()
        .position(|b| !is_space(*b))
        .unwrap_or(text.len());
    let end = text
        .iter()
        .rposition(|b| !is_space(*b))
        .map_or(start, |last| last + 1);
    text.get(start..end).unwrap_or(&[])
}

fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    let rest = haystack.get(from..)?;
    if needle.is_empty() {
        return Some(from);
    }
    rest.windows(needle.len())
        .position(|window| window == needle)
        .map(|at| at + from)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    find(haystack, needle, 0).is_some()
}

/// A leading integer the way C's `atoi` reads one: whitespace, a sign,
/// digits, and zero when there are none.
fn atoi(text: &[u8]) -> i32 {
    let mut bytes = text.iter().copied().skip_while(|b| is_space(*b)).peekable();
    let negative = match bytes.peek() {
        Some(b'-') => {
            bytes.next();
            true
        }
        Some(b'+') => {
            bytes.next();
            false
        }
        _ => false,
    };
    let mut value = 0i32;
    for byte in bytes.take_while(u8::is_ascii_digit) {
        value = value.wrapping_mul(10).wrapping_add(i32::from(byte - b'0'));
    }
    if negative {
        value.wrapping_neg()
    } else {
        value
    }
}

/// Reads `mm:ss:ff` as `%u:%u:%u` would.
fn scan_msf(text: &[u8]) -> Option<(u32, u32, u32)> {
    let mut rest = text;
    let mut fields = [0u32; 3];
    for (index, field) in fields.iter_mut().enumerate() {
        if index > 0 {
            rest = rest.strip_prefix(b":")?;
        }
        let skipped = rest.iter().take_while(|b| is_space(**b)).count();
        rest = rest.get(skipped..)?;
        let (negative, body) = match rest.first() {
            Some(b'-') => (true, rest.get(1..)?),
            Some(b'+') => (false, rest.get(1..)?),
            _ => (false, rest),
        };
        let digits = body.iter().take_while(|b| b.is_ascii_digit()).count();
        if digits == 0 {
            return None;
        }
        let mut value = 0u32;
        for byte in body.get(..digits)? {
            value = value.wrapping_mul(10).wrapping_add(u32::from(byte - b'0'));
        }
        *field = if negative {
            value.wrapping_neg()
        } else {
            value
        };
        rest = body.get(digits..)?;
    }
    let [minutes, seconds, frames] = fields;
    Some((minutes, seconds, frames))
}

/// A value read by [`scan`].
enum Scanned {
    Int(i32),
    Word(String),
}

/// A small `sscanf` for the CHD track metadata formats: `%d` and `%s`
/// conversions, literal text, and whitespace in the format matching any run
/// of whitespace. Returns the values converted before the first mismatch.
fn scan(text: &str, format: &str) -> Vec<Scanned> {
    let input = text.as_bytes();
    let mut at = 0usize;
    let mut values = Vec::new();
    let mut spec = format.bytes().peekable();
    let skip_space = |at: &mut usize| {
        while input.get(*at).is_some_and(|b| is_space(*b)) {
            *at += 1;
        }
    };
    while let Some(byte) = spec.next() {
        if byte == b'%' {
            skip_space(&mut at);
            match spec.next() {
                Some(b'd') => {
                    let start = at;
                    if matches!(input.get(at), Some(b'-' | b'+')) {
                        at += 1;
                    }
                    let digits_start = at;
                    while input.get(at).is_some_and(u8::is_ascii_digit) {
                        at += 1;
                    }
                    if at == digits_start {
                        return values;
                    }
                    let digits = input.get(start..at).unwrap_or(&[]);
                    let parsed = std::str::from_utf8(digits)
                        .ok()
                        .and_then(|s| s.parse::<i64>().ok());
                    let value = parsed.map_or(0, |v| {
                        v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
                    });
                    values.push(Scanned::Int(value));
                }
                Some(b's') => {
                    let start = at;
                    while input.get(at).is_some_and(|b| !is_space(*b)) {
                        at += 1;
                    }
                    if at == start {
                        return values;
                    }
                    let word = input.get(start..at).unwrap_or(&[]);
                    values.push(Scanned::Word(String::from_utf8_lossy(word).into_owned()));
                }
                _ => return values,
            }
        } else if is_space(byte) {
            skip_space(&mut at);
        } else if input.get(at) == Some(&byte) {
            at += 1;
        } else {
            return values;
        }
    }
    values
}

const TRACK_METADATA_FORMAT: &str = "TRACK:%d TYPE:%s SUBTYPE:%s FRAMES:%d";
const TRACK_METADATA2_FORMAT: &str =
    "TRACK:%d TYPE:%s SUBTYPE:%s FRAMES:%d PREGAP:%d PGTYPE:%s PGSUB:%s POSTGAP:%d";

/// Track number, type and frame count from one track's metadata text, when
/// it has all of the fields its format names.
fn parse_track(text: &str, format: &str, fields: usize) -> Option<(i32, String, i32)> {
    let values = scan(text, format);
    if values.len() != fields {
        return None;
    }
    match (values.first(), values.get(1), values.get(3)) {
        (Some(Scanned::Int(number)), Some(Scanned::Word(kind)), Some(Scanned::Int(frames))) => {
            Some((*number, kind.clone(), *frames))
        }
        _ => None,
    }
}

impl Disc {
    pub fn new() -> Disc {
        Disc {
            source: Source::Closed,
            path: String::new(),
            last_error: String::new(),
            layout: SectorLayout::Cooked2048,
            chd: ChdInfo::default(),
            sector_count: 0,
            tracks: Vec::new(),
        }
    }

    /// Opens an image: `.iso`, `.bin`, `.img`, `.chd` or a `.cue` sheet. The
    /// layout of a raw image is detected rather than assumed. Returns false
    /// and sets [`Disc::last_error`] on failure.
    pub fn open(&mut self, path: &str) -> bool {
        self.close();
        self.last_error.clear();

        if lowercase_extension(path) == ".cue" {
            return self.open_cue(path);
        }

        let Some(file) = self.open_image(path, SectorLayout::Cooked2048) else {
            return false;
        };
        self.open_opened(
            file,
            path,
            "Disc image is too small to contain a single sector: ",
        )
    }

    /// Opens a file someone else already opened — on Android, a descriptor
    /// the Storage Access Framework handed over for a content URI, with no
    /// path behind it. `display_name` is used for messages and to recognise
    /// the extension, since the file carries neither.
    ///
    /// A cue sheet cannot be opened this way: it names a sibling file that a
    /// descriptor gives no way to reach. Callers holding a cue must resolve
    /// the sibling themselves and pass the image.
    pub fn open_file(&mut self, mut file: File, display_name: &str) -> bool {
        self.close();
        self.last_error.clear();

        if lowercase_extension(display_name) == ".cue" {
            // Saying so is much better than opening the cue as though it were
            // an image and reporting a corrupt disc.
            self.last_error = format!(
                "A cue sheet cannot be opened by descriptor; open the image it names instead: {display_name}"
            );
            return false;
        }

        // A handle that cannot be read from (opened for writing only, say)
        // is refused here rather than taken for an empty disc.
        let mut probe = [0u8; 1];
        let readable = loop {
            match file.read(&mut probe) {
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                result => break result.is_ok(),
            }
        };
        if !readable || file.seek(SeekFrom::Start(0)).is_err() {
            self.last_error = format!("Could not read the descriptor for: {display_name}");
            return false;
        }

        self.path = display_name.to_string();
        self.open_opened(
            file,
            display_name,
            "Disc image is too small to contain a sector: ",
        )
    }

    /// The shared tail of [`Disc::open`] and [`Disc::open_file`].
    fn open_opened(&mut self, mut file: File, name: &str, too_small: &str) -> bool {
        // Recognise a CHD before anything else looks at the file. The raw-image
        // path must never see one: it would take compressed bytes for disc
        // contents.
        self.chd = probe_chd(&mut file);
        if self.chd.is_chd {
            return self.open_chd(file, name);
        }
        // A file named .chd that does not probe as one is a broken dump, not a
        // raw image in disguise. Serving its bytes as sectors leaves the BIOS
        // re-reading a label that can never make sense; saying so up front is
        // the difference between "corrupt image" and "machine hung on boot".
        if lowercase_extension(name) == ".chd" {
            self.last_error = format!(
                "File claims to be a CHD but does not contain one (corrupt dump?): {name}"
            );
            return false;
        }

        self.layout = detect_layout(&mut file);
        let size = file_size(&mut file);
        self.sector_count =
            u32::try_from(size / u64::from(self.layout.bytes_per_sector())).unwrap_or(u32::MAX);
        self.source = Source::Image(file);
        if self.sector_count == 0 {
            self.last_error = format!("{too_small}{name}");
            self.close();
            return false;
        }

        // A bare image is one data track covering the whole disc.
        self.tracks.push(Track {
            number: 1,
            is_audio: false,
            start_lba: 0,
            length_sectors: self.sector_count,
        });
        true
    }

    fn open_image(&mut self, path: &str, layout: SectorLayout) -> Option<File> {
        match File::open(path) {
            Ok(file) => {
                self.path = path.to_string();
                self.layout = layout;
                Some(file)
            }
            Err(_) => {
                self.last_error = format!("Could not open disc image: {path}");
                None
            }
        }
    }

    /// Opens a CHD and works out where the data track lives inside it.
    fn open_chd(&mut self, file: File, display_name: &str) -> bool {
        let mut chd = match ChdFile::open(file) {
            Ok(chd) => chd,
            Err(error) => {
                self.last_error = format!("{display_name}: {error}");
                return false;
            }
        };

        let hunk_bytes = chd.header().hunk_bytes;
        let unit_bytes = chd.header().unit_bytes;
        if unit_bytes == 0 || hunk_bytes == 0 || hunk_bytes % unit_bytes != 0 {
            self.last_error = format!("{display_name}: unexpected CHD geometry");
            return false;
        }
        let hunk_len = usize::try_from(hunk_bytes).unwrap_or(usize::MAX);
        if Vec::<u8>::new().try_reserve_exact(hunk_len).is_err() {
            self.last_error = format!("{display_name}: out of memory");
            return false;
        }

        let mut reader = ChdReader {
            prefetch: Arc::new(ChdPrefetch {
                state: Mutex::new(PrefetchState {
                    want: 0,
                    hunks: BTreeMap::new(),
                    stop: false,
                }),
                ready: Condvar::new(),
            }),
            worker: None,
            unit_bytes,
            frames_per_hunk: hunk_bytes / unit_bytes,
            data_track_frame: 0,
            payload_offset: 0,
            current: None,
        };

        // Walk the track metadata. Tracks sit one after another in the CHD,
        // each padded to a multiple of four frames, so a track's start has to
        // be accumulated rather than derived from its number.
        let mut chd_frame = 0u32;
        let mut lba = 0u32;
        let mut found_data = false;
        for index in 0..99u32 {
            let text = |data: Vec<u8>| {
                let end = data.iter().position(|b| *b == 0).unwrap_or(data.len());
                String::from_utf8_lossy(data.get(..end).unwrap_or(&[])).into_owned()
            };
            let parsed = if let Ok(data) = chd.metadata(CDROM_TRACK_METADATA2_TAG, index, 512) {
                parse_track(&text(data), TRACK_METADATA2_FORMAT, 8)
            } else if let Ok(data) = chd.metadata(CDROM_TRACK_METADATA_TAG, index, 512) {
                parse_track(&text(data), TRACK_METADATA_FORMAT, 4)
            } else {
                None
            };
            let Some((number, kind, frames)) = parsed else {
                break;
            };

            let frames = frames as u32;
            let track = Track {
                number: number as u32,
                is_audio: kind == "AUDIO",
                start_lba: lba,
                length_sectors: frames,
            };
            self.tracks.push(track);

            if !found_data && !track.is_audio {
                reader.data_track_frame = chd_frame;
                reader.payload_offset = chd_payload_offset(&kind);
                self.sector_count = frames;
                found_data = true;
            }

            lba = lba.wrapping_add(frames);
            chd_frame = chd_frame.wrapping_add(frames);
            // Pad to the track boundary the format requires.
            if !chd_frame.is_multiple_of(CD_TRACK_PADDING) {
                chd_frame = chd_frame.wrapping_add(CD_TRACK_PADDING - chd_frame % CD_TRACK_PADDING);
            }
        }

        if !found_data {
            self.last_error = format!("{display_name}: no data track");
            return false;
        }

        let prefetch = Arc::clone(&reader.prefetch);
        let mut file = chd;
        reader.worker = Some(std::thread::spawn(move || {
            prefetch_loop(&mut file, hunk_len, &prefetch);
        }));

        self.layout = SectorLayout::Cooked2048;
        self.source = Source::Chd(Box::new(reader));
        self.last_error.clear();
        true
    }

    /// Opens a cue sheet and the binary image it names.
    fn open_cue(&mut self, path: &str) -> bool {
        let Ok(cue) = File::open(path) else {
            self.last_error = format!("Could not open cue sheet: {path}");
            return false;
        };

        let mut binary_path = String::new();
        let mut parsed: Vec<Track> = Vec::new();
        let mut binary_is_raw = true;
        let mut pending: Option<Track> = None;

        let mut reader = BufReader::new(cue);
        let mut buffer = Vec::new();
        loop {
            // A line at a time, at most 1023 bytes of it, as `fgets` would.
            buffer.clear();
            match (&mut reader).take(1023).read_until(b'\n', &mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let line = trimmed(&buffer);
            if line.is_empty() {
                continue;
            }

            if line.starts_with(b"FILE") {
                if let Some(open) = find(line, b"\"", 0) {
                    if let Some(close) = find(line, b"\"", open + 1) {
                        let name = line.get(open + 1..close).unwrap_or(&[]);
                        binary_path = String::from_utf8_lossy(name).into_owned();
                    }
                }
                // A cue can name a WAVE or MP3 file for audio tracks. Only
                // binary images are handled; anything else is reported rather
                // than silently producing silence.
                if contains(line, b"WAVE") || contains(line, b"MP3") {
                    binary_is_raw = false;
                }
            } else if line.starts_with(b"TRACK") {
                parsed.extend(pending.take());
                pending = Some(Track {
                    number: atoi(line.get(6..).unwrap_or(&[])) as u32,
                    is_audio: contains(line, b"AUDIO"),
                    ..Track::default()
                });
            } else if line.starts_with(b"INDEX") {
                let Some(track) = pending.as_mut() else {
                    continue;
                };
                // INDEX nn mm:ss:ff — the start in minutes, seconds and
                // frames, 75 frames to the second.
                let Some(space) = find(line, b" ", 6) else {
                    continue;
                };
                if let Some((minutes, seconds, frames)) =
                    scan_msf(line.get(space + 1..).unwrap_or(&[]))
                {
                    let lba = minutes
                        .wrapping_mul(60)
                        .wrapping_add(seconds)
                        .wrapping_mul(75)
                        .wrapping_add(frames);
                    // INDEX 01 is the track proper; INDEX 00 is the pregap.
                    let index_number = atoi(line.get(6..).unwrap_or(&[]));
                    if index_number == 1 || track.start_lba == 0 {
                        track.start_lba = lba;
                    }
                }
            }
        }
        parsed.extend(pending.take());

        if binary_path.is_empty() {
            self.last_error = format!("Cue sheet names no image file: {path}");
            return false;
        }
        if !binary_is_raw {
            self.last_error =
                format!("Cue sheet references WAVE or MP3 audio, which is not supported: {path}");
            return false;
        }

        let resolved = format!("{}{}", directory_of(path), binary_path);
        let Some(mut file) = self.open_image(&resolved, SectorLayout::Raw2352Mode1) else {
            return false;
        };
        self.layout = detect_layout(&mut file);
        let size = file_size(&mut file);
        self.sector_count =
            u32::try_from(size / u64::from(self.layout.bytes_per_sector())).unwrap_or(u32::MAX);
        self.source = Source::Image(file);
        if self.sector_count == 0 {
            self.last_error = format!("Image named by the cue sheet is empty: {resolved}");
            self.close();
            return false;
        }

        // Track lengths come from the next track's start, and the last runs
        // to the end of the image.
        parsed.sort_by_key(|track| track.start_lba);
        let starts: Vec<u32> = parsed.iter().map(|track| track.start_lba).collect();
        for (index, track) in parsed.iter_mut().enumerate() {
            let end = starts.get(index + 1).copied().unwrap_or(self.sector_count);
            track.length_sectors = end.saturating_sub(track.start_lba);
        }
        self.tracks = parsed;

        if self.tracks.is_empty() {
            self.tracks.push(Track {
                number: 1,
                is_audio: false,
                start_lba: 0,
                length_sectors: self.sector_count,
            });
        }
        true
    }

    /// Closes the image. The last error is kept.
    pub fn close(&mut self) {
        self.source = Source::Closed;
        self.path.clear();
        self.tracks.clear();
        self.sector_count = 0;
        self.chd = ChdInfo::default();
    }

    pub fn is_open(&self) -> bool {
        !matches!(self.source, Source::Closed)
    }

    /// The image being read. For a cue sheet, the binary file it names.
    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn last_error(&self) -> &str {
        &self.last_error
    }

    pub fn layout(&self) -> SectorLayout {
        self.layout
    }

    /// What the file's header said, when it was recognised as a CHD — which
    /// is how nearly every 3DO dump in the wild is stored.
    pub fn chd(&self) -> &ChdInfo {
        &self.chd
    }

    pub fn is_chd(&self) -> bool {
        self.chd.is_chd
    }

    /// Sectors in the data track.
    pub fn sector_count(&self) -> u32 {
        self.sector_count
    }

    pub fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    /// Reads one logical block's 2048 bytes of user data. Returns false past
    /// the end of the image or when the read fails.
    pub fn read_sector(&mut self, lba: u32, out: &mut [u8; 2048]) -> bool {
        if lba >= self.sector_count {
            return false;
        }
        match &mut self.source {
            Source::Closed => false,
            Source::Chd(reader) => {
                let mut frame = [0u8; RAW_SECTOR_BYTES as usize];
                if !reader.read_frame(reader.data_track_frame.wrapping_add(lba), &mut frame) {
                    return false;
                }
                match frame.get(..out.len()) {
                    Some(user) => {
                        out.copy_from_slice(user);
                        true
                    }
                    None => false,
                }
            }
            Source::Image(file) => {
                let stride = u64::from(self.layout.bytes_per_sector());
                let offset = u64::from(lba) * stride + u64::from(self.layout.user_offset());
                file.seek(SeekFrom::Start(offset)).is_ok() && file.read_exact(out).is_ok()
            }
        }
    }

    /// Reads a sector's raw bytes, whatever the container holds — used for
    /// audio, where there is no user-data subset to extract. Returns how many
    /// bytes were written: the image's stride for a flat image, 2352 for a
    /// CHD (taken from the track's payload offset onward, as the frame is
    /// stored).
    pub fn read_raw_sector(&mut self, lba: u32, out: &mut [u8; 2352]) -> Option<u32> {
        if lba >= self.sector_count {
            return None;
        }
        match &mut self.source {
            Source::Closed => None,
            Source::Chd(reader) => reader
                .read_frame(reader.data_track_frame.wrapping_add(lba), out)
                .then_some(CD_MAX_SECTOR_DATA),
            Source::Image(file) => {
                let stride = self.layout.bytes_per_sector();
                let target = out.get_mut(..stride as usize)?;
                file.seek(SeekFrom::Start(u64::from(lba) * u64::from(stride)))
                    .ok()?;
                file.read_exact(target).ok()?;
                Some(stride)
            }
        }
    }
}

/// Raw or cooked, from the first sector's sync pattern. Byte 15 of a raw
/// sector is its mode, which decides whether a subheader comes first.
fn detect_layout(file: &mut File) -> SectorLayout {
    let mut header = [0u8; 24];
    let read = file.seek(SeekFrom::Start(0)).is_ok() && file.read_exact(&mut header).is_ok();
    if !read || header.get(..12) != Some(&SYNC_PATTERN[..]) {
        // Too small to be raw, or no sync pattern: user data directly.
        return SectorLayout::Cooked2048;
    }
    if header[15] == 2 {
        SectorLayout::Raw2352Mode2
    } else {
        SectorLayout::Raw2352Mode1
    }
}
