// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the STATUS LINE a `CreateStreamingURL` refusal leaves with.
//!
//! Every case drives the awsJson1_1 front door through the shared
//! [`Emulator`](crate::stack_fleet_lifecycle::Emulator) harness — `POST /` with
//! `X-Amz-Target: PhotonAdminProxyService.CreateStreamingURL` — so what is asserted is the
//! status line, the wire code and the message a client actually parses.
//!
//! The rules under test, each asserted over a TABLE of inputs rather than over one
//! recorded request:
//!
//! * a refusal this service serves leaves over HTTP 400, whatever status the refused
//!   shape's own `@httpError` trait states: a streaming URL asked for against a stack or a
//!   fleet the request cannot reach is refused `ResourceNotFoundException` with AWS's
//!   sentence for that class at HTTP 400, and neither the status nor the code varies with
//!   which side of the request is unreachable, what form the name takes, or what the
//!   optional members carry;
//! * the status does not vary with the refusal CLASS either — a value outside the range
//!   `Validity` documents and a fleet that is not streaming are refused over the same
//!   status, each with its own code — while an operation nobody has written yet still
//!   answers HTTP 501, because that is not a refusal of the request at all;
//! * the 400s above are this operation's refusal path and not a blanket refusal: on the
//!   same emulator a reachable identity on a streaming fleet is answered with a URL.
//!
//! `ResourceNotFoundException` is the one error shape of this model whose `@httpError`
//! does not state the status AppStream serves its refusals over
//! (`smithy_query(query="shape", shape="ResourceNotFoundException")` reports 404, while
//! every other error shape of the service reports 400), and it is the shape this
//! operation's unreachable-resource refusal carries.

use serde_json::{json, Value};

use crate::refusal_messages::{not_found_sentence, refusal};
use crate::stack_fleet_lifecycle::{assert_error, Emulator};
use crate::streaming_url::{
    sessions_of, stack_with_fleet, start_and_await_running, streaming_request,
};

/// The stack and the fleet every case below can reach: a stack with one fleet associated
/// with it, so a row can make exactly one side of the request the unreachable one.
const STACK: &str = "reachable-stack";
const FLEET: &str = "reachable-fleet";

/// The user a streaming URL is asked for (`UserId`, len 2..32, pattern `^[\w+=,.@-]*$`).
const USER: &str = "stream-user1";

/// The status this service serves a refused request over.
const REFUSAL_STATUS: u16 = 400;

/// The status an operation nobody has written yet answers with, which is not a refusal of
/// the request.
const SCAFFOLD_STATUS: u16 = 501;

/// The wire code a refusal for a resource this account holds nothing under carries, as the
/// crate states it beside the sentence such a refusal words.
const NOT_FOUND_CODE: &str =
    service_com_amazonaws_appstream_photonadminproxyservice::state::RESOURCE_NOT_FOUND_CODE;

/// The documented bounds of `Validity` ("Specify a value between 1 and 604800 seconds"),
/// read from the crate so these rows track the constants rather than a copy of them.
const VALIDITY_MIN: i64 =
    service_com_amazonaws_appstream_photonadminproxyservice::state::STREAMING_URL_MIN_VALIDITY_SECONDS;
const VALIDITY_MAX: i64 =
    service_com_amazonaws_appstream_photonadminproxyservice::state::STREAMING_URL_MAX_VALIDITY_SECONDS;

/// A `CreateStreamingURL` request naming `stack`, `fleet` and the user above, carrying
/// whatever optional members `extra` adds — the request shape the sibling suite states
/// (`tests/all/streaming_url.rs`), for this suite's one user.
fn request(stack: &str, fleet: &str, extra: &Value) -> Value {
    streaming_request(stack, fleet, USER, extra)
}

/// Name forms a create admits, spanning length, character classes and case. The last is
/// the longest name a create takes — `Name` matches `^[a-zA-Z0-9][a-zA-Z0-9_.-]{0,100}$`
/// (`smithy_query(query="operation", operation="CreateFleet")`), so 101 characters.
fn creatable_names() -> Vec<String> {
    vec![
        "absent".to_string(),
        "Mixed.Case_name-9".to_string(),
        "0".to_string(),
        format!("z{}", "a".repeat(100)),
    ]
}

/// Every name form the request may carry: the ones a create admits, plus forms only a
/// request can name — `StackName` and `FleetName` are `@length(min: 1)` and carry no
/// pattern (`smithy_query(query="operation", operation="CreateStreamingURL")`), so a name
/// no resource could be created under still has to be answered as unreachable rather than
/// refused some other way.
fn request_names() -> Vec<String> {
    let mut names = creatable_names();
    names.push("a name with spaces".to_string());
    names.push("*".to_string());
    names
}

/// What the optional members of the request carry. Each is admissible on its own, so the
/// gate that bites is the one under test: the validities are the published bounds, which
/// the range gate lets through.
fn request_shapes() -> Vec<(&'static str, Value)> {
    vec![
        ("the required members only", json!({})),
        ("an application to launch", json!({ "ApplicationId": "Desktop" })),
        ("a session context", json!({ "SessionContext": "ctx-1" })),
        (
            "the shortest validity allowed",
            json!({ "Validity": VALIDITY_MIN }),
        ),
        (
            "every optional member, at the longest validity allowed",
            json!({
                "ApplicationId": "Desktop",
                "SessionContext": "ctx-1",
                "Validity": VALIDITY_MAX,
            }),
        ),
    ]
}

/// One unreachable-resource row: which side of the request names something it cannot
/// reach, whether that name has to be stored first, and the resource kind and name the
/// refusal's sentence carries.
struct UnreachableRow {
    case: &'static str,
    /// The `(StackName, FleetName)` the request names, for the row's name.
    identity: fn(&str) -> (String, String),
    /// The resource kind the refusal names, and the name its sentence carries.
    refused: fn(&str) -> (&'static str, String),
    /// Whether the row needs a fleet stored under its name — a fleet that exists and is
    /// still not a fleet of the stack the request names.
    stores_a_fleet: bool,
}

/// Every way a `CreateStreamingURL` can name a resource it cannot reach: either side
/// absent, both absent, and a fleet that exists but is not associated with the stack.
fn unreachable_rows() -> Vec<UnreachableRow> {
    vec![
        UnreachableRow {
            case: "a stack nothing was created under",
            identity: |name| (name.to_string(), FLEET.to_string()),
            refused: |name| ("stack", name.to_string()),
            stores_a_fleet: false,
        },
        UnreachableRow {
            case: "a fleet nothing was created under",
            identity: |name| (STACK.to_string(), name.to_string()),
            refused: |name| ("fleet", name.to_string()),
            stores_a_fleet: false,
        },
        UnreachableRow {
            // The stack is resolved first, so the stack is what the sentence names.
            case: "neither side was created",
            identity: |name| (name.to_string(), format!("{name}-fleet")),
            refused: |name| ("stack", name.to_string()),
            stores_a_fleet: false,
        },
        UnreachableRow {
            case: "a fleet that exists but is not a fleet of that stack",
            identity: |name| (STACK.to_string(), name.to_string()),
            refused: |name| ("fleet", name.to_string()),
            stores_a_fleet: true,
        },
    ]
}

/// The sessions a `DescribeSessions` reports for the stack and fleet above.
async fn sessions(emulator: &Emulator, case: &str) -> Vec<Value> {
    sessions_of(emulator, case, STACK, FLEET).await
}

#[tokio::test]
async fn an_unreachable_stack_or_fleet_is_refused_over_the_services_refusal_status() {
    for row in unreachable_rows() {
        for name in request_names() {
            if row.stores_a_fleet && !creatable_names().contains(&name) {
                // A fleet can only be stored under a name a create admits; the absent
                // rows above carry the other name forms.
                continue;
            }

            let setup = format!("{} named {name:?}", row.case);
            let emulator = Emulator::new();
            stack_with_fleet(&emulator, &setup, STACK, FLEET).await;
            if row.stores_a_fleet {
                emulator
                    .ok(
                        &setup,
                        "CreateFleet",
                        json!({ "Name": name, "InstanceType": "stream.standard.small" }),
                    )
                    .await;
            }

            let (stack, fleet) = (row.identity)(&name);
            let (kind, refused) = (row.refused)(&name);
            for (shape, extra) in request_shapes() {
                let case = format!("{} named {name:?}, asked for with {shape}", row.case);
                let message = refusal(
                    &emulator,
                    &case,
                    "CreateStreamingURL",
                    request(&stack, &fleet, &extra),
                    REFUSAL_STATUS,
                    NOT_FOUND_CODE,
                )
                .await;

                let expected = not_found_sentence(kind, &refused);
                assert_eq!(
                    message, expected,
                    "case {case:?}: expected message {expected:?}, got {message:?}"
                );
            }

            // Nothing was issued, so the refusals dedicated no session on the fleet the
            // request could reach.
            let sessions = sessions(&emulator, &setup).await;
            assert!(
                sessions.is_empty(),
                "case {setup:?}: a refused call must dedicate no session, got {sessions:?}"
            );
        }
    }
}

#[tokio::test]
async fn the_refusal_status_does_not_vary_with_the_refusal_class() {
    let case = "setup";
    let emulator = Emulator::new();
    stack_with_fleet(&emulator, case, STACK, FLEET).await;

    // Rows: the refusal classes this operation answers, each with the code its own class
    // carries. The created fleet is not streaming yet, which is what makes the
    // state-gate row refusable without starting it.
    let rows: Vec<(&str, Value, u16, &str)> = vec![
        (
            "a stack the request cannot reach",
            request("absent-stack", FLEET, &json!({})),
            REFUSAL_STATUS,
            NOT_FOUND_CODE,
        ),
        (
            "a fleet that is not streaming",
            request(STACK, FLEET, &json!({})),
            REFUSAL_STATUS,
            "OperationNotPermittedException",
        ),
        (
            "a validity one past the shortest allowed",
            request(STACK, FLEET, &json!({ "Validity": VALIDITY_MIN - 1 })),
            REFUSAL_STATUS,
            "ValidationException",
        ),
        (
            "a validity one past the longest allowed",
            request(STACK, FLEET, &json!({ "Validity": VALIDITY_MAX + 1 })),
            REFUSAL_STATUS,
            "ValidationException",
        ),
        (
            "a user id outside the member's pattern",
            json!({ "StackName": STACK, "FleetName": FLEET, "UserId": "no spaces allowed" }),
            REFUSAL_STATUS,
            "ValidationException",
        ),
    ];

    for (case, request, status, code) in rows {
        assert_error(&emulator, case, "CreateStreamingURL", request, status, code).await;
    }

    // A streaming URL for a builder nobody has written the operation for is not a refusal
    // of the request: the status mapping restates what a modelled REFUSAL leaves with and
    // leaves the scaffold channel alone.
    assert_error(
        &emulator,
        "a streaming URL for an image builder, which is still a scaffold",
        "CreateImageBuilderStreamingURL",
        json!({ "Name": "absent-image-builder" }),
        SCAFFOLD_STATUS,
        "NotImplemented",
    )
    .await;
}

#[tokio::test]
async fn a_reachable_identity_on_a_streaming_fleet_is_still_answered_with_a_url() {
    let case = "setup";
    let emulator = Emulator::new();
    stack_with_fleet(&emulator, case, STACK, FLEET).await;
    start_and_await_running(&emulator, case, FLEET).await;

    let payload = emulator
        .ok(
            "a reachable stack and fleet",
            "CreateStreamingURL",
            request(STACK, FLEET, &json!({})),
        )
        .await;
    let url = payload
        .get("StreamingURL")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case \"a reachable stack and fleet\": expected a StreamingURL, got {payload}")
        });
    assert!(
        !url.is_empty(),
        "case \"a reachable stack and fleet\": the issued URL must not be empty, got {payload}"
    );

    // The same emulator, one unreachable name later: the refusal above is this
    // operation's not-found path rather than a blanket refusal.
    let message = refusal(
        &emulator,
        "a stack the request cannot reach, on an emulator that just issued a URL",
        "CreateStreamingURL",
        request("absent-stack", FLEET, &json!({})),
        REFUSAL_STATUS,
        NOT_FOUND_CODE,
    )
    .await;
    let expected = not_found_sentence("stack", "absent-stack");
    assert_eq!(
        message, expected,
        "case \"a stack the request cannot reach, on an emulator that just issued a URL\": \
         expected message {expected:?}, got {message:?}"
    );
}
