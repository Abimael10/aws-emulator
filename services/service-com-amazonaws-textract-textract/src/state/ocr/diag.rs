// SPDX-License-Identifier: CC0-1.0

//! Reading the characters of a TOKEN off a page set in a PROPORTIONAL face, where nothing
//! but the ink names each character.
//!
//! A token is a run of capitals and figures that is no word, so no reading of it can be had
//! from the characters around it: every one of them has to be named from the ink the page
//! carries. What such a page asks of the segmentation, which the lines of ordinary text in
//! [`super::probe`] and the crate's raster suites do not, is the two ways a proportional
//! face sets characters that no vertical slice of the page separates:
//!
//! * a letter whose ink reaches BACK UNDER the letter before it — the hook of a `J`, the
//!   tail of a `Q` — so the two boxes overlap in columns while the strokes never touch, and
//!   the pair comes out as one run of inked columns no wider than one wide capital
//!   ([`super::Ink::regions_in`]);
//! * a wide capital set against a narrow one — a `W` beside a `V` — whose strokes DO touch,
//!   so the run has to be cut where its ink narrows, and the widest letter the face sets is
//!   wider than the capitals it stands with ([`super::glyphs::widest_glyph`]).
//!
//! A third class is what the ink of ONE character is made of: the characters that are the
//! same ink but for where one of them closes — a `6` against an `8` — which the counts a
//! mark's own strokes and counters give settle ([`super::glyphs`]).
//!
//! The rows below are drawn from the classes that share a letterform, at the sizes a page is
//! set at, and the assertion is a round trip: the characters drawn are the characters read.
//!
//! TODO: this file belongs at `src/state/ocr/tokens.rs`, which is the module it is declared
//! as; it is reached through a `#[path]` override only because the tooling that added it
//! could create a file but not rename one. Rename it and drop the override on the next touch
//! of this module.

use super::*;

/// Draw `lines` with the PROPORTIONAL outline face `face` rasterised at `size` pixels, as a
/// PNG, the way a document rendered from an outline face carries its text: every letter
/// advanced by its own width, every stroke anti-aliased.
///
/// `fontdue` hands out one byte of ink per pixel of a glyph's own ink box, with the metrics
/// that place that box against the line's baseline — including the letters whose box reaches
/// left of the pen (`xmin` below zero) or under the baseline (`ymin` below zero), which is
/// the ink this module is about.
fn page(lines: &[&str], face: &'static [u8], size: f32) -> Vec<u8> {
    let font = fontdue::Font::from_bytes(face, fontdue::FontSettings::default())
        .expect("the embedded outline face parses");
    let metrics = font
        .horizontal_line_metrics(size)
        .expect("the outline face sets its lines");
    let pitch = metrics.new_line_size.ceil() as usize;
    let margin = (size * 1.5).ceil() as usize;
    let ground = 245u8;
    let width = margin * 2
        + lines
            .iter()
            .map(|line| {
                line.chars()
                    .map(|ch| font.metrics(ch, size).advance_width.ceil() as usize)
                    .sum::<usize>()
            })
            .max()
            .unwrap_or(0);
    let height = margin * 2 + lines.len() * pitch;
    let mut drawn = image::GrayImage::from_pixel(width as u32, height as u32, image::Luma([ground]));
    for (row, line) in lines.iter().enumerate() {
        let baseline = margin + row * pitch + metrics.ascent.ceil() as usize;
        let mut pen = margin as f32;
        for ch in line.chars() {
            let (glyph, bitmap) = font.rasterize(ch, size);
            for y in 0..glyph.height {
                for x in 0..glyph.width {
                    let coverage = bitmap[y * glyph.width + x];
                    if coverage == 0 {
                        continue;
                    }
                    // `ymin` is how far the foot of the glyph's ink box stands above the
                    // baseline, so its top row is that much more than its height above it.
                    let at_x = pen as isize + glyph.xmin as isize + x as isize;
                    let above = glyph.height as isize + glyph.ymin as isize;
                    let at_y = baseline as isize - above + y as isize;
                    if at_x < 0 || at_y < 0 || at_x >= width as isize || at_y >= height as isize {
                        continue;
                    }
                    let (at_x, at_y) = (at_x as u32, at_y as u32);
                    let held = drawn.get_pixel(at_x, at_y).0[0];
                    drawn.put_pixel(
                        at_x,
                        at_y,
                        image::Luma([held.min(ground.saturating_sub(coverage))]),
                    );
                }
            }
            pen += font.metrics(ch, size).advance_width;
        }
    }
    let mut out = Cursor::new(Vec::new());
    drawn
        .write_to(&mut out, ImageFormat::Png)
        .expect("a grey page encodes as a PNG");
    out.into_inner()
}

/// The characters of `text`, with the spaces left out: where a line is broken into words is
/// the separation rule's ([`super::figures`]), not this one's.
fn characters(text: &str) -> String {
    text.chars().filter(|ch| !ch.is_whitespace()).collect()
}

/// A page of tokens is read character for character, at the sizes a document is set at.
///
/// Each row sets a letter that reaches under its neighbour, or a wide capital against a
/// narrow one, which is what makes two characters one run of ink; each also draws from the
/// classes that share a letterform, so no character is read by elimination from the shapes
/// around it. The letterform classes themselves, and the same rule over the service's own
/// wire, are the raster suites' (`tests/all/typefaces.rs`).
#[test]
fn the_characters_of_a_token_are_read_as_the_characters_drawn() {
    let tokens = [
        // A `J` after a capital, a figure and a round letter: its hook reaches back under
        // each of them, so every pair is one run of inked columns.
        "XSJMK34S",
        "18JK73EU",
        // A `J` and a `1` side by side, the pair that shares a stem.
        "5A91JT85",
        // The wide capitals against the narrow ones, whose strokes touch.
        "AVWX8MW2",
        // The characters drawn around two counters against the ones drawn around one and
        // none: an 8 and a B against a 6, a 9, an A and a 0, against a 5 and a 7.
        "68B9A057",
    ];
    for token in tokens {
        for size in [18.0f32, 28.0] {
            let drawn = format!("SIM2REAL {token}");
            let read = read_raster(&page(&[&drawn], ttf_noto_sans::REGULAR, size), ImageFormat::Png)
                .unwrap_or_else(|error| {
                    panic!("case {token:?} at {size} pixels: the page must be read, got {error:?}")
                });
            assert_eq!(
                characters(&read.text),
                characters(&drawn),
                "case {token:?} at {size} pixels: the characters drawn must be the characters \
                 read, got {:?}",
                read.text
            );
        }
    }
}
