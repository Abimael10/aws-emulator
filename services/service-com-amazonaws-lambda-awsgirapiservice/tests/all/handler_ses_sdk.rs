// SPDX-License-Identifier: CC0-1.0

//! The AWS SDK a RUNNING handler reaches Amazon SES (the 2010-12-01 API) through, end to end
//! over the wire: Lambda and that service on ONE engine, a `python3.x` function whose code
//! does `import boto3` and builds a client for `ses`, and what each call did read back where a
//! client reads it.
//!
//! RULE: a managed runtime's bundled SDK can build a client for, and call, any service this
//! emulator serves, over that service's own protocol — so what a handler reads back is that
//! service's own answer (the operation's output members on a success, its own error code and
//! message raised as `botocore.exceptions.ClientError` on a refusal) and never a client-side
//! report that the service is unknown. This suite is that rule for the service whose model
//! declares `awsQuery` and whose SDK token is `ses`: the surface almost every existing
//! mail-sending handler is written against, and a sibling of the `sesv2` entry
//! [`crate::handler_email_sdk`] covers.
//!
//! The rows differ along what the rule generalises over:
//!
//! * the BINDING CLASS of the call — one taking no input at all, one carrying a single string
//!   whose output carries no member, a list with an enumerated filter, a read whose output is
//!   a MAP, a read whose output members are fractional numbers, a send whose input nests
//!   several structures, a send carrying the message's own BYTES, and a create whose input IS
//!   a nested structure read back by a reader of its own;
//! * whether the call is SERVED or REFUSED, and for a refusal which of the service's own
//!   error codes comes back;
//! * an operation the seam does NOT bind, which must be reported as a call that was not made,
//!   naming it — never guessed at and never answered with a made-up success.
//!
//! Each served row is asserted on BOTH sides: what the guest read back, and what Amazon SES
//! itself holds, read through its own awsQuery API.
//!
//! The suite RUNS handlers, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]): this emulator serves at
//! most `MAX_CONCURRENT_RUNS` invocations at once for the whole process.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE awsQuery parameter encoder this target keeps, the ONE reader of an element out of
// an XML answer, and the ONE writer of a deployment archive whose handler RUNS — each owned by
// the suite that first needed it and reached here rather than copied.
use crate::event_source_mapping_role_access::element;
use crate::handler_s3_object_access::form_encode;
use crate::synchronous_invoke_run::package;

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The API version every awsQuery request of this service carries.
const VERSION: &str = "2010-12-01";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The module and attribute the package below carries its handler as.
const HANDLER: &str = "lambda_function.lambda_handler";

/// How long a wait on a transition the CLOCK owes may take before the case gives up.
/// Generous: the condition is what is waited on, not this bound, and a loaded host may
/// take far longer than the span itself to get round to the work.
const SETTLE_DEADLINE: Duration = Duration::from_secs(180);

/// How often such a wait looks again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// `State`'s value for a function an invocation may reach.
const ACTIVE: &str = "Active";

/// The identity the served sends are made from: verified by one row, which the clock then
/// carries through to `Success`.
const SENDER: &str = "reports@example.com";

/// An identity no row ever verifies, which is what the refused send is made from.
const STRANGER: &str = "stranger@example.net";

/// The one recipient every send below addresses.
const RECIPIENT: &str = "owner@example.org";

/// The template the rows create, read back and render a send from.
const TEMPLATE: &str = "scheduled-report";

/// The verification status an identity reaches once the move the clock owes it falls due
/// (`VerificationStatus`, whose values are `Pending`, `Success`, `Failed`,
/// `TemporaryFailure` and `NotStarted`).
const VERIFIED: &str = "Success";

/// Lambda and Amazon SES over ONE shared engine, as `core/router/src/lib.rs` mounts them: the
/// handler's SDK call is dispatched back into this same router, so the identity it verifies
/// and the messages it sends are the ones this suite reads.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_ses_simpleemailservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature is never checked here.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One Lambda `restJson1` call, as its status, headers and parsed body.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &[u8],
) -> (u16, http::HeaderMap, Value) {
    let host = format!("lambda.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{uri}"))
        .header("host", host)
        .header("authorization", authorization("lambda"))
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(body.to_vec()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    let parsed: Value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).to_string()))
    };
    (parts.status.as_u16(), parts.headers, parsed)
}

/// One Amazon SES call made the way a CLIENT makes one — a form-encoded `Action` posted to the
/// root path — to read back what the handler's calls did.
async fn ses(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let mut body = format!("Action={action}&Version={VERSION}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let host = format!("email.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("authorization", authorization("ses"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(bytes::Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered = String::from_utf8_lossy(response.body()).into_owned();
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} ({answered})"
    );
    answered
}

/// The handler every row runs: it builds a client for the service the event names, calls the
/// method the event names with the parameters the event carries, and reports what came back —
/// so the invocation's payload IS what the SDK gave the guest.
///
/// A member the event carries as `{"__bytes__": "<text>"}` is handed over as the BYTES of that
/// text, which is how a handler passes a raw MIME message to this service.
const HANDLER_SOURCE: &str = r#"
import json

import boto3
import botocore.exceptions


def as_bytes(value):
    if isinstance(value, dict) and list(value) == ["__bytes__"]:
        return value["__bytes__"].encode("utf-8")
    if isinstance(value, dict):
        return {name: as_bytes(held) for name, held in value.items()}
    if isinstance(value, list):
        return [as_bytes(held) for held in value]
    return value


def lambda_handler(event, context):
    try:
        client = boto3.client(event["service"])
    except botocore.exceptions.UnknownServiceError as error:
        return {"ok": False, "unknown_service": str(error)}
    parameters = as_bytes(event["parameters"])
    try:
        answered = getattr(client, event["method"])(**parameters)
    except botocore.exceptions.ClientError as error:
        return {
            "ok": False,
            "code": error.response["Error"]["Code"],
            "message": error.response["Error"]["Message"],
            "sentence": str(error),
        }
    except botocore.exceptions.BotoCoreError as error:
        return {"ok": False, "raised": type(error).__name__, "sentence": str(error)}
    held = {key: value for key, value in answered.items() if key != "ResponseMetadata"}
    return {
        "ok": True,
        "status": answered["ResponseMetadata"]["HTTPStatusCode"],
        "members": sorted(held),
        "answered": json.loads(json.dumps(held, default=str)),
    }
"#;

/// What must come of one row's call.
enum Outcome {
    /// The service served it, and the answer must carry these output members.
    Served(&'static [&'static str]),
    /// The service refused it, with this error code of its own.
    Refused(&'static str),
    /// The seam binds no such call, so it was NOT MADE and the sentence names the operation.
    NotMade(&'static str),
}

/// One SDK call the handler makes, and what must come of it.
struct Row {
    case: &'static str,
    /// The client method the handler calls, which names the operation the call addresses.
    method: &'static str,
    /// The keyword arguments it calls that method with: the operation's input document.
    parameters: fn() -> Value,
    outcome: Outcome,
}

/// One message, as `SendEmail` takes one: a destination and a message, each its own nested
/// structure, with the subject and the body nesting a content part of their own.
fn message_from(sender: &str) -> Value {
    json!({
        "Source": sender,
        "Destination": { "ToAddresses": [RECIPIENT] },
        "Message": {
            "Subject": { "Data": "the scheduled report" },
            "Body": { "Text": { "Data": "one report, sent by a handler" } },
        },
    })
}

/// The rows, in the order they run: each one's state is the state the next reads, so the chain
/// is what the service answered rather than what this suite assumed.
///
/// Every operation named here is one Amazon SES's own model binds
/// (`smithy_query(query="operation",operation="GetAccountSendingEnabled,VerifyEmailIdentity,\
/// ListIdentities,GetIdentityVerificationAttributes,GetSendQuota,SendEmail,SendRawEmail,\
/// CreateTemplate,GetTemplate,SendTemplatedEmail,ListTemplates,SendBounce")`), and the member
/// list each served row expects is that operation's own output shape's.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a call taking no input at all: GetAccountSendingEnabled",
            method: "get_account_sending_enabled",
            parameters: || json!({}),
            outcome: Outcome::Served(&["Enabled"]),
        },
        Row {
            case: "one string in, no output member: VerifyEmailIdentity",
            method: "verify_email_identity",
            parameters: || json!({ "EmailAddress": SENDER }),
            outcome: Outcome::Served(&[]),
        },
        Row {
            case: "a list with an enumerated filter: ListIdentities",
            method: "list_identities",
            parameters: || json!({ "IdentityType": "EmailAddress" }),
            outcome: Outcome::Served(&["Identities"]),
        },
        Row {
            case: "a read whose output is a map: GetIdentityVerificationAttributes",
            method: "get_identity_verification_attributes",
            parameters: || json!({ "Identities": [SENDER] }),
            outcome: Outcome::Served(&["VerificationAttributes"]),
        },
        Row {
            case: "a send whose input nests several structures: SendEmail",
            method: "send_email",
            parameters: || message_from(SENDER),
            outcome: Outcome::Served(&["MessageId"]),
        },
        Row {
            case: "a send carrying the message's own bytes: SendRawEmail",
            method: "send_raw_email",
            parameters: || {
                json!({
                    "Source": SENDER,
                    "Destinations": [RECIPIENT],
                    "RawMessage": {
                        "Data": {
                            "__bytes__": format!(
                                "From: {SENDER}\r\nTo: {RECIPIENT}\r\n\
                                 Subject: the raw report\r\n\r\nthe raw body\r\n"
                            ),
                        },
                    },
                })
            },
            outcome: Outcome::Served(&["MessageId"]),
        },
        Row {
            case: "a create whose input is a nested structure: CreateTemplate",
            method: "create_template",
            parameters: || {
                json!({
                    "Template": {
                        "TemplateName": TEMPLATE,
                        "SubjectPart": "the {{period}} report",
                        "TextPart": "{{total}} rows",
                    },
                })
            },
            outcome: Outcome::Served(&[]),
        },
        Row {
            case: "that structure read back: GetTemplate",
            method: "get_template",
            parameters: || json!({ "TemplateName": TEMPLATE }),
            outcome: Outcome::Served(&["Template"]),
        },
        Row {
            case: "a send rendered from the stored template: SendTemplatedEmail",
            method: "send_templated_email",
            parameters: || {
                json!({
                    "Source": SENDER,
                    "Destination": { "ToAddresses": [RECIPIENT] },
                    "Template": TEMPLATE,
                    "TemplateData": r#"{"period":"daily","total":"12"}"#,
                })
            },
            outcome: Outcome::Served(&["MessageId"]),
        },
        Row {
            case: "a listing of the account's templates: ListTemplates",
            method: "list_templates",
            parameters: || json!({}),
            outcome: Outcome::Served(&["TemplatesMetadata"]),
        },
        Row {
            case: "a read whose output members are fractional numbers: GetSendQuota",
            method: "get_send_quota",
            parameters: || json!({}),
            outcome: Outcome::Served(&["Max24HourSend", "MaxSendRate", "SentLast24Hours"]),
        },
        Row {
            case: "a send from an identity that was never verified",
            method: "send_email",
            parameters: || message_from(STRANGER),
            outcome: Outcome::Refused("MessageRejected"),
        },
        Row {
            case: "an operation this runtime's SDK binds no call for",
            method: "send_bounce",
            parameters: || {
                json!({
                    "OriginalMessageId": "0000000000000000-00000000-0000-0000-0000-000000000000",
                    "BounceSender": SENDER,
                })
            },
            outcome: Outcome::NotMade("SendBounce"),
        },
    ]
}

/// Invoke `function` with `event` and answer the payload the run produced beside the
/// `FunctionError` signal it carried, if any.
async fn invoke(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    event: &Value,
) -> (Option<String>, Value) {
    let (status, headers, payload) = lambda(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{function}/invocations"),
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

/// Wait for the verification Amazon SES owes the sender to fall due, read through that
/// service's own API: the move is owed by the CLOCK, so this looks again until the service
/// reports it instead of sleeping for a fixed span.
async fn await_verified_sender(router: &MultiServiceRouter, case: &str) {
    let deadline = std::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let read = ses(
            router,
            case,
            "GetIdentityVerificationAttributes",
            &[("Identities.member.1", SENDER)],
        )
        .await;
        if element(&read, "VerificationStatus") == VERIFIED {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: expected the verification the clock owes to fall due, last read \
             {read}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Every binding class of the service's protocol, one invocation per row, each asserted on
/// what the guest read back — and then on what Amazon SES itself holds.
#[tokio::test]
async fn a_handler_calls_amazon_ses_through_the_sdk_the_runtime_supplies() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let router = emulator();
    let function = "ses-sdk-caller";
    let (status, _headers, created) = lambda(
        &router,
        "POST",
        FUNCTIONS_URI,
        &serde_json::to_vec(&json!({
            "FunctionName": function,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": HANDLER,
            "Timeout": 10,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD.encode(package(HANDLER_SOURCE)),
            },
        }))
        .expect("the create body serializes"),
    )
    .await;
    assert_eq!(
        status, 201,
        "expected HTTP 201 from CreateFunction, got {status} ({created})"
    );
    // The activation is owed by the CLOCK, so this looks again until the function reports the
    // state an invocation needs instead of sleeping for a fixed span.
    let deadline = std::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, _headers, configuration) = lambda(
            &router,
            "GET",
            &format!("{FUNCTIONS_URI}/{function}/configuration"),
            &[],
        )
        .await;
        assert_eq!(
            status, 200,
            "expected HTTP 200 from GetFunctionConfiguration, got {status} ({configuration})"
        );
        if configuration["State"] == json!(ACTIVE) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "expected the activation the clock owes to fall due, last read {configuration}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    let mut sent_message_ids: Vec<String> = Vec::new();
    for row in rows() {
        let case = row.case;
        let (signal, payload) = invoke(
            &router,
            case,
            function,
            &json!({
                "service": "ses",
                "method": row.method,
                "parameters": (row.parameters)(),
            }),
        )
        .await;
        // The call reaches the service, so nothing about it surfaces as the handler's own
        // unhandled exception.
        assert_eq!(
            signal, None,
            "case {case:?}: the handler must run to completion, got the function error \
             {signal:?} with {payload}"
        );
        assert_eq!(
            payload["unknown_service"],
            Value::Null,
            "case {case:?}: the runtime's SDK must reach the service, got {payload}"
        );

        match row.outcome {
            Outcome::Served(members) => {
                assert_eq!(
                    payload["ok"],
                    json!(true),
                    "case {case:?}: expected the call to be served, got {payload}"
                );
                assert_eq!(
                    payload["status"],
                    json!(200),
                    "case {case:?}: expected the SDK to report HTTP 200, got {payload}"
                );
                let answered = &payload["answered"];
                for member in members {
                    assert!(
                        answered.get(member).is_some(),
                        "case {case:?}: expected the output member {member:?}, got {payload}"
                    );
                }
                if members.is_empty() {
                    assert_eq!(
                        payload["members"],
                        json!([]),
                        "case {case:?}: expected an answer carrying no output member, got \
                         {payload}"
                    );
                }
            }
            Outcome::Refused(code) => {
                assert_eq!(
                    payload["ok"],
                    json!(false),
                    "case {case:?}: expected the service to refuse the call, got {payload}"
                );
                assert_eq!(
                    payload["code"],
                    json!(code),
                    "case {case:?}: expected the service's own error code, got {payload}"
                );
                let message = payload["message"].as_str().unwrap_or_default();
                assert!(
                    !message.is_empty(),
                    "case {case:?}: expected the service's own message, got {payload}"
                );
                // The sentence botocore renders names the operation the method was turned
                // into, so a handler that logs the exception logs what a real client logs.
                let sentence = payload["sentence"].as_str().unwrap_or_default();
                let opening = format!("An error occurred ({code}) when calling the ");
                assert!(
                    sentence.starts_with(&opening),
                    "case {case:?}: expected the sentence to open {opening:?}, got {sentence:?}"
                );
            }
            Outcome::NotMade(operation) => {
                assert_eq!(
                    payload["ok"],
                    json!(false),
                    "case {case:?}: expected the call not to be made, got {payload}"
                );
                let sentence = payload["sentence"].as_str().unwrap_or_default();
                assert!(
                    sentence.contains(operation),
                    "case {case:?}: expected the sentence to name {operation:?}, got {payload}"
                );
                assert_eq!(
                    payload["code"],
                    Value::Null,
                    "case {case:?}: a call that was not made carries no service error code, \
                     got {payload}"
                );
            }
        }

        // What each row's own answer says about what it addressed.
        match case {
            "a list with an enumerated filter: ListIdentities" => assert!(
                payload["answered"]["Identities"]
                    .as_array()
                    .is_some_and(|listed| listed.iter().any(|held| held == &json!(SENDER))),
                "case {case:?}: expected the verified identity among the listed ones, got \
                 {payload}"
            ),
            "a read whose output is a map: GetIdentityVerificationAttributes" => assert_eq!(
                payload["answered"]["VerificationAttributes"][SENDER]["VerificationStatus"],
                json!(VERIFIED),
                "case {case:?}: expected the status the service holds for the identity, got \
                 {payload}"
            ),
            "that structure read back: GetTemplate" => assert_eq!(
                payload["answered"]["Template"]["TemplateName"],
                json!(TEMPLATE),
                "case {case:?}: expected the template the name addressed, got {payload}"
            ),
            "a listing of the account's templates: ListTemplates" => assert!(
                payload["answered"]["TemplatesMetadata"]
                    .as_array()
                    .is_some_and(|listed| listed
                        .iter()
                        .any(|metadata| metadata["Name"] == json!(TEMPLATE))),
                "case {case:?}: expected the created template among the listed ones, got \
                 {payload}"
            ),
            "a read whose output members are fractional numbers: GetSendQuota" => assert!(
                payload["answered"]["SentLast24Hours"]
                    .as_f64()
                    .is_some_and(|sent| sent >= sent_message_ids.len() as f64),
                "case {case:?}: expected the account to have sent at least the {} messages \
                 the handler sent, got {payload}",
                sent_message_ids.len()
            ),
            _ => {}
        }
        if let Some(minted) = payload["answered"]["MessageId"].as_str() {
            assert!(
                !minted.is_empty(),
                "case {case:?}: expected the message id the service minted, got {payload}"
            );
            sent_message_ids.push(minted.to_string());
        }

        // The sender the second row asked to verify is only a SENDING identity once the move
        // the clock owes it falls due, which the sends below are made from.
        if case == "one string in, no output member: VerifyEmailIdentity" {
            await_verified_sender(&router, case).await;
        }
    }

    // What Amazon SES itself holds: the work the calls reported is the work that happened.
    let case = "the service's own view of what the handler's calls did";
    assert_eq!(
        sent_message_ids.len(),
        3,
        "case {case:?}: expected one message id per served send, got {sent_message_ids:?}"
    );
    let mut distinct = sent_message_ids.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        sent_message_ids.len(),
        "case {case:?}: every send must be minted its own message id, got {sent_message_ids:?}"
    );
    let attributes = ses(
        &router,
        case,
        "GetIdentityVerificationAttributes",
        &[("Identities.member.1", SENDER)],
    )
    .await;
    assert_eq!(
        element(&attributes, "VerificationStatus"),
        VERIFIED,
        "case {case:?}: expected the identity the handler verified, got {attributes}"
    );
    let template = ses(
        &router,
        case,
        "GetTemplate",
        &[("TemplateName", TEMPLATE)],
    )
    .await;
    assert_eq!(
        element(&template, "TemplateName"),
        TEMPLATE,
        "case {case:?}: expected the template the handler created, got {template}"
    );
    let quota = ses(&router, case, "GetSendQuota", &[]).await;
    let sent = element(&quota, "SentLast24Hours")
        .parse::<f64>()
        .unwrap_or_default();
    assert!(
        sent >= sent_message_ids.len() as f64,
        "case {case:?}: expected the account's own figures to count the {} messages the \
         handler sent, got {quota}",
        sent_message_ids.len()
    );
}
