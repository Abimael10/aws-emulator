// SPDX-License-Identifier: CC0-1.0

//! What a labelled line's VALUE is, end to end: the span of the page a `QUERIES` answer,
//! a `FORMS` VALUE block and an `AnalyzeExpense` summary field all report.
//!
//! The rule under test, over a TABLE of lines rather than one recorded page: the value a
//! labelled line carries is the COMPLETE run of characters from where that value begins to
//! the end of the line — every character of it, in order — and never a part of it; where the
//! page sets no separator at all, the value begins at the line's FIRST token that carries a
//! figure, so the tokens before it label it and every one from it on is the value. A line
//! whose own first token carries the figures labels nothing: it IS the figures.
//!
//! The rows differ along what the rule generalises over: how the page separates the label
//! from the value (a colon, a colon with the value in the next column, the next column
//! alone, no punctuation at all), how many tokens the value is written in, what the value is
//! made of (figures, an identifier of letters and figures, an amount with its currency sign,
//! a date, an amount with a currency code beside it), where in the line the figures begin,
//! and whether the page was submitted as characters or RECOGNISED in a raster's pixels.
//!
//! Each row is asserted through both read paths that derive a field from a line — the
//! `QUERY_RESULT` a query is answered with and the `KEY_VALUE_SET` pair `FORMS` reports — so
//! the one rule is pinned wherever it holds rather than on one operation.

use image::ImageFormat;
use serde_json::{json, Value};

use crate::document_extraction::{
    block_id, block_type, blocks_of, call, harness, of_type, text_document,
};
use crate::raster_documents::{raster_document, render, Paper};

/// One row of the table: a line a page carries, and the field it holds.
struct Case {
    name: &'static str,
    /// The line drawn on the page. A tab is the wide gap a page sets its second COLUMN at.
    line: &'static str,
    /// The words that label the value, as the page writes them — the separator a label is
    /// punctuated with is part of the word it is drawn on.
    label_words: &'static str,
    /// The value that label stands against, or `None` where the line labels nothing.
    value: Option<&'static str>,
    /// What a client asks to read that value.
    query: &'static str,
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "a colon between the label and an identifier",
            line: "Invoice Number: INV-99887",
            label_words: "Invoice Number:",
            value: Some("INV-99887"),
            query: "What is the invoice number?",
        },
        Case {
            // The defect this suite was written for: the value is the WHOLE identifier,
            // not the figures at the end of it.
            name: "no punctuation at all, an identifier of letters and figures",
            line: "Invoice Number INV-99887",
            label_words: "Invoice Number",
            value: Some("INV-99887"),
            query: "What is the invoice number?",
        },
        Case {
            name: "no punctuation, a value written in several tokens",
            line: "Amount Due 1 250.00 USD",
            label_words: "Amount Due",
            value: Some("1 250.00 USD"),
            query: "What is the amount due?",
        },
        Case {
            name: "no punctuation, a value with its currency sign",
            line: "Total $12.34",
            label_words: "Total",
            value: Some("$12.34"),
            query: "What is the total?",
        },
        Case {
            name: "no punctuation, a date",
            line: "Invoice Date 01/02/2024",
            label_words: "Invoice Date",
            value: Some("01/02/2024"),
            query: "What is the invoice date?",
        },
        Case {
            name: "a colon, with the value in the next column",
            line: "Invoice Number:\tINV-99887",
            label_words: "Invoice Number:",
            value: Some("INV-99887"),
            query: "What is the invoice number?",
        },
        Case {
            name: "the label in one column and the value in the next",
            line: "Invoice Number\tINV-99887",
            label_words: "Invoice Number",
            value: Some("INV-99887"),
            query: "What is the invoice number?",
        },
        Case {
            name: "a colon, with a value of several words of letters",
            line: "Vendor Name: Acme Supplies Ltd",
            label_words: "Vendor Name:",
            value: Some("Acme Supplies Ltd"),
            query: "What is the vendor name?",
        },
        Case {
            // The figures open the line, so there is nothing in front of them to label
            // them: the line is a heading, not a field.
            name: "a line whose own first token carries the figures labels nothing",
            line: "SIM2REAL LEDGER",
            label_words: "",
            value: None,
            query: "What is the ledger?",
        },
        Case {
            name: "a line of nothing but letters labels nothing",
            line: "THANK YOU FOR YOUR BUSINESS",
            label_words: "",
            value: None,
            query: "What is the business?",
        },
    ]
}

/// The ids a block relates to by `kind`.
fn related(block: &Value, kind: &str) -> Vec<String> {
    block
        .get("Relationships")
        .and_then(Value::as_array)
        .map(|all| {
            all.iter()
                .filter(|it| it.get("Type").and_then(Value::as_str) == Some(kind))
                .filter_map(|it| it.get("Ids").and_then(Value::as_array))
                .flatten()
                .filter_map(|id| id.as_str().map(ToString::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The text of the `WORD` blocks `ids` names, in the page's own order, joined by a space:
/// the characters of the page those blocks were read from.
fn words_of(blocks: &[Value], ids: &[String]) -> String {
    of_type(blocks, "WORD")
        .iter()
        .filter(|word| ids.iter().any(|id| id == block_id(word)))
        .map(|word| word.get("Text").and_then(Value::as_str).unwrap_or_default())
        .collect::<Vec<&str>>()
        .join(" ")
}

/// Whether a `KEY_VALUE_SET` block is the KEY or the VALUE half of a pair.
fn entity(block: &Value, kind: &str) -> bool {
    block
        .get("EntityTypes")
        .and_then(Value::as_array)
        .is_some_and(|types| types.iter().any(|it| it.as_str() == Some(kind)))
}

/// The `QUERY_RESULT` texts an answered query resolves to, in answer order.
fn answers(blocks: &[Value]) -> Vec<String> {
    let queries = of_type(blocks, "QUERY");
    let ids: Vec<String> = queries
        .iter()
        .flat_map(|query| related(query, "ANSWER"))
        .collect();
    of_type(blocks, "QUERY_RESULT")
        .iter()
        .filter(|result| ids.iter().any(|id| id == block_id(result)))
        .map(|result| {
            result
                .get("Text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// A page carrying one labelled line answers a query about it with the WHOLE value that
/// line holds, and reports the same span as the `FORMS` value of the same label.
#[tokio::test]
async fn a_labelled_line_reports_the_whole_value_it_holds() {
    let router = harness();
    for case in cases() {
        let (status, body) = call(
            &router,
            "AnalyzeDocument",
            &json!({
                "Document": text_document(case.line),
                "FeatureTypes": ["QUERIES", "FORMS"],
                "QueriesConfig": { "Queries": [{ "Text": case.query }] },
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected HTTP 200, got {status} with {body}",
            case.name
        );
        let blocks = blocks_of(&body);
        let answered = answers(&blocks);

        match case.value {
            Some(value) => {
                assert_eq!(
                    answered,
                    vec![value.to_string()],
                    "case {:?}: the answer to {:?} is the whole value the line holds ({:?}), \
                     got {answered:?}",
                    case.name,
                    case.query,
                    value
                );
                // The same line read as a FORMS pair: the KEY's words are the label's and
                // the VALUE's are every word of the value, so both derivations report the
                // one span.
                let pairs = of_type(&blocks, "KEY_VALUE_SET");
                let keys: Vec<&&Value> = pairs.iter().filter(|it| entity(it, "KEY")).collect();
                assert_eq!(
                    keys.len(),
                    1,
                    "case {:?}: one labelled line is one KEY block, got {}",
                    case.name,
                    keys.len()
                );
                let key = keys[0];
                assert_eq!(
                    words_of(&blocks, &related(key, "CHILD")),
                    case.label_words,
                    "case {:?}: the KEY's own words are the label's ({:?})",
                    case.name,
                    case.label_words
                );
                let valued = related(key, "VALUE");
                let value_block = pairs
                    .iter()
                    .find(|block| valued.iter().any(|id| id == block_id(block)))
                    .unwrap_or_else(|| {
                        panic!(
                            "case {:?}: the KEY's VALUE relationship must resolve to a block",
                            case.name
                        )
                    });
                assert!(
                    entity(value_block, "VALUE"),
                    "case {:?}: the block a KEY names by VALUE is the VALUE half, got \
                     {value_block}",
                    case.name
                );
                assert_eq!(
                    words_of(&blocks, &related(value_block, "CHILD")),
                    value.split_whitespace().collect::<Vec<&str>>().join(" "),
                    "case {:?}: the VALUE's own words are every word of {:?}",
                    case.name,
                    value
                );
            }
            None => {
                assert!(
                    answered.is_empty(),
                    "case {:?}: the line labels nothing, so the query must be answered with \
                     no QUERY_RESULT, got {answered:?}",
                    case.name
                );
                assert!(
                    of_type(&blocks, "KEY_VALUE_SET").is_empty(),
                    "case {:?}: the line labels nothing, so no KEY_VALUE_SET is derived, got \
                     {:?}",
                    case.name,
                    of_type(&blocks, "KEY_VALUE_SET")
                        .iter()
                        .map(|block| block.get("Text"))
                        .collect::<Vec<Option<&Value>>>()
                );
            }
        }
    }
}

/// The same rule over RECOGNISED pixels: a page that draws an unpunctuated labelled line
/// answers with the whole value drawn on it, leading characters included.
#[tokio::test]
async fn a_value_recognised_in_a_raster_comes_back_whole() {
    let router = harness();
    let drawn = ["Invoice Number INV-99887", "Total $12.34"];
    // Drawn at the font's own size: the smallest page that carries these two lines, which
    // is what keeps the recognition this row costs down.
    let page = render(
        &drawn,
        Paper {
            scale: 1,
            ..Paper::plain()
        },
    );
    let (status, body) = call(
        &router,
        "AnalyzeDocument",
        &json!({
            "Document": raster_document(&page, ImageFormat::Png),
            "FeatureTypes": ["QUERIES"],
            "QueriesConfig": {
                "Queries": [
                    { "Text": "What is the invoice number?", "Alias": "INVOICE_NUMBER" },
                    { "Text": "What is the total?", "Alias": "TOTAL" },
                ]
            },
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "a readable page must be analysed, got {status} with {body}"
    );
    let blocks = blocks_of(&body);
    let answered = answers(&blocks);
    assert_eq!(
        answered,
        vec!["INV-99887".to_string(), "$12.34".to_string()],
        "the answers are the whole values the page draws ({drawn:?}), got {answered:?}"
    );
    // Every answer is a QUERY_RESULT with a measured confidence and geometry: the value is
    // reported as the span of the page it was read from.
    for result in of_type(&blocks, "QUERY_RESULT") {
        assert_eq!(
            block_type(result),
            "QUERY_RESULT",
            "the ANSWER of a query is a QUERY_RESULT, got {result}"
        );
        let confidence = result
            .get("Confidence")
            .and_then(Value::as_f64)
            .unwrap_or_default();
        assert!(
            confidence > 0.0 && confidence <= 100.0,
            "an answer reports the confidence its characters were read with, got \
             {confidence} on {result}"
        );
        assert!(
            result.get("Geometry").is_some(),
            "an answer reports where on the page its value sits, got {result}"
        );
    }
}
