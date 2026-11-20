// SPDX-License-Identifier: CC0-1.0

//! The discovery path a Route 53 ARC client follows, across BOTH planes of the assembled
//! emulator: create a cluster on the recovery-control CONFIG plane, read the Regional
//! endpoints that cluster publishes, and drive the routing-control data plane at EACH of
//! them.
//!
//! The rule under test: **every address a resource publishes for a caller to send its next
//! call to is an address this emulator answers that call on.** For a cluster, the addresses
//! are `Cluster$ClusterEndpoints[].Endpoint` — not labels a client reads but the endpoints it
//! dials ("Specify an endpoint and Amazon Web Services Region when you want to set or
//! retrieve a routing control state in the cluster",
//! `smithy_query(query="doc",shape="ClusterEndpoint",member="Endpoint")`) — and the cluster
//! data plane publishes no generic regional endpoint to fall back on, so a published host
//! outside this process leaves the client with no way in at all. AWS documents a cluster as
//! "a set of five redundant Regional endpoints against which you can execute API calls to
//! update or get the state of routing controls" (`smithy_query(query="doc",shape="Cluster")`
//! on the recovery-control-config model), so the endpoints are REDUNDANT: each one serves the
//! whole `ToggleCustomerAPI` surface over the same routing-control state, and a toggle sent
//! to one is the state the next one reports.
//!
//! The rows span what the rule generalises over: the base this emulator is reachable at (the
//! unserved default, a loopback address with a port, a DNS name with a port), which of the
//! five published entries the call is addressed to (every one of them), which operation
//! performs the write (`UpdateRoutingControlState` and the batch `UpdateRoutingControlStates`,
//! alternating), and which routing control of the panel is toggled. A published address that
//! named one fixed host — an AWS one above all — would pass against no row.
//!
//! Every config-plane operation that answers a `Cluster` is read: `CreateCluster` and
//! `DescribeCluster` for the discovery path itself, then `ListClusters` and `UpdateCluster`,
//! so a client that found the cluster in a listing or read it straight after a change is
//! handed the same served address.
//!
//! Two crates have to agree for one of these rows to hold: the config plane publishes the
//! address, the cluster data plane answers at it, both over one engine, so the suite lives
//! here rather than in either crate. The config plane's own
//! `tests/all/cluster_endpoint_addressing.rs` owns the projection; this reads the half that
//! only the assembled pair shows — that the published address really serves the data plane,
//! through all four of its operations.
//!
//! How the two halves divide, so a later reader does not take one for the other: a signed
//! request routes by the signing name in its credential scope, so what carries a call to the
//! data plane is that scope and not the authority it is addressed to. The half that catches a
//! published host outside this process is therefore the check that each entry's address IS the
//! base this emulator announces (and the authority a request to it carries is that base's);
//! the half the five entries catch is that each one answers the whole surface over the one
//! store, rather than one of them being served and the rest naming nothing.

use std::collections::BTreeMap;
use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_route53recoverycluster_togglecustomerapi::EmulatedService as ClusterDataPlane;
use service_com_amazonaws_route53recoverycontrolconfig_route53recoverycontrolconfig::EmulatedService as RecoveryControlConfig;

// An ARN travels to the config plane as one URI path segment, escaped the way an AWS SDK
// escapes it, and a published address is read for the authority a client dials: the readers of
// both in this target own them.
use crate::aoss_published_collection_endpoint_dialled_by_a_stock_client::authority_of;
use crate::cloudfront_key_value_store_handshake::segment as percent_encode;

/// The Region a request below is signed for. Recovery-control resource ARNs are
/// account-wide, so it appears in none of them.
const REGION: &str = "us-west-2";

/// The signing name that routes a request to the recovery-control config plane.
const CONFIG_SIGNING_NAME: &str = "route53-recovery-control-config";

/// The signing name that routes a request to the cluster data plane.
const DATA_PLANE_SIGNING_NAME: &str = "route53-recovery-cluster";

/// The `X-Amz-Target` prefix the cluster data plane's model fixes (service shape
/// `ToggleCustomerAPI`, awsJson1.0).
const TARGET_PREFIX: &str = "ToggleCustomerAPI";

/// The Region labels a cluster publishes an endpoint under: "an array of the five cluster
/// endpoints (one for each supported Amazon Web Services Region)"
/// (`smithy_query(query="doc",shape="CreateCluster")` on the recovery-control-config model).
const PUBLISHED_REGIONS: &[&str] = &[
    "ap-northeast-1",
    "ap-southeast-2",
    "eu-west-1",
    "us-east-1",
    "us-west-2",
];

/// The bases these rows make the emulator reachable at: the base of an emulator served on no
/// port at all (`smithy_server_runtime::UNSERVED_URL_BASE`), a loopback address with a port,
/// and a DNS name with a port — the forms a served data port publishes.
const BASES: &[&str] = &[
    smithy_server_runtime::UNSERVED_URL_BASE,
    "https://127.0.0.1:39631",
    "https://arc-cluster.test.invalid:9297",
];

/// The routing controls each row creates in its panel, so a batch write spans more than one
/// control and a single write can pick a different one per endpoint.
const CONTROL_NAMES: &[&str] = &["primary", "secondary", "tertiary"];

/// The two values of the modelled `RoutingControlState` enum
/// (`smithy_query(query="shape",shape="RoutingControlState")` -> `On | Off`), and the state a
/// freshly created routing control reports.
const STATE_OFF: &str = "Off";
const STATE_ON: &str = "On";

/// The other value of that enum, which is what a toggle moves a control to.
fn flipped(state: &str) -> &'static str {
    if state == STATE_ON {
        STATE_OFF
    } else {
        STATE_ON
    }
}

/// One emulator reachable at `base`, with both planes of Route 53 ARC mounted over ONE
/// engine exactly as `core/router/src/lib.rs` mounts them: `base` is what `ctx.endpoint()`
/// answers, and so the address the config plane publishes for the data plane beside it.
///
/// Request authentication is off, as in the other suites here; the credential scope is what
/// routes a request.
fn emulator_at(base: &str) -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.set_url_base(
        smithy_server_runtime::EndpointUrl::parse(base)
            .unwrap_or_else(|| panic!("{base:?} is a usable emulator base URL")),
    );
    router.register_service(ClusterDataPlane::new(Arc::clone(&engine)));
    router.register_service(RecoveryControlConfig::new(engine));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`, which is how
/// every signed SDK request routes. The signature is never checked here.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host;x-amz-target, \
         Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Send one request and read its status and JSON body, naming `case` when the body is not
/// JSON at all.
async fn dispatch(
    router: &MultiServiceRouter,
    request: http::Request<Bytes>,
    case: &str,
) -> (u16, Value) {
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = response.into_body();
    let parsed: Value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body).unwrap_or_else(|error| {
            panic!(
                "case {case}: the response body is not JSON ({error}): {:?}",
                String::from_utf8_lossy(&body)
            )
        })
    };
    (status, parsed)
}

/// Call one config-plane operation (a REST route) and return its status and body.
async fn config(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body.map_or_else(Bytes::new, |value| {
        Bytes::from(serde_json::to_vec(&value).expect("the request body serializes"))
    });
    let request = http::Request::builder()
        .method(method)
        .uri(format!(
            "https://{CONFIG_SIGNING_NAME}.{REGION}.amazonaws.com{path}"
        ))
        .header("content-type", "application/json")
        .header("authorization", authorization(CONFIG_SIGNING_NAME))
        .body(payload)
        .expect("a well-formed request");
    dispatch(router, request, path).await
}

/// One data-plane call sent to `endpoint`, exactly as a client configured with that published
/// address sends it: the request is addressed to the host the published endpoint names.
async fn data_plane_at(
    router: &MultiServiceRouter,
    endpoint: &str,
    operation: &str,
    body: &Value,
    case: &str,
) -> (u16, Value) {
    let base = endpoint.trim_end_matches('/');
    let authority = authority_of(endpoint);
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("{base}/"))
        .header("host", authority)
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("{TARGET_PREFIX}.{operation}"))
        .header("authorization", authorization(DATA_PLANE_SIGNING_NAME))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    dispatch(router, request, case).await
}

/// `CreateCluster`, answering the cluster's ARN.
async fn create_cluster(router: &MultiServiceRouter, name: &str, case: &str) -> String {
    let (status, created) =
        config(router, "POST", "/cluster", Some(json!({ "ClusterName": name }))).await;
    assert_eq!(
        status, 200,
        "case {case}: expected CreateCluster 200, got {status} with {created}"
    );
    created["Cluster"]["ClusterArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: a created cluster has an ARN, got {created}"))
        .to_string()
}

/// `CreateControlPanel` on `cluster_arn`, answering the panel's ARN.
async fn create_control_panel(
    router: &MultiServiceRouter,
    cluster_arn: &str,
    name: &str,
    case: &str,
) -> String {
    let (status, created) = config(
        router,
        "POST",
        "/controlpanel",
        Some(json!({ "ClusterArn": cluster_arn, "ControlPanelName": name })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case}: expected CreateControlPanel 200, got {status} with {created}"
    );
    created["ControlPanel"]["ControlPanelArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: a created panel has an ARN, got {created}"))
        .to_string()
}

/// `CreateRoutingControl` in `panel_arn`, answering the control's ARN.
async fn create_routing_control(
    router: &MultiServiceRouter,
    cluster_arn: &str,
    panel_arn: &str,
    name: &str,
    case: &str,
) -> String {
    let (status, created) = config(
        router,
        "POST",
        "/routingcontrol",
        Some(json!({
            "ClusterArn": cluster_arn,
            "ControlPanelArn": panel_arn,
            "RoutingControlName": name,
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case}: expected CreateRoutingControl 200, got {status} with {created}"
    );
    created["RoutingControl"]["RoutingControlArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: a created control has an ARN, got {created}"))
        .to_string()
}

/// The `(Region, Endpoint)` pairs a `Cluster` body publishes, in the order it reports them.
fn entries_of(cluster: &Value) -> Vec<(String, String)> {
    cluster["ClusterEndpoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|entry| {
            (
                entry["Region"].as_str().unwrap_or_default().to_string(),
                entry["Endpoint"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// The `(Region, Endpoint)` pairs `DescribeCluster` publishes for `cluster_arn`, in the order
/// it reports them.
async fn published_endpoints(
    router: &MultiServiceRouter,
    cluster_arn: &str,
    case: &str,
) -> Vec<(String, String)> {
    let (status, body) = config(
        router,
        "GET",
        &format!("/cluster/{}", percent_encode(cluster_arn)),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case}: expected DescribeCluster 200, got {status} with {body}"
    );
    entries_of(&body["Cluster"])
}

/// `GetRoutingControlState` for `arn`, addressed at `endpoint`.
async fn state_at(
    router: &MultiServiceRouter,
    endpoint: &str,
    arn: &str,
    case: &str,
) -> String {
    let (status, body) = data_plane_at(
        router,
        endpoint,
        "GetRoutingControlState",
        &json!({ "RoutingControlArn": arn }),
        case,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case}: GetRoutingControlState at the published {endpoint} must answer 200, got \
         {status} with {body}"
    );
    body["RoutingControlState"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case}: a routing control state is required, got {body}")
        })
        .to_string()
}

/// `ListRoutingControls` for `panel_arn`, addressed at `endpoint`, as the state it reports
/// per routing control ARN.
async fn listed_states_at(
    router: &MultiServiceRouter,
    endpoint: &str,
    panel_arn: &str,
    case: &str,
) -> BTreeMap<String, String> {
    let (status, body) = data_plane_at(
        router,
        endpoint,
        "ListRoutingControls",
        &json!({ "ControlPanelArn": panel_arn }),
        case,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case}: ListRoutingControls at the published {endpoint} must answer 200, got \
         {status} with {body}"
    );
    body["RoutingControls"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("case {case}: RoutingControls is required, got {body}"))
        .iter()
        .map(|control| {
            (
                control["RoutingControlArn"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                control["RoutingControlState"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect()
}

/// Every endpoint a cluster publishes is one this emulator serves the routing-control data
/// plane on, and all of them address one state: the whole discovery path, driven at each of
/// the five Regional entries in turn.
#[tokio::test]
async fn every_published_cluster_endpoint_serves_the_routing_control_data_plane() {
    for base in BASES {
        let router = emulator_at(base);
        let case = format!("base {base}");

        let cluster_arn = create_cluster(&router, "arc-published-endpoints", &case).await;
        let panel_arn =
            create_control_panel(&router, &cluster_arn, "published-endpoints-panel", &case).await;
        let mut controls = Vec::new();
        for name in CONTROL_NAMES {
            controls
                .push(create_routing_control(&router, &cluster_arn, &panel_arn, name, &case).await);
        }

        // Step one of the documented path: ask the cluster WHERE its data plane is.
        let endpoints = published_endpoints(&router, &cluster_arn, &case).await;
        let regions: Vec<&str> = endpoints
            .iter()
            .map(|(region, _endpoint)| region.as_str())
            .collect();
        assert_eq!(
            regions, PUBLISHED_REGIONS,
            "case {case:?}: a cluster publishes one endpoint per supported Region, got \
             {endpoints:?}"
        );
        for (region, endpoint) in &endpoints {
            assert_eq!(
                endpoint, base,
                "case {case:?}: the endpoint published for {region} must be the address this \
                 emulator is reachable at, got {endpoint:?}"
            );
            // The part a client puts in the `Host` header of the call it sends there: a
            // published address is only reachable if THAT names this emulator.
            assert_eq!(
                authority_of(endpoint),
                authority_of(base),
                "case {case:?}: a request addressed at the endpoint published for {region} \
                 must carry this emulator's own host, got {endpoint:?}"
            );
        }

        // Step two: send the state calls THERE — to each published entry in turn, so no entry
        // is a label that answers nothing.
        let mut expected: BTreeMap<String, String> = controls
            .iter()
            .map(|arn| (arn.clone(), STATE_OFF.to_string()))
            .collect();

        for (index, (region, endpoint)) in endpoints.iter().enumerate() {
            let case = format!("{base} addressed as {region}");

            // What this entry reports before the write: the state every other entry left.
            let listed = listed_states_at(&router, endpoint, &panel_arn, &case).await;
            assert_eq!(
                listed, expected,
                "case {case:?}: ListRoutingControls at {endpoint} must report the state the \
                 cluster holds, got {listed:?}"
            );

            // The write, alternating between the single and the batch update so both reach
            // the data plane through a published address.
            if index % 2 == 0 {
                let arn = controls[index % controls.len()].clone();
                let desired = flipped(&expected[&arn]);
                let (status, body) = data_plane_at(
                    &router,
                    endpoint,
                    "UpdateRoutingControlState",
                    &json!({ "RoutingControlArn": arn, "RoutingControlState": desired }),
                    &case,
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: UpdateRoutingControlState at the published {endpoint} must \
                     answer 200, got {status} with {body}"
                );
                expected.insert(arn, desired.to_string());
            } else {
                let entries: Vec<Value> = controls
                    .iter()
                    .map(|arn| {
                        json!({
                            "RoutingControlArn": arn,
                            "RoutingControlState": flipped(&expected[arn]),
                        })
                    })
                    .collect();
                let (status, body) = data_plane_at(
                    &router,
                    endpoint,
                    "UpdateRoutingControlStates",
                    &json!({ "UpdateRoutingControlStateEntries": entries }),
                    &case,
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: UpdateRoutingControlStates at the published {endpoint} must \
                     answer 200, got {status} with {body}"
                );
                for arn in &controls {
                    let desired = flipped(&expected[arn]).to_string();
                    expected.insert(arn.clone(), desired);
                }
            }

            // The entries are redundant addresses of ONE cluster: what was toggled here is
            // what the NEXT published entry reports.
            let (next_region, next_endpoint) = &endpoints[(index + 1) % endpoints.len()];
            for arn in &controls {
                let read = state_at(&router, next_endpoint, arn, &case).await;
                assert_eq!(
                    read, expected[arn],
                    "case {case:?}: the state written at {endpoint} must be the state \
                     {next_endpoint} (published for {next_region}) reports for {arn}, got \
                     {read:?}"
                );
            }
        }

        // The other two operations that answer a `Cluster` publish the same addresses: each
        // takes the base from its own request's context, so neither can report one the data
        // plane does not answer on, and a client that discovered the cluster through a listing
        // or straight after a change is in the same position as one that described it.
        let (status, listed) = config(&router, "GET", "/cluster", None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected ListClusters 200, got {status} with {listed}"
        );
        let in_listing = listed["Clusters"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .find(|cluster| cluster["ClusterArn"].as_str() == Some(cluster_arn.as_str()))
            .unwrap_or_else(|| {
                panic!("case {case:?}: ListClusters must report the created cluster, got {listed}")
            });
        assert_eq!(
            entries_of(&in_listing),
            endpoints,
            "case {case:?}: ListClusters must publish the addresses DescribeCluster does, got \
             {in_listing}"
        );

        let (status, updated) = config(
            &router,
            "PUT",
            "/cluster",
            Some(json!({ "ClusterArn": cluster_arn, "NetworkType": "DUALSTACK" })),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected UpdateCluster 200, got {status} with {updated}"
        );
        let after_update = entries_of(&updated["Cluster"]);
        assert_eq!(
            after_update, endpoints,
            "case {case:?}: a cluster moved onto another network type is reachable at the same \
             address, got {updated}"
        );

        // And that address still serves the data plane: the state the toggles above left is
        // what it reports.
        for (region, endpoint) in &after_update {
            let case = format!("{base} addressed as {region} after UpdateCluster");
            for arn in &controls {
                let read = state_at(&router, endpoint, arn, &case).await;
                assert_eq!(
                    read, expected[arn],
                    "case {case:?}: the address published after UpdateCluster must serve the \
                     state the cluster holds for {arn}, got {read:?}"
                );
            }
        }
    }
}
