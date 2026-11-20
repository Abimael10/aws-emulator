// SPDX-License-Identifier: CC0-1.0

//! A `Custom::` resource backed by a Lambda provider created OUT OF BAND, whose handler makes
//! an Amazon S3 write of its own and then answers CloudFormation at the request's
//! `ResponseURL` — the whole chain, over the wire.
//!
//! RULE under test: **a custom resource settles on the answer its provider sends, and on
//! nothing else.** A provider that carried its work out answers `SUCCESS`, so the resource
//! reaches `CREATE_COMPLETE`, the stack reaches `CREATE_COMPLETE`, the object the handler wrote
//! is readable, and a revision of the resource's properties sends the same provider an `Update`
//! whose work is done beside what the `Create` already made. A provider whose own AWS call is
//! refused answers `FAILED` with that refusal as its `Reason`, so CloudFormation fails the
//! resource with its own envelope for an answered failure — `Received response status [FAILED]
//! from custom resource. Message returned: <the provider's reason> (RequestId: <the id of the
//! request it answered>)` — and rolls the stack back, leaving nothing behind.
//!
//! Four services have to agree for that to hold, and this crate's harness mounts them all on
//! one engine: CloudFormation sends the provider request and holds the resource until it is
//! answered, AWS Lambda runs the provider's deployment package, IAM holds the execution role
//! it runs as, and Amazon S3 decides the write that role either carries or does not.
//!
//! The rows differ along what the rule generalises over: how the custom resource's type is
//! spelled (an author's own `Custom::<Name>` and the modelled
//! `AWS::CloudFormation::CustomResource`), the SHAPE of the object key (a name at the bucket
//! root and one under a prefix), the text of the body (plain text, a JSON document, and text
//! carrying punctuation and a multi-byte character) and whether the execution role grants the
//! write the handler makes.
//!
//! Nothing here sleeps for a span the emulator owes: every wait is a poll for the CONDITION
//! under a generous deadline, and no case asserts how long anything took.

use std::collections::BTreeMap;

use serde_json::json;
use service_com_amazonaws_cloudformation_cloudformation::state::{resource_status, status};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    call, create_stack, failed_resource_reason, is_a_request_id, member, router_with_engine,
    settled_stack, xml_text, xml_texts, REGION,
};
use crate::lambda_backed_custom_resources::{given_function, given_role};
use crate::stack_lifecycle::iam;
use crate::stack_resource_provisioning::s3;

/// The logical id every row declares its custom resource under.
const WRITER: &str = "Writer";

/// The provider: it writes the body its properties name at the key they name with the SDK its
/// runtime supplies, and answers CloudFormation at the request's own `ResponseURL` — `SUCCESS`
/// when the write was served, and `FAILED` carrying the refusal it was answered with when it
/// was not. A `Delete` has nothing to undo, so it is answered straight away, which is what
/// lets a rollback finish.
const WRITES_THE_OBJECT_IT_IS_DECLARED_WITH: &str = r#"
import json
import urllib.request

import boto3


def answer(event, status, reason, data):
    document = json.dumps(
        {
            "Status": status,
            "Reason": reason,
            "PhysicalResourceId": "writer-" + event["LogicalResourceId"],
            "StackId": event["StackId"],
            "RequestId": event["RequestId"],
            "LogicalResourceId": event["LogicalResourceId"],
            "Data": data,
        }
    ).encode("utf-8")
    request = urllib.request.Request(
        event["ResponseURL"],
        data=document,
        headers={"content-type": ""},
        method="PUT",
    )
    with urllib.request.urlopen(request) as answered:
        answered.read()


def handler(event, context):
    if event["RequestType"] == "Delete":
        answer(event, "SUCCESS", "nothing of its own to remove", {})
        return {}
    properties = event["ResourceProperties"]
    try:
        boto3.client("s3").put_object(
            Bucket=properties["Bucket"],
            Key=properties["Key"],
            Body=properties["Body"].encode("utf-8"),
        )
    except Exception as refused:
        answer(event, "FAILED", str(refused), {})
        return {}
    answer(event, "SUCCESS", "wrote the object", {"WrittenKey": properties["Key"]})
    return {}
"#;

/// The fixed text CloudFormation wraps an ANSWERED failure in, and the two parts of the
/// envelope its variable halves sit between: the provider's own reason, and the `RequestId` of
/// the request that answer was for.
const ANSWERED_OPENS: &str =
    "Received response status [FAILED] from custom resource. Message returned: ";
const ANSWERED_REQUEST_ID: &str = " (RequestId: ";

/// The S3 error code a call no identity-based policy allows is refused with, which is what the
/// provider's own reason carries into that envelope.
const ACCESS_DENIED: &str = "AccessDenied";

/// The provider's reason and the id of the request it answered, read out of the envelope
/// CloudFormation recorded on the resource's event.
fn answered_parts(case: &str, reason: &str) -> (String, String) {
    let rest = reason.strip_prefix(ANSWERED_OPENS).unwrap_or_else(|| {
        panic!("case {case:?}: expected the reason to open with {ANSWERED_OPENS:?}, got {reason:?}")
    });
    let (message, tail) = rest.rsplit_once(ANSWERED_REQUEST_ID).unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected the provider's message and the id of the request it \
             answered, got {reason:?}"
        )
    });
    let request_id = tail.strip_suffix(')').unwrap_or_else(|| {
        panic!("case {case:?}: expected the envelope to close, got {reason:?}")
    });
    (message.to_string(), request_id.to_string())
}

/// The statements one row's execution role carries: the log writes every execution role needs,
/// plus the object write where the row grants it — scoped to the objects of its own bucket, the
/// way a least-privilege role for one writer is written.
fn role_document(bucket: &str, grants_the_write: bool) -> String {
    let mut statements = vec![json!({
        "Effect": "Allow",
        "Action": "logs:*",
        "Resource": "*",
    })];
    if grants_the_write {
        statements.push(json!({
            "Effect": "Allow",
            "Action": "s3:PutObject",
            "Resource": format!("arn:aws:s3:::{bucket}/*"),
        }));
    }
    json!({"Version": "2012-10-17", "Statement": statements}).to_string()
}

/// The template one row deploys: the custom resource alone, backed by the literal ARN of the
/// function created outside the stack, with the bucket, key and body it is to write — and
/// outputs publishing what `Ref` and `Fn::GetAtt` resolve to for it.
fn template(
    resource_type: &str,
    service_token: &str,
    bucket: &str,
    key: &str,
    body: &str,
) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            WRITER: {
                "Type": resource_type,
                "Properties": {
                    "ServiceToken": service_token,
                    "Bucket": bucket,
                    "Key": key,
                    "Body": body,
                },
            },
        },
        "Outputs": {
            "WriterRef": {"Value": {"Ref": WRITER}},
            "WrittenKey": {"Value": {"Fn::GetAtt": [WRITER, "WrittenKey"]}},
        },
    })
    .to_string()
}

/// The bucket the row's provider writes into, created through Amazon S3's own wire before any
/// stack names it.
async fn given_bucket(router: &MultiServiceRouter, case: &str, bucket: &str) {
    let created = s3(router, "PUT", &format!("/{bucket}"), b"").await;
    assert!(
        (200..300).contains(&created.status),
        "case {case:?}: expected the bucket {bucket:?} to be created, got {} with {}",
        created.status,
        created.body
    );
}

/// The execution role of one row: created outside the stack, carrying the statements the row
/// grants it. Answers the ARN IAM minted.
async fn given_execution_role(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    bucket: &str,
    grants_the_write: bool,
) -> String {
    let role_arn = given_role(router, case, name).await;
    let (code, body) = iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", "what-the-provider-may-do"),
            ("PolicyDocument", &role_document(bucket, grants_the_write)),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected PutRolePolicy({name:?}) to persist the policy, got {code} {body}"
    );
    role_arn
}

/// A stack's reported outputs, as the key/value map a client reads.
fn outputs_of(xml: &str) -> BTreeMap<String, String> {
    xml_texts(xml, "OutputKey")
        .into_iter()
        .zip(xml_texts(xml, "OutputValue"))
        .collect()
}

/// What Amazon S3 holds at one key right now: the HTTP status and the body it answered.
async fn object_at(router: &MultiServiceRouter, bucket: &str, key: &str) -> (u16, String) {
    let read = s3(router, "GET", &format!("/{bucket}/{key}"), b"").await;
    (read.status, read.body)
}

/// One row of the happy path: the names its resources are created under, how the custom
/// resource's type is spelled, and the two objects the create and the update ask for.
struct WroteRow {
    case: &'static str,
    stack: &'static str,
    bucket: &'static str,
    role: &'static str,
    function: &'static str,
    resource_type: &'static str,
    first_key: &'static str,
    first_body: &'static str,
    second_key: &'static str,
    second_body: &'static str,
}

const WROTE_ROWS: &[WroteRow] = &[
    WroteRow {
        case: "an author's own type writing plain text at the bucket root",
        stack: "provider-wrote-authored",
        bucket: "provider-wrote-authored-bucket",
        role: "provider-wrote-authored-role",
        function: "provider-wrote-authored-handler",
        resource_type: "Custom::S3Writer",
        first_key: "v1.txt",
        first_body: "the body the template declared",
        second_key: "v2.txt",
        second_body: "the body the revision declared",
    },
    // The MODELLED type, a key under a prefix, and bodies a form body and an XML envelope both
    // have to carry unharmed: a JSON document, then text with punctuation and a multi-byte
    // character.
    WroteRow {
        case: "the modelled type writing a document under a prefix",
        stack: "provider-wrote-modelled",
        bucket: "provider-wrote-modelled-bucket",
        role: "provider-wrote-modelled-role",
        function: "provider-wrote-modelled-handler",
        resource_type: "AWS::CloudFormation::CustomResource",
        first_key: "nested/prefix/v1.json",
        first_body: r#"{"revision": 1}"#,
        second_key: "nested/prefix/v2.json",
        second_body: "a body, with spaces & a ✓",
    },
];

#[tokio::test]
async fn a_provider_that_wrote_its_object_completes_the_stack_and_its_update() {
    for row in WROTE_ROWS {
        let case = row.case;
        let (router, _engine) = router_with_engine();
        given_bucket(&router, case, row.bucket).await;
        let role_arn = given_execution_role(&router, case, row.role, row.bucket, true).await;
        let service_token = given_function(
            &router,
            case,
            row.function,
            &role_arn,
            WRITES_THE_OBJECT_IT_IS_DECLARED_WITH,
        )
        .await;

        // 1. The create: the provider is invoked, writes the object its properties named, and
        // answers SUCCESS — which is what completes the resource and the stack.
        let created = template(
            row.resource_type,
            &service_token,
            row.bucket,
            row.first_key,
            row.first_body,
        );
        let (code, answered) = create_stack(&router, row.stack, &created, &[]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected CreateStack to be accepted, got {code} with {answered}"
        );
        let described = settled_stack(case, &router, REGION, row.stack).await;
        assert_eq!(
            xml_text(&described, "StackStatus").unwrap_or_default(),
            status::CREATE_COMPLETE,
            "case {case:?}: expected the stack to complete on its provider's answer, got \
             {described}"
        );
        let (read_code, read_body) = object_at(&router, row.bucket, row.first_key).await;
        assert_eq!(
            read_code, 200,
            "case {case:?}: expected the object the provider wrote at {:?} to be readable, got \
             {read_code} with {read_body}",
            row.first_key
        );
        assert_eq!(
            read_body, row.first_body,
            "case {case:?}: expected the object to carry the declared body, got {read_body:?}"
        );
        let outputs = outputs_of(&described);
        assert_eq!(
            outputs.get("WriterRef").map(String::as_str),
            Some(format!("writer-{WRITER}").as_str()),
            "case {case:?}: expected Ref to resolve to the id the provider answered, got \
             {outputs:?}"
        );
        assert_eq!(
            outputs.get("WrittenKey").map(String::as_str),
            Some(row.first_key),
            "case {case:?}: expected Fn::GetAtt to resolve to the Data the provider published, \
             got {outputs:?}"
        );

        // 2. The update: the same provider is sent an `Update` for the revised properties and
        // writes the second object — and what the `Create` wrote is untouched, because nothing
        // of this update asked for it to be removed.
        let revised = template(
            row.resource_type,
            &service_token,
            row.bucket,
            row.second_key,
            row.second_body,
        );
        let (code, answered) = call(
            &router,
            "UpdateStack",
            &[
                member("StackName", row.stack),
                member("TemplateBody", &revised),
            ],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected UpdateStack to be accepted, got {code} with {answered}"
        );
        let described = settled_stack(case, &router, REGION, row.stack).await;
        assert_eq!(
            xml_text(&described, "StackStatus").unwrap_or_default(),
            status::UPDATE_COMPLETE,
            "case {case:?}: expected the update to complete on its provider's answer, got \
             {described}"
        );
        let (read_code, read_body) = object_at(&router, row.bucket, row.second_key).await;
        assert_eq!(
            read_code, 200,
            "case {case:?}: expected the object the update wrote at {:?} to be readable, got \
             {read_code} with {read_body}",
            row.second_key
        );
        assert_eq!(
            read_body, row.second_body,
            "case {case:?}: expected the second object to carry the revised body, got \
             {read_body:?}"
        );
        let (read_code, read_body) = object_at(&router, row.bucket, row.first_key).await;
        assert_eq!(
            (read_code, read_body.as_str()),
            (200, row.first_body),
            "case {case:?}: expected the object the create wrote to be untouched by the update, \
             got {read_code} with {read_body:?}"
        );
    }
}

/// One row of the denial path: the names its resources are created under, how the custom
/// resource's type is spelled, and the object its provider is asked for and is refused.
struct DeniedRow {
    case: &'static str,
    stack: &'static str,
    bucket: &'static str,
    role: &'static str,
    function: &'static str,
    resource_type: &'static str,
    key: &'static str,
}

const DENIED_ROWS: &[DeniedRow] = &[
    DeniedRow {
        case: "an author's own type whose role grants no object write",
        stack: "provider-denied-authored",
        bucket: "provider-denied-authored-bucket",
        role: "provider-denied-authored-role",
        function: "provider-denied-authored-handler",
        resource_type: "Custom::S3Writer",
        key: "v1.txt",
    },
    // The modelled type and a key under a prefix: the refusal is about the CALL the handler
    // made, so neither the spelling of the type nor the shape of the key changes it.
    DeniedRow {
        case: "the modelled type whose role grants no object write",
        stack: "provider-denied-modelled",
        bucket: "provider-denied-modelled-bucket",
        role: "provider-denied-modelled-role",
        function: "provider-denied-modelled-handler",
        resource_type: "AWS::CloudFormation::CustomResource",
        key: "nested/prefix/v1.json",
    },
];

#[tokio::test]
async fn a_provider_refused_its_write_fails_the_resource_with_the_reason_it_answered() {
    for row in DENIED_ROWS {
        let case = row.case;
        let (router, _engine) = router_with_engine();
        given_bucket(&router, case, row.bucket).await;
        let role_arn = given_execution_role(&router, case, row.role, row.bucket, false).await;
        let service_token = given_function(
            &router,
            case,
            row.function,
            &role_arn,
            WRITES_THE_OBJECT_IT_IS_DECLARED_WITH,
        )
        .await;

        let created = template(
            row.resource_type,
            &service_token,
            row.bucket,
            row.key,
            "the body that was never written",
        );
        let (code, answered) = create_stack(&router, row.stack, &created, &[]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected CreateStack to be accepted, got {code} with {answered}"
        );

        // 1. The resource's own failure carries CloudFormation's envelope for an ANSWERED
        // failure, whose message is the provider's own reason — the refusal of the write its
        // role carries no grant for.
        let reason = failed_resource_reason(
            case,
            &router,
            REGION,
            row.stack,
            WRITER,
            resource_status::CREATE_FAILED,
        )
        .await;
        let (message, request_id) = answered_parts(case, &reason);
        assert!(
            message.contains(ACCESS_DENIED),
            "case {case:?}: expected the provider's reason to name the refusal of its write, \
             got {message:?}"
        );
        assert!(
            message.contains("PutObject"),
            "case {case:?}: expected the provider's reason to name the call that was refused, \
             got {message:?}"
        );
        assert!(
            is_a_request_id(&request_id),
            "case {case:?}: expected the envelope to name the id of the request the provider \
             answered, got {request_id:?}"
        );

        // 2. ...and the create that failed is rolled back: the stack rests at
        // ROLLBACK_COMPLETE, with nothing at the key the provider never wrote.
        let described = settled_stack(case, &router, REGION, row.stack).await;
        assert_eq!(
            xml_text(&described, "StackStatus").unwrap_or_default(),
            status::ROLLBACK_COMPLETE,
            "case {case:?}: expected the failed create to be rolled back, got {described}"
        );
        let head = s3(
            &router,
            "HEAD",
            &format!("/{}/{}", row.bucket, row.key),
            b"",
        )
        .await;
        assert_eq!(
            head.status, 404,
            "case {case:?}: expected nothing at {:?}, got {} with {}",
            row.key, head.status, head.body
        );
    }
}
