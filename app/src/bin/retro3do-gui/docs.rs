//! The DOCS tab: the bundled 3DO hardware guides, read inside the app.
//!
//! `tools/fetch-docs.sh` downloads each source PDF and pre-renders it to page
//! PNGs under the docs directory (`~/3do-library/docs/<slug>/page-*.png`).
//! Pages decode on the app's artwork thread like covers do, so opening the
//! reader costs nothing until a page is actually shown.

use std::path::{Path, PathBuf};

/// A bundled guide: where its pages live, how to label it, where it came
/// from and what it's for.
pub struct DocEntry {
    pub slug: &'static str,
    pub title: &'static str,
    pub source: &'static str,
    pub blurb: &'static str,
}

pub const DOCS: &[DocEntry] = &[
    DocEntry {
        slug: "fz1-technical-guide",
        title: "FZ-1 Technical Guide",
        source: "https://3dodev.com",
        blurb: "Panasonic's developer-facing tour of the FZ-1: the ARM60, MADAM \
                and CLIO chips, the cel engine, video/CD hardware and signals.",
    },
    DocEntry {
        slug: "fz1-service-manual",
        title: "FZ-1 Service Manual",
        source: "https://3dodev.com",
        blurb: "The bench manual: block diagrams, board layouts, schematics, \
                pin-outs and alignment procedures for the FZ-1.",
    },
    DocEntry {
        slug: "fz1-operating-manual",
        title: "FZ-1 Operating Manual",
        source: "https://3dodev.com",
        blurb: "The owner's handbook: console controls, hookups, the gamepad \
                layout and the disc/photo-CD features.",
    },
    DocEntry {
        slug: "beginners-guide",
        title: "3DO Beginner's Guide",
        source: "https://racketboy.com/retro/3do-interactive-multiplayer-101-a-beginners-guide",
        blurb: "A friendly history and buying guide: the hardware revisions, \
                the library's strengths and what the games are like.",
    },
];

/// Sorted `page-*.png` paths for one doc, empty if it isn't installed.
pub fn pages(docs_dir: &Path, slug: &str) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    if let Ok(read) = std::fs::read_dir(docs_dir.join(slug)) {
        for entry in read.flatten() {
            let path = entry.path();
            let is_page = path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("page-") && n.ends_with(".png"));
            if is_page {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}
