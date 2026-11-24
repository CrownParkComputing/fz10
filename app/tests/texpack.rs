//! The texture-pack fingerprint is the contract between the console and the
//! pack builder: the console hashes the cel texels it decoded, the builder
//! hashes the artwork PNG it read. Both must land on the same number, or a
//! pack built from a dump never matches the console that wrote it.

use retro3do::texpack::source_fnv;

/// A transparent texel's colour is presentation, not content: the dump writes
/// it as zero, so the fingerprint must as well.
#[test]
fn transparent_texels_fingerprint_as_colour_zero() {
    let with_colour = source_fnv(2, 1, |_, _| (0x1234, true));
    let without = source_fnv(2, 1, |_, _| (0, true));
    assert_eq!(with_colour, without);
}

/// The other half of the contract: an opaque texel's colour still counts, and
/// so does the texel's position.
#[test]
fn opaque_texel_colour_and_position_still_count() {
    let red = source_fnv(2, 1, |_, _| (0x7c00, false));
    let green = source_fnv(2, 1, |_, _| (0x03e0, false));
    assert_ne!(red, green, "colour must change the fingerprint");

    let swapped = source_fnv(2, 1, |x, _| if x == 0 { (0x7c00, false) } else { (0x03e0, false) });
    assert_ne!(red, swapped, "position must change the fingerprint");
}

/// The position bit is presentation state and must not enter the hash: the
/// same sprite has to fingerprint the same wherever it lands.
#[test]
fn position_bit_is_ignored() {
    let plain = source_fnv(2, 1, |_, _| (0x1234, false));
    let flagged = source_fnv(2, 1, |_, _| (0x9234, false));
    assert_eq!(plain, flagged);
}
