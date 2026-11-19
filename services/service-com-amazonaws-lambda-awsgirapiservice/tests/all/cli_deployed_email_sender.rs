// SPDX-License-Identifier: CC0-1.0

//! A function provisioned the way a COMMAND-LINE client provisions one — the execution role
//! written with IAM's own operations, the code uploaded as a .zip, the provisioning window
//! waited out by reading the function's own state, then invoked — and what its handler's own
//! Amazon SES API v2 send answers it: AWS Lambda, Amazon SES API v2, IAM and the token
//! service on ONE engine.
//!
//! RULE: a `python3.x` function created through this service's public API reaches `Active` by
//! the clock, and from then on every `Invoke` of it SETTLES — HTTP 200, no
//! `X-Amz-Function-Error` header, and the handler's own JSON document as the body — so an
//! in-handler SES v2 `SendEmail` the service serves gives the handler that operation's
//! modelled `MessageId` and one the service refuses gives it that refusal's own AWS error
//! code, status and message. Neither answer is ever a client-side report that the SDK could
//! not build a client for the service, and neither leaves the caller without a terminal
//! answer.
//!
//! The rows differ along what the rule generalises over: WHICH verified identity covers the
//! sender the handler sends from — the address identity itself, another address in the
//! verified sending domain, an address in a SUBDOMAIN of that domain — and the sender no
//! identity covers at all, which is the refused row. Every row asserts both sides: what the
//! handler read back, and, for a served send, what SES v2 itself holds about the message,
//! read through its own API.
//!
//! What the sibling suites pin, and this one does not repeat: `handler_email_sdk.rs` drives
//! the table of BINDING CLASSES a handler's SES v2 calls take (a body-only create, a read by
//! path label, a list, a read with no input), `handler_sdk_service_inventory.rs` pins which
//! services a handler's bundled SDK can build a client for at all, and
//! `cli_deployed_sender_outcome_classes.rs` drives the table of OUTCOME CLASSES one such
//! invocation settles on. What this suite adds is the CLI-shaped deploy path around one of
//! those calls: a role IAM holds, a packaged upload, the wait for `Active`, and an invocation
//! that answers terminally either way. The deploy path's steps are this module's own
//! ([`given_execution_role`], [`given_active_function`], [`given_verified_identity`],
//! [`invoke`]), so the sibling suite drives the same steps rather than a second copy of them.
//!
//! The suite RUNS handlers, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]): this emulator serves at
//! most `state::execution::max_concurrent_runs` invocations at once for the whole process.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// IAM speaks `awsQuery`, so its calls, the reader that takes a value back out of its XML
// answer and the statement builders are the ones the suite that first needed them owns,
// rather than a second copy here.
use crate::event_source_mapping_role_access::{allow, element, iam, logs_only};
use crate::invoke_request_id::one_invocation_stream_at_a_time;
// The one .zip writer this crate's suites package a handler with.
use crate::node_invoke_run::package;

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The Region every request below is signed for, and so the Region the handler's own client
/// calls in and the one a failed verification check is reported for.
const REGION: &str = "us-east-1";

/// The file the deployment package carries, and the module and attribute the function names
/// as its handler — the shape a command-line deploy of a single-file function takes.
const SOURCE_FILE: &str = "index.py";
const HANDLER: &str = "index.handler";

/// The managed runtime the function runs.
const RUNTIME: &str = "python3.12";

/// The execution role the function runs as, and the name of the one inline policy every role
/// this deploy path writes carries its statements in.
const ROLE_NAME: &str = "cli-deployed-email-sender";
const INLINE_POLICY: &str = "send-email";

/// The function this suite deploys and invokes.
const FUNCTION: &str = "cli-deployed-email-sender";

/// The `Timeout` the function carries, in seconds.
///
/// It bounds the WALL CLOCK one run may spend, and the handler here makes one SDK call, so
/// what it has to cover is not its work but the host's own scheduling: this target runs its
/// suites in parallel over a fixed number of run slots, so a run can sit mid-flight while
/// other suites' runs hold the CPU, and a value with no headroom would end such a run on its
/// deadline and answer a failure document instead of the handler's value.
const FUNCTION_TIMEOUT_SECONDS: i64 = 25;

/// The sending domain the account verifies, the address identity it verifies on its own, and
/// the three senders the rows send from.
const SENDING_DOMAIN: &str = "deploy.example.com";
const ADDRESS_IDENTITY: &str = "release-bot@mailer.example.net";
const DOMAIN_SENDER: &str = "reports@deploy.example.com";
const SUBDOMAIN_SENDER: &str = "notices@eu.deploy.example.com";

/// A sender in a domain no identity of this account covers, which is the refused row.
const STRANGER: &str = "nobody@unverified.example.org";

/// The recipient every row addresses: the mailbox of the SES simulator that accepts a message
/// and reports it delivered.
pub(crate) const RECIPIENT: &str = "success@simulator.amazonses.com";

/// The error code SES refuses a send from an identity it has not verified with, and the
/// status its model binds that error to
/// (`smithy_query(query="operation",operation="SendEmail")`: `MessageRejected`, 400).
const MESSAGE_REJECTED: &str = "MessageRejected";
const MESSAGE_REJECTED_STATUS: i64 = 400;

/// The status SES v2's model binds `SendEmail`'s success to.
pub(crate) const SEND_EMAIL_SUCCESS_STATUS: i64 = 200;

/// The message a send from an identity the account has not verified is refused with, as the
/// email service words it: its fixed text, the Region the check ran in upper-cased, and the
/// identity that failed the check — both taken from the request this suite made, so no value
/// of one run is written in. Spelled here rather than read from the service's own helper
/// (`service-com-amazonaws-sesv2-simpleemailservice-v2/src/state.rs`,
/// `unverified_identities_message`), so the wording is pinned independently of the code under
/// test and the two have to be corrected together; the sibling suite
/// `service-com-amazonaws-ses-simpleemailservice/tests/all/ses_source_identity_gate.rs` spells
/// the same sentence for the v1 API.
fn expected_refusal_message(identity: &str) -> String {
    format!(
        "Email address is not verified. The following identities failed the check in region \
         {}: {identity}",
        REGION.to_uppercase()
    )
}

/// The `errorType` an invocation reports when the HOST, not the handler, ended the run: the
/// runtime exiting instead of answering, which is what a run the host killed reports. It is no
/// answer about the rule under test — a run that never reached the SDK call says nothing about
/// what the email service answered — so the invocation below looks again when it sees it, the
/// way a client polling its own handler's flag does, rather than reading it as the service's
/// reply. A run that outlived the function's own `Timeout` is NOT in this set: that is an
/// answer about the call, and this suite fails on it. Source: the classes this crate's
/// runtime reports for a run whose runtime ended instead of answering and for one the memory
/// ceiling ended (`state::execution`).
const HOST_ENDED_THE_RUN: [&str; 2] = ["Runtime.ExitError", "Runtime.OutOfMemory"];

/// The state a function that can be invoked reports, and the one it is created in.
const ACTIVE: &str = "Active";

/// How long a clock-owed transition is given to fall due before this suite gives up.
///
/// Generous on purpose: the spans themselves are short, and this is the budget for the host
/// getting round to them, not a limit under test.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How long this suite waits between two reads of the same clock-owed transition.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Lambda, Amazon SES API v2, IAM and the token service over ONE shared engine, as
/// `core/router/src/lib.rs` mounts them: the handler's SDK call is dispatched back into this
/// same router as the session of its own execution role, and the identities this suite
/// verifies are the identities that session's send is checked against.
pub(crate) fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The suite signs nothing: the `Authorization` header carries only the credential scope,
    // which is where a service reads the Region. The handler's OWN call is signed by the
    // runtime, with the credential it was given.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_sesv2_simpleemailservice_v2::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
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
/// request and where the Region comes from. The signature is never checked here.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One request dispatched at `host`, as its status, headers and parsed body.
async fn dispatch(
    router: &MultiServiceRouter,
    service: &str,
    host_prefix: &str,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> (u16, http::HeaderMap, Value) {
    let host = format!("{host_prefix}.{REGION}.amazonaws.com");
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{uri}"))
        .header("host", host)
        .header("authorization", authorization(service))
        .header("content-type", "application/json");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder
        .body(bytes::Bytes::copy_from_slice(body))
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

/// One Lambda `restJson1` call.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> (u16, http::HeaderMap, Value) {
    dispatch(router, "lambda", "lambda", method, uri, headers, body).await
}

/// One SES v2 call made as a CLIENT makes it, which must be served.
pub(crate) async fn email(
    router: &MultiServiceRouter,
    what: &str,
    method: &str,
    uri: &str,
    body: &[u8],
    expected_status: u16,
) -> Value {
    let (status, _headers, parsed) =
        dispatch(router, "ses", "email", method, uri, &[], body).await;
    assert_eq!(
        status, expected_status,
        "{what}: expected HTTP {expected_status} from {method} {uri}, got {status} ({parsed})"
    );
    parsed
}

/// The handler the function runs: it builds a client for the email service, sends the message
/// the event describes, and reports what came back — so the invocation's payload IS what the
/// SDK gave the guest, whether the service served the send or refused it.
const HANDLER_SOURCE: &str = r#"
import boto3
import botocore.exceptions


def handler(event, context):
    try:
        client = boto3.client("sesv2")
    except botocore.exceptions.UnknownServiceError as error:
        return {"ok": False, "clientError": type(error).__name__, "detail": str(error)}
    try:
        answered = client.send_email(
            FromEmailAddress=event["from"],
            Destination={"ToAddresses": [event["to"]]},
            Content={
                "Simple": {
                    "Subject": {"Data": event["subject"]},
                    "Body": {"Text": {"Data": event["body"]}},
                },
            },
        )
    except botocore.exceptions.ClientError as error:
        return {
            "ok": False,
            "errorCode": error.response["Error"]["Code"],
            "message": error.response["Error"]["Message"],
            "status": error.response["ResponseMetadata"]["HTTPStatusCode"],
        }
    return {
        "ok": True,
        "messageId": answered["MessageId"],
        "status": answered["ResponseMetadata"]["HTTPStatusCode"],
    }
"#;

/// One row: the sender the handler sends from, and what must come of that send.
struct Row {
    case: &'static str,
    from: &'static str,
    /// The AWS error code SES must refuse the send with, or `None` when it must serve it.
    refusal: Option<&'static str>,
}

/// The rows, spanning which verified identity covers the sender — an address identity
/// authorises itself, a domain identity authorises every address in that domain and in any of
/// its subdomains, and an address under neither is authorised by nothing.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the sender is the verified address identity itself",
            from: ADDRESS_IDENTITY,
            refusal: None,
        },
        Row {
            case: "the sender is another address in the verified sending domain",
            from: DOMAIN_SENDER,
            refusal: None,
        },
        Row {
            case: "the sender is an address in a subdomain of the verified sending domain",
            from: SUBDOMAIN_SENDER,
            refusal: None,
        },
        Row {
            case: "the sender is an address no verified identity covers",
            from: STRANGER,
            refusal: Some(MESSAGE_REJECTED),
        },
    ]
}

/// Switch the account's Virtual Deliverability Manager configuration on, which is what SES
/// serves a message's insights report under: with it off the report does not exist for the
/// account and the read is refused.
pub(crate) async fn given_message_insights_enabled(router: &MultiServiceRouter) {
    email(
        router,
        "the account's VDM configuration",
        "PUT",
        "/v2/email/account/vdm",
        &serde_json::to_vec(&json!({ "VdmAttributes": { "VdmEnabled": "ENABLED" } }))
            .expect("the VDM body serializes"),
        200,
    )
    .await;
}

/// Create `identity` and read it back until the verification SES owes it is due.
///
/// The transition is owed by the CLOCK, so this looks again until the condition holds instead
/// of sleeping for a fixed span.
pub(crate) async fn given_verified_identity(router: &MultiServiceRouter, identity: &str) {
    let what = format!("the identity {identity}");
    email(
        router,
        &what,
        "POST",
        "/v2/email/identities",
        &serde_json::to_vec(&json!({ "EmailIdentity": identity }))
            .expect("the create body serializes"),
        200,
    )
    .await;
    let deadline = std::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let read = email(
            router,
            &what,
            "GET",
            &format!("/v2/email/identities/{identity}"),
            &[],
            200,
        )
        .await;
        if read["VerifiedForSendingStatus"] == json!(true) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "setup {what}: expected the verification the clock owes to fall due, last read \
             {read}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Create the execution role `role_name` with IAM's own operations, carrying `statements` as
/// its one inline document, and answer the ARN IAM persisted: a role Lambda may assume, which
/// may do what those statements allow and nothing else.
pub(crate) async fn given_execution_role(
    router: &MultiServiceRouter,
    role_name: &str,
    statements: Value,
) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "lambda.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let (status, created) = iam(
        router,
        "CreateRole",
        &[
            ("RoleName", role_name),
            ("AssumeRolePolicyDocument", &trust),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup the execution role: expected CreateRole to answer 200, got {status}: {created}"
    );
    let document = json!({
        "Version": "2012-10-17",
        "Statement": statements,
    })
    .to_string();
    let (status, written) = iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", role_name),
            ("PolicyName", INLINE_POLICY),
            ("PolicyDocument", &document),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup the execution role: expected PutRolePolicy to answer 200, got {status}: {written}"
    );
    element(&created, "Arn")
}

/// Create the function `function` from a packaged upload of `source` and read its own state
/// back until it reports `Active`, which is what a command-line client's function-active wait
/// does.
pub(crate) async fn given_active_function(
    router: &MultiServiceRouter,
    function: &str,
    source: &str,
    role_arn: &str,
) {
    let (status, _headers, created) = lambda(
        router,
        "POST",
        FUNCTIONS_URI,
        &[],
        &serde_json::to_vec(&json!({
            "FunctionName": function,
            "Role": role_arn,
            "Runtime": RUNTIME,
            "Handler": HANDLER,
            "Timeout": FUNCTION_TIMEOUT_SECONDS,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(&[(SOURCE_FILE, source)])),
            },
        }))
        .expect("the create body serializes"),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup the function: expected HTTP 201 from CreateFunction, got {status} ({created})"
    );

    // The activation is owed by the CLOCK, so the wait looks again until the function reports
    // the state an invocation needs instead of sleeping for a fixed span.
    let deadline = std::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, _headers, configuration) = lambda(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{function}/configuration"),
            &[],
            &[],
        )
        .await;
        assert_eq!(
            status, 200,
            "setup the function: expected HTTP 200 from GetFunctionConfiguration, got {status} \
             ({configuration})"
        );
        if configuration["State"] == json!(ACTIVE) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "setup the function: expected the activation the clock owes to fall due, last read \
             {configuration}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Invoke the function with `event`, the way a command-line client invokes one, and answer the
/// payload the run produced beside the `FunctionError` signal it carried, if any.
///
/// A run the HOST ended ([`HOST_ENDED_THE_RUN`]) is not an answer, so this looks again until
/// one that is arrives or a generous deadline passes — which is what the client this issue was
/// filed about does with its own invoke-until-answered loop. Every other answer, including a
/// handler's own unhandled exception and a run that outlived the function's `Timeout`, is
/// returned to the caller as it came.
pub(crate) async fn invoke(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    event: &Value,
) -> (Option<String>, Value) {
    let deadline = std::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, headers, payload) = lambda(
            router,
            "POST",
            &format!("{FUNCTIONS_URI}/{function}/invocations"),
            &[("x-amz-invocation-type", "RequestResponse")],
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
        let host_ended = HOST_ENDED_THE_RUN
            .iter()
            .any(|class| payload["errorType"] == json!(class));
        if !host_ended {
            return (signal, payload);
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: expected an invocation the service answered, got a run the host \
             ended every time: {payload}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The whole command-line deploy path, once, and then one invocation per row.
#[tokio::test]
async fn a_cli_deployed_function_sends_through_its_own_sdk_and_every_invocation_settles() {
    let _stream = one_invocation_stream_at_a_time().await;
    let router = emulator();

    let role_arn = given_execution_role(
        &router,
        ROLE_NAME,
        json!([logs_only(), allow("ses", &["SendEmail"], "*")]),
    )
    .await;
    assert_eq!(
        role_arn,
        format!(
            "arn:aws:iam::{}:role/{ROLE_NAME}",
            engine::emulator_account_id()
        ),
        "setup the execution role: expected the ARN IAM mints for a role of this account, got \
         {role_arn:?}"
    );
    given_verified_identity(&router, ADDRESS_IDENTITY).await;
    given_verified_identity(&router, SENDING_DOMAIN).await;
    given_message_insights_enabled(&router).await;
    given_active_function(&router, FUNCTION, HANDLER_SOURCE, &role_arn).await;

    let mut minted: Vec<String> = Vec::new();
    for row in rows() {
        let case = row.case;
        let subject = format!("deployment report: {case}");
        let body = format!("sent from {} by the deployed function", row.from);
        let (signal, payload) = invoke(
            &router,
            case,
            FUNCTION,
            &json!({
                "from": row.from,
                "to": RECIPIENT,
                "subject": subject,
                "body": body,
            }),
        )
        .await;

        // Whichever way the send went, the invocation is terminal: the handler ran to
        // completion, so nothing about the send surfaces as the function's own failure, and
        // the SDK reached the service rather than failing to build a client for it.
        assert_eq!(
            signal, None,
            "case {case:?}: the invocation must carry no function error, got {signal:?} with \
             {payload}"
        );
        assert_eq!(
            payload["clientError"],
            Value::Null,
            "case {case:?}: the runtime's SDK must reach the email service, got {payload}"
        );

        match row.refusal {
            None => {
                assert_eq!(
                    payload["ok"],
                    json!(true),
                    "case {case:?}: expected the send to be served, got {payload}"
                );
                assert_eq!(
                    payload["status"],
                    json!(SEND_EMAIL_SUCCESS_STATUS),
                    "case {case:?}: expected the modelled success status, got {payload}"
                );
                let message_id = payload["messageId"].as_str().unwrap_or_default().to_string();
                assert!(
                    !message_id.is_empty(),
                    "case {case:?}: expected the message id the service minted, got {payload}"
                );
                assert!(
                    !minted.contains(&message_id),
                    "case {case:?}: expected a message id of this send alone, got \
                     {message_id:?} among {minted:?}"
                );

                // What SES v2 itself holds about the message the handler sent: the send the
                // handler reported is the send that happened.
                let insights = email(
                    &router,
                    case,
                    "GET",
                    &format!("/v2/email/insights/{message_id}"),
                    &[],
                    200,
                )
                .await;
                assert_eq!(
                    insights["MessageId"],
                    json!(message_id.clone()),
                    "case {case:?}: expected the service to hold the message the handler sent, \
                     got {insights}"
                );
                assert_eq!(
                    insights["FromEmailAddress"],
                    json!(row.from),
                    "case {case:?}: expected the sender the handler sent from, got {insights}"
                );
                minted.push(message_id);
            }
            Some(code) => {
                assert_eq!(
                    payload["ok"],
                    json!(false),
                    "case {case:?}: expected the service to refuse the send, got {payload}"
                );
                assert_eq!(
                    payload["errorCode"],
                    json!(code),
                    "case {case:?}: expected the service's own error code, got {payload}"
                );
                assert_eq!(
                    payload["status"],
                    json!(MESSAGE_REJECTED_STATUS),
                    "case {case:?}: expected the status the model binds {code} to, got {payload}"
                );
                assert_eq!(
                    payload["message"],
                    json!(expected_refusal_message(row.from)),
                    "case {case:?}: expected the service's own refusal sentence, got {payload}"
                );
            }
        }
    }

    let served = rows().iter().filter(|row| row.refusal.is_none()).count();
    assert_eq!(
        minted.len(),
        served,
        "expected one message id per served send, got {minted:?}"
    );
}
