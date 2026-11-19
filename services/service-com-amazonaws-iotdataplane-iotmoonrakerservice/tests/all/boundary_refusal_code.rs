// SPDX-License-Identifier: CC0-1.0

//! How this service states a request the framework refused at the boundary — a value
//! outside a constraint the model declares, or a required member that is absent — end to
//! end over HTTP through the router this service mounts in production.
//!
//! The rules these cases assert:
//!
//! * A refusal carries an error code this service MODELS, under the status the model binds
//!   to that code. `ValidationException` is not a shape this service returns at all, so it
//!   must appear in no refusal: a client's `except InvalidRequestException`, and an SDK's
//!   modelled-error dispatch, would never match it.
//! * The NAMES this plane addresses a resource by are its own identity rules, each stated in
//!   its own fixed sentence:
//!   - a `clientId` outside what the model admits — too long, or in the reserved `$`
//!     namespace — is `InvalidRequestException` (HTTP 400) with the one client-id sentence,
//!     which states both rules whichever of them the value broke;
//!   - a `thingName` outside what the model admits names no thing this plane can resolve, so
//!     every shadow operation the model gives a not-found error reports it as the ABSENT
//!     resource the request addressed: `ResourceNotFoundException` (HTTP 404) with the one
//!     thing-name sentence. `UpdateThingShadow` declares no not-found error
//!     (`smithy_query(query="digest")`), so for it such a name stays a bad request.
//! * Every other front-door refusal — a `shadowName`, a `qos`, a `maxResults`, a `pageSize`,
//!   an absent member — is this data plane's one terse sentence, the same for every member of
//!   every operation.
//! * No refusal carries anything of the request: not the member at fault, not the bound it
//!   crossed, and not the value the caller sent. Every one of them carries the `traceId`
//!   member beside `message`. That absence is asserted on the members the service COMPOSED,
//!   not on the whole body: `traceId` is minted per call out of 32 hexadecimal characters,
//!   and every decimal digit is one, so a short value looked for in the whole body is also
//!   being looked for in an id that is different on every call.
//!
//! The table spans what the rules generalise over — which operation, which member, how the
//! member is bound (path label, query parameter), and which kind of constraint (length,
//! pattern, range, absence) — with the accepted row at each published bound beside the refused
//! one past it. The recorded cases that filed the issues (a 65-character `shadowName`, a
//! `$`-prefixed `clientId` on each connection operation, and a `thingName` outside its
//! pattern on `GetThingShadow`) are rows among them.

use crate::shadow_and_retained_message::{Answer, Harness};
use serde_json::{json, Value};

/// This plane's client-input error, and the one it reports a resource it cannot address
/// with: the two classes a front-door refusal of this service carries.
const INVALID_REQUEST: &str = "InvalidRequestException";
const NOT_FOUND: &str = "ResourceNotFoundException";

/// The sentences this front door states, written out here rather than taken from the crate,
/// so the wording is pinned independently of the renderer under test.
const BAD_REQUEST: &str = "Bad Request";
const CLIENT_ID_NOT_VALID: &str = "This client ID isn't valid. Client IDs must not exceed 128 \
     characters and can't start with a dollar sign ($). Enter a different client ID";
const INVALID_THING_NAME: &str = "Invalid thing name specified in request";

/// A valid shadow state document, so no row is refused for its body.
fn state_document() -> Vec<u8> {
    serde_json::to_vec(&json!({"state": {"reported": {"ok": true}}})).expect("serializable")
}

/// The `message` of a response body.
fn message(answer: &Answer) -> String {
    answer
        .json()
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The `traceId` of a response body.
fn trace_id(answer: &Answer) -> String {
    answer
        .json()
        .get("traceId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Whether `value` has the 8-4-4-4-12 hexadecimal shape a `traceId` carries.
fn is_uuid_shaped(value: &str) -> bool {
    let widths = [8usize, 4, 4, 4, 12];
    let groups: Vec<&str> = value.split('-').collect();
    groups.len() == widths.len()
        && groups.iter().zip(widths).all(|(group, width)| {
            group.len() == width && group.chars().all(|c| c.is_ascii_hexdigit())
        })
}

/// One request that breaks a constraint the model declares.
struct RefusedCase {
    name: &'static str,
    method: &'static str,
    uri: String,
    body: Vec<u8>,
    /// The status and the error code this plane answers that class of refusal with.
    status: u16,
    code: &'static str,
    /// The whole sentence the body carries.
    expected: &'static str,
    /// The member at fault, which the refusal must NOT name — these sentences reflect
    /// nothing of the request back.
    member: &'static str,
    /// The value the request carried for that member, which the refusal must not echo
    /// either; `None` for a row whose value is too short to tell from ordinary text.
    value: Option<String>,
}

/// One request that is INSIDE every constraint, at the bound the refused row crosses, and
/// what answering it proves the front door let through.
struct AcceptedCase {
    name: &'static str,
    method: &'static str,
    uri: String,
    body: Vec<u8>,
    /// The status the HANDLER answers such a request with: 200 where it can be served, and
    /// the handler's own not-found where the named resource does not exist, which is still
    /// proof the name itself was addressed.
    status: u16,
    /// The sentence the handler's own answer carries, where it is a failure. A row that is
    /// served carries no sentence at all.
    expected: Option<String>,
}

/// A client id 129 characters long: one past the modelled `ClientId` maximum
/// (`smithy_query(query="shape", shape="ClientId")` — `length 1..128`).
fn over_long_client_id() -> String {
    "c".repeat(129)
}

/// A thing name 129 characters long: one past the modelled `ThingName` maximum.
fn over_long_thing_name() -> String {
    "t".repeat(129)
}

fn refused_cases() -> Vec<RefusedCase> {
    let mut cases = vec![
        RefusedCase {
            // The recorded case: one character past `shadowName`'s modelled maximum.
            name: "a shadowName one character past its modelled maximum",
            method: "POST",
            uri: format!("/things/refusalThing/shadow?name={}", "s".repeat(65)),
            body: state_document(),
            status: 400,
            code: INVALID_REQUEST,
            expected: BAD_REQUEST,
            member: "shadowName",
            value: Some("s".repeat(65)),
        },
        RefusedCase {
            name: "a shadowName far past its modelled maximum",
            method: "POST",
            uri: format!("/things/refusalThing/shadow?name={}", "s".repeat(400)),
            body: state_document(),
            status: 400,
            code: INVALID_REQUEST,
            expected: BAD_REQUEST,
            member: "shadowName",
            value: Some("s".repeat(400)),
        },
        RefusedCase {
            name: "a shadowName outside its modelled pattern",
            method: "POST",
            uri: "/things/refusalThing/shadow?name=not%20a%20shadow%21".to_string(),
            body: state_document(),
            status: 400,
            code: INVALID_REQUEST,
            expected: BAD_REQUEST,
            member: "shadowName",
            value: Some("not a shadow!".to_string()),
        },
        RefusedCase {
            // The recorded case of the read path: a shadowName outside the pattern.
            name: "a shadowName on a READ, outside its modelled pattern",
            method: "GET",
            uri: "/things/refusalThing/shadow?name=bad%21shadow".to_string(),
            body: Vec::new(),
            status: 400,
            code: INVALID_REQUEST,
            expected: BAD_REQUEST,
            member: "shadowName",
            value: Some("bad!shadow".to_string()),
        },
        RefusedCase {
            name: "a shadowName on a DELETE, outside its modelled pattern",
            method: "DELETE",
            uri: "/things/refusalThing/shadow?name=bad%21shadow".to_string(),
            body: Vec::new(),
            status: 400,
            code: INVALID_REQUEST,
            expected: BAD_REQUEST,
            member: "shadowName",
            value: Some("bad!shadow".to_string()),
        },
        RefusedCase {
            // `UpdateThingShadow` models no not-found error, so a thing name it cannot
            // resolve is a bad request rather than an absent resource.
            name: "a thingName one character past its modelled maximum, on the write path",
            method: "POST",
            uri: format!("/things/{}/shadow", over_long_thing_name()),
            body: state_document(),
            status: 400,
            code: INVALID_REQUEST,
            expected: BAD_REQUEST,
            member: "thingName",
            value: Some(over_long_thing_name()),
        },
        RefusedCase {
            name: "a thingName outside its modelled pattern, on the write path",
            method: "POST",
            uri: "/things/bad%21thing/shadow".to_string(),
            body: state_document(),
            status: 400,
            code: INVALID_REQUEST,
            expected: BAD_REQUEST,
            member: "thingName",
            value: Some("bad!thing".to_string()),
        },
        RefusedCase {
            name: "a qos above its modelled range",
            method: "POST",
            uri: "/topics/refusal-topic?qos=2&retain=false".to_string(),
            body: b"x".to_vec(),
            status: 400,
            code: INVALID_REQUEST,
            expected: BAD_REQUEST,
            member: "qos",
            // A one-character value is not distinguishable from ordinary text, so only
            // the member name is asserted absent for this row.
            value: None,
        },
        RefusedCase {
            name: "a qos below its modelled range",
            method: "POST",
            uri: "/topics/refusal-topic?qos=-1&retain=false".to_string(),
            body: b"x".to_vec(),
            status: 400,
            code: INVALID_REQUEST,
            expected: BAD_REQUEST,
            member: "qos",
            value: None,
        },
        RefusedCase {
            name: "a maxResults above its modelled range",
            method: "GET",
            uri: "/retainedMessage?maxResults=201".to_string(),
            body: Vec::new(),
            status: 400,
            code: INVALID_REQUEST,
            expected: BAD_REQUEST,
            member: "maxResults",
            value: Some("201".to_string()),
        },
        RefusedCase {
            name: "a pageSize above its modelled range",
            method: "GET",
            uri: "/api/things/shadow/ListNamedShadowsForThing/refusalThing?pageSize=101"
                .to_string(),
            body: Vec::new(),
            status: 400,
            code: INVALID_REQUEST,
            expected: BAD_REQUEST,
            member: "pageSize",
            value: Some("101".to_string()),
        },
        RefusedCase {
            name: "a pageSize below its modelled range",
            method: "GET",
            uri: "/api/things/shadow/ListNamedShadowsForThing/refusalThing?pageSize=0".to_string(),
            body: Vec::new(),
            status: 400,
            code: INVALID_REQUEST,
            expected: BAD_REQUEST,
            member: "pageSize",
            value: None,
        },
        RefusedCase {
            // The boundary refusal is decided before the handler, so this answers 400
            // and not the 404 an unknown client id would otherwise get.
            name: "a maxResults above its modelled range on a client id with no session",
            method: "GET",
            uri: "/connections/refusal-client/subscriptions?maxResults=201".to_string(),
            body: Vec::new(),
            status: 400,
            code: INVALID_REQUEST,
            expected: BAD_REQUEST,
            member: "maxResults",
            value: Some("201".to_string()),
        },
        RefusedCase {
            // Two members at fault at once: no ONE name's rules were broken, so the terse
            // front-door sentence is what states it.
            name: "a reserved clientId beside a maxResults above its range",
            method: "GET",
            uri: "/connections/%24reserved-client/subscriptions?maxResults=201".to_string(),
            body: Vec::new(),
            status: 400,
            code: INVALID_REQUEST,
            expected: BAD_REQUEST,
            member: "maxResults",
            value: Some("$reserved-client".to_string()),
        },
    ];

    // The client-id rows: every connection operation, and both rules a client id has to
    // satisfy. Each is answered the one client-id sentence.
    for (name, method, uri, value) in [
        (
            "a reserved clientId on GetConnection",
            "GET",
            "/connections/%24reserved-client".to_string(),
            "$reserved-client".to_string(),
        ),
        (
            "a reserved clientId on ListSubscriptions",
            "GET",
            "/connections/%24reserved-client/subscriptions".to_string(),
            "$reserved-client".to_string(),
        ),
        (
            "a reserved clientId on SendDirectMessage",
            "POST",
            "/connections/%24reserved-client/messages?topic=refusal-topic".to_string(),
            "$reserved-client".to_string(),
        ),
        (
            "a reserved clientId on DeleteConnection",
            "DELETE",
            "/connections/%24reserved-client".to_string(),
            "$reserved-client".to_string(),
        ),
        (
            "a clientId one character past its modelled maximum",
            "GET",
            format!("/connections/{}", over_long_client_id()),
            over_long_client_id(),
        ),
        (
            "a clientId far past its modelled maximum",
            "DELETE",
            format!("/connections/{}", "c".repeat(600)),
            "c".repeat(600),
        ),
        (
            "a clientId past its maximum on ListSubscriptions",
            "GET",
            format!("/connections/{}/subscriptions", over_long_client_id()),
            over_long_client_id(),
        ),
    ] {
        cases.push(RefusedCase {
            name,
            method,
            uri,
            body: if method == "POST" {
                b"x".to_vec()
            } else {
                Vec::new()
            },
            status: 400,
            code: INVALID_REQUEST,
            expected: CLIENT_ID_NOT_VALID,
            member: "clientId",
            value: Some(value),
        });
    }

    // The thing-name rows: every shadow operation the model gives a not-found error, and
    // both rules a thing name has to satisfy. Each reports the thing as absent.
    for (name, method, uri, value) in [
        (
            "a thingName outside its modelled pattern on GetThingShadow",
            "GET",
            "/things/bad%21thing/shadow".to_string(),
            "bad!thing".to_string(),
        ),
        (
            "a thingName with a path-legal character outside its pattern",
            "GET",
            "/things/bad.thing/shadow".to_string(),
            "bad.thing".to_string(),
        ),
        (
            "a thingName past its modelled maximum on GetThingShadow",
            "GET",
            format!("/things/{}/shadow", over_long_thing_name()),
            over_long_thing_name(),
        ),
        (
            "a thingName outside its modelled pattern on DeleteThingShadow",
            "DELETE",
            "/things/bad%21thing/shadow".to_string(),
            "bad!thing".to_string(),
        ),
        (
            "a thingName outside its modelled pattern on ListNamedShadowsForThing",
            "GET",
            "/api/things/shadow/ListNamedShadowsForThing/bad%21thing".to_string(),
            "bad!thing".to_string(),
        ),
        (
            "a thingName past its maximum on ListNamedShadowsForThing",
            "GET",
            format!(
                "/api/things/shadow/ListNamedShadowsForThing/{}",
                over_long_thing_name()
            ),
            over_long_thing_name(),
        ),
    ] {
        cases.push(RefusedCase {
            name,
            method,
            uri,
            body: Vec::new(),
            status: 404,
            code: NOT_FOUND,
            expected: INVALID_THING_NAME,
            member: "thingName",
            value: Some(value),
        });
    }

    cases
}

fn accepted_cases() -> Vec<AcceptedCase> {
    vec![
        AcceptedCase {
            name: "a shadowName exactly at its modelled maximum",
            method: "POST",
            uri: format!("/things/refusalThing/shadow?name={}", "s".repeat(64)),
            body: state_document(),
            status: 200,
            expected: None,
        },
        AcceptedCase {
            name: "a thingName exactly at its modelled maximum",
            method: "POST",
            uri: format!("/things/{}/shadow", "t".repeat(128)),
            body: state_document(),
            status: 200,
            expected: None,
        },
        AcceptedCase {
            name: "a qos at the top of its modelled range",
            method: "POST",
            uri: "/topics/refusal-topic?qos=1&retain=false".to_string(),
            body: b"x".to_vec(),
            status: 200,
            expected: None,
        },
        AcceptedCase {
            name: "a maxResults at the top of its modelled range",
            method: "GET",
            uri: "/retainedMessage?maxResults=200".to_string(),
            body: Vec::new(),
            status: 200,
            expected: None,
        },
        AcceptedCase {
            name: "a pageSize at the top of its modelled range",
            method: "GET",
            uri: "/api/things/shadow/ListNamedShadowsForThing/refusalThing?pageSize=100"
                .to_string(),
            body: Vec::new(),
            status: 200,
            expected: None,
        },
        AcceptedCase {
            // A thing name INSIDE the modelled bounds that no shadow was ever written for
            // is a different 404 from a name this plane cannot resolve: the handler's own
            // not-found names the shadow it looked for.
            name: "a thingName inside its bounds whose shadow was never written",
            method: "GET",
            uri: "/things/absentThing/shadow".to_string(),
            body: Vec::new(),
            status: 404,
            expected: Some("No shadow exists with name: 'absentThing'".to_string()),
        },
        AcceptedCase {
            name: "a thingName at its modelled maximum whose shadow was never written",
            method: "GET",
            uri: format!("/things/{}/shadow", "u".repeat(128)),
            body: Vec::new(),
            status: 404,
            expected: Some(format!("No shadow exists with name: '{}'", "u".repeat(128))),
        },
        AcceptedCase {
            // A client id inside the modelled bounds reaches the handler, which reports the
            // session it holds none of — proof the name itself was addressed.
            name: "a clientId exactly at its modelled maximum",
            method: "GET",
            uri: format!("/connections/{}", "c".repeat(128)),
            body: Vec::new(),
            status: 404,
            expected: Some(
                "The client ID is not found. Verify that the client is connected and try again"
                    .to_string(),
            ),
        },
        AcceptedCase {
            // The modelled pattern bars a LEADING `$` only, so one anywhere else is an
            // ordinary client id.
            name: "a clientId carrying a dollar sign that does not lead it",
            method: "GET",
            uri: "/connections/fleet%24client".to_string(),
            body: Vec::new(),
            status: 404,
            expected: Some(
                "The client ID is not found. Verify that the client is connected and try again"
                    .to_string(),
            ),
        },
        AcceptedCase {
            name: "a clientId of one character, at the bottom of its modelled length",
            method: "DELETE",
            uri: "/connections/q".to_string(),
            body: Vec::new(),
            status: 404,
            expected: Some(
                "The client ID is not found. Verify that the client is connected and try again"
                    .to_string(),
            ),
        },
    ]
}

#[tokio::test]
async fn a_constraint_refusal_carries_this_services_own_error_code() {
    let harness = Harness::new();
    // A thing with a shadow, so the accepted rows that read one are not 404s for a
    // reason that has nothing to do with the constraint.
    harness.update("refusalThing", None, state_document()).await;

    let mut trace_ids: Vec<String> = Vec::new();
    for case in refused_cases() {
        let answer = harness
            .send(case.method, &case.uri, &[], case.body.clone())
            .await;
        assert_eq!(
            answer.status, case.status,
            "case {:?}: expected {}, got {} with body {}",
            case.name,
            case.status,
            answer.status,
            String::from_utf8_lossy(&answer.body)
        );
        assert_eq!(
            answer.error_code(),
            case.code,
            "case {:?}: expected the error class {:?}, got {}",
            case.name,
            case.code,
            String::from_utf8_lossy(&answer.body)
        );
        // The code a restJson1 client resolves the error SHAPE from must name the same
        // class as the body does.
        assert_eq!(
            answer.error_type.as_deref().unwrap_or_default(),
            case.code,
            "case {:?}: expected x-amzn-errortype {:?}, got {:?}",
            case.name,
            case.code,
            answer.error_type
        );
        // Asserted on the members the service composed, not on the whole body: the
        // body also carries the id this call minted, which no absence assertion may
        // read a value out of (see `Answer::body_without_trace_id`).
        assert!(
            !answer.body_without_trace_id().contains("ValidationException"),
            "case {:?}: ValidationException is not a shape this service returns, got {}",
            case.name,
            String::from_utf8_lossy(&answer.body)
        );
        let stated = message(&answer);
        assert_eq!(
            stated, case.expected,
            "case {:?}: expected this data plane's sentence {:?}, got {stated:?}",
            case.name, case.expected
        );
        assert!(
            !stated.contains(case.member),
            "case {:?}: the refusal must not name the member {:?}, got {stated:?}",
            case.name,
            case.member
        );
        if let Some(value) = &case.value {
            assert!(
                !answer.body_without_trace_id().contains(value.as_str()),
                "case {:?}: the refusal must not echo the value the request carried, got {}",
                case.name,
                answer.body_without_trace_id()
            );
        }
        // Every refusal body carries the correlation id this plane writes beside `message`,
        // and it identifies the REQUEST: no two of them repeat.
        let trace = trace_id(&answer);
        assert!(
            is_uuid_shaped(&trace),
            "case {:?}: expected a UUID-shaped traceId on the refusal, got {trace:?} in body {}",
            case.name,
            String::from_utf8_lossy(&answer.body)
        );
        assert!(
            !trace_ids.contains(&trace),
            "case {:?}: expected a fresh traceId per request, got {trace:?} again",
            case.name
        );
        trace_ids.push(trace);
    }
}

#[tokio::test]
async fn a_request_at_each_modelled_bound_is_not_refused() {
    let harness = Harness::new();
    harness.update("refusalThing", None, state_document()).await;

    for case in accepted_cases() {
        let answer = harness
            .send(case.method, &case.uri, &[], case.body.clone())
            .await;
        assert_eq!(
            answer.status, case.status,
            "case {:?}: expected the bound itself to reach the handler ({}), got {} with body {}",
            case.name,
            case.status,
            answer.status,
            String::from_utf8_lossy(&answer.body)
        );
        if let Some(expected) = &case.expected {
            // The handler's own answer for the named resource, which is what proves the
            // front door addressed the name rather than refusing it.
            assert_eq!(
                &message(&answer),
                expected,
                "case {:?}: expected the handler's own sentence {expected:?}, got {:?}",
                case.name,
                message(&answer)
            );
        }
    }
}
