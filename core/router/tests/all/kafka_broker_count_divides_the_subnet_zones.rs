// SPDX-License-Identifier: CC0-1.0

//! Amazon MSK spreads a provisioned cluster's brokers evenly over the Availability
//! Zones its client subnets are in, and those zones are Amazon EC2's fact.
//!
//! RULE: the number of broker nodes a provisioned cluster is asked for — by a
//! create, and by an `UpdateBrokerCount` on a cluster that already exists — must be
//! a MULTIPLE of the number of Availability Zones the request's (or the cluster's)
//! `BrokerNodeGroupInfo$ClientSubnets` span, because every one of those zones gets
//! the same number of brokers. A count that does not divide by that many zones is
//! refused with the modelled `BadRequestException` (HTTP 400) carrying MSK's fixed
//! sentence for it, mints nothing and leaves an existing cluster exactly as it was;
//! a count that divides is accepted and the cluster holds it.
//!
//! The ZONES are what the rule counts, not the subnet ids: two subnets of one zone
//! span one zone, so a count that is odd is admitted over them and refused over two
//! subnets in two zones. That is why this suite lives beside the router — the VPC
//! and its subnets are minted by the real Amazon EC2 operations on the SAME engine
//! that serves Amazon MSK, so every zone the rule is applied over is one EC2 itself
//! handed out rather than one this suite made up.
//!
//! The rows differ along what the rule generalises over: the operation that names
//! the count (`CreateCluster`, `CreateClusterV2`'s provisioned body,
//! `UpdateBrokerCount`), how many zones the placement spans, how many subnets those
//! zones are named by, and the count asked for — one that divides, one that does
//! not, the published maximum, and one below the number of zones.
//!
//! Error source: `smithy_query(query="operation", operation="CreateCluster,
//! CreateClusterV2,UpdateBrokerCount")` on the kafka model — `BadRequestException`,
//! `errorKind` client, `httpError` 400.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// The region every request below is signed for.
const REGION: &str = "us-west-2";

/// The EC2 API version a client sends in an ec2Query body.
const EC2_API_VERSION: &str = "2016-11-15";

/// The Apache Kafka version and broker type every create below asks for: a
/// version Amazon MSK still takes for a NEW cluster, since each create has to be
/// admitted.
const KAFKA_VERSION: &str = "3.9.x";
const INSTANCE_TYPE: &str = "kafka.m5.large";

/// The largest number of broker nodes one cluster may hold, from the `range` on
/// `CreateClusterRequest$NumberOfBrokerNodes` (`__integerMin1Max15`).
const MAX_BROKERS: u32 = 15;

/// What Amazon MSK answers a broker count that does not divide by the number of
/// Availability Zones the client subnets span: entirely fixed text, including the
/// example it ends with, so no value of the request appears in it. Written out here
/// rather than read from the service's own constant, so the wording is pinned by
/// this suite instead of by the code under test.
const ZONE_MULTIPLE_MESSAGE: &str =
    "The target number of broker nodes must be a multiple of the number of Availability Zones \
     in the Client subnets parameter. For example, for a cluster with subnets in 2 Availability \
     Zones, the target number of brokers must be a multiple of 2.";

/// How often a wait below looks again, and how many looks it takes before it gives
/// up: a GENEROUS budget, since what is waited for is a transition the cluster's own
/// record already owes and the only thing a longer wait costs is a slow machine's
/// patience.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);
/// See [`POLL_INTERVAL`]: 600 looks at that interval is half a minute of looking.
const POLLS: usize = 600;

/// One emulator carrying Amazon EC2 and Amazon MSK over ONE shared engine, as the
/// production router mounts them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_kafka_kafka::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`, which
/// is what routes the request here; the signature is never checked.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One answer: its HTTP status and its body as JSON (`Value::Null` when it is not
/// JSON).
struct Reply {
    status: u16,
    body: Value,
}

impl Reply {
    /// The wire error code (`__type` of the restJson1 error envelope), or `""` on
    /// success.
    fn error_code(&self) -> &str {
        self.body
            .get("__type")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }

    /// The error envelope's message, or `""` when there is none.
    fn message(&self) -> &str {
        self.body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }

    /// The text at `pointer`, or `""` when the answer carries nothing there.
    fn at(&self, pointer: &str) -> &str {
        self.body
            .pointer(pointer)
            .and_then(Value::as_str)
            .unwrap_or_default()
    }
}

/// Call one Amazon MSK operation over its restJson1 wire.
async fn kafka(router: &MultiServiceRouter, method: &str, uri: &str, body: Option<Value>) -> Reply {
    let payload = body
        .map(|value| serde_json::to_vec(&value).expect("serialize the request body"))
        .unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("kafka.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("kafka"))
        .body(Bytes::from(payload))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    Reply { status, body }
}

/// One ec2Query action that must succeed, answered with its XML body.
async fn ec2(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let mut body = format!("Action={action}&Version={EC2_API_VERSION}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ec2.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("ec2"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "setup: EC2 {action} must succeed, got {status} with {text}"
    );
    text
}

/// The network every case runs over: four subnets of one VPC, minted by EC2 itself —
/// three in three DIFFERENT Availability Zones, and a fourth sharing the first
/// one's zone, so a list of two subnets can span either two zones or one.
async fn ec2_subnets(router: &MultiServiceRouter) -> Vec<String> {
    let vpc = element(
        &ec2(router, "CreateVpc", &[("CidrBlock", "10.90.0.0/16")]).await,
        "vpcId",
    );
    let mut subnets = Vec::new();
    for (cidr, zone) in [
        ("10.90.1.0/24", "us-west-2a"),
        ("10.90.2.0/24", "us-west-2b"),
        ("10.90.3.0/24", "us-west-2c"),
        ("10.90.4.0/24", "us-west-2a"),
    ] {
        subnets.push(element(
            &ec2(
                router,
                "CreateSubnet",
                &[
                    ("VpcId", vpc.as_str()),
                    ("CidrBlock", cidr),
                    ("AvailabilityZone", zone),
                ],
            )
            .await,
            "subnetId",
        ));
    }
    subnets
}

/// A `CreateCluster` body placing `brokers` brokers in `subnets`.
fn v1_body(name: &str, brokers: u32, subnets: &[String]) -> Value {
    json!({
        "clusterName": name,
        "kafkaVersion": KAFKA_VERSION,
        "numberOfBrokerNodes": brokers,
        "brokerNodeGroupInfo": {
            "instanceType": INSTANCE_TYPE,
            "clientSubnets": subnets,
        },
    })
}

/// A `CreateClusterV2` body whose `Provisioned` block does the same.
fn v2_body(name: &str, brokers: u32, subnets: &[String]) -> Value {
    json!({
        "clusterName": name,
        "provisioned": {
            "kafkaVersion": KAFKA_VERSION,
            "numberOfBrokerNodes": brokers,
            "brokerNodeGroupInfo": {
                "instanceType": INSTANCE_TYPE,
                "clientSubnets": subnets,
            },
        },
    })
}

/// Every cluster name `ListClustersV2` reports, over every page.
async fn listed_cluster_names(router: &MultiServiceRouter) -> Vec<String> {
    let mut names = Vec::new();
    let mut uri = "/api/v2/clusters".to_string();
    loop {
        let reply = kafka(router, "GET", &uri, None).await;
        assert_eq!(
            reply.status, 200,
            "a cluster listing must answer 200, got {} with {}",
            reply.status, reply.body
        );
        for cluster in reply
            .body
            .get("clusterInfoList")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            if let Some(name) = cluster.get("clusterName").and_then(Value::as_str) {
                names.push(name.to_string());
            }
        }
        match reply.body.get("nextToken").and_then(Value::as_str) {
            Some(token) => uri = format!("/api/v2/clusters?nextToken={}", form_encode(token)),
            None => break,
        }
    }
    names
}

/// The cluster `cluster_arn` names, read again until Amazon MSK reports it `ACTIVE` —
/// the state the clock owes it after the create — or the budget above runs out, and
/// answered as the LAST read either way so an assertion reports what MSK really
/// said.
///
/// Waits for the CONDITION rather than for a span: a broker count is updated only
/// from a settled cluster, and when that holds is the cluster's own business rather
/// than this suite's.
async fn cluster_once_it_is_active(router: &MultiServiceRouter, cluster_arn: &str) -> Reply {
    let uri = format!("/v1/clusters/{}", form_encode(cluster_arn));
    let mut last = kafka(router, "GET", &uri, None).await;
    for _ in 0..POLLS {
        // A read that is not answered at all is not a transient condition — the ARN
        // is the one the create just minted — so it is answered straight back.
        if last.status != 200 || last.at("/clusterInfo/state") == "ACTIVE" {
            return last;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
        last = kafka(router, "GET", &uri, None).await;
    }
    last
}

/// One row of the create table: the case, how many brokers it asks for, which of
/// EC2's subnets it places them in, how many zones those span, and whether MSK
/// admits the count.
struct CountRow {
    case: &'static str,
    brokers: u32,
    subnets: Vec<usize>,
    zones: usize,
    accepted: bool,
}

/// The table, over the network [`ec2_subnets`] mints: subnets 0, 1 and 2 are each in
/// their own Availability Zone, while subnet 3 shares subnet 0's.
fn count_rows() -> Vec<CountRow> {
    vec![
        CountRow {
            case: "one broker in one zone",
            brokers: 1,
            subnets: vec![0],
            zones: 1,
            accepted: true,
        },
        CountRow {
            case: "two brokers over two zones",
            brokers: 2,
            subnets: vec![0, 1],
            zones: 2,
            accepted: true,
        },
        CountRow {
            case: "three brokers over two zones",
            brokers: 3,
            subnets: vec![0, 1],
            zones: 2,
            accepted: false,
        },
        CountRow {
            case: "one broker over two zones",
            brokers: 1,
            subnets: vec![0, 1],
            zones: 2,
            accepted: false,
        },
        CountRow {
            case: "four brokers over two zones",
            brokers: 4,
            subnets: vec![0, 1],
            zones: 2,
            accepted: true,
        },
        CountRow {
            case: "three brokers over three zones",
            brokers: 3,
            subnets: vec![0, 1, 2],
            zones: 3,
            accepted: true,
        },
        CountRow {
            case: "five brokers over three zones",
            brokers: 5,
            subnets: vec![0, 1, 2],
            zones: 3,
            accepted: false,
        },
        CountRow {
            case: "the published maximum over three zones",
            brokers: MAX_BROKERS,
            subnets: vec![0, 1, 2],
            zones: 3,
            accepted: true,
        },
        // The rule counts ZONES, not subnet ids: these two subnets are both in the
        // first zone, so an odd count spreads over them evenly.
        CountRow {
            case: "three brokers over two subnets of ONE zone",
            brokers: 3,
            subnets: vec![0, 3],
            zones: 1,
            accepted: true,
        },
    ]
}

/// RULE (the create half): a provisioned create's `NumberOfBrokerNodes` must be a
/// multiple of the number of Availability Zones its `ClientSubnets` span, whichever
/// create names it; a count that is not is refused 400 `BadRequestException` with
/// MSK's fixed sentence and registers no cluster, and a count that is lands a
/// cluster holding exactly it.
#[tokio::test]
async fn a_create_admits_only_a_broker_count_the_subnet_zones_divide() {
    let router = emulator();
    let subnets = ec2_subnets(&router).await;

    for (index, row) in count_rows().into_iter().enumerate() {
        let placed: Vec<String> = row
            .subnets
            .iter()
            .map(|subnet| subnets[*subnet].clone())
            .collect();
        for (operation, uri, name, body) in [
            (
                "CreateCluster",
                "/v1/clusters",
                format!("zones-v1-{index}"),
                v1_body(&format!("zones-v1-{index}"), row.brokers, &placed),
            ),
            (
                "CreateClusterV2",
                "/api/v2/clusters",
                format!("zones-v2-{index}"),
                v2_body(&format!("zones-v2-{index}"), row.brokers, &placed),
            ),
        ] {
            let case = format!(
                "{operation}: {} ({} broker(s) over {} zone(s) named by {} subnet(s))",
                row.case,
                row.brokers,
                row.zones,
                placed.len()
            );
            let reply = kafka(&router, "POST", uri, Some(body)).await;
            if row.accepted {
                assert_eq!(
                    reply.status, 200,
                    "case {case}: expected the create to be accepted, got {} with {}",
                    reply.status, reply.body
                );
                let cluster_arn = reply
                    .body
                    .get("clusterArn")
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| panic!("case {case}: no clusterArn in {}", reply.body))
                    .to_string();
                let settled = cluster_once_it_is_active(&router, &cluster_arn).await;
                assert_eq!(
                    settled
                        .body
                        .pointer("/clusterInfo/numberOfBrokerNodes")
                        .and_then(Value::as_u64),
                    Some(u64::from(row.brokers)),
                    "case {case}: the cluster must hold the brokers it was created with, got {}",
                    settled.body
                );
                continue;
            }

            assert_eq!(
                (reply.status, reply.error_code(), reply.message()),
                (400, "BadRequestException", ZONE_MULTIPLE_MESSAGE),
                "case {case}: expected the zone-multiple refusal, got {} with {}",
                reply.status,
                reply.body
            );
            let listed = listed_cluster_names(&router).await;
            assert!(
                !listed.contains(&name),
                "case {case}: a refused create must register no cluster, got {listed:?}"
            );
        }
    }
}

/// RULE (the update half): the same rule holds for the target an
/// `UpdateBrokerCount` asks for — the zones are the ones the CLUSTER's own client
/// subnets span — so a target those zones do not divide is refused with the same
/// fixed sentence and the cluster keeps its brokers, its state and its version,
/// while one they do divide is accepted and lands.
#[tokio::test]
async fn an_update_admits_only_a_target_the_clusters_subnet_zones_divide() {
    let router = emulator();
    let subnets = ec2_subnets(&router).await;
    // Two brokers over two of EC2's zones: every odd target is then refused and
    // every even one admitted, whichever way the count moves.
    let placed = vec![subnets[0].clone(), subnets[1].clone()];
    let created = kafka(
        &router,
        "POST",
        "/v1/clusters",
        Some(v1_body("zoned-update", 2, &placed)),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "setup: a create over EC2's own subnets must be accepted, got {} with {}",
        created.status, created.body
    );
    let cluster_arn = created
        .body
        .get("clusterArn")
        .and_then(Value::as_str)
        .expect("the create reports a clusterArn")
        .to_string();
    let settled = cluster_once_it_is_active(&router, &cluster_arn).await;
    assert_eq!(
        settled.at("/clusterInfo/state"),
        "ACTIVE",
        "setup: expected the new cluster to settle ACTIVE, got {}",
        settled.body
    );
    let version = settled.at("/clusterInfo/currentVersion").to_string();
    let uri = format!("/v1/clusters/{}/nodes/count", form_encode(&cluster_arn));

    // The rows differ along the size of the target the two zones do not divide: one
    // above the cluster's count, further above it, and the published maximum.
    for (case, target) in [
        ("one above the cluster's count", 3),
        ("three above it", 5),
        ("the published maximum", MAX_BROKERS),
    ] {
        let reply = kafka(
            &router,
            "PUT",
            &uri,
            Some(json!({
                "currentVersion": version,
                "targetNumberOfBrokerNodes": target,
            })),
        )
        .await;
        assert_eq!(
            (reply.status, reply.error_code(), reply.message()),
            (400, "BadRequestException", ZONE_MULTIPLE_MESSAGE),
            "case \"a target of {target}, {case}\": expected the zone-multiple refusal, got {} \
             with {}",
            reply.status,
            reply.body
        );
        let after = kafka(
            &router,
            "GET",
            &format!("/v1/clusters/{}", form_encode(&cluster_arn)),
            None,
        )
        .await;
        assert_eq!(
            (
                after.at("/clusterInfo/state"),
                after
                    .body
                    .pointer("/clusterInfo/numberOfBrokerNodes")
                    .and_then(Value::as_u64),
                after.at("/clusterInfo/currentVersion"),
            ),
            (("ACTIVE"), Some(2), version.as_str()),
            "case \"a target of {target}, {case}\": a refused update must leave the cluster \
             untouched, got {}",
            after.body
        );
    }

    // The control: a target those same two zones DO divide is accepted and lands.
    let accepted = kafka(
        &router,
        "PUT",
        &uri,
        Some(json!({
            "currentVersion": version,
            "targetNumberOfBrokerNodes": 4,
        })),
    )
    .await;
    assert_eq!(
        accepted.status, 200,
        "case \"a target the zones divide\": expected 200, got {} with {}",
        accepted.status, accepted.body
    );
    let settled = cluster_once_it_is_active(&router, &cluster_arn).await;
    assert_eq!(
        settled
            .body
            .pointer("/clusterInfo/numberOfBrokerNodes")
            .and_then(Value::as_u64),
        Some(4),
        "case \"a target the zones divide\": expected the update to land, got {}",
        settled.body
    );
}
