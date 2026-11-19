// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of the `@pattern` class of this service's constraint envelope:
//! which half of a clause about an expression is STATED.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `iot` signing name — so what is asserted is what an AWS SDK client
//! would see, not an internal function's return value.
//!
//! The rule under test: a clause about a member outside the `@pattern` of a THING-REGISTRY
//! NAME states that name, single-quoted, beside the expression it had to match — `Value
//! '<name>' at '<member>' failed to satisfy constraint: Member must satisfy regular
//! expression pattern: <expression>` — while a member of any OTHER plane outside its own
//! `@pattern` is stated with no value at all, the clause naming the expression alone. The
//! registry's name types are one string type repeated (`ThingName`, `ThingGroupName`,
//! `ThingTypeName`, `BillingGroupName`, each `length 1..128, pattern ^[a-zA-Z0-9:_-]+$`:
//! `smithy_query(query="shape",shape="ThingName,ThingGroupName,ThingTypeName,BillingGroupName")`),
//! so the split is by the name TYPE the member targets and not by the operation: it holds for
//! a create, a read and a delete alike, and for a name bound to the URI as much as one bound
//! to the body. The expression a clause quotes is the model's with the whole-value anchors
//! dropped.
//!
//! The other classes of the same envelope are the sibling suites': `@length` in
//! [`super::length_constraint_clauses`] and `@range` in
//! [`super::range_constraint_clauses`]; the envelope itself — 400,
//! `InvalidRequestException` qualified with the service's API doc namespace in
//! `x-amzn-errortype`, `N validation error(s) detected: ` over one clause per broken
//! constraint — is the parent suite's.
//!
//! The table spans what the rule generalises over rather than one request: four registry name
//! types over six operations, a name bound to the URI path and one bound to the body, three
//! shapes of illegal name (an embedded space, a dot, a plus, a slash), and two other planes'
//! expressions for the withholding half. The round trip at the end is the other side of the
//! same decision: a name the expression ADMITS is created and read back, so the refusals are
//! about the names the requests sent and not about the operations.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region every request in this suite is signed for. No refusal here is regional, so no
/// assertion depends on it.
const REGION: &str = "eu-west-3";

/// `InvalidRequestException` as AWS IoT names it in `x-amzn-errortype`: the shape name, a
/// `:`, and the service's API doc namespace.
const QUALIFIED_INVALID_REQUEST: &str =
    "InvalidRequestException:http://iot.amazonaws.com/doc/2015-10-02/";

/// The expression every registry name type carries, as a clause quotes it: the model's
/// `^[a-zA-Z0-9:_-]+$` without the whole-value anchors.
const REGISTRY_NAME_EXPRESSION: &str = "[a-zA-Z0-9:_-]+";

/// A SigV4 `Authorization` header whose credential scope names the `iot` signing name: the
/// scope is how the router picks the claiming service. The signature itself is never checked
/// here — the harness turns request authentication off, exactly as an unauthenticated
/// emulator runs.
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

/// The emulator under test: ONE shared engine with the IoT service mounted on it, reached the
/// way the production router reaches it.
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

/// A URI path segment carrying `value`: a character a path may not hold travels
/// percent-encoded, as any client sends it, and what the clause states is the value the
/// service read back.
fn path_segment(value: &str) -> String {
    value
        .chars()
        .map(|character| match character {
            ' ' => "%20".to_string(),
            '+' => "%2B".to_string(),
            '?' | '#' | '&' | '/' => format!("%{:02X}", character as u32),
            other => other.to_string(),
        })
        .collect()
}

/// The clause a refusal states for a member outside its `@pattern`, with the submitted value
/// quoted.
fn outside_the_pattern_stating(member: &str, value: &str, expression: &str) -> String {
    format!(
        "Value '{value}' at '{member}' failed to satisfy constraint: Member must satisfy \
         regular expression pattern: {expression}"
    )
}

/// The clause a refusal states for a member outside its `@pattern` with no value at all.
fn outside_the_pattern_withholding(member: &str, expression: &str) -> String {
    format!(
        "Value at '{member}' failed to satisfy constraint: Member must satisfy regular \
         expression pattern: {expression}"
    )
}

/// One request refused for a member outside its `@pattern`, and what the clause must state.
struct PatternRefusal {
    case: String,
    method: &'static str,
    path: String,
    body: Option<Value>,
    clause: String,
    /// What the request sent for that member.
    rejected: String,
    /// Whether the clause quotes it.
    quotes_the_value: bool,
}

/// Every row of the `@pattern` table: the registry's names, which state the rejected name,
/// and the other planes' members, which state none.
fn pattern_refusals() -> Vec<PatternRefusal> {
    let mut rows = Vec::new();

    // A registry NAME bound to the URI, over the four name types the registry repeats and the
    // operations that address one: a create of each, a read and a delete. The rejected names
    // differ in which character the expression refuses and where it sits.
    let registry_labels: [(&str, &str, &str, &str, Option<Value>); 6] = [
        (
            "CreateThing",
            "POST",
            "/things",
            "thingName",
            Some(json!({})),
        ),
        ("DescribeThing", "GET", "/things", "thingName", None),
        (
            "CreateThingGroup",
            "POST",
            "/thing-groups",
            "thingGroupName",
            Some(json!({})),
        ),
        (
            "DeleteThingGroup",
            "DELETE",
            "/thing-groups",
            "thingGroupName",
            None,
        ),
        (
            "CreateThingType",
            "POST",
            "/thing-types",
            "thingTypeName",
            Some(json!({})),
        ),
        (
            "CreateBillingGroup",
            "POST",
            "/billing-groups",
            "billingGroupName",
            Some(json!({})),
        ),
    ];
    // Three shapes of name the registry expression refuses: a space inside the name, a dot,
    // and a trailing plus.
    let rejected_names = ["probe name", "probe.name", "probe+name"];
    for (operation, method, prefix, member, body) in registry_labels {
        for rejected in rejected_names {
            rows.push(PatternRefusal {
                case: format!("{operation} on the name {rejected:?}"),
                method,
                path: format!("{prefix}/{}", path_segment(rejected)),
                body: body.clone(),
                clause: outside_the_pattern_stating(member, rejected, REGISTRY_NAME_EXPRESSION),
                rejected: rejected.to_string(),
                quotes_the_value: true,
            });
        }
    }

    // The same name type bound to the BODY rather than to the URI: `CreateThing`'s
    // `thingTypeName` targets the registry's thing-type name
    // (`smithy_query(query="operation",operation="CreateThing")`), so the rule is about the
    // member's type and not about where the request carries it.
    for rejected in ["probe type", "probe/type"] {
        rows.push(PatternRefusal {
            case: format!("CreateThing with the body thing type {rejected:?}"),
            method: "POST",
            path: "/things/probe-thing".to_string(),
            body: Some(json!({ "thingTypeName": rejected })),
            clause: outside_the_pattern_stating(
                "thingTypeName",
                rejected,
                REGISTRY_NAME_EXPRESSION,
            ),
            rejected: rejected.to_string(),
            quotes_the_value: true,
        });
    }

    // Another plane's member outside ITS expression states no value: the domain-configuration
    // name (`DomainConfigurationName`: `^[\w.-]+$`) and a policy version id
    // (`PolicyVersionId`: `^[0-9]+$`).
    for rejected in ["IoT:reserved", "config!"] {
        rows.push(PatternRefusal {
            case: format!("CreateDomainConfiguration named {rejected:?}"),
            method: "POST",
            path: format!("/domainConfigurations/{}", path_segment(rejected)),
            body: Some(json!({})),
            clause: outside_the_pattern_withholding("domainConfigurationName", r"[\w.-]+"),
            rejected: rejected.to_string(),
            quotes_the_value: false,
        });
    }
    for rejected in ["v2", "latest"] {
        rows.push(PatternRefusal {
            case: format!("SetDefaultPolicyVersion on the version {rejected:?}"),
            method: "PATCH",
            path: format!("/policies/probe-policy/version/{}", path_segment(rejected)),
            body: None,
            clause: outside_the_pattern_withholding("policyVersionId", "[0-9]+"),
            rejected: rejected.to_string(),
            quotes_the_value: false,
        });
    }

    rows
}

/// RULE: a clause about a member outside the registry name expression states the rejected
/// name, single-quoted, beside that expression; a clause about any other member outside its
/// own expression states no value at all.
///
/// The expression each clause must quote is also what pins this service's registry-name
/// discriminator to the MODEL: every row drives the generated schema of one of the four
/// registry name types over the wire, so a model that re-spells that pattern, or a change in
/// how a `@pattern` is requoted, fails here instead of silently reclassifying the class.
#[tokio::test]
async fn a_pattern_clause_states_the_rejected_registry_name_and_no_other_value() {
    for case in pattern_refusals() {
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
        let expected = format!("1 validation error detected: {}", case.clause);
        assert_eq!(
            reply.message(),
            expected,
            "case {:?}: expected message {expected:?}, got {:?}",
            case.case,
            reply.message()
        );
        let quoted = format!("Value '{}'", case.rejected);
        assert_eq!(
            reply.message().contains(&quoted),
            case.quotes_the_value,
            "case {:?}: expected the clause to quote the submitted value: {}, got {:?}",
            case.case,
            case.quotes_the_value,
            reply.message()
        );
    }
}

/// The other side of the same decision: a registry name the expression ADMITS draws no
/// constraint clause at all — the thing is created and read back, and the thing group too —
/// so the refusals above are about the names the requests sent and not about the operations.
#[tokio::test]
async fn a_registry_name_within_the_expression_is_created_and_read_back() {
    // Names the expression admits, spanning the characters it does admit: a colon, an
    // underscore, a hyphen and digits.
    for name in ["probe-thing_1", "probe:thing-2", "PROBE3"] {
        let emulator = Emulator::new();
        let created = emulator
            .call("POST", &format!("/things/{name}"), Some(&json!({})))
            .await;
        assert_eq!(
            created.status, 200,
            "case {name:?}: expected the create to be accepted, got {} {:?}",
            created.status, created.body
        );
        let read = emulator.call("GET", &format!("/things/{name}"), None).await;
        assert_eq!(
            read.status, 200,
            "case {name:?}: expected the thing to be readable, got {} {:?}",
            read.status, read.body
        );
        assert_eq!(
            read.body.get("thingName").and_then(Value::as_str),
            Some(name),
            "case {name:?}: expected the name to be read back, got {:?}",
            read.body
        );

        let group = emulator
            .call("POST", &format!("/thing-groups/{name}"), Some(&json!({})))
            .await;
        assert_eq!(
            group.status, 200,
            "case {name:?}: expected the group create to be accepted, got {} {:?}",
            group.status, group.body
        );
        let group_read = emulator
            .call("GET", &format!("/thing-groups/{name}"), None)
            .await;
        assert_eq!(
            group_read.body.get("thingGroupName").and_then(Value::as_str),
            Some(name),
            "case {name:?}: expected the group name to be read back, got {:?}",
            group_read.body
        );
    }
}
