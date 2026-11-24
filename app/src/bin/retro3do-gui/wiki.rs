//! The Wiki section of the settings page: how this machine was written.
//!
//! The text is the repository's own documentation, bundled at build time
//! with `include_str!`, so what the app says and what `docs/` says cannot
//! drift apart: the build fails if the file moves, and the app ships the
//! same words.

/// The player-facing summary and the per-core public specifications.
pub const KNOWLEDGE_MD: &str = include_str!("../../../../docs/KNOWLEDGE.md");

/// The method, in full, as `docs/CLEANROOM.md` tells it.
pub const CLEANROOM_MD: &str = include_str!("../../../../docs/CLEANROOM.md");

/// Where the project lives.
pub const REPO_URL: &str = "https://github.com/CrownParkComputing/fz10";

/// The wiki, and the development notes, as clickable links.
pub const LINKS: [(&str, &str); 5] = [
    ("GitHub wiki", "https://github.com/CrownParkComputing/fz10/wiki"),
    (
        "CLEANROOM.md",
        "https://github.com/CrownParkComputing/fz10/blob/main/docs/CLEANROOM.md",
    ),
    (
        "SOURCES.md",
        "https://github.com/CrownParkComputing/fz10/blob/main/docs/SOURCES.md",
    ),
    (
        "PROVENANCE.md",
        "https://github.com/CrownParkComputing/fz10/blob/main/docs/PROVENANCE.md",
    ),
    (
        "task-progress.md",
        "https://github.com/CrownParkComputing/fz10/blob/main/docs/task-progress.md",
    ),
];

// ---------------------------------------------------------------------------
// A small Markdown reader
//
// Enough of Markdown for the project's own documents: headings, paragraphs,
// bullets, fenced code, and inline bold, code and links. Nothing else, so
// nothing to get wrong.

/// A block of the document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    /// A heading and its level (1 for `#`).
    Heading(usize, String),
    /// A run of text.
    Paragraph(String),
    /// A bulleted line.
    Bullet(String),
    /// A fenced code block, without its fences.
    Code(String),
    /// A horizontal rule.
    Rule,
}

/// A piece of a run of text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inline {
    /// Plain text.
    Text(String),
    /// `**bold**`.
    Bold(String),
    /// `` `code` ``.
    Code(String),
    /// A `[label](url)` link.
    Link { label: String, url: String },
}

/// Split a document into blocks.
pub fn parse(markdown: &str) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::new();
    let mut paragraph: Vec<String> = Vec::new();
    let mut code: Option<String> = None;

    for line in markdown.lines() {
        let trimmed = line.trim();

        // Inside a fence, everything is code until the fence closes.
        if let Some(buffer) = code.as_mut() {
            if trimmed.starts_with("```") {
                out.push(Block::Code(std::mem::take(buffer)));
                code = None;
            } else {
                buffer.push_str(line);
                buffer.push('\n');
            }
            continue;
        }
        if trimmed.starts_with("```") {
            // A fence that opens right after a paragraph still ends it.
            flush_paragraph(&mut paragraph, &mut out);
            code = Some(String::new());
            continue;
        }

        if trimmed.is_empty() {
            flush_paragraph(&mut paragraph, &mut out);
            continue;
        }
        if trimmed == "---" {
            flush_paragraph(&mut paragraph, &mut out);
            out.push(Block::Rule);
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix('#') {
            flush_paragraph(&mut paragraph, &mut out);
            let level = 1 + rest.chars().take_while(|c| *c == '#').count();
            let text = rest.trim_start_matches('#').trim().to_string();
            out.push(Block::Heading(level, text));
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("- ") {
            flush_paragraph(&mut paragraph, &mut out);
            out.push(Block::Bullet(rest.trim().to_string()));
            continue;
        }
        paragraph.push(trimmed.to_string());
    }
    flush_paragraph(&mut paragraph, &mut out);
    if let Some(buffer) = code {
        // An unterminated fence still shows what it held.
        out.push(Block::Code(buffer));
    }
    out
}

/// A paragraph ends at the first blank line; its lines are joined with
/// spaces so a wrapped source line reads as one line in the app.
fn flush_paragraph(lines: &mut Vec<String>, out: &mut Vec<Block>) {
    if lines.is_empty() {
        return;
    }
    out.push(Block::Paragraph(lines.join(" ")));
    lines.clear();
}

/// Split a run of text into its inline pieces.
pub fn inline(text: &str) -> Vec<Inline> {
    let mut out: Vec<Inline> = Vec::new();
    let mut plain = String::new();
    let bytes: Vec<char> = text.chars().collect();
    let mut i = 0;

    while i < bytes.len() {
        // An autolink: <https://example.test>
        if bytes[i] == '<' {
            if let Some((url, used)) = take_autolink(&bytes, i) {
                if !plain.is_empty() {
                    out.push(Inline::Text(std::mem::take(&mut plain)));
                }
                out.push(Inline::Link {
                    label: url.clone(),
                    url,
                });
                i += used;
                continue;
            }
        }
        // A link: [label](url)
        if bytes[i] == '[' {
            if let Some((label, url, used)) = take_link(&bytes, i) {
                if !plain.is_empty() {
                    out.push(Inline::Text(std::mem::take(&mut plain)));
                }
                out.push(Inline::Link { label, url });
                i += used;
                continue;
            }
        }
        // Bold: **text**
        if bytes[i] == '*' && bytes.get(i + 1) == Some(&'*') {
            if let Some((body, used)) = take_until(&bytes, i + 2, "**") {
                if !plain.is_empty() {
                    out.push(Inline::Text(std::mem::take(&mut plain)));
                }
                out.push(Inline::Bold(body));
                // The opening `**` is not part of `used`, which counts from
                // the body onwards.
                i += 2 + used;
                continue;
            }
        }
        // Code: `text`
        if bytes[i] == '`' {
            if let Some((body, used)) = take_until(&bytes, i + 1, "`") {
                if !plain.is_empty() {
                    out.push(Inline::Text(std::mem::take(&mut plain)));
                }
                out.push(Inline::Code(body));
                // The opening backtick is not part of `used` either.
                i += 1 + used;
                continue;
            }
        }
        plain.push(bytes[i]);
        i += 1;
    }

    if !plain.is_empty() {
        out.push(Inline::Text(plain));
    }
    out
}

/// Read a `[label](url)` link at `start`; returns the parts and the length.
fn take_link(chars: &[char], start: usize) -> Option<(String, String, usize)> {
    let mut i = start + 1;
    while i < chars.len() && chars[i] != ']' {
        if chars[i] == '\n' {
            return None;
        }
        i += 1;
    }
    if i >= chars.len() || chars.get(i + 1) != Some(&'(') {
        return None;
    }
    let label: String = chars[start + 1..i].iter().collect();
    let mut j = i + 2;
    while j < chars.len() && chars[j] != ')' {
        if chars[j] == '\n' {
            return None;
        }
        j += 1;
    }
    if j >= chars.len() {
        return None;
    }
    let url: String = chars[i + 2..j].iter().collect();
    Some((label, url, j + 1 - start))
}

/// Read an autolink `<https://...>` at `start`; returns the URL and length.
fn take_autolink(chars: &[char], start: usize) -> Option<(String, usize)> {
    let mut j = start + 1;
    while j < chars.len() && chars[j] != '>' {
        if chars[j] == '\n' || chars[j] == ' ' {
            return None;
        }
        j += 1;
    }
    if j >= chars.len() {
        return None;
    }
    let url: String = chars[start + 1..j].iter().collect();
    if !url.starts_with("https://") && !url.starts_with("http://") {
        return None;
    }
    Some((url, j + 1 - start))
}

/// Read up to the next `closer`, returning the text and the length taken.
fn take_until(chars: &[char], start: usize, closer: &str) -> Option<(String, usize)> {
    let close: Vec<char> = closer.chars().collect();
    let mut i = start;
    while i + close.len() <= chars.len() {
        if chars[i..i + close.len()] == close[..] {
            let body: String = chars[start..i].iter().collect();
            return Some((body, close.len() + (i - start)));
        }
        i += 1;
    }
    None
}

// ---------------------------------------------------------------------------
// Opening a link
//
// egui's hyperlink reports the click; the browser is ours to open, because
// the front end is built without eframe's link feature.

/// Turn a link in a document into one a browser can open. The documents
/// link to each other by file name; the app resolves those against the
/// project's repository, where the same file can be read.
pub fn resolve(url: &str) -> String {
    if url.starts_with("https://") || url.starts_with("http://") {
        return url.to_string();
    }
    if url.ends_with(".md") {
        let name = url.rsplit('/').next().unwrap_or(url);
        return format!("{REPO_URL}/blob/main/docs/{name}");
    }
    url.to_string()
}

/// Open a URL in the desktop's browser. True when the opener was started.
pub fn open_in_browser(url: &str) -> bool {
    #[cfg(target_os = "macos")]
    let started = std::process::Command::new("open").arg(url).spawn().is_ok();
    #[cfg(target_os = "windows")]
    let started = std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .spawn()
        .is_ok();
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let started = std::process::Command::new("xdg-open").arg(url).spawn().is_ok();
    started
}

// ---------------------------------------------------------------------------
// Drawing

/// The colour of a link, from the app's own text colours.
fn link_colour(ui: &egui::Ui) -> egui::Color32 {
    ui.visuals().hyperlink_color
}

/// Draw a document.
pub fn show(ui: &mut egui::Ui, markdown: &str) {
    for block in parse(markdown) {
        match block {
            Block::Heading(level, text) => {
                let size = match level {
                    1 => 26.0,
                    2 => 21.0,
                    3 => 18.0,
                    _ => 16.0,
                };
                ui.label(egui::RichText::new(text).size(size).strong());
                ui.add_space(8.0);
            }
            Block::Paragraph(text) => {
                show_inline(ui, &text);
                ui.add_space(10.0);
            }
            Block::Bullet(text) => {
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new("•"));
                    show_inline(ui, &text);
                });
                ui.add_space(4.0);
            }
            Block::Code(text) => {
                ui.code(text.trim_end());
                ui.add_space(10.0);
            }
            Block::Rule => {
                ui.separator();
                ui.add_space(6.0);
            }
        }
    }
}

/// Draw a run of text, with its bold, code and link pieces kept apart.
fn show_inline(ui: &mut egui::Ui, text: &str) {
    let pieces = inline(text);
    if pieces.len() == 1 {
        if let Inline::Text(only) = &pieces[0] {
            ui.label(egui::RichText::new(only));
            return;
        }
    }
    ui.horizontal_wrapped(|ui| {
        for piece in pieces {
            match piece {
                Inline::Text(t) => {
                    ui.label(egui::RichText::new(t));
                }
                Inline::Bold(t) => {
                    ui.label(egui::RichText::new(t).strong());
                }
                Inline::Code(t) => {
                    ui.label(egui::RichText::new(t).monospace());
                }
                Inline::Link { label, url } => {
                    // A link into the repository's own documents is written
                    // relative (`PROVENANCE.md`); it is resolved to its web
                    // address so it opens rather than pointing at a file
                    // that is not there.
                    let href = resolve(&url);
                    let response = ui.add(egui::Hyperlink::from_label_and_url(
                        egui::RichText::new(label).color(link_colour(ui)),
                        href.clone(),
                    ));
                    if response.clicked() {
                        open_in_browser(&href);
                    }
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn parse_splits_headings_paragraphs_and_bullets() {
        let blocks = parse("# Title\n\nSome text\nwrapped over lines.\n\n- one\n- two\n");
        assert_eq!(
            blocks,
            vec![
                Block::Heading(1, "Title".to_string()),
                Block::Paragraph("Some text wrapped over lines.".to_string()),
                Block::Bullet("one".to_string()),
                Block::Bullet("two".to_string()),
            ]
        );
    }

    #[test]
    fn parse_keeps_fenced_code_whole() {
        let blocks = parse("```sh\nbash docs/audit.sh\n```\n");
        assert_eq!(
            blocks,
            vec![Block::Code("bash docs/audit.sh\n".to_string())]
        );
    }

    #[test]
    fn parse_reads_heading_levels() {
        let blocks = parse("## Two\n\n### Three\n");
        assert_eq!(
            blocks,
            vec![
                Block::Heading(2, "Two".to_string()),
                Block::Heading(3, "Three".to_string()),
            ]
        );
    }

    #[test]
    fn inline_reads_bold_code_and_links() {
        let pieces = inline("a **bold** and `code` and [label](https://example.test/x) end");
        assert_eq!(
            pieces,
            vec![
                Inline::Text("a ".to_string()),
                Inline::Bold("bold".to_string()),
                Inline::Text(" and ".to_string()),
                Inline::Code("code".to_string()),
                Inline::Text(" and ".to_string()),
                Inline::Link {
                    label: "label".to_string(),
                    url: "https://example.test/x".to_string(),
                },
                Inline::Text(" end".to_string()),
            ]
        );
    }

    #[test]
    fn inline_leaves_an_unfinished_link_alone() {
        let pieces = inline("a [label] and more");
        assert_eq!(pieces, vec![Inline::Text("a [label] and more".to_string())]);
    }

    #[test]
    fn the_bundled_documents_carry_the_story() {
        // The app must not be able to show a different story from the docs.
        for text in [KNOWLEDGE_MD, CLEANROOM_MD] {
            assert!(text.contains("FreeDO"), "the emulator names are missing");
            assert!(text.contains("Opera"), "the emulator names are missing");
            assert!(text.contains("MAME"), "the emulator names are missing");
        }
        // The five cores the section promises, each with its public spec.
        for core in ["ARM60", "MADAM", "DSP", "CLIO", "XBUS"] {
            assert!(KNOWLEDGE_MD.contains(core), "{core} is missing");
        }
        for spec in [
            "ARM DDI 0100",
            "WO 94/10644",
            "WO 94/16383",
            "WO 94/16382",
            "3DO Graphics Programming Guide",
        ] {
            assert!(KNOWLEDGE_MD.contains(spec), "{spec} is missing");
        }
    }

    #[test]
    fn a_relative_document_link_becomes_a_web_address() {
        // The bundled documents link to each other by name; the app has to
        // turn that into an address a browser can open.
        assert_eq!(
            resolve("PROVENANCE.md"),
            format!("{REPO_URL}/blob/main/docs/PROVENANCE.md")
        );
        assert_eq!(
            resolve("https://example.test/notes"),
            "https://example.test/notes"
        );
    }

    #[test]
    fn the_links_point_at_the_project() {
        for (_, url) in LINKS {
            assert!(
                url.starts_with(REPO_URL),
                "{url} is not a project link"
            );
        }
        assert!(LINKS.iter().any(|(_, url)| url.ends_with("/wiki")));
    }

    #[test]
    fn every_link_in_the_bundled_document_is_reachable_in_the_app() {
        // A link in the text is drawn by the app; a broken one would be a
        // dead end for the reader.
        for block in parse(KNOWLEDGE_MD) {
            let text = match block {
                Block::Paragraph(t) | Block::Bullet(t) => t,
                _ => continue,
            };
            for piece in inline(&text) {
                if let Inline::Link { url, .. } = piece {
                    assert!(
                        url.starts_with("https://")
                            || url.ends_with(".md")
                            || url.starts_with("PROVENANCE"),
                        "{url} is not a usable link"
                    );
                }
            }
        }
    }
}
