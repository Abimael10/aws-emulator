// SPDX-License-Identifier: CC0-1.0

//! Which CLAUSE refuses a `TableName` the service cannot resolve: the ARN-format clause
//! for a value spelled as an ARN, the member's own length and pattern clauses for a value
//! spelled as a bare name.
//!
//! A `TableName` member targets `TableArn` (length 1..=1024, no modelled pattern), so it
//! accepts either spelling — "You can also provide the Amazon Resource Name (ARN) of the
//! table in this parameter" (the `DescribeTable$TableName` doc). The rule asserted here is
//! about what a refusal SAYS:
//!
//! * a value spelled as an ARN is parsed as one, and its RESOURCE NAME is held to the form
//!   a table name takes (3..=255 characters of `[a-zA-Z0-9_.-]`, the
//!   `CreateTable$TableName` doc's own bounds) — a violation is stated by the ARN-format
//!   clause, which carries the template and those three resource-name sub-clauses, over
//!   the whole ARN that was sent;
//! * the bare-name clauses (`Member must have length ...`, `Member must satisfy regular
//!   expression pattern: ...`) state a violation only for a value sent as a bare name:
//!   telling a caller that a well-formed ARN failed a table-name character pattern names
//!   a constraint the value was never held to.
//!
//! The rows span HOW the resource name is unacceptable (a sub-resource path, below the
//! minimum, past the maximum, an excluded character, absent) and over which operation the
//! value arrived, and they are contrasted with the refusals that must NOT change: another
//! resource type, a bare name, and the table's own ARN, which resolves.
//!
//! Every call goes over the wire (`POST /` with
//! `X-Amz-Target: DynamoDB_20120810.<OperationName>`) through the same
//! `MultiServiceRouter` the emulator's front doors use.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call, describe_when_active, emulator, error_code, message, ok, REGION};

/// The account every ARN this emulator mints carries, and so the account a caller
/// spelling a table's ARN by hand addresses.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The one key attribute the table below is created with; no row varies it.
const PARTITION_KEY: &str = "Part";

/// The table every row addresses, so no refusal here is about an empty store.
const TABLE: &str = "arn-resource-name";

/// The documented form a table name takes, which is the form an ARN's resource name is
/// held to: the `CreateTable$TableName` bounds (3..=255) and pattern (`[a-zA-Z0-9_.-]+`,
/// stated without the anchors the model spells it with).
const MIN_NAME_LENGTH: usize = 3;
const MAX_NAME_LENGTH: usize = 255;
const NAME_PATTERN: &str = "[a-zA-Z0-9_.-]+";

/// The whole message a value spelled as an ARN earns when it is not a well-formed table
/// ARN: the validation envelope, the member it came in, the value as the request spelled
/// it, and the ARN-format constraint with its resource-name sub-clauses.
fn arn_format_message(value: &str) -> String {
    format!(
        "1 validation error detected: Value '{value}' at 'tableName' failed to satisfy \
         constraint: Valid ARN format is \
         'arn:<awsPartition>:<vendor>:<region>:<subscriber>:resourceType/resourceName', where \
         Resource name must have length less than or equal to {MAX_NAME_LENGTH}, Resource name \
         must have length greater than or equal to {MIN_NAME_LENGTH}, Resource name must \
         satisfy regular expression pattern: {NAME_PATTERN}"
    )
}

/// Create the table every row addresses and wait for it to report `ACTIVE`, so a refusal
/// below is about the value that was sent.
async fn table(router: &MultiServiceRouter) {
    ok(
        router,
        "CreateTable",
        json!({
            "TableName": TABLE,
            "KeySchema": [{"AttributeName": PARTITION_KEY, "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": PARTITION_KEY, "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    describe_when_active(router, "the table every row addresses", TABLE).await;
}

/// Every operation one row drives the value through, as the operation name and a body that
/// is otherwise valid: a control-plane reader, a data-plane read and a settings reader, so
/// the clause is the resolution's and not one operation's.
fn requests(value: &str) -> Vec<(&'static str, Value)> {
    vec![
        ("DescribeTable", json!({"TableName": value})),
        (
            "GetItem",
            json!({"TableName": value, "Key": {PARTITION_KEY: {"S": "one"}}}),
        ),
        ("DescribeTimeToLive", json!({"TableName": value})),
    ]
}

/// Send `request` and answer the error code and message of the refusal it must be.
async fn refusal(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    request: Value,
) -> (String, String) {
    let (status, body) = call(router, operation, request).await;
    assert_eq!(
        status, 400,
        "case {case:?} ({operation}): expected HTTP 400, got {status} with {body}"
    );
    (error_code(&body), message(&body))
}

#[tokio::test]
async fn an_arn_whose_resource_name_is_not_a_table_s_is_refused_by_the_arn_format() {
    let router = emulator();
    table(&router).await;

    // The rows differ in HOW the ARN's resource name fails the table-name form: a
    // sub-resource path (a stream's, an index's, and one deeper still), a name below the
    // documented minimum, one past the documented maximum, a character the pattern
    // excludes, and no resource name at all.
    let longest_name = "z".repeat(MAX_NAME_LENGTH + 1);
    let values: Vec<(&str, String)> = vec![
        (
            "a stream sub-resource path",
            format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/{TABLE}/stream/2026-01-01T00:00:00.000"),
        ),
        (
            "an index sub-resource path",
            format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/{TABLE}/index/by-part"),
        ),
        (
            "a path deeper than a sub-resource",
            format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/{TABLE}/stream/label/extra"),
        ),
        (
            "a resource name one character below the minimum length",
            format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/ab"),
        ),
        (
            "a resource name one character past the maximum length",
            format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/{longest_name}"),
        ),
        (
            "a resource name holding a character the pattern excludes",
            format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/bad name!"),
        ),
        (
            "no resource name at all",
            format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/"),
        ),
    ];

    for (case, value) in &values {
        let expected = arn_format_message(value);
        for (operation, request) in requests(value) {
            let (code, stated) = refusal(&router, case, operation, request).await;
            assert_eq!(
                (code.as_str(), stated.as_str()),
                ("ValidationException", expected.as_str()),
                "case {case:?} ({operation}): expected the ARN-format clause, got {code} with \
                 {stated:?}"
            );
            assert!(
                !stated.contains("Member must"),
                "case {case:?} ({operation}): an ARN is never refused by a bare name's clause, \
                 got {stated:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_value_that_is_no_arn_at_all_is_refused_by_the_arn_format_too() {
    let router = emulator();
    table(&router).await;

    // A value spelled as an ARN that is not a well-formed one is refused by the SAME
    // clause: the rows span a field left empty and a field count short of one.
    let values: Vec<(&str, String)> = vec![
        (
            "an empty partition",
            format!("arn::dynamodb:{REGION}:{ACCOUNT}:table/{TABLE}"),
        ),
        (
            "an empty vendor",
            format!("arn:aws::{REGION}:{ACCOUNT}:table/{TABLE}"),
        ),
        (
            "an empty region",
            format!("arn:aws:dynamodb::{ACCOUNT}:table/{TABLE}"),
        ),
        (
            "an empty subscriber",
            format!("arn:aws:dynamodb:{REGION}::table/{TABLE}"),
        ),
        (
            "an empty resource",
            format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:"),
        ),
        (
            "one field too few",
            format!("arn:aws:dynamodb:{REGION}:table/{TABLE}"),
        ),
    ];

    for (case, value) in &values {
        let expected = arn_format_message(value);
        let (code, stated) = refusal(&router, case, "DescribeTable", json!({"TableName": value}))
            .await;
        assert_eq!(
            (code.as_str(), stated.as_str()),
            ("ValidationException", expected.as_str()),
            "case {case:?}: expected the ARN-format clause, got {code} with {stated:?}"
        );
    }
}

#[tokio::test]
async fn a_bare_name_is_still_refused_by_the_member_s_own_clauses() {
    let router = emulator();
    table(&router).await;

    // A value the caller sent as a NAME carries no ARN to blame, so the clause is the
    // bound it broke, over the name itself. The rows span each of the three bounds.
    let too_long = "z".repeat(MAX_NAME_LENGTH + 1);
    let rows: Vec<(&str, String, String)> = vec![
        (
            "one character below the minimum length",
            "ab".to_string(),
            format!("Member must have length greater than or equal to {MIN_NAME_LENGTH}"),
        ),
        (
            "one character past the maximum length",
            too_long.clone(),
            format!("Member must have length less than or equal to {MAX_NAME_LENGTH}"),
        ),
        (
            "a character the pattern excludes",
            "bad name!".to_string(),
            format!("Member must satisfy regular expression pattern: {NAME_PATTERN}"),
        ),
    ];

    for (case, value, requirement) in &rows {
        let expected = format!(
            "1 validation error detected: Value '{value}' at 'tableName' failed to satisfy \
             constraint: {requirement}"
        );
        let (code, stated) = refusal(&router, case, "DescribeTable", json!({"TableName": value}))
            .await;
        assert_eq!(
            (code.as_str(), stated.as_str()),
            ("ValidationException", expected.as_str()),
            "case {case:?}: expected the member's own clause, got {code} with {stated:?}"
        );
    }
}

#[tokio::test]
async fn an_arn_of_another_resource_kind_still_states_its_type_and_a_table_s_own_resolves() {
    let router = emulator();
    table(&router).await;

    // An ARN of this service whose resource TYPE is not a table's is refused by the type:
    // its resource name is acceptable, so the ARN-format clause has nothing to say about
    // it. The diagnosis names the FIELD of the ARN that is wrong, over the value the
    // request sent — the frame this operation states an ARN field's refusals in, the same
    // one it states a cross-region ARN in. The rows span another type, a mis-cased one and
    // no type at all.
    for (case, value) in [
        (
            "a backup ARN",
            format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:backup/{TABLE}"),
        ),
        (
            "a mis-cased table type",
            format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:Table/{TABLE}"),
        ),
        (
            "an ARN with no resource type",
            format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:{TABLE}"),
        ),
    ] {
        let (code, stated) = refusal(
            &router,
            case,
            "DescribeTable",
            json!({"TableName": value.clone()}),
        )
        .await;
        assert_eq!(
            (code.as_str(), stated.as_str()),
            (
                "ValidationException",
                format!("1 validation error detected: Invalid resource type in '{value}'").as_str()
            ),
            "case {case:?}: expected the resource-type sentence over the ARN, got {code} with \
             {stated:?}"
        );
    }

    // And the table's OWN ARN resolves, so the refusals above are about the values that
    // were sent and not about ARNs in the member.
    let own = format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/{TABLE}");
    let described = ok(&router, "DescribeTable", json!({"TableName": own})).await;
    assert_eq!(
        described["Table"]["TableName"],
        json!(TABLE),
        "case the table's own ARN: expected the table it names, got {described}"
    );
}
