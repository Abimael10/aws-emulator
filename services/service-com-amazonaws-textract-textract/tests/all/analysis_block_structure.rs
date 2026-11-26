// SPDX-License-Identifier: CC0-1.0

//! End-to-end coverage of the rule that the analysis-only blocks `AnalyzeDocument` adds
//! are CLASSIFIED from the structure detected on the submitted page.
//!
//! The `Block$EntityTypes` docs state which entity types each analysis returns: `KEY` and
//! `VALUE` "can be returned by FORMS analysis", while `COLUMN_HEADER` ("identifies a cell
//! that is a header of a column"), `STRUCTURED_TABLE` ("identifies a table with column
//! headers where the content of each row corresponds to the headers") and
//! `SEMI_STRUCTURED_TABLE` ("identifies a non-structured table") are returned by TABLES
//! analysis. So the rule under test, over a TABLE of documents rather than one recorded
//! page: for every table detected on a page, the cells of the row that heads the columns
//! beneath it are that table's column headers, and the `TABLE` block says whether those
//! headers and its rows make it a structured table or not — and a page whose rows head
//! nothing reports no column header at all.
//!
//! The rows differ along what the rule generalises over: whether the first row heads the
//! columns beneath it, whether anything on the page carries data for a header to stand
//! against, how many columns and rows the table holds, whether its rows line up with its
//! first one, and how many tables the one page carries.
//!
//! Every page here is DRAWN from known text with the same font the recogniser holds
//! templates for (`crate::document_extraction::text_document`), so each case first asserts
//! the round trip — the rows that come back are the rows that went in — and then asserts
//! the classification against the structure that was drawn.

use serde_json::{json, Value};
use service_com_amazonaws_textract_textract::ops::types::ENTITY_TYPE_VALUES;

use crate::document_extraction::{
    block_id, block_text, block_type, blocks_of, call, child_ids, find, harness, of_type,
    text_document,
};

/// The `EntityTypes` a block reports, in the order it reports them.
fn entity_types(block: &Value) -> Vec<String> {
    block
        .get("EntityTypes")
        .and_then(Value::as_array)
        .map(|types| {
            types
                .iter()
                .map(|entity| entity.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// The text read IN a block: the words it parents, joined with a space. A `CELL` carries
/// no `Text` of its own — what stands in it is the words it is the parent of.
fn words_in(blocks: &[Value], parent: &Value) -> String {
    child_ids(parent)
        .iter()
        .map(|id| {
            find(blocks, id)
                .map(block_text)
                .unwrap_or_default()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The `LINE` texts of an answer, in the order they came back.
fn rows_read(blocks: &[Value]) -> Vec<String> {
    of_type(blocks, "LINE")
        .iter()
        .map(|line| block_text(line).to_string())
        .collect()
}

/// The cells of one table, in the order the table names them.
fn cells_of<'a>(blocks: &'a [Value], table: &Value) -> Vec<&'a Value> {
    child_ids(table)
        .iter()
        .filter_map(|id| find(blocks, id))
        .collect()
}

/// What one table of a case's page must come back as.
struct Table {
    /// The one `EntityTypes` entry the `TABLE` block must report.
    kind: &'static str,
    /// The texts of the cells that must carry `COLUMN_HEADER`, left to right; empty when
    /// the table heads no columns at all.
    headers: &'static [&'static str],
}

/// One document of the table-classification table.
struct TableCase {
    name: &'static str,
    /// The page's text, one line per line of ink, tabs dividing the columns of a row.
    text: &'static str,
    /// The tables the page holds, in page order.
    tables: &'static [Table],
}

#[tokio::test]
async fn tables_analysis_classifies_each_table_from_the_rows_detected_on_the_page() {
    let router = harness();

    let cases: &[TableCase] = &[
        TableCase {
            name: "a three-column table whose first row names what stands under it",
            text: "Item Code\tQuantity Sent\tAmount Due\n\
                   Widget Mk2\t3 Crates\t12.00 USD\n\
                   Gadget Mk1\t1 Crate\t99.95 USD",
            tables: &[Table {
                kind: "STRUCTURED_TABLE",
                headers: &["Item Code", "Quantity Sent", "Amount Due"],
            }],
        },
        TableCase {
            name: "the same table two columns wide",
            text: "Item Code\tAmount Due\n\
                   Widget Mk2\t12.00 USD\n\
                   Gadget Mk1\t99.95 USD",
            tables: &[Table {
                kind: "STRUCTURED_TABLE",
                headers: &["Item Code", "Amount Due"],
            }],
        },
        TableCase {
            // The first row carries data of its own, so it heads nothing: there is no row
            // of names for the rows beneath to correspond to.
            name: "a table whose first row carries data of its own",
            text: "Widget Mk2\t3 Crates\t12.00 USD\n\
                   Gadget Mk1\t1 Crate\t99.95 USD",
            tables: &[Table {
                kind: "SEMI_STRUCTURED_TABLE",
                headers: &[],
            }],
        },
        TableCase {
            // Nothing on this page carries data, so its first row stands against nothing.
            name: "a table of nothing but names",
            text: "Item Code\tColour Name\n\
                   Widget Standard\tDeep Blue\n\
                   Gadget Deluxe\tPale Green",
            tables: &[Table {
                kind: "SEMI_STRUCTURED_TABLE",
                headers: &[],
            }],
        },
        TableCase {
            // Headed, but the row beneath does not break into the columns the header
            // names, so the content of each row does NOT correspond to the headers.
            name: "a headed table whose rows do not line up with it",
            text: "Item Code\tQuantity Sent\tAmount Due\n\
                   Widget Mk2\t12.00 USD",
            tables: &[Table {
                kind: "SEMI_STRUCTURED_TABLE",
                headers: &["Item Code", "Quantity Sent", "Amount Due"],
            }],
        },
        TableCase {
            // One row alone: nothing stands under it, so it heads nothing.
            name: "a table of one row",
            text: "Widget Mk2\t3 Crates\t12.00 USD",
            tables: &[Table {
                kind: "SEMI_STRUCTURED_TABLE",
                headers: &[],
            }],
        },
        TableCase {
            // Two runs of rows on one page, divided by a line that is no row at all: each
            // is classified from ITS OWN rows.
            name: "two tables on one page",
            text: "Item Code\tAmount Due\n\
                   Widget Mk2\t12.00 USD\n\
                   Summary follows\n\
                   Colour Code\tColour Name\n\
                   Widget Standard\tDeep Blue",
            tables: &[
                Table {
                    kind: "STRUCTURED_TABLE",
                    headers: &["Item Code", "Amount Due"],
                },
                Table {
                    kind: "SEMI_STRUCTURED_TABLE",
                    headers: &[],
                },
            ],
        },
    ];

    for case in cases {
        let (status, body) = call(
            &router,
            "AnalyzeDocument",
            &json!({
                "Document": text_document(case.text),
                "FeatureTypes": ["TABLES"],
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected HTTP 200, got {status} with {body}",
            case.name
        );
        let blocks = blocks_of(&body);

        // The round trip first: the rows classified below are the rows that were drawn.
        let drawn: Vec<String> = case.text.lines().map(str::to_string).collect();
        assert_eq!(
            rows_read(&blocks),
            drawn,
            "case {:?}: the rows read must be the rows drawn",
            case.name
        );

        // Nothing invents an entity type the model does not carry (`ENTITY_TYPE_VALUES`
        // is the model's own EntityType enum).
        for block in &blocks {
            for entity in entity_types(block) {
                assert!(
                    ENTITY_TYPE_VALUES.contains(&entity.as_str()),
                    "case {:?}: block {} reports {entity:?}, which is not a modelled \
                     EntityType",
                    case.name,
                    block_id(block)
                );
            }
        }

        let tables = of_type(&blocks, "TABLE");
        assert_eq!(
            tables.len(),
            case.tables.len(),
            "case {:?}: expected {} TABLE blocks, got {}",
            case.name,
            case.tables.len(),
            tables.len()
        );

        for (index, (table, expected)) in tables.iter().zip(case.tables).enumerate() {
            assert_eq!(
                entity_types(table),
                vec![expected.kind.to_string()],
                "case {:?}: table {index} must report the one entity type {:?}, got {:?}",
                case.name,
                expected.kind,
                entity_types(table)
            );

            let cells = cells_of(&blocks, table);
            assert!(
                !cells.is_empty(),
                "case {:?}: table {index} must name the cells it was read from, got {table}",
                case.name
            );
            for cell in &cells {
                assert_eq!(
                    block_type(cell),
                    "CELL",
                    "case {:?}: a TABLE's children are CELL blocks, got {:?}",
                    case.name,
                    block_type(cell)
                );
            }

            // The cells that carry COLUMN_HEADER are exactly the cells of the row that
            // heads the columns, and they carry the text that was drawn in them.
            let headers: Vec<&&Value> = cells
                .iter()
                .filter(|cell| entity_types(cell) == vec!["COLUMN_HEADER".to_string()])
                .collect();
            let read: Vec<String> = headers
                .iter()
                .map(|cell| words_in(&blocks, cell))
                .collect();
            assert_eq!(
                read,
                expected
                    .headers
                    .iter()
                    .map(|text| text.to_string())
                    .collect::<Vec<_>>(),
                "case {:?}: table {index} must report its column headers, got {read:?}",
                case.name
            );
            for cell in &headers {
                assert_eq!(
                    cell.get("RowIndex").and_then(Value::as_u64),
                    Some(1),
                    "case {:?}: a column header stands in the table's first row, got {cell}",
                    case.name
                );
            }
            // Every other cell of the table is data, and carries no entity type at all.
            for cell in &cells {
                if entity_types(cell).is_empty() {
                    continue;
                }
                assert_eq!(
                    entity_types(cell),
                    vec!["COLUMN_HEADER".to_string()],
                    "case {:?}: cell {} is no column header, so it reports no entity type, \
                     got {:?}",
                    case.name,
                    block_id(cell),
                    entity_types(cell)
                );
            }
        }
    }
}

/// One document of the forms-classification table: the page's text and the label/value
/// pairs it sets out, in page order.
struct FormCase {
    name: &'static str,
    text: &'static str,
    pairs: &'static [(&'static str, &'static str)],
}

#[tokio::test]
async fn forms_analysis_classifies_each_pair_from_the_lines_detected_on_the_page() {
    let router = harness();

    // The rows differ along how many fields the page carries, how the label is divided
    // from the value, and how many words the value is made of.
    let cases: &[FormCase] = &[
        FormCase {
            name: "one punctuated field",
            text: "Invoice Number: INV-7781",
            pairs: &[("Invoice Number:", "INV-7781")],
        },
        FormCase {
            name: "two punctuated fields, the second a value of two words",
            text: "Invoice Number: INV-7781\nCustomer Name: Acme Corporation",
            pairs: &[
                ("Invoice Number:", "INV-7781"),
                ("Customer Name:", "Acme Corporation"),
            ],
        },
        FormCase {
            name: "a line that labels nothing carries no pair",
            text: "Statement of Account",
            pairs: &[],
        },
    ];

    for FormCase { name: case, text, pairs } in cases {
        let (status, body) = call(
            &router,
            "AnalyzeDocument",
            &json!({
                "Document": text_document(text),
                "FeatureTypes": ["FORMS"],
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {body}"
        );
        let blocks = blocks_of(&body);
        assert_eq!(
            rows_read(&blocks),
            text.lines().map(str::to_string).collect::<Vec<_>>(),
            "case {case:?}: the lines read must be the lines drawn"
        );

        // A FORMS analysis reports the KEY and the VALUE halves and nothing else: the
        // table entity types belong to a TABLES analysis.
        let sets = of_type(&blocks, "KEY_VALUE_SET");
        assert_eq!(
            sets.len(),
            pairs.len() * 2,
            "case {case:?}: expected a KEY and a VALUE block for each of {} fields, got {}",
            pairs.len(),
            sets.len()
        );

        let keys: Vec<&&Value> = sets
            .iter()
            .filter(|set| entity_types(set) == vec!["KEY".to_string()])
            .collect();
        assert_eq!(
            keys.len(),
            pairs.len(),
            "case {case:?}: expected {} KEY blocks, got {}",
            pairs.len(),
            keys.len()
        );

        for (key, (label, value)) in keys.iter().zip(*pairs) {
            assert_eq!(
                words_in(&blocks, key),
                *label,
                "case {case:?}: a KEY parents the words that label the field"
            );
            let linked = crate::document_extraction::related_ids(key, "VALUE");
            assert_eq!(
                linked.len(),
                1,
                "case {case:?}: a KEY links exactly one VALUE, got {linked:?}"
            );
            let half = find(&blocks, &linked[0]).unwrap_or_else(|| {
                panic!("case {case:?}: the VALUE {} is not in the answer", linked[0])
            });
            assert_eq!(
                block_type(half),
                "KEY_VALUE_SET",
                "case {case:?}: the other half of a pair is a KEY_VALUE_SET, got {:?}",
                block_type(half)
            );
            assert_eq!(
                entity_types(half),
                vec!["VALUE".to_string()],
                "case {case:?}: the linked half is the VALUE, got {:?}",
                entity_types(half)
            );
            assert_eq!(
                words_in(&blocks, half),
                *value,
                "case {case:?}: a VALUE parents the words of the value it carries"
            );
        }
    }
}
