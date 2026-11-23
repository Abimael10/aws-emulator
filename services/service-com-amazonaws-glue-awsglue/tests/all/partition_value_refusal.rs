// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the refusals of a partition filter — the `Message` `GetPartitions`
//! answers an `Expression` it cannot run with.
//!
//! The rule under test: a partition-filter `Expression` is refused in one of THREE classes.
//! Against a table that declares no partition keys there is no key collection to read a
//! filter against, and the read fails on that absent collection whatever the expression
//! says. Against a partitioned table, a comparison with a literal that cannot be read as
//! the addressed key's declared type is refused with `InvalidInputException` whose `Message`
//! is the server-side exception class name, then the offending literal as the expression
//! spells it, then the type the key is declared with —
//! `com.amazonaws.daylight.core.exceptions.InvalidPartitionValueException: <literal> is not
//! a <type>.`; the key's own name is no part of that sentence. Every OTHER way an expression
//! fails to be a filter over the keys that table declares — text that is no expression, an
//! operator the member does not document, a column it does not partition by, a key of a type
//! the filter cannot compare — is the one FIXED sentence `Invalid partition expression!`,
//! which carries nothing of the request at all. Text that is no expression is read before
//! any table is, so it earns that fixed sentence whatever table it addresses.
//!
//! The suites are tables whose rows differ along what the rule generalises over: which
//! declared type the addressed key carries, in which way the literal fails to be a value
//! of it, how the literal is written (quoted text, digits, `NULL`, a boolean, a signed
//! number), and which operator and operand order the comparison uses. The recorded wire
//! case (a word against a `date` key) is ONE row among them.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::error_messages::assert_refusal;
use crate::wire::{call, emulator, error_code, error_message, ok};

/// The fixed text of the refusal, stated here rather than read from the service's own
/// constant so the test pins the wording instead of restating it.
const CLASS_PREFIX: &str = "com.amazonaws.daylight.core.exceptions.InvalidPartitionValueException";

/// The whole `Message` the rule requires for `literal` against a key declared `declared`.
fn expected_message(literal: &str, declared: &str) -> String {
    format!("{CLASS_PREFIX}: {literal} is not a {declared}.")
}

/// The database every suite here seeds into.
const DATABASE: &str = "partition_values";

/// Create `table` in [`DATABASE`], partitioned by one key named `key` of declared type
/// `declared`, and register one partition at `value` so the table has something a
/// well-formed filter could select.
async fn seed_table(
    router: &MultiServiceRouter,
    table: &str,
    key: &str,
    declared: &str,
    value: &str,
) {
    let (status, body) = call(
        router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": DATABASE } }),
    )
    .await;
    assert!(
        status == 200 || error_code(&body).contains("AlreadyExistsException"),
        "seeding {DATABASE}: expected the database to exist, got {status} with {body}"
    );
    ok(
        router,
        "CreateTable",
        &json!({
            "DatabaseName": DATABASE,
            "TableInput": {
                "Name": table,
                "PartitionKeys": [{ "Name": key, "Type": declared }]
            }
        }),
    )
    .await;
    ok(
        router,
        "CreatePartition",
        &json!({
            "DatabaseName": DATABASE,
            "TableName": table,
            "PartitionInput": {
                "Values": [value],
                "StorageDescriptor": { "Location": format!("s3://lake/{table}/{value}") }
            }
        }),
    )
    .await;
}

/// One row of the refusal suite: the case name, the declared type of the single partition
/// key, one stored partition value of that type, the `Expression` to list with, and the
/// literal the refusal must name.
struct Case<'row> {
    case: &'row str,
    declared: &'row str,
    stored: &'row str,
    expression: &'row str,
    literal: &'row str,
}

/// Every type class a filter compares, and every way a literal can fail to be a value of
/// it, each refused with the class-prefixed message naming that literal and that type.
///
/// The declared types are the ones the member documents as filterable (`string`, `date`,
/// `timestamp`, `int`, `bigint`, `long`, `tinyint`, `smallint`, `decimal`, from
/// `smithy_query` on `GetPartitionsRequest$Expression`) — including entries the issue did
/// not cite, and a parameterised `decimal(10,2)` whose declared type the message carries
/// exactly as the table stores it.
#[tokio::test]
async fn a_literal_that_is_no_value_of_the_key_type_is_refused_by_class_and_literal() {
    let cases = vec![
        Case {
            case: "a word against a date key",
            declared: "date",
            stored: "2024-01-05",
            expression: "eventdate = 'yesterday'",
            literal: "yesterday",
        },
        Case {
            case: "calendar components that name no day against a date key",
            declared: "date",
            stored: "2024-01-05",
            expression: "eventdate >= '2024-13-45'",
            literal: "2024-13-45",
        },
        Case {
            case: "digits that are no instant against a date key",
            declared: "date",
            stored: "2024-01-05",
            expression: "eventdate <> 20241301",
            literal: "20241301",
        },
        Case {
            case: "a time of day no clock shows against a timestamp key",
            declared: "timestamp",
            stored: "2024-01-05 01:00:00",
            expression: "eventdate < '2024-01-05 25:00:00'",
            literal: "2024-01-05 25:00:00",
        },
        Case {
            case: "a word against an int key",
            declared: "int",
            stored: "9",
            expression: "eventdate = 'abc'",
            literal: "abc",
        },
        Case {
            case: "a word against a bigint key",
            declared: "bigint",
            stored: "9000000000",
            expression: "eventdate > 'many'",
            literal: "many",
        },
        Case {
            case: "a word against a long key",
            declared: "long",
            stored: "12",
            expression: "eventdate <= 'twelve'",
            literal: "twelve",
        },
        Case {
            case: "a word against a tinyint key",
            declared: "tinyint",
            stored: "3",
            expression: "eventdate = 'three'",
            literal: "three",
        },
        Case {
            case: "a word against a smallint key",
            declared: "smallint",
            stored: "300",
            expression: "eventdate >= 'three hundred'",
            literal: "three hundred",
        },
        Case {
            case: "a word against a parameterised decimal key",
            declared: "decimal(10,2)",
            stored: "1.50",
            expression: "eventdate > 'a lot'",
            literal: "a lot",
        },
        Case {
            case: "NULL against an int key",
            declared: "int",
            stored: "9",
            expression: "eventdate = NULL",
            literal: "NULL",
        },
        Case {
            case: "NULL against a string key",
            declared: "string",
            stored: "us-east-1",
            expression: "eventdate = NULL",
            literal: "NULL",
        },
        Case {
            case: "a boolean against a string key",
            declared: "string",
            stored: "us-east-1",
            expression: "eventdate = true",
            literal: "true",
        },
        Case {
            case: "a signed number against a date key",
            declared: "date",
            stored: "2024-01-05",
            expression: "eventdate = -2024",
            literal: "-2024",
        },
        Case {
            case: "the literal written on the left of the comparison",
            declared: "date",
            stored: "2024-01-05",
            expression: "'yesterday' = eventdate",
            literal: "yesterday",
        },
        Case {
            case: "one entry of an IN list",
            declared: "int",
            stored: "9",
            expression: "eventdate IN (9, 'abc')",
            literal: "abc",
        },
        Case {
            case: "the high end of a BETWEEN",
            declared: "date",
            stored: "2024-01-05",
            expression: "eventdate BETWEEN '2024-01-01' AND 'tomorrow'",
            literal: "tomorrow",
        },
        Case {
            case: "a negated comparison",
            declared: "int",
            stored: "9",
            expression: "NOT eventdate = 'abc'",
            literal: "abc",
        },
        Case {
            case: "one side of an AND of two comparisons",
            declared: "int",
            stored: "9",
            expression: "eventdate = 9 AND eventdate = 'abc'",
            literal: "abc",
        },
    ];

    for (index, row) in cases.iter().enumerate() {
        let router = emulator();
        let table = format!("events{index}");
        seed_table(&router, &table, "eventdate", row.declared, row.stored).await;

        let body = assert_refusal(
            &router,
            row.case,
            "GetPartitions",
            &json!({
                "DatabaseName": DATABASE,
                "TableName": table,
                "Expression": row.expression
            }),
            "InvalidInputException",
            &expected_message(row.literal, row.declared),
        )
        .await;

        // The sentence is about the VALUE: the key's own name is no part of it.
        assert!(
            !error_message(&body).contains("eventdate"),
            "case {:?}: the refusal must not name the partition key, got {:?}",
            row.case,
            error_message(&body)
        );
    }
}

/// The controls around that refusal: a well-formed comparison on the same keys still
/// serves the partitions, and every OTHER way an expression fails to be a filter is the one
/// fixed refusal — the exception class belongs to the value fault alone.
#[tokio::test]
async fn the_neighbouring_outcomes_of_a_partition_filter_are_unchanged() {
    let router = emulator();
    seed_table(&router, "dated", "eventdate", "date", "2024-01-05").await;
    seed_table(&router, "numbered", "eventdate", "int", "9").await;
    seed_table(&router, "flagged", "eventdate", "boolean", "true").await;
    // A table with a real column that is NOT one of its partition keys, so a filter on that
    // column names a column the table HAS and still is no filter on its partitions.
    ok(
        &router,
        "CreateTable",
        &json!({
            "DatabaseName": DATABASE,
            "TableInput": {
                "Name": "columned",
                "StorageDescriptor": { "Columns": [{ "Name": "payload", "Type": "string" }] },
                "PartitionKeys": [{ "Name": "eventdate", "Type": "date" }]
            }
        }),
    )
    .await;

    // A literal the key's type reads selects by value, whatever form it is written in.
    let served: Vec<(&str, &str, &str)> = vec![
        ("a date literal of the key's own form", "dated", "eventdate = '2024-01-05'"),
        ("a date literal written with a time", "dated", "eventdate >= '2024-01-05 00:00:00'"),
        ("a number literal against a numeric key", "numbered", "eventdate = 9"),
        ("a quoted number against a numeric key", "numbered", "eventdate = '9'"),
        ("a range the stored value falls in", "numbered", "eventdate BETWEEN 1 AND 10"),
    ];
    for (case, table, expression) in served {
        let body = ok(
            &router,
            "GetPartitions",
            &json!({
                "DatabaseName": DATABASE,
                "TableName": table,
                "Expression": expression
            }),
        )
        .await;
        let listed = body["Partitions"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: expected a Partitions list, got {body}"));
        assert_eq!(
            listed.len(),
            1,
            "case {case:?}: expected the stored partition to be selected, got {body}"
        );
    }

    // Every OTHER way an expression fails to be a filter over the table's partition keys is
    // the one fixed refusal: it carries nothing of the request, so the column at fault, the
    // declared type and the expression itself are all withheld.
    let other_faults: Vec<(&str, &str, &str)> = vec![
        ("a column the table does not hold at all", "dated", "country = 'us'"),
        (
            "a real column that is no partition key",
            "columned",
            "payload = 'hello'",
        ),
        (
            "a partition key of a type an expression cannot compare",
            "flagged",
            "eventdate = 'true'",
        ),
        ("text that is no expression at all", "dated", "this is not a filter("),
        ("a column name on its own", "dated", "eventdate"),
        ("text past the expression", "dated", "eventdate = '2024-01-05' leftover"),
        ("an operator the member does not document", "numbered", "eventdate + 1 = 10"),
    ];
    for (case, table, expression) in other_faults {
        let body = assert_refusal(
            &router,
            case,
            "GetPartitions",
            &json!({
                "DatabaseName": DATABASE,
                "TableName": table,
                "Expression": expression
            }),
            "InvalidInputException",
            "Invalid partition expression!",
        )
        .await;
        assert!(
            !error_message(&body).contains(CLASS_PREFIX),
            "case {case:?}: only the value fault carries the exception class, got {:?}",
            error_message(&body)
        );
    }
}

/// A table that declares NO partition keys is its own refusal class: a filter selects among
/// the keys a partition is addressed by, so there is no key collection to read one against,
/// and the read fails on that absent collection whatever the expression says.
#[tokio::test]
async fn a_filter_on_a_table_that_declares_no_partition_keys_fails_on_the_absent_key_list() {
    let router = emulator();
    let (status, body) = call(
        &router,
        "CreateDatabase",
        &json!({ "DatabaseInput": { "Name": DATABASE } }),
    )
    .await;
    assert!(
        status == 200 || error_code(&body).contains("AlreadyExistsException"),
        "seeding {DATABASE}: expected the database to exist, got {status} with {body}"
    );
    // Two tables with no partition keys: one that declares the member as an empty list and
    // one that leaves it out, each with a real column so a filter can name a column the
    // table does hold.
    for table in ["empty_keys", "no_keys"] {
        let mut input = json!({
            "DatabaseName": DATABASE,
            "TableInput": {
                "Name": table,
                "StorageDescriptor": { "Columns": [{ "Name": "payload", "Type": "string" }] }
            }
        });
        if table == "empty_keys" {
            input["TableInput"]["PartitionKeys"] = json!([]);
        }
        ok(&router, "CreateTable", &input).await;
    }

    // Rows differ along what the expression says: a column the table holds, one it does not,
    // and a comparison against a literal of another type — the answer is the same, because
    // the key list is missing before any of it is read. Text that is no expression at all is
    // refused earlier, when the request's own text is read, so it stays the expression
    // refusal and is covered by the suite above.
    let cases: Vec<(&str, &str, &str)> = vec![
        ("a real column of a table with an empty key list", "empty_keys", "payload = 'hello'"),
        ("a real column of a table that declares no keys", "no_keys", "payload = 'hello'"),
        ("a column the table does not hold", "no_keys", "eventdate = '2024-01-05'"),
        ("a comparison a key type would refuse", "no_keys", "payload = NULL"),
        ("an IN list over a column the table holds", "no_keys", "payload IN ('a', 'b')"),
    ];
    for (case, table, expression) in cases {
        assert_refusal(
            &router,
            case,
            "GetPartitions",
            &json!({
                "DatabaseName": DATABASE,
                "TableName": table,
                "Expression": expression
            }),
            "InvalidInputException",
            "Cannot invoke \"java.util.Collection.toArray()\" because \"elements\" is null",
        )
        .await;
    }

    // The control: the same tables read with NO expression are served, so what the refusal
    // is about is the filter and not the table.
    for table in ["empty_keys", "no_keys"] {
        let body = ok(
            &router,
            "GetPartitions",
            &json!({ "DatabaseName": DATABASE, "TableName": table }),
        )
        .await;
        assert_eq!(
            body["Partitions"],
            json!([]),
            "case {table:?}: a table with no partition keys holds no partition, got {body}"
        );
    }
}

/// Two reads answer the same thing: a refused listing writes nothing, so the table still
/// holds its partition and the refusal repeats verbatim.
#[tokio::test]
async fn a_refused_listing_changes_nothing_a_later_caller_sees() {
    let router = emulator();
    seed_table(&router, "dated", "eventdate", "date", "2024-01-05").await;

    let refusing = json!({
        "DatabaseName": DATABASE,
        "TableName": "dated",
        "Expression": "eventdate = 'yesterday'"
    });
    let expected = expected_message("yesterday", "date");
    for attempt in 1..=2 {
        assert_refusal(
            &router,
            &format!("attempt {attempt}"),
            "GetPartitions",
            &refusing,
            "InvalidInputException",
            &expected,
        )
        .await;
    }

    let body: Value = ok(
        &router,
        "GetPartitions",
        &json!({ "DatabaseName": DATABASE, "TableName": "dated" }),
    )
    .await;
    assert_eq!(
        body["Partitions"][0]["Values"],
        json!(["2024-01-05"]),
        "a refused listing must leave the table's partitions untouched, got {body}"
    );
}
