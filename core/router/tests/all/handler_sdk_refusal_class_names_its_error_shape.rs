// SPDX-License-Identifier: CC0-1.0

//! What CLASS of exception the AWS SDK a managed Lambda runtime supplies raises when the
//! service a handler called REFUSES the call.
//!
//! RULE: a refusal of a call made through that SDK is raised as the exception class named
//! after the error shape the called service's own model declares for the operation called —
//! so the class a handler catches it by (`client.exceptions.<Shape>`) and the class name it
//! reports (`type(error).__name__`) are both that shape's name, while remaining a
//! `botocore.exceptions.ClientError`; only a code the model declares no error shape for is
//! the base class itself.
//!
//! Only a cross-service suite can pin this: the refusal is CloudFront's own — its code, its
//! sentence and its status — and the class it is raised as is the Lambda runtime's SDK, built
//! out of the called service's model. The CloudFront crate's own suites pin what the service
//! answers on the wire, and the Lambda crate's pin what a handler computes; neither can state
//! what a RUNNING handler's `except` clause catches.
//!
//! The rows differ along what the rule generalises over: which error shape the refusal names
//! (`NoSuchDistribution`, `NoSuchInvalidation`, `InvalidArgument`), which operation declares
//! it (one shape reached through three different operations), and the status that shape is
//! answered at (404 and 400). Every row also asserts the boundary of the catalogue the
//! classes are built from: a name the service's model declares no error shape for has no
//! class of its own, so it is the SDK's base class.
//!
//! The error shapes and their statuses are the model's own
//! (`smithy_query(query="operation",operation="CreateInvalidation,GetInvalidation,GetDistribution,ListInvalidations",
//! model_path="services/service-com-amazonaws-cloudfront-cloudfront2020-05-31/smithy-model/com-amazonaws-cloudfront-cloudfront2020-05-31.json")`).
//!
//! The wire harness is [`super::lambda_handler_invalidates_its_distribution`]'s, reused rather
//! than repeated: that suite reaches CloudFront, Amazon S3, Lambda and CloudFormation exactly
//! this way.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::lambda_handler_invalidates_its_distribution::{
    build_router, call, create_distribution, create_origin, deployed, wait_until_active, Answer,
    REGION,
};

/// The role the probe function runs as, the function's own name, and the stack that declares
/// both.
const ROLE: &str = "sdk-refusal-class-role";
const FUNCTION: &str = "sdk-refusal-class-probe";
const STACK: &str = "xsvc-sdk-refusal-class-stack";

/// A distribution identifier of the form CloudFront mints (`E` and thirteen characters of its
/// identifier alphabet) that this account holds no distribution under — so a call naming it
/// reaches the service and is refused for the resource being absent, not for the identifier
/// being unreadable.
const ABSENT_DISTRIBUTION: &str = "E3ABSENTID0001";

/// An invalidation identifier the distribution this suite creates holds no invalidation under.
const ABSENT_INVALIDATION: &str = "I2ABSENTBATCHID0000000001";

/// A name CloudFront's model declares NO error shape for, which is therefore raised — and
/// looked up — as the SDK's base error class rather than as a class of its own.
const UNMODELLED_CODE: &str = "NoCloudFrontErrorShapeIsNamedThis";

/// The probe handler: it builds the CloudFront client once at module scope, asks the SDK for
/// the class it exports for the error shape the row expects, makes the call the row names, and
/// reports whether that class is what caught the refusal — and what the refusal itself carried.
const HANDLER: &str = r#"
import boto3
import botocore.exceptions

CLIENT = boto3.client("cloudfront")


def described(error):
    """What the refusal itself carried, as the SDK hands it to a handler."""
    return {
        "raised": type(error).__name__,
        "code": error.response["Error"]["Code"],
        "message": error.response["Error"]["Message"],
        "http_status": error.response["ResponseMetadata"]["HTTPStatusCode"],
        "is_client_error": isinstance(error, botocore.exceptions.ClientError),
    }


def lambda_handler(event, context):
    method = getattr(CLIENT, event["method"])
    modelled = getattr(CLIENT.exceptions, event["shape"])
    report = {
        "refused": True,
        "class_of_shape": modelled.__name__,
        "unmodelled_is_base": (
            CLIENT.exceptions.from_code(event["unmodelled"])
            is botocore.exceptions.ClientError
        ),
    }
    try:
        method(**event["parameters"])
    except modelled as error:
        report["caught_by_class"] = True
        report.update(described(error))
        return report
    except botocore.exceptions.ClientError as error:
        report["caught_by_class"] = False
        report.update(described(error))
        return report
    report["refused"] = False
    return report
"#;

/// One Lambda `restJson1` call over the shared request helper.
async fn lambda(router: &MultiServiceRouter, method: &str, uri: &str, body: &str) -> Answer {
    call(
        router,
        "lambda",
        &format!("lambda.{REGION}.amazonaws.com"),
        method,
        uri,
        "application/json",
        body,
    )
    .await
}

/// The stack that declares the execution role and the probe function. The role is granted the
/// invalidation and distribution reads on EVERY distribution, so a call naming one the account
/// does not hold is judged permitted and reaches the service.
fn probe_stack() -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "ProbeRole": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": ROLE,
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": {"Service": "lambda.amazonaws.com"},
                            "Action": "sts:AssumeRole",
                        }],
                    },
                    "Policies": [{
                        "PolicyName": "read-and-invalidate-any-distribution",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Effect": "Allow",
                                "Action": [
                                    "cloudfront:CreateInvalidation",
                                    "cloudfront:GetInvalidation",
                                    "cloudfront:ListInvalidations",
                                    "cloudfront:GetDistribution",
                                ],
                                "Resource": "*",
                            }],
                        },
                    }],
                },
            },
            "Probe": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION,
                    "Runtime": "python3.12",
                    "Handler": "index.lambda_handler",
                    "Role": {"Fn::GetAtt": ["ProbeRole", "Arn"]},
                    "Timeout": 30,
                    "Code": {"ZipFile": HANDLER},
                },
            },
        },
        "Outputs": {
            "FunctionName": {"Value": {"Ref": "Probe"}},
        },
    })
    .to_string()
}

/// One row: the call the handler makes, the error shape the service's model declares for that
/// refusal, and the status that shape is answered at.
struct Row {
    case: &'static str,
    /// The client method the handler calls.
    method: &'static str,
    /// The parameters it calls with, built around the distribution this suite created.
    parameters: fn(&str) -> Value,
    /// The error shape the model declares for this refusal — the class the refusal has to be.
    shape: &'static str,
    /// The status that shape is answered at, as the model states it.
    status: u16,
}

const ROWS: &[Row] = &[
    Row {
        case: "create_invalidation on a distribution the account holds none under",
        method: "create_invalidation",
        parameters: |_distribution| {
            json!({
                "DistributionId": ABSENT_DISTRIBUTION,
                "InvalidationBatch": {
                    "Paths": {"Items": ["/assets/*"]},
                    "CallerReference": "refusal-class-absent-distribution",
                },
            })
        },
        shape: "NoSuchDistribution",
        status: 404,
    },
    Row {
        case: "create_invalidation naming a value that is no invalidation path",
        method: "create_invalidation",
        parameters: |distribution| {
            json!({
                "DistributionId": distribution,
                "InvalidationBatch": {
                    // "The path is relative to the distribution and must begin with a slash":
                    // a value that does not names no object to invalidate.
                    "Paths": {"Items": ["assets/app.js"]},
                    "CallerReference": "refusal-class-unrooted-path",
                },
            })
        },
        shape: "InvalidArgument",
        status: 400,
    },
    Row {
        case: "get_invalidation for an invalidation the distribution does not hold",
        method: "get_invalidation",
        parameters: |distribution| {
            json!({ "DistributionId": distribution, "Id": ABSENT_INVALIDATION })
        },
        shape: "NoSuchInvalidation",
        status: 404,
    },
    Row {
        case: "get_distribution on a distribution the account holds none under",
        method: "get_distribution",
        parameters: |_distribution| json!({ "Id": ABSENT_DISTRIBUTION }),
        shape: "NoSuchDistribution",
        status: 404,
    },
    Row {
        case: "list_invalidations under a distribution the account holds none under",
        method: "list_invalidations",
        parameters: |_distribution| json!({ "DistributionId": ABSENT_DISTRIBUTION }),
        shape: "NoSuchDistribution",
        status: 404,
    },
];

#[tokio::test]
async fn a_refusal_a_handler_reads_is_the_class_named_after_its_error_shape() {
    let router = build_router();
    create_origin(&router).await;
    let distribution = create_distribution(&router, "refusal-class-reference").await;
    let outputs = deployed(&router, STACK, &probe_stack()).await;
    let function = outputs
        .get("FunctionName")
        .cloned()
        .unwrap_or_else(|| FUNCTION.to_string());
    wait_until_active(&router, &function).await;

    for row in ROWS {
        let case = row.case;
        let event = json!({
            "method": row.method,
            "parameters": (row.parameters)(&distribution),
            "shape": row.shape,
            "unmodelled": UNMODELLED_CODE,
        });
        let answered = lambda(
            &router,
            "POST",
            &format!("/2015-03-31/functions/{function}/invocations"),
            &event.to_string(),
        )
        .await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: the invocation must be served, got {} with {}",
            answered.status, answered.body
        );
        assert!(
            answered.function_error.is_none(),
            "case {case:?}: the handler must not fault, got {:?} with {}",
            answered.function_error,
            answered.body
        );
        let payload: Value =
            serde_json::from_str(&answered.body).unwrap_or_else(|_| json!(answered.body));
        assert_eq!(
            payload["refused"],
            json!(true),
            "case {case:?}: the call must be refused by the service, got {payload}"
        );
        // The CLASS the SDK exports for that shape is named after the shape itself.
        assert_eq!(
            payload["class_of_shape"],
            json!(row.shape),
            "case {case:?}: expected the class for the shape to be named {:?}, got {payload}",
            row.shape
        );
        // And it is the class the refusal was raised as: caught by that `except` clause, and
        // reporting that name.
        assert_eq!(
            payload["caught_by_class"],
            json!(true),
            "case {case:?}: expected `except client.exceptions.{}` to catch the refusal, got \
             {payload}",
            row.shape
        );
        assert_eq!(
            payload["raised"],
            json!(row.shape),
            "case {case:?}: expected the raised class to be named {:?}, got {payload}",
            row.shape
        );
        // A modelled class is still a ClientError, so a handler catching the base class
        // catches it too.
        assert_eq!(
            payload["is_client_error"],
            json!(true),
            "case {case:?}: a modelled refusal is still a ClientError, got {payload}"
        );
        // The refusal is the service's own: its code, its sentence and its status.
        assert_eq!(
            payload["code"],
            json!(row.shape),
            "case {case:?}: expected the error code {:?}, got {payload}",
            row.shape
        );
        assert_eq!(
            payload["http_status"],
            json!(row.status),
            "case {case:?}: expected the status {} the model states for that shape, got \
             {payload}",
            row.status
        );
        assert!(
            payload["message"].as_str().is_some_and(|held| !held.is_empty()),
            "case {case:?}: the refusal must carry the service's own sentence, got {payload}"
        );
        // The boundary of the catalogue: a name the model declares no error shape for has no
        // class of its own.
        assert_eq!(
            payload["unmodelled_is_base"],
            json!(true),
            "case {case:?}: a code no error shape of the service is declared for must be the \
             SDK's base class, got {payload}"
        );
    }
}
