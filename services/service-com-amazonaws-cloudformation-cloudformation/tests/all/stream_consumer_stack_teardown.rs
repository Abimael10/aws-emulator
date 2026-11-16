// SPDX-License-Identifier: CC0-1.0

//! The DynamoDB Streams estate a CDK-shaped stack deploys: a table whose declaration asks
//! for a stream, the function the same template wires to that stream, and the second table
//! the function copies every insert into.
//!
//! RULE: a resource type's property whose shape differs from the owning API's member of the
//! same name is TRANSLATED into that member before the call rather than passed through — an
//! `AWS::DynamoDB::Table`'s `StreamSpecification` states what the stream CAPTURES and
//! declaring the property is what asks for a stream, while the API's own specification
//! carries a flag its shape REQUIRES beside the view type
//! (`smithy_query(query="shape",shape="StreamSpecification")`: `StreamEnabled` is required) —
//! so the stream a declaration asks for really exists, really carries the table's item
//! changes, and is really polled by the event source mapping the same template declares over
//! `Fn::GetAtt … StreamArn`.
//!
//! `dynamodb_table_stream_views.rs` holds the same rule for the IDENTIFIERS such a table
//! publishes (the minted ARN behind every view type, and what a change to an enabled stream
//! does). This suite is the rule's END-TO-END consequence, which no other suite of this
//! crate drives: the records of the declared stream reaching the declared consumer, the
//! consumer's own writes landing in the second table of the same template, and the whole
//! estate — two tables, a role, the inline policy its grants produce, the function and the
//! mapping — being taken down again by the stack's deletion.
//!
//! The rows differ along what the rule generalises over: which `StreamViewType` the
//! declaration asks for (so the stream the mapping polls is a different one per row), and
//! whether the declaration spells the API's flag itself or leaves it to the provider.
//!
//! The TEARDOWN half of the suite pins the two rules a stack's deletion of such an estate
//! stands on:
//!
//! * a resource is reported `DELETE_COMPLETE` only once the service that OWNS it no longer
//!   holds it — `DeleteTable` is accepted, not carried out ("DynamoDB immediately returns a
//!   response with a `TableStatus` of `DELETING`"), so a client that reads the table the
//!   instant the stack says the resource is gone is refused by DynamoDB;
//! * the deletion of a resource removes what that resource still HOLDS first, whatever put
//!   it there — IAM will not delete an identity while a policy is embedded in or attached to
//!   it, and a document a client embedded itself holds a stack's role exactly as a declared
//!   one does.
//!
//! Every case drives the real request path for all four services through one router: the
//! CloudFormation calls over the awsQuery wire exactly as the CDK CLI makes them (a `CREATE`
//! change set, then `ExecuteChangeSet`), and the DynamoDB, Lambda and IAM reads as a client's
//! SDK makes them.

use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::change_set_lifecycle::{create_change_set, execute_change_set};
use crate::dynamodb_table_resources::{describe_table, dynamodb};
use crate::hotswapped_function_code::lambda;
use crate::stack_lifecycle::{
    acknowledging_iam, call, describe_stack, iam, member, output_value, router,
    wait_for_the_transition, xml_text, REGION,
};

/// The logical ids the CDK gives the constructs of an app that wires a streamed table to a
/// function: two `ddb.Table`s, a `_lambda.Function` with the role and inline policy its
/// grants produce, and the mapping `fn.addEventSource(new DynamoEventSource(source))` adds.
const SOURCE: &str = "SourceTableCD117FA1";
const SINK: &str = "SinkTableE9BC2A1F";
const ROLE: &str = "ConsumerServiceRole7D7F0988";
const POLICY: &str = "ConsumerServiceRoleDefaultPolicyA2BD6A50";
const FUNCTION: &str = "Consumer9270CBC0";
const MAPPING: &str = "ConsumerDynamoDBEventSourceSourceTable0B4A4D3E";

/// The partition key every table below is declared with.
const PARTITION_KEY: &str = "pk";

/// How long one invocation of the consumer is given, as the function's own `Timeout`: well
/// past what a run needs, because what is under test is the stream the declaration asked for
/// and not how fast an interpreter gets through a handler.
const FUNCTION_TIMEOUT: u64 = 30;

/// The consumer the app deploys: for every record the mapping delivers it copies the key of
/// the changed item into the second table, which is a write its role is granted and which
/// works whatever the stream captures — a `KEYS_ONLY` record carries the keys and nothing
/// else.
const COPY_INSERTS: &str = r#"
import os

import boto3

CLIENT = boto3.client("dynamodb")


def handler(event, context):
    copied = 0
    for record in event["Records"]:
        key = record["dynamodb"]["Keys"]["pk"]["S"]
        CLIENT.put_item(
            TableName=os.environ["SINK"],
            Item={"pk": {"S": key}, "event": {"S": record["eventName"]}},
        )
        copied += 1
    return {"copied": copied}
"#;

/// The actions the CDK's `table.grantStreamRead(fn)` attaches to a consumer's role, as the
/// synthesized inline policy carries them.
const GRANT_STREAM_READ: &[&str] = &[
    "dynamodb:DescribeStream",
    "dynamodb:GetRecords",
    "dynamodb:GetShardIterator",
    "dynamodb:ListStreams",
];

/// The actions `table.grantWriteData(fn)` attaches, which is what lets the consumer write
/// what it read into the second table.
const GRANT_WRITE_DATA: &[&str] = &[
    "dynamodb:BatchWriteItem",
    "dynamodb:PutItem",
    "dynamodb:UpdateItem",
    "dynamodb:DeleteItem",
    "dynamodb:DescribeTable",
];

/// How often a case looks again at something an owning service settles on its own, and how
/// long it goes on looking. What every wait here is for is a CONDITION — a stack status that
/// is not an operation in flight, a record the mapping has delivered, a table DynamoDB has
/// finished removing — never a span, and the budget is far longer than the spans this
/// emulator gives those transitions so a loaded machine cannot fail a row.
const LOOK_AGAIN: Duration = Duration::from_millis(50);
const LOOKS: usize = 600;

/// One row: the stream one deployed estate declares, and how the declaration spells it.
struct Row {
    case: &'static str,
    /// The stack the row deploys, and the prefix of every name in it.
    stack: &'static str,
    /// The `StreamViewType` the declaration asks for, which is what the stream captures.
    view_type: &'static str,
    /// Whether the declaration states the API's own `StreamEnabled` flag itself. The type
    /// publishes no such property, so a template written against it states the view type
    /// alone and the provider supplies the flag; one written against the API's spelling
    /// states both, and asks for the same stream.
    declared_flag: bool,
    /// Whether the inline policy the template declares grants the consumer the writes it
    /// makes into the second table. A row that does NOT is the estate whose consumer keeps
    /// being refused by DynamoDB, so its mapping goes on retrying the batch it was delivered.
    grants_the_sink: bool,
}

/// The rows: one per `StreamViewType` the model declares
/// (`smithy_query(query="shape",shape="StreamViewType")`), in the order its enum states them,
/// with the last also spelling the API's flag — so the rule is not read off one view type or
/// one spelling. Every row is a whole estate, so the set is the smallest one that spans what
/// the rule generalises over.
const ROWS: &[Row] = &[
    Row {
        case: "the images before and after the change, the type's own spelling",
        stack: "cdk-stream-consumer-both-images",
        view_type: "NEW_AND_OLD_IMAGES",
        declared_flag: false,
        grants_the_sink: true,
    },
    Row {
        case: "the keys alone, the type's own spelling",
        stack: "cdk-stream-consumer-keys-only",
        view_type: "KEYS_ONLY",
        declared_flag: false,
        grants_the_sink: true,
    },
    Row {
        case: "the image after the change, the API's own spelling",
        stack: "cdk-stream-consumer-new-image",
        view_type: "NEW_IMAGE",
        declared_flag: true,
        grants_the_sink: true,
    },
];

/// The same estate with the grant for the second table LEFT OUT: the consumer the stream
/// carries records to is refused its own write, so the mapping goes on retrying the batch —
/// which is the state the stack is deleted in by the teardown row below.
const UNGRANTED: Row = Row {
    case: "an estate whose consumer is not granted the second table",
    stack: "cdk-stream-consumer-missing-grant",
    view_type: "NEW_AND_OLD_IMAGES",
    declared_flag: false,
    grants_the_sink: false,
};

/// The `StreamSpecification` one row declares.
fn declared_specification(row: &Row) -> Value {
    let mut specification = json!({"StreamViewType": row.view_type});
    if row.declared_flag {
        specification
            .as_object_mut()
            .expect("the specification is an object")
            .insert("StreamEnabled".to_string(), json!(true));
    }
    specification
}

/// The statements the inline policy one row declares carries: the stream read the mapping's
/// own poller needs, and — for a row that grants it — the writes the consumer makes into the
/// second table.
fn granted_statements(row: &Row) -> Value {
    let mut statements = vec![json!({
        "Action": GRANT_STREAM_READ,
        "Effect": "Allow",
        "Resource": [{"Fn::GetAtt": [SOURCE, "StreamArn"]}],
    })];
    if row.grants_the_sink {
        statements.push(json!({
            "Action": GRANT_WRITE_DATA,
            "Effect": "Allow",
            "Resource": [{"Fn::GetAtt": [SINK, "Arn"]}],
        }));
    }
    Value::Array(statements)
}

/// The template one row deploys: the two tables, the role and the inline policy the grants
/// produce, the consumer, and the mapping over the stream the source table's declaration asks
/// for — plus the outputs a client reads the estate through.
fn template(row: &Row) -> String {
    json!({
        "Resources": {
            SOURCE: {
                "Type": "AWS::DynamoDB::Table",
                "Properties": {
                    "KeySchema": [{"AttributeName": PARTITION_KEY, "KeyType": "HASH"}],
                    "AttributeDefinitions": [
                        {"AttributeName": PARTITION_KEY, "AttributeType": "S"},
                    ],
                    "BillingMode": "PAY_PER_REQUEST",
                    "StreamSpecification": declared_specification(row),
                },
                "UpdateReplacePolicy": "Delete",
                "DeletionPolicy": "Delete",
                "Metadata": {"aws:cdk:path": format!("{}/Source/Resource", row.stack)},
            },
            SINK: {
                "Type": "AWS::DynamoDB::Table",
                "Properties": {
                    "KeySchema": [{"AttributeName": PARTITION_KEY, "KeyType": "HASH"}],
                    "AttributeDefinitions": [
                        {"AttributeName": PARTITION_KEY, "AttributeType": "S"},
                    ],
                    "BillingMode": "PAY_PER_REQUEST",
                },
                "UpdateReplacePolicy": "Delete",
                "DeletionPolicy": "Delete",
                "Metadata": {"aws:cdk:path": format!("{}/Sink/Resource", row.stack)},
            },
            ROLE: {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Action": "sts:AssumeRole",
                            "Effect": "Allow",
                            "Principal": {"Service": "lambda.amazonaws.com"},
                        }],
                    },
                    // The managed policy the CDK attaches to every function's role, in the
                    // form it synthesizes it in.
                    "ManagedPolicyArns": [
                        {"Fn::Join": ["", [
                            "arn:",
                            {"Ref": "AWS::Partition"},
                            ":iam::aws:policy/service-role/AWSLambdaBasicExecutionRole",
                        ]]},
                    ],
                },
            },
            POLICY: {
                "Type": "AWS::IAM::Policy",
                "Properties": {
                    "PolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": granted_statements(row),
                    },
                    "PolicyName": POLICY,
                    "Roles": [{"Ref": ROLE}],
                },
            },
            FUNCTION: {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "Code": {"ZipFile": COPY_INSERTS},
                    "Environment": {"Variables": {"SINK": {"Ref": SINK}}},
                    "Handler": "index.handler",
                    "Role": {"Fn::GetAtt": [ROLE, "Arn"]},
                    "Runtime": "python3.12",
                    "Timeout": FUNCTION_TIMEOUT,
                },
                "DependsOn": [POLICY, ROLE],
            },
            MAPPING: {
                "Type": "AWS::Lambda::EventSourceMapping",
                "Properties": {
                    "BatchSize": 1,
                    "EventSourceArn": {"Fn::GetAtt": [SOURCE, "StreamArn"]},
                    "FunctionName": {"Ref": FUNCTION},
                    "StartingPosition": "TRIM_HORIZON",
                },
            },
        },
        "Outputs": {
            "SourceName": {"Value": {"Ref": SOURCE}},
            "SinkName": {"Value": {"Ref": SINK}},
            "RoleName": {"Value": {"Ref": ROLE}},
            "FunctionName": {"Value": {"Ref": FUNCTION}},
            "MappingId": {"Value": {"Ref": MAPPING}},
            "StreamArn": {"Value": {"Fn::GetAtt": [SOURCE, "StreamArn"]}},
        },
    })
    .to_string()
}

/// Poll `DescribeStacks` until the stack reports a status no operation is in flight in, and
/// answer the body it was read from — the empty string once this service stops reporting the
/// stack at all, which is what a completed deletion can leave behind.
async fn settled(case: &str, router: &MultiServiceRouter, stack: &str) -> String {
    for _ in 0..LOOKS {
        let (status_code, body) = describe_stack(router, stack).await;
        if status_code != 200 {
            return String::new();
        }
        let reported = xml_text(&body, "StackStatus").unwrap_or_default();
        if !reported.is_empty() && !reported.ends_with("_IN_PROGRESS") {
            return body;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    panic!("case {case:?}: expected {stack} to settle, it never did");
}

/// One output of the settled stack, which every row's assertions are computed from.
fn output(case: &str, described: &str, key: &str) -> String {
    output_value(described, key)
        .unwrap_or_else(|| panic!("case {case:?}: the stack published no {key} in {described}"))
}

/// Deploy one row the way the CDK CLI deploys a stack — a `CREATE` change set over the
/// synthesized template, then `ExecuteChangeSet` — and answer the settled stack's
/// `DescribeStacks` body.
async fn deploy(case: &str, router: &MultiServiceRouter, row: &Row) -> String {
    let change_set = format!("{}-changeset", row.stack);
    let (status_code, body) = create_change_set(
        router,
        row.stack,
        &change_set,
        &acknowledging_iam(&[
            member("ChangeSetType", "CREATE"),
            member("TemplateBody", &template(row)),
        ]),
    )
    .await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: expected HTTP 200 from CreateChangeSet, got {status_code} {body}"
    );
    wait_for_the_transition().await;
    let (status_code, body) = execute_change_set(router, &change_set, Some(row.stack)).await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: expected HTTP 200 from ExecuteChangeSet, got {status_code} {body}"
    );
    let described = settled(case, router, row.stack).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the deployment to complete, got {described}"
    );
    described
}

/// The mapping AWS Lambda holds under the id the stack published for it, or `None` where that
/// service holds none.
async fn reported_mapping(router: &MultiServiceRouter, uuid: &str) -> Option<Value> {
    let answer = lambda(
        router,
        "GET",
        &format!(
            "https://lambda.{REGION}.amazonaws.com/2015-03-31/event-source-mappings/{uuid}"
        ),
        &[],
        &[],
    )
    .await;
    (answer.status == 200).then_some(answer.body)
}

/// The item the sink table holds under `key`, read with a consistent read so a write the
/// consumer has made is visible, or `None` where it holds none.
async fn sink_item(router: &MultiServiceRouter, sink: &str, key: &str) -> Option<Value> {
    let (status_code, answered) = dynamodb(
        router,
        "GetItem",
        &json!({
            "TableName": sink,
            "Key": {PARTITION_KEY: {"S": key}},
            "ConsistentRead": true,
        }),
    )
    .await;
    if status_code != 200 {
        return None;
    }
    answered.get("Item").filter(|item| !item.is_null()).cloned()
}

#[tokio::test]
async fn a_declared_stream_carries_inserts_to_the_consumer_the_template_wires_to_it() {
    for row in ROWS {
        let case = row.case;
        let router = router();
        let described = deploy(case, &router, row).await;
        let source = output(case, &described, "SourceName");
        let sink = output(case, &described, "SinkName");
        let published_arn = output(case, &described, "StreamArn");
        let mapping_id = output(case, &described, "MappingId");

        // The table the declaration asked for is held by DynamoDB WITH its stream: the flag
        // the API's shape requires is the one the declaration means, and the view type is
        // the one it asked for.
        let table = describe_table(&router, &source)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: DynamoDB holds no table {source}"));
        assert_eq!(
            table.get("StreamSpecification"),
            Some(&json!({"StreamEnabled": true, "StreamViewType": row.view_type})),
            "case {case:?}: expected the declared stream to be enabled capturing {:?}, got {table}",
            row.view_type
        );
        let minted_arn = table
            .get("LatestStreamArn")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert_eq!(
            minted_arn, published_arn,
            "case {case:?}: expected the stack's StreamArn output to carry the ARN DynamoDB \
             minted, got {published_arn:?} against {minted_arn:?}"
        );

        // ...and the mapping the template declared over that ARN is held by Lambda, polling
        // exactly the stream the declaration enabled.
        let mapping = reported_mapping(&router, &mapping_id)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: Lambda holds no mapping {mapping_id}"));
        assert_eq!(
            mapping.get("EventSourceArn").and_then(Value::as_str),
            Some(published_arn.as_str()),
            "case {case:?}: expected the mapping to poll the declared stream, got {mapping}"
        );

        // The stream really carries the table's item changes to the consumer, whose own
        // write lands in the second table of the same template: one insert, one copy.
        let key = format!("{}-insert", row.view_type);
        let (status_code, body) = dynamodb(
            &router,
            "PutItem",
            &json!({
                "TableName": source,
                "Item": {PARTITION_KEY: {"S": key}, "marker": {"S": "copied"}},
            }),
        )
        .await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: expected the insert to be accepted, got {status_code} {body}"
        );
        let mut copied = None;
        let mut mapping_state = Value::Null;
        for _ in 0..LOOKS {
            copied = sink_item(&router, &sink, &key).await;
            if copied.is_some() {
                break;
            }
            // The read of the mapping drives this service's own clock exactly as a client's
            // poll of it does, and is what the failure below reports the mapping's state from.
            mapping_state = reported_mapping(&router, &mapping_id)
                .await
                .unwrap_or(Value::Null);
            tokio::time::sleep(LOOK_AGAIN).await;
        }
        assert_eq!(
            copied,
            Some(json!({
                PARTITION_KEY: {"S": key.clone()},
                "event": {"S": "INSERT"},
            })),
            "case {case:?}: expected the consumer to copy the insert the declared stream \
             carried into {sink}, got {copied:?} with the mapping reported as {mapping_state}"
        );
    }
}

#[tokio::test]
async fn deleting_the_stack_takes_the_whole_declared_estate_away() {
    // ONE row's estate is enough for the teardown — what it is about is the stack's own
    // deletion, which is the same walk whatever the stream captures — so the row is the one
    // whose declaration leaves the flag to the provider.
    let row = &ROWS[0];
    let case = row.case;
    let router = router();
    let described = deploy(case, &router, row).await;
    let source = output(case, &described, "SourceName");
    let sink = output(case, &described, "SinkName");
    let role = output(case, &described, "RoleName");
    let function = output(case, &described, "FunctionName");
    let mapping_id = output(case, &described, "MappingId");

    let (status_code, body) = call(&router, "DeleteStack", &[member("StackName", row.stack)]).await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: expected HTTP 200 from DeleteStack, got {status_code} {body}"
    );
    let (_, described) = describe_stack(&router, row.stack).await;
    let stack_id = xml_text(&described, "StackId").unwrap_or_default();
    let described = settled(case, &router, &stack_id).await;
    let (_, events) = call(
        &router,
        "DescribeStackEvents",
        &[member("StackName", &stack_id)],
    )
    .await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some("DELETE_COMPLETE"),
        "case {case:?}: expected the teardown to reach DELETE_COMPLETE, got {described} with \
         events {events}"
    );

    // Every owning service has let the stack's resources go, as of the instant the stack
    // reported the deletion complete: a resource CloudFormation reports `DELETE_COMPLETE` is
    // one the service that owns it no longer holds, so the reads below take no second look.
    for table in [&source, &sink] {
        let held = describe_table(&router, table).await;
        assert!(
            held.is_none(),
            "case {case:?}: expected DynamoDB to hold no table {table} once the stack reports \
             the deletion complete, got {held:?}"
        );
    }
    assert!(
        reported_mapping(&router, &mapping_id).await.is_none(),
        "case {case:?}: expected Lambda to hold no mapping {mapping_id} once the stack is \
         deleted"
    );
    let answer = lambda(
        &router,
        "GET",
        &format!("https://lambda.{REGION}.amazonaws.com/2015-03-31/functions/{function}"),
        &[],
        &[],
    )
    .await;
    assert_eq!(
        answer.status, 404,
        "case {case:?}: expected Lambda to hold no function {function}, got {} {:?}",
        answer.status, answer.body
    );
    let (status_code, body) = iam(&router, "GetRole", &[("RoleName", role.as_str())]).await;
    assert_eq!(
        status_code, 404,
        "case {case:?}: expected IAM to hold no role {role} once the stack is deleted, got \
         {status_code} {body}"
    );
}

/// A stack whose consumer keeps FAILING is deleted exactly as one whose consumer works: the
/// mapping the template declared is still retrying the batch the stream delivered when the
/// deletion starts, and the teardown takes the whole estate down all the same.
///
/// A resource an owning service would not delete in that state would leave the stack in
/// `DELETE_FAILED`, naming it as the resource it could not delete.
#[tokio::test]
async fn a_stack_whose_consumer_keeps_failing_is_still_torn_down() {
    let row = &UNGRANTED;
    let case = row.case;
    let router = router();
    let described = deploy(case, &router, row).await;
    let source = output(case, &described, "SourceName");
    let sink = output(case, &described, "SinkName");
    let role = output(case, &described, "RoleName");
    let mapping_id = output(case, &described, "MappingId");

    // The insert the stream carries to a consumer DynamoDB refuses the write of: what the
    // mapping reports is the function's own error, and it goes on retrying the batch.
    let key = "ungranted-insert";
    let (status_code, body) = dynamodb(
        &router,
        "PutItem",
        &json!({
            "TableName": source,
            "Item": {PARTITION_KEY: {"S": key}, "marker": {"S": "refused"}},
        }),
    )
    .await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: expected the insert to be accepted, got {status_code} {body}"
    );
    let mut reported = Value::Null;
    for _ in 0..LOOKS {
        reported = reported_mapping(&router, &mapping_id)
            .await
            .unwrap_or(Value::Null);
        if reported
            .get("LastProcessingResult")
            .and_then(Value::as_str)
            .is_some_and(|result| result.contains("PROBLEM"))
        {
            break;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    assert!(
        reported
            .get("LastProcessingResult")
            .and_then(Value::as_str)
            .is_some_and(|result| result.contains("PROBLEM")),
        "case {case:?}: expected the mapping to report the consumer's failure, got {reported}"
    );
    assert!(
        sink_item(&router, &sink, key).await.is_none(),
        "case {case:?}: expected nothing to reach {sink} while the consumer is refused the \
         write"
    );

    let (status_code, body) = call(&router, "DeleteStack", &[member("StackName", row.stack)]).await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: expected HTTP 200 from DeleteStack, got {status_code} {body}"
    );
    let (_, described) = describe_stack(&router, row.stack).await;
    let stack_id = xml_text(&described, "StackId").unwrap_or_default();
    let described = settled(case, &router, &stack_id).await;
    let (_, events) = call(
        &router,
        "DescribeStackEvents",
        &[member("StackName", &stack_id)],
    )
    .await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some("DELETE_COMPLETE"),
        "case {case:?}: expected the teardown to reach DELETE_COMPLETE, got {described} with \
         events {events}"
    );
    let (status_code, body) = iam(&router, "GetRole", &[("RoleName", role.as_str())]).await;
    assert_eq!(
        status_code, 404,
        "case {case:?}: expected IAM to hold no role {role} once the stack is deleted, got \
         {status_code} {body}"
    );
}

/// What one row embeds in or attaches to the stack's identity OUT OF BAND — through IAM's
/// own wire, as a client or another stack does — before the stack is deleted.
///
/// Each is a dependency IAM refuses to delete the identity while it is there
/// (`DeleteConflictException`: "attempted to delete a resource that has attached subordinate
/// entities"), and none of them is in the template, so none can be read off the declaration.
#[derive(Debug, Clone, Copy)]
enum PutThere {
    /// A document embedded in the identity (`PutRolePolicy`/`PutUserPolicy`).
    AnInlinePolicy,
    /// A managed policy attached to it (`AttachRolePolicy`/`AttachUserPolicy`).
    AnAttachedPolicy,
    /// Both of them at once, which is the state a grant repaired outside the template leaves
    /// an identity in.
    Both,
}

/// The identity one row is about, which decides the calls that hold it.
#[derive(Debug, Clone, Copy)]
enum Identity {
    Role,
    User,
}

impl Identity {
    /// The resource type a template declares one under.
    fn resource_type(self) -> &'static str {
        match self {
            Self::Role => "AWS::IAM::Role",
            Self::User => "AWS::IAM::User",
        }
    }

    /// The read that reports whether IAM still holds it.
    fn read(self) -> &'static str {
        match self {
            Self::Role => "GetRole",
            Self::User => "GetUser",
        }
    }

    /// The request member every call about one names it in.
    fn member(self) -> &'static str {
        match self {
            Self::Role => "RoleName",
            Self::User => "UserName",
        }
    }

    /// The call that embeds a document in it.
    fn embed(self) -> &'static str {
        match self {
            Self::Role => "PutRolePolicy",
            Self::User => "PutUserPolicy",
        }
    }

    /// The call that attaches a managed policy to it.
    fn attach(self) -> &'static str {
        match self {
            Self::Role => "AttachRolePolicy",
            Self::User => "AttachUserPolicy",
        }
    }
}

/// A policy document IAM accepts, as the text its calls take it as.
fn allow_document(action: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{"Effect": "Allow", "Action": action, "Resource": "*"}],
    })
    .to_string()
}

/// RULE: a stack's deletion removes what each of its resources still HOLDS before deleting
/// it — for an IAM identity, every document embedded in it and every managed policy attached
/// to it, as IAM itself reports them — so an identity something granted outside the template
/// is still taken away by the stack that created it.
///
/// The rows differ along what the rule generalises over: which identity type the stack
/// declares, and what was put on it outside the template (a document embedded in it, a
/// managed policy attached to it, or both) beside the policies the declaration itself asked
/// for.
#[tokio::test]
async fn the_deletion_removes_what_an_identity_still_holds_whatever_put_it_there() {
    let rows: &[(&str, Identity, PutThere)] = &[
        (
            "a role carrying a document a client embedded itself",
            Identity::Role,
            PutThere::AnInlinePolicy,
        ),
        (
            "a role a client attached a managed policy to",
            Identity::Role,
            PutThere::AnAttachedPolicy,
        ),
        (
            "a role holding both, beside the policies its declaration asked for",
            Identity::Role,
            PutThere::Both,
        ),
        (
            "a user holding both, beside the policies its declaration asked for",
            Identity::User,
            PutThere::Both,
        ),
    ];
    for (index, (case, identity, put_there)) in rows.iter().enumerate() {
        let router = router();
        let stack = format!("identity-teardown-{index}");
        // The declaration asks for an inline policy and an attached managed policy of its
        // own, so a row proves the declared sets still go as well as the held ones. The
        // managed policy the declaration names is one this account holds, created here.
        let (status_code, body) = iam(
            &router,
            "CreatePolicy",
            &[
                ("PolicyName", &format!("declared-policy-{index}")),
                ("PolicyDocument", &allow_document("s3:ListAllMyBuckets")),
            ],
        )
        .await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: CreatePolicy must answer 200, got {status_code} {body}"
        );
        let declared_arn = xml_text(&body, "Arn").unwrap_or_default();
        let mut properties = json!({
            "Policies": [{
                "PolicyName": format!("declared-inline-{index}"),
                "PolicyDocument": {
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Action": "sqs:ReceiveMessage",
                        "Resource": "*",
                    }],
                },
            }],
            "ManagedPolicyArns": [declared_arn],
        });
        if matches!(identity, Identity::Role) {
            properties
                .as_object_mut()
                .expect("the properties are an object")
                .insert(
                    "AssumeRolePolicyDocument".to_string(),
                    json!({
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Action": "sts:AssumeRole",
                            "Effect": "Allow",
                            "Principal": {"Service": "lambda.amazonaws.com"},
                        }],
                    }),
                );
        }
        let template = json!({
            "Resources": {
                "Identity": {"Type": identity.resource_type(), "Properties": properties},
            },
            "Outputs": {"IdentityName": {"Value": {"Ref": "Identity"}}},
        })
        .to_string();
        let (status_code, body) = call(
            &router,
            "CreateStack",
            &acknowledging_iam(&[
                member("StackName", &stack),
                member("TemplateBody", &template),
            ]),
        )
        .await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status_code} {body}"
        );
        let described = settled(case, &router, &stack).await;
        assert_eq!(
            xml_text(&described, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the deployment to complete, got {described}"
        );
        let name = output(case, &described, "IdentityName");

        // What a client puts on the identity itself, through IAM's own wire.
        if matches!(put_there, PutThere::AnInlinePolicy | PutThere::Both) {
            let (status_code, body) = iam(
                &router,
                identity.embed(),
                &[
                    (identity.member(), name.as_str()),
                    ("PolicyName", &format!("put-there-{index}")),
                    ("PolicyDocument", &allow_document("dynamodb:PutItem")),
                ],
            )
            .await;
            assert_eq!(
                status_code,
                200,
                "case {case:?}: {} must answer 200, got {status_code} {body}",
                identity.embed()
            );
        }
        if matches!(put_there, PutThere::AnAttachedPolicy | PutThere::Both) {
            let (status_code, body) = iam(
                &router,
                "CreatePolicy",
                &[
                    ("PolicyName", &format!("put-there-managed-{index}")),
                    ("PolicyDocument", &allow_document("dynamodb:GetItem")),
                ],
            )
            .await;
            assert_eq!(
                status_code, 200,
                "case {case:?}: CreatePolicy must answer 200, got {status_code} {body}"
            );
            let attached_arn = xml_text(&body, "Arn").unwrap_or_default();
            let (status_code, body) = iam(
                &router,
                identity.attach(),
                &[
                    (identity.member(), name.as_str()),
                    ("PolicyArn", attached_arn.as_str()),
                ],
            )
            .await;
            assert_eq!(
                status_code,
                200,
                "case {case:?}: {} must answer 200, got {status_code} {body}",
                identity.attach()
            );
        }

        let (status_code, body) =
            call(&router, "DeleteStack", &[member("StackName", &stack)]).await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: expected HTTP 200 from DeleteStack, got {status_code} {body}"
        );
        let (_, described) = describe_stack(&router, &stack).await;
        let stack_id = xml_text(&described, "StackId").unwrap_or_default();
        let described = settled(case, &router, &stack_id).await;
        let (_, events) = call(
            &router,
            "DescribeStackEvents",
            &[member("StackName", &stack_id)],
        )
        .await;
        assert_eq!(
            xml_text(&described, "StackStatus").as_deref(),
            Some("DELETE_COMPLETE"),
            "case {case:?}: expected the teardown to reach DELETE_COMPLETE, got {described} \
             with events {events}"
        );
        let (status_code, body) = iam(
            &router,
            identity.read(),
            &[(identity.member(), name.as_str())],
        )
        .await;
        assert_eq!(
            status_code,
            404,
            "case {case:?}: expected IAM to hold no {} {name} once the stack is deleted, got \
             {status_code} {body}",
            identity.member()
        );
    }
}

/// The teardown asks the owning services in reverse DEPENDENCY order, so the document an
/// `AWS::IAM::Policy` embedded in a role is removed before the role itself — whatever the two
/// logical ids sort as, which is what this case varies: the policy's sorts FIRST.
///
/// What a role still holding an embedded policy would answer is IAM's own `DeleteConflict`,
/// which would leave the stack in `DELETE_FAILED` with the role named as the resource it
/// could not delete.
#[tokio::test]
async fn an_embedded_policy_is_removed_before_the_role_whatever_the_ids_sort_as() {
    let case = "an inline policy whose logical id sorts before the role's";
    let stack = "embedded-policy-teardown-order";
    let template = json!({
        "Resources": {
            "APolicy": {
                "Type": "AWS::IAM::Policy",
                "Properties": {
                    "PolicyName": "a-policy",
                    "PolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Action": "dynamodb:ListStreams",
                            "Effect": "Allow",
                            "Resource": "*",
                        }],
                    },
                    "Roles": [{"Ref": "ZRole"}],
                },
            },
            "ZRole": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Action": "sts:AssumeRole",
                            "Effect": "Allow",
                            "Principal": {"Service": "lambda.amazonaws.com"},
                        }],
                    },
                },
            },
        },
        "Outputs": {"RoleName": {"Value": {"Ref": "ZRole"}}},
    })
    .to_string();
    let router = router();
    let (status_code, body) = call(
        &router,
        "CreateStack",
        &acknowledging_iam(&[member("StackName", stack), member("TemplateBody", &template)]),
    )
    .await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status_code} {body}"
    );
    let described = settled(case, &router, stack).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the deployment to complete, got {described}"
    );
    let role = output(case, &described, "RoleName");

    let (status_code, body) = call(&router, "DeleteStack", &[member("StackName", stack)]).await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: expected HTTP 200 from DeleteStack, got {status_code} {body}"
    );
    let (_, described) = describe_stack(&router, stack).await;
    let stack_id = xml_text(&described, "StackId").unwrap_or_default();
    let described = settled(case, &router, &stack_id).await;
    let (_, events) = call(
        &router,
        "DescribeStackEvents",
        &[member("StackName", &stack_id)],
    )
    .await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some("DELETE_COMPLETE"),
        "case {case:?}: expected the teardown to reach DELETE_COMPLETE, got {described} with \
         events {events}"
    );
    let (status_code, body) = iam(&router, "GetRole", &[("RoleName", role.as_str())]).await;
    assert_eq!(
        status_code, 404,
        "case {case:?}: expected IAM to hold no role {role} once the stack is deleted, got \
         {status_code} {body}"
    );
}
