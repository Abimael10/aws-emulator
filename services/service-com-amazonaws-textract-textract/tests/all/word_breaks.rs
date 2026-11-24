// SPDX-License-Identifier: CC0-1.0

//! Where a line of a RASTER page separates its WORDS: one `WORD` block per word drawn,
//! whatever letterforms stand either side of the space.
//!
//! The rule under test, over a table of pages rather than one recorded page: a line that
//! draws N whitespace-separated words is reported as N `WORD` blocks, each the CHILD of
//! that line's `LINE` block, in reading order and each with its own `Geometry` — because
//! what a word space measures is the gap PAST what the two characters either side of it
//! leave when the face sets them side by side. The gap two characters leave is a property
//! of those two letterforms: a `T` and a `J` set together all but touch, while a `1` inks a
//! third of the tabular advance its face sets every figure on and leaves a gap inside its
//! own cell as wide as a space's — so a line measured against one width for all its gaps
//! loses the first pair's space and invents one inside the second's number.
//!
//! The rows differ along what the rule generalises over: the letterforms across each space
//! (a crossbar against a hook, a letter against a figure, a colon against a capital, a
//! bowl against a stem), how many words the line holds, the size the text is drawn at, and
//! whether the face sets every character in one cell (the embedded bitmap font) or in the
//! width of its own ink (a proportional outline face). Every page is rendered here from
//! known text, so what the assertion compares is a ROUND TRIP through real pixels.

use image::{GrayImage, ImageFormat};
use serde_json::Value;

use crate::document_extraction::{block_type, blocks_of, call, harness, of_type};
use crate::raster_documents::{raster_document, render, Paper};
use crate::typefaces::{dark_on_light, outline_page};

/// The lines each row draws, chosen for the letterforms that stand across their spaces.
const LINES: &[&[&str]] = &[
    // A crossbar against a hook (`T J`) and a bowl against a stem (`B L`): two pairs whose
    // ink all but meets when the face sets them side by side, so the gap a space leaves
    // there is narrower than the gap two straight-sided letters leave inside one word.
    &["ASYNC TEXTRACT JOB LINE"],
    // A letter against a FIGURE, and a number whose own figures stand a space's width apart
    // inside their tabular cells.
    &["DATE 01/02/2023", "TOTAL $12.34"],
    // A colon against a capital, and small letters against capitals.
    &["Name: John Smith", "Invoice Number: INV-99887"],
    // One word, which has no space to find: nothing inside it may be read as one.
    &["WIDGET"],
];

/// The words `line` draws, in reading order.
fn drawn_words(line: &str) -> Vec<&str> {
    line.split_whitespace().collect()
}

/// The `WORD` blocks the `LINE` block `line` names as its children, in the order the line
/// names them.
fn word_children<'a>(blocks: &'a [Value], line: &Value) -> Vec<&'a Value> {
    let ids: Vec<&str> = line
        .get("Relationships")
        .and_then(Value::as_array)
        .map(|relationships| {
            relationships
                .iter()
                .filter(|relationship| {
                    relationship.get("Type").and_then(Value::as_str) == Some("CHILD")
                })
                .filter_map(|relationship| relationship.get("Ids").and_then(Value::as_array))
                .flatten()
                .filter_map(Value::as_str)
                .collect()
        })
        .unwrap_or_default();
    ids.iter()
        .filter_map(|id| {
            blocks.iter().find(|block| {
                block_type(block) == "WORD" && block.get("Id").and_then(Value::as_str) == Some(id)
            })
        })
        .collect()
}

/// The left edge of a block's own `BoundingBox`.
fn left_of(block: &Value) -> f64 {
    block
        .get("Geometry")
        .and_then(|geometry| geometry.get("BoundingBox"))
        .and_then(|bbox| bbox.get("Left"))
        .and_then(Value::as_f64)
        .unwrap_or(-1.0)
}

/// Every page of the table is reported with one `WORD` block per word drawn on each of its
/// lines, in reading order and each with its own box.
///
/// A page drawn with the embedded font's own strokes round-trips character for character, so
/// those rows assert the word as it was drawn. A page drawn in a face NO template is a copy
/// of may read one glyph as another, or read two glyphs whose strokes touch as one, without
/// reading a space where the page left none: those rows therefore assert what this rule is
/// about — where the line separates its words — and the characters are
/// `crate::raster_documents`' and `crate::typefaces`' own assertion.
#[tokio::test]
async fn a_line_reports_one_word_block_per_word_drawn() {
    let router = harness();
    for lines in LINES {
        // A monospaced bitmap face and a proportional outline face, each at two sizes: one
        // line of ink, read four ways.
        let pages: Vec<(String, GrayImage, bool)> = vec![
            (
                "the bitmap face at twice its own size".to_string(),
                render(lines, Paper { scale: 2, ..Paper::plain() }),
                true,
            ),
            (
                "the bitmap face at four times its own size".to_string(),
                render(lines, Paper { scale: 4, ..Paper::plain() }),
                true,
            ),
            (
                "a proportional face at 18 pixels".to_string(),
                outline_page(lines, ttf_noto_sans::REGULAR, 18.0, dark_on_light()),
                false,
            ),
            (
                "a proportional face at 30 pixels".to_string(),
                outline_page(lines, ttf_noto_sans::REGULAR, 30.0, dark_on_light()),
                false,
            ),
        ];
        for (face, page, exact) in pages {
            let case = format!("{lines:?} set in {face}");
            let (status, body) = call(
                &router,
                "DetectDocumentText",
                &serde_json::json!({ "Document": raster_document(&page, ImageFormat::Png) }),
            )
            .await;
            assert_eq!(status, 200, "case {case}: expected HTTP 200, got {status} with {body}");
            let blocks = blocks_of(&body);
            let read_lines = of_type(&blocks, "LINE");
            assert_eq!(
                read_lines.len(),
                lines.len(),
                "case {case}: a page of {} lines of ink is read as that many LINE blocks, got {:?}",
                lines.len(),
                of_type(&blocks, "LINE")
                    .iter()
                    .map(|line| line.get("Text").and_then(Value::as_str).unwrap_or_default())
                    .collect::<Vec<_>>()
            );
            for (drawn, read) in lines.iter().zip(read_lines) {
                let words = drawn_words(drawn);
                let children = word_children(&blocks, read);
                assert_eq!(
                    children.len(),
                    words.len(),
                    "case {case}: the line {drawn:?} draws {} words, so it must report that \
                     many WORD children, got {:?}",
                    words.len(),
                    children
                        .iter()
                        .map(|word| word.get("Text").and_then(Value::as_str).unwrap_or_default())
                        .collect::<Vec<_>>()
                );
                let mut edge = -1.0f64;
                for (drawn_word, read_word) in words.iter().zip(&children) {
                    let text = read_word
                        .get("Text")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if exact {
                        assert_eq!(
                            text, *drawn_word,
                            "case {case}: the word drawn is the word reported, got {text:?}"
                        );
                    } else {
                        assert!(
                            !text.is_empty(),
                            "case {case}: the word {drawn_word:?} must be reported as the \
                             characters read inside it, got {text:?}"
                        );
                    }
                    let left = left_of(read_word);
                    assert!(
                        left > edge,
                        "case {case}: the words must be reported in reading order, each further \
                         across the page than the one before it; {text:?} starts at {left} after \
                         {edge}"
                    );
                    edge = left;
                }
            }
        }
    }
}
