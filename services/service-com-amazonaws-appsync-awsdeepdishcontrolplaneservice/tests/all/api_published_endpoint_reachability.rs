// SPDX-License-Identifier: CC0-1.0

//! Where an `Api` — AppSync's Event API resource — tells a client to dial, over EVERY
//! projection of it, and what the traffic a client makes there is counted as.
//!
//! The rule these cases assert, one level above the recorded case (one Event API whose
//! channel namespace a check drives against the published endpoint and then polls the
//! lifecycle counters of):
//!
//! > Every endpoint address an AppSync API publishes is an address of the server that
//! > published it: `Api$dns` carries, per endpoint, the `host:port` of the socket that
//! > endpoint is SERVED on — never a host belonging to another server. The create, every
//! > read, the reply to an update and every listing of one API publish the same records,
//! > each endpoint has a socket of its own, and no two APIs that exist at once publish the
//! > same socket. So a client that takes those addresses out of the INVENTORY and dials
//! > them reaches that API's data plane, and what it did there is counted in CloudWatch's
//! > `AWS/AppSync` namespace under the dimensions of the API and the channel namespace that
//! > served it.
//!
//! The rows span what the rule generalises over: the authorization the API is configured
//! for, how many providers it sets up, whether the projection carries `ownerContact` and
//! `tags`, each endpoint key the family publishes, and the five operations that project one
//! API.
//!
//! This is the `Api` half of the rule, which no suite covers past the create and the read
//! ([`crate::event_api`], [`crate::event_api_data_plane`]); the `GraphqlApi` half — the same
//! rule over `dns` and `uris` for every projection of a GraphQL API — is
//! [`crate::graphql_api_announced_endpoints`] and [`crate::graphql_api_published_uri`], and
//! what an Event API's data plane DOES once reached is [`crate::event_api_data_plane`].

use std::collections::BTreeSet;
use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_api::{event_config, LOOPBACK_HOST};
use crate::event_api_data_plane::{
    appsync, connected, create_event_api, create_namespace, dial, emulator, exchange,
    loopback_port, metric_sum, EventApi, FILTER_HANDLER,
};
use crate::graphql_api_mapping_template_resolvers::try_post_answer;

/// The collection an `Api` is created in and listed from.
const COLLECTION: &str = "/v2/apis";

/// The two endpoints an Event API publishes, under these keys. Source: `Api$dns` — "the DNS
/// records for the API. This will include an HTTP and a real-time endpoint"
/// (`smithy_query(query="shape", shape="Api")`).
const ENDPOINTS: &[&str] = &["HTTP", "REALTIME"];

/// How long a case waits for a published socket to accept a connection: generous for a
/// loaded test machine, and bounded so an address nothing serves fails instead of hanging.
const DIAL_DEADLINE: Duration = Duration::from_secs(30);

/// How long a case waits for a series the data plane published to be readable, and how often
/// it looks again. The wait is on the CONDITION — the series carrying a datapoint — so
/// nothing here depends on how fast the host runs, and it is short because the plane
/// publishes a series before it answers the call that produced it.
const METRIC_DEADLINE: Duration = Duration::from_secs(15);
const POLL_SPACING: Duration = Duration::from_millis(50);

/// One row: the API created, and the update whose reply projects it again.
struct Row {
    case: &'static str,
    create: Value,
    update: Value,
}

/// An `eventConfig` whose every mode names `auth_type`, over the providers `set_up`; the
/// builder is the create-and-read suite's ([`crate::event_api`]).
fn modes_of(set_up: &[&str], auth_type: &str) -> Value {
    event_config(set_up, &[auth_type], &[auth_type], &[auth_type], None)
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an api authorizing with a key, over the one provider it set up",
            create: json!({
                "name": "Published Keyed",
                "eventConfig": modes_of(&["API_KEY"], "API_KEY"),
            }),
            update: json!({
                "name": "Published Keyed",
                "eventConfig": modes_of(&["API_KEY"], "API_KEY"),
            }),
        },
        Row {
            case: "an api authorizing with IAM, with a second provider set up beside it",
            create: json!({
                "name": "Published Signed",
                "eventConfig": modes_of(&["API_KEY", "AWS_IAM"], "AWS_IAM"),
                "ownerContact": "endpoints@example.com",
            }),
            update: json!({
                "name": "Published Signed",
                "eventConfig": modes_of(&["API_KEY", "AWS_IAM"], "AWS_IAM"),
            }),
        },
        Row {
            case: "an api carrying tags and an owner contact",
            create: json!({
                "name": "Published Tagged",
                "eventConfig": modes_of(&["API_KEY", "AWS_IAM"], "API_KEY"),
                "ownerContact": "endpoints@example.com",
                "tags": {"team": "events"},
            }),
            update: json!({
                "name": "Published Tagged",
                "eventConfig": modes_of(&["API_KEY", "AWS_IAM"], "API_KEY"),
                "ownerContact": "owner@example.com",
            }),
        },
    ]
}

#[tokio::test]
async fn every_projection_of_an_event_api_publishes_endpoints_this_emulator_answers_on() {
    let router = emulator();
    // Every address published so far, over every API of the table: a socket belongs to one
    // endpoint of one API.
    let mut taken: BTreeSet<String> = BTreeSet::new();

    for row in rows() {
        let case = row.case;
        let (status, created) = appsync(&router, "POST", COLLECTION, Some(row.create)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateApi must answer 200, got {status} and {created}"
        );
        let api = created["api"].clone();
        let api_id = api["apiId"].as_str().unwrap_or_default().to_string();

        // The create publishes one address per endpoint of the family, and no others.
        let published = addresses(case, "the create", &api);
        let mut records: Vec<String> = api["dns"]
            .as_object()
            .map(|members| members.keys().cloned().collect())
            .unwrap_or_default();
        records.sort();
        let mut expected: Vec<String> = ENDPOINTS.iter().map(|key| (*key).to_string()).collect();
        expected.sort();
        assert_eq!(
            records, expected,
            "case {case:?}: expected exactly the endpoints of the family in dns, got {api}"
        );

        // Each endpoint is published on a socket of its own, and no socket is published for
        // two APIs: an address identifies where ONE endpoint of ONE API is served.
        for (endpoint, authority) in &published {
            assert!(
                taken.insert(authority.clone()),
                "case {case:?}: {endpoint} is published at {authority:?}, which another \
                 endpoint already publishes"
            );
        }

        // Every operation that projects the API publishes the same records.
        let single = format!("{COLLECTION}/{api_id}");
        let projections: Vec<(&str, &str, Option<Value>)> = vec![
            ("the read", "GET", None),
            ("the update", "POST", Some(row.update)),
            ("the read after the update", "GET", None),
        ];
        for (label, method, body) in projections {
            let (status, answered) = appsync(&router, method, &single, body).await;
            assert_eq!(
                status, 200,
                "case {case:?}: {label} must answer 200, got {status} and {answered}"
            );
            assert_eq!(
                addresses(case, label, &answered["api"]),
                published,
                "case {case:?}: {label} must publish the records the create minted, got \
                 {answered}"
            );
        }

        // And the listing, which is where a client that holds no id finds the API.
        let entry = inventory(&router, &api_id).await;
        assert_eq!(
            addresses(case, "the listing", &entry),
            published,
            "case {case:?}: the listing must publish the records the create minted, got {entry}"
        );

        // Each published address is a socket this emulator answers on: a client that dials
        // exactly what was published is connected. The dial is the data-plane suite's own
        // (`event_api_data_plane::dial`), which reaches this emulator's loopback listener
        // and nothing else.
        for (endpoint, authority) in &published {
            let label = format!("{case} ({endpoint})");
            assert!(
                loopback_port(&label, authority) > 0,
                "case {label:?}: expected the port the endpoint is served on, got {authority:?}"
            );
            let dialled = tokio::time::timeout(DIAL_DEADLINE, dial(&label, authority)).await;
            assert!(
                matches!(dialled, Ok(Ok(_))),
                "case {label:?}: the published address {authority:?} must accept a connection, \
                 got {dialled:?}"
            );
        }
    }
}

#[tokio::test]
async fn traffic_dialled_at_the_addresses_the_inventory_published_is_counted_under_that_api() {
    let router = emulator();
    let case = "an event api dialled at the records its listing published";
    let namespace = "lifecycle";
    let created = create_event_api(&router, "Published Lifecycle").await;
    create_namespace(&router, &created.api_id, namespace, Some(FILTER_HANDLER)).await;

    // The API as a client that found it in the inventory holds it: the two addresses below
    // are the LISTING's, and nothing of the create's reply is dialed.
    let listing = inventory(&router, &created.api_id).await;
    let discovered = EventApi {
        api_id: created.api_id.clone(),
        http: listing["dns"]["HTTP"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        realtime: listing["dns"]["REALTIME"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        api_key: created.api_key.clone(),
    };

    let mut socket = connected(case, &discovered).await;
    let subscribed = exchange(
        case,
        &mut socket,
        &json!({"type": "subscribe", "id": "sub-1", "channel": format!("/{namespace}/room1")}),
    )
    .await;
    assert_eq!(
        subscribed.as_ref().map(|frame| frame["type"].clone()),
        Some(json!("subscribe_success")),
        "case {case:?}: subscribe must be answered subscribe_success, got {subscribed:?}"
    );

    // One publish on the discovered HTTP address, carrying one event the namespace's handler
    // keeps and one it drops, so what the counters report is what the handler decided.
    let kept = json!({"m": "a", "keep": true}).to_string();
    let dropped = json!({"m": "b", "keep": false}).to_string();
    let answered = try_post_answer(
        case,
        &discovered.http,
        "/event",
        "POST",
        &json!({"channel": format!("/{namespace}/room1"), "events": [kept, dropped]}),
        &[("x-api-key", discovered.api_key.as_str())],
    )
    .await;
    let Some((status, _, accepted)) = answered else {
        panic!(
            "case {case:?}: the published HTTP address {:?} must be one this emulator answers on",
            discovered.http
        )
    };
    assert_eq!(
        (status, accepted["successful"].as_array().map(Vec::len)),
        (200, Some(2)),
        "case {case:?}: the published endpoint must accept both events, got {status} and \
         {accepted}"
    );

    // The namespace's lifecycle counters, under the dimensions of the API and namespace that
    // served the traffic, and the connection's under the API's own id.
    let namespace_dimensions = [
        ("EventAPIId", discovered.api_id.as_str()),
        ("ChannelNamespaceName", namespace),
    ];
    for (metric, expected) in [
        ("SubscribeSuccess", 1.0),
        ("PublishHandlerInvocations", 1.0),
        ("SuccessfulEvents", 2.0),
        ("DroppedEvents", 1.0),
    ] {
        let summed = summed_within(&router, metric, &namespace_dimensions).await;
        assert_eq!(
            summed,
            Some(expected),
            "case {case:?}: expected {metric} to sum to {expected} under \
             [EventAPIId, ChannelNamespaceName], got {summed:?}"
        );
    }
    let api_dimensions = [("EventAPIId", discovered.api_id.as_str())];
    let connections = summed_within(&router, "ConnectSuccess", &api_dimensions).await;
    assert_eq!(
        connections,
        Some(1.0),
        "case {case:?}: expected one ConnectSuccess under [EventAPIId], got {connections:?}"
    );
}

/// The entry the listing carries for `api_id`.
async fn inventory(router: &MultiServiceRouter, api_id: &str) -> Value {
    let (status, inventory) = appsync(router, "GET", COLLECTION, None).await;
    assert_eq!(
        status, 200,
        "ListApis must answer 200, got {status} and {inventory}"
    );
    inventory["apis"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|entry| entry["apiId"] == json!(api_id))
        .cloned()
        .unwrap_or_else(|| panic!("the listing must carry the API {api_id:?}, got {inventory}"))
}

/// Every address `api` publishes, read out of the projection as a client reads it, having
/// checked each one is this emulator's own: the host is the one the core binds a resource
/// listener on, so a host belonging to another server — an AWS-owned domain among them —
/// fails here.
fn addresses(case: &str, label: &str, api: &Value) -> Vec<(&'static str, String)> {
    ENDPOINTS
        .iter()
        .map(|endpoint| {
            let authority = api["dns"][endpoint]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: {label} must publish {endpoint}, got {api}")
                })
                .to_string();
            assert!(
                authority.starts_with(&format!("{LOOPBACK_HOST}:")),
                "case {case:?}: {label} must publish {endpoint} at an address this emulator \
                 serves, got {authority:?}"
            );
            (*endpoint, authority)
        })
        .collect()
}

/// The `Sum` of one series, looking again until it carries a datapoint or
/// [`METRIC_DEADLINE`] passes: the wait is on the series being readable, never on a span of
/// the clock.
async fn summed_within(
    router: &MultiServiceRouter,
    metric: &str,
    dimensions: &[(&str, &str)],
) -> Option<f64> {
    let deadline = tokio::time::Instant::now() + METRIC_DEADLINE;
    loop {
        let summed = metric_sum(router, metric, dimensions).await;
        if summed.is_some() || tokio::time::Instant::now() >= deadline {
            return summed;
        }
        tokio::time::sleep(POLL_SPACING).await;
    }
}
