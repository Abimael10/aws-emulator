// SPDX-License-Identifier: CC0-1.0

//! The IMAGE a service serves for a record whose own image bytes it does not hold.
//!
//! Several emulated services publish an address for a client to FETCH a picture of a record
//! — a workload catalogue entry's `iconUrl`, a marketplace listing's `logoThumbnailUrl`. AWS
//! serves whatever the owner uploaded; this emulator holds no uploaded bytes for a seeded
//! record, so what it serves at the address it published is an image DERIVED from that
//! record's own identifier: the same picture on every fetch, a different one per record, and
//! a real PNG a client's decoder reads rather than bytes shaped like one.
//!
//! One renderer for every service that publishes such an address, so two services cannot
//! drift into two pictures of the same kind, and the format a published address is served as
//! is stated once ([`IDENTICON_MEDIA_TYPE`]).

use ::sha2::{Digest, Sha256};

/// The media type the image this module writes is served as, which is the format
/// [`identicon_png`] encodes.
pub const IDENTICON_MEDIA_TYPE: &str = "image/png";

/// How many blocks a side of the image is divided into.
const GRID_BLOCKS: usize = 8;

/// How many pixels a side of one block is, which makes the image
/// `GRID_BLOCKS * BLOCK_PIXELS` pixels square — a thumbnail, which is the size the members
/// publishing one name.
const BLOCK_PIXELS: usize = 8;

/// How many samples one pixel carries: 8-bit RGB.
const SAMPLES_PER_PIXEL: usize = 3;

/// The background every image is drawn on: a light neutral, so the figure drawn over it
/// stands out whatever colour the record's own is.
const BACKGROUND: [u8; SAMPLES_PER_PIXEL] = [0xF2, 0xF2, 0xF2];

/// The darkest and the lightest a figure sample may be, so the figure is neither invisible
/// against [`BACKGROUND`] nor a flat black whatever the digest says.
const FIGURE_FLOOR: u8 = 0x20;
/// See [`FIGURE_FLOOR`].
const FIGURE_CEILING: u8 = 0xC0;

/// The image of the record `seed` identifies, as PNG bytes written by the maintained `png`
/// encoder, or `None` when the image cannot be encoded.
///
/// DERIVED from `seed` — the identifier of the record whose picture it is — so a catalogue
/// needs no image file beside it and two records cannot share a picture: a SHA-256 digest of
/// the seed picks the figure's colour and the blocks it fills on a [`GRID_BLOCKS`] square
/// grid, mirrored across the vertical axis the way a generated avatar is. Nothing about it
/// depends on the request or the clock, so the address of a record's image always serves the
/// same image.
#[must_use]
pub fn identicon_png(seed: &str) -> Option<Vec<u8>> {
    let digest = Sha256::digest(seed.as_bytes());
    let figure: Vec<u8> = digest
        .iter()
        .take(SAMPLES_PER_PIXEL)
        .map(|sample| {
            let span = u16::from(FIGURE_CEILING - FIGURE_FLOOR) + 1;
            let offset = u16::from(*sample) % span;
            FIGURE_FLOOR + u8::try_from(offset).unwrap_or(0)
        })
        .collect();
    let side_pixels = GRID_BLOCKS * BLOCK_PIXELS;
    let mut samples = Vec::with_capacity(side_pixels * side_pixels * SAMPLES_PER_PIXEL);
    for y in 0..side_pixels {
        for x in 0..side_pixels {
            let row = y / BLOCK_PIXELS;
            // Mirrored across the vertical axis: a column in the right half takes the block
            // of the column facing it in the left half.
            let column = (x / BLOCK_PIXELS).min(GRID_BLOCKS - 1 - x / BLOCK_PIXELS);
            let bit = digest
                .get(SAMPLES_PER_PIXEL + row)
                .map_or(0, |byte| (byte >> column) & 1);
            let pixel = if bit == 1 { &figure } else { &BACKGROUND[..] };
            samples.extend_from_slice(pixel);
        }
    }
    let mut rendered = Vec::new();
    {
        let side = u32::try_from(side_pixels).ok()?;
        let mut encoder = ::png::Encoder::new(&mut rendered, side, side);
        encoder.set_color(::png::ColorType::Rgb);
        encoder.set_depth(::png::BitDepth::Eight);
        let mut writer = encoder.write_header().ok()?;
        writer.write_image_data(&samples).ok()?;
        writer.finish().ok()?;
    }
    Some(rendered)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first eight bytes of every PNG file.
    const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

    /// RULE: the image is a real PNG, the same one for the same record on every call, and a
    /// different one per record.
    ///
    /// Rows differ along the form of the seed a service hands this module: a name, an
    /// identifier the catalogue issued, a path-shaped id, and one differing from another only
    /// in its last character.
    #[test]
    fn a_records_image_is_a_real_png_that_is_its_own_and_does_not_change() {
        let seeds = [
            "SAP",
            "a4f1c0de-0000-5000-8000-000000000001",
            "a4f1c0de-0000-5000-8000-000000000002",
            "l-0001-analytics/logo",
            "l-0001-analytics/logp",
        ];
        let mut images: Vec<Vec<u8>> = Vec::new();
        for seed in seeds {
            let image = identicon_png(seed)
                .unwrap_or_else(|| panic!("case {seed:?}: expected an encoded image"));
            assert!(
                image.starts_with(&PNG_SIGNATURE),
                "case {seed:?}: expected PNG bytes, got {} bytes starting {:?}",
                image.len(),
                image.get(..PNG_SIGNATURE.len())
            );
            assert_eq!(
                identicon_png(seed).as_ref(),
                Some(&image),
                "case {seed:?}: the same record must render the same image"
            );
            assert!(
                !images.contains(&image),
                "case {seed:?}: every record must render an image of its own"
            );
            images.push(image);
        }
    }
}
