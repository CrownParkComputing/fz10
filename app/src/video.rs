//! Pixel facts shared by the chip that writes the framebuffer (MADAM) and the
//! one that reads it (the VDLP). They live in one place because if the two ever
//! disagree, the picture is wrong in a way that looks like a rendering bug in
//! whichever chip you happen to suspect.
#![forbid(unsafe_code)]

/// The top bit of a framebuffer pixel. Meaningful only when the display list
/// asks for CLUT bypass: then it says "already a colour, do not look it up".
pub const PIXEL_LITERAL: u16 = 0x8000;

/// Byte offset of pixel (x, y) in an interleaved framebuffer `width` pixels
/// wide. Each 32-bit word holds the same x on two adjacent lines: the even line
/// in the high half, the odd line in the low half. A line pair is `width`
/// words, and neighbouring pixels on one line are four bytes apart.
pub const fn framebuffer_offset(x: u32, y: u32, width: u32) -> u32 {
    let pair = y / 2;
    let odd = if y & 1 != 0 { 2 } else { 0 };
    pair.wrapping_mul(width.wrapping_mul(4))
        .wrapping_add(x.wrapping_mul(4))
        .wrapping_add(odd)
}

/// Widen 0RRRRRGGGGGBBBBB to opaque XRGB8888, replicating each channel's top
/// three bits into the low ones so full scale maps to 0xFF, not 0xF8.
pub const fn expand_rgb555(pixel: u16) -> u32 {
    const fn widen(five: u32) -> u32 {
        (five << 3) | (five >> 2)
    }
    let r = widen(((pixel >> 10) & 0x1f) as u32);
    let g = widen(((pixel >> 5) & 0x1f) as u32);
    let b = widen((pixel & 0x1f) as u32);
    0xff00_0000 | (r << 16) | (g << 8) | b
}
