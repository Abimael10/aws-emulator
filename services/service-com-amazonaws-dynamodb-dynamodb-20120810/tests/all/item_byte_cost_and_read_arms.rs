// SPDX-License-Identifier: CC0-1.0

//! What the SIZE of an item is made of, and what the size a read or a write touched
//! costs, over the four places this service measures bytes.
//!
//! The rules pinned here, each over a SET of inputs rather than one example:
//!
//! 1. the stored width of an `N` attribute value is "1 byte per two significant digits"
//!    rounded up "plus 1 byte" (the Developer Guide's "Item sizes and formats"), and one
//!    more byte for a negative number — which is what makes an `N` value at the
//!    documented 38-digit precision ceiling the documented 21 bytes wide at most. The
//!    significant digits are the ones the value's own canonical decimal form spells with
//!    its leading zeros left out: the zeros a whole number carries before the decimal
//!    point are digits of it, the zeros a fraction ends with are not, and a zero between
//!    two significant digits always is. The boundary the rule is read at is the documented
//!    item ceiling — "The maximum item size in DynamoDB is 400 KB" — which an item one
//!    byte past is refused with HTTP 400 `ValidationException`;
//! 2. `ReturnConsumedCapacity=TOTAL` asks for "only the aggregate ConsumedCapacity for
//!    the operation" (the `ReturnConsumedCapacity` doc), so that answer carries the table
//!    and the aggregate and nothing else, while `INDEXES` carries the itemisation beside
//!    them;
//! 3. a page of a read that pages is the SHORTEST run of items whose sizes cross the
//!    documented 1 MB dataset limit, so a page that carries a `LastEvaluatedKey` because
//!    the budget stopped it always exceeds 1,048,576 bytes, and each item is measured as
//!    the store holds it — attribute names as well as values;
//! 4. a read served from a secondary index spends its units ON THAT INDEX, and the
//!    table's own arm carries what the read spent on the TABLE: nothing for a read the
//!    index answered on its own, and each fetched item's own 4 KB-rounded size where the
//!    read had to go on to the table for attributes the index does not project ("If any
//!    of the requested attributes are not projected into the local secondary index,
//!    DynamoDB fetches each of these attributes from the parent table" — `Query$Select`).

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, describe_when_active, emulator, error_code, message, ok};

/// "The maximum item size in DynamoDB is 400 KB" ("Service, Account, and Table Quotas").
const MAX_ITEM_BYTES: usize = 400 * 1024;

/// The fixed sentence a write past that ceiling is refused with.
const TOO_LARGE: &str = "Item size has exceeded the maximum allowed size";

/// The documented dataset limit of one page of a `Query` or a `Scan`, 1 MB.
const MAX_PAGE_BYTES: usize = 1024 * 1024;

/// One read capacity unit covers 4 KB of a strongly consistent read.
const READ_UNIT_BYTES: usize = 4 * 1024;

/// Create a table and wait until it serves requests.
async fn table(router: &MultiServiceRouter, case: &str, body: Value) {
    let name = body_table_name(&body);
    ok(router, "CreateTable", body).await;
    describe_when_active(router, case, name.as_str()).await;
}

/// The `TableName` a create carried.
fn body_table_name(body: &Value) -> String {
    body["TableName"].as_str().unwrap_or_default().to_string()
}

/// One row of the `N` catalogue: a literal, and the bytes storing it is accounted for.
///
/// The widths come from the rule stated in this module's docs, not from any recorded
/// response; the rows span what it generalises over — the digit count's parity, a whole
/// number's trailing zeros against a fraction's, an interior zero, leading zeros, the
/// sign, zero itself and the documented precision ceiling.
struct NumberCase {
    case: &'static str,
    literal: &'static str,
    bytes: usize,
}

/// The catalogue.
fn number_cases() -> Vec<NumberCase> {
    vec![
        NumberCase {
            case: "one significant digit",
            literal: "7",
            bytes: 2,
        },
        NumberCase {
            case: "two significant digits fill one byte",
            literal: "42",
            bytes: 2,
        },
        NumberCase {
            case: "a third digit pays for a byte of its own",
            literal: "123",
            bytes: 3,
        },
        NumberCase {
            case: "the trailing zeros of a whole number are digits of it",
            literal: "1200",
            bytes: 3,
        },
        NumberCase {
            case: "the trailing zeros of a fraction are not",
            literal: "12.00",
            bytes: 2,
        },
        NumberCase {
            case: "a zero between two significant digits is one of them",
            literal: "1.0001",
            bytes: 4,
        },
        NumberCase {
            case: "leading zeros are not significant",
            literal: "0.00123",
            bytes: 3,
        },
        NumberCase {
            case: "a negative number pays one byte more than its magnitude",
            literal: "-123",
            bytes: 4,
        },
        NumberCase {
            case: "zero holds no significant digits",
            literal: "0",
            bytes: 1,
        },
        NumberCase {
            case: "the documented precision ceiling, 38 digits",
            literal: "12345678901234567890123456789012345678",
            bytes: 20,
        },
        NumberCase {
            case: "a negative number at that ceiling is the widest an N value gets",
            literal: "-12345678901234567890123456789012345678",
            bytes: 21,
        },
        NumberCase {
            // A magnitude past the precision ceiling is spelled with an exponent rather
            // than with the run of zeros the decimal point would ask for, so what it is
            // spelled with is its mantissa — one digit here, either direction.
            case: "a magnitude past the precision ceiling spells its mantissa",
            literal: "1E+100",
            bytes: 2,
        },
        NumberCase {
            case: "and so does a magnitude under it",
            literal: "1e-100",
            bytes: 2,
        },
    ]
}

/// The table every size probe writes to.
const SIZED: &str = "Sized";

#[tokio::test]
async fn a_number_is_accounted_for_by_the_digits_its_canonical_form_spells() {
    let router = emulator();
    table(
        &router,
        "the table the size probes write to",
        json!({
            "TableName": SIZED,
            "BillingMode": "PAY_PER_REQUEST",
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
        }),
    )
    .await;

    for (position, row) in number_cases().into_iter().enumerate() {
        let key = format!("k{position}");
        let case = row.case;
        // The item is "pk" + its key, "n" + the number, and "v" + the padding that fills
        // the rest: every attribute costs its name's bytes as well as its value's.
        let fixed = "pk".len() + key.len() + "n".len() + row.bytes + "v".len();
        assert!(
            fixed < MAX_ITEM_BYTES,
            "case {case:?}: the fixed part of the probe must leave room to pad, got {fixed}"
        );
        let item = |pad: usize| {
            json!({
                "pk": {"S": key.clone()},
                "n": {"N": row.literal},
                "v": {"S": "p".repeat(pad)},
            })
        };

        // At the ceiling the write is stored...
        let at = MAX_ITEM_BYTES - fixed;
        let (status, body) = call(
            &router,
            "PutItem",
            json!({"TableName": SIZED, "Item": item(at)}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: an item of exactly {MAX_ITEM_BYTES} bytes with the number \
             {literal:?} ({bytes} bytes) must be stored, got {status} with {body}",
            literal = row.literal,
            bytes = row.bytes
        );

        // ...and one byte past it is refused, with the sentence that quota states.
        let (status, body) = call(
            &router,
            "PutItem",
            json!({"TableName": SIZED, "Item": item(at + 1)}),
        )
        .await;
        assert_eq!(
            (status, error_code(&body).as_str(), message(&body).as_str()),
            (400, "ValidationException", TOO_LARGE),
            "case {case:?}: one byte past the ceiling must be refused, got {status} with \
             {body}"
        );

        // The stored item is the accepted one: the refusal left nothing behind.
        let read = ok(
            &router,
            "GetItem",
            json!({"TableName": SIZED, "Key": {"pk": {"S": key.clone()}}, "ConsistentRead": true}),
        )
        .await;
        assert_eq!(
            read["Item"]["v"]["S"].as_str().map(str::len),
            Some(at),
            "case {case:?}: the stored item must be the one at the ceiling, got {read}"
        );
    }
}

#[tokio::test]
async fn the_total_level_reports_the_table_and_the_aggregate_alone() {
    let router = emulator();
    table(
        &router,
        "the table the capacity probes run against",
        json!({
            "TableName": "Levels",
            "BillingMode": "PAY_PER_REQUEST",
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [
                {"AttributeName": "pk", "AttributeType": "S"},
                {"AttributeName": "owner", "AttributeType": "S"},
            ],
            "GlobalSecondaryIndexes": [{
                "IndexName": "ByOwner",
                "KeySchema": [{"AttributeName": "owner", "KeyType": "HASH"}],
                "Projection": {"ProjectionType": "ALL"},
            }],
        }),
    )
    .await;
    ok(
        &router,
        "PutItem",
        json!({
            "TableName": "Levels",
            "Item": {"pk": {"S": "a"}, "owner": {"S": "o"}, "v": {"S": "x"}},
        }),
    )
    .await;

    // The rows span the data-plane surfaces that report a single `ConsumedCapacity`, read
    // and write alike: each is sent twice, once per level.
    let rows: Vec<(&str, &str, Value)> = vec![
        (
            "a strongly consistent GetItem",
            "GetItem",
            json!({"TableName": "Levels", "Key": {"pk": {"S": "a"}}, "ConsistentRead": true}),
        ),
        (
            "a Query of the base table",
            "Query",
            json!({
                "TableName": "Levels",
                "KeyConditionExpression": "pk = :p",
                "ExpressionAttributeValues": {":p": {"S": "a"}},
            }),
        ),
        (
            "a Scan of the base table",
            "Scan",
            json!({"TableName": "Levels"}),
        ),
        (
            "a PutItem",
            "PutItem",
            json!({
                "TableName": "Levels",
                "Item": {"pk": {"S": "b"}, "owner": {"S": "o"}, "v": {"S": "y"}},
            }),
        ),
        (
            "a DeleteItem",
            "DeleteItem",
            json!({"TableName": "Levels", "Key": {"pk": {"S": "b"}}}),
        ),
    ];

    for (case, operation, body) in rows {
        let mut total = body.clone();
        total["ReturnConsumedCapacity"] = json!("TOTAL");
        let answered = ok(&router, operation, total).await;
        let consumed = &answered["ConsumedCapacity"];
        let mut members: Vec<&String> = consumed
            .as_object()
            .unwrap_or_else(|| panic!("case {case:?}: expected a capacity object, got {answered}"))
            .keys()
            .collect();
        members.sort();
        assert_eq!(
            members,
            vec!["CapacityUnits", "TableName"],
            "case {case:?}: TOTAL asks for the aggregate alone, got {consumed}"
        );
        assert_eq!(
            consumed["TableName"],
            json!("Levels"),
            "case {case:?}: the aggregate names the table it was spent on, got {consumed}"
        );

        // The same call at the level that asks for the itemisation carries the `Table`
        // line, which is what tells the two levels apart.
        let mut indexes = body.clone();
        indexes["ReturnConsumedCapacity"] = json!("INDEXES");
        let answered = ok(&router, operation, indexes).await;
        let consumed = &answered["ConsumedCapacity"];
        assert!(
            consumed.get("Table").is_some(),
            "case {case:?}: INDEXES asks for the itemisation, got {consumed}"
        );
    }
}

/// One row of the paging table: the size of each item seeded, how many of them there are,
/// and how many a first page then holds.
struct PageCase {
    case: &'static str,
    item_bytes: usize,
    items: usize,
}

/// The rows: two item sizes whose runs cross the 1 MB budget at different counts, and one
/// whose whole table fits inside it.
fn page_cases() -> Vec<PageCase> {
    vec![
        PageCase {
            case: "items of a hundred thousand bytes",
            item_bytes: 100_000,
            items: 14,
        },
        PageCase {
            case: "items of sixty thousand bytes",
            item_bytes: 60_000,
            items: 20,
        },
        PageCase {
            case: "a table that fits inside the budget",
            item_bytes: 100_000,
            items: 5,
        },
    ]
}

/// The shortest run of `item_bytes`-sized items that CROSSES the budget, which is what a
/// truncated page holds — or `None` for a table that fits inside it.
fn crossing_prefix(row: &PageCase) -> Option<usize> {
    (1..=row.items).find(|count| count * row.item_bytes > MAX_PAGE_BYTES)
}

#[tokio::test]
async fn a_page_is_the_shortest_run_of_items_that_crosses_the_budget() {
    for (position, row) in page_cases().into_iter().enumerate() {
        let router = emulator();
        let name = format!("Paged{position}");
        let case = row.case;
        table(
            &router,
            case,
            json!({
                "TableName": name.clone(),
                "BillingMode": "PAY_PER_REQUEST",
                "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
                "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            }),
        )
        .await;

        // Each item is "pk" and its two-character key, plus "v" and its padding: the
        // names count, so the padding is the rest of the item's bytes.
        let fixed = "pk".len() + 2 + "v".len();
        let pad = row.item_bytes - fixed;
        for index in 0..row.items {
            ok(
                &router,
                "PutItem",
                json!({
                    "TableName": name.clone(),
                    "Item": {
                        "pk": {"S": format!("{index:02}")},
                        "v": {"S": "p".repeat(pad)},
                    },
                }),
            )
            .await;
        }

        let page = ok(&router, "Scan", json!({"TableName": name.clone()})).await;
        let count = page["Count"].as_u64().unwrap_or_default() as usize;
        match crossing_prefix(&row) {
            Some(expected) => {
                assert_eq!(
                    (count, page["ScannedCount"].as_u64().unwrap_or_default() as usize),
                    (expected, expected),
                    "case {case:?}: the page is the shortest run of {} items crossing \
                     {MAX_PAGE_BYTES} bytes, got Count {count} and {page}",
                    row.item_bytes
                );
                assert!(
                    count * row.item_bytes > MAX_PAGE_BYTES,
                    "case {case:?}: a page the budget stopped must exceed it, got {} bytes \
                     over {count} items",
                    count * row.item_bytes
                );
                assert!(
                    page.get("LastEvaluatedKey").is_some(),
                    "case {case:?}: a page the budget stopped reports where it stopped, got \
                     {page}"
                );
                // ...and the client pages on from there, over the items the first page
                // left: every item is read exactly once across the two pages.
                let rest = ok(
                    &router,
                    "Scan",
                    json!({
                        "TableName": name.clone(),
                        "ExclusiveStartKey": page["LastEvaluatedKey"].clone(),
                    }),
                )
                .await;
                let paged = count + rest["Count"].as_u64().unwrap_or_default() as usize;
                assert_eq!(
                    paged, row.items,
                    "case {case:?}: the pages together carry every item, got {paged} of {}",
                    row.items
                );
            }
            None => {
                assert_eq!(
                    (count, page.get("LastEvaluatedKey")),
                    (row.items, None),
                    "case {case:?}: a page inside the budget carries every item and no stop \
                     key, got {page}"
                );
            }
        }
    }
}

/// One row of the index-read table: how the query selects, and what the two arms then
/// carry.
struct ArmCase {
    case: &'static str,
    index: &'static str,
    select: Value,
    table_arm: f64,
}

#[tokio::test]
async fn an_index_read_bills_the_table_arm_for_what_it_fetched_from_the_table() {
    let router = emulator();
    let name = "Arms";
    table(
        &router,
        "a table with a keys-only and an all-projected local index",
        json!({
            "TableName": name,
            "BillingMode": "PAY_PER_REQUEST",
            "KeySchema": [
                {"AttributeName": "pk", "KeyType": "HASH"},
                {"AttributeName": "sk", "KeyType": "RANGE"},
            ],
            "AttributeDefinitions": [
                {"AttributeName": "pk", "AttributeType": "S"},
                {"AttributeName": "sk", "AttributeType": "N"},
                {"AttributeName": "l", "AttributeType": "S"},
                {"AttributeName": "m", "AttributeType": "S"},
            ],
            "LocalSecondaryIndexes": [
                {
                    "IndexName": "Keys",
                    "KeySchema": [
                        {"AttributeName": "pk", "KeyType": "HASH"},
                        {"AttributeName": "l", "KeyType": "RANGE"},
                    ],
                    "Projection": {"ProjectionType": "KEYS_ONLY"},
                },
                {
                    "IndexName": "All",
                    "KeySchema": [
                        {"AttributeName": "pk", "KeyType": "HASH"},
                        {"AttributeName": "m", "KeyType": "RANGE"},
                    ],
                    "Projection": {"ProjectionType": "ALL"},
                },
            ],
        }),
    )
    .await;

    // Two items of KNOWN size in one partition: one a byte past a read unit, one three
    // units wide, so the table arm can only be the SUM of each item's own rounded size
    // (2 + 3) and not one rounding over the pair.
    let sizes = [READ_UNIT_BYTES + 1, 12_000];
    let mut per_item: f64 = 0.0;
    for (index, bytes) in sizes.iter().enumerate() {
        // "pk" + 2, "sk" + 2 (a one-digit number is two bytes), "l" + 1, "m" + 1, "v" +
        // the padding that fills the rest. The sort keys are 1 and 2, so every item's
        // fixed part is the same width.
        let fixed = "pk".len() + 2 + "sk".len() + 2 + "l".len() + 1 + "m".len() + 1 + "v".len();
        ok(
            &router,
            "PutItem",
            json!({
                "TableName": name,
                "Item": {
                    "pk": {"S": "p1"},
                    "sk": {"N": (index + 1).to_string()},
                    "l": {"S": "a"},
                    "m": {"S": "b"},
                    "v": {"S": "p".repeat(bytes - fixed)},
                },
            }),
        )
        .await;
        let units = bytes.div_ceil(READ_UNIT_BYTES);
        per_item += units as f64;
    }

    let rows = vec![
        ArmCase {
            case: "a keys-only index asked for every attribute, which it must fetch",
            index: "Keys",
            select: json!({"Select": "ALL_ATTRIBUTES"}),
            table_arm: per_item,
        },
        ArmCase {
            // The fetch is a read of the table's own copy of each item, so what it costs
            // does not follow how the INDEX was read: the same rows cost the same on the
            // table arm strongly consistently.
            case: "the same fetch, read strongly consistently",
            index: "Keys",
            select: json!({"Select": "ALL_ATTRIBUTES", "ConsistentRead": true}),
            table_arm: per_item,
        },
        ArmCase {
            case: "a keys-only index asked only for what it projects",
            index: "Keys",
            select: json!({"Select": "ALL_PROJECTED_ATTRIBUTES"}),
            table_arm: 0.0,
        },
        ArmCase {
            case: "an all-projected index asked for every attribute, which it holds",
            index: "All",
            select: json!({"Select": "ALL_ATTRIBUTES"}),
            table_arm: 0.0,
        },
    ];

    for row in rows {
        let case = row.case;
        let mut request = json!({
            "TableName": name,
            "IndexName": row.index,
            "KeyConditionExpression": "pk = :p",
            "ExpressionAttributeValues": {":p": {"S": "p1"}},
            "ReturnConsumedCapacity": "INDEXES",
        });
        for (member, value) in row.select.as_object().into_iter().flatten() {
            request[member] = value.clone();
        }
        let answered = ok(&router, "Query", request).await;
        let consumed = &answered["ConsumedCapacity"];
        assert_eq!(
            consumed["Table"]["CapacityUnits"].as_f64(),
            Some(row.table_arm),
            "case {case:?}: the table arm carries what the read fetched from the table, \
             expected {}, got {consumed}",
            row.table_arm
        );
        let index_arm = consumed["LocalSecondaryIndexes"][row.index]["CapacityUnits"]
            .as_f64()
            .unwrap_or_else(|| {
                panic!("case {case:?}: the index the read was served from carries its own \
                        units, got {consumed}")
            });
        assert!(
            index_arm > 0.0,
            "case {case:?}: reading the index costs units on the index, got {consumed}"
        );
        assert_eq!(
            consumed["CapacityUnits"].as_f64(),
            Some(index_arm + row.table_arm),
            "case {case:?}: the aggregate is what the index and the table arms carry \
             together, got {consumed}"
        );
    }
}
