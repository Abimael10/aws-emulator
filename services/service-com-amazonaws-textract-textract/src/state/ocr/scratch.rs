// SPDX-License-Identifier: CC0-1.0

//! How a line that sets its characters on ONE ADVANCE is separated into words: the
//! separation stage of [`super::read_raster`], over marks laid out here rather than off a
//! page.
//!
//! A proportional face sets every letter in the width of its own ink, but its FIGURES on a
//! single advance — tabular figures, so columns of them line up. A narrow figure therefore
//! inks a fraction of its own cell and leaves a gap either side of it as wide as a space's,
//! which is why [`super::separate`] measures two figures by the STEP from one to the next
//! past that advance ([`super::figure_advance`]) instead of by their gap.
//!
//! The cases here are marks with known boxes, so the layout under test is exact rather than
//! whatever a rasteriser happened to draw: [`super::probe`] and the crate's raster suites
//! read real pixels, and what those cannot pin is the boundary itself — a figure's gap and a
//! space's are the same width there by construction.
//!
//! TODO: this file belongs at `src/state/ocr/figures.rs`, which is the module it is declared
//! as; it is reached through a `#[path]` override only because the tooling that added it
//! could create a file but not rename one. Rename it and drop the override on the next
//! touch of this module.

use super::*;

/// How wide a figure's own cell is, in pixels: the advance the face below sets every figure
/// on, whatever each of them inks inside it.
const ADVANCE: usize = 16;

/// How much a space adds to the step across it, in pixels: about a quarter of the em a
/// figure's advance is well over half of, which is the proportion a sans-serif face sets a
/// space at.
const SPACE: usize = 8;

/// The ink width of a figure drawn WIDE, as a `0` or an `8` is: most of its cell.
const WIDE: usize = 13;

/// The ink width of a figure drawn NARROW, as a `1` is: a stem and its flag, leaving the
/// rest of its cell empty.
const NARROW: usize = 5;

/// How much ink a POINT carries, and the advance a face sets it on: its own width, since a
/// face sets its figures on one advance but its punctuation on what each mark needs.
const POINT: (usize, usize) = (3, 7);

/// How much ink a letter carries, and the advance it is set on: its own width and a bearing,
/// which is what makes the rest of the line PROPORTIONAL.
const LETTER: (usize, usize) = (11, 14);

/// One mark of a line: the box its ink occupies, and the character it was named with.
fn mark(x0: usize, width: usize, ch: char) -> Settled {
    Settled {
        glyph: GlyphBox {
            x0,
            x1: x0 + width - 1,
            y0: 0,
            y1: 19,
        },
        ink: (width * 20) as f32,
        named: Some(glyphs::Named {
            ch,
            score: 0.9,
            open: false,
            letter: Some(ch),
        }),
        piece: None,
    }
}

/// How much ink `ch` carries, and the advance the face sets it on: every FIGURE on one
/// advance of [`ADVANCE`] whatever it inks inside it, and everything else on its own width.
fn set(ch: char) -> (usize, usize) {
    match ch {
        '.' | ',' => POINT,
        '1' => (NARROW, ADVANCE),
        figure if figure.is_ascii_digit() => (WIDE, ADVANCE),
        _ => LETTER,
    }
}

/// Lay out `words` on a line, with a space's own advance between two of them.
///
/// The one thing the layout does NOT do is leave a wider gap for a space than a narrow
/// figure leaves inside its cell: that is the case the separation has to get right.
fn line_of(words: &[&str]) -> Vec<Settled> {
    let mut marks = Vec::new();
    let mut pen = 7usize;
    for (index, word) in words.iter().enumerate() {
        if index > 0 {
            pen += SPACE;
        }
        for ch in word.chars() {
            let (ink, advance) = set(ch);
            marks.push(mark(pen, ink, ch));
            pen += advance;
        }
    }
    marks
}

/// A line's figures are separated into the numbers the line sets, whatever each figure inks
/// inside the advance it is set on.
#[test]
fn the_figures_of_one_number_are_one_word() {
    let cases: Vec<(&str, Vec<&str>)> = vec![
        ("a number of wide figures", vec!["48203"]),
        ("a number whose narrow figures leave their cells empty", vec!["11711"]),
        ("a number of nothing but narrow figures", vec!["111"]),
        ("an amount of narrow figures", vec!["Due", "11.1"]),
        ("an amount, whose point inks as little as a narrow figure", vec!["1250.00"]),
        ("two numbers the line really does separate", vec!["4821", "3307"]),
        ("a labelled number", vec!["Account", "Number", "48213"]),
        ("a number of one figure", vec!["Page", "1"]),
    ];
    for (case, words) in cases {
        let marks = line_of(&words);
        let read = separate(&marks).words();
        assert_eq!(
            read,
            words.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "case {case:?}: the line sets the words {words:?}, got {read:?}"
        );
    }
}
