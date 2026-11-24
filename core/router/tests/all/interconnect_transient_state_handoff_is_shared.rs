// SPDX-License-Identifier: CC0-1.0

//! ONE handoff puts an Interconnect connection into a transient provisioning state and
//! leaves its successor to the clock.
//!
//! The rule under test: **a call that starts provisioning work on an existing connection
//! reports the transient `ConnectionState` the model documents for that work and leaves
//! that work's successor to the clock, which settles it one provisioning span later — so
//! the caller observes the transient state, the successor arrives without anybody asking
//! for it, and which call started the work changes neither of those two things.** The
//! documented states are `Connection$state`'s own
//! (`smithy_query(query="doc",shape="Connection",member="state")`): "deleting: The
//! connection is being deleted" -> "deleted: The connection has been deleted" for a
//! teardown, and "updating: The connection is being updated" -> the state the change
//! found the connection in for a re-provisioning bandwidth change.
//!
//! That is what the shared writer in the owning crate's `state` module buys: both of the
//! service's re-provisioning writes — the teardown `DeleteConnection` applies and the
//! bandwidth change `UpdateConnection` applies — are the same two-field computation over
//! a different pair of states, so neither handler can time the handoff, or arm the
//! clock's successor, differently from the other. This suite drives that one handoff
//! through BOTH of those operations.
//!
//! The rows differ along what the rule generalises over: the operation that starts the
//! work (both of them), the state the work is started from (the state a connection is
//! created in and the fully provisioned one), the Region the connection lives in, the
//! bandwidth the connection carries and the one a change asks for, and the gateway the
//! connection is attached to.
//!
//! This suite lives here, beside the other suites of the assembled emulator, because the
//! owning crate roots its integration tests at a hand-declared `tests/all.rs`, which a
//! fix may not edit; the router's `tests/all/` picks a new suite up with nothing to
//! declare. Every call goes through a router with Interconnect mounted beside the service
//! that owns the account's Direct Connect gateways on one engine, exactly as the
//! production router mounts them, so each assertion exercises the real wire path.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The signing name the Interconnect model gives the service, which is the credential
/// scope the router picks the claiming service by.
const SIGNING_NAME: &str = "interconnect";

/// The signing name and `awsJson1_1` target prefix of the service that owns the
/// account's Direct Connect gateways, one of which is what an attach point IS.
const DIRECT_CONNECT_SIGNING_NAME: &str = "directconnect";
const DIRECT_CONNECT_TARGET_PREFIX: &str = "OvertureService";

/// The Regions the rows sign their requests for: a connection belongs to the Region it
/// was created in, so the rule is checked across that axis too.
const REGIONS: [&str; 2] = ["us-east-1", "eu-west-1"];

/// "requested: The initial state of a connection. The state will remain here until the
/// Connection is accepted on the Partner portal."
const INITIAL_STATE: &str = "requested";

/// "available: The connection has been fully provisioned between AWS and the Partner."
const PROVISIONED_STATE: &str = "available";

/// "deleting: The connection is being deleted."
const DELETING_STATE: &str = "deleting";

/// "deleted: The connection has been deleted."
const DELETED_STATE: &str = "deleted";

/// "updating: The connection is being updated."
const UPDATING_STATE: &str = "updating";

/// The `DirectConnectGatewayState` that service documents as ready for use, which is the
/// only state a gateway is an attach point in.
const GATEWAY_READY_STATE: &str = "available";

/// The `Environment$state` that says an Environment carries no connections, so no row is
/// built on one.
const UNAVAILABLE_ENVIRONMENT_STATE: &str = "unavailable";

/// An email address of the shape an Environment declaring the `email`
/// `RemoteAccountIdentifierType` asks a create for.
const PARTNER_EMAIL: &str = "interconnect-handoff@example.test";

/// How long a wait for a state a service OWES runs before the case fails. Generous on
/// purpose: every span here is a stored due instant that nothing sleeps for, so what a
/// row waits for is the STATE and a loaded host must not be what decides it.
const LOOK_BUDGET: Duration = Duration::from_secs(60);

/// How often such a wait looks again — short enough to observe the state a change
/// settles into before the clock carries the connection on from it.
const LOOK_INTERVAL: Duration = Duration::from_millis(5);

/// The gateways the rows attach to, differing in name and in `amazonSideAsn`.
const GATEWAY_ROWS: [(&str, Option<i64>); 2] = [
    ("ic-shared-handoff-a", None),
    ("ic-shared-handoff-b", Some(64_514)),
];

/// The provisioning work a row starts on its connection: the two calls of this service
/// that hand a connection to the clock in a transient state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Work {
    /// `DeleteConnection`: the teardown.
    Teardown,
    /// `UpdateConnection` asking for a new bandwidth: the re-provisioning change.
    Rebandwidth,
}

impl Work {
    /// The operation that starts this work.
    fn operation(self) -> &'static str {
        match self {
            Work::Teardown => "DeleteConnection",
            Work::Rebandwidth => "UpdateConnection",
        }
    }

    /// The transient `ConnectionState` the model documents for this work, which is what
    /// the call that starts it must report.
    fn transient_state(self) -> &'static str {
        match self {
            Work::Teardown => DELETING_STATE,
            Work::Rebandwidth => UPDATING_STATE,
        }
    }

    /// The state the clock owes this work, given the state the work was started from: a
    /// teardown ends in the terminal deleted state, a re-provisioning change comes back
    /// to the state it found the connection in.
    fn successor(self, came_from: &str) -> String {
        match self {
            Work::Teardown => DELETED_STATE.to_string(),
            Work::Rebandwidth => came_from.to_string(),
        }
    }
}

/// One row of the table.
#[derive(Debug, Clone)]
struct Row {
    region: &'static str,
    environment: Value,
    gateway: String,
    /// The state the work is started from.
    from_state: &'static str,
    created_with: String,
    asked_for: String,
    work: Work,
}

impl Row {
    fn environment_id(&self) -> String {
        text(&self.environment, "environmentId")
    }

    fn label(&self) -> String {
        format!(
            "{} in {} on {} / {} -> {} started while {}",
            self.work.operation(),
            self.region,
            self.environment_id(),
            self.created_with,
            self.asked_for,
            self.from_state
        )
    }
}

/// A router with Interconnect and the service owning the account's Direct Connect
/// gateways mounted over ONE engine, as the assembled emulator mounts them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The Authorization header below still carries the credential scope the router routes
    // by; the signature itself is not what this suite is about.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_interconnect_interconnect::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_directconnect_overtureservice::EmulatedService::new(engine),
    );
    router
}

/// `POST /` for one operation of `signing_name`, signed for `region`.
async fn post_in(
    router: &MultiServiceRouter,
    region: &str,
    signing_name: &str,
    target: &str,
    json_version: &str,
    body: Value,
) -> (u16, Value) {
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/{signing_name}/aws4_request, \
         SignedHeaders=host;x-amz-date;x-amz-target, Signature=not-verified"
    );
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.localhost"))
        .header(
            "content-type",
            format!("application/x-amz-json-{json_version}"),
        )
        .header("x-amz-target", target)
        .header("authorization", authorization)
        .body(bytes::Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let decoded = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, decoded)
}

/// One Interconnect operation (`awsJson1_0`) signed for `region`, as a `(status, body)`
/// pair.
async fn ic_in(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: Value,
) -> (u16, Value) {
    post_in(
        router,
        region,
        SIGNING_NAME,
        &format!("Interconnect.{operation}"),
        "1.0",
        body,
    )
    .await
}

/// One Interconnect operation signed for `region`, required to succeed.
async fn ic_ok_in(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: Value,
) -> Value {
    let (status, decoded) = ic_in(router, region, operation, body).await;
    assert_eq!(
        status, 200,
        "case {operation:?} in {region:?}: expected HTTP 200, got {status} with {decoded:?}"
    );
    decoded
}

/// One Direct Connect operation (`awsJson1_1`), required to succeed.
async fn dx_ok(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    let (status, decoded) = post_in(
        router,
        REGIONS[0],
        DIRECT_CONNECT_SIGNING_NAME,
        &format!("{DIRECT_CONNECT_TARGET_PREFIX}.{operation}"),
        "1.1",
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {operation:?}: expected HTTP 200, got {status} with {decoded:?}"
    );
    decoded
}

/// Create the gateways of [`GATEWAY_ROWS`] and look until that service reports each one
/// ready for use, answering their server-minted ids in the order created.
async fn ready_gateways(router: &MultiServiceRouter) -> Vec<String> {
    let mut ids = Vec::new();
    for (name, asn) in GATEWAY_ROWS {
        let mut request = Map::new();
        request.insert("directConnectGatewayName".to_string(), json!(name));
        if let Some(asn) = asn {
            request.insert("amazonSideAsn".to_string(), json!(asn));
        }
        let created = dx_ok(router, "CreateDirectConnectGateway", Value::Object(request)).await;
        ids.push(text(
            &created["directConnectGateway"],
            "directConnectGatewayId",
        ));
    }
    for id in &ids {
        let started = Instant::now();
        loop {
            let described = dx_ok(
                router,
                "DescribeDirectConnectGateways",
                json!({ "directConnectGatewayId": id }),
            )
            .await;
            let state = described["directConnectGateways"]
                .as_array()
                .and_then(|gateways| gateways.first().cloned())
                .map(|gateway| text(&gateway, "directConnectGatewayState"))
                .unwrap_or_default();
            if state == GATEWAY_READY_STATE {
                break;
            }
            assert!(
                started.elapsed() < LOOK_BUDGET,
                "setup {id:?}: expected the gateway to be ready for use within {LOOK_BUDGET:?}, \
                 it is still {state:?}"
            );
            tokio::time::sleep(LOOK_INTERVAL).await;
        }
    }
    ids
}

/// A string member, or `""` when absent — so a missing member fails an assertion rather
/// than panicking the suite.
fn text(value: &Value, member: &str) -> String {
    value[member].as_str().unwrap_or_default().to_string()
}

/// `Environment$bandwidths.available` ("the list of currently available bandwidths") for
/// one listed Environment.
fn available_bandwidths(environment: &Value) -> Vec<String> {
    environment["bandwidths"]["available"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|bandwidth| bandwidth.as_str().unwrap_or_default().to_string())
        .collect()
}

/// The identifying information an Environment that declares a `remoteIdentifierType`
/// asks a create for, or nothing at all for one that declares none.
fn remote_account_for(environment: &Value) -> Option<Value> {
    match environment["remoteIdentifierType"].as_str()? {
        "account" => Some(json!({ "identifier": engine::EMULATOR_ACCOUNT_ID })),
        "email" => Some(json!({ "identifier": PARTNER_EMAIL })),
        _ => None,
    }
}

/// Every Environment the endpoint of `region` offers that a connection can be created
/// on, with at least two bandwidths so a row has a change to ask for.
async fn placeable_environments(router: &MultiServiceRouter, region: &str) -> Vec<Value> {
    let listed = ic_ok_in(router, region, "ListEnvironments", json!({})).await;
    listed["environments"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|environment| {
            text(environment, "state") != UNAVAILABLE_ENVIRONMENT_STATE
                && available_bandwidths(environment).len() >= 2
        })
        .collect()
}

/// The table: both kinds of work, started from both of the states this service permits a
/// bandwidth change from, in each Region, on that Region's own live catalog.
async fn table(router: &MultiServiceRouter, gateways: &[String]) -> Vec<Row> {
    let mut rows = Vec::new();
    for (seed, region) in REGIONS.into_iter().enumerate() {
        let catalog = placeable_environments(router, region).await;
        let environment = catalog
            .first()
            .cloned()
            .unwrap_or_else(|| panic!("case {region:?}: expected the Region to publish an Environment a connection can be created on"));
        let offered = available_bandwidths(&environment);
        for (offset, from_state) in [INITIAL_STATE, PROVISIONED_STATE].into_iter().enumerate() {
            for (step, work) in [Work::Teardown, Work::Rebandwidth].into_iter().enumerate() {
                // The rows differ in the bandwidth the connection carries and in the one
                // a change asks for, taken from the live catalog of the row's own Region.
                let created_with = offered[(seed + offset) % offered.len()].clone();
                let asked_for = offered
                    .iter()
                    .find(|bandwidth| **bandwidth != created_with)
                    .cloned()
                    .unwrap_or_else(|| created_with.clone());
                rows.push(Row {
                    region,
                    environment: environment.clone(),
                    gateway: gateways[(seed + offset + step) % gateways.len()].clone(),
                    from_state,
                    created_with,
                    asked_for,
                    work,
                });
            }
        }
    }
    rows
}

/// `CreateConnection` for one row, answering the stored `Connection`.
async fn create(router: &MultiServiceRouter, row: &Row, description: &str) -> Value {
    let mut request = Map::new();
    request.insert("environmentId".to_string(), json!(row.environment_id()));
    request.insert("bandwidth".to_string(), json!(row.created_with));
    request.insert(
        "attachPoint".to_string(),
        json!({ "directConnectGateway": row.gateway }),
    );
    request.insert("description".to_string(), json!(description));
    if let Some(remote_account) = remote_account_for(&row.environment) {
        request.insert("remoteAccount".to_string(), remote_account);
    }
    let created = ic_ok_in(router, row.region, "CreateConnection", Value::Object(request)).await;
    created["connection"].clone()
}

/// `GetConnection` in the row's Region, answering the stored `Connection`.
async fn read_back(router: &MultiServiceRouter, row: &Row, identifier: &str) -> Value {
    let fetched = ic_ok_in(
        router,
        row.region,
        "GetConnection",
        json!({ "identifier": identifier }),
    )
    .await;
    fetched["connection"].clone()
}

/// Look until the connection reports `wanted`, answering the record it reported it as.
async fn wait_for_state(
    router: &MultiServiceRouter,
    row: &Row,
    identifier: &str,
    wanted: &str,
) -> Value {
    let case = row.label();
    let started = Instant::now();
    loop {
        let connection = read_back(router, row, identifier).await;
        let state = text(&connection, "state");
        if state == wanted {
            return connection;
        }
        assert!(
            started.elapsed() < LOOK_BUDGET,
            "case {case:?}: expected the connection to reach {wanted:?} within {LOOK_BUDGET:?}, \
             it is still {state:?}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Look until the connection reports anything OTHER than `transient`, answering the
/// FIRST record it settled as — which is the state the clock was handed.
async fn wait_until_settled(
    router: &MultiServiceRouter,
    row: &Row,
    identifier: &str,
    transient: &str,
) -> Value {
    let case = row.label();
    let started = Instant::now();
    loop {
        let connection = read_back(router, row, identifier).await;
        let state = text(&connection, "state");
        if state != transient {
            return connection;
        }
        assert!(
            started.elapsed() < LOOK_BUDGET,
            "case {case:?}: expected the clock to settle the connection within {LOOK_BUDGET:?}, \
             it is still {state:?}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Bring the row's connection to the state its work is started from, answering its
/// identifier.
async fn connection_in_from_state(
    router: &MultiServiceRouter,
    row: &Row,
    description: &str,
) -> String {
    let case = row.label();
    let created = create(router, row, description).await;
    let identifier = text(&created, "id");
    assert_eq!(
        text(&created, "state"),
        INITIAL_STATE,
        "case {case:?}: expected a created connection to report {INITIAL_STATE:?}, got {:?}",
        created["state"]
    );
    if row.from_state == PROVISIONED_STATE {
        wait_for_state(router, row, &identifier, PROVISIONED_STATE).await;
    }
    identifier
}

/// Start the row's work on `identifier`, answering the `Connection` the call reports.
async fn start_work(router: &MultiServiceRouter, row: &Row, identifier: &str) -> Value {
    let request = match row.work {
        Work::Teardown => json!({ "identifier": identifier }),
        Work::Rebandwidth => json!({ "identifier": identifier, "bandwidth": row.asked_for }),
    };
    let answered = ic_ok_in(router, row.region, row.work.operation(), request).await;
    answered["connection"].clone()
}

/// RULE: a call that starts provisioning work on an existing connection reports the
/// transient `ConnectionState` the model documents for that work and leaves that work's
/// successor to the clock, which settles it a provisioning span later — the terminal
/// deleted state for a teardown, the state the change found the connection in for a
/// re-provisioning bandwidth change — so a caller reading straight back still observes
/// the transient state and the successor then arrives with nobody asking for it.
#[tokio::test]
async fn both_provisioning_calls_hand_the_connection_to_the_clock_the_same_way() {
    let router = emulator();
    let gateways = ready_gateways(&router).await;
    let rows = table(&router, &gateways).await;
    // The table spans both operations in both of the states a change is permitted from,
    // in both Regions, so an operation quietly dropping out of it fails here rather than
    // passing vacuously.
    assert_eq!(
        rows.len(),
        REGIONS.len() * 4,
        "case \"the handoff table\": expected both kinds of work from both states in every \
         Region, got {} rows",
        rows.len()
    );

    for row in &rows {
        let case = row.label();
        let identifier = connection_in_from_state(&router, row, "one shared handoff").await;

        // The state the work is actually started from, read immediately before it.
        let before = read_back(&router, row, &identifier).await;
        let came_from = text(&before, "state");
        assert_eq!(
            came_from, row.from_state,
            "case {case:?}: expected the work to be started while the connection was {:?}, got \
             {came_from:?}",
            row.from_state
        );

        let started = start_work(&router, row, &identifier).await;
        assert_eq!(
            text(&started, "state"),
            row.work.transient_state(),
            "case {case:?}: expected the call to report {:?}, got {:?}",
            row.work.transient_state(),
            started["state"]
        );
        assert_eq!(
            text(&started, "id"),
            identifier,
            "case {case:?}: expected the call to answer for the connection it was made on, got {:?}",
            started["id"]
        );

        // The successor is the CLOCK's to deliver, not this call's: the span the handoff
        // arms is non-zero, so the record a caller reads straight back is still the
        // transient one rather than the settled one.
        let straight_back = read_back(&router, row, &identifier).await;
        assert_eq!(
            text(&straight_back, "state"),
            row.work.transient_state(),
            "case {case:?}: expected a read straight back to still report {:?}, got {:?}",
            row.work.transient_state(),
            straight_back["state"]
        );

        // And the state the clock was handed is this work's successor.
        let settled =
            wait_until_settled(&router, row, &identifier, row.work.transient_state()).await;
        assert_eq!(
            text(&settled, "state"),
            row.work.successor(&came_from),
            "case {case:?}: expected the clock to settle the connection into {:?}, got {:?}",
            row.work.successor(&came_from),
            settled["state"]
        );

        // Nothing else about the record moved: a teardown keeps the connection readable
        // by its identifier with the bandwidth it carried, and a change carries the
        // bandwidth it asked for through the settle.
        let expected_bandwidth = match row.work {
            Work::Teardown => row.created_with.clone(),
            Work::Rebandwidth => row.asked_for.clone(),
        };
        assert_eq!(
            text(&settled, "bandwidth"),
            expected_bandwidth,
            "case {case:?}: expected the settled record to carry {expected_bandwidth:?}, got {:?}",
            settled["bandwidth"]
        );
        assert_eq!(
            text(&settled, "description"),
            "one shared handoff",
            "case {case:?}: expected the description the create set to survive the work, got {:?}",
            settled["description"]
        );
    }
}

/// RULE: whether the work is to be started at all is the CALLER's verdict and not the
/// handoff's — a teardown repeated on a connection already on its way out neither
/// restarts the transition nor fails, so the clock still settles the terminal state at
/// the instant the FIRST call armed it, and a change is refused once the teardown has
/// started.
#[tokio::test]
async fn a_repeated_teardown_neither_rearms_the_clock_nor_fails() {
    let router = emulator();
    let gateways = ready_gateways(&router).await;
    let rows: Vec<Row> = table(&router, &gateways)
        .await
        .into_iter()
        .filter(|row| row.work == Work::Teardown)
        .collect();
    assert!(
        !rows.is_empty(),
        "case \"the teardown rows\": expected the table to hold teardowns, got none"
    );

    for row in &rows {
        let case = row.label();
        let identifier = connection_in_from_state(&router, row, "torn down once").await;

        let first = start_work(&router, row, &identifier).await;
        let repeat = start_work(&router, row, &identifier).await;
        assert_eq!(
            repeat, first,
            "case {case:?}: expected a repeated teardown to replay the same record, got {repeat:?}"
        );

        // A connection on its way out takes no change at all, whichever call made it.
        let (status, refused) = ic_in(
            &router,
            row.region,
            "UpdateConnection",
            json!({ "identifier": identifier, "bandwidth": row.asked_for }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected a change on a connection being torn down to be refused, got \
             {status} with {refused:?}"
        );

        let settled =
            wait_until_settled(&router, row, &identifier, row.work.transient_state()).await;
        assert_eq!(
            text(&settled, "state"),
            DELETED_STATE,
            "case {case:?}: expected the repeated teardown to still settle into {DELETED_STATE:?}, \
             got {:?}",
            settled["state"]
        );
    }
}
