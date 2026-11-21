// SPDX-License-Identifier: CC0-1.0

//! A function target reached AS the role the target names RUNS, and its own side effect is
//! where that is observed — end to end over the wire through the real `MultiServiceRouter`
//! with Lambda, Amazon SQS, IAM and the token service mounted beside EventBridge on one
//! engine.
//!
//! The RULE asserted here: EventBridge invokes a rule's target as the identity the target
//! names — `Target$RoleArn` when it names one — so for a function target, whose call AWS
//! otherwise governs by the function's own resource-based policy ("for Lambda functions,
//! Amazon SNS topics and Amazon SQS queues, EventBridge relies on resource-based policies",
//! `PutTargets`), the ROLE is what the delivery is decided by: within one account "the
//! resource-based policy and the identity-based policy are evaluated together, and access is
//! granted if either allows it", so a role EventBridge may assume and which may invoke the
//! function carries the delivery even though the function itself has granted this service
//! nothing — and a role whose trust policy does not admit this service carries nothing,
//! whatever it is allowed to do.
//!
//! What this suite adds to `tests/all/target_role_assumption.rs` and to the router's own
//! `events_target_role_grants_the_delivery`, which pin the DECISION, is the invocation's own
//! effect: the deployment package runs and the message its handler sends arrives, so a
//! handover recorded but never run fails here.
//!
//! The rows differ along what the rule generalises over: what the target's role TRUSTS (this
//! service, or another service, so it cannot be assumed) and what it is ALLOWED (the
//! invocation, or another service's call). The function's own policy is empty in every row.

use std::io::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoding, the reader of an answer's first element and the trust document a
// role is created with: the sibling harness owns all three, so this suite keeps no second copy.
use crate::target_dispatch::{form_encode, trust_policy, xml_text};

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The `source` and `detail-type` the rule's pattern names.
const SOURCE: &str = "com.demo.purge.requested";
const DETAIL_TYPE: &str = "Cache Purge Requested";

/// The service principal a target's role has to trust for EventBridge to act as it, and a
/// principal that is not this service — written as the literals AWS publishes.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";
const OTHER_PRINCIPAL: &str = "lambda.amazonaws.com";

/// The action an invocation of a function performs, and one that is not it.
const INVOKE_ACTION: &str = "lambda:InvokeFunction";
const ANOTHER_ACTION: &str = "s3:PutObject";

/// The module and attribute the deployment package carries its handler as.
const HANDLER: &str = "lambda_function.lambda_handler";

/// How long one invocation is given, as the function's own `Timeout`: well past what the
/// handler needs on any host.
const FUNCTION_TIMEOUT: u64 = 60;

/// How long each observation of work the clock owes waits before giving up, and how long
/// between two looks. Generous: what is under test is that the work happens at all, never how
/// fast this host gets to it.
const BUDGET: Duration = Duration::from_secs(60);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// How long a row that expects NO invocation keeps looking before concluding the delivery was
/// withheld. The delivering row proves the same wiring runs the handler in a fraction of it.
const WITHHELD_WINDOW: Duration = Duration::from_secs(3);

/// The handler every row deploys: it echoes the `requestId` the event's detail carries onto
/// the queue whose url its environment names, which is where a client observes that the
/// delivery reached running code.
const ECHOING_SOURCE: &str = r#"
import json
import os

import boto3


def lambda_handler(event, context):
    boto3.client("sqs").send_message(
        QueueUrl=os.environ["QUEUE_URL"],
        MessageBody=json.dumps({"requestId": event["detail"]["requestId"]}),
    )
    return {"echoed": event["detail"]["requestId"]}
"#;

/// EventBridge, Lambda, Amazon SQS, IAM and the token service over ONE shared engine, as
/// `core/router/src/lib.rs` mounts them: the handler's own SDK call is dispatched back into
/// this same router, so the queue it writes is the queue this suite reads.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The suite signs nothing: the `Authorization` header carries only the credential scope.
    // The handler's OWN call is signed by the runtime with the credential it was given.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_eventbridge_awsevents::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature is never checked here.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One `X-Amz-Target` RPC call, as its status and parsed body.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    json_version: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(signing_name))
        .header(
            "content-type",
            format!("application/x-amz-json-{json_version}"),
        )
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One EventBridge call (`awsJson1_1`) that must answer 200.
async fn events(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(router, "events", "AWSEvents", "1.1", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from EventBridge {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// One Amazon SQS call (`awsJson1_0`) that must answer 200.
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(router, "sqs", "AmazonSQS", "1.0", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Amazon SQS {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// A Lambda `restJson1` call, as its status and parsed body.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &Value,
) -> (u16, Value) {
    let payload = if body.is_null() {
        bytes::Bytes::new()
    } else {
        bytes::Bytes::from(body.to_string())
    };
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(payload)
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One awsQuery call to IAM, as the account's own credential, which must be served.
async fn iam(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in params {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "iam.amazonaws.com")
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("iam"))
        .body(bytes::Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from IAM {action}, got {status} with {answered}"
    );
    answered
}

/// A .zip deployment package holding the handler above as `lambda_function.py`, written in
/// memory the way a caller uploads one and base64-encoded as `FunctionCode$ZipFile` carries
/// it.
fn package() -> String {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
        writer
            .start_file("lambda_function.py", options)
            .expect("the entry starts");
        writer
            .write_all(ECHOING_SOURCE.as_bytes())
            .expect("the entry's bytes");
        writer.finish().expect("a finished archive");
    }
    base64::engine::general_purpose::STANDARD.encode(buffer.into_inner())
}

/// An identity document allowing `action` on `resource`.
fn identity_document(action: &str, resource: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": action, "Resource": resource }],
    })
    .to_string()
}

/// A role called `name` trusting `principal` and allowed `action` on `resource`; answers the
/// ARN IAM persisted.
async fn given_role(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    principal: &str,
    action: &str,
    resource: &str,
) -> String {
    let trust = trust_policy(principal);
    let created = iam(
        router,
        case,
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", &trust)],
    )
    .await;
    let arn = xml_text(&created, "Arn")
        .unwrap_or_else(|| panic!("case {case:?}: expected an Arn in {created}"));
    let document = identity_document(action, resource);
    iam(
        router,
        case,
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", "delivery"),
            ("PolicyDocument", &document),
        ],
    )
    .await;
    arn
}

/// One row: what the target's role trusts, what it is allowed, and whether the handler runs.
struct Row {
    case: &'static str,
    trusts: &'static str,
    allows: &'static str,
    echoes: bool,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a role trusting this service and allowed the invocation runs the handler",
            trusts: EVENTS_PRINCIPAL,
            allows: INVOKE_ACTION,
            echoes: true,
        },
        Row {
            case: "a role that does not trust this service runs nothing",
            trusts: OTHER_PRINCIPAL,
            allows: INVOKE_ACTION,
            echoes: false,
        },
        Row {
            case: "an assumable role allowed another service's call runs nothing",
            trusts: EVENTS_PRINCIPAL,
            allows: ANOTHER_ACTION,
            echoes: false,
        },
    ]
}

/// Every row: the function's own policy grants this service nothing, so what the delivery
/// turns on is the role the target names — and what a delivery that happened leaves behind is
/// the handler's own message.
#[tokio::test]
async fn a_role_granted_function_target_runs_its_handler() {
    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let router = router();

        // The queue the handler echoes onto, which is also where this suite looks.
        let queue = sqs(
            &router,
            case,
            "CreateQueue",
            &json!({ "QueueName": format!("echoed-{index}") }),
        )
        .await;
        let queue_url = queue["QueueUrl"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a QueueUrl, got {queue}"))
            .to_string();

        // The function's EXECUTION role: trusted by Lambda and allowed the one call its
        // handler makes.
        let execution_role = given_role(
            &router,
            case,
            &format!("echoing-exec-{index}"),
            OTHER_PRINCIPAL,
            "sqs:SendMessage",
            "*",
        )
        .await;
        let function_name = format!("echoing-{index}");
        let (status, created) = lambda(
            &router,
            "POST",
            "/2015-03-31/functions",
            &json!({
                "FunctionName": &function_name,
                "Role": execution_role,
                "Runtime": "python3.12",
                "Handler": HANDLER,
                "Timeout": FUNCTION_TIMEOUT,
                "Environment": { "Variables": { "QUEUE_URL": &queue_url } },
                "Code": { "ZipFile": package() },
            }),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {case:?}: expected HTTP 201 from CreateFunction, got {status} with {created}"
        );
        let function_arn = created["FunctionArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a FunctionArn, got {created}"))
            .to_string();

        // The role the TARGET names, which is the identity the invocation is made as. The
        // function itself carries no resource-based policy at all: no `AddPermission` is made
        // in any row.
        let target_role = given_role(
            &router,
            case,
            &format!("echoing-target-{index}"),
            row.trusts,
            row.allows,
            &function_arn,
        )
        .await;

        let rule = format!("echoing-rule-{index}");
        events(
            &router,
            case,
            "PutRule",
            &json!({
                "Name": &rule,
                "EventPattern": json!({ "source": [SOURCE], "detail-type": [DETAIL_TYPE] })
                    .to_string(),
                "State": "ENABLED",
            }),
        )
        .await;
        let wired = events(
            &router,
            case,
            "PutTargets",
            &json!({
                "Rule": &rule,
                "Targets": [{
                    "Id": "echoing-target",
                    "Arn": &function_arn,
                    "RoleArn": &target_role,
                }],
            }),
        )
        .await;
        assert_eq!(
            wired["FailedEntryCount"],
            json!(0),
            "case {case:?}: expected the target to be accepted, got {wired}"
        );

        // A created function leaves `Pending` by the clock: the handover is made once Lambda
        // reports it `Active`, looked for in a loop rather than waited out.
        let deadline = Instant::now() + BUDGET;
        loop {
            let (status, configuration) = lambda(
                &router,
                "GET",
                &format!("/2015-03-31/functions/{function_name}"),
                &Value::Null,
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200 from GetFunction, got {status} with \
                 {configuration}"
            );
            if configuration["Configuration"]["State"].as_str() == Some("Active") {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the created function never became Active, last answer \
                 {configuration}"
            );
            tokio::time::sleep(LOOK_AGAIN).await;
        }

        let request_id = format!("req-{index}");
        let published = events(
            &router,
            case,
            "PutEvents",
            &json!({
                "Entries": [{
                    "Source": SOURCE,
                    "DetailType": DETAIL_TYPE,
                    "Detail": json!({ "requestId": &request_id, "paths": ["/*"] }).to_string(),
                }],
            }),
        )
        .await;
        assert_eq!(
            published["FailedEntryCount"],
            json!(0),
            "case {case:?}: expected the event to be accepted, got {published}"
        );

        // What the handler itself left behind, waited for on the condition rather than on the
        // clock.
        let deadline = Instant::now() + if row.echoes { BUDGET } else { WITHHELD_WINDOW };
        let mut echoed: Vec<String> = Vec::new();
        loop {
            let received = sqs(
                &router,
                case,
                "ReceiveMessage",
                &json!({ "QueueUrl": &queue_url, "MaxNumberOfMessages": 10 }),
            )
            .await;
            for message in received["Messages"].as_array().cloned().unwrap_or_default() {
                echoed.push(message["Body"].as_str().unwrap_or_default().to_string());
            }
            if (row.echoes && !echoed.is_empty()) || Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(LOOK_AGAIN).await;
        }

        if row.echoes {
            assert_eq!(
                echoed.len(),
                1,
                "case {case:?}: expected exactly the handler's own message, got {echoed:?}"
            );
            assert!(
                echoed[0].contains(&request_id),
                "case {case:?}: expected the handler to echo {request_id:?}, got {echoed:?}"
            );
        } else {
            assert!(
                echoed.is_empty(),
                "case {case:?}: expected the handler never to run, got {echoed:?}"
            );
        }
    }
}
