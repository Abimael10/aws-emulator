// SPDX-License-Identifier: CC0-1.0

//! What a RUNNING function's own AWS SDK can do with AMAZON RDS, with the whole emulator
//! assembled.
//!
//! RULE: the AWS SDK a managed runtime supplies reaches every service this emulator serves
//! that the SDK seam binds, Amazon RDS included — so a handler's `boto3.client("rds")` call is
//! built, signed and dispatched into this emulator, answered from that service's own records,
//! and refused with that service's own error code when it names a database it does not hold.
//! A service token the seam does not bind makes every call through it raise
//! `UnknownServiceError` instead, which is what a credential-rotation handler's `setSecret`
//! step runs into: changing an RDS master credential IS `ModifyDBInstance` on the instance the
//! secret belongs to ("ModifyDBInstance — modifies settings for a DB instance", that service's
//! modelled documentation), the one way to put a new master password in force without a
//! database connection of its own.
//!
//! Four crates have to agree for that to hold — Lambda runs the handler and supplies it the
//! SDK, IAM answers for the execution role, CloudFormation deploys the role and the function
//! together, and Amazon RDS owns the database the call acts on — so the suite lives here: a
//! crate-local suite can only ever see one side of it.
//!
//! The rows differ along what the rule generalises over: WHICH operation the handler calls
//! (the two reads and the two modifies the seam binds), whether it names a resource that
//! exists, and whether what it names is bound at all (an operation the seam does not bind, and
//! a parameter it does not carry) — and one row's change is read back out of Amazon RDS
//! itself, so what the call reached is that service's own record and not the answer it echoed.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, elements, form_encode, opt_element, physical_ids_of};

/// The Region every request here is signed for, and so the Region the database lives in.
const REGION: &str = "us-west-2";

/// The DB instance and the DB cluster every row acts on, and the credential they were created
/// with.
const INSTANCE: &str = "xsvc-in-fn-rds-instance";
const CLUSTER: &str = "xsvc-in-fn-rds-cluster";
const MASTER_USERNAME: &str = "appadmin";
const MASTER_PASSWORD: &str = "initial-Password-1";

/// The engine and class the instance is created with, which a read of it must report back.
const INSTANCE_ENGINE: &str = "mysql";
const INSTANCE_CLASS: &str = "db.t3.micro";
const CLUSTER_ENGINE: &str = "aurora-mysql";

/// An identifier Amazon RDS holds nothing under, which is what a row names to reach that
/// service's own refusal.
const ABSENT_INSTANCE: &str = "xsvc-in-fn-rds-nothing";

/// The error code and status Amazon RDS refuses a read of a DB instance it does not hold with
/// (`DBInstanceNotFoundFault`, awsQueryError code `DBInstanceNotFound`, httpError 404 —
/// `smithy_query(query="operation",operation="DescribeDBInstances")`).
const INSTANCE_NOT_FOUND: &str = "DBInstanceNotFound";
const NOT_FOUND_STATUS: u64 = 404;

/// The new password one row puts in force, and the retention one row changes — the change read
/// back out of Amazon RDS itself.
const ROTATED_PASSWORD: &str = "rotated-Password-2";
const RETENTION_DAYS: i64 = 7;

/// The role the probe function runs as, the function itself and the stack that deploys them.
const EXEC_ROLE: &str = "xsvc-in-fn-rds-exec";
const FUNCTION: &str = "xsvc-in-fn-rds-probe";
const STACK: &str = "xsvc-in-fn-rds-probes";

/// The `StackStatus` that stack must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The `DBInstanceStatus` an instance that serves connections is in, which this suite waits
/// for before the rows run.
const AVAILABLE: &str = "available";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run
/// needs on any host, because what is under test is which calls a handler can make and not how
/// fast an interpreter gets through one.
const FUNCTION_TIMEOUT: u64 = 120;

/// How often to look while waiting for work the clock owes — a stack completing, a function
/// reaching `Active`, a database reaching `available`, a change becoming readable — and how
/// many looks to take. Every span waited for is a service's own, so nothing here sleeps for a
/// fixed one, and the deadline is generous so a loaded host cannot decide the outcome.
const LOOK: Duration = Duration::from_millis(50);
/// See [`LOOK`].
const LOOKS: usize = 2_400;

/// The handler every row runs: it builds an Amazon RDS client with no credentials of its own —
/// so the call is signed as the session of the function's execution role — calls the operation
/// the event names with the parameters the event carries, and reports what came back: the
/// answer, the service's own refusal, or the SDK's own report of a call it did not make.
const PROBE_HANDLER: &str = r##"
import json

import boto3
from botocore.exceptions import ClientError


def handler(event, context):
    client = boto3.client("rds")
    method = event["method"]
    try:
        answered = getattr(client, method)(**event.get("parameters", {}))
    except ClientError as refused:
        return {
            "ok": False,
            "code": refused.response["Error"]["Code"],
            "status": refused.response["ResponseMetadata"]["HTTPStatusCode"],
        }
    except Exception as raised:
        return {"ok": False, "raised": type(raised).__name__, "detail": str(raised)}
    answered.pop("ResponseMetadata", None)
    return {"ok": True, "answered": json.loads(json.dumps(answered, default=str))}
"##;

/// The emulator as it is ASSEMBLED, on one engine: the handler's own SDK call is dispatched
/// back into this same router, so the database it acts on is the one this suite created.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request and what each handler reads its Region from.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One awsQuery call of `service`, answered with the HTTP status and the XML body.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(service))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// One Amazon RDS call, made by this suite as the account's own credential.
async fn rds(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    query(router, "rds", "2014-10-31", action, members).await
}

/// One Lambda `restJson1` call, as its status, its headers and its parsed body.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &[u8],
) -> (u16, http::HeaderMap, Value) {
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(Bytes::from(body.to_vec()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    let parsed = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).to_string()))
    };
    (parts.status.as_u16(), parts.headers, parsed)
}

/// The template the suite deploys: the execution role, granted the database surface a
/// credential handler uses, and the probe function. The inline source is archived into the
/// deployment package by CloudFormation, which is how a function whose handler is written here
/// comes to exist at all.
fn stack_template() -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "ExecRole": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": EXEC_ROLE,
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": {"Service": "lambda.amazonaws.com"},
                            "Action": "sts:AssumeRole",
                        }],
                    },
                    "Policies": [{
                        "PolicyName": "probe-grants",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [
                                {
                                    "Effect": "Allow",
                                    "Action": [
                                        "logs:CreateLogGroup",
                                        "logs:CreateLogStream",
                                        "logs:PutLogEvents",
                                    ],
                                    "Resource": "*",
                                },
                                {
                                    "Effect": "Allow",
                                    "Action": [
                                        "rds:DescribeDBInstances",
                                        "rds:ModifyDBInstance",
                                        "rds:DescribeDBClusters",
                                        "rds:ModifyDBCluster",
                                        "rds:DeleteDBInstance",
                                    ],
                                    "Resource": "*",
                                },
                            ],
                        },
                    }],
                },
            },
            "Probe": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": {"Fn::GetAtt": ["ExecRole", "Arn"]},
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": {"ZipFile": PROBE_HANDLER},
                },
            },
        },
    })
    .to_string()
}

/// Deploy the stack and wait for `CREATE_COMPLETE`, answering the physical id of each declared
/// resource.
async fn deploy(router: &MultiServiceRouter) -> BTreeMap<String, String> {
    let template = stack_template();
    let (status, answered) = query(
        router,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &template),
            ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateStack({STACK:?}) must be accepted, got {status} with {answered}"
    );
    let mut reported = String::new();
    let mut described = String::new();
    for _ in 0..LOOKS {
        let (status, body) = query(
            router,
            "cloudformation",
            "2010-05-15",
            "DescribeStacks",
            &[("StackName", STACK)],
        )
        .await;
        assert_eq!(
            status, 200,
            "setup: DescribeStacks({STACK:?}) must succeed, got {status} with {body}"
        );
        reported = opt_element(&body, "StackStatus").unwrap_or_default();
        described = body;
        if reported == CREATE_COMPLETE || !reported.ends_with("_IN_PROGRESS") {
            break;
        }
        tokio::time::sleep(LOOK).await;
    }
    let failures = if reported == CREATE_COMPLETE {
        Vec::new()
    } else {
        let (_, events) = query(
            router,
            "cloudformation",
            "2010-05-15",
            "DescribeStackEvents",
            &[("StackName", STACK)],
        )
        .await;
        elements(&events, "ResourceStatusReason")
    };
    assert_eq!(
        reported, CREATE_COMPLETE,
        "setup: the stack must reach {CREATE_COMPLETE}, got {described} with resource failures \
         {failures:?}"
    );
    let (status, resources) = query(
        router,
        "cloudformation",
        "2010-05-15",
        "DescribeStackResources",
        &[("StackName", STACK)],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: DescribeStackResources({STACK:?}) must succeed, got {status} with {resources}"
    );
    physical_ids_of(&resources)
}

/// Create the DB instance and the DB cluster every row acts on, and wait until Amazon RDS
/// reports the instance `available` — the state it serves in, owed on that service's own clock.
async fn plant_the_database(router: &MultiServiceRouter) -> String {
    let (status, answered) = rds(
        router,
        "CreateDBInstance",
        &[
            ("DBInstanceIdentifier", INSTANCE),
            ("Engine", INSTANCE_ENGINE),
            ("DBInstanceClass", INSTANCE_CLASS),
            ("AllocatedStorage", "20"),
            ("MasterUsername", MASTER_USERNAME),
            ("MasterUserPassword", MASTER_PASSWORD),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateDBInstance({INSTANCE:?}) must succeed, got {status} with {answered}"
    );
    let (status, answered) = rds(
        router,
        "CreateDBCluster",
        &[
            ("DBClusterIdentifier", CLUSTER),
            ("Engine", CLUSTER_ENGINE),
            ("MasterUsername", MASTER_USERNAME),
            ("MasterUserPassword", MASTER_PASSWORD),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateDBCluster({CLUSTER:?}) must succeed, got {status} with {answered}"
    );
    let mut described = String::new();
    for _ in 0..LOOKS {
        let (status, body) = rds(
            router,
            "DescribeDBInstances",
            &[("DBInstanceIdentifier", INSTANCE)],
        )
        .await;
        assert_eq!(
            status, 200,
            "setup: DescribeDBInstances({INSTANCE:?}) must succeed, got {status} with {body}"
        );
        described = body;
        if element(&described, "DBInstanceStatus") == AVAILABLE {
            return element(&described, "Port");
        }
        tokio::time::sleep(LOOK).await;
    }
    panic!("setup: Amazon RDS must report {INSTANCE:?} {AVAILABLE}, last reported {described}")
}

/// Wait until Lambda reports the probe function `Active`, which is the state an invocation
/// needs.
async fn wait_until_active(router: &MultiServiceRouter) {
    let mut last = Value::Null;
    for _ in 0..LOOKS {
        let (_, _, configuration) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{FUNCTION}/configuration"),
            &[],
        )
        .await;
        if configuration["State"].as_str() == Some("Active") {
            return;
        }
        last = configuration;
        tokio::time::sleep(LOOK).await;
    }
    panic!("setup: Lambda must report {FUNCTION:?} Active, last reported {last}");
}

/// Invoke the probe function with one row's event and answer the report it returned.
async fn probe(router: &MultiServiceRouter, case: &str, event: &Value) -> Value {
    let (status, headers, payload) = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{FUNCTION}/invocations"),
        event.to_string().as_bytes(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the invocation must be served, got {status} with {payload}"
    );
    let raised = headers
        .get("x-amz-function-error")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    assert_eq!(
        raised, None,
        "case {case:?}: the handler must run to completion, got {raised:?} with {payload}"
    );
    payload
}

/// Every row: what a handler's own Amazon RDS client reaches.
#[tokio::test]
async fn a_handlers_own_sdk_reaches_amazon_rds() {
    let router = build_router();
    let port = plant_the_database(&router).await;
    let physical = deploy(&router).await;
    let reported = physical.get("Probe").cloned().unwrap_or_default();
    assert_eq!(
        reported, FUNCTION,
        "setup: the stack must report the probe function as {FUNCTION:?}, got {reported:?}"
    );
    wait_until_active(&router).await;

    // ---- The read of a DB instance: the record Amazon RDS holds, as that service reports it.
    let case = "a read of the DB instance the credential belongs to";
    let report = probe(
        &router,
        case,
        &json!({
            "method": "describe_db_instances",
            "parameters": {"DBInstanceIdentifier": INSTANCE},
        }),
    )
    .await;
    assert_eq!(
        report["ok"], json!(true),
        "case {case:?}: expected the call to be answered, got {report}"
    );
    let instance = report
        .pointer("/answered/DBInstances/0")
        .cloned()
        .unwrap_or(Value::Null);
    for (member, wanted) in [
        ("DBInstanceIdentifier", json!(INSTANCE)),
        ("Engine", json!(INSTANCE_ENGINE)),
        ("DBInstanceClass", json!(INSTANCE_CLASS)),
        ("MasterUsername", json!(MASTER_USERNAME)),
        ("DBInstanceStatus", json!(AVAILABLE)),
    ] {
        let answered = instance.get(member).cloned().unwrap_or(Value::Null);
        assert_eq!(
            answered, wanted,
            "case {case:?}: expected the instance's {member} {wanted}, got {answered} in \
             {instance}"
        );
    }
    // The address a client connects the database at is the one Amazon RDS published for it,
    // read here out of that service's own answer to this suite.
    let answered_port = instance
        .pointer("/Endpoint/Port")
        .and_then(Value::as_i64)
        .map(|held| held.to_string())
        .unwrap_or_default();
    assert_eq!(
        answered_port, port,
        "case {case:?}: expected the endpoint port Amazon RDS published ({port}), got \
         {answered_port:?} in {instance}"
    );

    // ---- The credential change a rotation's `setSecret` step makes.
    let case = "a new master password put in force on the DB instance";
    let report = probe(
        &router,
        case,
        &json!({
            "method": "modify_db_instance",
            "parameters": {
                "DBInstanceIdentifier": INSTANCE,
                "MasterUserPassword": ROTATED_PASSWORD,
                "ApplyImmediately": true,
            },
        }),
    )
    .await;
    assert_eq!(
        report["ok"], json!(true),
        "case {case:?}: expected the change to be accepted, got {report}"
    );
    let answered = report
        .pointer("/answered/DBInstance/DBInstanceIdentifier")
        .cloned()
        .unwrap_or(Value::Null);
    assert_eq!(
        answered,
        json!(INSTANCE),
        "case {case:?}: expected the changed instance to be reported, got {report}"
    );

    // ---- A configuration change the OWNING SERVICE reports afterwards: what the call reached
    // is Amazon RDS's own record, read back here through that service's own API.
    let case = "a configuration change Amazon RDS reports afterwards";
    let report = probe(
        &router,
        case,
        &json!({
            "method": "modify_db_instance",
            "parameters": {
                "DBInstanceIdentifier": INSTANCE,
                "BackupRetentionPeriod": RETENTION_DAYS,
                "ApplyImmediately": true,
            },
        }),
    )
    .await;
    assert_eq!(
        report["ok"], json!(true),
        "case {case:?}: expected the change to be accepted, got {report}"
    );
    let mut held = String::new();
    for _ in 0..LOOKS {
        let (_, body) = rds(
            &router,
            "DescribeDBInstances",
            &[("DBInstanceIdentifier", INSTANCE)],
        )
        .await;
        held = body;
        if element(&held, "BackupRetentionPeriod") == RETENTION_DAYS.to_string() {
            break;
        }
        tokio::time::sleep(LOOK).await;
    }
    assert_eq!(
        element(&held, "BackupRetentionPeriod"),
        RETENTION_DAYS.to_string(),
        "case {case:?}: expected Amazon RDS to report the retention the handler asked for, got \
         {held}"
    );

    // ---- The same two calls on a CLUSTER, which is where an Aurora credential lives.
    let case = "a read of the DB cluster";
    let report = probe(
        &router,
        case,
        &json!({
            "method": "describe_db_clusters",
            "parameters": {"DBClusterIdentifier": CLUSTER},
        }),
    )
    .await;
    assert_eq!(
        report["ok"], json!(true),
        "case {case:?}: expected the call to be answered, got {report}"
    );
    let cluster = report
        .pointer("/answered/DBClusters/0")
        .cloned()
        .unwrap_or(Value::Null);
    for (member, wanted) in [
        ("DBClusterIdentifier", json!(CLUSTER)),
        ("Engine", json!(CLUSTER_ENGINE)),
        ("MasterUsername", json!(MASTER_USERNAME)),
    ] {
        let answered = cluster.get(member).cloned().unwrap_or(Value::Null);
        assert_eq!(
            answered, wanted,
            "case {case:?}: expected the cluster's {member} {wanted}, got {answered} in {cluster}"
        );
    }

    let case = "a new master password put in force on the DB cluster";
    let report = probe(
        &router,
        case,
        &json!({
            "method": "modify_db_cluster",
            "parameters": {
                "DBClusterIdentifier": CLUSTER,
                "MasterUserPassword": ROTATED_PASSWORD,
                "ApplyImmediately": true,
            },
        }),
    )
    .await;
    assert_eq!(
        report["ok"], json!(true),
        "case {case:?}: expected the change to be accepted, got {report}"
    );
    let answered = report
        .pointer("/answered/DBCluster/DBClusterIdentifier")
        .cloned()
        .unwrap_or(Value::Null);
    assert_eq!(
        answered,
        json!(CLUSTER),
        "case {case:?}: expected the changed cluster to be reported, got {report}"
    );

    // ---- A database Amazon RDS does not hold: that service's own refusal reaches the
    // handler, which catches it as the SDK's `ClientError`.
    let case = "a read of a DB instance Amazon RDS does not hold";
    let report = probe(
        &router,
        case,
        &json!({
            "method": "describe_db_instances",
            "parameters": {"DBInstanceIdentifier": ABSENT_INSTANCE},
        }),
    )
    .await;
    assert_eq!(
        report,
        json!({"ok": false, "code": INSTANCE_NOT_FOUND, "status": NOT_FOUND_STATUS}),
        "case {case:?}: expected Amazon RDS's own refusal, got {report}"
    );

    // ---- What the seam does NOT bind is reported as a call that was not made, naming it,
    // rather than sent with something guessed at or dropped.
    let case = "an operation the runtime's SDK does not bind";
    let report = probe(
        &router,
        case,
        &json!({
            "method": "delete_db_instance",
            "parameters": {"DBInstanceIdentifier": INSTANCE, "SkipFinalSnapshot": true},
        }),
    )
    .await;
    assert_eq!(
        report["ok"], json!(false),
        "case {case:?}: expected the call not to be made, got {report}"
    );
    let detail = report["detail"].as_str().unwrap_or_default().to_lowercase();
    assert!(
        detail.contains("deletedbinstance") && detail.contains("describedbinstances"),
        "case {case:?}: expected the report to name the call and the bound operations, got \
         {report}"
    );

    let case = "a parameter the runtime's SDK does not carry";
    let report = probe(
        &router,
        case,
        &json!({
            "method": "modify_db_instance",
            "parameters": {
                "DBInstanceIdentifier": INSTANCE,
                "TdeCredentialArn": "arn:aws:iam::000000000000:key/tde",
            },
        }),
    )
    .await;
    assert_eq!(
        report["ok"], json!(false),
        "case {case:?}: expected the call not to be made, got {report}"
    );
    let detail = report["detail"].as_str().unwrap_or_default();
    assert!(
        detail.contains("TdeCredentialArn"),
        "case {case:?}: expected the report to name the parameter, got {report}"
    );

    // Nothing the rows did removed the database: the resources every row addressed are still
    // the ones Amazon RDS holds.
    for (action, member, identifier) in [
        ("DescribeDBInstances", "DBInstanceIdentifier", INSTANCE),
        ("DescribeDBClusters", "DBClusterIdentifier", CLUSTER),
    ] {
        let (status, body) = rds(&router, action, &[(member, identifier)]).await;
        assert_eq!(
            (status, element(&body, member).as_str()),
            (200, identifier),
            "after the rows: Amazon RDS must still hold {identifier:?}, got {status} with {body}"
        );
    }
}
