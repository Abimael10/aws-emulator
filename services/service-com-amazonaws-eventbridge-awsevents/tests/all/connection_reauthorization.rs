// SPDX-License-Identifier: CC0-1.0

//! Re-authorizing a connection that holds no authorization — end to end over the
//! wire (`awsJson1_1`, `POST /` with `X-Amz-Target: AWSEvents.<Operation>`)
//! through the real `MultiServiceRouter`.
//!
//! The RULE asserted here: an `UpdateConnection` that supplies credentials which
//! are themselves the authorization settles the connection in the TERMINAL
//! `AUTHORIZED` state whatever state it was in when the update arrived — the
//! update's own envelope answers it, every later reader reports it, the
//! authorization is stamped at that instant, the reason the connection held none
//! is gone, and the store owes the connection no further state, so a caller
//! waiting for a terminal state is not waiting on a transient that never moves.
//! `ConnectionState` is modelled `ACTIVE | AUTHORIZED | AUTHORIZING | CREATING |
//! DEAUTHORIZED | DEAUTHORIZING | DELETING | FAILED_CONNECTIVITY | UPDATING`
//! (`smithy_query(query="operation",operation="UpdateConnection")`), in which
//! `UPDATING` and `AUTHORIZING` are the transients a connection passes THROUGH.
//! An update that supplies no authorization parameters decides nothing about the
//! authorization: the state, the reason and the stamp are left as they were.
//!
//! The table spans the class rather than the one recorded case: its rows differ
//! along the authorization type the connection was created with, along the type
//! the update supplies (the same one, re-stated with rotated credentials, or a
//! different one brought with its own credentials), along which parameter block
//! carries those credentials, and along whether the deauthorization has SETTLED
//! before the update arrives or is still a transition the clock owes. The
//! downstream reader is asserted too: an API destination invoking through the
//! connection is `INACTIVE` while it holds no authorization and `ACTIVE` again
//! once the update has re-authorized it.
//!
//! What a connection holds straight after the CREATE, and that reading a settled
//! connection moves nothing about it, belong to
//! `connection_authorization_is_settled_by_its_credentials`; this suite is about
//! the connection that held NO authorization when the update arrived.

use std::sync::Arc;
use std::time::SystemTime;

use serde_json::{json, Value};
use service_com_amazonaws_eventbridge_awsevents::state::{
    self, ComAmazonawsEventbridgeAwseventsState,
};
use service_com_amazonaws_eventbridge_awsevents::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region every request below is signed for.
const REGION: &str = "eu-west-1";

/// Why a deauthorized connection holds no authorization, as
/// `DeauthorizeConnection` states it ("Removes all authorization parameters from
/// the connection"): fixed text with no variable part, spelled out here rather
/// than imported, so a wrong wording in the implementation cannot make an
/// assertion pass.
const DEAUTHORIZED_REASON: &str = "The authorization parameters were removed from this connection \
                                   by a DeauthorizeConnection operation.";

/// The emulator: one router carrying only this service over one shared engine,
/// plus that engine, so a test can read the record the handlers wrote and bring a
/// transition the clock owes forward instead of waiting for it.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

fn emulator() -> Emulator {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    Emulator { router, engine }
}

/// One `awsJson1_1` call to EventBridge over the router.
async fn events(emulator: &Emulator, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("events.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AWSEvents.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTESTTESTTEST/20260101/{REGION}/events/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = emulator.router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice::<Value>(&payload).unwrap_or_else(|error| {
            panic!(
                "{operation} answered a body that is not JSON ({error}): {:?}",
                String::from_utf8_lossy(&payload)
            )
        })
    };
    (status, parsed)
}

/// Bring the transition the clock owes the connection `name` forward to this
/// instant, so the state it is heading for is observed on the next call without
/// waiting out the span. The emulator stores a DUE INSTANT rather than sleeping,
/// which is exactly what makes this possible.
fn make_the_owed_transition_due(emulator: &Emulator, case: &str, name: &str) {
    let state = emulator
        .engine
        .state::<ComAmazonawsEventbridgeAwseventsState>();
    let mut store = state.lock();
    let record = store
        .connection_mut(name)
        .unwrap_or_else(|| panic!("case {case:?}: the store holds the connection {name:?}"));
    let pending = record
        .pending
        .as_mut()
        .unwrap_or_else(|| panic!("case {case:?}: the connection {name:?} is owed a transition"));
    pending.due = SystemTime::now();
}

/// Whether the store still owes the connection `name` a later state: read off the
/// record the handlers wrote, so a TERMINAL state is asserted rather than waited
/// for.
fn still_owes_a_transition(emulator: &Emulator, case: &str, name: &str) -> bool {
    let state = emulator
        .engine
        .state::<ComAmazonawsEventbridgeAwseventsState>();
    let store = state.lock();
    store
        .connection(name)
        .unwrap_or_else(|| panic!("case {case:?}: the store holds the connection {name:?}"))
        .pending
        .is_some()
}

/// The entry `ListConnections` carries for the connection called `name`.
async fn listed(emulator: &Emulator, case: &str, name: &str) -> Value {
    let (status, listing) = events(emulator, "ListConnections", &json!({})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: ListConnections answered {status} {listing}"
    );
    listing["Connections"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: no Connections in {listing}"))
        .iter()
        .find(|entry| entry["Name"] == json!(name))
        .unwrap_or_else(|| panic!("case {case:?}: no entry called {name:?} in {listing}"))
        .clone()
}

/// The `ApiDestinationState` every reader of the destination `name` reports.
async fn destination_state(emulator: &Emulator, case: &str, name: &str) -> Value {
    let (status, described) =
        events(emulator, "DescribeApiDestination", &json!({ "Name": name })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeApiDestination answered {status} {described}"
    );
    described["ApiDestinationState"].clone()
}

/// One row of the re-authorization table.
struct Row {
    case: &'static str,
    /// The `AuthorizationType` the connection is created with, and the
    /// `AuthParameters` block carrying its credentials.
    created_as: &'static str,
    created_with: Value,
    /// Whether the deauthorization has SETTLED before the update arrives (the
    /// update sees `DEAUTHORIZED`) or is still a transition the clock owes it
    /// (the update sees `DEAUTHORIZING`).
    settled_before_the_update: bool,
    /// The `AuthorizationType` the update names, and the block carrying the
    /// credentials it supplies.
    updated_as: &'static str,
    updated_with: Value,
    /// The member of `ConnectionAuthResponseParameters` a read must report the
    /// non-secret half of those credentials in, and that half itself.
    reported_member: &'static str,
    reported_half: Value,
}

/// The rows. Every `AuthorizationType` whose credentials ARE the authorization is
/// covered — `API_KEY` and `BASIC`, as
/// `smithy_query(query="operation",operation="UpdateConnection")` names them
/// (`API_KEY | BASIC | OAUTH_CLIENT_CREDENTIALS`); the third obtains a token from
/// an authorization endpoint first, which is `connection_oauth_grant`'s subject.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a rotated api key, while the deauthorization is still in flight",
            created_as: "API_KEY",
            created_with: json!({
                "ApiKeyAuthParameters": { "ApiKeyName": "x-api-key", "ApiKeyValue": "first-key" },
            }),
            settled_before_the_update: false,
            updated_as: "API_KEY",
            updated_with: json!({
                "ApiKeyAuthParameters": {
                    "ApiKeyName": "x-rotated-key",
                    "ApiKeyValue": "second-key",
                },
            }),
            reported_member: "ApiKeyAuthParameters",
            reported_half: json!({ "ApiKeyName": "x-rotated-key" }),
        },
        Row {
            case: "a rotated api key, once the deauthorization has settled",
            created_as: "API_KEY",
            created_with: json!({
                "ApiKeyAuthParameters": { "ApiKeyName": "x-api-key", "ApiKeyValue": "first-key" },
            }),
            settled_before_the_update: true,
            updated_as: "API_KEY",
            updated_with: json!({
                "ApiKeyAuthParameters": {
                    "ApiKeyName": "x-rotated-key",
                    "ApiKeyValue": "second-key",
                },
            }),
            reported_member: "ApiKeyAuthParameters",
            reported_half: json!({ "ApiKeyName": "x-rotated-key" }),
        },
        Row {
            case: "rotated basic credentials, once the deauthorization has settled",
            created_as: "BASIC",
            created_with: json!({
                "BasicAuthParameters": { "Username": "first-user", "Password": "first-password" },
            }),
            settled_before_the_update: true,
            updated_as: "BASIC",
            updated_with: json!({
                "BasicAuthParameters": { "Username": "second-user", "Password": "second-password" },
            }),
            reported_member: "BasicAuthParameters",
            reported_half: json!({ "Username": "second-user" }),
        },
        Row {
            case: "a different authorization type, brought with its own credentials",
            created_as: "API_KEY",
            created_with: json!({
                "ApiKeyAuthParameters": { "ApiKeyName": "x-api-key", "ApiKeyValue": "first-key" },
            }),
            settled_before_the_update: true,
            updated_as: "BASIC",
            updated_with: json!({
                "BasicAuthParameters": { "Username": "the-user", "Password": "the-password" },
            }),
            reported_member: "BasicAuthParameters",
            reported_half: json!({ "Username": "the-user" }),
        },
        Row {
            case: "an api key replacing basic credentials while the deauthorization is in flight",
            created_as: "BASIC",
            created_with: json!({
                "BasicAuthParameters": { "Username": "the-user", "Password": "the-password" },
            }),
            settled_before_the_update: false,
            updated_as: "API_KEY",
            updated_with: json!({
                "ApiKeyAuthParameters": { "ApiKeyName": "x-new-key", "ApiKeyValue": "the-key" },
            }),
            reported_member: "ApiKeyAuthParameters",
            reported_half: json!({ "ApiKeyName": "x-new-key" }),
        },
    ]
}

/// The epoch-seconds reading a timestamp member carries, as a number.
fn at(case: &str, member: &str, body: &Value) -> f64 {
    body[member]
        .as_f64()
        .unwrap_or_else(|| panic!("case {case:?}: expected a {member} in {body}"))
}

/// An update supplying credentials that ARE the authorization re-authorizes the
/// connection, whatever state it held when the update arrived, and leaves it with
/// nothing further owed.
#[tokio::test]
async fn an_update_supplying_credentials_reauthorizes_the_connection() {
    for row in rows() {
        let Row {
            case,
            created_as,
            created_with,
            settled_before_the_update,
            updated_as,
            updated_with,
            reported_member,
            reported_half,
        } = row;
        let emulator = emulator();
        let name = "the-connection";
        let destination = "the-destination";

        let (status, created) = events(
            &emulator,
            "CreateConnection",
            &json!({
                "Name": name,
                "AuthorizationType": created_as,
                "AuthParameters": created_with,
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateConnection answered {status} {created}"
        );
        assert_eq!(
            created["ConnectionState"],
            json!(state::CONNECTION_STATE_AUTHORIZED),
            "case {case:?}: credentials that are the authorization settle the create, got {created}"
        );
        // `CreateConnectionResponse` models no `LastAuthorizedTime`
        // (`smithy_query(query="operation",operation="CreateConnection")`), so when
        // the connection was authorized is read off the connection itself.
        let (status, fresh) =
            events(&emulator, "DescribeConnection", &json!({ "Name": name })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeConnection answered {status} {fresh}"
        );
        let first_authorized = at(case, "LastAuthorizedTime", &fresh);
        let connection_arn = created["ConnectionArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no ConnectionArn in {created}"))
            .to_string();

        // The downstream reader of the connection's authorization: a destination
        // can be invoked only while the connection authorizes the invocation.
        let (status, made) = events(
            &emulator,
            "CreateApiDestination",
            &json!({
                "Name": destination,
                "ConnectionArn": connection_arn,
                "InvocationEndpoint": "https://destination.example.com/events",
                "HttpMethod": "POST",
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateApiDestination answered {status} {made}"
        );
        assert_eq!(
            destination_state(&emulator, case, destination).await,
            json!(state::API_DESTINATION_STATE_ACTIVE),
            "case {case:?}: a destination on an authorized connection is active"
        );

        // The authorization is taken away, and the connection is observed either
        // in the transient the deauthorize answers or in the state its clock
        // takes it to.
        let (status, deauthorized) =
            events(&emulator, "DeauthorizeConnection", &json!({ "Name": name })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DeauthorizeConnection answered {status} {deauthorized}"
        );
        let expected_before = if settled_before_the_update {
            make_the_owed_transition_due(&emulator, case, name);
            state::CONNECTION_STATE_DEAUTHORIZED
        } else {
            state::CONNECTION_STATE_DEAUTHORIZING
        };
        let (status, before) =
            events(&emulator, "DescribeConnection", &json!({ "Name": name })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeConnection answered {status} {before}"
        );
        assert_eq!(
            before["ConnectionState"],
            json!(expected_before),
            "case {case:?}: expected {expected_before} before the update, got {before}"
        );
        assert_eq!(
            before["StateReason"],
            json!(DEAUTHORIZED_REASON),
            "case {case:?}: a connection holding no authorization says why, got {before}"
        );
        // A destination takes its connection's authorization when that
        // connection's own transition settles under it, so the deauthorization
        // reaches the destination once it has settled and not while it is still
        // owed.
        let expected_destination = if settled_before_the_update {
            state::API_DESTINATION_STATE_INACTIVE
        } else {
            state::API_DESTINATION_STATE_ACTIVE
        };
        assert_eq!(
            destination_state(&emulator, case, destination).await,
            json!(expected_destination),
            "case {case:?}: expected a destination on this connection to be \
             {expected_destination} before the update"
        );

        // The update supplies credentials that ARE the authorization, so it
        // decides the authorization: a terminal state, stamped at the instant the
        // update accepted them.
        let (status, updated) = events(
            &emulator,
            "UpdateConnection",
            &json!({
                "Name": name,
                "AuthorizationType": updated_as,
                "AuthParameters": updated_with,
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: UpdateConnection answered {status} {updated}"
        );
        assert_eq!(
            updated["ConnectionState"],
            json!(state::CONNECTION_STATE_AUTHORIZED),
            "case {case:?}: expected the update to answer AUTHORIZED, got {updated}"
        );
        let reauthorized = at(case, "LastAuthorizedTime", &updated);
        assert_eq!(
            updated["LastAuthorizedTime"], updated["LastModifiedTime"],
            "case {case:?}: the update that accepted the credentials is when the connection was \
             last authorized, got {updated}"
        );
        assert!(
            reauthorized >= first_authorized,
            "case {case:?}: expected the authorization to be re-stamped at or after \
             {first_authorized}, got {reauthorized}"
        );
        assert!(
            !still_owes_a_transition(&emulator, case, name),
            "case {case:?}: an update that decided the authorization leaves no transition owed, \
             so a reader is never left on a transient"
        );

        // Every later reader reports that terminal state, the reason the
        // connection held none is gone, and the credentials the update supplied
        // are the ones the connection now holds.
        let (status, after) = events(&emulator, "DescribeConnection", &json!({ "Name": name })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeConnection answered {status} {after}"
        );
        assert_eq!(
            after["ConnectionState"],
            json!(state::CONNECTION_STATE_AUTHORIZED),
            "case {case:?}: expected a read to report AUTHORIZED, got {after}"
        );
        assert_eq!(
            after["StateReason"],
            Value::Null,
            "case {case:?}: an authorized connection reports no reason for holding none, got {after}"
        );
        assert_eq!(
            after["LastAuthorizedTime"], updated["LastAuthorizedTime"],
            "case {case:?}: a read and the update that wrote it must agree, got {after}"
        );
        assert_eq!(
            after["AuthorizationType"],
            json!(updated_as),
            "case {case:?}: expected the type the update named, got {after}"
        );
        assert_eq!(
            after["AuthParameters"][reported_member], reported_half,
            "case {case:?}: expected the non-secret half of the supplied credentials under \
             {reported_member}, got {after}"
        );
        let entry = listed(&emulator, case, name).await;
        assert_eq!(
            entry["ConnectionState"],
            json!(state::CONNECTION_STATE_AUTHORIZED),
            "case {case:?}: a listing and a read must agree about the state, got {entry}"
        );

        // The downstream reader follows the re-authorization.
        assert_eq!(
            destination_state(&emulator, case, destination).await,
            json!(state::API_DESTINATION_STATE_ACTIVE),
            "case {case:?}: a destination on a re-authorized connection is active again"
        );
    }
}

/// An update that supplies no authorization parameters decides nothing about the
/// authorization: whatever state, reason and stamp the connection held, it keeps.
#[tokio::test]
async fn an_update_supplying_no_credentials_decides_no_authorization() {
    // (the case, whether the connection is deauthorized before the update, the
    //  state every reader must report after it)
    let rows = [
        (
            "an authorized connection, described afresh",
            false,
            state::CONNECTION_STATE_AUTHORIZED,
        ),
        (
            "a connection whose authorization was taken away",
            true,
            state::CONNECTION_STATE_DEAUTHORIZED,
        ),
    ];

    for (case, deauthorize_first, expected) in rows {
        let emulator = emulator();
        let name = "the-connection";
        let (status, created) = events(
            &emulator,
            "CreateConnection",
            &json!({
                "Name": name,
                "AuthorizationType": "API_KEY",
                "AuthParameters": {
                    "ApiKeyAuthParameters": { "ApiKeyName": "x-api-key", "ApiKeyValue": "the-key" },
                },
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateConnection answered {status} {created}"
        );
        // `CreateConnectionResponse` models no `LastAuthorizedTime`, so the stamp
        // the update must leave alone is read off the connection.
        let (status, fresh) =
            events(&emulator, "DescribeConnection", &json!({ "Name": name })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeConnection answered {status} {fresh}"
        );
        let authorized_at = fresh["LastAuthorizedTime"].clone();
        assert!(
            authorized_at.is_number(),
            "case {case:?}: expected a LastAuthorizedTime, got {fresh}"
        );

        if deauthorize_first {
            let (status, body) =
                events(&emulator, "DeauthorizeConnection", &json!({ "Name": name })).await;
            assert_eq!(
                status, 200,
                "case {case:?}: DeauthorizeConnection answered {status} {body}"
            );
            make_the_owed_transition_due(&emulator, case, name);
            let (_, settled) =
                events(&emulator, "DescribeConnection", &json!({ "Name": name })).await;
            assert_eq!(
                settled["ConnectionState"],
                json!(state::CONNECTION_STATE_DEAUTHORIZED),
                "case {case:?}: expected the deauthorization to settle, got {settled}"
            );
        }

        let (status, updated) = events(
            &emulator,
            "UpdateConnection",
            &json!({ "Name": name, "Description": "a description, and nothing else" }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: UpdateConnection answered {status} {updated}"
        );
        assert_eq!(
            updated["ConnectionState"],
            json!(expected),
            "case {case:?}: expected the state the connection already held ({expected}), \
             got {updated}"
        );
        assert_eq!(
            updated["LastAuthorizedTime"], authorized_at,
            "case {case:?}: an update deciding no authorization re-stamps none, got {updated}"
        );

        let (status, after) = events(&emulator, "DescribeConnection", &json!({ "Name": name })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeConnection answered {status} {after}"
        );
        assert_eq!(
            after["ConnectionState"],
            json!(expected),
            "case {case:?}: expected a read to report {expected}, got {after}"
        );
        assert_eq!(
            after["Description"],
            json!("a description, and nothing else"),
            "case {case:?}: the member the update DID carry is written, got {after}"
        );
        let expected_reason = if deauthorize_first {
            json!(DEAUTHORIZED_REASON)
        } else {
            Value::Null
        };
        assert_eq!(
            after["StateReason"], expected_reason,
            "case {case:?}: expected the reason the connection already reported, got {after}"
        );
    }
}
