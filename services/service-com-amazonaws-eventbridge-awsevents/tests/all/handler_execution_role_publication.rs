// SPDX-License-Identifier: CC0-1.0

//! A publication a RUNNING Lambda handler makes, end to end over the wire: EventBridge,
//! Lambda, Amazon SQS, IAM and the token service on ONE engine, a `python3.x` function whose
//! code does `import boto3` and calls `PutEvents`, and the published event read back where a
//! client reads it — off the queue the matching rule targets.
//!
//! The RULE pinned here: a call a running handler makes with the AWS SDK its runtime supplies
//! is signed as a SESSION OF THE FUNCTION'S EXECUTION ROLE, and a session of a role is
//! resolvable to that role by every service that authorizes a request — so THIS service judges
//! the publication against the policies in force on that role: it serves one those policies
//! grant, putting the event on the bus the entry names, and refuses one they do not with
//! `AccessDeniedException` naming the session as the principal. The handler catches that
//! refusal itself, so the invocation is HTTP 200 with NO `X-Amz-Function-Error` header,
//! carrying whatever the handler returned.
//!
//! Only a suite spanning both services can pin it: Lambda runs the handler and mints the
//! credential its SDK call carries, the token service vends that session, IAM holds the role's
//! policies, and this service decides the publication. The suite lives in THIS crate because
//! the decision and its wording are this service's, and because the sibling crate's
//! `handler_eventbridge_publication.rs` drives the TABLE of policy shapes with the same
//! credential; what this one adds is the handler actually RUNNING, so the payload asserted here
//! is what the SDK gave the guest.
//!
//! The two invocations differ along the one thing the handler cannot change: what the role's
//! policies say when it calls — a grant of the action over every resource, and no grant at all
//! beyond the AWS-managed policy a Lambda execution role is created with. Each asserts both
//! halves: what the handler read back, and what the bus did with the event.
//!
//! This module also OWNS the harness every suite of this crate that runs a handler's own SDK
//! call shares — [`Emulator`], [`package`], [`form_encode`], the ARN helpers and the handler
//! source itself — the way `target_dispatch` owns the one its own siblings share, so a second
//! suite states only the inputs it varies. `handler_publication_bus_resource_scope` is that
//! second suite: it drives the same harness over the forms a grant's `Resource` element takes.

use std::io::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for.
pub(crate) const REGION: &str = "us-east-1";

/// Where `CreateFunction`, the configuration read and `Invoke` are bound.
pub(crate) const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The module and attribute a package built here carries its handler as.
pub(crate) const HANDLER: &str = "lambda_function.lambda_handler";

/// How long a state the CLOCK owes — a function reaching `Active` — is waited for before
/// giving up, and how long to wait between looks. The condition is POLLED until it holds, so
/// nothing sleeps for a fixed span and no outcome depends on how fast the host runs.
pub(crate) const BUDGET: Duration = Duration::from_secs(60);
pub(crate) const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The access-key id standing for the account's own published credential: neither the token
/// service nor IAM holds a record of it, so it resolves to the account's ambient identity,
/// which is what every request this suite makes on its own behalf presents.
pub(crate) const ACCOUNT_KEY: &str = "AKIDEMULATOR";

/// The execution role the function runs as, and the function's own name — the session a
/// handler's calls are made under is named for both.
const ROLE_NAME: &str = "lambda-event-publisher";
const FUNCTION_NAME: &str = "publishes-events";

/// The name of the inline identity policy this suite writes on the execution role.
const INLINE_POLICY: &str = "inline";

/// The AWS-managed policy a Lambda execution role is created with: it grants the log writes a
/// function makes and nothing else, so a role carrying only it has no grant on this service.
pub(crate) const BASIC_EXECUTION_POLICY: &str =
    "arn:aws:iam::aws:policy/service-role/AWSLambdaBasicExecutionRole";

/// The rule that carries this suite's control queue, its target id, and the queue's name.
const RULE: &str = "receives-handler-publications";
const TARGET_ID: &str = "t1";
const QUEUE: &str = "handler-publications";

/// The source and detail type the handler publishes under, which is what the rule matches on.
const SOURCE: &str = "com.demo.handler-publications";
const DETAIL_TYPE: &str = "Handler Published";

/// The action a publication onto a bus performs, as a statement names it and as a refusal
/// reports it.
pub(crate) const PUT_EVENTS_ACTION: &str = "events:PutEvents";

/// The code EventBridge refuses an unauthorized publication with.
pub(crate) const ACCESS_DENIED: &str = "AccessDeniedException";

/// The service principal EventBridge delivers to a queue as, which the queue's own policy has
/// to grant: "for Lambda functions, Amazon SNS topics and Amazon SQS queues, EventBridge
/// relies on resource-based policies" (`PutTargets`).
pub(crate) const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// EventBridge, Lambda, Amazon SQS, IAM and the token service over ONE shared engine, as
/// `core/router/src/lib.rs` mounts them: the handler's SDK call is dispatched back into this
/// same router, so the bus it publishes onto is the bus this suite put a rule on.
pub(crate) struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    pub(crate) fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // The suite signs nothing: the `Authorization` header carries only the credential
        // scope, which is where a service reads the Region and the caller's access key id.
        // The handler's OWN call is signed by the runtime, with the credential it was given.
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_eventbridge_awsevents::EmulatedService::new(Arc::clone(&engine)),
        );
        router.register_service(
            service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
            Arc::clone(&engine),
        ));
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
                engine,
            ),
        );
        Self { router }
    }

    /// A SigV4 `Authorization` whose credential scope names `service`, which is what routes
    /// the request. The signature is never checked here.
    fn authorization(service: &str) -> String {
        format!(
            "AWS4-HMAC-SHA256 Credential={ACCOUNT_KEY}/20260101/{REGION}/{service}/\
             aws4_request, SignedHeaders=host, Signature=not-verified"
        )
    }

    /// One `awsJson` RPC call against the service whose signing name is `signing_name` — the
    /// credential scope is what routes it — which must be served.
    async fn rpc(
        &self,
        case: &str,
        signing_name: &str,
        target_prefix: &str,
        json_version: &str,
        operation: &str,
        body: &Value,
    ) -> Value {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
            .header(
                "content-type",
                format!("application/x-amz-json-{json_version}"),
            )
            .header("x-amz-target", format!("{target_prefix}.{operation}"))
            .header("authorization", Self::authorization(signing_name))
            .body(bytes::Bytes::from(body.to_string()))
            .expect("the test request is well formed");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let payload = response.into_body();
        let parsed: Value = if payload.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&payload)
                .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&payload).to_string()))
        };
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from {operation}, got {status} with {parsed}"
        );
        parsed
    }

    /// One EventBridge call that must be served (`awsJson1_1`, `AWSEvents.<Operation>`).
    pub(crate) async fn events(&self, case: &str, operation: &str, body: &Value) -> Value {
        self.rpc(case, "events", "AWSEvents", "1.1", operation, body)
            .await
    }

    /// One Amazon SQS call that must be served (`awsJson1_0`, `AmazonSQS.<Operation>`).
    pub(crate) async fn sqs(&self, case: &str, operation: &str, body: &Value) -> Value {
        self.rpc(case, "sqs", "AmazonSQS", "1.0", operation, body)
            .await
    }

    /// One awsQuery call to IAM, as the account's own credential, which must be served.
    pub(crate) async fn iam(&self, case: &str, action: &str, params: &[(&str, String)]) {
        let mut body = format!("Action={action}&Version=2010-05-08");
        for (name, value) in params {
            body.push('&');
            body.push_str(name);
            body.push('=');
            body.push_str(&form_encode(value));
        }
        let request = http::Request::builder()
            .method("POST")
            .uri("https://iam.amazonaws.com/")
            .header("content-type", "application/x-www-form-urlencoded")
            .header("authorization", Self::authorization("iam"))
            .body(bytes::Bytes::from(body))
            .expect("the test request is well formed");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let answered = String::from_utf8_lossy(response.body()).to_string();
        assert_eq!(
            status, 200,
            "case {case:?}: IAM {action} must succeed, got {status} {answered}"
        );
    }

    /// One Lambda `restJson1` call, as its status, headers and parsed body.
    pub(crate) async fn lambda(
        &self,
        method: &str,
        uri: &str,
        body: &[u8],
    ) -> (u16, http::HeaderMap, Value) {
        let request = http::Request::builder()
            .method(method)
            .uri(uri)
            .header("host", format!("lambda.{REGION}.amazonaws.com"))
            .header("authorization", Self::authorization("lambda"))
            .header("content-type", "application/json")
            .body(bytes::Bytes::copy_from_slice(body))
            .expect("the test request is well formed");
        let response = self.router.dispatch(request).await;
        let (parts, body) = response.into_parts();
        let parsed: Value = if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&body)
                .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).to_string()))
        };
        (parts.status.as_u16(), parts.headers, parsed)
    }

    /// Create the execution role `role_name`, trusted by Lambda and carrying the AWS-managed
    /// policy a Lambda execution role is created with — written with IAM's own operations, so
    /// the role a session is minted of is the record `iam:CreateRole` wrote. Answers its ARN.
    pub(crate) async fn execution_role(&self, case: &str, role_name: &str) -> String {
        let trust = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Principal": {"Service": "lambda.amazonaws.com"},
                "Action": "sts:AssumeRole",
            }],
        })
        .to_string();
        self.iam(
            case,
            "CreateRole",
            &[
                ("RoleName", role_name.to_string()),
                ("AssumeRolePolicyDocument", trust),
            ],
        )
        .await;
        self.iam(
            case,
            "AttachRolePolicy",
            &[
                ("RoleName", role_name.to_string()),
                ("PolicyArn", BASIC_EXECUTION_POLICY.to_string()),
            ],
        )
        .await;
        role_arn(role_name)
    }

    /// The inline identity policy `policy_name` in force on the role `role_name` from now on:
    /// `document`, or none at all — which is what makes a later invocation about the grant in
    /// force when the handler CALLS, rather than the one in force when its credential was
    /// minted.
    pub(crate) async fn role_policy(
        &self,
        case: &str,
        role_name: &str,
        policy_name: &str,
        document: Option<&str>,
    ) {
        match document {
            Some(document) => {
                self.iam(
                    case,
                    "PutRolePolicy",
                    &[
                        ("RoleName", role_name.to_string()),
                        ("PolicyName", policy_name.to_string()),
                        ("PolicyDocument", document.to_string()),
                    ],
                )
                .await;
            }
            None => {
                self.iam(
                    case,
                    "DeleteRolePolicy",
                    &[
                        ("RoleName", role_name.to_string()),
                        ("PolicyName", policy_name.to_string()),
                    ],
                )
                .await;
            }
        }
    }

    /// Create the function `name`, running as `role_arn` with `source` as its handler, and look
    /// again until Lambda reports it `Active` — the provisioning window is owed by the CLOCK, so
    /// nothing here sleeps on the emulator's behalf.
    pub(crate) async fn function(&self, case: &str, name: &str, role_arn: &str, source: &str) {
        let (status, _headers, created) = self
            .lambda(
                "POST",
                FUNCTIONS_URI,
                &serde_json::to_vec(&json!({
                    "FunctionName": name,
                    "Role": role_arn,
                    "Runtime": "python3.12",
                    "Handler": HANDLER,
                    "Timeout": 30,
                    "Code": {
                        "ZipFile": base64::engine::general_purpose::STANDARD
                            .encode(package(source)),
                    },
                }))
                .expect("the create body serializes"),
            )
            .await;
        assert_eq!(
            status, 201,
            "case {case:?}: expected HTTP 201 from CreateFunction, got {status} ({created})"
        );
        let deadline = Instant::now() + BUDGET;
        loop {
            let (_, _, configuration) = self
                .lambda("GET", &format!("{FUNCTIONS_URI}/{name}/configuration"), &[])
                .await;
            if configuration["State"].as_str() == Some("Active") {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: Lambda never reported {name:?} Active, last read {configuration}"
            );
            tokio::time::sleep(LOOK_AGAIN).await;
        }
    }

    /// `Invoke` the function `name` synchronously with `event` as its payload; answers the
    /// `FunctionError` signal the invocation carried, if any, and what the handler returned.
    pub(crate) async fn invoke(
        &self,
        case: &str,
        name: &str,
        event: &Value,
    ) -> (Option<String>, Value) {
        let (status, headers, payload) = self
            .lambda(
                "POST",
                &format!("{FUNCTIONS_URI}/{name}/invocations"),
                event.to_string().as_bytes(),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from Invoke, got {status} ({payload})"
        );
        let signal = headers
            .get("x-amz-function-error")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        (signal, payload)
    }

    /// A queue named `queue_name` carrying the resource policy an EventBridge delivery to it
    /// needs; answers its url and its ARN.
    pub(crate) async fn events_queue(&self, case: &str, queue_name: &str) -> (String, String) {
        let created = self
            .sqs(case, "CreateQueue", &json!({ "QueueName": queue_name }))
            .await;
        let url = created["QueueUrl"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no QueueUrl in {created}"))
            .to_string();
        let attributes = self
            .sqs(
                case,
                "GetQueueAttributes",
                &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
            )
            .await;
        let arn = attributes["Attributes"]["QueueArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no QueueArn in {attributes}"))
            .to_string();
        let policy = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Sid": "AllowEventBridge",
                "Effect": "Allow",
                "Principal": {"Service": EVENTS_PRINCIPAL},
                "Action": "sqs:SendMessage",
                "Resource": arn,
            }],
        })
        .to_string();
        self.sqs(
            case,
            "SetQueueAttributes",
            &json!({ "QueueUrl": url, "Attributes": { "Policy": policy } }),
        )
        .await;
        (url, arn)
    }

    /// The queue the rule targets, carrying the policy EventBridge's delivery needs, and the
    /// rule that watches the default bus for the handler's own event; answers the queue's url.
    async fn control_queue(&self, case: &str) -> String {
        let (url, arn) = self.events_queue(case, QUEUE).await;
        // The rule that carries the queue: it matches exactly the event the handler
        // publishes, so a message on the queue is that event and nothing else.
        let pattern = json!({ "source": [SOURCE], "detail-type": [DETAIL_TYPE] }).to_string();
        self.events(
            case,
            "PutRule",
            &json!({ "Name": RULE, "EventPattern": pattern, "State": "ENABLED" }),
        )
        .await;
        let targets = self
            .events(
                case,
                "PutTargets",
                &json!({
                    "Rule": RULE,
                    "Targets": [{ "Id": TARGET_ID, "Arn": arn }],
                }),
            )
            .await;
        assert_eq!(
            targets["FailedEntryCount"],
            json!(0),
            "case {case:?}: PutTargets reported a failed entry: {targets}"
        );
        url
    }

    /// The `detail.marker` of every message waiting on the queue at `url`, taken off it and
    /// sorted; every message is asserted to carry `source`, so nothing else is counted.
    pub(crate) async fn drained_markers(
        &self,
        case: &str,
        url: &str,
        source: &str,
    ) -> Vec<String> {
        let received = self
            .sqs(
                case,
                "ReceiveMessage",
                &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
            )
            .await;
        let messages = received["Messages"].as_array().cloned().unwrap_or_default();
        let mut markers: Vec<String> = messages
            .iter()
            .map(|message| {
                let text = message["Body"].as_str().unwrap_or_default();
                let body: Value = serde_json::from_str(text).unwrap_or(Value::Null);
                assert_eq!(
                    body["source"].as_str().unwrap_or_default(),
                    source,
                    "case {case:?}: expected the handler's own source on the queue, got {body}"
                );
                body["detail"]["marker"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        markers.sort();
        markers
    }
}

/// The ARN of the execution role `role_name`, as IAM holds it.
pub(crate) fn role_arn(role_name: &str) -> String {
    format!(
        "arn:aws:iam::{}:role/{role_name}",
        engine::emulator_account_id()
    )
}

/// The assumed-role session ARN a call made by an invocation of `function_name` running as
/// `role_name` speaks for: the session a function's calls are made under is named for the
/// FUNCTION.
pub(crate) fn session_arn(role_name: &str, function_name: &str) -> String {
    format!(
        "arn:aws:sts::{}:assumed-role/{role_name}/{function_name}",
        engine::emulator_account_id()
    )
}

/// The ARN of the event bus `name` in the Region every call here is signed for — the resource
/// a publication onto it is authorized on.
pub(crate) fn bus_arn(name: &str) -> String {
    format!(
        "arn:aws:events:{REGION}:{}:event-bus/{name}",
        engine::emulator_account_id()
    )
}

/// The sentence AWS refuses a call with when no identity-based policy in force allows it.
pub(crate) fn implicit_deny_message(principal: &str, action: &str, resource: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} \
         because no identity-based policy allows the {action} action"
    )
}

/// `application/x-www-form-urlencoded` for one awsQuery parameter value.
pub(crate) fn form_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(char::from(byte));
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

/// A .zip deployment package holding `source` as `lambda_function.py`, written in memory the
/// way a caller uploads one.
pub(crate) fn package(source: &str) -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
        writer
            .start_file("lambda_function.py", options)
            .expect("the entry starts");
        writer
            .write_all(source.as_bytes())
            .expect("the entry's bytes");
        writer.finish().expect("a finished archive");
    }
    buffer.into_inner()
}

/// The handler these suites run: it publishes the event it was given — onto the bus the event
/// names, or onto the default bus when it names none, which is what an entry carrying no
/// `EventBusName` is published onto — and reports what the call answered, catching the
/// service's refusal itself. So the invocation is a 200 with no `FunctionError` whichever way
/// the call went, and its payload IS what the SDK gave the guest.
pub(crate) const PUBLISHER_SOURCE: &str = r#"
import json

import boto3
import botocore.exceptions


def lambda_handler(event, context):
    client = boto3.client("events")
    entry = {
        "Source": event["source"],
        "DetailType": event["detail_type"],
        "Detail": json.dumps(event["detail"]),
    }
    if event.get("bus"):
        entry["EventBusName"] = event["bus"]
    try:
        answered = client.put_events(Entries=[entry])
    except botocore.exceptions.ClientError as error:
        return {
            "ok": False,
            "error_code": error.response["Error"]["Code"],
            "message": error.response["Error"]["Message"],
        }
    return {"ok": True, "failed_count": answered.get("FailedEntryCount", 0)}
"#;

/// The payload this suite invokes the handler with: one event carrying `marker`, published onto
/// the default bus because it names no bus.
fn publication_event(marker: &str) -> Value {
    json!({
        "source": SOURCE,
        "detail_type": DETAIL_TYPE,
        "detail": {"marker": marker},
    })
}

/// RULE: the call a RUNNING handler makes with the SDK its runtime supplies is signed as a
/// session of the function's execution role, so this service judges the publication against
/// the policies in force on that role — serving it where they grant the action and refusing it
/// with `AccessDeniedException` naming the session where they do not. The invocation is a 200
/// with no `FunctionError` either way, because the handler catches the refusal.
#[tokio::test]
async fn a_running_handlers_publication_is_judged_against_its_execution_roles_policies() {
    let emulator = Emulator::new();
    let role = emulator.execution_role("setup", ROLE_NAME).await;
    let queue = emulator.control_queue("setup").await;
    emulator
        .function("setup", FUNCTION_NAME, &role, PUBLISHER_SOURCE)
        .await;

    // The role's own policy grants the publication: the handler's call is served, and the
    // event reaches the rule's queue.
    let granted = "the role's own policy grants the action";
    emulator
        .role_policy(
            granted,
            ROLE_NAME,
            INLINE_POLICY,
            Some(
                &json!({
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Action": PUT_EVENTS_ACTION,
                        "Resource": "*",
                    }],
                })
                .to_string(),
            ),
        )
        .await;
    let (signal, payload) = emulator
        .invoke(granted, FUNCTION_NAME, &publication_event("marker-granted"))
        .await;
    assert_eq!(
        signal, None,
        "case {granted:?}: expected no FunctionError, got {signal:?} with {payload}"
    );
    assert_eq!(
        payload,
        json!({"ok": true, "failed_count": 0}),
        "case {granted:?}: expected the handler's own report of a served publication, got \
         {payload}"
    );
    let delivered = emulator.drained_markers(granted, &queue, SOURCE).await;
    assert_eq!(
        delivered,
        vec!["marker-granted".to_string()],
        "case {granted:?}: expected the handler's event on the rule's queue, got {delivered:?}"
    );

    // The same handler, with the role's own policy deleted — leaving the AWS-managed policy it
    // was created with, which grants no action of this service: the publication is refused, the
    // handler reports the refusal's own code, and nothing reaches the queue.
    let denied = "the role carries only the AWS-managed policy it was created with";
    emulator
        .role_policy(denied, ROLE_NAME, INLINE_POLICY, None)
        .await;
    let (signal, payload) = emulator
        .invoke(denied, FUNCTION_NAME, &publication_event("marker-denied"))
        .await;
    assert_eq!(
        signal, None,
        "case {denied:?}: expected no FunctionError, got {signal:?} with {payload}"
    );
    assert_eq!(
        payload["ok"],
        json!(false),
        "case {denied:?}: expected the handler to report a refused publication, got {payload}"
    );
    assert_eq!(
        payload["error_code"],
        json!(ACCESS_DENIED),
        "case {denied:?}: expected the code {ACCESS_DENIED}, got {payload}"
    );
    // The refusal names the principal the call really spoke for: the session of the function's
    // execution role, named for the function.
    let expected = implicit_deny_message(
        &session_arn(ROLE_NAME, FUNCTION_NAME),
        PUT_EVENTS_ACTION,
        &bus_arn("default"),
    );
    assert_eq!(
        payload["message"].as_str().unwrap_or_default(),
        expected,
        "case {denied:?}: expected the refusal {expected:?}, got {payload}"
    );
    let delivered = emulator.drained_markers(denied, &queue, SOURCE).await;
    assert!(
        delivered.is_empty(),
        "case {denied:?}: expected nothing on the rule's queue, got {delivered:?}"
    );
}
