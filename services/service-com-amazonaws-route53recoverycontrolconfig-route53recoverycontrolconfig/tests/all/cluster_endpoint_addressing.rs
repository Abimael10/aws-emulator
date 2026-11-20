// SPDX-License-Identifier: CC0-1.0

//! WHERE a cluster publishes its data plane, and in WHICH ORDER, end to end over HTTP.
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
//! The second rule, the one the ORDER suite below states: a cluster publishes the entry
//! for its OWN Region — the Region it was created in and is addressed on — first, and the
//! remaining supported Regions after it. The order belongs to the cluster, so every
//! operation that projects a `Cluster` answers the same one whichever Region the read
//! itself is addressed to.
//!
//! The table spans what the rule generalises over: the bases an emulator reports (the
//! unserved default, a loopback address with a port, a DNS name with a port, an IPv6
//! literal), every `NetworkType` the model declares plus the request that names none, and
//! more than one cluster per emulator. A published address that named a fixed host — an
//! AWS one above all — would pass against at most one row.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::common::{call, call_in_region, create_cluster, enc, router_at, REGION};
use super::recovery_control_lifecycle::published_endpoint_regions;

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

/// What a cluster reachable at `base`, created in `home_region`, must publish: one entry
/// per supported Region label, each naming that one base, led by the entry for the
/// cluster's own Region.
fn expected(base: &str, home_region: &str) -> Vec<(String, String)> {
    published_endpoint_regions(home_region)
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
/// `home_region` is the Region the cluster was created in, whose entry leads.
fn check(cluster: &Value, base: &str, home_region: &str, case: &str, operation: &str) {
    let entries = published(cluster);
    assert_eq!(
        entries,
        expected(base, home_region),
        "case {case:?}: {operation} must publish the base this emulator is reachable at \
         ({base}) under each Region label, the cluster's own Region ({home_region}) \
         first, got {entries:?}"
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
            check(&created, base, REGION, &case, "CreateCluster");
            let arn = created["ClusterArn"]
                .as_str()
                .unwrap_or_default()
                .to_string();

            // Read back through the implemented reader: a create that answered one
            // address and stored another would publish two.
            let read = describe(&router, &arn, &case).await;
            check(&read, base, REGION, &case, "DescribeCluster");

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
            check(cluster, base, REGION, &case, "ListClusters");
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
            check(&answer["Cluster"], base, REGION, &case, "UpdateCluster");
            check(
                &describe(&router, arn, &case).await,
                base,
                REGION,
                &case,
                "DescribeCluster after UpdateCluster",
            );
        }
    }
}

/// The Regions a cluster is created in, row by row.
///
/// The first five are the supported Regions a cluster publishes an endpoint in (source:
/// `super::recovery_control_lifecycle::ENDPOINT_REGIONS`, from `CreateCluster`'s
/// documented "one for each supported Amazon Web Services Region") — among them the
/// alphabetically first and the alphabetically last, so no constant order passes every
/// row. The last row is a Region outside that set, which publishes no endpoint of its own:
/// the five keep their own order then, since there is nothing of the cluster's to lead
/// with.
const HOME_REGION_CASES: &[&str] = &[
    "us-west-2",
    "us-east-1",
    "ap-northeast-1",
    "ap-southeast-2",
    "eu-west-1",
    "eu-central-1",
];

/// A Region to address a READ to that is never the Region the cluster was created in, so
/// an order taken from the asking request rather than from the cluster cannot pass.
fn another_region(home_region: &str) -> &'static str {
    if home_region == "ap-northeast-1" {
        "eu-west-1"
    } else {
        "ap-northeast-1"
    }
}

/// The Region labels of a `Cluster` body, in the order it publishes them.
fn published_regions(cluster: &Value) -> Vec<String> {
    published(cluster)
        .into_iter()
        .map(|(region, _endpoint)| region)
        .collect()
}

/// A cluster leads its published endpoints with the entry for its OWN Region, and that
/// order is the cluster's: every operation that answers a `Cluster` reports it, whichever
/// Region the call is addressed to.
#[tokio::test]
async fn a_cluster_publishes_its_own_regions_endpoint_first() {
    // One base is enough here: WHERE each entry points is the suite above; this suite is
    // about the order the entries come in.
    let base = smithy_server_runtime::UNSERVED_URL_BASE;

    for home_region in HOME_REGION_CASES {
        let router = router_at(base);
        let elsewhere = another_region(home_region);
        let case = format!("cluster created in {home_region}, read from {elsewhere}");
        let expected_regions: Vec<String> = published_endpoint_regions(home_region)
            .iter()
            .map(|region| (*region).to_string())
            .collect();

        let (status, body) = call_in_region(
            &router,
            home_region,
            "POST",
            "/cluster",
            Some(json!({ "ClusterName": format!("order-{home_region}") })),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateCluster 200, got {status} {body:?}"
        );
        let created = body["Cluster"].clone();
        let arn = created["ClusterArn"]
            .as_str()
            .unwrap_or_default()
            .to_string();

        // The create answers the stored record, so it publishes the stored order too.
        check(&created, base, home_region, &case, "CreateCluster");
        let regions = published_regions(&created);
        assert_eq!(
            regions, expected_regions,
            "case {case:?}: CreateCluster must publish the cluster's own Region first, \
             got {regions:?}"
        );

        // Each read the service serves, addressed to a Region the cluster was NOT created
        // in: the order is the cluster's, not the request's.
        let reads: &[(&str, &str, Option<Value>)] = &[
            ("DescribeCluster", "GET", None),
            ("ListClusters", "GET", None),
            (
                "UpdateCluster",
                "PUT",
                Some(json!({ "ClusterArn": arn, "NetworkType": "DUALSTACK" })),
            ),
        ];
        for (operation, method, request_body) in reads {
            let path = match *operation {
                "DescribeCluster" => format!("/cluster/{}", enc(&arn)),
                _ => "/cluster".to_string(),
            };
            let (status, body) =
                call_in_region(&router, elsewhere, method, &path, request_body.clone()).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected {operation} 200, got {status} {body:?}"
            );
            let answered = match *operation {
                "ListClusters" => body["Clusters"]
                    .as_array()
                    .and_then(|clusters| clusters.first().cloned())
                    .unwrap_or(Value::Null),
                _ => body["Cluster"].clone(),
            };
            check(&answered, base, home_region, &case, operation);
            let regions = published_regions(&answered);
            assert_eq!(
                regions, expected_regions,
                "case {case:?}: {operation} must publish the cluster's own Region first, \
                 got {regions:?}"
            );
        }
    }
}
