// SPDX-License-Identifier: CC0-1.0

//! WHICH services the AWS SDK a managed Python runtime puts on a handler's import path can
//! address, and WHOSE STORE the listing it makes with one of them is answered out of, with the
//! whole emulator assembled.
//!
//! RULE: the SDK a managed `python3.x` runtime supplies carries the service data of every
//! service this emulator serves, so `boto3.client(<token>)` for any one of them — under each
//! spelling a client names that service by — builds a client whose call really leaves the
//! sandbox: the named service answers it out of its own records, so what the handler reads back
//! is that service's own HTTP status and either its answer or its refusal, decided against the
//! identity-based documents in force on the function's execution role. A client-side failure of
//! the SDK itself (`botocore.exceptions.UnknownServiceError`, raised before anything is signed)
//! is neither outcome, and a handler given it for a service this emulator serves never made the
//! call at all — so the arm of an IAM test that withholds a grant could not be told from the arm
//! that gives it.
//!
//! Several crates have to agree for that to hold — CloudFormation archives the inline handler
//! into a deployment package and deploys the execution role beside it, IAM holds that role and
//! its documents, the token service vends the session the handler's calls are signed as, Lambda
//! runs the handler and supplies it the SDK, and each addressed service answers out of its own
//! store — so the suite lives here: a crate-local suite can only ever see one side of it.
//!
//! The rows differ along what the rule generalises over: WHICH service the handler's client is
//! built for, WHICH protocol that service binds its requests with (`restJson1` and two AWS JSON
//! versions), and WHICH spelling of its token the client names — CloudWatch Logs is reached
//! under both, and both rows must see the one store. Every row's token is checked against the
//! runtime's own inventory of reachable services ([`reachable_services`]) before the rows are
//! read, so the table is grounded in that inventory rather than written out here.
//!
//! Each row states what the role granting nothing but its log writes owes it: the listings whose
//! own model declares `AccessDeniedException` must be refused it at the modelled status, naming
//! the action and blaming the run's own session, while for a listing whose model declares no
//! authorization error what the rule requires is that the NAMED SERVICE answered at all. With
//! the listings then granted, every row must be served, and what each served listing reports is
//! held against the resources this suite seeded through the owning service's own API — so a
//! listing answered out of the wrong store, or an empty one, fails here.
//!
//! What the neighbours pin and this one does not repeat: the Lambda crate's
//! `handler_sdk_service_inventory` pins WHICH tokens the inventory answers to, with a probe that
//! presigns rather than calls, and `handler_vector_and_agent_runtime_grants` pins the grant
//! moving under a FIXED pair of services; `cfn_role_widening_update_decides_a_functions_listings`
//! moves that grant through a stack update. What only this suite reads is the rule across the
//! service TOKEN itself: several services, three protocols and one service under both of its
//! spellings, answering several calls of ONE invocation out of their own stores — so a token
//! that is merely listed in the inventory while nothing serves the calls made under it fails
//! here.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::execution::aws_api::reachable_services;
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{elements, form_encode, opt_element, physical_ids_of};

/// The Region every request here is signed for, and so the Region every resource belongs to.
const REGION: &str = "us-east-1";

/// Where Lambda binds the function collection, its configuration read and one invocation.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The role the probe function runs as, the function itself, and the stack that deploys both.
const EXEC_ROLE: &str = "xsvc-sandbox-sdk-exec";
const FUNCTION: &str = "xsvc-sandbox-sdk-probe";
const STACK: &str = "xsvc-sandbox-sdk-probes";

/// The logical id the template declares the function under.
const FUNCTION_RESOURCE: &str = "Probe";

/// The `StackStatus` the deployment must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The `State` an invocation needs the function to be in ("the function is ready to be
/// invoked", `FunctionConfiguration$State`).
const STATE_ACTIVE: &str = "Active";

/// The status Lambda answers an invocation it has NOT served with: the concurrent-execution
/// limit of this process was reached (`TooManyRequestsException`, `@httpError(429)`). Whether
/// one arrives is decided by what the rest of this process is running at that instant, so a
/// caller looks again rather than reading a verdict out of an invocation that never ran.
const UNSERVED: u16 = 429;

/// The runtime and handler the function is created with: a managed Python runtime, which is the
/// one that supplies the SDK the handler imports.
const RUNTIME: &str = "python3.12";
const HANDLER: &str = "index.handler";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run
/// needs on any host, because what is under test is which services a handler can reach and
/// never how fast an interpreter gets through the calls.
const FUNCTION_TIMEOUT: u64 = 120;

/// How often to look again for work the clock owes — the stack settling, the function reaching
/// [`STATE_ACTIVE`], an invocation being served — and how many looks to take. Every span waited
/// for is a service's own, so nothing here sleeps for a fixed one, and the deadline is generous
/// because every case asserts the CONDITION and never the time it took.
const LOOK: Duration = Duration::from_millis(50);
/// See [`LOOK`]: 3600 looks of 50ms, a generous deadline no healthy host comes near.
const LOOKS: usize = 3_600;

/// The log writes a function's runtime performs, which the execution role carries whatever a
/// phase withholds ("Actions defined by Amazon CloudWatch Logs").
const LOG_ACTIONS: [&str; 3] = [
    "logs:CreateLogGroup",
    "logs:CreateLogStream",
    "logs:PutLogEvents",
];

/// The name of the inline document the stack puts on the role, and of the one this suite adds to
/// it afterwards to widen what the handler's calls may do.
const BASE_POLICY: &str = "probe-grants";
const WIDENED_POLICY: &str = "probe-listings";

/// The error code AWS answers a call no document in force allows with — the AWS-common
/// `AccessDeniedException`, which is the code the SDK hands the handler.
const ACCESS_DENIED: &str = "AccessDeniedException";

/// The vector buckets, queues and log groups this suite seeds through each owning service's own
/// API, which are what the handler's served listings must report. `VectorBucketName` is
/// `@length(min: 3, max: 63)` with pattern `^[a-z0-9][a-z0-9-]{1,61}[a-z0-9]$`.
const VECTOR_BUCKETS: [&str; 2] = [
    "xsvc-sandbox-sdk-vectors-one",
    "xsvc-sandbox-sdk-vectors-two",
];
const QUEUES: [&str; 2] = ["xsvc-sandbox-sdk-queue-one", "xsvc-sandbox-sdk-queue-two"];
const LOG_GROUPS: [&str; 2] = ["xsvc-sandbox-sdk-group-one", "xsvc-sandbox-sdk-group-two"];

/// What the role granting nothing but its log writes owes one row.
enum Decision {
    /// The listing's own model declares `AccessDeniedException`, so a role no statement of which
    /// allows the action must be refused it at the status that model declares.
    RefusedAt(u64),
    /// The listing's model declares no authorization error of its own, so what a refusal of it
    /// answers is not that model's to say and this suite does not invent one: what the rule
    /// requires of such a row is that the NAMED SERVICE answered it — a status that service
    /// sent, and never the SDK failing to make the call — and that a refusal, where that service
    /// takes one, is the AWS-common access refusal naming the action.
    ServiceAnswers,
}

/// One row: the service a client is built for, the spelling of its token, the listing it asks
/// for, the output member that listing carries, the IAM action it stands on, what a role
/// granting nothing owes it, and the resources a served listing must report.
struct Probe {
    case: &'static str,
    service: &'static str,
    method: &'static str,
    member: &'static str,
    action: &'static str,
    ungranted: Decision,
    /// Whether the listing's model marks the member above `required`, so a served answer must
    /// carry it rather than leave it out.
    member_required: bool,
    /// The resources this suite seeded through the owning service's own API, every one of which
    /// a served listing has to report.
    seeded: &'static [&'static str],
}

/// The services one invocation addresses. Each token is the one an SDK client names that service
/// by, and every one of them is checked against the runtime's own inventory
/// ([`reachable_services`]) before the rows are read, so a token this emulator does not serve
/// cannot sit here unnoticed.
const PROBES: [Probe; 5] = [
    // `smithy_query(query="operation",operation="ListVectorBuckets")` on the s3vectors model:
    // output member `vectorBuckets` required, errors include `AccessDeniedException` with
    // `httpError` 403.
    Probe {
        case: "s3vectors ListVectorBuckets",
        service: "s3vectors",
        method: "list_vector_buckets",
        member: "vectorBuckets",
        action: "s3vectors:ListVectorBuckets",
        ungranted: Decision::RefusedAt(403),
        member_required: true,
        seeded: &VECTOR_BUCKETS,
    },
    // `smithy_query(query="operation",operation="ListSessions")` on the Bedrock Agent runtime
    // model: output member `sessionSummaries` required, `AccessDeniedException` with `httpError`
    // 403. That plane publishes its session actions under `bedrock`, its model's
    // `awsService.arnNamespace`, which is the prefix of its IAM actions.
    Probe {
        case: "bedrock-agent-runtime ListSessions",
        service: "bedrock-agent-runtime",
        method: "list_sessions",
        member: "sessionSummaries",
        action: "bedrock:ListSessions",
        ungranted: Decision::RefusedAt(403),
        member_required: true,
        seeded: &[],
    },
    // `smithy_query(query="operation",operation="ListQueues")` on the Amazon SQS model declares
    // no authorization error and does not mark `QueueUrls` required.
    Probe {
        case: "sqs ListQueues",
        service: "sqs",
        method: "list_queues",
        member: "QueueUrls",
        action: "sqs:ListQueues",
        ungranted: Decision::ServiceAnswers,
        member_required: false,
        seeded: &QUEUES,
    },
    // `smithy_query(query="operation",operation="DescribeLogGroups")` on the CloudWatch Logs
    // model declares no authorization error and does not mark `logGroups` required. The next row
    // is this same service under the other token a client reaches it by, so both must report the
    // groups this suite seeded once.
    Probe {
        case: "logs DescribeLogGroups",
        service: "logs",
        method: "describe_log_groups",
        member: "logGroups",
        action: "logs:DescribeLogGroups",
        ungranted: Decision::ServiceAnswers,
        member_required: false,
        seeded: &LOG_GROUPS,
    },
    Probe {
        case: "cloudwatchlogs DescribeLogGroups",
        service: "cloudwatchlogs",
        method: "describe_log_groups",
        member: "logGroups",
        action: "logs:DescribeLogGroups",
        ungranted: Decision::ServiceAnswers,
        member_required: false,
        seeded: &LOG_GROUPS,
    },
];

/// The case name of the one row whose served COUNT this suite can hold against the owning
/// service's own answer: nothing but this suite creates a vector bucket, so the two reads cannot
/// disagree over a resource some other part of the run made.
const VECTOR_CASE: &str = "s3vectors ListVectorBuckets";

/// The handler every invocation runs: for each probe it is handed, it builds a client for that
/// service token with no credentials of its own — so the call is signed as the session of the
/// function's own execution role — asks for that service's listing, and reports what came back:
/// the service's answer, the service's refusal, or the SDK's own failure to make the call.
const PROBE_HANDLER: &str = r##"
import json

import boto3
from botocore.exceptions import ClientError, UnknownServiceError


def one(asked):
    try:
        client = boto3.client(asked["service"])
        answered = getattr(client, asked["method"])()
    except ClientError as refused:
        return {
            "outcome": "refused",
            "error_code": refused.response["Error"]["Code"],
            "error_message": refused.response["Error"]["Message"],
            "http_status": refused.response["ResponseMetadata"]["HTTPStatusCode"],
        }
    except UnknownServiceError as unknown:
        return {
            "outcome": "sdk_failure",
            "raised": "UnknownServiceError",
            "detail": str(unknown),
        }
    except Exception as raised:
        return {
            "outcome": "sdk_failure",
            "raised": type(raised).__name__,
            "detail": str(raised),
        }
    held = answered.get(asked["member"])
    kind = "absent"
    count = -1
    if held is not None:
        kind = type(held).__name__
    if isinstance(held, list):
        count = len(held)
    return {
        "outcome": "answered",
        "http_status": answered["ResponseMetadata"]["HTTPStatusCode"],
        "member_kind": kind,
        "count": count,
        "listed": json.dumps(held, default=str),
    }


def handler(event, context):
    reports = {}
    for asked in event["probes"]:
        reports[asked["case"]] = one(asked)
    return {"reports": reports}
"##;

/// The emulator as it is ASSEMBLED, on one engine: the handler's own SDK calls are dispatched
/// back into this same router, so every service they address is one this suite reads directly.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the credential
    // scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request and where a handler reads its Region from.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
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

/// One awsQuery call that must be accepted, answered with its XML body.
async fn accepted(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = query(router, service, version, action, members).await;
    assert_eq!(
        status, 200,
        "setup: {service} {action} must answer 200, got {status} with {body}"
    );
    body
}

/// The document an answer carried, or `Null` for an empty body.
fn parsed(body: &[u8]) -> Value {
    if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(body).to_string()))
    }
}

/// One Amazon S3 Vectors operation over its `restJson1` wire: a POST to the operation's own URI,
/// which is how that model binds every one of them.
async fn s3vectors(router: &MultiServiceRouter, operation: &str, input: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("/{operation}"))
        .header("host", format!("s3vectors.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("s3vectors"))
        .body(Bytes::from(input.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    (parts.status.as_u16(), parsed(&body))
}

/// One AWS JSON call of the service signed as `signing`: the operation travels in the target
/// header under the service shape's own name and the input document is the body.
async fn aws_json(
    router: &MultiServiceRouter,
    signing: &str,
    json_version: &str,
    target: &str,
    input: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing}.{REGION}.amazonaws.com"))
        .header(
            "content-type",
            format!("application/x-amz-json-{json_version}"),
        )
        .header("x-amz-target", target)
        .header("authorization", authorization(signing))
        .body(Bytes::from(input.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    (parts.status.as_u16(), parsed(&body))
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
    (parts.status.as_u16(), parts.headers, parsed(&body))
}

/// The template the suite deploys: the execution role, granted its log writes and nothing else,
/// and the probe function running as it. The inline source is archived into the deployment
/// package by CloudFormation, which is how a function whose handler is written here comes to
/// exist at all.
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
                        "PolicyName": BASE_POLICY,
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Effect": "Allow",
                                "Action": LOG_ACTIONS,
                                "Resource": "*",
                            }],
                        },
                    }],
                },
            },
            FUNCTION_RESOURCE: {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION,
                    "Runtime": RUNTIME,
                    "Handler": HANDLER,
                    "Role": {"Fn::GetAtt": ["ExecRole", "Arn"]},
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": {"ZipFile": PROBE_HANDLER},
                },
            },
        },
    })
    .to_string()
}

/// Deploy the stack and wait for [`CREATE_COMPLETE`], answering the physical id of each declared
/// resource.
async fn deploy(router: &MultiServiceRouter) -> BTreeMap<String, String> {
    let template = stack_template();
    accepted(
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
    let mut reported = String::new();
    let mut described = String::new();
    for _ in 0..LOOKS {
        let body = accepted(
            router,
            "cloudformation",
            "2010-05-15",
            "DescribeStacks",
            &[("StackName", STACK)],
        )
        .await;
        reported = opt_element(&body, "StackStatus").unwrap_or_default();
        described = body;
        if !reported.ends_with("_IN_PROGRESS") {
            break;
        }
        tokio::time::sleep(LOOK).await;
    }
    let failures = if reported == CREATE_COMPLETE {
        Vec::new()
    } else {
        let events = accepted(
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
    let resources = accepted(
        router,
        "cloudformation",
        "2010-05-15",
        "DescribeStackResources",
        &[("StackName", STACK)],
    )
    .await;
    physical_ids_of(&resources)
}

/// Wait until Lambda reports the probe function ready to be invoked.
async fn wait_until_active(router: &MultiServiceRouter) {
    let mut last = Value::Null;
    for _ in 0..LOOKS {
        let (_, _, configuration) = lambda(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{FUNCTION}/configuration"),
            &[],
        )
        .await;
        if configuration["State"].as_str() == Some(STATE_ACTIVE) {
            return;
        }
        last = configuration;
        tokio::time::sleep(LOOK).await;
    }
    panic!("setup: Lambda must report {FUNCTION:?} {STATE_ACTIVE}, last reported {last}");
}

/// Create the resources the handler's served listings have to report, each through the owning
/// service's own API: the vector buckets, the queues and the log groups.
async fn seed(router: &MultiServiceRouter) {
    for bucket in VECTOR_BUCKETS {
        let (status, created) = s3vectors(
            router,
            "CreateVectorBucket",
            &json!({"vectorBucketName": bucket}),
        )
        .await;
        assert_eq!(
            status, 200,
            "setup: CreateVectorBucket({bucket:?}) must answer 200, got {status} with {created}"
        );
    }
    for queue in QUEUES {
        let (status, created) = aws_json(
            router,
            "sqs",
            "1.0",
            "AmazonSQS.CreateQueue",
            &json!({"QueueName": queue}),
        )
        .await;
        assert_eq!(
            status, 200,
            "setup: CreateQueue({queue:?}) must answer 200, got {status} with {created}"
        );
    }
    for group in LOG_GROUPS {
        let (status, created) = aws_json(
            router,
            "logs",
            "1.1",
            "Logs_20140328.CreateLogGroup",
            &json!({"logGroupName": group}),
        )
        .await;
        assert_eq!(
            status, 200,
            "setup: CreateLogGroup({group:?}) must answer 200, got {status} with {created}"
        );
    }
}

/// The probes one invocation is handed: every row of [`PROBES`].
fn asked_probes() -> Value {
    PROBES
        .iter()
        .map(|probe| {
            json!({
                "case": probe.case,
                "service": probe.service,
                "method": probe.method,
                "member": probe.member,
            })
        })
        .collect()
}

/// Invoke the probe function with every probe and answer the reports it returned, keyed by each
/// row's own case name. Looks again while this process answers that it did not run the
/// invocation at all ([`UNSERVED`]).
async fn probed(router: &MultiServiceRouter, phase: &str) -> Value {
    let payload = json!({"probes": asked_probes()}).to_string();
    for _ in 0..LOOKS {
        let (status, headers, answered) = lambda(
            router,
            "POST",
            &format!("{FUNCTIONS_URI}/{FUNCTION}/invocations"),
            payload.as_bytes(),
        )
        .await;
        if status == UNSERVED {
            tokio::time::sleep(LOOK).await;
            continue;
        }
        assert_eq!(
            status, 200,
            "phase {phase:?}: expected HTTP 200 from Invoke, got {status} with {answered}"
        );
        let raised = headers
            .get("x-amz-function-error")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        assert_eq!(
            raised, None,
            "phase {phase:?}: the handler must run to completion, got {raised:?} with {answered}"
        );
        return answered.get("reports").cloned().unwrap_or_else(|| {
            panic!("phase {phase:?}: the handler must report every probe, got {answered}")
        });
    }
    panic!("phase {phase:?}: expected an invocation of {FUNCTION:?} to be served");
}

/// One row's report, or a failure naming the phase it is missing from.
fn report(reports: &Value, phase: &str, case: &str) -> Value {
    reports
        .get(case)
        .cloned()
        .unwrap_or_else(|| panic!("phase {phase:?} case {case:?}: no report for it in {reports}"))
}

/// The session a run of this function speaks for: a session of its execution role named after
/// the function, which is the identity an AWS refusal of one of its calls blames.
fn run_session() -> String {
    format!(":assumed-role/{EXEC_ROLE}/{FUNCTION}")
}

/// Assert that one row's report carries the NAMED SERVICE's own answer rather than the SDK
/// failing to make the call: an outcome that service decided, at a status it sent.
fn assert_the_service_answered(answered: &Value, phase: &str, case: &str) {
    let outcome = answered["outcome"].as_str().unwrap_or_default();
    assert!(
        outcome == "answered" || outcome == "refused",
        "phase {phase:?} case {case:?}: expected the named service's own answer rather than a \
         client-side SDK failure, got {answered}"
    );
    let status = answered["http_status"].as_u64().unwrap_or_default();
    assert!(
        (200..500).contains(&status),
        "phase {phase:?} case {case:?}: expected the status the service answered, got {answered}"
    );
}

/// Assert that a refusal is the AWS access refusal, naming the action asked for and blaming the
/// run's own session.
fn assert_access_refusal(answered: &Value, phase: &str, case: &str, action: &str) {
    assert_eq!(
        answered["error_code"],
        json!(ACCESS_DENIED),
        "phase {phase:?} case {case:?}: expected the AWS access refusal, got {answered}"
    );
    let message = answered["error_message"].as_str().unwrap_or_default();
    assert!(
        message.contains(action),
        "phase {phase:?} case {case:?}: the refusal must name the action {action:?}, got \
         {answered}"
    );
    assert!(
        message.contains(&run_session()),
        "phase {phase:?} case {case:?}: the refusal must blame the run's own session {:?}, got \
         {answered}",
        run_session()
    );
}

/// How many vector buckets Amazon S3 Vectors itself reports for this account right now — what a
/// handler allowed to make that listing has to count.
async fn vector_bucket_count(router: &MultiServiceRouter, phase: &str) -> u64 {
    let (status, answered) = s3vectors(router, "ListVectorBuckets", &json!({})).await;
    assert_eq!(
        status, 200,
        "phase {phase:?}: expected HTTP 200 from ListVectorBuckets, got {status} with {answered}"
    );
    let held = answered["vectorBuckets"]
        .as_array()
        .map(Vec::len)
        .unwrap_or_else(|| panic!("phase {phase:?}: no vectorBuckets list in {answered}"));
    u64::try_from(held).expect("a bucket count fits a u64")
}

/// Every row: a client for each service token the runtime serves makes a real call, decided by
/// the service it names while the role grants only its log writes, and served out of that
/// service's own store once the listings are granted.
#[tokio::test]
async fn a_handlers_sdk_resolves_every_service_this_emulator_serves() {
    let router = build_router();
    let reachable = reachable_services();
    for probe in &PROBES {
        assert!(
            reachable.contains(&probe.service),
            "case {:?}: the runtime must make {:?} reachable, it names {reachable:?}",
            probe.case,
            probe.service
        );
    }

    let physical = deploy(&router).await;
    let reported = physical.get(FUNCTION_RESOURCE).cloned().unwrap_or_default();
    assert_eq!(
        reported, FUNCTION,
        "setup: the stack must report the probe function as {FUNCTION:?}, got {reported:?} in \
         {physical:?}"
    );
    wait_until_active(&router).await;
    seed(&router).await;

    // ---- The role grants nothing but its log writes: every client still resolves, and every
    // ---- call carries the decision the addressed service took.
    let phase = "the role granted only its log writes";
    let reports = probed(&router, phase).await;
    for probe in &PROBES {
        let case = probe.case;
        let answered = report(&reports, phase, case);
        assert_the_service_answered(&answered, phase, case);
        match probe.ungranted {
            Decision::RefusedAt(modelled) => {
                assert_eq!(
                    (
                        answered["outcome"].clone(),
                        answered["http_status"].clone()
                    ),
                    (json!("refused"), json!(modelled)),
                    "phase {phase:?} case {case:?}: the listing's model declares \
                     {ACCESS_DENIED} at {modelled}, so a role allowing the action nowhere must \
                     be refused it, got {answered}"
                );
                assert_access_refusal(&answered, phase, case, probe.action);
            }
            Decision::ServiceAnswers => {
                if answered["outcome"] == json!("refused") {
                    assert_access_refusal(&answered, phase, case, probe.action);
                }
            }
        }
    }

    // ---- The same calls with every listing granted: each is served out of the named service's
    // ---- own store.
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": PROBES.iter().map(|probe| probe.action).collect::<Vec<_>>(),
            "Resource": "*",
        }],
    })
    .to_string();
    accepted(
        &router,
        "iam",
        "2010-05-08",
        "PutRolePolicy",
        &[
            ("RoleName", EXEC_ROLE),
            ("PolicyName", WIDENED_POLICY),
            ("PolicyDocument", &document),
        ],
    )
    .await;

    let phase = "the role granted every listing";
    let reports = probed(&router, phase).await;
    for probe in &PROBES {
        let case = probe.case;
        let answered = report(&reports, phase, case);
        assert_the_service_answered(&answered, phase, case);
        assert_eq!(
            (answered["outcome"].clone(), answered["http_status"].clone()),
            (json!("answered"), json!(200)),
            "phase {phase:?} case {case:?}: expected the service's own 200 answer, got {answered}"
        );
        let kind = answered["member_kind"].as_str().unwrap_or_default();
        if probe.member_required {
            assert_eq!(
                kind, "list",
                "phase {phase:?} case {case:?}: the model marks {:?} required, so a served \
                 answer must carry the list, got {answered}",
                probe.member
            );
        } else {
            assert!(
                kind == "list" || kind == "absent",
                "phase {phase:?} case {case:?}: expected the {:?} member to be a list where the \
                 answer carries one, got {answered}",
                probe.member
            );
        }
        // What the served listing reported, held against the resources this suite created
        // through the owning service's own API: a listing answered out of another store, or an
        // empty one, names none of them.
        let listed = answered["listed"].as_str().unwrap_or_default();
        for seeded in probe.seeded {
            assert!(
                listed.contains(seeded),
                "phase {phase:?} case {case:?}: the served listing must report {seeded:?}, which \
                 this suite created through that service's own API, got {answered}"
            );
        }
    }
    // The one count this suite can hold against the owning service's own answer, read through
    // that service's API rather than written here.
    let served = report(&reports, phase, VECTOR_CASE);
    assert_eq!(
        served["count"],
        json!(vector_bucket_count(&router, phase).await),
        "phase {phase:?} case {VECTOR_CASE:?}: expected the count Amazon S3 Vectors itself \
         reports, got {served}"
    );
}
