// SPDX-License-Identifier: CC0-1.0

//! Reading a page whose INK IS BROKEN: a character whose ink stands in two pieces is still
//! that character, and no piece of it is reported as a character of its own.
//!
//! The rule under test, over a table of pages rather than one recorded page: the `Text` a
//! raster operation reports is the characters the page was DRAWN with, so a page carries no
//! character it never set. A stroke of a drawn character does not always reach the reader in
//! one piece — a hairline stroke falls under the floor that tells ink from ground, a column
//! of a scan drops out, a lossy coding thins the waist of a figure — and what is left is two
//! marks a column apart where the page set one character. Each of those pieces is ink shaped
//! like something: the ends of the strokes a break separated agree closely with the
//! punctuation they look like, which is why a reading taken piece by piece reports a mark of
//! punctuation standing inside a word that never held one.
//!
//! The break is drawn here the way a scan loses a column: one full-height column of the page
//! INSIDE a character's own ink is set back to the page's ground, so that character's ink
//! stands as two runs of inked columns. Which character it falls in differs row by row, since
//! the column is picked by counting the page's own inked columns rather than by naming a
//! letter.
//!
//! The other half of the rule is that the punctuation a page REALLY sets is still reported:
//! a point inside a number, a colon after a label and a hyphen inside an identifier are
//! characters of the page, not pieces of the letters beside them. Rows of both kinds are
//! below, so the rule cannot be satisfied by dropping every narrow mark.
//!
//! The rows differ along what the rule generalises over: where the break falls, the text the
//! line is set from (a token of capitals and figures, words, a labelled line, a number, an
//! identifier punctuated with a hyphen), the size and the weight the page is set at, and the
//! format it is submitted in.

use image::{GrayImage, ImageFormat, Luma};
use serde_json::json;

use crate::document_extraction::{assert_block_graph, blocks_of, call, harness};
use crate::raster_documents::{raster_document, texts_of};
use crate::typefaces::{dark_on_light, outline_page};

/// How far a pixel's level must stand from the page's ground to be ink: an anti-aliased edge
/// carries less than a stroke's middle, and this is well inside both.
const INK_APART: u8 = 40;

/// The characters of `text`, with the spaces left out: where a line is broken into words is
/// the word-separation rule's (`crate::typefaces`), not this one's.
fn characters(text: &str) -> String {
    text.chars().filter(|ch| !ch.is_whitespace()).collect()
}

/// Whether column `x` of `page` carries any ink at all.
fn inked(page: &GrayImage, ground: u8, x: u32) -> bool {
    (0..page.height()).any(|y| page.get_pixel(x, y).0[0].abs_diff(ground) > INK_APART)
}

/// `page` with one full-height column of its ink DROPPED OUT: the `which`-th column that
/// carries ink and has inked columns either side of it, so the break falls inside a
/// character's own ink rather than in the space between two characters.
///
/// Returning the page unchanged is not possible here — every page below carries ink several
/// columns wide — but a page that held no such column would simply be read as it was drawn.
fn with_a_dropped_column(page: &GrayImage, which: usize) -> GrayImage {
    let ground = page.get_pixel(0, 0).0[0];
    let inside: Vec<u32> = (1..page.width().saturating_sub(1))
        .filter(|x| {
            inked(page, ground, *x) && inked(page, ground, x - 1) && inked(page, ground, x + 1)
        })
        .collect();
    let mut broken = page.clone();
    if inside.is_empty() {
        return broken;
    }
    let at = inside[which % inside.len()];
    for y in 0..broken.height() {
        broken.put_pixel(at, y, Luma([ground]));
    }
    broken
}

/// Read `page` through `DetectDocumentText` and hand back the LINE texts it reports.
async fn read_lines(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    page: &GrayImage,
    format: ImageFormat,
) -> Vec<String> {
    let request = json!({ "Document": raster_document(page, format) });
    let (status, body) = call(router, "DetectDocumentText", &request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: a page in a format Textract reads must be read, got {status} with \
         {body}"
    );
    let blocks = blocks_of(&body);
    assert_block_graph(case, &body, &blocks, None);
    texts_of(&blocks, "LINE")
}

/// One page of the table: the lines it is set from, the face and size it is set at, the
/// format it is submitted in, and which of its inked columns drops out.
struct Page {
    case: &'static str,
    lines: Vec<&'static str>,
    face: &'static [u8],
    size: f32,
    format: ImageFormat,
    /// The inked column that drops out, counted among the columns inside a character's ink.
    dropped: usize,
}

/// RULE: a page one of whose characters stands in two pieces is read as the characters it was
/// drawn with — no piece is reported as a character, and none of the page's own characters is
/// lost.
#[tokio::test]
async fn a_character_broken_into_two_pieces_is_read_as_the_one_character_drawn() {
    let router = harness();
    let pages = vec![
        Page {
            case: "a token of capitals and figures, the break near its first character",
            lines: vec!["SIM2REAL ECWGGCY3", "TEXTRACT CHECK"],
            face: ttf_noto_sans::REGULAR,
            size: 24.0,
            format: ImageFormat::Png,
            dropped: 2,
        },
        Page {
            case: "the same page, the break a few columns further in",
            lines: vec!["SIM2REAL ECWGGCY3", "TEXTRACT CHECK"],
            face: ttf_noto_sans::REGULAR,
            size: 24.0,
            format: ImageFormat::Png,
            dropped: 9,
        },
        Page {
            case: "a token of the letterforms that share a shape, set larger",
            lines: vec!["SIM2REAL B8S5OQ01"],
            face: ttf_noto_sans::REGULAR,
            size: 32.0,
            format: ImageFormat::Png,
            dropped: 5,
        },
        Page {
            case: "a token of wide capitals against narrow ones",
            lines: vec!["SIM2REAL VWMWXY7K"],
            face: ttf_noto_sans::REGULAR,
            size: 28.0,
            format: ImageFormat::Png,
            dropped: 13,
        },
        Page {
            case: "words rather than a token, set in the bold cut",
            lines: vec!["Invoice Number"],
            face: ttf_noto_sans::BOLD,
            size: 24.0,
            format: ImageFormat::Png,
            dropped: 4,
        },
        Page {
            case: "a page submitted as a JPEG",
            lines: vec!["SIM2REAL ECWGGCY3"],
            face: ttf_noto_sans::REGULAR,
            size: 28.0,
            format: ImageFormat::Jpeg,
            dropped: 7,
        },
    ];

    for page in pages {
        let drawn = outline_page(&page.lines, page.face, page.size, dark_on_light());
        let broken = with_a_dropped_column(&drawn, page.dropped);
        let read = read_lines(&router, page.case, &broken, page.format).await;
        assert_eq!(
            read.len(),
            page.lines.len(),
            "case {:?}: a page of {} lines of ink is read as that many lines, got {read:?}",
            page.case,
            page.lines.len()
        );
        for (line, (read, drawn)) in read.iter().zip(page.lines.iter()).enumerate() {
            assert_eq!(
                characters(read),
                characters(drawn),
                "case {:?}: line {line} was drawn {drawn:?}, got {read:?}",
                page.case
            );
        }
    }
}

/// RULE: the PUNCTUATION a page really sets is a character of that page — a point inside a
/// number, a colon after a label, a hyphen inside an identifier — whether or not one of its
/// characters was broken, so the rule above cannot be met by dropping narrow marks.
#[tokio::test]
async fn the_punctuation_a_page_sets_is_still_reported() {
    let router = harness();
    let lines = vec!["Account Number: INV-99887", "Total Due 1250.00"];
    for (case, dropped) in [
        ("a page read as it was drawn", None),
        ("the same page with a column of a character dropped out", Some(3usize)),
        ("the same page with a later column dropped out", Some(11usize)),
    ] {
        for size in [24.0f32, 32.0] {
            let case = format!("{case} at {size} pixels");
            let drawn = outline_page(&lines, ttf_noto_sans::REGULAR, size, dark_on_light());
            let page = match dropped {
                Some(which) => with_a_dropped_column(&drawn, which),
                None => drawn,
            };
            let read = read_lines(&router, &case, &page, ImageFormat::Png).await;
            let joined = read.join(" ");
            for mark in [':', '-', '.'] {
                assert!(
                    joined.contains(mark),
                    "case {case:?}: the {mark:?} the page sets must be reported, got {read:?}"
                );
            }
            // And nothing the page never set: the mark count of the reading is the mark
            // count of the lines drawn.
            let drawn_marks = characters(&lines.join(""))
                .chars()
                .filter(|ch| !ch.is_ascii_alphanumeric())
                .count();
            let read_marks = characters(&joined)
                .chars()
                .filter(|ch| !ch.is_ascii_alphanumeric())
                .count();
            assert_eq!(
                read_marks, drawn_marks,
                "case {case:?}: the page sets {drawn_marks} marks of punctuation, got \
                 {read_marks} in {read:?}"
            );
        }
    }
}

/// The same pixels answer the same way: a broken page is read from its ink, not from a roll.
#[tokio::test]
async fn a_broken_page_reads_the_same_twice() {
    let router = harness();
    let lines = vec!["SIM2REAL ECWGGCY3"];
    let drawn = outline_page(&lines, ttf_noto_sans::REGULAR, 24.0, dark_on_light());
    let broken = with_a_dropped_column(&drawn, 6);
    let case = "a broken page read twice";
    let first = read_lines(&router, case, &broken, ImageFormat::Png).await;
    let again = read_lines(&router, case, &broken, ImageFormat::Png).await;
    assert_eq!(
        first, again,
        "case {case:?}: the same page must answer with the same lines"
    );
    assert!(
        !first.is_empty(),
        "case {case:?}: the page must be read at all, got {first:?}"
    );
}
