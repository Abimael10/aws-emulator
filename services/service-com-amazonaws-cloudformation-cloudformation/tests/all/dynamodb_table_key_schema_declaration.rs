// SPDX-License-Identifier: CC0-1.0

//! The one rule the `AWS::DynamoDB::Table` resource handler holds a declaration's two key
//! properties to, and the diagnosis it refuses a declaration that breaks it with.
//!
//! RULE under test: **every attribute a `KeySchema` element of the declaration names — the
//! table's own key schema and each secondary index's — has to be declared in
//! `AttributeDefinitions`; the handler holds the declaration to that rule itself, before the
//! owning service is called at all, so the resource fails with the handler's own sentence
//! stating the rule (`An attribute referenced in a KeySchema element is not defined in
//! AttributeDefinitions`, entirely fixed text, naming neither the attribute nor the index)
//! under `HandlerErrorCode: InvalidRequest`, and no table is created.**
//!
//! Why it matters: the `ResourceStatusReason` is the only thing that tells a template defect
//! from a throttle, a quota or a permission failure, all of which also end in a rolled-back
//! stack. A client (or CI gate) matching on the rule the handler states cannot classify the
//! failure when what it is handed instead is Amazon DynamoDB's own `CreateTable` validation
//! string, which is worded about an INDEX — a rule a template declaring no index did not
//! break.
//!
//! The rows differ along what the rule generalises over: WHICH key schema names the
//! undeclared attribute (the table's partition key, its sort key, a global secondary index's,
//! a local secondary index's) and how many definitions the declaration does carry; the last
//! row is the well-formed counterpart, which has to be created rather than refused, so the
//! check cannot pass by refusing everything.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::dynamodb_table_resources::describe_table;
use crate::stack_lifecycle::{
    call, create_stack, member, resource_event_reason, router, xml_text,
};
use service_com_amazonaws_cloudformation_cloudformation::state::{resource_status, status};

/// The logical id every row declares its table under.
const TABLE: &str = "Table";

/// How often the suite asks the stack what became of it, and how long it goes on asking.
///
/// The bound is a WALL-CLOCK deadline rather than a count of reads, and generous: what is
/// waited for is the stack's own clock-owed conclusion and, for the well-formed row, the
/// window DynamoDB creates a table in. All it is for is that the wait TERMINATES.
const POLL: Duration = Duration::from_millis(25);
const DEADLINE: Duration = Duration::from_secs(60);

/// The handler's own sentence for the rule, as the reason quotes it. Entirely fixed text: it
/// names neither the attribute nor the index.
const STATES_THE_RULE: &str =
    "An attribute referenced in a KeySchema element is not defined in AttributeDefinitions";

/// The class a declaration the handler found the fault in is reported under.
const INVALID_REQUEST: &str = "HandlerErrorCode: InvalidRequest";

/// The index-oriented sentence DynamoDB words its OWN `CreateTable` refusal with, which the
/// reason must not be: it states a rule about an index a template declaring none does not
/// have.
const DYNAMODBS_OWN_SENTENCE: &str = "Invalid KeySchema";

/// One attribute definition.
fn definition(name: &str, kind: &str) -> Value {
    json!({"AttributeName": name, "AttributeType": kind})
}

/// One key schema element.
fn key(name: &str, kind: &str) -> Value {
    json!({"AttributeName": name, "KeyType": kind})
}

/// One row: the stack and table names it deploys under, the table properties it declares, and
/// whether the handler must refuse the declaration.
struct Row {
    case: &'static str,
    stack: &'static str,
    table: &'static str,
    properties: Value,
    refused: bool,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the table's partition key is not defined",
            stack: "ddb-key-schema-hash",
            table: "ddb-key-schema-hash-table",
            properties: json!({
                "KeySchema": [key("sk", "HASH")],
                "AttributeDefinitions": [definition("pk", "S")],
                "BillingMode": "PAY_PER_REQUEST",
            }),
            refused: true,
        },
        Row {
            case: "the table's sort key is not defined while its partition key is",
            stack: "ddb-key-schema-range",
            table: "ddb-key-schema-range-table",
            properties: json!({
                "KeySchema": [key("pk", "HASH"), key("sk", "RANGE")],
                "AttributeDefinitions": [definition("pk", "S")],
                "BillingMode": "PAY_PER_REQUEST",
            }),
            refused: true,
        },
        Row {
            case: "a global secondary index's key is not defined",
            stack: "ddb-key-schema-gsi",
            table: "ddb-key-schema-gsi-table",
            properties: json!({
                "KeySchema": [key("pk", "HASH")],
                "AttributeDefinitions": [definition("pk", "S")],
                "BillingMode": "PAY_PER_REQUEST",
                "GlobalSecondaryIndexes": [{
                    "IndexName": "by-status",
                    "KeySchema": [key("status", "HASH")],
                    "Projection": {"ProjectionType": "ALL"},
                }],
            }),
            refused: true,
        },
        Row {
            case: "a local secondary index's sort key is not defined",
            stack: "ddb-key-schema-lsi",
            table: "ddb-key-schema-lsi-table",
            properties: json!({
                "KeySchema": [key("pk", "HASH"), key("sk", "RANGE")],
                "AttributeDefinitions": [definition("pk", "S"), definition("sk", "N")],
                "BillingMode": "PAY_PER_REQUEST",
                "LocalSecondaryIndexes": [{
                    "IndexName": "by-created",
                    "KeySchema": [key("pk", "HASH"), key("created", "RANGE")],
                    "Projection": {"ProjectionType": "KEYS_ONLY"},
                }],
            }),
            refused: true,
        },
        Row {
            // The well-formed counterpart: every key the declaration names, the table's own
            // and its index's, IS defined — so the handler holds nothing back and the table
            // is created.
            case: "every key the declaration names is defined",
            stack: "ddb-key-schema-complete",
            table: "ddb-key-schema-complete-table",
            properties: json!({
                "KeySchema": [key("pk", "HASH"), key("sk", "RANGE")],
                "AttributeDefinitions": [
                    definition("pk", "S"),
                    definition("sk", "N"),
                    definition("status", "S"),
                ],
                "BillingMode": "PAY_PER_REQUEST",
                "GlobalSecondaryIndexes": [{
                    "IndexName": "by-status",
                    "KeySchema": [key("status", "HASH")],
                    "Projection": {"ProjectionType": "ALL"},
                }],
            }),
            refused: false,
        },
    ]
}

/// The template one row deploys.
fn template(table: &str, properties: &Value) -> String {
    let mut declared = properties.clone();
    declared
        .as_object_mut()
        .expect("a row's properties are an object")
        .insert("TableName".to_string(), json!(table));
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {TABLE: {"Type": "AWS::DynamoDB::Table", "Properties": declared}},
    })
    .to_string()
}

/// Poll `DescribeStacks` until the stack reports one of `wanted`, and answer the status it
/// reported with the answer it was read from. The wait is on that CONDITION — the stack's own
/// conclusion is owed from the instant the deployment is done, which no fixed pause names.
async fn wait_for_stack_status(
    router: &MultiServiceRouter,
    stack: &str,
    wanted: &[&str],
) -> (String, String) {
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let (code, body) = call(router, "DescribeStacks", &[member("StackName", stack)]).await;
        assert_eq!(
            code, 200,
            "DescribeStacks {stack:?}: expected HTTP 200, got {code} with body {body}"
        );
        let reported = xml_text(&body, "StackStatus").unwrap_or_default();
        if wanted.contains(&reported.as_str()) || Instant::now() >= giving_up_at {
            return (reported, body);
        }
        tokio::time::sleep(POLL).await;
    }
}

#[tokio::test]
async fn an_undefined_key_attribute_fails_the_table_with_the_handlers_own_rule() {
    for row in rows() {
        let router = router();
        // "DO_NOTHING — if the stack creation fails, do nothing": the failed state and the
        // resource event carrying the diagnosis are what the assertions below read.
        let (code, body) = create_stack(
            &router,
            row.stack,
            &template(row.table, &row.properties),
            &[member("OnFailure", "DO_NOTHING")],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {:?}: CreateStack must be accepted, got {code} {body}",
            row.case
        );

        if !row.refused {
            let (reported, described) =
                wait_for_stack_status(&router, row.stack, &[status::CREATE_COMPLETE]).await;
            assert_eq!(
                reported,
                status::CREATE_COMPLETE,
                "case {:?}: a declaration that defines every key it names must deploy, got \
                 {described}",
                row.case
            );
            let table = describe_table(&router, row.table).await;
            assert!(
                table.is_some(),
                "case {:?}: expected DynamoDB to hold the table {:?} the completed stack \
                 published, got nothing",
                row.case,
                row.table
            );
            continue;
        }

        let (reported, described) = wait_for_stack_status(
            &router,
            row.stack,
            &[status::CREATE_FAILED, status::ROLLBACK_COMPLETE],
        )
        .await;
        assert_eq!(
            reported,
            status::CREATE_FAILED,
            "case {:?}: a declaration whose key schema names an undefined attribute must fail \
             the resource, got {described}",
            row.case
        );

        // The diagnosis the resource's own event carries: the handler's sentence stating the
        // rule, under the class a declaration the handler found the fault in falls in — and
        // never the owning service's index-oriented wording.
        let reason =
            resource_event_reason(&router, row.stack, TABLE, resource_status::CREATE_FAILED).await;
        assert!(
            reason.contains(STATES_THE_RULE),
            "case {:?}: expected the reason to state the rule as {STATES_THE_RULE:?}, got \
             {reason:?}",
            row.case
        );
        assert!(
            reason.contains(INVALID_REQUEST),
            "case {:?}: expected the reason to classify the failure as {INVALID_REQUEST:?}, got \
             {reason:?}",
            row.case
        );
        assert!(
            !reason.contains(DYNAMODBS_OWN_SENTENCE),
            "case {:?}: expected the reason not to carry the owning service's own \
             {DYNAMODBS_OWN_SENTENCE:?} wording, got {reason:?}",
            row.case
        );

        // ...and nothing was created: the handler refused the declaration before it called
        // DynamoDB at all, so that service holds no table of the declared name.
        let table = describe_table(&router, row.table).await;
        assert!(
            table.is_none(),
            "case {:?}: expected DynamoDB to hold no table {:?} for a declaration the handler \
             refused, got {table:?}",
            row.case,
            row.table
        );
    }
}
