// SPDX-License-Identifier: CC0-1.0

//! MediaLive's channel-role check and the ATOMICITY of `CreateChannel`, across the two
//! services the check spans.
//!
//! RULE: a service that creates a resource which RUNS AS a caller-supplied IAM role
//! resolves that role against this account's IAM — and the role's trust policy — BEFORE it
//! evaluates any precondition on the resources the request references, and before it writes
//! anything; a role this account's IAM holds nothing under, or one whose trust policy does
//! not let the service's own principal assume it, is reported at the member the request
//! spelled it in. For `CreateChannel` (`smithy_query(query="operation",
//! operation="CreateChannel")`: POST /prod/channels, success code 201, modelled errors
//! include BadRequestException 400, ConflictException 409 and UnprocessableEntityException
//! 422) that report is the modelled 422, carrying exactly one `ValidationErrors` entry
//! (`UnprocessableEntityException$ValidationErrors` of `ValidationError{elementPath,
//! errorMessage}`) whose `elementPath` is the member's `jsonName` — and, because the whole
//! request is judged before the first write, a refused create leaves NO channel and takes
//! NO input attachment, so the input stays free and the corrected retry succeeds instead of
//! colliding with a channel that was never created.
//!
//! The rows differ along what the rule generalises over: whether IAM holds the role at all
//! (a name nothing was created under, a name that is a prefix of one that does exist, a
//! name under a path), the form of the trust policy the held role carries (which service
//! principal it names, whether it names several, whether the grant is an `Allow` or a
//! `Deny`, which action it names, whether it names an account principal rather than a
//! service), whether the value is the ARN of an IAM role at all, and whether the request
//! names a role in the first place.
//!
//! The role is created through the real `CreateRole` on the same engine that serves
//! MediaLive, so what the check reads is the trust document IAM persisted and not a copy;
//! that is why this suite lives beside the router rather than in the service crate, whose
//! own harness mounts MediaLive alone.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The percent-encoder that builds an awsQuery form body and the reader that pulls one
// element's text out of the XML answer are the test crate's own, written once in
// `all/query_wire.rs`.
use crate::query_wire::{element, form_encode};

/// The region every MediaLive request below is signed for.
const REGION: &str = "us-west-2";

/// The account every emulated resource belongs to.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The service principal a MediaLive channel's role has to let assume it, as the crate
/// states it.
const SERVICE_PRINCIPAL: &str =
    service_com_amazonaws_medialive_medialive::state::CHANNEL_SERVICE_PRINCIPAL;

/// How long a poll waits for a state the clock owes before giving up. Generous on purpose:
/// the span is the service's, and nothing here asserts on how fast the host runs.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits between two reads.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// One emulator carrying both services over ONE shared engine, exactly as the production
/// router builds them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // These suites exercise service behaviour, not request signing.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_medialive_medialive::EmulatedService::new(
        engine,
    ));
    router
}

/// One awsQuery call to IAM, answered with the HTTP status and the XML body.
async fn iam(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "iam.amazonaws.com")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = String::from_utf8_lossy(response.body()).to_string();
    (status, body)
}

/// One restJson1 call to MediaLive, answered with the HTTP status and the JSON body.
async fn medialive(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body.map_or_else(Bytes::new, |value| {
        Bytes::from(serde_json::to_vec(&value).expect("the test body serializes"))
    });
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", format!("medialive.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/medialive/\
                 aws4_request, SignedHeaders=host, Signature=abc"
            ),
        )
        .body(payload)
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The wire error code of a restJson1 failure body (`__type`, possibly qualified).
fn error_code(payload: &Value) -> String {
    payload
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit(['#', ':'])
        .next()
        .unwrap_or("<no __type>")
        .to_string()
}

/// The `message` of a restJson1 failure body.
fn error_message(payload: &Value) -> String {
    payload
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("<no message>")
        .to_string()
}

/// A trust policy naming `principals` in one `Allow` statement of `sts:AssumeRole` — the
/// form a role a service assumes carries.
fn trust_allowing(principals: &[&str]) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": principals },
            "Action": "sts:AssumeRole"
        }]
    })
    .to_string()
}

/// Create a role in IAM carrying `document` under `path` and hand back its ARN.
async fn role_with_trust(
    router: &MultiServiceRouter,
    name: &str,
    path: &str,
    document: &str,
) -> String {
    let (status, created) = iam(
        router,
        "CreateRole",
        &[
            ("RoleName", name),
            ("Path", path),
            ("AssumeRolePolicyDocument", document),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup CreateRole({name:?}) under {path:?}: expected the role to be persisted, got \
         {status}: {created}"
    );
    element(&created, "Arn")
}

/// The sentence AWS words a channel role it cannot assume with. Fixed text; the only
/// variable part is the role ARN the REQUEST named.
fn role_not_assumable_sentence(role_arn: &str) -> String {
    format!(
        "Make sure the role '{role_arn}' exists in the user account and \
         medialive.amazonaws.com is a trusted service"
    )
}

/// The 422's `message`: the element path the problem was found at, then the sentence — the
/// same two facts the one `ValidationErrors` entry beside it carries.
fn role_not_assumable_message(role_arn: &str) -> String {
    format!("roleArn {}", role_not_assumable_sentence(role_arn))
}

/// The `EncoderSettings` every channel configuration has to declare
/// (`smithy_query(query="shape", shape="EncoderSettings")`: AudioDescriptions,
/// OutputGroups, TimecodeConfig, VideoDescriptions).
fn encoder_settings() -> Value {
    json!({
        "audioDescriptions": [],
        "outputGroups": [],
        "timecodeConfig": { "source": "EMBEDDED" },
        "videoDescriptions": [],
    })
}

/// A token no other request of this suite uses, so two creates are never read as a retry of
/// one another (`RequestId` is `@idempotencyToken`, which an SDK fills for the caller).
fn fresh_token() -> String {
    static MINTED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let minted = MINTED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("medialive-role-suite-{minted}")
}

/// One input security group admitting any pushed stream, answering its minted id.
async fn input_security_group(router: &MultiServiceRouter, case: &str) -> String {
    let (status, created) = medialive(
        router,
        "POST",
        "/prod/inputSecurityGroups",
        Some(json!({ "whitelistRules": [{ "cidr": "0.0.0.0/0" }] })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateInputSecurityGroup: expected 201, got {status}: {created}"
    );
    created["securityGroup"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// One `RTMP_PUSH` input admitted through `group`, answering its minted id — the ingest
/// family a channel is pushed to, whose two ingest endpoints the service allocates.
async fn push_input(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    group: &str,
) -> String {
    let (status, created) = medialive(
        router,
        "POST",
        "/prod/inputs",
        Some(json!({
            "name": name,
            "type": "RTMP_PUSH",
            "requestId": fresh_token(),
            "inputSecurityGroups": [group],
            "destinations": [{ "streamName": "live/stream1" }, { "streamName": "live/stream2" }],
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateInput({name:?}): expected 201, got {status}: {created}"
    );
    created["input"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// A `CreateChannel` request for `name` attaching `input_id` and, where the row names one,
/// running as `role_arn`.
fn create_channel_body(name: &str, input_id: &str, role_arn: Option<&str>) -> Value {
    let mut body = json!({
        "name": name,
        "requestId": fresh_token(),
        "encoderSettings": encoder_settings(),
        "inputAttachments": [{ "inputAttachmentName": "feed", "inputId": input_id }],
    });
    if let Some(role_arn) = role_arn {
        body["roleArn"] = json!(role_arn);
    }
    body
}

/// `DescribeInput` on `id`, which must answer 200.
async fn describe_input(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
    let (status, described) = medialive(router, "GET", &format!("/prod/inputs/{id}"), None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeInput({id}): expected 200, got {status}: {described}"
    );
    described
}

/// Assert that `input_id` is held by no channel: the input a create that was REFUSED must
/// have left exactly as it found it.
async fn assert_input_is_free(router: &MultiServiceRouter, case: &str, input_id: &str) {
    let described = describe_input(router, case, input_id).await;
    assert_eq!(
        described["attachedChannels"],
        json!([]),
        "case {case:?}: a refused create must take no attachment, got {:?}",
        described["attachedChannels"]
    );
    assert_eq!(
        described["state"],
        json!("DETACHED"),
        "case {case:?}: expected the input to stay DETACHED, got {:?}",
        described["state"]
    );
}

/// Assert that this account holds no channel named `name`, which is what a refused create
/// leaves behind.
async fn assert_no_channel_named(router: &MultiServiceRouter, case: &str, name: &str) {
    let (status, listed) = medialive(router, "GET", "/prod/channels", None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: ListChannels: expected 200, got {status}: {listed}"
    );
    let named: Vec<&Value> = listed["channels"]
        .as_array()
        .map(|channels| {
            channels
                .iter()
                .filter(|channel| channel["name"] == json!(name))
                .collect()
        })
        .unwrap_or_default();
    assert!(
        named.is_empty(),
        "case {case:?}: a refused create must store no channel, got {named:?}"
    );
}

/// Wait until the channel `id` reports `expected`, for the same reason [`input_in_state`]
/// polls: the state a create or a delete starts is one the clock owes.
async fn channel_in_state(router: &MultiServiceRouter, case: &str, id: &str, expected: &str) {
    let started = Instant::now();
    let mut last = Value::Null;
    while started.elapsed() < POLL_DEADLINE {
        let (status, described) =
            medialive(router, "GET", &format!("/prod/channels/{id}"), None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeChannel({id}): expected 200, got {status}: {described}"
        );
        if described["state"] == json!(expected) {
            return;
        }
        last = described;
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("case {case:?}: channel {id} never reached {expected:?}, last read {last}");
}

/// The input `id`, read back once its `State` is `expected`.
///
/// A poll, not a sleep: the terminal state of a delete is one the clock owes, so the test
/// looks again until the condition holds or [`POLL_DEADLINE`] passes.
async fn input_in_state(router: &MultiServiceRouter, case: &str, id: &str, expected: &str) -> Value {
    let started = Instant::now();
    let mut last = Value::Null;
    while started.elapsed() < POLL_DEADLINE {
        last = describe_input(router, case, id).await;
        if last["state"] == json!(expected) {
            return last;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("case {case:?}: input {id} never reached {expected:?}, last read {last}");
}

/// What a row's `CreateChannel` must be answered with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// The role is one this service may run the channel under: the channel is created.
    Created,
    /// IAM answered that the role is not one this service may assume: the modelled 422,
    /// reported at `roleArn`.
    NotAssumable,
    /// The value is not the ARN of an IAM role at all, so it names no role IAM could be
    /// asked about: the modelled 400, naming the value.
    MalformedArn,
}

/// How a row addresses the role.
enum Role {
    /// IAM holds a role of this name under this path, carrying this trust document.
    Held {
        name: &'static str,
        path: &'static str,
        document: String,
    },
    /// The request carries this value verbatim; nothing is created for it.
    Verbatim(String),
    /// The request names no role at all.
    None,
}

/// One row: how the role is addressed, and what the create must be answered with.
struct Row {
    case: &'static str,
    role: Role,
    outcome: Outcome,
}

/// The rows: the two halves of the question IAM answers (does it hold the role, and does
/// the role's trust policy admit this service), the forms each half takes, the values that
/// name no role at all, and the request that names none.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a role whose trust policy names medialive",
            role: Role::Held {
                name: "medialive-channel-role",
                path: "/",
                document: trust_allowing(&[SERVICE_PRINCIPAL]),
            },
            outcome: Outcome::Created,
        },
        Row {
            case: "a role whose trust policy names medialive among several services",
            role: Role::Held {
                name: "medialive-channel-role-among-several",
                path: "/",
                document: trust_allowing(&["mediapackage.amazonaws.com", SERVICE_PRINCIPAL]),
            },
            outcome: Outcome::Created,
        },
        Row {
            case: "a role under a path whose trust policy names medialive",
            role: Role::Held {
                name: "medialive-channel-role-under-a-path",
                path: "/service-role/",
                document: trust_allowing(&[SERVICE_PRINCIPAL]),
            },
            outcome: Outcome::Created,
        },
        Row {
            case: "a role whose trust policy names another service only",
            role: Role::Held {
                name: "medialive-channel-role-other-service",
                path: "/",
                document: trust_allowing(&["mediapackage.amazonaws.com"]),
            },
            outcome: Outcome::NotAssumable,
        },
        Row {
            case: "a role whose trust policy denies medialive",
            role: Role::Held {
                name: "medialive-channel-role-denied",
                path: "/",
                document: json!({
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Deny",
                        "Principal": { "Service": SERVICE_PRINCIPAL },
                        "Action": "sts:AssumeRole"
                    }]
                })
                .to_string(),
            },
            outcome: Outcome::NotAssumable,
        },
        Row {
            // The statement names medialive, but for an action that is not assuming the
            // role, so it grants nothing: a trust policy governs `sts:AssumeRole`.
            case: "a role that allows medialive an action that is not assuming it",
            role: Role::Held {
                name: "medialive-channel-role-other-action",
                path: "/",
                document: json!({
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Principal": { "Service": SERVICE_PRINCIPAL },
                        "Action": "s3:GetObject"
                    }]
                })
                .to_string(),
            },
            outcome: Outcome::NotAssumable,
        },
        Row {
            // An account principal is not a service principal, so the document admits the
            // account's own principals and not this service.
            case: "a role whose trust policy names an account principal, not the service",
            role: Role::Held {
                name: "medialive-channel-role-account-principal",
                path: "/",
                document: json!({
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Principal": { "AWS": format!("arn:aws:iam::{ACCOUNT}:root") },
                        "Action": "sts:AssumeRole"
                    }]
                })
                .to_string(),
            },
            outcome: Outcome::NotAssumable,
        },
        Row {
            case: "a role nothing was created under",
            role: Role::Verbatim(format!("arn:aws:iam::{ACCOUNT}:role/medialive-no-such-role")),
            outcome: Outcome::NotAssumable,
        },
        Row {
            case: "a role name that is a prefix of one that exists",
            role: Role::Verbatim(format!("arn:aws:iam::{ACCOUNT}:role/medialive-channel-rol")),
            outcome: Outcome::NotAssumable,
        },
        Row {
            case: "an ARN naming an IAM user rather than a role",
            role: Role::Verbatim(format!("arn:aws:iam::{ACCOUNT}:user/medialive-channel-role")),
            outcome: Outcome::MalformedArn,
        },
        Row {
            case: "a value that is not an ARN at all",
            role: Role::Verbatim("medialive-channel-role".to_string()),
            outcome: Outcome::MalformedArn,
        },
        Row {
            case: "no role at all",
            role: Role::None,
            outcome: Outcome::Created,
        },
    ]
}

/// Rule, over every form of the role a request can name: `CreateChannel` resolves `RoleArn`
/// against this account's IAM and its trust policy before it stores anything, reports a role
/// this service cannot assume as the modelled 422 at `roleArn`, and a value that is not a
/// role ARN at all as the modelled 400 — and in both cases creates no channel and takes no
/// input attachment.
#[tokio::test]
async fn create_channel_resolves_its_role_against_iam_before_it_stores_a_channel() {
    let router = emulator();
    let group = input_security_group(&router, "setup").await;

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        // Each row gets its OWN input, so no row depends on what another row attached.
        let input_id = push_input(&router, case, &format!("feed-{index}"), &group).await;
        let role_arn = match &row.role {
            Role::Held {
                name,
                path,
                document,
            } => Some(role_with_trust(&router, name, path, document).await),
            Role::Verbatim(value) => Some(value.clone()),
            Role::None => None,
        };
        let channel_name = format!("role-channel-{index}");
        let (status, payload) = medialive(
            &router,
            "POST",
            "/prod/channels",
            Some(create_channel_body(
                &channel_name,
                &input_id,
                role_arn.as_deref(),
            )),
        )
        .await;

        match row.outcome {
            Outcome::Created => {
                assert_eq!(
                    status, 201,
                    "case {case:?}: a role this service may assume must be accepted, got \
                     {status}: {payload}"
                );
                assert_eq!(
                    payload.pointer("/channel/roleArn").and_then(Value::as_str),
                    role_arn.as_deref(),
                    "case {case:?}: expected the created channel to carry the role it runs \
                     as, got {payload}"
                );
                assert_eq!(
                    payload.pointer("/channel/state").and_then(Value::as_str),
                    Some("CREATING"),
                    "case {case:?}: expected the accepted create to report CREATING, got \
                     {payload}"
                );
                assert_eq!(
                    payload
                        .pointer("/channel/inputAttachments/0/inputId")
                        .and_then(Value::as_str),
                    Some(input_id.as_str()),
                    "case {case:?}: expected the created channel to attach the input the \
                     request named, got {payload}"
                );
                let channel_id = payload
                    .pointer("/channel/id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                // Read back through the service's own reader: what was stored, not what
                // the acknowledgement happened to say.
                let (status, described) = medialive(
                    &router,
                    "GET",
                    &format!("/prod/channels/{channel_id}"),
                    None,
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: DescribeChannel({channel_id}): expected 200, got \
                     {status}: {described}"
                );
                assert_eq!(
                    described["roleArn"].as_str(),
                    role_arn.as_deref(),
                    "case {case:?}: expected the stored channel to run as {role_arn:?}, got \
                     {described}"
                );
                // The accepted create is what takes the attachment.
                let held = describe_input(&router, case, &input_id).await;
                assert_eq!(
                    held["attachedChannels"],
                    json!([channel_id]),
                    "case {case:?}: expected the input to be held by the created channel, \
                     got {:?}",
                    held["attachedChannels"]
                );
                assert_eq!(
                    held["state"],
                    json!("ATTACHED"),
                    "case {case:?}: expected the held input to be ATTACHED, got {:?}",
                    held["state"]
                );
            }
            Outcome::NotAssumable => {
                let named = role_arn.clone().unwrap_or_default();
                assert_eq!(
                    status, 422,
                    "case {case:?}: a role IAM says this service cannot assume must be \
                     refused 422, got {status}: {payload}"
                );
                assert_eq!(
                    error_code(&payload),
                    "UnprocessableEntityException",
                    "case {case:?}: expected the modelled validation refusal, got {payload}"
                );
                assert_eq!(
                    error_message(&payload),
                    role_not_assumable_message(&named),
                    "case {case:?}: expected the element path then the sentence, got {payload}"
                );
                assert_eq!(
                    payload["validationErrors"],
                    json!([{
                        "elementPath": "roleArn",
                        "errorMessage": role_not_assumable_sentence(&named),
                    }]),
                    "case {case:?}: expected exactly one validationErrors entry naming \
                     roleArn, got {:?}",
                    payload["validationErrors"]
                );
                assert_no_channel_named(&router, case, &channel_name).await;
                assert_input_is_free(&router, case, &input_id).await;
            }
            Outcome::MalformedArn => {
                let named = role_arn.clone().unwrap_or_default();
                assert_eq!(
                    status, 400,
                    "case {case:?}: a value that names no IAM role must be refused 400, got \
                     {status}: {payload}"
                );
                assert_eq!(
                    error_code(&payload),
                    "BadRequestException",
                    "case {case:?}: expected the modelled malformed-request refusal, got \
                     {payload}"
                );
                assert_eq!(
                    error_message(&payload),
                    service_com_amazonaws_medialive_medialive::state::invalid_role_arn_message(
                        &named
                    ),
                    "case {case:?}: expected the refusal to name the value supplied, got \
                     {payload}"
                );
                assert_no_channel_named(&router, case, &channel_name).await;
                assert_input_is_free(&router, case, &input_id).await;
            }
        }
    }
}

/// Rule, at the ordering the bundle turns on: the role is resolved BEFORE any precondition
/// on the resources the request references, so a request that both names a role this
/// service cannot assume AND attaches an input another channel already holds is answered
/// the role's 422 — not the attachment's 409.
#[tokio::test]
async fn the_role_refusal_precedes_the_attachment_conflict() {
    let router = emulator();
    let group = input_security_group(&router, "setup").await;
    let good_role = role_with_trust(
        &router,
        "medialive-ordering-role",
        "/",
        &trust_allowing(&[SERVICE_PRINCIPAL]),
    )
    .await;

    // The rows differ in WHY IAM says the role cannot be assumed — it holds no such role,
    // and it holds one whose trust policy admits another service — because the ordering
    // must not depend on which half of the question answered.
    let unusable = [
        (
            "a role nothing was created under",
            format!("arn:aws:iam::{ACCOUNT}:role/medialive-ordering-absent"),
        ),
        (
            "a role whose trust policy names another service",
            role_with_trust(
                &router,
                "medialive-ordering-other-service",
                "/",
                &trust_allowing(&["mediapackage.amazonaws.com"]),
            )
            .await,
        ),
    ];

    for (index, (case, role_arn)) in unusable.into_iter().enumerate() {
        let input_id = push_input(&router, case, &format!("ordered-feed-{index}"), &group).await;
        // The input is first taken by a channel this service DID create, so the request
        // below is one the attachment rule would refuse on its own.
        let (status, holder) = medialive(
            &router,
            "POST",
            "/prod/channels",
            Some(create_channel_body(
                &format!("ordered-holder-{index}"),
                &input_id,
                Some(&good_role),
            )),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {case:?}: the holding channel must be created, got {status}: {holder}"
        );

        let contender = format!("ordered-contender-{index}");
        let (status, payload) = medialive(
            &router,
            "POST",
            "/prod/channels",
            Some(create_channel_body(&contender, &input_id, Some(&role_arn))),
        )
        .await;
        assert_eq!(
            status, 422,
            "case {case:?}: the role must be judged before the attachment, got {status}: \
             {payload}"
        );
        assert_eq!(
            error_code(&payload),
            "UnprocessableEntityException",
            "case {case:?}: expected the role's modelled refusal rather than the \
             attachment's conflict, got {payload}"
        );
        assert_eq!(
            error_message(&payload),
            role_not_assumable_message(&role_arn),
            "case {case:?}: expected the refusal to name roleArn, got {payload}"
        );
        assert_no_channel_named(&router, case, &contender).await;
        // The holding channel still holds exactly one attachment: the refused create
        // neither added nor released one.
        let held = describe_input(&router, case, &input_id).await;
        assert_eq!(
            held["attachedChannels"].as_array().map(Vec::len),
            Some(1),
            "case {case:?}: expected the input to stay held by the one channel that was \
             created, got {:?}",
            held["attachedChannels"]
        );
    }
}

/// Rule, over the whole trajectory a client recovers along: because the refused creates
/// stored nothing, the input they named is still free, so the corrected create succeeds —
/// and the estate tears down afterwards (channel, input, security group), with the input
/// reaching the terminal `DELETED` state and the group going back to `IDLE`.
#[tokio::test]
async fn a_create_corrected_after_a_role_refusal_succeeds_and_the_estate_tears_down() {
    let router = emulator();
    let case = "corrected after two role refusals";
    let group = input_security_group(&router, case).await;
    let input_id = push_input(&router, case, "recovered-feed", &group).await;

    // Both halves of the role question, refused in turn on the SAME input.
    let refused = [
        format!("arn:aws:iam::{ACCOUNT}:role/medialive-recovery-absent"),
        role_with_trust(
            &router,
            "medialive-recovery-other-service",
            "/",
            &trust_allowing(&["mediapackage.amazonaws.com"]),
        )
        .await,
    ];
    for (attempt, role_arn) in refused.iter().enumerate() {
        let channel_name = format!("recovered-channel-refused-{attempt}");
        let (status, payload) = medialive(
            &router,
            "POST",
            "/prod/channels",
            Some(create_channel_body(&channel_name, &input_id, Some(role_arn))),
        )
        .await;
        assert_eq!(
            status, 422,
            "case {case:?} attempt {attempt}: expected the role refusal, got {status}: \
             {payload}"
        );
        assert_input_is_free(&router, case, &input_id).await;
        assert_no_channel_named(&router, case, &channel_name).await;
    }

    // The retry with a role this service may assume: the input was never taken, so this is
    // a create and not a conflict.
    let good_role = role_with_trust(
        &router,
        "medialive-recovery-role",
        "/",
        &trust_allowing(&[SERVICE_PRINCIPAL]),
    )
    .await;
    let (status, created) = medialive(
        &router,
        "POST",
        "/prod/channels",
        Some(create_channel_body(
            "recovered-channel",
            &input_id,
            Some(&good_role),
        )),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: the corrected create must succeed, got {status}: {created}"
    );
    assert_eq!(
        created.pointer("/channel/state").and_then(Value::as_str),
        Some("CREATING"),
        "case {case:?}: expected the created channel in CREATING, got {created}"
    );
    assert_eq!(
        created
            .pointer("/channel/pipelinesRunningCount")
            .and_then(Value::as_u64),
        Some(0),
        "case {case:?}: expected no pipeline running on a channel just created, got {created}"
    );
    let channel_id = created
        .pointer("/channel/id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    // Teardown, in the order a client tears one down: the channel releases the input, the
    // input's own delete is then accepted, and the group is released once no input uses it.
    // A channel is deletable once it has stopped, which the create's own transition is what
    // settles it into, so that state is polled for rather than waited out.
    channel_in_state(&router, case, &channel_id, "IDLE").await;
    let (status, deleting) =
        medialive(&router, "DELETE", &format!("/prod/channels/{channel_id}"), None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DeleteChannel({channel_id}): expected 200, got {status}: {deleting}"
    );
    assert_input_is_free(&router, case, &input_id).await;

    let (status, deleted) =
        medialive(&router, "DELETE", &format!("/prod/inputs/{input_id}"), None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DeleteInput({input_id}): expected 200, got {status}: {deleted}"
    );
    assert_eq!(
        deleted["state"],
        json!("DELETING"),
        "case {case:?}: expected the accepted delete to answer DELETING, got {deleted}"
    );
    assert_eq!(
        deleted["attachedChannels"],
        json!([]),
        "case {case:?}: expected the deleted input to report no attached channel, got \
         {deleted}"
    );
    assert_eq!(
        deleted["securityGroups"],
        json!([group]),
        "case {case:?}: expected the deleted input to report the group that admitted it, \
         got {deleted}"
    );

    // The terminal state is the clock's to make due, so it is polled for.
    let settled = input_in_state(&router, case, &input_id, "DELETED").await;
    assert_eq!(
        settled["destinations"],
        json!([]),
        "case {case:?}: expected the deleted input to release its ingest endpoints, got \
         {settled}"
    );

    let (status, released) = medialive(
        &router,
        "GET",
        &format!("/prod/inputSecurityGroups/{group}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeInputSecurityGroup({group}): expected 200, got {status}: \
         {released}"
    );
    assert_eq!(
        released["inputs"],
        json!([]),
        "case {case:?}: expected no input to be using the group, got {released}"
    );
    assert_eq!(
        released["state"],
        json!("IDLE"),
        "case {case:?}: expected the released group to be IDLE, got {released}"
    );

    let (status, group_deleted) = medialive(
        &router,
        "DELETE",
        &format!("/prod/inputSecurityGroups/{group}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DeleteInputSecurityGroup({group}): expected 200, got {status}: \
         {group_deleted}"
    );
}
