// SPDX-License-Identifier: CC0-1.0

//! What a REFUSAL of this service states about the request that earned it.
//!
//! One rule, asserted over sets of inputs rather than over one recorded request:
//!
//! > a refusal states AWS's sentence for its error class, and every variable part of that
//! > sentence is taken from the request as the request sent it — the value a clause quotes,
//! > whole and in the object form this service's request validator renders a structure in;
//! > the member a clause names, by the path that validator builds; the key an entry of a
//! > substitution table sat under; and the value a caller ADDRESSED a resource by, echoed
//! > rather than re-derived.
//!
//! The suites here pin the five classes of refusal that rule reaches, each over the inputs
//! the rule generalises across — the datatype of a set, the position of a value inside one,
//! the operation that carried it, the element count of a capped list, the actions its
//! elements state, the member a resource was addressed by, and which resource an ARN names:
//!
//! * a `null` listed as a SET member is the parameter-value refusal AWS words for that set
//!   type, not the reader's complaint about the class it found;
//! * a transactional list past the cap the model declares is quoted back in full, at the
//!   member path the request validator builds;
//! * a clause quotes the WHOLE value the request submitted, with no elision of its own;
//! * a not-found refusal echoes the value the request addressed the resource by;
//! * a `*ResourcePolicy` `ResourceArn` is classified by what it names: no DynamoDB resource
//!   at all, a DynamoDB resource a policy does not attach to, or one outside this endpoint.
//!
//! Every call goes over the wire (`POST /` with `X-Amz-Target: DynamoDB_20120810.<Operation>`)
//! through the same `MultiServiceRouter` the emulator's front doors use.

use serde_json::{json, Value};

use crate::emulator::{call, emulator, error_code, message, ok, REGION};
use crate::table_items::create_active_table;

/// The account id every generated emulator presents.
const ACCOUNT: &str = "000000000000";

/// The table these suites address, keyed on one string attribute.
const TABLE: &str = "Echoed";

/// That table's partition-key attribute.
const KEY: &str = "pk";

/// The ARN this endpoint mints for `table`.
fn table_arn(table: &str) -> String {
    format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/{table}")
}

// ===========================================================================
// RULE 1. A `null` listed as a SET member
// ===========================================================================

/// The parameter-value class every rule about a value a request carried is stated in.
const INVALID_PARAMETER: &str = "One or more parameter values were invalid: ";

/// The sentence each set type's `null` member earns, written out here so this suite pins
/// the wording and not the crate's own copy of it. One sentence per set type, as the
/// empty-set clauses of the same class also differ per type.
fn null_member_refusal(datatype: &str) -> String {
    let clause = match datatype {
        "SS" => "An string set may not have a null string as a member",
        "NS" => "Numeric sets may not contain null",
        "BS" => "Binary sets may not contain null or empty values",
        other => panic!("case {other}: the suite states a sentence for SS, NS and BS only"),
    };
    format!("{INVALID_PARAMETER}{clause}")
}

/// A set of `datatype` whose members are the texts `members`, with `null` at `at`.
fn set_with_null(datatype: &str, members: &[&str], at: usize) -> Value {
    let mut listed: Vec<Value> = members.iter().map(|member| json!(member)).collect();
    listed.insert(at.min(listed.len()), Value::Null);
    json!({datatype: listed})
}

#[tokio::test]
async fn a_null_listed_as_a_set_member_is_refused_as_the_parameter_value_it_is() {
    let router = emulator();
    create_active_table(&router, TABLE, &[(KEY, "HASH", "S")]).await;

    // The members of each set type, as the wire carries them: text for `SS`, number text
    // for `NS`, base64 for `BS` (`smithy_query(query="shape",
    // shape="com.amazonaws.dynamodb#AttributeValue")`).
    let members: [(&str, [&str; 2]); 3] = [
        ("SS", ["a", "b"]),
        ("NS", ["1", "2.5"]),
        ("BS", ["YQ==", "Yg=="]),
    ];

    for (datatype, listed) in members {
        // The POSITION of the `null` inside the set, and the position of the SET inside
        // the item: the rule is about a `null` standing where a member stands, wherever
        // that set sits.
        let places: Vec<(String, Value)> = vec![
            (
                format!("a {datatype} whose first member is null"),
                set_with_null(datatype, &listed, 0),
            ),
            (
                format!("a {datatype} whose last member is null"),
                set_with_null(datatype, &listed, listed.len()),
            ),
            (
                format!("a {datatype} that lists nothing but null"),
                set_with_null(datatype, &[], 0),
            ),
            (
                format!("a {datatype} nested in a map"),
                json!({"M": {"inner": set_with_null(datatype, &listed, 1)}}),
            ),
            (
                format!("a {datatype} nested in a list"),
                json!({"L": [{"S": "ok"}, set_with_null(datatype, &listed, 0)]}),
            ),
            (
                format!("a {datatype} nested two levels down"),
                json!({"L": [{"M": {"inner": set_with_null(datatype, &listed, 0)}}]}),
            ),
        ];

        for (case, value) in places {
            // Every surface that carries an item or a value of its own: an item write, an
            // update's substitution table, a batched write and a transactional write.
            let requests: Vec<(&str, Value)> = vec![
                (
                    "PutItem",
                    json!({"TableName": TABLE, "Item": {KEY: {"S": "k"}, "v": value}}),
                ),
                (
                    "UpdateItem",
                    json!({"TableName": TABLE, "Key": {KEY: {"S": "k"}},
                           "UpdateExpression": "SET v = :v",
                           "ExpressionAttributeValues": {":v": value}}),
                ),
                (
                    "BatchWriteItem",
                    json!({"RequestItems": {TABLE: [
                        {"PutRequest": {"Item": {KEY: {"S": "k"}, "v": value}}}
                    ]}}),
                ),
                (
                    "TransactWriteItems",
                    json!({"TransactItems": [
                        {"Put": {"TableName": TABLE, "Item": {KEY: {"S": "k"}, "v": value}}}
                    ]}),
                ),
                (
                    "Query",
                    json!({"TableName": TABLE,
                           "KeyConditionExpression": "pk = :p",
                           "FilterExpression": "v = :v",
                           "ExpressionAttributeValues": {":p": {"S": "k"}, ":v": value}}),
                ),
            ];
            for (operation, body) in requests {
                let (status, answered) = call(&router, operation, body).await;
                assert_eq!(
                    (status, error_code(&answered).as_str()),
                    (400, "ValidationException"),
                    "case {case:?} ({operation}): expected 400 ValidationException, got {status} \
                     with {answered}"
                );
                assert_eq!(
                    message(&answered),
                    null_member_refusal(datatype),
                    "case {case:?} ({operation}): expected {:?}, got {:?}",
                    null_member_refusal(datatype),
                    message(&answered)
                );
            }
        }
    }

    // Nothing was written by any of them: the refusal happens while the request is read,
    // so no item of it reaches the store.
    let read = ok(
        &router,
        "GetItem",
        json!({"TableName": TABLE, "Key": {KEY: {"S": "k"}}}),
    )
    .await;
    assert!(
        read.get("Item").is_none(),
        "case a refused set member: expected nothing stored, got {read}"
    );

    // And a set of the same type with no `null` in it is stored and read back, so the rows
    // above are about the `null` and not about the set.
    for (datatype, listed) in members {
        let value = json!({datatype: listed});
        ok(
            &router,
            "PutItem",
            json!({"TableName": TABLE, "Item": {KEY: {"S": datatype}, "v": value}}),
        )
        .await;
        let read = ok(
            &router,
            "GetItem",
            json!({"TableName": TABLE, "Key": {KEY: {"S": datatype}}}),
        )
        .await;
        assert_eq!(
            read["Item"]["v"], value,
            "case a {datatype} with no null member: expected it stored, got {read}"
        );
    }
}

// ===========================================================================
// RULE 2. A transactional list past the cap the model declares
// ===========================================================================

/// The fields the service's own attribute-value object states, in the order it states
/// them, as the recorded refusals of these members render them — six of them belong to no
/// member this model declares (`smithy_query(query="shape",
/// shape="com.amazonaws.dynamodb#AttributeValue")` gives `S N B SS NS BS M L NULL BOOL`),
/// and they are part of the frame all the same. Written out here so this suite pins the
/// rendering rather than the crate's own copy of it.
const CORAL_FIELDS: [&str; 16] = [
    "s", "n", "b", "iNT", "dECIMAL", "dOUBLE", "sS", "nS", "bS", "iNTSET", "dECIMALSET",
    "dOUBLESET", "m", "l", "nULL", "bOOL",
];

/// One attribute value in that rendering: every field of the frame, with `field` carrying
/// `value` and the rest unset.
fn coral_value(field: &str, value: &str) -> String {
    let fields: Vec<String> = CORAL_FIELDS
        .iter()
        .map(|declared| {
            if *declared == field {
                format!("{declared}={value}")
            } else {
                format!("{declared}=null")
            }
        })
        .collect();
    format!("AttributeValue({})", fields.join(", "))
}

/// One item or key map in that rendering: `{<attribute>=<value>}`.
fn coral_item(attribute: &str, field: &str, value: &str) -> String {
    format!("{{{attribute}={}}}", coral_value(field, value))
}

/// The members every action of a transactional write carries beyond the item or key it
/// addresses and the table it names, in the order the model declares them, and the
/// service's own trailing field after them.
const ACTION_TAIL: &str = "conditionExpression=null, expressionAttributeNames=null, \
                           expressionAttributeValues=null, \
                           returnValuesOnConditionCheckFailure=null, workloadProfileName=null";

/// One `TransactGetItem` of a read transaction, as the refusal quotes it.
pub(crate) fn coral_transact_get(table: &str, key: &str) -> String {
    format!(
        "TransactGetItem(get=Get(key={}, tableName={table}, projectionExpression=null, \
         expressionAttributeNames=null, workloadProfileName=null))",
        coral_item(KEY, "s", key)
    )
}

/// One `TransactWriteItem` carrying a `Put`, as the refusal quotes it.
pub(crate) fn coral_transact_put(table: &str, key: &str) -> String {
    format!(
        "TransactWriteItem(conditionCheck=null, put=Put(item={}, tableName={table}, \
         {ACTION_TAIL}), delete=null, update=null)",
        coral_item(KEY, "s", key)
    )
}

/// One `TransactWriteItem` carrying a `Delete`.
fn coral_transact_delete(table: &str, key: &str) -> String {
    format!(
        "TransactWriteItem(conditionCheck=null, put=null, delete=Delete(key={}, \
         tableName={table}, {ACTION_TAIL}), update=null)",
        coral_item(KEY, "s", key)
    )
}

/// The whole message a transactional list past its cap earns: the envelope over one clause
/// that quotes the list, names the member by the path the request validator builds and
/// states the bound that was broken.
pub(crate) fn quoted_cap_message(elements: &[String], max: usize) -> String {
    format!(
        "1 validation error detected: Value '[{}]' at 'transactItems' failed to satisfy \
         constraint: Member must have length less than or equal to {max}",
        elements.join(", ")
    )
}

/// `count` `Get` entries of that table, each addressing a distinct key.
fn get_entries(count: usize) -> Vec<Value> {
    (0..count)
        .map(|index| json!({"Get": {"TableName": TABLE, "Key": {KEY: {"S": format!("k{index}")}}}}))
        .collect()
}

/// `count` `Put` entries of that table.
fn put_entries(count: usize) -> Vec<Value> {
    (0..count)
        .map(|index| {
            json!({"Put": {"TableName": TABLE, "Item": {KEY: {"S": format!("k{index}")}}}})
        })
        .collect()
}

/// `count` entries alternating `Put` and `Delete`, so the quoting cannot depend on which
/// action an element states.
fn mixed_entries(count: usize) -> Vec<Value> {
    (0..count)
        .map(|index| {
            if index % 2 == 0 {
                json!({"Put": {"TableName": TABLE, "Item": {KEY: {"S": format!("k{index}")}}}})
            } else {
                json!({"Delete": {"TableName": TABLE, "Key": {KEY: {"S": format!("k{index}")}}}})
            }
        })
        .collect()
}

/// The cap the model declares on both transactional lists (`smithy_query(query="shape",
/// shape="com.amazonaws.dynamodb#TransactGetItemList")` and `TransactWriteItemList` are
/// `@length{min:1,max:100}`).
const TRANSACT_CAP: usize = 100;

#[tokio::test]
async fn a_transactional_list_past_its_cap_is_quoted_back_at_the_validator_s_member_path() {
    let router = emulator();
    create_active_table(&router, TABLE, &[(KEY, "HASH", "S")]).await;

    // The rows differ along the operation, the element count (one past the cap and half
    // again as many) and the actions the elements state.
    let rows: Vec<(&str, &str, Value, String)> = vec![
        (
            "a read transaction one past the cap",
            "TransactGetItems",
            json!({"TransactItems": get_entries(TRANSACT_CAP + 1)}),
            quoted_cap_message(
                &(0..TRANSACT_CAP + 1)
                    .map(|index| coral_transact_get(TABLE, &format!("k{index}")))
                    .collect::<Vec<String>>(),
                TRANSACT_CAP,
            ),
        ),
        (
            "a read transaction half again past the cap",
            "TransactGetItems",
            json!({"TransactItems": get_entries(TRANSACT_CAP + TRANSACT_CAP / 2)}),
            quoted_cap_message(
                &(0..TRANSACT_CAP + TRANSACT_CAP / 2)
                    .map(|index| coral_transact_get(TABLE, &format!("k{index}")))
                    .collect::<Vec<String>>(),
                TRANSACT_CAP,
            ),
        ),
        (
            "a write transaction of puts one past the cap",
            "TransactWriteItems",
            json!({"TransactItems": put_entries(TRANSACT_CAP + 1)}),
            quoted_cap_message(
                &(0..TRANSACT_CAP + 1)
                    .map(|index| coral_transact_put(TABLE, &format!("k{index}")))
                    .collect::<Vec<String>>(),
                TRANSACT_CAP,
            ),
        ),
        (
            "a write transaction mixing two actions, past the same cap",
            "TransactWriteItems",
            json!({"TransactItems": mixed_entries(TRANSACT_CAP + 1)}),
            quoted_cap_message(
                &(0..TRANSACT_CAP + 1)
                    .map(|index| {
                        let key = format!("k{index}");
                        if index % 2 == 0 {
                            coral_transact_put(TABLE, &key)
                        } else {
                            coral_transact_delete(TABLE, &key)
                        }
                    })
                    .collect::<Vec<String>>(),
                TRANSACT_CAP,
            ),
        ),
    ];

    for (case, operation, body, expected) in rows {
        let (status, answered) = call(&router, operation, body).await;
        assert_eq!(
            (status, error_code(&answered).as_str()),
            (400, "ValidationException"),
            "case {case:?} ({operation}): expected 400 ValidationException, got {status} with \
             {answered}"
        );
        assert_eq!(
            message(&answered),
            expected,
            "case {case:?} ({operation}): expected the rejected list quoted back, got {:?}",
            message(&answered)
        );
    }

    // AT the cap the list is not refused as a constraint violation at all, which is what
    // makes the rows above about the bound and not an off-by-one.
    for operation in ["TransactGetItems", "TransactWriteItems"] {
        let entries = if operation == "TransactGetItems" {
            get_entries(TRANSACT_CAP)
        } else {
            put_entries(TRANSACT_CAP)
        };
        let (status, answered) = call(&router, operation, json!({"TransactItems": entries})).await;
        assert!(
            !message(&answered).contains("failed to satisfy constraint"),
            "case a list AT the cap ({operation}): expected no constraint clause, got {status} \
             with {answered}"
        );
    }
}

// ===========================================================================
// RULE 3. A clause quotes the WHOLE value the request submitted
// ===========================================================================

/// The ceiling the model declares on a `ResourceArn` (`smithy_query(query="shape",
/// shape="com.amazonaws.dynamodb#ResourceArnString")` gives `@length{min:1,max:1283}`).
const RESOURCE_ARN_CEILING: usize = 1283;

#[tokio::test]
async fn a_clause_quotes_the_whole_value_the_request_submitted() {
    let router = emulator();
    create_active_table(&router, TABLE, &[(KEY, "HASH", "S")]).await;

    // Values past the member's declared ceiling, at the ceiling and well past it: each is
    // quoted back whole, however long it is, and nothing of this emulator's own — an
    // elision marker, a byte count — appears in the clause.
    let prefix = format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/");
    for over in [1usize, 2, 717] {
        let name = "x".repeat(RESOURCE_ARN_CEILING + over - prefix.len());
        let arn = format!("{prefix}{name}");
        assert_eq!(
            arn.len(),
            RESOURCE_ARN_CEILING + over,
            "case {over} past the ceiling: the row builds a value of that length"
        );
        let (status, answered) = call(
            &router,
            "GetResourcePolicy",
            json!({"ResourceArn": &arn}),
        )
        .await;
        let expected = format!(
            "1 validation error detected: Value '{arn}' at 'resourceArn' failed to satisfy \
             constraint: Member must have length less than or equal to {RESOURCE_ARN_CEILING}"
        );
        assert_eq!(
            (status, error_code(&answered).as_str()),
            (400, "ValidationException"),
            "case {over} past the ceiling: expected 400 ValidationException, got {status} with \
             {answered}"
        );
        assert_eq!(
            message(&answered),
            expected,
            "case {over} past the ceiling: expected the whole value quoted, got {:?}",
            message(&answered)
        );
    }

    // At the ceiling the value is not refused for its length at all.
    let name = "x".repeat(RESOURCE_ARN_CEILING - prefix.len());
    let (_, answered) = call(
        &router,
        "GetResourcePolicy",
        json!({"ResourceArn": format!("{prefix}{name}")}),
    )
    .await;
    assert!(
        !message(&answered).contains("failed to satisfy constraint"),
        "case a value AT the ceiling: expected no length clause, got {answered}"
    );
}

// ===========================================================================
// RULE 4. A not-found refusal echoes the value the request addressed by
// ===========================================================================

#[tokio::test]
async fn a_not_found_refusal_echoes_the_value_the_request_addressed_the_resource_by() {
    let router = emulator();
    create_active_table(&router, TABLE, &[(KEY, "HASH", "S")]).await;

    // An ARN-only member carries an ARN, so that is what the refusal echoes — over several
    // spellings of the absent table, including the longest name a table can carry
    // (`[A-Za-z0-9_.-]{3,255}`).
    let absent = [
        "Absent".to_string(),
        "abc".to_string(),
        "a.table-with_every.class-0".to_string(),
        "z".repeat(255),
    ];
    for name in &absent {
        let arn = table_arn(name);
        for (operation, body) in [
            (
                "ExportTableToPointInTime",
                // The idempotency token every export is asked for: what is under test here
                // is the table the ARN names, so the token is one of this suite's own.
                json!({"TableArn": &arn, "S3Bucket": "bucket",
                       "ClientToken": format!("echoed-{name}")}),
            ),
            (
                "RestoreTableToPointInTime",
                json!({"SourceTableArn": &arn, "TargetTableName": "Restored",
                       "UseLatestRestorableTime": true}),
            ),
        ] {
            let (status, answered) = call(&router, operation, body).await;
            assert_eq!(
                (status, error_code(&answered).as_str()),
                (400, "TableNotFoundException"),
                "case the ARN of {name:?} ({operation}): expected 400 TableNotFoundException, \
                 got {status} with {answered}"
            );
            assert_eq!(
                message(&answered),
                format!("Table not found: {arn}"),
                "case the ARN of {name:?} ({operation}): expected the ARN the request sent, got \
                 {:?}",
                message(&answered)
            );
        }
    }

    // A member that carries either documented form of the table names the TABLE the value
    // resolved to, whichever form named it: the refusal is about the table, not the form.
    for name in &absent {
        for (operation, body) in [
            ("CreateBackup", json!({"TableName": name, "BackupName": "copy"})),
            (
                "RestoreTableToPointInTime",
                json!({"SourceTableName": name, "TargetTableName": "Restored",
                       "UseLatestRestorableTime": true}),
            ),
        ] {
            let (status, answered) = call(&router, operation, body).await;
            assert_eq!(
                (status, error_code(&answered).as_str()),
                (400, "TableNotFoundException"),
                "case the name {name:?} ({operation}): expected 400 TableNotFoundException, got \
                 {status} with {answered}"
            );
            assert_eq!(
                message(&answered),
                format!("Table not found: {name}"),
                "case the name {name:?} ({operation}): expected the table named, got {:?}",
                message(&answered)
            );
        }
    }

    // A STREAM ARN addresses the table's stream, so a stream this endpoint does not hold is
    // reported as that stream: by the label the ARN carries and the table it belongs to —
    // for a table that is there and one that is not, since neither holds the stream.
    for table in [TABLE, "Absent"] {
        for label in ["2026-01-01T00:00:00.000", "2020-12-31T23:59:59.999"] {
            let arn = format!("{}/stream/{label}", table_arn(table));
            for operation in [
                "GetResourcePolicy",
                "DeleteResourcePolicy",
            ] {
                let (status, answered) =
                    call(&router, operation, json!({"ResourceArn": &arn})).await;
                assert_eq!(
                    (status, error_code(&answered).as_str()),
                    (400, "ResourceNotFoundException"),
                    "case the stream {label} of {table} ({operation}): expected 400 \
                     ResourceNotFoundException, got {status} with {answered}"
                );
                assert_eq!(
                    message(&answered),
                    format!(
                        "Requested resource not found: Stream: {label} not found for Table: \
                         {table}"
                    ),
                    "case the stream {label} of {table} ({operation}): expected the stream \
                     named, got {:?}",
                    message(&answered)
                );
            }
        }
    }
}

// ===========================================================================
// RULE 5. A `ResourceArn` is classified by what it names
// ===========================================================================

/// A policy document this emulator accepts, so a `PutResourcePolicy` row is refused for
/// its `ResourceArn` and not for its document.
fn policy(resource_arn: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Deny",
            "Principal": {"AWS": "*"},
            "Action": "dynamodb:GetItem",
            "Resource": resource_arn,
        }],
    })
    .to_string()
}

#[tokio::test]
async fn a_resource_policy_arn_is_classified_by_the_resource_it_names() {
    let router = emulator();
    create_active_table(&router, TABLE, &[(KEY, "HASH", "S")]).await;

    // (case, the submitted value, the sentence it earns). The first group is a value no
    // DynamoDB resource can be read from at all; the second names a DynamoDB resource a
    // policy does not attach to — the sub-resources this service mints ARNs for.
    let unreadable = |arn: &str| {
        format!("{INVALID_PARAMETER}Provided Arn is not a DynamoDB resource arn: {arn}")
    };
    let unsupported = |arn: &str| {
        format!(
            "{INVALID_PARAMETER}Invalid resource arn provided, only table or stream is \
             accepted. Provided resource arn: {arn}"
        )
    };
    let out_of_scope = |arn: &str| format!("{INVALID_PARAMETER}Invalid resource Arn: {arn}");

    let rows: Vec<(&str, String, String)> = vec![
        {
            // The class the issue cited: the resource field spelled with a capital, which
            // is not a form this service mints.
            let arn = format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:Table/{TABLE}");
            ("a capitalised resource type", arn.clone(), unreadable(&arn))
        },
        {
            let arn = format!("arn:aws:sqs:{REGION}:{ACCOUNT}:{TABLE}");
            ("another service's resource", arn.clone(), unreadable(&arn))
        },
        {
            let arn = format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:{TABLE}");
            ("no resource type at all", arn.clone(), unreadable(&arn))
        },
        {
            let arn = format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/{TABLE}/widget/1");
            (
                "a sub-resource this service mints none of",
                arn.clone(),
                unreadable(&arn),
            )
        },
        {
            let arn = format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/ab");
            (
                "a table name below the published minimum length",
                arn.clone(),
                unreadable(&arn),
            )
        },
        {
            let arn = format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/{TABLE}/index/by-rank");
            ("an index of the table", arn.clone(), unsupported(&arn))
        },
        {
            let arn =
                format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/{TABLE}/backup/01-abcdef12");
            ("a backup of the table", arn.clone(), unsupported(&arn))
        },
        {
            let arn = format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/{TABLE}/import/01");
            ("an import of the table", arn.clone(), unsupported(&arn))
        },
        {
            let arn = format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/{TABLE}/export/01");
            ("an export of the table", arn.clone(), unsupported(&arn))
        },
        {
            let arn = format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/{TABLE}/stream/");
            (
                "a stream ARN with no label",
                arn.clone(),
                unsupported(&arn),
            )
        },
        {
            let arn = format!("arn:aws:dynamodb:eu-west-1:{ACCOUNT}:table/{TABLE}");
            ("the table's ARN in another region", arn.clone(), out_of_scope(&arn))
        },
    ];

    for (case, arn, expected) in &rows {
        for (operation, body) in [
            ("GetResourcePolicy", json!({"ResourceArn": arn})),
            (
                "PutResourcePolicy",
                json!({"ResourceArn": arn, "Policy": policy(arn)}),
            ),
            ("DeleteResourcePolicy", json!({"ResourceArn": arn})),
        ] {
            let (status, answered) = call(&router, operation, body).await;
            assert_eq!(
                (status, error_code(&answered).as_str()),
                (400, "ValidationException"),
                "case {case:?} ({operation}): expected 400 ValidationException, got {status} \
                 with {answered}"
            );
            assert_eq!(
                &message(&answered),
                expected,
                "case {case:?} ({operation}): expected {expected:?}, got {:?}",
                message(&answered)
            );
        }
    }
}

// ===========================================================================
// RULE 6. An expression-value entry that states no datatype
// ===========================================================================

/// The reader's own sentence for a value that states no datatype.
const EMPTY_VALUE: &str =
    "Supplied AttributeValue is empty, must contain exactly one of the supported datatypes";

#[tokio::test]
async fn an_expression_value_that_states_no_datatype_names_the_member_and_the_key() {
    let router = emulator();
    create_active_table(&router, TABLE, &[(KEY, "HASH", "S")]).await;

    // The rows differ along WHERE inside the entry the value that states no datatype sits
    // and along the KEY the entry sat under, which the refusal takes from the request.
    let shapes: Vec<(&str, Value)> = vec![
        ("an entry stating no datatype at all", json!({})),
        ("an entry whose datatype is null", json!({"S": null})),
        ("an empty value inside a list", json!({"L": [{"S": "ok"}, {}]})),
        ("an empty value inside a map", json!({"M": {"inner": {}}})),
        (
            "an empty value two levels down",
            json!({"L": [{"M": {"inner": {}}}]}),
        ),
    ];

    for (case, value) in shapes {
        for key in [":v", ":zz"] {
            let requests: Vec<(&str, Value)> = vec![
                (
                    "PutItem",
                    json!({"TableName": TABLE, "Item": {KEY: {"S": "k"}},
                           "ConditionExpression": format!("v = {key}"),
                           "ExpressionAttributeValues": {key: value}}),
                ),
                (
                    "UpdateItem",
                    json!({"TableName": TABLE, "Key": {KEY: {"S": "k"}},
                           "UpdateExpression": format!("SET v = {key}"),
                           "ExpressionAttributeValues": {key: value}}),
                ),
                (
                    "DeleteItem",
                    json!({"TableName": TABLE, "Key": {KEY: {"S": "k"}},
                           "ConditionExpression": format!("v = {key}"),
                           "ExpressionAttributeValues": {key: value}}),
                ),
                (
                    "Query",
                    json!({"TableName": TABLE,
                           "KeyConditionExpression": format!("pk = {key}"),
                           "ExpressionAttributeValues": {key: value}}),
                ),
                (
                    "Scan",
                    json!({"TableName": TABLE,
                           "FilterExpression": format!("v = {key}"),
                           "ExpressionAttributeValues": {key: value}}),
                ),
            ];
            for (operation, body) in requests {
                let (status, answered) = call(&router, operation, body).await;
                let expected = format!(
                    "ExpressionAttributeValues contains invalid value: {EMPTY_VALUE} for key {key}"
                );
                assert_eq!(
                    (status, error_code(&answered).as_str()),
                    (400, "ValidationException"),
                    "case {case:?} under {key} ({operation}): expected 400 ValidationException, \
                     got {status} with {answered}"
                );
                assert_eq!(
                    message(&answered),
                    expected,
                    "case {case:?} under {key} ({operation}): expected {expected:?}, got {:?}",
                    message(&answered)
                );
            }
        }
    }

    // An entry that DOES state a datatype is served, so the rows above are about the empty
    // value and not about the table.
    ok(
        &router,
        "PutItem",
        json!({"TableName": TABLE, "Item": {KEY: {"S": "stored"}, "v": {"S": "text"}}}),
    )
    .await;
    let read = ok(
        &router,
        "Query",
        json!({"TableName": TABLE,
               "KeyConditionExpression": "pk = :p",
               "ExpressionAttributeValues": {":p": {"S": "stored"}}}),
    )
    .await;
    assert_eq!(
        read["Items"][0]["v"],
        json!({"S": "text"}),
        "case an entry stating a datatype: expected the item served, got {read}"
    );
}
