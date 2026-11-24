// SPDX-License-Identifier: CC0-1.0

//! At which TURN a raster page is read: a page whose characters are drawn upright is
//! reported upright.
//!
//! A document can be scanned or photographed turned, so the recogniser reads every page at
//! each of its quarter turns and reports the turn the page is really set at
//! (`Geometry$RotationAngle`, which the `Block` docs state is "a numerical value
//! corresponding to the rotation of the WORD block. Possible values are 0, 90, 180, and
//! 270"). The rule under test, over a table of pages rather than one recorded page: a page
//! drawn upright is reported at 0 degrees with the text it was drawn from, because the turn
//! a page is read at is the turn whose marks AGREE with their characters best — not merely
//! the turn that accounts for most of the page's ink. The same ink turned covers the page
//! just as fully and agrees with whatever its shapes happen to resemble, so a reading
//! chosen on coverage alone can report a page of plain text upside down and name every mark
//! after a character that was never drawn.
//!
//! The rows differ along what the rule generalises over: the face the page is set in (a
//! pre-rasterised sans-serif face at three of its rasters and three of its weights, drawn by
//! `crate::foreign_typeface`'s own renderer, and the embedded bitmap font drawn by
//! `crate::raster_documents`'), what the lines hold (capitals, small letters, figures and
//! punctuation), and how many lines the page carries. Every page is rendered here from
//! known text, so the assertion is a round trip through real pixels.
//!
//! The file's name is an artefact of the tooling this suite was added with, which could
//! create and rewrite a file but neither rename nor delete one; what it holds is the
//! orientation rule above.

use image::{GrayImage, ImageFormat};
use noto_sans_mono_bitmap::{FontWeight, RasterHeight};
use serde_json::{json, Value};

use crate::document_extraction::{blocks_of, call, harness, of_type};
use crate::foreign_typeface::{draw, Face};
use crate::raster_documents::{raster_document, render, Paper};

/// The lines the rows are drawn from: one line of mixed case with a labelled field, and a
/// page of several lines of capitals, small letters, figures and punctuation.
const LINES: &[&[&str]] = &[
    &["Name: John Smith"],
    &["SIM2REAL LEDGER", "Invoice Number: INV-99887", "Total Due $12.34"],
];

/// How much of the text a page was drawn from came back, character for character, with the
/// spaces left out: where a line separates its words is `crate::word_breaks`' rule.
fn share_read(drawn: &[&str], read: &[String]) -> f32 {
    let bare = |text: &str| -> String { text.chars().filter(|ch| !ch.is_whitespace()).collect() };
    let drawn = bare(&drawn.join(""));
    let read = bare(&read.join(""));
    let right = drawn
        .chars()
        .zip(read.chars())
        .filter(|(drawn, read)| drawn == read)
        .count();
    if drawn.is_empty() {
        return 1.0;
    }
    right as f32 / drawn.chars().count() as f32
}

/// How much of a page's drawn text must come back for the page to have been read at the turn
/// it was drawn at.
///
/// A page read UPRIGHT reports the characters it was drawn with, give or take the glyph a
/// face no template is an exact copy of reads as another; the same ink read at another turn
/// reports the characters its turned marks happen to resemble, which is next to none of
/// them. The boundary therefore sits between those two readings rather than measuring how
/// exactly a face is recognised, which is `crate::typefaces`' own assertion.
const READ_UPRIGHT: f32 = 0.5;

/// Every page of the table is reported upright — `RotationAngle` 0 on every `WORD` block —
/// and with the text it was drawn from.
#[tokio::test]
async fn a_page_drawn_upright_is_read_upright() {
    let router = harness();
    for lines in LINES {
        let pages: Vec<(String, GrayImage)> = vec![
            (
                "the pre-rasterised face, regular at its 16-pixel raster".to_string(),
                draw(
                    lines,
                    Face {
                        weight: FontWeight::Regular,
                        height: RasterHeight::Size16,
                        ..Face::plain()
                    },
                ),
            ),
            (
                "the pre-rasterised face, light at its 20-pixel raster".to_string(),
                draw(
                    lines,
                    Face {
                        weight: FontWeight::Light,
                        height: RasterHeight::Size20,
                        ..Face::plain()
                    },
                ),
            ),
            (
                "the pre-rasterised face, bold at its 24-pixel raster".to_string(),
                draw(
                    lines,
                    Face {
                        weight: FontWeight::Bold,
                        height: RasterHeight::Size24,
                        ..Face::plain()
                    },
                ),
            ),
            (
                "the embedded bitmap font at twice its own size".to_string(),
                render(lines, Paper { scale: 2, ..Paper::plain() }),
            ),
        ];
        for (face, page) in pages {
            let case = format!("{lines:?} set in {face}");
            let (status, body) = call(
                &router,
                "DetectDocumentText",
                &json!({ "Document": raster_document(&page, ImageFormat::Png) }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case}: expected HTTP 200, got {status} with {body}"
            );
            let blocks = blocks_of(&body);
            let words = of_type(&blocks, "WORD");
            assert!(
                !words.is_empty(),
                "case {case}: a page of ink must be read as the words drawn on it, got {body}"
            );
            for word in &words {
                let turn = word
                    .get("Geometry")
                    .and_then(|geometry| geometry.get("RotationAngle"))
                    .and_then(Value::as_f64);
                assert_eq!(
                    turn,
                    Some(0.0),
                    "case {case}: a page drawn upright is read upright, so every WORD reports \
                     RotationAngle 0, got {turn:?} on {:?}",
                    word.get("Text")
                );
            }
            let read: Vec<String> = of_type(&blocks, "LINE")
                .iter()
                .map(|line| {
                    line.get("Text")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                })
                .collect();
            assert_eq!(
                read.len(),
                lines.len(),
                "case {case}: a page of {} lines of ink is read as that many LINE blocks, got \
                 {read:?}",
                lines.len()
            );
            let share = share_read(lines, &read);
            assert!(
                share >= READ_UPRIGHT,
                "case {case}: the characters read must be the characters drawn, got {read:?} \
                 against {lines:?} ({:.0}% of them)",
                share * 100.0
            );
        }
    }
}
