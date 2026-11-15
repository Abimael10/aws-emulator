// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for how `TestMapping` CARRIES the mapped file, driven over the wire
//! (awsJson1_0, `x-amz-target: B2BI.TestMapping`) through the same router the emulator
//! boots.
//!
//! Two rules are under test here, both about the layer that carries a mapped document
//! rather than about what the mapping computes (which `tests/all/test_mapping.rs` asserts):
//!
//! * `mappedFileContent` is a `String`, and what it holds is the mapped file's TEXT as a
//!   JSON string literal — so a client parses the member once for that text and parses the
//!   text for the mapped document. The encoding is a property of the MEMBER, not of the
//!   result's JSON type: a mapping whose result is a scalar is recovered by the same two
//!   steps as one whose result is an object, which is what makes the member readable at all
//!   for a projecting mapping.
//! * an object of the mapped document carries its members in the order the document the
//!   mapping READ writes them — never sorted, which a client diffing the mapped output
//!   against an expected document would see as differences the mapping did not make. A
//!   member the input document does not name follows the ones it does, so one request
//!   always answers with one text.
//!
//! The rows differ along what the rules generalise over: the JSON type of the mapped
//! result (object, array, string, number, boolean, null), whether the mapped text needs
//! escaping (quotes, newlines, a backslash), the member order of the input document, how
//! deeply the members nest, and whether a mapped member comes from the input at all.

use serde_json::{json, Value};

use crate::onboarding_loop::{b2bi_router, call};

/// One row: the case, the input document, the template, and the TEXT the mapped file must
/// hold (what one parse of the member yields).
type EncodingCase<'a> = (&'a str, &'a str, &'a str, &'a str);

/// Invoke `TestMapping` and return the raw `mappedFileContent` member.
async fn mapped_member(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    input: &str,
    template: &str,
) -> String {
    let (status, body) = call(
        router,
        "TestMapping",
        json!({
            "fileFormat": "JSON",
            "inputFileContent": input,
            "mappingTemplate": template,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the mapping must be served, got {status} {body}"
    );
    body["mappedFileContent"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: mappedFileContent must be a string, got {body}"))
        .to_string()
}

/// The mapped file's text: one parse of the member, the way a client reads it.
fn mapped_text(case: &str, member: &str) -> String {
    serde_json::from_str::<String>(member).unwrap_or_else(|failure| {
        panic!(
            "case {case:?}: one parse of mappedFileContent {member:?} must yield the mapped \
             file's text: {failure}"
        )
    })
}

/// RULE: `mappedFileContent` holds the mapped file's text JSON-string-encoded, whatever
/// the mapped result's JSON type.
#[tokio::test]
async fn mapped_file_content_holds_the_mapped_text_json_string_encoded() {
    let router = b2bi_router();

    let cases: Vec<EncodingCase> = vec![
        (
            "an object result",
            r#"{"orderId":"PO-1","amount":2}"#,
            "$",
            "{\n  \"orderId\": \"PO-1\",\n  \"amount\": 2\n}",
        ),
        (
            "an array result",
            r#"{"items":[1,2]}"#,
            "items",
            "[\n  1,\n  2\n]",
        ),
        (
            "a string result, the projecting mapping a caller validates with",
            r#"{"orderId":"PO-10042"}"#,
            "orderId",
            "\"PO-10042\"",
        ),
        (
            "a number result",
            r#"{"items":[1,2,3]}"#,
            "$sum(items)",
            "6",
        ),
        (
            "a boolean result",
            r#"{"amount":40}"#,
            "amount > 10",
            "true",
        ),
        (
            "a null member",
            r#"{"note":null}"#,
            "{\"note\": note}",
            "{\n  \"note\": null\n}",
        ),
        (
            "a string carrying the characters JSON escapes",
            r#"{"note":"a \"quoted\" line\nand a \\ backslash"}"#,
            "note",
            "\"a \\\"quoted\\\" line\\nand a \\\\ backslash\"",
        ),
        (
            "an object whose member value needs escaping",
            r#"{"note":"line\none"}"#,
            "$",
            "{\n  \"note\": \"line\\none\"\n}",
        ),
    ];

    for (case, input, template, expected_text) in cases {
        let member = mapped_member(&router, case, input, template).await;

        // The member is a JSON string literal: ONE parse yields the mapped file's text,
        // never the document itself.
        let parsed: Value = serde_json::from_str(&member).unwrap_or_else(|failure| {
            panic!("case {case:?}: mappedFileContent {member:?} must be JSON: {failure}")
        });
        let text = parsed.as_str().unwrap_or_else(|| {
            panic!(
                "case {case:?}: one parse of mappedFileContent must yield the mapped file's \
                 text, got {parsed}"
            )
        });
        assert_eq!(
            text, expected_text,
            "case {case:?}: expected the mapped text {expected_text:?}, got {text:?}"
        );

        // …and parsing that text yields the mapped document, which is what the caller
        // reads the member for.
        let document: Value = serde_json::from_str(text).unwrap_or_else(|failure| {
            panic!("case {case:?}: the mapped text {text:?} must be the document: {failure}")
        });
        let round_tripped = serde_json::to_string(&document).unwrap_or_default();
        assert!(
            !round_tripped.is_empty(),
            "case {case:?}: the mapped document must be a JSON value, got {document}"
        );
    }
}

/// RULE (the same member, read as a client reads it): the two parses are what recover the
/// mapped document, and the identity template recovers the INPUT document — for a document
/// of any shape, member for member AND in the order that document writes its members.
///
/// The rows differ along the document's shape (object, array, nested, scalar members), the
/// order its members are written in and the characters its values carry.
#[tokio::test]
async fn two_parses_recover_the_input_document_for_an_identity_mapping() {
    let router = b2bi_router();

    let documents: &[(&str, &str)] = &[
        ("a flat object", r#"{"a":1,"b":"two","c":false,"d":null}"#),
        ("an array of objects", r#"[{"a":1},{"a":2}]"#),
        (
            "a nested document",
            r#"{"outer":{"inner":[1,{"deep":"value"}]}}"#,
        ),
        ("an empty object", "{}"),
        ("an empty array", "[]"),
        (
            "values carrying quotes and newlines",
            r#"{"note":"a \"quoted\" line\nand more"}"#,
        ),
        (
            "members written in reverse alphabetical order",
            r#"{"zulu":1,"mike":2,"alpha":3}"#,
        ),
        (
            "a nested document whose members are written out of order",
            r#"{"third":{"inner":3,"before":2},"first":1}"#,
        ),
    ];

    for (case, input) in documents {
        let member = mapped_member(&router, case, input, "$").await;
        let text = mapped_text(case, &member);
        let mapped: Value = serde_json::from_str(&text)
            .unwrap_or_else(|failure| panic!("case {case:?}: {text:?} must be JSON: {failure}"));
        let expected: Value = serde_json::from_str(input)
            .unwrap_or_else(|failure| panic!("case {case:?}: the row's own document: {failure}"));
        assert_eq!(
            mapped, expected,
            "case {case:?}: the identity mapping must recover the input document, got {mapped}"
        );

        // The ORDER too: the names in the mapped text appear in the order the input wrote
        // them, which a sorted answer would not do.
        assert_eq!(
            member_names(&text),
            member_names(input),
            "case {case:?}: the mapped text {text:?} must keep the member order of {input:?}"
        );
    }
}

/// RULE: a mapped object's members are written in the order the INPUT DOCUMENT writes them,
/// whatever order the template or the interpreter holds them in, and a member the input
/// does not name follows the ones it does — so the same request always answers with the
/// same text.
#[tokio::test]
async fn a_mapped_objects_members_follow_the_input_documents_order() {
    let router = b2bi_router();

    // Each row: the case, the input document, the template, and the member names the mapped
    // text must carry, in order.
    let cases: &[(&str, &str, &str, &[&str])] = &[
        (
            "every member comes from the input, the template naming them in another order",
            r#"{"orderId":"12345","item":"widget","quantity":3}"#,
            r#"{"quantity": quantity, "orderId": orderId, "item": item}"#,
            &["orderId", "item", "quantity"],
        ),
        (
            "the input writes its members in reverse of the template's order",
            r#"{"quantity":3,"item":"widget","orderId":"12345"}"#,
            r#"{"orderId": orderId, "item": item, "quantity": quantity}"#,
            &["quantity", "item", "orderId"],
        ),
        (
            "a member the input does not carry follows the ones it does",
            r#"{"orderId":"12345"}"#,
            r#"{"total": 7, "orderId": orderId}"#,
            &["orderId", "total"],
        ),
        (
            "no member comes from the input, so the names order themselves",
            "{}",
            r#"{"zulu": 1, "alpha": 2}"#,
            &["alpha", "zulu"],
        ),
        (
            "a nested object follows the order of the document it was read from",
            r#"{"outer":{"second":2,"first":1}}"#,
            r#"{"outer": {"first": outer.first, "second": outer.second}}"#,
            &["outer", "second", "first"],
        ),
    ];

    for (case, input, template, expected) in cases {
        let member = mapped_member(&router, case, input, template).await;
        let text = mapped_text(case, &member);
        assert_eq!(
            member_names(&text),
            *expected,
            "case {case:?}: expected the member order {expected:?}, got {text:?}"
        );
        // Repeatable: the same request answers with the same text, byte for byte, which a
        // hash-map order would not.
        let again = mapped_member(&router, case, input, template).await;
        assert_eq!(
            again, member,
            "case {case:?}: the same request must answer with the same text, got {again:?}"
        );
    }
}

/// The member names of every object in the JSON document `text`, in the order the document
/// writes them — read off the TEXT, because a parsed `serde_json::Value` sorts them.
fn member_names(text: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut characters = text.chars().peekable();
    let mut current = String::new();
    let mut in_string = false;
    let mut escaped = false;
    while let Some(character) = characters.next() {
        if in_string {
            if escaped {
                current.push(character);
                escaped = false;
            } else if character == '\\' {
                current.push(character);
                escaped = true;
            } else if character == '"' {
                in_string = false;
                // A name is a string followed by a colon; anything else is a value.
                let mut rest = characters.clone();
                while rest.peek().is_some_and(|next| next.is_whitespace()) {
                    rest.next();
                }
                if rest.peek() == Some(&':') {
                    names.push(current.clone());
                }
                current.clear();
            } else {
                current.push(character);
            }
        } else if character == '"' {
            in_string = true;
        }
    }
    names
}
