// SPDX-License-Identifier: CC0-1.0

//! An event-driven estate fronted by an HTTP api, driven ONLY through the address the api
//! PUBLISHES — the one value a client of `CreateApi`/`GetApi` has to reach it with.
//!
//! The rule, one level above the recorded case (an HTTP api whose `EventBridge-PutEvents`
//! route was never exercised because the published address led away from this emulator):
//!
//! > An api's `ApiEndpoint` is an address THIS emulator serves the api's data plane at, so
//! > the invoke URL a client builds out of that value ALONE reaches the api's route and the
//! > route's integration is performed there — for an `AWS_PROXY` integration naming an
//! > `IntegrationSubtype`, the action the subtype names is carried out in the service that
//! > owns it as a session of the role the integration's `CredentialsArn` names, or refused
//! > by that service on that role's own policies — whatever endpoint the control-plane
//! > client that created the api was configured with.
//!
//! The rows span what the rule generalises over: the endpoint the control-plane client was
//! configured with (an IPv4 loopback literal with a port, a loopback NAME, a bracketed IPv6
//! literal, and one of AWS's own service hosts), the Region the calls are signed for, the
//! form of the invoke URL built from the published value (the endpoint on its own, which is
//! what the stage serving the api's root is reached at, and the endpoint with the stage name
//! appended — "the stage name is typically appended to this URI to form a complete path to a
//! deployed API stage", `Api$ApiEndpoint`), and whether the integration's role may perform
//! the action. The recorded case — an api created through the emulator's loopback endpoint,
//! with and without the grant — is TWO of those rows.
//!
//! `http_api_published_invoke_address` holds the addressing rule on its own (every endpoint
//! form, every reporter, a proxy route), and `http_api_service_action_integration` holds the
//! subtype rule reached at the api's own documented host (every mapping expression, every
//! policy outcome, several subtypes). This suite is the two JOINED, which is the trajectory
//! the issue filed: nothing here is a value this emulator fills in — the event is read back
//! off the queue a rule of the named bus targets, which another service holds.

use serde_json::json;

use crate::http_api_published_invoke_address::{announced_parts, expected_announced_parts};
use crate::http_api_service_action_integration::{
    grant, subtype_integration, Emulator, ACCESS_DENIED, ACTION_REFUSED, BUS, PUT_EVENTS_ACTION,
    ROLE_NAME, SESSION_NAME,
};

/// The first-class action the route below performs: the publication of an event onto the bus
/// its mapping names.
const PUT_EVENTS_SUBTYPE: &str = "EventBridge-PutEvents";

/// The route every row deploys, and the path within the stage that matches it.
const ROUTE_KEY: &str = "POST /orders";
const ROUTE_PATH: &str = "/orders";

/// The reserved stage the api is deployed to, the one "that serves an api's root": a client
/// reaches it with no stage segment in the path at all, or with the stage name appended.
const DEFAULT_STAGE: &str = "$default";

/// The scheme an HTTP api is invoked over, which its published endpoint names.
const SCHEME: &str = "https://";

/// What the route's mapping publishes, and the body the client posts to it.
const SOURCE: &str = "app.orders";
const DETAIL_TYPE: &str = "OrderPlaced";
const BODY: &str = r#"{"id":"o-1"}"#;

/// One row: the client that created the api, the invoke URL it then builds out of the
/// published endpoint, and what the integration's role may do.
struct Row {
    case: &'static str,
    /// The endpoint the control-plane client is configured with — the `Host` its calls carry.
    control_endpoint: &'static str,
    region: &'static str,
    /// Whether that client resolves nothing but the endpoint it reached, which is the address
    /// the api is then announced on.
    reaches_only_the_emulator: bool,
    /// Whether the invoke URL appends the stage name to the published endpoint.
    appends_the_stage_name: bool,
    /// Whether the role the integration's `CredentialsArn` names may publish the event.
    granted: bool,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the recorded case: an api created through the emulator's loopback \
                   endpoint, whose role may publish",
            control_endpoint: "127.0.0.1:45923",
            region: "us-west-2",
            reaches_only_the_emulator: true,
            appends_the_stage_name: false,
            granted: true,
        },
        Row {
            case: "the recorded denial: the same client, a role that may not publish",
            control_endpoint: "127.0.0.1:45923",
            region: "us-west-2",
            reaches_only_the_emulator: true,
            appends_the_stage_name: false,
            granted: false,
        },
        Row {
            case: "the same endpoint named by its loopback NAME, with the stage name \
                   appended to the published endpoint",
            control_endpoint: "localhost:9297",
            region: "eu-west-1",
            reaches_only_the_emulator: true,
            appends_the_stage_name: true,
            granted: true,
        },
        Row {
            case: "the same endpoint named by a bracketed IPv6 literal",
            control_endpoint: "[::1]:9297",
            region: "ap-southeast-2",
            reaches_only_the_emulator: true,
            appends_the_stage_name: true,
            granted: false,
        },
        Row {
            case: "a client that resolves AWS's own hosts: the api's own documented host",
            control_endpoint: "apigateway.us-east-1.amazonaws.com",
            region: "us-east-1",
            reaches_only_the_emulator: false,
            appends_the_stage_name: false,
            granted: true,
        },
        Row {
            case: "a client that resolves AWS's own hosts, in another Region, with the stage \
                   name appended and a role that may not publish",
            control_endpoint: "apigateway.eu-central-1.amazonaws.com",
            region: "eu-central-1",
            reaches_only_the_emulator: false,
            appends_the_stage_name: true,
            granted: false,
        },
    ]
}

/// Assert `announced` is the address this emulator serves the row's api at — the expectation
/// the addressing suite owns ([`expected_announced_parts`]), read here against the value this
/// row's estate published.
fn assert_announced(row: &Row, api_id: &str, announced: &str) {
    let case = row.case;
    assert!(
        announced.starts_with(SCHEME),
        "case {case:?}: an HTTP api must publish an {SCHEME} endpoint, got {announced:?}"
    );
    let (authority, base_path) = announced_parts(announced);
    let (expected_authority, expected_base_path) = expected_announced_parts(
        row.reaches_only_the_emulator,
        row.control_endpoint,
        row.region,
        api_id,
    );
    assert_eq!(
        authority, expected_authority,
        "case {case:?}: the api must be published on {expected_authority}, got {announced:?}"
    );
    assert_eq!(
        base_path, expected_base_path,
        "case {case:?}: the api must be published under {expected_base_path:?}, got \
         {announced:?}"
    );
}

/// RULE: the address an api publishes is one this emulator serves, so a client holding
/// nothing but that value drives the route's first-class service action end to end — the
/// event lands on the queue a rule of the named bus targets when the integration's role may
/// publish it, and the serving service's own refusal is relayed, with nothing delivered, when
/// it may not.
#[tokio::test]
async fn the_published_api_endpoint_drives_the_subtype_route_end_to_end() {
    for row in rows() {
        let case = row.case;
        // One estate per row: this service, EventBridge, Amazon SQS, IAM and the token
        // service over one engine, reached by a client configured with the row's endpoint.
        let emulator = Emulator::at_endpoint(row.control_endpoint, row.region);
        let role = emulator
            .role(case, row.granted.then(|| grant(PUT_EVENTS_ACTION)))
            .await;
        let (matching, near_miss) = emulator.bus_with_rules(case, SOURCE, DETAIL_TYPE).await;
        let (api_id, announced) = emulator
            .api(
                case,
                subtype_integration(
                    PUT_EVENTS_SUBTYPE,
                    Some(&role),
                    json!({
                        "Source": SOURCE,
                        "DetailType": DETAIL_TYPE,
                        "Detail": "$request.body",
                        "EventBusName": BUS,
                    }),
                ),
                ROUTE_KEY,
            )
            .await;
        assert_announced(&row, &api_id, &announced);

        // The reader a client comes back to reports the same address, so the value it dials
        // is the value the api was minted with.
        let (status, fetched) = emulator
            .control("GET", &format!("/v2/apis/{api_id}"), None)
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetApi must answer 200, got {status} {fetched}"
        );
        assert_eq!(
            fetched["apiEndpoint"].as_str(),
            Some(announced.as_str()),
            "case {case:?}: GetApi must report the published endpoint {announced:?}, got \
             {fetched}"
        );

        // The invoke URL a client builds out of that value, and nothing else.
        let path = if row.appends_the_stage_name {
            format!("/{DEFAULT_STAGE}{ROUTE_PATH}")
        } else {
            ROUTE_PATH.to_string()
        };
        let (status, answered) = emulator.invoke(&announced, &path, &[], BODY).await;
        let expected_status = if row.granted { 200 } else { ACTION_REFUSED };
        assert_eq!(
            status, expected_status,
            "case {case:?}: {announced}{path} must answer {expected_status}, got {status} \
             {answered}"
        );

        let delivered = emulator.drained(case, &matching).await;
        if row.granted {
            // EventBridge's own answer to the publication, and the event the mapping
            // described on the queue the matching rule targets.
            assert_eq!(
                answered["FailedEntryCount"],
                json!(0),
                "case {case:?}: expected no failed entry in {answered}"
            );
            assert!(
                answered["Entries"][0]["EventId"].is_string(),
                "case {case:?}: expected EventBridge's own event id in {answered}"
            );
            assert_eq!(
                delivered.len(),
                1,
                "case {case:?}: expected one event on the matching rule's queue, got \
                 {delivered:?}"
            );
            let event = &delivered[0];
            assert_eq!(
                event["source"].as_str(),
                Some(SOURCE),
                "case {case:?}: expected the mapped source, got {event}"
            );
            assert_eq!(
                event["detail-type"].as_str(),
                Some(DETAIL_TYPE),
                "case {case:?}: expected the mapped detail type, got {event}"
            );
            assert_eq!(
                event["detail"],
                json!({ "id": "o-1" }),
                "case {case:?}: expected the request's body as the detail, got {event}"
            );
        } else {
            // The serving service's own refusal, relayed: the error shape it named, and the
            // text naming the action it refused and the session of the integration's role it
            // refused it for — so the call really was made as that role, and no publication
            // was reported for one nobody made.
            let refusal = answered["__type"].as_str().unwrap_or_default();
            assert!(
                refusal.ends_with(ACCESS_DENIED),
                "case {case:?}: expected the serving service's own {ACCESS_DENIED}, got \
                 {answered}"
            );
            let detail = answered["Message"].as_str().unwrap_or_default();
            let session = format!("assumed-role/{ROLE_NAME}/{SESSION_NAME}");
            assert!(
                detail.contains(&session) && detail.contains(PUT_EVENTS_ACTION),
                "case {case:?}: expected a refusal naming {session:?} and \
                 {PUT_EVENTS_ACTION:?}, got {answered}"
            );
            assert!(
                delivered.is_empty(),
                "case {case:?}: expected nothing delivered, got {delivered:?}"
            );
        }
        // A rule differing from the matching one only in the letter case of the detail type
        // receives nothing, whichever address the api was invoked at.
        let missed = emulator.drained(case, &near_miss).await;
        assert!(
            missed.is_empty(),
            "case {case:?}: expected nothing on the case-differing rule's queue, got \
             {missed:?}"
        );
    }
}
