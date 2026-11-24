// SPDX-License-Identifier: CC0-1.0

//! End-to-end coverage of WHAT a `PAGE` block's `Geometry` describes: the page, not the
//! ink that was found on it.
//!
//! `Geometry` is "information about where the following items are located on a document
//! page: detected page, text, key-value pairs, tables, table cells, and selection
//! elements" and `BoundingBox` is "the bounding box around the detected page, text,
//! key-value pair, table, table cell, or selection element on a document page", so the
//! item a `PAGE` block locates is the PAGE itself; the text on it is located by the
//! `LINE`, `WORD` and feature blocks the page is read into. Every pixel submitted is
//! page — this service reads the document it is given as one page — so for any submitted
//! document the `PAGE` block reports the whole page as its box and the page's own four
//! corners as its polygon, listed from the corner the page's characters were read from,
//! and every block read off the page sits inside it.
//!
//! The rows of the table differ along what the rule generalises over: how much of the
//! page the ink covers (a word in a corner of a mostly blank page, lines filling the page,
//! a page with no ink at all), WHERE on the page it sits, the page's proportions, the
//! polarity of the ink, the quarter turn the page is submitted at, the read path it is
//! read through (`DetectDocumentText` and `AnalyzeDocument` with each feature set), and
//! whether the page's text carries measured geometry at all.
//!
//! Every page here is drawn in this test, so the assertions compare what the service
//! reports with the page that was submitted — never with a recorded answer.

use image::{GrayImage, ImageFormat, Luma};
use serde_json::{json, Value};
use service_com_amazonaws_textract_textract::state;

use crate::block_geometry::{bbox_of, polygon_of, Rect};
use crate::document_extraction::{
    assert_block_graph, block_id, block_type, blocks_of, call, harness, of_type,
};
use crate::raster_documents::{raster_document, render, Paper};

/// One row of the table: the page that is submitted, and how it is read.
struct Case {
    name: &'static str,
    lines: Vec<&'static str>,
    paper: Paper,
    /// The size of the page the drawn ink is placed on, in pixels, when it is to be
    /// bigger than the ink: `None` leaves the drawn page as it is.
    page: Option<(u32, u32)>,
    /// Where the drawn ink is placed on that page, as a fraction of the room left over,
    /// so `(0.0, 0.0)` is its top-left corner and `(1.0, 1.0)` its bottom-right one.
    at: (f32, f32),
    /// Clockwise quarter turns applied to the page before it is submitted.
    turns: u8,
    operation: &'static str,
    /// The `FeatureTypes` of an `AnalyzeDocument` row; empty for text detection.
    features: Vec<&'static str>,
}

impl Case {
    /// A row read through `DetectDocumentText`, which is the plain text-detection path.
    fn detected(name: &'static str, lines: Vec<&'static str>) -> Self {
        Self {
            name,
            lines,
            paper: Paper::plain(),
            page: None,
            at: (0.0, 0.0),
            turns: 0,
            operation: "DetectDocumentText",
            features: Vec::new(),
        }
    }
}

/// The lines a row that needs no text of its own is drawn from: a heading, a labelled
/// field and a total, so the analysis rows have a key/value pair to derive.
fn sample_lines() -> Vec<&'static str> {
    vec!["SIMPARITY LEDGER", "Account Number: 48213", "Total Due 1250.00"]
}

fn cases() -> Vec<Case> {
    vec![
        // The crux of the rule: one word on a page many times its size. The ink covers a
        // few per cent of the page, and the PAGE block still reports the page.
        Case {
            page: Some((320, 240)),
            ..Case::detected("one word in the top-left corner of a large page", vec!["LEDGER"])
        },
        // The same ink, moved. Nothing about the PAGE block may change with it.
        Case {
            page: Some((320, 240)),
            at: (1.0, 1.0),
            ..Case::detected("the same word in the bottom-right corner", vec!["LEDGER"])
        },
        Case {
            page: Some((320, 240)),
            at: (0.5, 0.5),
            ..Case::detected("the same word in the middle of the page", vec!["LEDGER"])
        },
        // The other extreme: ink over nearly the whole page.
        Case::detected("lines filling the page", sample_lines()),
        // A page with no ink at all: one PAGE block, which is still the whole page.
        Case {
            page: Some((160, 120)),
            ..Case::detected("a page carrying no ink at all", Vec::new())
        },
        // Proportions: the box's members are ratios of the page's own width and height,
        // so a page far wider than it is tall reports the same box as a square one.
        Case {
            page: Some((480, 120)),
            at: (0.5, 0.5),
            ..Case::detected("a page far wider than it is tall", vec!["LEDGER"])
        },
        Case {
            page: Some((160, 640)),
            at: (0.5, 0.5),
            ..Case::detected("a page far taller than it is wide", vec!["LEDGER"])
        },
        // Light text on a dark ground: the ink is the lighter class, and a page is still a
        // page.
        Case {
            paper: Paper {
                background: 20,
                ink: 240,
                ..Paper::plain()
            },
            page: Some((400, 300)),
            at: (0.25, 0.75),
            ..Case::detected("light ink on a dark ground", sample_lines())
        },
        // A rule drawn across the page is page structure, no line of text: it is inside
        // the page like everything else.
        Case {
            paper: Paper {
                rule: true,
                ..Paper::plain()
            },
            ..Case::detected("a page carrying a rule", sample_lines())
        },
        // The quarter turns: the box is the page however the page was turned, and the
        // polygon starts at the corner the characters were read from.
        Case {
            turns: 1,
            page: Some((400, 300)),
            at: (0.2, 0.8),
            ..Case::detected("a page submitted a quarter turn clockwise", sample_lines())
        },
        Case {
            turns: 2,
            ..Case::detected("a page submitted a half turn", sample_lines())
        },
        Case {
            turns: 3,
            page: Some((400, 300)),
            ..Case::detected("a page submitted three quarters of a turn", sample_lines())
        },
        // The analysis path, which assembles its own response around the same blocks, with
        // each feature set it derives blocks for.
        Case {
            operation: "AnalyzeDocument",
            features: vec!["FORMS"],
            page: Some((400, 300)),
            at: (0.9, 0.1),
            ..Case::detected("the analysis path over a labelled page", sample_lines())
        },
        Case {
            operation: "AnalyzeDocument",
            features: vec!["TABLES"],
            ..Case::detected(
                "the analysis path over a page carrying a table",
                vec!["Item      Price", "Pen       2.50", "Pad       11.75"],
            )
        },
        Case {
            operation: "AnalyzeDocument",
            features: vec!["FORMS", "TABLES"],
            page: Some((420, 320)),
            at: (0.1, 0.9),
            ..Case::detected("the analysis path over both feature sets", sample_lines())
        },
    ]
}

/// `drawn` placed on a `width` x `height` page whose ground is `background`, at `at` of
/// the room left over — so a page much larger than its ink can be drawn with that ink
/// anywhere on it.
fn on_page(drawn: &GrayImage, (width, height): (u32, u32), background: u8, at: (f32, f32)) -> GrayImage {
    let mut page = GrayImage::from_pixel(width.max(drawn.width()), height.max(drawn.height()), Luma([background]));
    let room = |page: u32, ink: u32, at: f32| -> u32 {
        let left = page.saturating_sub(ink);
        (left as f32 * at.clamp(0.0, 1.0)).round() as u32
    };
    let (dx, dy) = (
        room(page.width(), drawn.width(), at.0),
        room(page.height(), drawn.height(), at.1),
    );
    for y in 0..drawn.height() {
        for x in 0..drawn.width() {
            page.put_pixel(x + dx, y + dy, *drawn.get_pixel(x, y));
        }
    }
    page
}

fn turned(page: &GrayImage, turns: u8) -> GrayImage {
    match turns % 4 {
        1 => image::imageops::rotate90(page),
        2 => image::imageops::rotate180(page),
        3 => image::imageops::rotate270(page),
        _ => page.clone(),
    }
}

/// The page one row submits.
fn page_of(case: &Case) -> GrayImage {
    let drawn = if case.lines.is_empty() {
        GrayImage::from_pixel(1, 1, Luma([case.paper.background]))
    } else {
        render(&case.lines, case.paper)
    };
    let placed = match case.page {
        Some(size) => on_page(&drawn, size, case.paper.background, case.at),
        None => drawn,
    };
    turned(&placed, case.turns)
}

/// The four corners of the whole page, clockwise from its top-left one, as the
/// page-relative points `Polygon` is expressed in.
const PAGE_CORNERS: [(f64, f64); 4] = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];

/// `PAGE_CORNERS` started at the corner a page whose characters are turned by `rotation`
/// was read from: an upright page is read from its top-left corner, and each quarter turn
/// moves that corner one step clockwise.
fn corners_read_from(rotation: f64) -> Vec<(f64, f64)> {
    let first = ((rotation / 90.0).round() as i64).rem_euclid(4) as usize;
    (0..PAGE_CORNERS.len())
        .map(|step| PAGE_CORNERS[(step + first) % PAGE_CORNERS.len()])
        .collect()
}

/// The `Geometry$RotationAngle` of the first `WORD` of an answer, or `0.0` for a page that
/// was read into no words at all.
fn rotation_of(blocks: &[Value]) -> f64 {
    of_type(blocks, "WORD")
        .first()
        .and_then(|word| {
            word.get("Geometry")
                .and_then(|geometry| geometry.get("RotationAngle"))
                .and_then(Value::as_f64)
        })
        .unwrap_or(0.0)
}

/// The whole page, as the box a `PAGE` block must report.
const WHOLE_PAGE: Rect = Rect {
    left: 0.0,
    top: 0.0,
    width: 1.0,
    height: 1.0,
};

fn assert_is_the_page(case: &str, page_box: &Rect) {
    assert!(
        (page_box.left - WHOLE_PAGE.left).abs() < 1e-6
            && (page_box.top - WHOLE_PAGE.top).abs() < 1e-6
            && (page_box.width - WHOLE_PAGE.width).abs() < 1e-6
            && (page_box.height - WHOLE_PAGE.height).abs() < 1e-6,
        "case {case:?}: the PAGE block locates the page, so its BoundingBox must be \
         {WHOLE_PAGE:?}, got {page_box:?}"
    );
}

#[tokio::test]
async fn a_page_block_locates_the_page_whatever_ink_was_found_on_it() {
    let router = harness();

    // Every row's PAGE box, to state at the end what each row states on its own: the box
    // does not vary with the document at all.
    let mut page_boxes: Vec<(&str, Rect)> = Vec::new();
    // The box of the first LINE of each row, which DOES vary: the ink moved between rows,
    // so a PAGE box that followed the ink could not have stayed put.
    let mut line_boxes: Vec<(&str, Rect)> = Vec::new();

    for case in cases() {
        let page = page_of(&case);
        let document = raster_document(&page, ImageFormat::Png);
        let request = if case.features.is_empty() {
            json!({ "Document": document })
        } else {
            json!({ "Document": document, "FeatureTypes": case.features })
        };
        let (status, body) = call(&router, case.operation, &request).await;
        assert_eq!(
            status, 200,
            "case {:?}: a readable page must answer 200, got {status} with {body}",
            case.name
        );
        let blocks = blocks_of(&body);
        assert_block_graph(case.name, &body, &blocks, None);

        let pages = of_type(&blocks, "PAGE");
        assert_eq!(
            pages.len(),
            1,
            "case {:?}: one submitted page is one PAGE block, got {} of them",
            case.name,
            pages.len()
        );
        let page_box = bbox_of(case.name, pages[0]);
        assert_is_the_page(case.name, &page_box);
        page_boxes.push((case.name, page_box));

        // The polygon is that page's corners, in the frame the page was read in.
        let rotation = rotation_of(&blocks);
        let expected = corners_read_from(rotation);
        let reported = polygon_of(case.name, pages[0]);
        assert_eq!(
            reported.len(),
            4,
            "case {:?}: a PAGE outlines the page with four points, got {reported:?}",
            case.name
        );
        for (index, (point, want)) in reported.iter().zip(expected.iter()).enumerate() {
            assert!(
                (point.0 - want.0).abs() < 1e-6 && (point.1 - want.1).abs() < 1e-6,
                "case {:?}: the PAGE Polygon must be the page's own corners read from the \
                 corner its characters were read at ({rotation} degrees: {expected:?}), got \
                 {reported:?} differing at point {index}",
                case.name
            );
        }

        // Every block read off the page sits inside the page it was read off.
        for block in &blocks {
            if block_type(block) == "PAGE" {
                continue;
            }
            let block_box = bbox_of(case.name, block);
            assert!(
                block_box.left >= -1e-6
                    && block_box.top >= -1e-6
                    && block_box.right() <= 1.0 + 1e-6
                    && block_box.bottom() <= 1.0 + 1e-6,
                "case {:?}: {} block {} at {block_box:?} must sit inside its PAGE \
                 {page_box:?}",
                case.name,
                block_type(block),
                block_id(block)
            );
        }

        if let Some(line) = of_type(&blocks, "LINE").first() {
            line_boxes.push((case.name, bbox_of(case.name, line)));
        }
    }

    // One box for every document, however much of the page its ink covered and wherever
    // on the page it sat.
    for (case, page_box) in &page_boxes {
        assert_is_the_page(case, page_box);
    }
    // And the documents really did differ: the first three rows draw the same word in
    // three places, and the LINE blocks they were read into report three different boxes.
    let moved: Vec<(&str, Rect)> = line_boxes
        .iter()
        .filter(|(case, _)| case.contains("corner") || case.contains("middle"))
        .cloned()
        .collect();
    assert!(
        moved.len() >= 2,
        "the table must submit the same ink in more than one place, got {moved:?}"
    );
    for pair in moved.windows(2) {
        assert!(
            (pair[0].1.left - pair[1].1.left).abs() > 1e-3
                || (pair[0].1.top - pair[1].1.top).abs() > 1e-3,
            "the ink moved between {:?} and {:?}, so their LINE boxes must differ: {:?} \
             against {:?}",
            pair[0].0,
            pair[1].0,
            pair[0].1,
            pair[1].1
        );
    }
}

/// RULE: the `PAGE` block is the page on the fallback layout too — a document whose text
/// carries no measured geometry at all, where every other block is laid out on the
/// character grid the text's own lines and columns place it on.
///
/// Driven through `state::blocks`, the one block builder every read path of this service
/// answers from, because no submitted page produces unmeasured text: a raster is measured.
#[test]
fn a_page_block_is_the_page_for_text_that_carries_no_measurement() {
    // (case, the document's pages)
    let cases: Vec<(&str, Vec<String>)> = vec![
        ("one short line", vec!["LEDGER\n".to_string()]),
        (
            "a labelled line and a table row",
            vec!["Account Number: 48213\nItem\tPrice\nPen\t2.50\n".to_string()],
        ),
        (
            "two pages of different lengths",
            vec![
                "SIMPARITY LEDGER\n".to_string(),
                "Total Due 1250.00\nAccount Number: 48213\n".to_string(),
            ],
        ),
    ];

    for (case, pages) in cases {
        let page_count = pages.len();
        let content = state::DocumentContent {
            pages,
            confidence: 90.0,
            measured: Vec::new(),
        };
        let blocks = state::blocks(
            "a-document-key",
            &content,
            state::FeatureSet {
                forms: true,
                tables: true,
            },
        );
        let page_blocks: Vec<_> = blocks
            .iter()
            .filter(|block| block.block_type.as_deref() == Some("PAGE"))
            .collect();
        assert_eq!(
            page_blocks.len(),
            page_count,
            "case {case:?}: one PAGE block per page of the document, got {}",
            page_blocks.len()
        );
        for block in page_blocks {
            let geometry = block
                .geometry
                .as_ref()
                .unwrap_or_else(|| panic!("case {case:?}: a PAGE must carry a Geometry"));
            let bbox = geometry
                .bounding_box
                .as_ref()
                .unwrap_or_else(|| panic!("case {case:?}: a PAGE must carry a BoundingBox"));
            assert!(
                bbox.left.abs() < 1e-6
                    && bbox.top.abs() < 1e-6
                    && (bbox.width - 1.0).abs() < 1e-6
                    && (bbox.height - 1.0).abs() < 1e-6,
                "case {case:?}: the PAGE block locates the page, so its BoundingBox must be \
                 the whole page, got {bbox:?}"
            );
            let polygon = geometry
                .polygon
                .as_ref()
                .unwrap_or_else(|| panic!("case {case:?}: a PAGE must carry a Polygon"));
            let reported: Vec<(f64, f64)> = polygon
                .iter()
                .map(|point| (f64::from(point.x), f64::from(point.y)))
                .collect();
            assert_eq!(
                reported,
                PAGE_CORNERS.to_vec(),
                "case {case:?}: the PAGE Polygon must be the page's own corners, got \
                 {reported:?}"
            );
        }
    }
}
