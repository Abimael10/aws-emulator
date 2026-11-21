// SPDX-License-Identifier: CC0-1.0

//! What `sms-voice` answers a request it refuses BEFORE any handler runs, end to end over
//! the wire.
//!
//! Every request goes through [`MultiServiceRouter::dispatch`] as `awsJson1_0` — `POST /`
//! with `X-Amz-Target: PinpointSMSVoiceV2.<Op>` — so these tests exercise routing,
//! deserialization and the boundary constraint walk exactly as a real SDK client reaches
//! them.
//!
//! The rule under test: a request that breaks a rule the MODEL declares — a `@required`
//! member absent, a value outside a declared `@length`, `@range` or `@pattern` — is
//! answered with this service's own modelled `ValidationException`: HTTP 400, error code
//! `ValidationException`, `Message` the fixed sentence naming the rejected members,
//! `Reason` `FIELD_VALIDATION_FAILED`, and one `Fields` entry per rejected member carrying
//! its wire name and the requirement it failed. It holds for every operation the model
//! declares `ValidationException` on, whether that operation's handler is implemented or
//! still a stub, because the refusal happens before the handler is reached — and a request
//! the model ACCEPTS still reaches its handler untouched.
//!
//! The suite also pins the three message templates this service's own handlers render for
//! the refusals they own (a conflicting name, a replayed `ClientToken`, an absent
//! resource), since those travel in the same error bodies.
//!
//! Its harness is this suite's own: the sibling suites' helpers are private to their
//! modules, and a test target shares nothing it does not export.

use std::sync::Arc;

use service_com_amazonaws_pinpointsmsvoicev2_pinpointsmsvoicev2 as sms_voice;
use smithy_server_runtime::MultiServiceRouter;

/// The region these requests sign for.
const REGION: &str = "us-west-2";

/// The fixed text this service refuses a request with, up to the bracketed member list.
const REFUSAL_PREFIX: &str = "The following parameter is null, empty or invalid: ";

/// One emulator: the shared engine plus a router with only this service mounted.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // These tests assert behaviour, not signatures; the emulator verifies signatures
        // itself and a service never checks them.
        router.set_request_authentication(false);
        router.register_service(sms_voice::EmulatedService::new(engine));
        Self { router }
    }

    /// Call one operation and answer `(http status, parsed JSON body)`.
    async fn call(&self, operation: &str, body: serde_json::Value) -> (u16, serde_json::Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("content-type", "application/x-amz-json-1.0")
            .header("x-amz-target", format!("PinpointSMSVoiceV2.{operation}"))
            // The SigV4 credential scope is what routes the request to this service.
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/sms-voice/aws4_request, \
                     SignedHeaders=host;x-amz-date, Signature=0123456789abcdef"
                ),
            )
            .body(bytes::Bytes::from(
                serde_json::to_vec(&body).expect("the request body serializes"),
            ))
            .expect("a valid request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed = if response.body().is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(serde_json::Value::Null)
        };
        (status, parsed)
    }

    /// Call one operation and require HTTP 200, reporting the body when it is not.
    async fn ok(&self, operation: &str, body: serde_json::Value) -> serde_json::Value {
        let (status, parsed) = self.call(operation, body).await;
        assert_eq!(
            status, 200,
            "{operation}: expected HTTP 200, got {status} with {parsed}"
        );
        parsed
    }
}

/// The wire error code of a failed call (`__type`, with any namespace prefix dropped).
fn error_code(body: &serde_json::Value) -> String {
    let raw = body
        .get("__type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("<no __type>");
    raw.rsplit('#').next().unwrap_or(raw).to_string()
}

fn member(body: &serde_json::Value, name: &str) -> String {
    body.get(name)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| panic!("expected a string member {name} in {body}"))
        .to_string()
}

/// `Fields` as `(Name, Message)` pairs, in the order the body lists them.
fn fields(body: &serde_json::Value) -> Vec<(String, String)> {
    body.get("Fields")
        .and_then(serde_json::Value::as_array)
        .unwrap_or_else(|| panic!("expected a Fields list in {body}"))
        .iter()
        .map(|field| (member(field, "Name"), member(field, "Message")))
        .collect()
}

/// One refused request: the operation, the body to send, and the members the refusal must
/// name with the requirement each of them failed.
struct Refused<'case> {
    case: &'case str,
    operation: &'case str,
    body: serde_json::Value,
    rejected: Vec<(&'case str, &'case str)>,
}

/// Assert `refused` is answered in this service's modelled `ValidationException`.
async fn assert_refused(emulator: &Emulator, refused: &Refused<'_>) {
    let case = refused.case;
    let (status, body) = emulator.call(refused.operation, refused.body.clone()).await;
    assert_eq!(status, 400, "case {case:?}: expected HTTP 400, got {body}");
    assert_eq!(
        error_code(&body),
        "ValidationException",
        "case {case:?}: expected the modelled error code, got {body}"
    );
    let quoted: Vec<String> = refused
        .rejected
        .iter()
        .map(|(name, _)| format!("'{name}'"))
        .collect();
    // The members are named in the order the input shape declares them, which is the order
    // the boundary walk reports a violation in.
    let expected_message = format!("{REFUSAL_PREFIX}[{}]", quoted.join(", "));
    assert_eq!(
        member(&body, "Message"),
        expected_message,
        "case {case:?}: got {body}"
    );
    assert_eq!(
        member(&body, "Reason"),
        "FIELD_VALIDATION_FAILED",
        "case {case:?}: got {body}"
    );
    let expected_fields: Vec<(String, String)> = refused
        .rejected
        .iter()
        .map(|(name, requirement)| ((*name).to_string(), (*requirement).to_string()))
        .collect();
    assert_eq!(
        fields(&body),
        expected_fields,
        "case {case:?}: got {body}"
    );
}

/// A modelled-constraint violation is refused in the modelled shape, across the constraint
/// classes the model declares and across operations — including one whose handler is still
/// a stub, since the refusal happens before any handler runs.
///
/// The requirements are the ones the model declares at each member
/// (`smithy_query(query="operation", operation="CreateConfigurationSet,PutOptedOutNumber")`:
/// `ConfigurationSetName` len 1..64 pattern `^[A-Za-z0-9_-]+$`, `ClientToken` len 1..64
/// pattern `^[!-~]+$`, `OptOutListName` len 1..256 pattern `^[A-Za-z0-9_:/-]+$`,
/// `OptedOutNumber` len 1..20 pattern `^\+?[1-9][0-9]{1,18}$`), each quoted without the
/// whole-value anchors the model writes around it.
#[tokio::test]
async fn a_modelled_constraint_violation_is_refused_as_the_modelled_validation_exception() {
    let emulator = Emulator::new();
    let too_long = "a".repeat(65);
    let cases = [
        Refused {
            case: "a name outside its pattern",
            operation: "CreateConfigurationSet",
            body: serde_json::json!({"ConfigurationSetName": "bad name!"}),
            rejected: vec![(
                "configurationSetName",
                "Member must satisfy regular expression pattern: [A-Za-z0-9_-]+",
            )],
        },
        Refused {
            case: "a name past its declared maximum length",
            operation: "CreateConfigurationSet",
            body: serde_json::json!({"ConfigurationSetName": too_long}),
            rejected: vec![(
                "configurationSetName",
                "Member must have length less than or equal to 64",
            )],
        },
        Refused {
            case: "a required member absent",
            operation: "CreateConfigurationSet",
            body: serde_json::json!({}),
            rejected: vec![("configurationSetName", "Member must not be null")],
        },
        Refused {
            case: "a second member of the same operation, outside its own pattern",
            operation: "CreateConfigurationSet",
            body: serde_json::json!({
                "ConfigurationSetName": "refusal-probe-set",
                "ClientToken": "token with a space",
            }),
            rejected: vec![(
                "clientToken",
                "Member must satisfy regular expression pattern: [!-~]+",
            )],
        },
        Refused {
            case: "a phone number outside its pattern, on an operation that is still a stub",
            operation: "PutOptedOutNumber",
            body: serde_json::json!({
                "OptOutListName": "refusal-probe-list",
                "OptedOutNumber": "not-a-number",
            }),
            rejected: vec![(
                "optedOutNumber",
                "Member must satisfy regular expression pattern: \\+?[1-9][0-9]{1,18}",
            )],
        },
        Refused {
            case: "two members of one request, each outside its own pattern",
            operation: "PutOptedOutNumber",
            body: serde_json::json!({
                "OptOutListName": "bad list!",
                "OptedOutNumber": "0123456789",
            }),
            rejected: vec![
                (
                    "optOutListName",
                    "Member must satisfy regular expression pattern: [A-Za-z0-9_:/-]+",
                ),
                (
                    "optedOutNumber",
                    "Member must satisfy regular expression pattern: \\+?[1-9][0-9]{1,18}",
                ),
            ],
        },
    ];
    for refused in &cases {
        assert_refused(&emulator, refused).await;
    }
}

/// A `@range` is served on each published bound and refused one past it, in the same
/// modelled shape.
///
/// `DescribeConfigurationSets.MaxResults` is modelled `range {min 1, max 100}`
/// (`smithy_query(query="operation", operation="DescribeConfigurationSets")`).
#[tokio::test]
async fn a_range_is_served_on_its_published_bounds_and_refused_past_them() {
    let emulator = Emulator::new();
    for max_results in [1, 100] {
        let answered = emulator
            .ok(
                "DescribeConfigurationSets",
                serde_json::json!({"MaxResults": max_results}),
            )
            .await;
        assert!(
            answered.get("ConfigurationSets").is_some(),
            "case {max_results}: a bound of the declared range is served, got {answered}"
        );
    }
    let cases = [
        Refused {
            case: "below the declared minimum",
            operation: "DescribeConfigurationSets",
            body: serde_json::json!({"MaxResults": 0}),
            rejected: vec![(
                "maxResults",
                "Member must have value greater than or equal to 1",
            )],
        },
        Refused {
            case: "above the declared maximum",
            operation: "DescribeConfigurationSets",
            body: serde_json::json!({"MaxResults": 101}),
            rejected: vec![("maxResults", "Member must have value less than or equal to 100")],
        },
    ];
    for refused in &cases {
        assert_refused(&emulator, refused).await;
    }
}

/// A request the model accepts is not refused: it reaches its handler, which persists it
/// and answers from the stored record.
#[tokio::test]
async fn a_request_the_model_accepts_still_reaches_its_handler() {
    let emulator = Emulator::new();
    let created = emulator
        .ok(
            "CreateConfigurationSet",
            serde_json::json!({
                "ConfigurationSetName": "refusal-probe-set",
                "ClientToken": "refusal-probe-token-1",
                "Tags": [{"Key": "team", "Value": "messaging"}],
            }),
        )
        .await;
    assert_eq!(
        member(&created, "ConfigurationSetName"),
        "refusal-probe-set",
        "the create answers from the record it stored, got {created}"
    );
    let described = emulator
        .ok(
            "DescribeConfigurationSets",
            serde_json::json!({"ConfigurationSetNames": ["refusal-probe-set"]}),
        )
        .await;
    let names: Vec<String> = described
        .get("ConfigurationSets")
        .and_then(serde_json::Value::as_array)
        .unwrap_or_else(|| panic!("expected ConfigurationSets in {described}"))
        .iter()
        .map(|set| member(set, "ConfigurationSetName"))
        .collect();
    assert_eq!(
        names,
        vec!["refusal-probe-set".to_string()],
        "the stored set is read back, got {described}"
    );

    // A request the model accepts on an operation whose handler is still a stub is not a
    // validation refusal either: it is whatever that handler answers, with none of the
    // refusal's members.
    let (status, body) = emulator
        .call(
            "PutOptedOutNumber",
            serde_json::json!({
                "OptOutListName": "refusal-probe-list",
                "OptedOutNumber": "+14255550199",
            }),
        )
        .await;
    assert_ne!(
        error_code(&body),
        "ValidationException",
        "a request the model accepts is not refused as invalid, got HTTP {status} with {body}"
    );
    assert!(
        body.get("Fields").is_none() && body.get("Reason").is_none(),
        "a request the model accepts carries no field list, got HTTP {status} with {body}"
    );
}

/// The refusals this service's own handlers own keep their message templates, which travel
/// in the same error bodies as the boundary refusal above: a conflicting name and a
/// replayed `ClientToken` are conflicts, and an absent resource is a not-found.
#[tokio::test]
async fn a_handler_refusal_states_this_service_s_message_template() {
    let emulator = Emulator::new();
    emulator
        .ok(
            "CreateConfigurationSet",
            serde_json::json!({
                "ConfigurationSetName": "template-probe-set",
                "ClientToken": "template-probe-token-1",
            }),
        )
        .await;

    let (status, duplicate) = emulator
        .call(
            "CreateConfigurationSet",
            serde_json::json!({"ConfigurationSetName": "template-probe-set"}),
        )
        .await;
    assert_eq!(status, 400, "a duplicate name conflicts, got {duplicate}");
    assert_eq!(
        error_code(&duplicate),
        "ConflictException",
        "got {duplicate}"
    );
    assert_eq!(
        member(&duplicate, "Message"),
        "Conflict Occurred - Reason=\"RESOURCE_ALREADY_EXISTS\" \
         ResourceType=\"configuration-set\" ResourceId=\"template-probe-set\"",
        "got {duplicate}"
    );

    let (status, replayed) = emulator
        .call(
            "CreateConfigurationSet",
            serde_json::json!({
                "ConfigurationSetName": "template-probe-other-set",
                "ClientToken": "template-probe-token-1",
            }),
        )
        .await;
    assert_eq!(
        status, 400,
        "a token replayed with another request conflicts, got {replayed}"
    );
    assert_eq!(error_code(&replayed), "ConflictException", "got {replayed}");
    assert_eq!(
        member(&replayed, "Message"),
        "Token used for different request",
        "got {replayed}"
    );

    let (status, absent) = emulator
        .call(
            "DeleteConfigurationSet",
            serde_json::json!({"ConfigurationSetName": "template-probe-never-created"}),
        )
        .await;
    assert_eq!(status, 400, "an absent set is not found, got {absent}");
    assert_eq!(
        error_code(&absent),
        "ResourceNotFoundException",
        "got {absent}"
    );
    assert_eq!(
        member(&absent, "Message"),
        "Resource Not Found - ResourceType=\"configuration-set\" \
         ResourceId=\"template-probe-never-created\"",
        "got {absent}"
    );
}
