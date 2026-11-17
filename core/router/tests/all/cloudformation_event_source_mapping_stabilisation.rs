// SPDX-License-Identifier: CC0-1.0

//! A stack that declares an `AWS::Lambda::EventSourceMapping` is not reported complete before
//! AWS Lambda reports that mapping settled — so the first call a client makes after the stack
//! completes finds a trigger that is really polling its event source.
//!
//! The rule under test: **CloudFormation does not report a resource (and so the stack
//! operation holding it) created or changed while the service that owns it still reports the
//! resource short of the state it is settled in; it holds the resource `*_IN_PROGRESS`, under
//! the identifier that service already assigned, until that service reports it settled.** For
//! an event source mapping the settled states are the two its `Enabled` setting asks for —
//! "When true, the event source mapping is active. When false, Lambda pauses polling and
//! invocation" — while `Creating` is the window Lambda stands the poller up in and `Updating`
//! the window it applies an accepted change in
//! (`EventSourceMappingConfiguration$State`: "Creating, Enabling, Enabled, Disabling,
//! Disabled, Updating, or Deleting").
//!
//! Several crates have to agree for that to hold (CloudFormation makes the calls and holds the
//! resource open; Lambda decides the mapping's state; Amazon DynamoDB, Amazon Kinesis and
//! Amazon SQS hold the sources, and IAM answers for the execution role the poller reads them
//! under), so the suite lives here rather than in any one of them.
//!
//! What this pins is the observation an infrastructure-as-code client makes: the instant the
//! stack FIRST reports the operation complete, `lambda get-event-source-mapping` resolves the
//! id the stack published and reports the mapping in the state its declaration asked for. A
//! stack reported complete while its mapping is still `Creating` fails these cases, because
//! the next deployment step then reads a trigger that is consuming nothing, and every call
//! that changes or deletes the mapping is refused as a conflict with a mapping Lambda is still
//! standing up.
//!
//! The rows differ along what the rule generalises over: WHICH event source the mapping polls
//! (an Amazon DynamoDB table's stream — the recorded case — an Amazon Kinesis data stream, and
//! an Amazon SQS queue, which are the three kinds of source whose poller the declaration can
//! name here), WHICH state the declaration's `Enabled` setting settles the mapping in (stated
//! nowhere, stated `true`, stated `false`) and the batch size each one declares. Every
//! expected value is computed here from what the row declares, never read back out of the
//! answer being checked — except the UUID Lambda mints for the mapping, which is that
//! service's to assign.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery CloudFormation caller, the one-shot stack read, the Lambda restJson1 caller,
// the XML readers and the mapping constants are each written ONCE in this test crate: a suite
// that copies one drifts from the others.
use crate::cloudformation_apigatewayv2_resources::{cloudformation, REGION};
use crate::cloudformation_firehose_stream_stabilisation::stack_status;
use crate::cloudformation_lambda_ssm_sourced_update::lambda;
use crate::cloudformation_provisions_lambda_event_source_mappings::{
    MAPPINGS_URI, MAPPING_STATE_DISABLED, MAPPING_STATE_ENABLED,
};
use crate::query_wire::{elements, physical_ids_of};

/// The `StackStatus` the rule is stated in, and the one a create reports while it runs.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const CREATE_IN_PROGRESS: &str = "CREATE_IN_PROGRESS";

/// How long a row waits for the create to finish, and how long it hands the runtime back
/// between two looks.
///
/// Generous slack over every span the services here owe a new resource — the table's own
/// creation window, and the mapping's — because what a row asserts is the state at the first
/// COMPLETED read, never how fast the host got there. The wait is on that CONDITION; all the
/// deadline is for is that it terminates.
const WAIT: Duration = Duration::from_secs(180);
/// See [`WAIT`].
const LOOK_AGAIN: Duration = Duration::from_millis(100);

/// The logical ids every row's template declares its resources under.
const SOURCE_ID: &str = "Source";
const ROLE_ID: &str = "Role";
const FUNCTION_ID: &str = "Consumer";
const MAPPING_ID: &str = "Trigger";

/// The partition key the DynamoDB row's table is declared with.
const KEY_ATTRIBUTE: &str = "pk";

/// Which event source a row's mapping polls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    /// An Amazon DynamoDB table's stream, read through `Fn::GetAtt … StreamArn`.
    DynamodbStream,
    /// An Amazon Kinesis data stream.
    KinesisStream,
    /// An Amazon SQS queue.
    SqsQueue,
}

impl Source {
    /// The poller's own reads of this kind of source, as the owning service's refusal of a
    /// mapping whose role cannot make them names them: "ensure the role can perform the
    /// GetRecords, GetShardIterator, DescribeStream, and ListStreams Actions on your stream"
    /// for a shard source, and `ReceiveMessage`/`DeleteMessage`/`GetQueueAttributes` for a
    /// queue.
    fn reader_actions(self) -> Vec<&'static str> {
        match self {
            Self::DynamodbStream => vec![
                "dynamodb:DescribeStream",
                "dynamodb:GetRecords",
                "dynamodb:GetShardIterator",
                "dynamodb:ListStreams",
            ],
            Self::KinesisStream => vec![
                "kinesis:DescribeStream",
                "kinesis:GetRecords",
                "kinesis:GetShardIterator",
                "kinesis:ListStreams",
            ],
            Self::SqsQueue => vec![
                "sqs:DeleteMessage",
                "sqs:GetQueueAttributes",
                "sqs:ReceiveMessage",
            ],
        }
    }

    /// The reference that resolves to the ARN of the source the mapping polls: a table
    /// publishes its stream's ARN under `StreamArn`, and a data stream and a queue publish
    /// their own under `Arn`.
    fn arn_reference(self) -> Value {
        match self {
            Self::DynamodbStream => json!({"Fn::GetAtt": [SOURCE_ID, "StreamArn"]}),
            Self::KinesisStream | Self::SqsQueue => json!({"Fn::GetAtt": [SOURCE_ID, "Arn"]}),
        }
    }

    /// The source resource itself, as the row's template declares it.
    fn declared(self, stack: &str) -> Value {
        match self {
            Self::DynamodbStream => json!({
                "Type": "AWS::DynamoDB::Table",
                "Properties": {
                    "KeySchema": [{"AttributeName": KEY_ATTRIBUTE, "KeyType": "HASH"}],
                    "AttributeDefinitions": [
                        {"AttributeName": KEY_ATTRIBUTE, "AttributeType": "S"}
                    ],
                    "BillingMode": "PAY_PER_REQUEST",
                    "StreamSpecification": {"StreamViewType": "NEW_AND_OLD_IMAGES"},
                },
            }),
            Self::KinesisStream => json!({
                "Type": "AWS::Kinesis::Stream",
                "Properties": {"Name": format!("{stack}-stream"), "ShardCount": 1},
            }),
            Self::SqsQueue => json!({
                "Type": "AWS::SQS::Queue",
                "Properties": {"QueueName": format!("{stack}-queue")},
            }),
        }
    }

    /// Whether a mapping on this source needs a starting position, which
    /// `CreateEventSourceMapping` requires of the two SHARD sources and takes for no queue.
    fn reads_shards(self) -> bool {
        matches!(self, Self::DynamodbStream | Self::KinesisStream)
    }

    /// The ARN the mapping must report polling, computed from what the row declares — `None`
    /// for a table's stream, whose ARN carries the instant DynamoDB enabled the stream and so
    /// is checkable only by the table it nests in.
    fn expected_arn(self, stack: &str) -> Option<String> {
        let account = engine::emulator_account_id();
        match self {
            Self::DynamodbStream => None,
            Self::KinesisStream => Some(format!(
                "arn:aws:kinesis:{REGION}:{account}:stream/{stack}-stream"
            )),
            Self::SqsQueue => Some(format!("arn:aws:sqs:{REGION}:{account}:{stack}-queue")),
        }
    }
}

/// One row: the source the mapping polls, what its declaration says about polling, the batch
/// size it declares, and the state Lambda has to report the mapping in once the stack reports
/// the operation complete.
struct Row {
    case: &'static str,
    stack: &'static str,
    source: Source,
    /// The `Enabled` property, where the declaration states one at all.
    enabled: Option<bool>,
    /// The `BatchSize` the mapping is declared with — one per row, so the row's own figure is
    /// what the settled mapping has to carry rather than one value shared by the table.
    batch_size: i64,
    /// The state the declaration settles the mapping in — `Enabled` for a mapping asked to
    /// poll (which is the default: "Default: True"), `Disabled` for one asked not to.
    settles_into: &'static str,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            // The recorded case: a table's stream wired to a function by one template.
            case: "a DynamoDB table's stream, with polling left to the default",
            stack: "esm-settles-ddb-stream",
            source: Source::DynamodbStream,
            enabled: None,
            batch_size: 1,
            settles_into: MAPPING_STATE_ENABLED,
        },
        Row {
            case: "a DynamoDB table's stream, with polling paused",
            stack: "esm-settles-ddb-stream-paused",
            source: Source::DynamodbStream,
            enabled: Some(false),
            batch_size: 7,
            settles_into: MAPPING_STATE_DISABLED,
        },
        Row {
            case: "a Kinesis data stream, with polling asked for",
            stack: "esm-settles-kinesis",
            source: Source::KinesisStream,
            enabled: Some(true),
            batch_size: 100,
            settles_into: MAPPING_STATE_ENABLED,
        },
        Row {
            case: "an SQS queue",
            stack: "esm-settles-queue",
            source: Source::SqsQueue,
            enabled: None,
            batch_size: 5,
            settles_into: MAPPING_STATE_ENABLED,
        },
    ]
}

/// The emulator with the services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them. Request authentication is off, as in every other
/// suite here; the in-process call CloudFormation makes signs its request either way (the
/// credential scope is what routes it).
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_dynamodbstreams_dynamodbstreams_20120810::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_kinesis_kinesis_20131202::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// The template a row deploys: the event source, the execution role granted the poller's own
/// reads of it, the function the mapping invokes, and the mapping itself, declared with the
/// row's own batch size.
fn template(row: &Row) -> String {
    let source_arn = row.source.arn_reference();
    let mut mapping = json!({
        "EventSourceArn": source_arn.clone(),
        "FunctionName": {"Ref": FUNCTION_ID},
        "BatchSize": row.batch_size,
    });
    if row.source.reads_shards() {
        mapping["StartingPosition"] = json!("LATEST");
    }
    if let Some(enabled) = row.enabled {
        mapping["Enabled"] = json!(enabled);
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            SOURCE_ID: row.source.declared(row.stack),
            ROLE_ID: {
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
                    "Policies": [{
                        "PolicyName": "source-reads",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Action": row.source.reader_actions(),
                                "Effect": "Allow",
                                "Resource": source_arn,
                            }],
                        },
                    }],
                },
            },
            FUNCTION_ID: {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "Code": {"ZipFile": "def handler(event, context):\n    return {}\n"},
                    "Handler": "index.handler",
                    "Role": {"Fn::GetAtt": [ROLE_ID, "Arn"]},
                    "Runtime": "python3.12",
                    "Timeout": 30,
                },
            },
            MAPPING_ID: {
                "Type": "AWS::Lambda::EventSourceMapping",
                "Properties": mapping,
            },
        },
    })
    .to_string()
}

/// The physical id the stack publishes for one of its logical resources — for the mapping,
/// the UUID Lambda minted ("Ref returns the event source mapping's ID").
async fn published_id(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
) -> String {
    let (status, resources) =
        cloudformation(router, "DescribeStackResources", &[("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackResources, got {status} with body \
         {resources}"
    );
    physical_ids_of(&resources)
        .remove(logical_id)
        .unwrap_or_else(|| {
            panic!("case {case:?}: no physical id for {logical_id} in {resources}")
        })
}

/// What Lambda reports for the mapping `uuid`, read ONCE: the rule is about the state that
/// service is in at the instant the stack first reported the operation complete, so nothing
/// here waits for the state to improve.
async fn reported_mapping(router: &MultiServiceRouter, case: &str, uuid: &str) -> Value {
    let (status, _, body) = lambda(router, "GET", &format!("{MAPPINGS_URI}/{uuid}"), &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected GetEventSourceMapping to resolve the id the stack published \
         ({uuid:?}), got {status} with body {body}"
    );
    body
}

/// Why a stack did not reach the status a case expected, for the assertion's own message.
async fn failure_reasons(router: &MultiServiceRouter, stack: &str) -> Vec<String> {
    let (_, events) =
        cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    elements(&events, "ResourceStatusReason")
}

/// Read the stack again until its create has FINISHED, in whatever status it finished in, and
/// answer that status with the answer it was read from.
///
/// The transitions on the way are owed on the clocks of the services that own the stack's
/// resources, so this waits for the CONDITION rather than for a span — and the FIRST completed
/// read is what the rule below is stated about, so nothing is waited out past it.
async fn created(router: &MultiServiceRouter, stack: &str) -> (String, String) {
    let giving_up_at = Instant::now() + WAIT;
    let mut last = stack_status(router, stack).await;
    while last.0 == CREATE_IN_PROGRESS && Instant::now() < giving_up_at {
        tokio::time::sleep(LOOK_AGAIN).await;
        last = stack_status(router, stack).await;
    }
    last
}

/// Every row: the stack reaches `CREATE_COMPLETE`, and at that FIRST observation Lambda
/// reports the mapping the stack published in the state the declaration asked for — never in
/// the `Creating` window it stands a poller up in.
#[tokio::test]
async fn a_stack_completes_only_once_lambda_settles_its_event_source_mapping() {
    for row in rows() {
        let router = build_router();
        let case = row.case;

        let (status, body) = cloudformation(
            &router,
            "CreateStack",
            &[
                ("StackName", row.stack),
                ("TemplateBody", &template(&row)),
                ("Capabilities.member.1", "CAPABILITY_IAM"),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
        );

        let (reached, described) = created(&router, row.stack).await;
        assert_eq!(
            reached, CREATE_COMPLETE,
            "case {case:?}: expected the stack to deploy, got {described} with resource \
             failures {:?}",
            failure_reasons(&router, row.stack).await
        );

        let uuid = published_id(&router, case, row.stack, MAPPING_ID).await;
        let reported = reported_mapping(&router, case, &uuid).await;
        assert_eq!(
            reported.get("State").and_then(Value::as_str),
            Some(row.settles_into),
            "case {case:?}: expected the mapping to be {:?} the instant the stack reported \
             {CREATE_COMPLETE}, got {reported}",
            row.settles_into
        );
        // The settled mapping is the one the declaration asked for: it polls the source the
        // template named, with the batch size it declared.
        let polled = reported
            .get("EventSourceArn")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        match row.source.expected_arn(row.stack) {
            Some(expected) => assert_eq!(
                polled, expected,
                "case {case:?}: expected the mapping to poll {expected:?}, got {polled:?}"
            ),
            // A table's stream: the ARN nests inside the table's own, under the id the stack
            // published for the table.
            None => {
                let table = published_id(&router, case, row.stack, SOURCE_ID).await;
                let within = format!(
                    "arn:aws:dynamodb:{REGION}:{}:table/{table}/stream/",
                    engine::emulator_account_id()
                );
                assert!(
                    polled.starts_with(&within),
                    "case {case:?}: expected the mapping to poll a stream of the table the \
                     stack created ({within:?}), got {polled:?}"
                );
            }
        }
        assert_eq!(
            reported.get("BatchSize").and_then(Value::as_i64),
            Some(row.batch_size),
            "case {case:?}: expected the declared batch size {} to be in force, got {reported}",
            row.batch_size
        );
    }
}
