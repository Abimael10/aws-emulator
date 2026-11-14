// SPDX-License-Identifier: CC0-1.0

//! WHERE a cluster publishes its data plane, end to end over HTTP.
//!
//! The rule under test: `ClusterEndpoint$Endpoint` is not a label a client reads but the
//! address it SENDS its next call to ("Specify an endpoint and Amazon Web Services Region
//! when you want to set or retrieve a routing control state in the cluster",
//! `smithy_query(query="doc",shape="ClusterEndpoint",member="Endpoint")`), and the cluster
//! data plane has no generic regional endpoint to fall back on — so every entry a cluster
//! publishes is the base URL THIS emulator is reachable at, under each of the Region
//! labels a cluster is reachable by, on `CreateCluster`, `DescribeCluster`,
//! `ListClusters` and `UpdateCluster` alike.
//!
//! The table spans what the rule generalises over: the bases an emulator reports (the
//! unserved default, a loopback address with a port, a DNS name with a port, an IPv6
//! literal), every `NetworkType` the model declares plus the request that names none, and
//! more than one cluster per emulator. A published address that named a fixed host — an
//! AWS one above all — would pass against at most one row.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::common::{call, create_cluster, enc, router_at};
use super::recovery_control_lifecycle::ENDPOINT_REGIONS;

/// One base URL an emulator can be reachable at, as
/// `smithy_server_runtime::OperationContext::endpoint` reports it to a handler.
struct BaseCase {
    /// The base itself. Every published endpoint must be exactly this.
    base: &'static str,
    /// Whether the base is written only with the characters the modelled
    /// `__stringMin1Max128PatternAZaZ09` admits (`@pattern ^[A-Za-z0-9:.\/_-]*$`, via
    /// `smithy_query(query="shape",shape="ClusterEndpoint")`).
    ///
    /// False for an IPv6 literal alone: RFC 3986 requires its brackets in an authority,
    /// so an emulator bound to one cannot be dialled by an address inside that pattern —
    /// the pattern describes the DNS names AWS itself vends. An address a client has to
    /// dial is published as the client must dial it.
    inside_modelled_pattern: bool,
}

/// The bases the rows below drive. `https://emulator.invalid` is the base of an emulator
/// that is served on no port (`smithy_server_runtime::UNSERVED_URL_BASE`); the others are
/// the forms a served data port publishes.
const BASE_CASES: &[BaseCase] = &[
    BaseCase {
        base: smithy_server_runtime::UNSERVED_URL_BASE,
        inside_modelled_pattern: true,
    },
    BaseCase {
        base: "https://127.0.0.1:39301",
        inside_modelled_pattern: true,
    },
    BaseCase {
        base: "https://recovery-control.test.invalid:9297",
        inside_modelled_pattern: true,
    },
    BaseCase {
        base: "https://[::1]:9297",
        inside_modelled_pattern: false,
    },
];

/// The `NetworkType` values the model declares
/// (`smithy_query(query="shape",shape="NetworkType")` -> `IPV4 | DUALSTACK`), and the
/// request that names none, which the model gives no default.
const NETWORK_TYPE_CASES: &[Option<&str>] = &[None, Some("IPV4"), Some("DUALSTACK")];

/// The modelled length of `ClusterEndpoint$Endpoint`: `@length 1..128`.
const ENDPOINT_MAX_LENGTH: usize = 128;

/// Every `(Region, Endpoint)` pair of a `Cluster` body, in the order it reports them.
fn published(cluster: &Value) -> Vec<(String, String)> {
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

/// What a cluster reachable at `base` must publish: one entry per supported Region label,
/// each naming that one base.
fn expected(base: &str) -> Vec<(String, String)> {
    ENDPOINT_REGIONS
        .iter()
        .map(|region| ((*region).to_string(), base.to_string()))
        .collect()
}

/// Whether `value` is written only with the characters
/// `ClusterEndpoint$Endpoint`'s `@pattern ^[A-Za-z0-9:.\/_-]*$` admits.
fn inside_endpoint_pattern(value: &str) -> bool {
    value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '.' | '/' | '_' | '-'))
}

/// Check one `Cluster` body against the rule, naming `case` and the operation.
fn check(cluster: &Value, base: &str, case: &str, operation: &str) {
    let entries = published(cluster);
    assert_eq!(
        entries,
        expected(base),
        "case {case:?}: {operation} must publish the base this emulator is reachable at \
         ({base}) under each Region label, got {entries:?}"
    );
    for (region, endpoint) in &entries {
        assert!(
            !endpoint.is_empty() && endpoint.len() <= ENDPOINT_MAX_LENGTH,
            "case {case:?}: {operation}'s endpoint for {region} must stay inside the \
             modelled length 1..{ENDPOINT_MAX_LENGTH}, got {} characters ({endpoint:?})",
            endpoint.len()
        );
        assert!(
            !endpoint.contains("amazonaws.com") && !endpoint.contains("api.aws"),
            "case {case:?}: {operation}'s endpoint for {region} must not name a host AWS \
             serves, got {endpoint:?}"
        );
    }
}

/// `DescribeCluster` for `cluster_arn`, as the stored `Cluster` body.
async fn describe(router: &MultiServiceRouter, cluster_arn: &str, case: &str) -> Value {
    let (status, body) = call(router, "GET", &format!("/cluster/{}", enc(cluster_arn)), None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected DescribeCluster 200, got {status} {body:?}"
    );
    body["Cluster"].clone()
}

/// `ListClusters`, as the whole list it answers with.
async fn list(router: &MultiServiceRouter, case: &str) -> Vec<Value> {
    let (status, body) = call(router, "GET", "/cluster", None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected ListClusters 200, got {status} {body:?}"
    );
    body["Clusters"].as_array().cloned().unwrap_or_default()
}

/// Every cluster an emulator publishes names that emulator's own base — whichever base it
/// is reachable at, whatever network type the cluster is on, and through every operation
/// that answers a `Cluster`.
#[tokio::test]
async fn a_cluster_publishes_the_base_this_emulator_is_reachable_at() {
    for BaseCase {
        base,
        inside_modelled_pattern,
    } in BASE_CASES
    {
        let router = router_at(base);
        let mut created_arns = Vec::new();

        for network_type in NETWORK_TYPE_CASES {
            let case = format!("{base} with NetworkType {network_type:?}");
            let mut body = json!({
                "ClusterName": format!("endpoint-{}", network_type.unwrap_or("omitted"))
            });
            if let Some(network_type) = network_type {
                body["NetworkType"] = json!(network_type);
            }

            let created = create_cluster(&router, body).await;
            check(&created, base, &case, "CreateCluster");
            let arn = created["ClusterArn"]
                .as_str()
                .unwrap_or_default()
                .to_string();

            // Read back through the implemented reader: a create that answered one
            // address and stored another would publish two.
            let read = describe(&router, &arn, &case).await;
            check(&read, base, &case, "DescribeCluster");

            for (region, endpoint) in published(&read) {
                assert_eq!(
                    inside_endpoint_pattern(&endpoint),
                    *inside_modelled_pattern,
                    "case {case:?}: the endpoint for {region} must be written as a client \
                     dials it, got {endpoint:?}"
                );
            }

            created_arns.push(arn);
        }

        // Every cluster of this emulator, through the listing: the endpoints are a
        // projection, so a list cannot answer a different address from a describe.
        let case = format!("{base} listed");
        let listed = list(&router, &case).await;
        assert_eq!(
            listed.len(),
            NETWORK_TYPE_CASES.len(),
            "case {case:?}: expected every created cluster listed, got {listed:?}"
        );
        for cluster in &listed {
            check(cluster, base, &case, "ListClusters");
        }

        // An update moves the cluster onto the other network type; the address a client
        // dials is the emulator's either way.
        for arn in &created_arns {
            let case = format!("{base} updated to DUALSTACK");
            let (status, answer) = call(
                &router,
                "PUT",
                "/cluster",
                Some(json!({ "ClusterArn": arn, "NetworkType": "DUALSTACK" })),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected UpdateCluster 200, got {status} {answer:?}"
            );
            check(&answer["Cluster"], base, &case, "UpdateCluster");
            check(
                &describe(&router, arn, &case).await,
                base,
                &case,
                "DescribeCluster after UpdateCluster",
            );
        }
    }
}
