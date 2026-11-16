// SPDX-License-Identifier: CC0-1.0

//! The CloudWatch series a NAT GATEWAY publishes about the traffic it carries, driven end to
//! end with Amazon EC2 and CloudWatch on one engine.
//!
//! RULE under test: A NAT GATEWAY PUBLISHES THE `AWS/NATGateway` METRICS OF THE AMAZON VPC
//! CATALOG WHILE IT IS AVAILABLE — one data point per minute per metric, dimensioned on that
//! gateway's own id, carrying the unit that catalog gives the metric and the traffic the
//! gateway carried in the window — and a gateway that never becomes available publishes
//! nothing. So a client polling for a gateway's series gets an answer from the clock alone,
//! instead of reading an empty series for ever with no way to tell "no traffic yet" from
//! "nothing publishes this".
//!
//! Every case goes through the front door: `CreateNatGateway` / `DeleteNatGateway` over
//! EC2's `ec2Query` wire, and `ListMetrics` / `GetMetricStatistics` over CloudWatch's own
//! `awsJson1_0` wire. Nothing reaches into either store.
//!
//! The rows span what the rule generalises over: every metric of the catalog (the issue this
//! suite was written for cited two of the fourteen), gateways of both connectivity types, more
//! than one gateway at a time (so a series belongs to ONE gateway's id), a gateway no route
//! points at, and a gateway that is deleted before it ever becomes available.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ec2::state::{
    CONNECTIVITY_TYPE_PRIVATE, CONNECTIVITY_TYPE_PUBLIC, METRIC_NAMESPACE, NAT_GATEWAY_DIMENSION,
    NAT_GATEWAY_METRICS,
};
use serde_json::{json, Value};
use service_com_amazonaws_cloudwatch_graniteserviceversion20100801 as cloudwatch;
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element};
use super::wire::{Param, param};

/// The Region every call of this suite is signed for.
const REGION: &str = "us-east-1";

/// The Availability Zone its subnets are created in, which has to be one of [`REGION`]'s.
const ZONE: &str = "us-east-1a";

/// The `X-Amz-Target` prefix CloudWatch's JSON wire carries.
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";

/// CloudWatch's SigV4 signing name, which is what routes a call to it.
const CLOUDWATCH_SIGNING_NAME: &str = "monitoring";

/// How long a case looks for a gateway's first window, far longer than the span the clock owes
/// the gateway's own move to `available` — so a loaded machine fails nothing, while a series
/// that never appears still ends the test instead of hanging.
const PUBLISH_DEADLINE: Duration = Duration::from_secs(120);

/// How often it looks again, as a client's own waiter does.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The period a read lays over the series, in seconds: the minute these metrics are published
/// per.
const PERIOD_SECONDS: i64 = 60;

/// What one window of a gateway NOTHING sends through comes to, whichever metric of the
/// catalog reports it: a gateway carries only the traffic something sent through it, and no
/// guest of this emulator sends any — so every count of every window it publishes is zero. The
/// series EXISTS and reads 0, which is the answer the recording's own "stays below the block
/// threshold" phase reads.
const QUIET_WINDOW: f64 = 0.0;

/// One emulator: Amazon EC2 and CloudWatch mounted on ONE shared engine, the way the router
/// mounts every service — because the gateway is EC2's resource and the series it publishes
/// lives in CloudWatch's store.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Signatures are not what this suite is about; the router still reads the SigV4 credential
    // scope, which is where each call's service and Region come from.
    router.set_request_authentication(false);
    router.register_service(ec2::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(cloudwatch::EmulatedService::new(Arc::clone(&engine)));
    router
}

/// One EC2 `action` call that has to be served, and the body it answered.
async fn ec2_ok(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = call(router, REGION, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from EC2 {action}, got {status} with body {body}"
    );
    body
}

/// One CloudWatch call that has to be served, and the parsed body it answered.
async fn cloudwatch_ok(
    case: &str,
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("monitoring.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header(
            "x-amz-target",
            format!("{CLOUDWATCH_TARGET_PREFIX}.{operation}"),
        )
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{REGION}/\
                 {CLOUDWATCH_SIGNING_NAME}/aws4_request, SignedHeaders=host, Signature=0"
            ),
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let text = String::from_utf8_lossy(&payload).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CloudWatch {operation}, got {status} with body \
         {text}"
    );
    serde_json::from_slice(&payload).unwrap_or(Value::Null)
}

/// A VPC of `cidr_block` with one subnet of `subnet_cidr` in it, created over the wire, as
/// that subnet's id.
async fn subnet(
    case: &str,
    router: &MultiServiceRouter,
    cidr_block: &str,
    subnet_cidr: &str,
) -> String {
    let body = ec2_ok(case, router, "CreateVpc", &[param("CidrBlock", cidr_block)]).await;
    let vpc_id = element(&body, "vpcId")
        .unwrap_or_else(|| panic!("case {case:?}: CreateVpc must mint a VPC: {body}"));
    let body = ec2_ok(
        case,
        router,
        "CreateSubnet",
        &[
            param("VpcId", &vpc_id),
            param("CidrBlock", subnet_cidr),
            param("AvailabilityZone", ZONE),
        ],
    )
    .await;
    element(&body, "subnetId")
        .unwrap_or_else(|| panic!("case {case:?}: CreateSubnet must mint a subnet: {body}"))
}

/// One NAT gateway of `connectivity_type` in a VPC and subnet of its own, as its id.
async fn gateway(
    case: &str,
    router: &MultiServiceRouter,
    connectivity_type: &str,
    network: (&str, &str),
) -> String {
    let (cidr_block, subnet_cidr) = network;
    let subnet_id = subnet(case, router, cidr_block, subnet_cidr).await;
    let mut params: Vec<Param> = vec![
        param("SubnetId", &subnet_id),
        param("ConnectivityType", connectivity_type),
    ];
    // "[Public NAT gateways only] The allocation ID of an Elastic IP address to associate with
    // the NAT gateway" (`CreateNatGatewayRequest$AllocationId`).
    if connectivity_type == CONNECTIVITY_TYPE_PUBLIC {
        let body = ec2_ok(case, router, "AllocateAddress", &[]).await;
        let allocation_id = element(&body, "allocationId")
            .unwrap_or_else(|| panic!("case {case:?}: AllocateAddress must mint one: {body}"));
        params.push(param("AllocationId", &allocation_id));
    }
    let body = ec2_ok(case, router, "CreateNatGateway", &params).await;
    element(&body, "natGatewayId")
        .unwrap_or_else(|| panic!("case {case:?}: CreateNatGateway must mint a gateway: {body}"))
}

/// Now, as the epoch seconds a CloudWatch read windows by.
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |since| since.as_secs_f64())
}

/// The `(Sum, Unit, SampleCount)` of every data point `metric_name` has for `nat_gateway_id`,
/// read exactly as a client reads one: one `GetMetricStatistics` over a window around now.
async fn datapoints(
    case: &str,
    router: &MultiServiceRouter,
    nat_gateway_id: &str,
    metric_name: &str,
) -> Vec<(f64, String, f64)> {
    let now = now_secs();
    let answered = cloudwatch_ok(
        case,
        router,
        "GetMetricStatistics",
        &json!({
            "Namespace": METRIC_NAMESPACE,
            "MetricName": metric_name,
            "Dimensions": [{"Name": NAT_GATEWAY_DIMENSION, "Value": nat_gateway_id}],
            // An hour back and a minute on, so every window the clock can have published by
            // now is inside the read whatever minute it started in.
            "StartTime": now - 3_600.0,
            "EndTime": now + 60.0,
            "Period": PERIOD_SECONDS,
            "Statistics": ["Sum", "SampleCount"],
        }),
    )
    .await;
    answered["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|point| {
            (
                point["Sum"].as_f64().unwrap_or_default(),
                point["Unit"].as_str().unwrap_or_default().to_string(),
                point["SampleCount"].as_f64().unwrap_or_default(),
            )
        })
        .collect()
}

/// Look at `metric_name`'s series for `nat_gateway_id` until it has a data point, up to a
/// deadline far longer than the span the gateway's own move to `available` is owed after.
/// Nothing but the clock publishes one.
async fn published(
    case: &str,
    router: &MultiServiceRouter,
    nat_gateway_id: &str,
    metric_name: &str,
) -> Vec<(f64, String, f64)> {
    let deadline = Instant::now() + PUBLISH_DEADLINE;
    loop {
        let points = datapoints(case, router, nat_gateway_id, metric_name).await;
        if !points.is_empty() {
            return points;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {metric_name} of {nat_gateway_id} to be published with \
             nothing but the clock running, the series is still empty"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE: every metric of the Amazon VPC catalog is published for an available gateway, under
/// that gateway's own id, with the unit the catalog gives it and the traffic the gateway
/// carried in the window.
#[tokio::test]
async fn an_available_gateway_publishes_every_metric_of_the_catalog() {
    let case = "the catalog table";
    let router = emulator();
    let nat_gateway_id = gateway(
        case,
        &router,
        CONNECTIVITY_TYPE_PUBLIC,
        ("10.80.0.0/16", "10.80.1.0/24"),
    )
    .await;

    for metric in NAT_GATEWAY_METRICS {
        let row = format!("{case}: {}", metric.name);
        let points = published(&row, &router, &nat_gateway_id, metric.name).await;
        for (sum, unit, sample_count) in &points {
            assert_eq!(
                *sum, QUIET_WINDOW,
                "case {row:?}: expected the traffic the gateway carried ({QUIET_WINDOW}), got \
                 {sum} in {points:?}"
            );
            assert_eq!(
                unit, metric.unit,
                "case {row:?}: expected the unit {:?} the catalog gives this metric, got \
                 {unit:?} in {points:?}",
                metric.unit
            );
            assert!(
                *sample_count >= 1.0,
                "case {row:?}: a published window is one observation, got a sample count of \
                 {sample_count} in {points:?}"
            );
        }
    }

    // And the series are REPORTED as the gateway's own: `ListMetrics` names each of them in
    // the namespace, dimensioned on exactly this gateway.
    let listed = cloudwatch_ok(
        case,
        &router,
        "ListMetrics",
        &json!({
            "Namespace": METRIC_NAMESPACE,
            "Dimensions": [{"Name": NAT_GATEWAY_DIMENSION, "Value": nat_gateway_id}],
        }),
    )
    .await;
    let mut named: Vec<String> = listed["Metrics"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|metric| metric["MetricName"].as_str().unwrap_or_default().to_string())
        .collect();
    named.sort();
    named.dedup();
    let mut expected: Vec<String> = NAT_GATEWAY_METRICS
        .iter()
        .map(|metric| metric.name.to_string())
        .collect();
    expected.sort();
    assert_eq!(
        named, expected,
        "case {case:?}: expected ListMetrics to name every metric of the catalog for \
         {nat_gateway_id}, got {listed}"
    );
}

/// One row of the gateway table: which gateway publishes, and in what network.
struct GatewayCase {
    case: &'static str,
    /// The `ConnectivityType` the gateway is created with.
    connectivity_type: &'static str,
    /// The VPC's block and the subnet's block it is created in.
    network: (&'static str, &'static str),
    /// Whether a route of the VPC's main route table points at it — a gateway's series is the
    /// gateway's own, so this is an axis the rule does NOT turn on.
    routed: bool,
}

/// The rows: both connectivity types, and a gateway nothing routes to beside one a private
/// subnet's route points at.
const GATEWAY_CASES: &[GatewayCase] = &[
    GatewayCase {
        case: "a public gateway a route points at",
        connectivity_type: CONNECTIVITY_TYPE_PUBLIC,
        network: ("10.81.0.0/16", "10.81.1.0/24"),
        routed: true,
    },
    GatewayCase {
        case: "a public gateway no route points at",
        connectivity_type: CONNECTIVITY_TYPE_PUBLIC,
        network: ("10.82.0.0/16", "10.82.1.0/24"),
        routed: false,
    },
    GatewayCase {
        case: "a private gateway",
        connectivity_type: CONNECTIVITY_TYPE_PRIVATE,
        network: ("10.83.0.0/16", "10.83.1.0/24"),
        routed: false,
    },
];

/// The metric the issue behind this suite reads, which every row asserts the series of: "the
/// number of bytes sent out through the NAT gateway to the destination" (Amazon VPC User
/// Guide, "Monitor NAT gateways with Amazon CloudWatch").
const BYTES_OUT_TO_DESTINATION: &str = "BytesOutToDestination";

/// RULE, over the gateways it generalises over: each publishes its OWN series, whatever its
/// connectivity type and whether or not a route points at it — and no gateway's series carries
/// another's traffic.
#[tokio::test]
async fn every_gateway_publishes_its_own_series() {
    let router = emulator();
    let mut created: Vec<(&str, String)> = Vec::new();
    for row in GATEWAY_CASES {
        let nat_gateway_id = gateway(row.case, &router, row.connectivity_type, row.network).await;
        if row.routed {
            // A private subnet's route to the gateway, which is what sends that subnet's
            // traffic through it.
            let body = ec2_ok(
                row.case,
                &router,
                "DescribeNatGateways",
                &[param("NatGatewayId.1", &nat_gateway_id)],
            )
            .await;
            let vpc_id = element(&body, "vpcId").unwrap_or_else(|| {
                panic!("case {:?}: the gateway must report its VPC: {body}", row.case)
            });
            let body = ec2_ok(
                row.case,
                &router,
                "CreateRouteTable",
                &[param("VpcId", &vpc_id)],
            )
            .await;
            let route_table_id = element(&body, "routeTableId").unwrap_or_else(|| {
                panic!("case {:?}: CreateRouteTable must mint one: {body}", row.case)
            });
            ec2_ok(
                row.case,
                &router,
                "CreateRoute",
                &[
                    param("RouteTableId", &route_table_id),
                    param("DestinationCidrBlock", "0.0.0.0/0"),
                    param("NatGatewayId", &nat_gateway_id),
                ],
            )
            .await;
        }
        created.push((row.case, nat_gateway_id));
    }

    assert!(
        NAT_GATEWAY_METRICS
            .iter()
            .any(|metric| metric.name == BYTES_OUT_TO_DESTINATION),
        "the catalog must carry the metric this suite reads"
    );
    for (case, nat_gateway_id) in &created {
        let points = published(case, &router, nat_gateway_id, BYTES_OUT_TO_DESTINATION).await;
        for (sum, _, _) in &points {
            assert_eq!(
                *sum, QUIET_WINDOW,
                "case {case:?}: expected {BYTES_OUT_TO_DESTINATION} of {nat_gateway_id} to \
                 report the traffic it carried ({QUIET_WINDOW}), got {sum} in {points:?}"
            );
        }
    }
}

/// RULE, the half that withholds a series: a gateway that never becomes available carries
/// nothing and publishes nothing — so an empty series is a fact about the gateway rather than
/// about the publisher.
///
/// Race-free without a timing assumption: a second gateway created in the same instant is left
/// alone, and the absence is only asserted once THAT one has published — which is what shows
/// the publisher ran at all.
#[tokio::test]
async fn a_gateway_deleted_before_it_comes_up_publishes_nothing() {
    let case = "a gateway deleted before it comes up";
    let router = emulator();
    let control = gateway(
        case,
        &router,
        CONNECTIVITY_TYPE_PUBLIC,
        ("10.84.0.0/16", "10.84.1.0/24"),
    )
    .await;
    let deleted = gateway(
        case,
        &router,
        CONNECTIVITY_TYPE_PUBLIC,
        ("10.85.0.0/16", "10.85.1.0/24"),
    )
    .await;
    // Straight away, while the gateway is still `pending`: it never reaches `available`, so no
    // window of it is ever owed.
    ec2_ok(
        case,
        &router,
        "DeleteNatGateway",
        &[param("NatGatewayId", &deleted)],
    )
    .await;

    let control_points = published(case, &router, &control, BYTES_OUT_TO_DESTINATION).await;
    assert!(
        !control_points.is_empty(),
        "case {case:?}: the control gateway must publish, got {control_points:?}"
    );
    for metric in NAT_GATEWAY_METRICS {
        let points = datapoints(case, &router, &deleted, metric.name).await;
        assert_eq!(
            points,
            Vec::new(),
            "case {case:?}: expected no {} data point for the gateway that never became \
             available, got {points:?}",
            metric.name
        );
    }
}
