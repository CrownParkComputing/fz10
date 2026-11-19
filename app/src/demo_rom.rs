//! The project's own hardware demonstration ROM, built into the core.
//!
//! It is a hardware test rather than a BIOS: it cannot boot a commercial disc.
//! It exists so the machine has something legal to run before the user has
//! found a BIOS image of their own. Source: demo/retro3do-demo.s in the
//! reference tree (Crown Park Computing, MIT); see demo/MANIFEST.md there.
#![forbid(unsafe_code)]

pub struct DemoRom {
    pub data: &'static [u8],
    pub name: &'static str,
    /// SHA-256 of the authored 1,794-byte image.
    pub authored_sha256: &'static str,
    /// SHA-256 of the image as loaded: zero-padded to the 1 MB ROM window.
    pub loader_sha256: &'static str,
}

static BYTES: &[u8] = include_bytes!("demo_rom.bin");

pub fn builtin_demo_rom() -> DemoRom {
    DemoRom {
        data: BYTES,
        name: "Retro-3DO Original Hardware Demo",
        authored_sha256: "f019b2a87431cfaec321dd894201f64b36ce964684661bb3eaf18d49fe1e0a4b",
        loader_sha256: "4d76c53f120b6f0465ae434108773747265ad91e6ce7b9050dfb19a16e3cab49",
    }
}
