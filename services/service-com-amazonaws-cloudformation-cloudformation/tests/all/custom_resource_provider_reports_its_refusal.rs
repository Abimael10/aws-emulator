// SPDX-License-Identifier: CC0-1.0

//! A Lambda-backed CUSTOM RESOURCE whose provider is REFUSED the call it makes, REPORTS that
//! refusal at the request's `ResponseURL`, and what the stack behind it then says.
//!
//! RULE under test: **a custom resource is held at the request its provider was sent only until
//! that provider answers, and the stack settles on the answer it gave — a `FAILED` answer fails
//! the resource with the `Reason` the provider stated, which CloudFormation reports as the
//! response it received from the custom resource, and the create then rolls back to
//! `ROLLBACK_COMPLETE`; a `SUCCESS` answer completes it.** A provider that reports the refusal
//! the service it called worded carries that service's own error code into the resource's
//! `ResourceStatusReason`, which is what a client reads to learn WHY its provider could not do
//! the work — and the work the refused call would have done is NOT there afterwards.
//!
//! This is the complement of `custom_resource_provider_denied_rollback.rs`, whose provider
//! RAISES on the refusal and so answers nothing at all (the wait runs out and the rollback it
//! cannot carry out leaves `ROLLBACK_FAILED`). The provider here is the one AWS's own
//! documentation writes: it catches the refusal and ANSWERS, so the stack reaches its terminal
//! status from the answer rather than from a window closing, and the provider's own sentence is
//! what the failure reads.
//!
//! Four services have to agree for that to hold, and this crate's harness mounts them on one
//! engine: CloudFormation sends the request and holds the resource until it is answered, AWS
//! Lambda runs the provider as its execution role and supplies it both the SDK and the response
//! module, IAM holds that role's one statement, and Amazon S3 decides the write the provider
//! makes and holds what arrived.
//!
//! The rows differ along what the rule generalises over: whether the execution role grants the
//! write the provider performs — which is what decides the `Status` the provider answers with,
//! and so the status the stack settles at — and how the custom resource's type is spelled (the
//! generic `AWS::CloudFormation::CustomResource` and an author's own `Custom::<Name>`). The
//! bucket is created OUTSIDE the stack, so what a row reads back afterwards is the state of a
//! bucket no rollback removed: an object the refused write never made is an object that is not
//! there.
//!
//! Nothing here sleeps a span the emulator owes: every wait is a poll for the CONDITION under a
//! generous deadline.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_cloudformation_cloudformation::state::{resource_status, status};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{call, create_stack, member, router_with_engine, xml_text};
use crate::provisioned_resources::s3;
use crate::stack_lifecycle::acknowledging_iam;

/// How long a condition the clock owes is waited for, and how often it is looked at.
const DEADLINE: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(50);

/// The logical id every row declares its custom resource under.
const CUSTOM: &str = "Writer";

/// What the provider states its own `Reason` behind when the write it makes is refused, so the
/// sentence the resource reports is recognisably the provider's and not CloudFormation's.
const REFUSED_PREFIX: &str = "the object write was refused: ";

/// The error code Amazon S3 refuses a request no document in force allows with — the code the
/// provider's own report then carries, and what a client reads the cause out of.
const ACCESS_DENIED: &str = "AccessDenied";

/// What CloudFormation reports a failure a provider ANSWERED with as: the response status it
/// received from the custom resource, quoting the message that response returned.
const RECEIVED_FAILED: &str = "Received response status [FAILED] from custom resource";

/// The provider AWS's own documentation writes: it does its work with the SDK its runtime
/// supplies and ANSWERS the request at its `ResponseURL` whatever that work came to — with the
/// refusal it was given as its own `Reason` where the call was refused, through the response
/// module CloudFormation supplies to inline code.
const PROVIDER_SOURCE: &str = r#"
import boto3
import cfnresponse


def handler(event, context):
    properties = event["ResourceProperties"]
    key = properties["Key"]
    if event["RequestType"] == "Delete":
        cfnresponse.send(
            event, context, cfnresponse.SUCCESS, {}, physicalResourceId=key
        )
        return
    try:
        boto3.client("s3").put_object(
            Bucket=properties["Bucket"],
            Key=key,
            Body=properties["Body"].encode("utf-8"),
        )
    except Exception as error:
        cfnresponse.send(
            event,
            context,
            cfnresponse.FAILED,
            {},
            physicalResourceId=key,
            reason="the object write was refused: %s" % (error,),
        )
        return
    cfnresponse.send(
        event,
        context,
        cfnresponse.SUCCESS,
        {"Written": key},
        physicalResourceId=key,
    )
"#;

/// One row: whether the provider's execution role grants the write it performs, how the custom
/// resource's type is spelled, and the names the row's own resources take.
struct Row {
    case: &'static str,
    stack: &'static str,
    bucket: &'static str,
    function: &'static str,
    key: &'static str,
    body: &'static str,
    resource_type: &'static str,
    /// Whether the role the provider runs as grants `s3:PutObject` on the bucket, which is what
    /// decides the `Status` the provider can answer with.
    grants_the_write: bool,
    /// The `StackStatus` the create then settles at.
    settles_at: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "a role that does not grant the write, the generic type",
        stack: "reported-refusal-generic",
        bucket: "reported-refusal-generic-bucket",
        function: "reported-refusal-generic-provider",
        key: "generic-object.txt",
        body: "the write the role does not allow",
        resource_type: "AWS::CloudFormation::CustomResource",
        grants_the_write: false,
        settles_at: status::ROLLBACK_COMPLETE,
    },
    Row {
        case: "a role that does not grant the write, the author's own namespace",
        stack: "reported-refusal-authored",
        bucket: "reported-refusal-authored-bucket",
        function: "reported-refusal-authored-provider",
        key: "authored-object.txt",
        body: "the write the role does not allow either",
        resource_type: "Custom::ObjectWriter",
        grants_the_write: false,
        settles_at: status::ROLLBACK_COMPLETE,
    },
    Row {
        case: "the positive control: the same stack whose role grants the write",
        stack: "reported-write-granted",
        bucket: "reported-write-granted-bucket",
        function: "reported-write-granted-provider",
        key: "granted-object.txt",
        body: "the write the role allows",
        resource_type: "Custom::ObjectWriter",
        grants_the_write: true,
        settles_at: status::CREATE_COMPLETE,
    },
];

/// The statements the execution role of one row carries: the log writes every execution role
/// needs, plus the one object write where the row grants it — on the objects of that row's
/// bucket and nothing else, which is the least privilege the provider could run under.
fn role_statements(row: &Row) -> Value {
    let mut statements = vec![json!({
        "Effect": "Allow",
        "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
        "Resource": "*",
    })];
    if row.grants_the_write {
        statements.push(json!({
            "Effect": "Allow",
            "Action": "s3:PutObject",
            "Resource": format!("arn:aws:s3:::{}/*", row.bucket),
        }));
    }
    json!({"Version": "2012-10-17", "Statement": statements})
}

/// The template one row deploys: the execution role the provider runs as, the provider function
/// itself, and the custom resource that function backs — the bucket it writes into is created
/// outside the stack.
fn template(row: &Row) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Role": {
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
                        "PolicyName": "what-the-provider-may-do",
                        "PolicyDocument": role_statements(row),
                    }],
                },
            },
            "Provider": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": row.function,
                    "Code": {"ZipFile": PROVIDER_SOURCE},
                    "Handler": "index.handler",
                    "Role": {"Fn::GetAtt": ["Role", "Arn"]},
                    "Runtime": "python3.12",
                    "Timeout": 60,
                },
            },
            CUSTOM: {
                "Type": row.resource_type,
                "DependsOn": ["Provider"],
                "Properties": {
                    "ServiceToken": {"Fn::GetAtt": ["Provider", "Arn"]},
                    "Bucket": row.bucket,
                    "Key": row.key,
                    "Body": row.body,
                },
            },
        },
    })
    .to_string()
}

/// What the stack reports now: the `StackStatus` it carries and the body it was read from.
async fn described(router: &MultiServiceRouter, case: &str, stack: &str) -> (String, String) {
    let (http_status, body) = call(router, "DescribeStacks", &[member("StackName", stack)]).await;
    assert_eq!(
        http_status, 200,
        "case {case:?}: expected DescribeStacks to answer for {stack:?}, got {http_status} {body}"
    );
    (xml_text(&body, "StackStatus").unwrap_or_default(), body)
}

/// Poll `stack` until it reports a status that is not `*_IN_PROGRESS` — what a client's waiter
/// does — and answer that status with the body it was read from.
///
/// The wait is on the CONDITION: nothing here is aged or slept on the stack's behalf, so what
/// ends it is the provider's own answer arriving.
async fn settled(router: &MultiServiceRouter, case: &str, stack: &str) -> (String, String) {
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let (reported, body) = described(router, case, stack).await;
        if !reported.is_empty() && !reported.ends_with("_IN_PROGRESS") {
            return (reported, body);
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: expected {stack:?} to settle, last was {reported:?} with {body}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// The `ResourceStatusReason`s the stack's events carry for `logical_id` in `resource_status`.
async fn reasons_for(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
    reported_status: &str,
) -> Vec<String> {
    let (http_status, body) =
        call(router, "DescribeStackEvents", &[member("StackName", stack)]).await;
    assert_eq!(
        http_status, 200,
        "case {case:?}: expected DescribeStackEvents to answer, got {http_status} {body}"
    );
    crate::harness::xml_blocks(&body, "member")
        .into_iter()
        .filter(|block| {
            xml_text(block, "LogicalResourceId").as_deref() == Some(logical_id)
                && xml_text(block, "ResourceStatus").as_deref() == Some(reported_status)
        })
        .map(|block| xml_text(&block, "ResourceStatusReason").unwrap_or_default())
        .collect()
}

/// The bucket the provider writes into, created through Amazon S3's own wire before the stack is
/// deployed — so it outlives a rollback and what it holds afterwards is the provider's doing.
async fn given_bucket(router: &MultiServiceRouter, case: &str, bucket: &str) {
    let (http_status, _, body) = s3(router, "PUT", &format!("/{bucket}"), b"").await;
    assert_eq!(
        http_status, 200,
        "case {case:?}: expected CreateBucket({bucket:?}) to answer 200, got {http_status} {body}"
    );
}

/// What Amazon S3 answers a read of `key` with: the status and the body.
async fn object_at(router: &MultiServiceRouter, bucket: &str, key: &str) -> (u16, String) {
    let (http_status, _, body) = s3(router, "GET", &format!("/{bucket}/{key}"), b"").await;
    (http_status, body)
}

#[tokio::test]
async fn the_stack_settles_on_the_answer_its_provider_gave_and_reports_that_answers_reason() {
    for row in ROWS {
        let case = row.case;
        let (router, _engine) = router_with_engine();
        given_bucket(&router, case, row.bucket).await;
        let (http_status, body) =
            create_stack(&router, row.stack, &template(row), &acknowledging_iam(&[])).await;
        assert_eq!(
            http_status, 200,
            "case {case:?}: expected CreateStack to be accepted, got {http_status} {body}"
        );

        // The stack settles on the ANSWER the provider gave — no window had to close for it,
        // because the provider answered whatever its own call came to.
        let (reported, described) = settled(&router, case, row.stack).await;
        if reported != row.settles_at {
            let failures = reasons_for(
                &router,
                case,
                row.stack,
                CUSTOM,
                resource_status::CREATE_FAILED,
            )
            .await;
            panic!(
                "case {case:?}: expected the stack to settle at {}, got {reported:?}; the \
                 resource reported {failures:?}; last answer {described}",
                row.settles_at
            );
        }

        let (read_status, read) = object_at(&router, row.bucket, row.key).await;
        if row.grants_the_write {
            // The provider's work landed, and the success it answered is what completed the
            // create: the object is in the bucket, with the bytes the declaration asked for.
            assert_eq!(
                (read_status, read.as_str()),
                (200, row.body),
                "case {case:?}: expected the object the provider wrote to hold the declared \
                 bytes, got {read_status} with {read:?}"
            );
            continue;
        }

        // The resource's own event carries the provider's report: CloudFormation's envelope for
        // an answer it RECEIVED, quoting the sentence the provider stated — which carries the
        // error code the service that refused the provider's call worded.
        let failures = reasons_for(
            &router,
            case,
            row.stack,
            CUSTOM,
            resource_status::CREATE_FAILED,
        )
        .await;
        assert!(
            failures.iter().any(|reason| reason.contains(RECEIVED_FAILED)
                && reason.contains(REFUSED_PREFIX)
                && reason.contains(ACCESS_DENIED)),
            "case {case:?}: expected the failed create to report the provider's own refusal \
             carrying {ACCESS_DENIED:?}, got {failures:?}"
        );

        // ...and the write that was refused did not happen: the bucket the rollback did not
        // touch holds no object at that key.
        assert_eq!(
            read_status, 404,
            "case {case:?}: expected no object at {:?} after the refused write, got \
             {read_status} with {read:?}",
            row.key
        );
    }
}
