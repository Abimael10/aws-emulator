// SPDX-License-Identifier: CC0-1.0

//! The TABLE a page's own RULING LINES draw, end to end over the wire (`awsJson1_1`,
//! `POST /` with `X-Amz-Target: Textract.<Operation>`).
//!
//! THE RULE UNDER TEST: a long thin straight run of ink is a rule — a ruling line, a cell
//! border, an underline — and a rule is page STRUCTURE rather than text. No face draws a
//! character as a stroke many times longer than it is thick, so a rule is never reported as
//! a `WORD` of the page and never joins the marks of the row it bounds into one; and what a
//! rule between two marks of a row DOES is divide that row's cells, so a ruled grid is read
//! as one `CELL` per drawn cell, with 1-based `RowIndex`/`ColumnIndex` and `CHILD` words
//! that are only the words drawn inside that cell.
//!
//! The tables span what the rule generalises over: the SIZE the page is drawn at (which
//! scales every stroke and every gap on it), how THICK the ruling lines are, how many rows
//! and columns the grid has, how many words stand in a cell, whether a cell was left empty,
//! and — as the controls for the other halves of the rule — the same grid drawn with no
//! ruling lines at all, whose columns the gaps alone must still find, and a grid whose cells
//! were left empty, which is ink on the page but no text in it.
//!
//! Every page here is rendered in this test from known text with the same public-domain 8x8
//! font the recogniser holds templates for, so the words read can be compared with the words
//! drawn: the assertions are a ROUND TRIP through real pixels rather than a comparison
//! against a recorded answer.

use image::{GrayImage, ImageFormat, Luma};
use serde_json::{json, Value};

use crate::document_extraction::{blocks_of, call, harness, of_type};
use crate::raster_documents::{raster_document, texts_of, CELL};

/// How a ruled table is drawn. Each knob is one axis the rule must hold along.
#[derive(Debug, Clone, Copy)]
struct Grid {
    /// Pixels per font pixel: the size the page is drawn at.
    scale: usize,
    /// How thick a ruling line is, in font pixels. Zero draws none at all.
    rule: usize,
    /// Blank glyph CELLS between a cell's own boundary and its text, across and down. In
    /// cells rather than pixels so that every row of the grid sets its characters on the
    /// font's own lattice, as a page set in a monospaced face does.
    pad: usize,
}

/// One row of the grid as it is drawn: the text of each of its cells.
type Row<'a> = Vec<&'a str>;

/// Where a cell of a grid stands: its 1-based row and column.
type At = (usize, usize);

/// The heading every page below draws above its grid: a labelled line, which is no row of
/// the table and must not be read as one.
const HEADING: &str = "Name: Test User";

/// Draw `rows` as a ruled grid under [`HEADING`], with the embedded 8x8 font.
///
/// `font8x8::legacy::BASIC_LEGACY` is the same public-domain font the recogniser's templates
/// come from: one byte per row, bit 0 the leftmost pixel. Every glyph is drawn on the font's
/// own cell grid and the ruling lines are drawn inside the padding between the cells, so the
/// characters of one row stand where a monospaced face would set them.
fn ruled_page(rows: &[Vec<&str>], grid: Grid) -> GrayImage {
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    // Each column's text, in glyph cells: as wide as its widest entry, padded either side.
    let widths: Vec<usize> = (0..columns)
        .map(|column| {
            rows.iter()
                .map(|row| row.get(column).map_or(0, |text| text.chars().count()))
                .max()
                .unwrap_or(0)
                + 2 * grid.pad
        })
        .collect();
    let margin = 1usize;
    // Where each column begins, in glyph cells: the cell its own ruling line stands in.
    let mut edges = vec![margin];
    for width in &widths {
        let last = *edges.last().unwrap_or(&margin);
        edges.push(last + width);
    }
    let row_cells = 1 + 2 * grid.pad;
    let table_left = margin * CELL;
    let table_right = edges.last().unwrap_or(&margin) * CELL + grid.rule;
    let table_top = (margin + 2) * CELL;
    let table_bottom = table_top + rows.len() * row_cells * CELL + grid.rule;
    let width = (table_right.max((margin + HEADING.chars().count() + 1) * CELL) + CELL) * grid.scale;
    let height = (table_bottom + CELL) * grid.scale;
    let mut page = GrayImage::from_pixel(width as u32, height as u32, Luma([255]));

    // Ink the rectangle `x0..=x1` by `y0..=y1`, in font pixels.
    let ink = |page: &mut GrayImage, x0: usize, x1: usize, y0: usize, y1: usize| {
        for y in y0 * grid.scale..(y1 + 1) * grid.scale {
            for x in x0 * grid.scale..(x1 + 1) * grid.scale {
                if (x as u32) < width as u32 && (y as u32) < height as u32 {
                    page.put_pixel(x as u32, y as u32, Luma([0]));
                }
            }
        }
    };
    // Draw `text` with its first glyph cell at the font pixel `(left, top)`.
    let draw = |page: &mut GrayImage, text: &str, left: usize, top: usize| {
        for (column, ch) in text.chars().enumerate() {
            let glyph = font8x8::legacy::BASIC_LEGACY[ch as usize];
            for (gy, bits) in glyph.iter().enumerate() {
                for gx in 0..CELL {
                    if bits & (1 << gx) == 0 {
                        continue;
                    }
                    let (at_x, at_y) = (left + column * CELL + gx, top + gy);
                    for dy in 0..grid.scale {
                        for dx in 0..grid.scale {
                            let (px, py) = (at_x * grid.scale + dx, at_y * grid.scale + dy);
                            if (px as u32) < width as u32 && (py as u32) < height as u32 {
                                page.put_pixel(px as u32, py as u32, Luma([0]));
                            }
                        }
                    }
                }
            }
        }
    };

    draw(&mut page, HEADING, margin * CELL, margin * CELL);
    if grid.rule > 0 {
        for edge in &edges {
            ink(
                &mut page,
                edge * CELL,
                edge * CELL + grid.rule - 1,
                table_top,
                table_bottom - 1,
            );
        }
        for row in 0..=rows.len() {
            let top = table_top + row * row_cells * CELL;
            ink(
                &mut page,
                table_left,
                table_right - 1,
                top,
                top + grid.rule - 1,
            );
        }
    }
    for (index, row) in rows.iter().enumerate() {
        let top = table_top + index * row_cells * CELL + grid.pad * CELL;
        for (column, text) in row.iter().enumerate() {
            draw(&mut page, text, (edges[column] + grid.pad) * CELL, top);
        }
    }
    page
}

/// The words of a page, in reading order: the heading's, then every row's.
fn drawn_words(rows: &[Vec<&str>]) -> Vec<String> {
    HEADING
        .split_whitespace()
        .chain(rows.iter().flat_map(|row| row.iter().copied()))
        .flat_map(|word| word.split_whitespace())
        .map(ToString::to_string)
        .collect()
}

/// The block of `blocks` with this id.
fn block<'a>(blocks: &'a [Value], id: &str) -> &'a Value {
    blocks
        .iter()
        .find(|block| block.get("Id").and_then(Value::as_str) == Some(id))
        .unwrap_or_else(|| panic!("the answer must hold the block {id} it names"))
}

/// The ids `block` names through its relationship of this kind, in order.
fn related(block: &Value, kind: &str) -> Vec<String> {
    block
        .get("Relationships")
        .and_then(Value::as_array)
        .map(|relationships| {
            relationships
                .iter()
                .filter(|relationship| {
                    relationship.get("Type").and_then(Value::as_str) == Some(kind)
                })
                .filter_map(|relationship| relationship.get("Ids").and_then(Value::as_array))
                .flatten()
                .filter_map(|id| id.as_str().map(ToString::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The text of the words `parent` names, joined with a space: what was read IN it.
fn words_of(blocks: &[Value], parent: &Value) -> String {
    related(parent, "CHILD")
        .iter()
        .map(|id| {
            block(blocks, id)
                .get("Text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The cells of the page's one table, with the words read in each.
fn cells_of<'a>(blocks: &'a [Value], case: &str) -> Vec<(usize, usize, String, &'a Value)> {
    let tables = of_type(blocks, "TABLE");
    assert_eq!(
        tables.len(),
        1,
        "case {case:?}: the page draws one table, got the lines {:?}",
        texts_of(blocks, "LINE")
    );
    related(tables[0], "CHILD")
        .iter()
        .map(|id| {
            let cell = block(blocks, id);
            let at = |name: &str| cell.get(name).and_then(Value::as_u64).unwrap_or(0) as usize;
            (at("RowIndex"), at("ColumnIndex"), words_of(blocks, cell), cell)
        })
        .collect()
}

/// Read `page` through `AnalyzeDocument` with both analysis features and hand back its
/// blocks.
async fn analyse(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    page: &GrayImage,
) -> Vec<Value> {
    let (status, body) = call(
        router,
        "AnalyzeDocument",
        &json!({
            "Document": raster_document(page, ImageFormat::Png),
            "FeatureTypes": ["TABLES", "FORMS"],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: a drawn page must be read, got {status} with {body}"
    );
    blocks_of(&body)
}

#[tokio::test]
async fn a_ruled_grid_is_read_as_one_cell_per_drawn_cell() {
    let router = harness();

    // (case, the rows as they are DRAWN, how the page is drawn)
    //
    // The rows differ along what the rule generalises over: the size the page is drawn at,
    // how thick its ruling lines are, how many rows and columns the grid holds, how many
    // words stand in a cell, and — as the control — the same grid with no ruling lines,
    // whose columns the gaps alone must find.
    let cases: Vec<(&str, Vec<Vec<&str>>, Grid)> = vec![
        (
            "a two-column grid at the font's own size",
            vec![vec!["Item", "Price"], vec!["Widget", "10.00"]],
            Grid { scale: 1, rule: 1, pad: 2 },
        ),
        (
            "the same grid drawn twice the size with two-pixel rules",
            vec![vec!["Item", "Price"], vec!["Widget", "10.00"]],
            Grid { scale: 2, rule: 2, pad: 2 },
        ),
        (
            "four times the size with three-pixel rules",
            vec![vec!["Item", "Price"], vec!["Widget", "10.00"]],
            Grid { scale: 4, rule: 3, pad: 2 },
        ),
        (
            "three columns, one of them two words",
            vec![
                vec!["Item", "Qty", "Unit Price"],
                vec!["Widget", "12", "1250.00"],
            ],
            Grid { scale: 2, rule: 2, pad: 2 },
        ),
        (
            "three rows of two columns, drawn with hairline rules",
            vec![
                vec!["Item", "Price"],
                vec!["Widget", "10.00"],
                vec!["Bolt", "2.50"],
            ],
            Grid { scale: 3, rule: 1, pad: 1 },
        ),
        (
            "no ruling lines at all, so the gaps alone divide the columns",
            vec![vec!["Item", "Price"], vec!["Widget", "10.00"]],
            Grid { scale: 2, rule: 0, pad: 2 },
        ),
    ];

    for (case, rows, grid) in cases {
        let page = ruled_page(&rows, grid);
        let blocks = analyse(&router, case, &page).await;

        // No ruling line is reported as a word: the words read are the words drawn, in the
        // order they were drawn, with nothing between them.
        assert_eq!(
            texts_of(&blocks, "WORD"),
            drawn_words(&rows),
            "case {case:?}: the words read must be the words drawn"
        );

        let cells = cells_of(&blocks, case);
        let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
        assert_eq!(
            cells.len(),
            columns * rows.len(),
            "case {case:?}: a grid of {} rows by {columns} columns must be read as that many \
             cells, got {:?}",
            rows.len(),
            cells
                .iter()
                .map(|(row, column, text, _)| (*row, *column, text.clone()))
                .collect::<Vec<_>>()
        );

        // Every cell reports where it stands, 1-based, and parents only the words drawn in
        // it: `Block$RowIndex` is "the row in which a table cell is located. The first row
        // position is 1", and `Block$ColumnIndex` the same for its column.
        let mut seen: Vec<(usize, usize)> = Vec::new();
        for (row, column, read, cell) in &cells {
            assert!(
                (1..=rows.len()).contains(row) && (1..=columns).contains(column),
                "case {case:?}: the cell {cell} is at {row}/{column}, outside the 1-based \
                 {}x{columns} grid",
                rows.len()
            );
            assert!(
                !seen.contains(&(*row, *column)),
                "case {case:?}: two cells claim row {row} column {column}"
            );
            seen.push((*row, *column));
            let drawn = rows[row - 1].get(column - 1).copied().unwrap_or_default();
            assert_eq!(
                read, drawn,
                "case {case:?}: the cell at row {row} column {column} must parent the words \
                 drawn in it"
            );
        }
    }
}

#[tokio::test]
async fn a_cell_left_empty_is_read_as_a_cell_with_no_words() {
    let router = harness();

    // A ruling line divides the cells of a row whatever stands inside them, so a row whose
    // middle cell was left empty is read as three cells, the middle one parenting no words;
    // and a cell at either END of a row is bounded by a ruling line and the table's own
    // border, so an empty first or last cell is a cell of its own too. The rows differ along
    // WHERE the empty cell stands, which row holds it, and the size the page is drawn at.
    let cases: Vec<(&str, Vec<Row<'_>>, &[At], Grid)> = vec![
        (
            "an empty cell in the middle of the second row",
            vec![vec!["Item", "Qty", "Price"], vec!["Widget", "", "10.00"]],
            &[(2, 2)],
            Grid { scale: 2, rule: 2, pad: 2 },
        ),
        (
            "an empty cell in each row, drawn larger",
            vec![vec!["Item", "", "Price"], vec!["Widget", "", "10.00"]],
            &[(1, 2), (2, 2)],
            Grid { scale: 3, rule: 2, pad: 1 },
        ),
        (
            "an empty cell at the start of the second row",
            vec![vec!["Item", "Qty", "Price"], vec!["", "12", "10.00"]],
            &[(2, 1)],
            Grid { scale: 2, rule: 2, pad: 2 },
        ),
        (
            "an empty cell at the end of the second row",
            vec![vec!["Item", "Qty", "Price"], vec!["Widget", "12", ""]],
            &[(2, 3)],
            Grid { scale: 2, rule: 2, pad: 2 },
        ),
    ];

    for (case, rows, empty, grid) in cases {
        let page = ruled_page(&rows, grid);
        let blocks = analyse(&router, case, &page).await;
        let cells = cells_of(&blocks, case);
        let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
        assert_eq!(
            cells.len(),
            columns * rows.len(),
            "case {case:?}: every drawn cell is a cell, the empty one included, got {:?}",
            cells
                .iter()
                .map(|(row, column, text, _)| (*row, *column, text.clone()))
                .collect::<Vec<_>>()
        );
        for (row, column, read, cell) in &cells {
            let drawn = rows[row - 1].get(column - 1).copied().unwrap_or_default();
            assert_eq!(
                read, drawn,
                "case {case:?}: the cell at row {row} column {column} must parent the words \
                 drawn in it"
            );
            if empty.contains(&(*row, *column)) {
                assert!(
                    related(cell, "CHILD").is_empty(),
                    "case {case:?}: the cell at row {row} column {column} holds no words, so \
                     it names no children, got {cell}"
                );
            }
        }
    }
}

#[tokio::test]
async fn a_grid_whose_cells_are_empty_is_content_but_no_text() {
    let router = harness();

    // The other half of the rule: the ruling lines of a grid whose cells were left empty are
    // no text at all — the page's only words are the heading's — while the ink is still
    // CONTENT of the page, which the `PAGE` block's own geometry bounds.
    let cases: Vec<(&str, Grid)> = vec![
        ("a grid of hairline rules", Grid { scale: 2, rule: 1, pad: 2 }),
        ("a grid of three-pixel rules", Grid { scale: 2, rule: 3, pad: 2 }),
    ];

    for (case, grid) in cases {
        let rows = vec![vec!["", ""], vec!["", ""]];
        let page = ruled_page(&rows, grid);
        let blocks = analyse(&router, case, &page).await;
        assert_eq!(
            texts_of(&blocks, "WORD"),
            drawn_words(&rows),
            "case {case:?}: the only words on the page are the heading's"
        );
        assert!(
            of_type(&blocks, "TABLE").is_empty() && of_type(&blocks, "CELL").is_empty(),
            "case {case:?}: a grid whose cells hold no words has no row of cells to report, \
             got {:?}",
            texts_of(&blocks, "LINE")
        );
        let pages = of_type(&blocks, "PAGE");
        assert_eq!(
            pages.len(),
            1,
            "case {case:?}: a page that was read reports one PAGE block, got {blocks:?}"
        );
        let bbox = pages[0]
            .get("Geometry")
            .and_then(|geometry| geometry.get("BoundingBox"))
            .and_then(Value::as_object)
            .unwrap_or_else(|| panic!("case {case:?}: a PAGE must carry a BoundingBox"));
        let number = |name: &str| bbox.get(name).and_then(Value::as_f64).unwrap_or(-1.0);
        // The grid is drawn below the heading and inside the page's margins, and the PAGE
        // spans every mark found on the page, the ruling lines included: so its extent
        // reaches past the heading's own rows and stays on the page.
        let heading_rows = f64::from((3 * CELL * grid.scale) as u32) / f64::from(page.height());
        assert!(
            number("Top") > 0.0 && number("Height") > heading_rows,
            "case {case:?}: the PAGE must span the ink found on it, the grid included, got \
             {bbox:?}"
        );
        assert!(
            number("Top") + number("Height") <= 1.0 + 1e-6
                && number("Left") + number("Width") <= 1.0 + 1e-6,
            "case {case:?}: the PAGE's extent must stay on the page, got {bbox:?}"
        );
    }
}
