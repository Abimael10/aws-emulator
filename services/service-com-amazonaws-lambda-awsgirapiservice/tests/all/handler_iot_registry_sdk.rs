// SPDX-License-Identifier: CC0-1.0

//! The AWS IoT calls a RUNNING handler makes with the AWS SDK its runtime supplies, end to end
//! over the wire: Lambda, AWS IoT, IAM and the token service on ONE engine, a `python3.x`
//! function whose code does `import boto3` and calls the device registry, and everything it
//! wrote read back where a client reads it — through that service's own `DescribeThing`.
//!
//! The rule pinned here: a managed runtime ships the AWS SDK for the handler's language on its
//! import path with the catalog of the services it can call, so a client built for a service
//! this emulator serves is CONSTRUCTED and its call is dispatched into that service — and the
//! call is made with temporary credentials for the FUNCTION'S EXECUTION ROLE, so the service
//! serving it judges it against the policies in force on that role exactly as it judges any
//! other signed request. What the call answers is therefore that service's own answer: a
//! success carrying the operation's output members and leaving the state that service then
//! reports, its modelled refusal, or IAM's `AccessDeniedException` when the role does not allow
//! the action — each raised inside the handler as `botocore.exceptions.ClientError`.
//!
//! The rows differ along what the rule generalises over: WHICH operation of the service is
//! called (a registry write whose name is in the request line, a read of one, an update of one,
//! a listing, and the endpoint reader that addresses no resource), what the execution role
//! allows when the handler calls (every action of the service, exactly the action called,
//! another action of it, nothing at all), and which answer the call earns (served, IAM's
//! refusal, the service's own modelled not-found). Each row asserts both halves — what the
//! handler read back, which is what it returns as the invocation's payload, and what AWS IoT
//! itself holds afterwards — so a call that answered without doing the work, or did the work
//! without answering, fails here.
//!
//! The suite RUNS a handler, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::io::Write as _;
use std::sync::Arc;

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mappings::await_function_active;
use crate::invoke_request_id::one_invocation_stream_at_a_time;

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The module and attribute the package below carries its handler as.
const HANDLER: &str = "lambda_function.lambda_handler";

/// The function this suite runs every row through.
const FUNCTION: &str = "iot-registry-caller";

/// The execution role the function runs as, and the name of the inline identity policy a row
/// puts in force on it.
const ROLE_NAME: &str = "iot-registry-caller-role";
const INLINE_POLICY: &str = "inline";

/// The access-key id standing for the account's own published credential: neither the token
/// service nor IAM holds a record of it, so it resolves to the account's ambient identity,
/// which is what every request this suite makes on its own behalf presents.
const ACCOUNT_KEY: &str = "AKIDEMULATOR";

/// The thing the handler registers and then updates, and one no registry entry answers to.
const THING: &str = "handler-registered-sensor";
const ABSENT_THING: &str = "handler-absent-sensor";

/// The error code IAM's authorization layer refuses a call with, and the one AWS IoT refuses a
/// read of a name it does not hold with (`smithy_query(query="operation",
/// operation="DescribeThing")`).
const ACCESS_DENIED: &str = "AccessDeniedException";
const NOT_FOUND: &str = "ResourceNotFoundException";

/// Lambda, AWS IoT, IAM and the token service over ONE shared engine, as
/// `core/router/src/lib.rs` mounts them: the handler's SDK call is dispatched back into this
/// same router, so the thing it registers is the thing this suite reads.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // The suite signs nothing: the `Authorization` header carries only the credential
        // scope, which is where a service reads the Region and the caller's access key id. The
        // handler's OWN call is signed by the runtime, with the credential it was given.
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(
            service_com_amazonaws_iot_awsiotservice::EmulatedService::new(Arc::clone(&engine)),
        );
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

    /// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
    /// request. The signature is never checked here.
    fn authorization(service: &str) -> String {
        format!(
            "AWS4-HMAC-SHA256 Credential={ACCOUNT_KEY}/20260101/{REGION}/{service}/\
             aws4_request, SignedHeaders=host, Signature=not-verified"
        )
    }

    /// One AWS IoT `restJson1` request, as the account's own credential — the way this suite
    /// reads back what a handler's call left behind.
    async fn iot(&self, method: &str, uri: &str, body: Option<&Value>) -> (u16, Value) {
        let payload = body.map_or_else(Vec::new, |value| {
            serde_json::to_vec(value).expect("the request body serializes")
        });
        let host = format!("iot.{REGION}.amazonaws.com");
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{uri}"))
            .header("host", host.clone())
            .header("authorization", Self::authorization("iot"))
            .header("content-type", "application/json")
            .body(bytes::Bytes::from(payload))
            .expect("the test request is well formed");
        let response = self.router.dispatch(request).await;
        let (parts, body) = response.into_parts();
        let parsed: Value = if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&body).unwrap_or(Value::Null)
        };
        (parts.status.as_u16(), parsed)
    }

    /// One awsQuery call to IAM, as the account's own credential, which must be served.
    async fn iam(&self, case: &str, action: &str, params: &[(&str, String)]) {
        let (status, answered) = self.iam_any(action, params).await;
        assert_eq!(
            status, 200,
            "case {case:?}: IAM {action} must succeed, got {status} with {answered}"
        );
    }

    /// One awsQuery call to IAM, whatever it answers.
    async fn iam_any(&self, action: &str, params: &[(&str, String)]) -> (u16, String) {
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
        (
            status,
            String::from_utf8_lossy(response.body()).to_string(),
        )
    }

    /// Create the execution role, trusted by Lambda, with no identity policy of its own yet —
    /// written with IAM's own operations, so the role a session is minted of is the record
    /// `iam:CreateRole` wrote.
    async fn given_execution_role(&self, case: &str) -> String {
        let trust = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Principal": { "Service": "lambda.amazonaws.com" },
                "Action": "sts:AssumeRole",
            }],
        })
        .to_string();
        self.iam(
            case,
            "CreateRole",
            &[
                ("RoleName", ROLE_NAME.to_string()),
                ("AssumeRolePolicyDocument", trust),
            ],
        )
        .await;
        format!(
            "arn:aws:iam::{}:role/{ROLE_NAME}",
            engine::emulator_account_id()
        )
    }

    /// The identity policy in force on the execution role from now on: `document`, or none at
    /// all — which is what makes a row about the grant that is in force when the handler calls,
    /// rather than about the one in force when its credentials were minted.
    async fn role_policy(&self, case: &str, document: Option<&Value>) {
        match document {
            Some(document) => {
                self.iam(
                    case,
                    "PutRolePolicy",
                    &[
                        ("RoleName", ROLE_NAME.to_string()),
                        ("PolicyName", INLINE_POLICY.to_string()),
                        ("PolicyDocument", document.to_string()),
                    ],
                )
                .await;
            }
            // A role carrying no such policy is already in the state the row asks for, and IAM
            // says so with its own `NoSuchEntity`.
            None => {
                self.iam_any(
                    "DeleteRolePolicy",
                    &[
                        ("RoleName", ROLE_NAME.to_string()),
                        ("PolicyName", INLINE_POLICY.to_string()),
                    ],
                )
                .await;
            }
        }
    }

    /// One Lambda `restJson1` call, as its status, headers and parsed body.
    async fn lambda(&self, method: &str, uri: &str, body: &[u8]) -> (u16, http::HeaderMap, Value) {
        let host = format!("lambda.{REGION}.amazonaws.com");
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{uri}"))
            .header("host", host)
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
}

/// One form value, percent-encoded the way an `application/x-www-form-urlencoded` body carries
/// it — written with the one encoder of such a body this crate's suites use.
fn form_encode(value: &str) -> String {
    form_urlencoded::Serializer::new(String::new())
        .append_pair("v", value)
        .finish()
        .trim_start_matches("v=")
        .to_string()
}

/// A .zip deployment package holding `source` as `lambda_function.py`, written in memory the
/// way a caller uploads one.
fn package(source: &str) -> Vec<u8> {
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

/// The handler every row runs: `import boto3` at MODULE scope — so a runtime shipping no SDK
/// fails the initialization phase rather than this call — then the ONE AWS IoT call the event
/// names, reporting what that call answered. The invocation's payload IS what the SDK gave the
/// guest.
const HANDLER_SOURCE: &str = r#"
import boto3
import botocore.exceptions

CLIENT = boto3.client("iot")


def lambda_handler(event, context):
    method = getattr(CLIENT, event["method"])
    try:
        answered = method(**event["parameters"])
    except botocore.exceptions.ClientError as error:
        return {
            "ok": False,
            "code": error.response["Error"]["Code"],
            "message": error.response["Error"].get("Message"),
        }
    return {
        "ok": True,
        "status": answered["ResponseMetadata"]["HTTPStatusCode"],
        "answered": {
            key: value for key, value in answered.items() if key != "ResponseMetadata"
        },
    }
"#;

/// A policy allowing `actions` on every resource, which is how an execution role is granted the
/// actions its function calls.
fn allow(actions: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": actions, "Resource": "*" }],
    })
}

/// One row: the client method the handler calls, the input it carries, what the execution role
/// allows while it calls, and what the call must answer.
struct Row {
    case: &'static str,
    method: &'static str,
    parameters: Value,
    /// The document in force on the execution role, or `None` for a role carrying none.
    policy: Option<Value>,
    /// The error code the call must be refused with, or `None` for a call that is served.
    refused_with: Option<&'static str>,
    /// What the handler must have read back, for a call that is served: the value at each named
    /// place of the output document it reports. The members the service fills from its own state
    /// (a thing's minted id, the endpoint address it publishes) are that service's to compute,
    /// so no row states them.
    answered: Vec<(&'static [&'static str], Value)>,
    /// A member of the answered document that must be present and non-empty, whatever its value
    /// — for an output whose value is the service's own to compute.
    answered_nonempty: Option<&'static str>,
}

/// The rows, in the order they run: each later row stands on what the earlier ones wrote, which
/// is what makes the listing and the update about stored state.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a write the role does not allow at all",
            method: "create_thing",
            parameters: json!({ "thingName": THING }),
            policy: None,
            refused_with: Some(ACCESS_DENIED),
            answered: Vec::new(),
            answered_nonempty: None,
        },
        Row {
            case: "a write the role allows another action of this service instead",
            method: "create_thing",
            parameters: json!({ "thingName": THING }),
            policy: Some(allow("iot:DescribeEndpoint")),
            refused_with: Some(ACCESS_DENIED),
            answered: Vec::new(),
            answered_nonempty: None,
        },
        Row {
            case: "a write the role allows exactly",
            method: "create_thing",
            parameters: json!({ "thingName": THING }),
            policy: Some(allow("iot:CreateThing")),
            refused_with: None,
            answered: vec![(&["thingName"], json!(THING))],
            answered_nonempty: Some("thingArn"),
        },
        Row {
            case: "an update the role allows through the whole namespace",
            method: "update_thing",
            parameters: json!({
                "thingName": THING,
                "attributePayload": { "attributes": { "site": "berlin" } },
            }),
            policy: Some(allow("iot:*")),
            refused_with: None,
            answered: Vec::new(),
            answered_nonempty: None,
        },
        Row {
            case: "a read of what the write registered",
            method: "describe_thing",
            parameters: json!({ "thingName": THING }),
            policy: Some(allow("iot:*")),
            refused_with: None,
            answered: vec![
                (&["thingName"], json!(THING)),
                (&["attributes", "site"], json!("berlin")),
            ],
            answered_nonempty: Some("thingId"),
        },
        Row {
            case: "a listing of the registry",
            method: "list_things",
            parameters: json!({}),
            policy: Some(allow("iot:*")),
            refused_with: None,
            answered: vec![(&["things", "0", "thingName"], json!(THING))],
            answered_nonempty: None,
        },
        Row {
            case: "the endpoint reader, which addresses no resource of its own",
            method: "describe_endpoint",
            parameters: json!({ "endpointType": "iot:Data-ATS" }),
            policy: Some(allow("iot:DescribeEndpoint")),
            refused_with: None,
            answered: Vec::new(),
            answered_nonempty: Some("endpointAddress"),
        },
        Row {
            case: "the endpoint reader the role does not allow",
            method: "describe_endpoint",
            parameters: json!({ "endpointType": "iot:Data-ATS" }),
            policy: Some(allow("iot:CreateThing")),
            refused_with: Some(ACCESS_DENIED),
            answered: Vec::new(),
            answered_nonempty: None,
        },
        Row {
            case: "a read the role allows of a name the registry does not hold",
            method: "describe_thing",
            parameters: json!({ "thingName": ABSENT_THING }),
            policy: Some(allow("iot:*")),
            refused_with: Some(NOT_FOUND),
            answered: Vec::new(),
            answered_nonempty: None,
        },
    ]
}

/// RULE: a handler reaches AWS IoT through the SDK its runtime supplies, so its call is
/// dispatched into that service as the function's execution role — and what the call answers is
/// that service's own answer: the output of a call the role's policies allow, IAM's
/// `AccessDeniedException` for one they do not, and the service's own modelled refusal for a
/// resource it does not hold.
#[tokio::test]
async fn a_running_handler_reaches_the_iot_registry_through_its_runtimes_sdk() {
    let _stream = one_invocation_stream_at_a_time().await;
    let emulator = Emulator::new();
    let role_arn = emulator.given_execution_role("setup").await;

    let (status, _headers, created) = emulator
        .lambda(
            "POST",
            FUNCTIONS_URI,
            &serde_json::to_vec(&json!({
                "FunctionName": FUNCTION,
                "Role": role_arn,
                "Runtime": "python3.12",
                "Handler": HANDLER,
                "Timeout": 20,
                "Code": {
                    "ZipFile": base64::engine::general_purpose::STANDARD
                        .encode(package(HANDLER_SOURCE)),
                },
            }))
            .expect("the create body serializes"),
        )
        .await;
    assert_eq!(
        status, 201,
        "setup: expected HTTP 201 from CreateFunction, got {status} ({created})"
    );
    // "During this time, you can't invoke or modify the function" (`CreateFunction`): the
    // created function leaves `Pending` by the clock, so the suite looks again until it has.
    await_function_active(&emulator.router, "setup", FUNCTION).await;

    for row in rows() {
        let case = row.case;
        emulator.role_policy(case, row.policy.as_ref()).await;
        let (status, headers, payload) = emulator
            .lambda(
                "POST",
                &format!("{FUNCTIONS_URI}/{FUNCTION}/invocations"),
                json!({ "method": row.method, "parameters": row.parameters })
                    .to_string()
                    .as_bytes(),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from Invoke, got {status} ({payload})"
        );
        let signal = headers
            .get("x-amz-function-error")
            .and_then(|value| value.to_str().ok());
        assert_eq!(
            signal, None,
            "case {case:?}: expected no FunctionError, got {signal:?} with {payload}"
        );

        match row.refused_with {
            None => {
                assert_eq!(
                    payload["ok"],
                    json!(true),
                    "case {case:?}: expected the handler's call to be served, got {payload}"
                );
                assert_eq!(
                    payload["status"],
                    json!(200),
                    "case {case:?}: expected the SDK to report HTTP 200, got {payload}"
                );
                for (place, expected) in &row.answered {
                    let mut read = &payload["answered"];
                    for member in *place {
                        read = match member.parse::<usize>() {
                            Ok(at) => &read[at],
                            Err(_) => &read[member],
                        };
                    }
                    assert_eq!(
                        read, expected,
                        "case {case:?}: expected the output member {place:?} to be {expected}, \
                         got {payload}"
                    );
                }
                if let Some(member) = row.answered_nonempty {
                    let read = payload["answered"][member].as_str().unwrap_or_default();
                    assert!(
                        !read.is_empty(),
                        "case {case:?}: expected the service to fill {member:?}, got {payload}"
                    );
                }
            }
            Some(code) => {
                assert_eq!(
                    payload["ok"],
                    json!(false),
                    "case {case:?}: expected the call to be refused, got {payload}"
                );
                assert_eq!(
                    payload["code"],
                    json!(code),
                    "case {case:?}: expected the error code {code:?}, got {payload}"
                );
            }
        }
    }

    // The other half: what AWS IoT itself holds, read where a client reads it — the thing the
    // handler registered and then updated, and nothing under the name every refused write
    // addressed before the grant was in force.
    let (status, reported) = emulator
        .iot("GET", &format!("/things/{THING}"), None)
        .await;
    assert_eq!(
        status, 200,
        "expected AWS IoT to report the thing the handler registered, got {status} ({reported})"
    );
    assert_eq!(
        reported["attributes"]["site"],
        json!("berlin"),
        "expected the attributes the handler's update wrote, got {reported}"
    );
    let (status, reported) = emulator
        .iot("GET", &format!("/things/{ABSENT_THING}"), None)
        .await;
    assert_eq!(
        status, 404,
        "expected the registry to hold nothing under the name no call registered, got {status} \
         ({reported})"
    );
}
