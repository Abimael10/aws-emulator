// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of the `@length` class of this service's constraint envelope:
//! what a clause about a length bound STATES.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `iot` signing name — so what is asserted is what an AWS SDK
//! client would see, not an internal function's return value.
//!
//! The rule under test: a clause about a member's `@length` names the member and the bound
//! it broke and states NO value — `Value at '<member>' failed to satisfy constraint: Member
//! must have length {less,greater} than or equal to <bound>` — so the text that was too
//! long or too short is not reflected back to the caller. A bound and the value it rejected
//! say the same thing twice, and the recorded refusal of a member that broke its bound AND
//! its expression together states neither clause with a value. The classes that DO quote
//! the submitted value are unchanged, which is what keeps this about the length class
//! rather than about the envelope: an enumerated set is still stated beside the value it
//! rejected, in the same message as a length clause when one request breaks both.
//!
//! The envelope itself is the parent suite's: 400, `InvalidRequestException` qualified with
//! the service's API doc namespace in `x-amzn-errortype`, `N validation error(s) detected: `
//! over one clause per broken constraint joined with `; `.
//!
//! The table spans what the rule generalises over rather than one request: both ends of a
//! bound (a value past a maximum and one short of a minimum), a bound both ends of which
//! are the same number and one whose ends differ, members bound to the URI path, to the
//! query string and to the body, four operations, a length clause alone and beside a clause
//! of another class, and the clause counts 1 and 2.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region every request in this suite is signed for. No refusal here is regional, so
/// no assertion depends on it.
const REGION: &str = "eu-west-3";

/// `InvalidRequestException` as AWS IoT names it in `x-amzn-errortype`: the shape name, a
/// `:`, and the service's API doc namespace.
const QUALIFIED_INVALID_REQUEST: &str =
    "InvalidRequestException:http://iot.amazonaws.com/doc/2015-10-02/";

/// A `certificateId` of the modelled form — `length 64..64, pattern ^(0x)?[a-fA-F0-9]+$`
/// (`smithy_query(query="operation",operation="UpdateCertificate")`) — so a row about
/// ANOTHER member of the same request draws no clause about this one. The store holding no
/// certificate under it is immaterial: the validation layer answers before any handler
/// runs.
const WELL_FORMED_CERTIFICATE_ID: &str =
    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// An `AwsAccountId` of the modelled form — `length 12..12, pattern ^[0-9]+$` — for the
/// rows that break a constraint on some other member.
const WELL_FORMED_ACCOUNT_ID: &str = "123456789012";

/// A SigV4 `Authorization` header whose credential scope names the `iot` signing name: the
/// scope is how the router picks the claiming service. The signature itself is never
/// checked here — the harness turns request authentication off, exactly as an
/// unauthenticated emulator runs.
fn signed_for_iot() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTIOT/20260101/{REGION}/iot/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=0000"
    )
}

/// One response, reduced to what a client branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    /// The `x-amzn-errortype` header, verbatim — including any namespace the shape name is
    /// qualified with.
    error_type: Option<String>,
    body: Value,
}

impl Reply {
    /// The `message` the body carries, or `""` when it carries none.
    fn message(&self) -> String {
        self.body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

/// The emulator under test: ONE shared engine with the IoT service mounted on it, reached
/// the way the production router reaches it.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    async fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Reply {
        let payload = body.map_or_else(Vec::new, |value| {
            serde_json::to_vec(value).expect("a request body serializes")
        });
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://iot.{REGION}.amazonaws.com{path}"))
            .header("authorization", signed_for_iot())
            .header("content-type", "application/json")
            .body(bytes::Bytes::from(payload))
            .expect("a well-formed request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_type = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Reply {
            status,
            error_type,
            body,
        }
    }
}

/// The clause a refusal states for a member past its `@length` maximum.
fn past_the_maximum(member: &str, maximum: usize) -> String {
    format!(
        "Value at '{member}' failed to satisfy constraint: Member must have length less \
         than or equal to {maximum}"
    )
}

/// The clause a refusal states for a member short of its `@length` minimum.
fn short_of_the_minimum(member: &str, minimum: usize) -> String {
    format!(
        "Value at '{member}' failed to satisfy constraint: Member must have length greater \
         than or equal to {minimum}"
    )
}

/// The clause a refusal states for a member outside its `@pattern`, whose expression is
/// quoted without the whole-value anchors the model wraps it in.
fn outside_the_pattern(member: &str, expression: &str) -> String {
    format!(
        "Value at '{member}' failed to satisfy constraint: Member must satisfy regular \
         expression pattern: {expression}"
    )
}

/// One request this service's validation layer will not accept, and the clauses the
/// refusal must state for it, in the order it states them.
struct RefusedRequest {
    /// What this row is, for the assertion messages.
    case: String,
    method: &'static str,
    path: String,
    /// A body for the rows that carry one.
    body: Option<Value>,
    clauses: Vec<String>,
    /// What the request sent for the member the length clause is about: no clause may quote
    /// it.
    rejected: String,
}

/// A string of `length` characters that every `@pattern` in this suite admits: word
/// characters satisfy `^[0-9]+$`'s complement cases as well as `^[\w+=,.@-]+$` and
/// `^[a-zA-Z0-9:_-]+$`, so a row's only broken constraint is the length bound it is about.
fn word_characters(length: usize) -> String {
    "a".repeat(length)
}

/// Every row of the length table.
fn refused_requests() -> Vec<RefusedRequest> {
    let mut rows = Vec::new();

    // `TransferCertificate$targetAwsAccount` — `@httpQuery`, `length 12..12, pattern
    // ^[0-9]+$` (`smithy_query(query="operation",operation="TransferCertificate")`): the
    // member whose bound has the same number at both ends, so a value of the wrong length
    // breaks one end or the other. Non-numeric text breaks the expression as well, and
    // neither clause quotes it.
    let target = "targetAwsAccount";
    let digits = "[0-9]+";
    for (case, value, clauses) in [
        (
            "a target account longer than the bound and not numeric",
            word_characters(31),
            vec![past_the_maximum(target, 12), outside_the_pattern(target, digits)],
        ),
        (
            "a target account shorter than the bound and not numeric",
            word_characters(5),
            vec![
                short_of_the_minimum(target, 12),
                outside_the_pattern(target, digits),
            ],
        ),
        (
            "a numeric target account one digit too long",
            "1234567890123".to_string(),
            vec![past_the_maximum(target, 12)],
        ),
        (
            "a numeric target account one digit too short",
            "12345678901".to_string(),
            vec![short_of_the_minimum(target, 12)],
        ),
    ] {
        rows.push(RefusedRequest {
            case: format!("TransferCertificate with {case}"),
            method: "PATCH",
            path: format!(
                "/transfer-certificate/{WELL_FORMED_CERTIFICATE_ID}?targetAwsAccount={value}"
            ),
            body: None,
            clauses,
            rejected: value,
        });
    }

    // The same operation's `transferMessage` — a BODY member, `length 0..128`: a bound
    // whose ends differ, broken at the only end a value can break.
    let message = word_characters(129);
    rows.push(RefusedRequest {
        case: "TransferCertificate with a transfer message past its maximum".to_string(),
        method: "PATCH",
        path: format!(
            "/transfer-certificate/{WELL_FORMED_CERTIFICATE_ID}\
             ?targetAwsAccount={WELL_FORMED_ACCOUNT_ID}"
        ),
        body: Some(json!({ "transferMessage": message.clone() })),
        clauses: vec![past_the_maximum("transferMessage", 128)],
        rejected: message,
    });

    // A URI LABEL past its maximum, on two operations of two other resource families:
    // `CreatePolicy$policyName` and `CreateThing$thingName`, each `length 1..128` with an
    // expression these characters satisfy
    // (`smithy_query(query="operation",operation="CreatePolicy")`,
    // `smithy_query(query="operation",operation="CreateThing")`).
    let name = word_characters(129);
    rows.push(RefusedRequest {
        case: "CreatePolicy with a policy name past its maximum".to_string(),
        method: "POST",
        path: format!("/policies/{name}"),
        body: Some(json!({ "policyDocument": "{}" })),
        clauses: vec![past_the_maximum("policyName", 128)],
        rejected: name.clone(),
    });
    rows.push(RefusedRequest {
        case: "CreateThing with a thing name past its maximum".to_string(),
        method: "POST",
        path: format!("/things/{name}"),
        body: Some(json!({})),
        clauses: vec![past_the_maximum("thingName", 128)],
        rejected: name,
    });

    rows
}

/// RULE: a clause about a `@length` bound names the member and the bound and states no
/// value, at either end of the bound, for a member bound anywhere in the request, on every
/// operation — and the envelope counts the clauses as it does for any other class.
#[tokio::test]
async fn a_length_clause_states_the_bound_and_not_the_text_that_broke_it() {
    for case in refused_requests() {
        // A fresh emulator per row: a validation refusal is decided by the REQUEST, so it
        // must not depend on what another row left behind.
        let emulator = Emulator::new();
        let reply = emulator
            .call(case.method, &case.path, case.body.as_ref())
            .await;
        assert_eq!(
            reply.status, 400,
            "case {:?}: expected status 400, got {} {:?}",
            case.case, reply.status, reply.body
        );
        assert_eq!(
            reply.error_type.as_deref(),
            Some(QUALIFIED_INVALID_REQUEST),
            "case {:?}: expected x-amzn-errortype {QUALIFIED_INVALID_REQUEST:?}, got {:?}",
            case.case,
            reply.error_type
        );
        let envelope = match case.clauses.len() {
            1 => "1 validation error detected: ".to_string(),
            n => format!("{n} validation errors detected: "),
        };
        let expected = format!("{envelope}{}", case.clauses.join("; "));
        assert_eq!(
            reply.message(),
            expected,
            "case {:?}: expected message {expected:?}, got {:?}",
            case.case,
            reply.message()
        );
        assert!(
            !reply.message().contains(&case.rejected),
            "case {:?}: expected the clause to state no value, got {:?}",
            case.case,
            reply.message()
        );
    }
}

/// RULE: the classes that quote the submitted value still quote it — the change is keyed to
/// the length class and not to the envelope — including in a message that states a length
/// clause beside one of them.
#[tokio::test]
async fn a_class_that_quotes_the_submitted_value_still_quotes_it() {
    // `UpdateCertificate$newStatus` is `@httpQuery`, required, and targets the
    // `CertificateStatus` enumerated type
    // (`smithy_query(query="operation",operation="UpdateCertificate")`), so a value outside
    // that type's set is the class that quotes what the request sent. The certificateId is
    // well formed in the first row and past its `length 64..64` maximum in the second, so
    // the second message carries both classes at once.
    let rejected_status = "NOT_A_STATUS";
    let over_long_id = format!("{WELL_FORMED_CERTIFICATE_ID}0");
    let rows = [
        (
            "an unmodelled status alone",
            WELL_FORMED_CERTIFICATE_ID.to_string(),
            1,
            None,
        ),
        (
            "an unmodelled status beside an over-long certificate id",
            over_long_id.clone(),
            2,
            Some(past_the_maximum("certificateId", 64)),
        ),
    ];

    for (case, certificate_id, clauses, length_clause) in rows {
        let emulator = Emulator::new();
        let reply = emulator
            .call(
                "PUT",
                &format!("/certificates/{certificate_id}?newStatus={rejected_status}"),
                None,
            )
            .await;
        assert_eq!(
            reply.status, 400,
            "case {case:?}: expected status 400, got {} {:?}",
            reply.status, reply.body
        );
        let envelope = match clauses {
            1 => "1 validation error detected: ".to_string(),
            n => format!("{n} validation errors detected: "),
        };
        assert!(
            reply.message().starts_with(&envelope),
            "case {case:?}: expected the envelope {envelope:?}, got {:?}",
            reply.message()
        );
        let quoted = format!(
            "Value '{rejected_status}' at 'newStatus' failed to satisfy constraint: Member \
             must satisfy enum value set: ["
        );
        assert!(
            reply.message().contains(&quoted),
            "case {case:?}: expected the enum clause to quote the submitted value, got {:?}",
            reply.message()
        );
        if let Some(length_clause) = length_clause {
            assert!(
                reply.message().contains(&length_clause),
                "case {case:?}: expected the clause {length_clause:?}, got {:?}",
                reply.message()
            );
            assert!(
                !reply.message().contains(&format!("Value '{certificate_id}'")),
                "case {case:?}: expected the length clause to state no value, got {:?}",
                reply.message()
            );
        }
    }
}

/// The other side of the same decision: a request whose members are all within their bounds
/// draws no constraint envelope at all, so the refusals above are about the lengths the
/// requests sent and not about the operations.
#[tokio::test]
async fn a_request_within_every_bound_is_not_refused_that_way() {
    let rows: Vec<(&str, &str, String, Option<Value>)> = vec![
        (
            "TransferCertificate with a twelve-digit target account",
            "PATCH",
            format!(
                "/transfer-certificate/{WELL_FORMED_CERTIFICATE_ID}\
                 ?targetAwsAccount={WELL_FORMED_ACCOUNT_ID}"
            ),
            None,
        ),
        (
            "TransferCertificate with a transfer message at its maximum",
            "PATCH",
            format!(
                "/transfer-certificate/{WELL_FORMED_CERTIFICATE_ID}\
                 ?targetAwsAccount={WELL_FORMED_ACCOUNT_ID}"
            ),
            Some(json!({ "transferMessage": word_characters(128) })),
        ),
        (
            "CreatePolicy with a policy name at its maximum",
            "POST",
            format!("/policies/{}", word_characters(128)),
            Some(json!({
                "policyDocument": r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"iot:Connect","Resource":"*"}]}"#,
            })),
        ),
        (
            "CreateThing with a thing name at its maximum",
            "POST",
            format!("/things/{}", word_characters(128)),
            Some(json!({})),
        ),
    ];

    for (case, method, path, body) in rows {
        let emulator = Emulator::new();
        let reply = emulator.call(method, &path, body.as_ref()).await;
        assert!(
            !reply.message().contains(" validation error"),
            "case {case:?}: expected no constraint clause, got {} {:?}",
            reply.status,
            reply.body
        );
    }
}
