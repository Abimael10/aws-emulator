// SPDX-License-Identifier: CC0-1.0

//! The discovery path AWS documents, across both planes: a client creates a cluster on
//! the recovery-control CONFIG plane, reads the Regional endpoint that cluster publishes,
//! and sends its routing-control state calls THERE.
//!
//! The rule under test: the address a cluster publishes in
//! `Cluster.ClusterEndpoints[].Endpoint` is one this emulator answers on, so a client
//! that dials it reaches this data plane — "Specify an endpoint and Amazon Web Services
//! Region when you want to set or retrieve a routing control state in the cluster", and
//! the cluster API publishes no generic regional endpoint a client could use instead. An
//! address naming a host outside this process would leave the emulator (or resolve
//! nowhere), and no routing control could be read or toggled through it at all.
//!
//! The rows span the bases an emulator is reachable at and every `NetworkType` the
//! config plane's model declares, and each row drives the whole sequence: discover, read
//! `Off`, toggle `On`, read back. The projection itself is asserted over more bases in
//! the config plane's own
//! `tests/all/cluster_endpoint_addressing.rs`.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::common::{
    authorization, control_plane, create_control_panel, create_routing_control, emulator_at,
    encode_arn, DATA_PLANE_SIGNING_NAME, REGION,
};

/// The bases these rows make the emulator reachable at: a loopback address with a port,
/// and a DNS name with a port — the forms a served data port publishes.
const BASES: &[&str] = &["https://127.0.0.1:39417", "https://arc.test.invalid:9297"];

/// The `NetworkType` values the config plane's model declares
/// (`smithy_query(query="shape",shape="NetworkType")` -> `IPV4 | DUALSTACK`), and the
/// create that names none.
const NETWORK_TYPES: &[Option<&str>] = &[None, Some("IPV4"), Some("DUALSTACK")];

/// Create a cluster on the config plane and answer its ARN.
async fn create_cluster(
    router: &MultiServiceRouter,
    name: &str,
    network_type: Option<&str>,
) -> String {
    let mut body = json!({ "ClusterName": name });
    if let Some(network_type) = network_type {
        body["NetworkType"] = json!(network_type);
    }
    let (status, created) = control_plane(router, "POST", "/cluster", Some(body)).await;
    assert_eq!(
        status, 200,
        "case {name}: expected CreateCluster 200, got {status} with {created}"
    );
    created["Cluster"]["ClusterArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {name}: a created cluster has an ARN, got {created}"))
        .to_string()
}

/// The endpoint `DescribeCluster` publishes for `region`, failing the case when the
/// cluster publishes none for it.
async fn published_endpoint(
    router: &MultiServiceRouter,
    cluster_arn: &str,
    region: &str,
    case: &str,
) -> String {
    let (status, body) = control_plane(
        router,
        "GET",
        &format!("/cluster/{}", encode_arn(cluster_arn)),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case}: expected DescribeCluster 200, got {status} with {body}"
    );
    let entries = body["Cluster"]["ClusterEndpoints"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let found = entries
        .iter()
        .find(|entry| entry["Region"].as_str() == Some(region));
    let endpoint = found
        .and_then(|entry| entry["Endpoint"].as_str())
        .unwrap_or_else(|| {
            panic!(
                "case {case}: the cluster must publish an endpoint for {region}, got \
                 {entries:?}"
            )
        });
    endpoint.to_string()
}

/// One data-plane call sent to `endpoint`, exactly as a client configured with that
/// published address sends it: the request is addressed to the host the endpoint names.
async fn call_at(
    router: &MultiServiceRouter,
    endpoint: &str,
    operation: &str,
    body: &Value,
    case: &str,
) -> (u16, Value) {
    let base = endpoint.trim_end_matches('/');
    let authority = base.split_once("://").map_or(base, |(_scheme, rest)| rest);
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("{base}/"))
        .header("host", authority)
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("ToggleCustomerAPI.{operation}"))
        .header("authorization", authorization(DATA_PLANE_SIGNING_NAME))
        .body(bytes::Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a valid request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            panic!(
                "case {case}: {operation} at {endpoint} answered a body that is not JSON \
                 ({error}): {:?}",
                String::from_utf8_lossy(&bytes)
            )
        })
    };
    (status, parsed)
}

/// A client that follows the documented discovery path reaches this data plane: the
/// endpoint the cluster publishes serves `GetRoutingControlState` and
/// `UpdateRoutingControlState` for the routing controls of that cluster.
#[tokio::test]
async fn the_published_cluster_endpoint_serves_the_routing_control_data_plane() {
    for base in BASES {
        let router = emulator_at(base);

        for network_type in NETWORK_TYPES {
            let label = network_type.unwrap_or("omitted");
            let case = format!("{base} with NetworkType {label}");

            let cluster_arn =
                create_cluster(&router, &format!("discovery-{label}"), *network_type).await;
            let panel =
                create_control_panel(&router, &cluster_arn, &format!("panel-{label}")).await;
            let control =
                create_routing_control(&router, &panel, &format!("control-{label}")).await;

            // Step one of the documented path: ask the cluster where its data plane is.
            let endpoint = published_endpoint(&router, &cluster_arn, REGION, &case).await;
            assert_eq!(
                endpoint, *base,
                "case {case}: the cluster must publish the address this emulator is \
                 reachable at, got {endpoint:?}"
            );

            // Step two: send the state calls there. A newly created routing control is
            // `Off`, and nothing but an update moves it.
            let (status, body) = call_at(
                &router,
                &endpoint,
                "GetRoutingControlState",
                &json!({ "RoutingControlArn": control.arn }),
                &case,
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case}: GetRoutingControlState at the published {endpoint} must \
                 answer 200, got {status} with {body}"
            );
            assert_eq!(
                body["RoutingControlState"].as_str(),
                Some("Off"),
                "case {case}: a freshly created routing control reads Off at {endpoint}, \
                 got {body}"
            );

            let (status, body) = call_at(
                &router,
                &endpoint,
                "UpdateRoutingControlState",
                &json!({ "RoutingControlArn": control.arn, "RoutingControlState": "On" }),
                &case,
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case}: UpdateRoutingControlState at the published {endpoint} must \
                 answer 200, got {status} with {body}"
            );

            // The address serves ONE store: the toggle sent there is the state read back
            // there.
            let (status, body) = call_at(
                &router,
                &endpoint,
                "GetRoutingControlState",
                &json!({ "RoutingControlArn": control.arn }),
                &case,
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case}: the read-back at {endpoint} must answer 200, got {status} \
                 with {body}"
            );
            assert_eq!(
                body["RoutingControlState"].as_str(),
                Some("On"),
                "case {case}: the state toggled at {endpoint} must be the state read back \
                 there, got {body}"
            );
        }
    }
}
