// SPDX-License-Identifier: CC0-1.0

//! What UNIT `size()` measures a value in, read through the predicate it is compared in.
//!
//! RULE: `size (path)` of a String is the number of UTF-16 CODE UNITS the string's
//! characters are carried in — one per character of the basic multilingual plane whatever
//! its transfer encoding needs, and two for a character outside that plane, which a
//! surrogate pair spells. A binary value has no characters, so its size is its bytes, and a
//! collection's is its members. Measuring a string in the bytes of its UTF-8 transfer
//! encoding instead would make every `size()` predicate over a non-ASCII string select a
//! different set of items than the same call against AWS.
//!
//! The rows span what the rule generalises over: the width a character takes in each
//! encoding (ASCII, a two-byte and a three-byte character of the basic plane, a character
//! outside it, a mixture of all four), the empty string, the comparison the size is read in
//! (`=` and the orderings) and the surface that reads it (a `Scan` filter and a condition
//! on a write). The byte counts are stated beside the code-unit counts, and the predicates
//! over them must select NOTHING, which is what tells the two units apart.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, emulator, error_code, message, ok};
use crate::table_items::create_active_table;

/// The table every row scans: one string partition key.
const TABLE: &str = "SizeUnits";

/// One stored string: the key it is written under, the text, how many UTF-16 code units it
/// carries and how many bytes its UTF-8 encoding needs.
struct Stored {
    key: &'static str,
    text: &'static str,
    units: i64,
    bytes: i64,
}

fn strings() -> Vec<Stored> {
    vec![
        Stored {
            key: "empty",
            text: "",
            units: 0,
            bytes: 0,
        },
        Stored {
            key: "ascii",
            text: "abc",
            units: 3,
            bytes: 3,
        },
        Stored {
            // Three characters of the basic multilingual plane, three bytes each.
            key: "bmp",
            text: "日本語",
            units: 3,
            bytes: 9,
        },
        Stored {
            // One basic-plane character, two bytes.
            key: "latin",
            text: "é",
            units: 1,
            bytes: 2,
        },
        Stored {
            // Two characters OUTSIDE the basic plane: two code units each.
            key: "astral",
            text: "😀😀",
            units: 4,
            bytes: 8,
        },
        Stored {
            key: "mixed",
            text: "aé日😀",
            units: 5,
            bytes: 10,
        },
    ]
}

async fn seeded() -> MultiServiceRouter {
    let router = emulator();
    create_active_table(&router, TABLE, &[("pk", "HASH", "S")]).await;
    for stored in strings() {
        ok(
            &router,
            "PutItem",
            json!({"TableName": TABLE, "Item": {
                "pk": {"S": stored.key},
                "s": {"S": stored.text},
            }}),
        )
        .await;
    }
    // One binary value beside them: a `B` has no characters to count, so its size is its
    // bytes — which is what keeps the rule above about STRINGS.
    ok(
        &router,
        "PutItem",
        json!({"TableName": TABLE, "Item": {
            "pk": {"S": "binary"},
            "b": {"B": "AQID"},
        }}),
    )
    .await;
    router
}

/// The keys a `Scan` whose filter is `filter` selects, read off the page it answers.
async fn selected(
    router: &MultiServiceRouter,
    case: &str,
    filter: &str,
    values: Value,
) -> Vec<String> {
    let (status, body) = call(
        router,
        "Scan",
        json!({"TableName": TABLE, "ConsistentRead": true,
               "FilterExpression": filter,
               "ExpressionAttributeValues": values}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the filter to be served, got {status} with {body}"
    );
    let mut keys: Vec<String> = body["Items"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|item| item["pk"]["S"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default();
    keys.sort();
    keys
}

#[tokio::test]
async fn size_of_a_string_selects_by_its_code_unit_count() {
    let router = seeded().await;

    // Each row: the code-unit count, and the keys whose stored string carries exactly
    // that many. Driven over every count the stored strings reach, plus the one no string
    // reaches (2), so the rule is read as a function and not as one lucky row.
    for count in 0..=5i64 {
        let expected: Vec<String> = strings()
            .iter()
            .filter(|stored| stored.units == count)
            .map(|stored| stored.key.to_string())
            .collect();
        let mut expected = expected;
        expected.sort();
        let case = format!("size(s) = {count}");
        let keys = selected(
            &router,
            &case,
            "size(s) = :n",
            json!({":n": {"N": count.to_string()}}),
        )
        .await;
        assert_eq!(
            keys, expected,
            "case {case:?}: expected the strings of {count} code units {expected:?}, got \
             {keys:?}"
        );
    }

    // The same predicate over each string's UTF-8 BYTE count selects it only where the two
    // counts agree, which is what tells the unit apart.
    for stored in strings() {
        let case = format!("{:?} compared against its {} bytes", stored.text, stored.bytes);
        let keys = selected(
            &router,
            &case,
            "size(s) = :n",
            json!({":n": {"N": stored.bytes.to_string()}}),
        )
        .await;
        let selects_it = keys.contains(&stored.key.to_string());
        assert_eq!(
            selects_it,
            stored.units == stored.bytes,
            "case {case:?}: a byte count selects the string only where it equals its \
             {} code units, got {keys:?}",
            stored.units
        );
    }
}

#[tokio::test]
async fn the_orderings_and_a_write_condition_read_the_same_unit() {
    let router = seeded().await;

    // The orderings read the same number: every string of at most four code units is
    // selected by `<= 4`, and the one of five is not.
    let at_most_four: Vec<String> = {
        let mut keys: Vec<String> = strings()
            .iter()
            .filter(|stored| stored.units <= 4)
            .map(|stored| stored.key.to_string())
            .collect();
        keys.sort();
        keys
    };
    let keys = selected(
        &router,
        "size(s) <= 4",
        "size(s) <= :n",
        json!({":n": {"N": "4"}}),
    )
    .await;
    assert_eq!(
        keys, at_most_four,
        "case size(s) <= 4: expected {at_most_four:?}, got {keys:?}"
    );

    // A BINARY value has no characters, so its size is its bytes: three for the three
    // bytes the stored blob carries.
    let keys = selected(
        &router,
        "size(b) = 3",
        "size(b) = :n",
        json!({":n": {"N": "3"}}),
    )
    .await;
    assert_eq!(
        keys,
        vec!["binary".to_string()],
        "case size(b) = 3: expected the binary item, got {keys:?}"
    );

    // And a write gated on the code-unit count of a stored string lands, while the same
    // write gated on its byte count is the unmet condition it is.
    let (status, body) = call(
        &router,
        "DeleteItem",
        json!({"TableName": TABLE, "Key": {"pk": {"S": "bmp"}},
               "ConditionExpression": "size(s) = :n",
               "ExpressionAttributeValues": {":n": {"N": "9"}}}),
    )
    .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, "ConditionalCheckFailedException"),
        "case a delete gated on the byte count: expected the condition to fail, got \
         {status} with {} ",
        message(&body)
    );
    let (status, body) = call(
        &router,
        "DeleteItem",
        json!({"TableName": TABLE, "Key": {"pk": {"S": "bmp"}},
               "ConditionExpression": "size(s) = :n",
               "ExpressionAttributeValues": {":n": {"N": "3"}}}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case a delete gated on the code-unit count: expected it to apply, got {status} \
         with {body}"
    );
    // The delete it gated actually happened, read back through the implemented reader.
    let read = ok(
        &router,
        "GetItem",
        json!({"TableName": TABLE, "Key": {"pk": {"S": "bmp"}}, "ConsistentRead": true}),
    )
    .await;
    assert_eq!(
        read.get("Item"),
        None,
        "case a delete gated on the code-unit count: expected the item gone, got {read}"
    );
}
