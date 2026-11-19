// SPDX-License-Identifier: CC0-1.0

//! What a `Workgroup` reports that the create did NOT supply: the release the workgroup runs
//! and its patch level, the track it is assigned to, its price-performance target, and the
//! managed VPC endpoint it is reached through.
//!
//! The rule under test: a member Redshift Serverless DERIVES server-side is reported for every
//! workgroup, from the serving service's own facts rather than from the request — so a create
//! that names none of them still reports a release version and a patch level, the current
//! track, the price-performance target's explicit off-state, and the VPC endpoint the account
//! holds for the workgroup's subnets. Every one of them is STORED, so `GetWorkgroup` and
//! `ListWorkgroups` report exactly what the create answered.
//!
//! A VPC, a subnet, a VPC endpoint and a network interface are Amazon EC2's resources, so the
//! endpoint cases build the network through that service's own API, mounted on the same engine
//! the assembled emulator mounts it on, and read the interfaces back from it: what the
//! workgroup reports has to be what the account actually holds.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::namespace_workgroup_lifecycle::{at, create_namespace};

/// The region every request of this suite is signed for — not the emulator default, so a value
/// read off a record of this Region proves it was not taken from a constant.
const REGION: &str = "us-west-2";

/// One emulator carrying Redshift Serverless AND Amazon EC2 on the same shared engine, exactly
/// as the assembled emulator mounts them.
fn emulator_with_ec2() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_redshiftserverless_redshiftserverless::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router
}

/// Call `operation` on this service, signed for [`REGION`], and answer its HTTP status and
/// parsed body.
async fn call(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("RedshiftServerless.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{REGION}/redshift-serverless/\
                 aws4_request, SignedHeaders=host, Signature=deadbeef"
            ),
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// [`call`] for an operation that must succeed.
async fn ok(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    let (status, parsed) = call(router, operation, body.clone()).await;
    assert_eq!(
        status, 200,
        "case {operation} {body}: expected 200, got {status} with {parsed}"
    );
    parsed
}

/// An Amazon EC2 client for [`REGION`] over this emulator's own in-process callback — the same
/// seam the handler asks EC2 for a workgroup's managed endpoint through.
fn ec2(router: &MultiServiceRouter) -> ::aws_sdk_ec2::Client {
    let callback = smithy_server_runtime::EmbeddedEmulator::from_router(router.clone());
    let config = ::aws_sdk_ec2::Config::builder()
        .behavior_version(::aws_sdk_ec2::config::BehaviorVersion::latest())
        .region(::aws_sdk_ec2::config::Region::new(REGION.to_string()))
        .endpoint_url(format!("https://ec2.{REGION}.amazonaws.com"))
        .credentials_provider(callback.aws_credentials_provider())
        .http_client(callback.http_client())
        .retry_config(::aws_sdk_ec2::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_ec2::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_ec2::config::IdentityCache::no_cache())
        .stalled_stream_protection(::aws_sdk_ec2::config::StalledStreamProtectionConfig::disabled())
        .build();
    ::aws_sdk_ec2::Client::from_conf(config)
}

/// One subnet of the network a case builds: its id, the zone it lies in and the address prefix
/// of its own range, which an interface placed in it must take an address out of.
#[derive(Debug, Clone)]
struct Subnet {
    id: String,
    availability_zone: String,
    address_prefix: String,
}

/// Build a VPC with one subnet in each of the first `zones` Availability Zones of [`REGION`],
/// through Amazon EC2's own API, and answer the VPC id and those subnets in order.
async fn network(ec2: &::aws_sdk_ec2::Client, zones: usize) -> (String, Vec<Subnet>) {
    let created = ec2
        .create_vpc()
        .cidr_block("10.0.0.0/16")
        .send()
        .await
        .expect("CreateVpc answers");
    let vpc_id = created
        .vpc()
        .and_then(::aws_sdk_ec2::types::Vpc::vpc_id)
        .expect("CreateVpc reports the VPC it minted")
        .to_string();
    let mut subnets: Vec<Subnet> = Vec::with_capacity(zones);
    for index in 0..zones {
        // The zones of a Region are named `<region>a`, `<region>b`, … — the names Amazon EC2
        // reports for them.
        let zone = format!(
            "{REGION}{letter}",
            letter = char::from(b'a' + u8::try_from(index).unwrap_or(0))
        );
        let address_prefix = format!("10.0.{index}.");
        let created = ec2
            .create_subnet()
            .vpc_id(&vpc_id)
            .cidr_block(format!("{address_prefix}0/24"))
            .availability_zone(&zone)
            .send()
            .await
            .expect("CreateSubnet answers");
        let id = created
            .subnet()
            .and_then(::aws_sdk_ec2::types::Subnet::subnet_id)
            .expect("CreateSubnet reports the subnet it minted")
            .to_string();
        subnets.push(Subnet {
            id,
            availability_zone: zone,
            address_prefix,
        });
    }
    (vpc_id, subnets)
}

/// Whether `value` is a full `<major>.<minor>.<build>` release version: three numeric segments.
fn is_release_version(value: &str) -> bool {
    let segments: Vec<&str> = value.split('.').collect();
    segments.len() == 3 && segments.iter().all(|segment| is_number(segment))
}

/// Whether `value` is a non-empty run of decimal digits.
fn is_number(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|digit| digit.is_ascii_digit())
}

/// The member `path` of a workgroup as every reader of it reports it: the create's answer, a
/// `GetWorkgroup` and the workgroup's entry in `ListWorkgroups`.
///
/// A derived member is STORED, so the three must agree; a case asserts on the value this
/// returns, which is that agreement.
async fn reported_by_every_reader(
    router: &MultiServiceRouter,
    case: &str,
    workgroup_name: &str,
    created: &Value,
    path: &str,
) -> Value {
    let read = ok(
        router,
        "GetWorkgroup",
        json!({ "workgroupName": workgroup_name }),
    )
    .await;
    let read = at(&read["workgroup"], path).clone();
    let created = at(created, path).clone();
    assert_eq!(
        read, created,
        "case {case:?}: GetWorkgroup must report what the create stored for {path}, \
         expected {created:?}, got {read:?}"
    );
    let listed = ok(router, "ListWorkgroups", json!({})).await;
    let listed = listed["workgroups"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|workgroup| workgroup["workgroupName"] == json!(workgroup_name))
        .unwrap_or_else(|| panic!("case {case:?}: ListWorkgroups must report {workgroup_name}"));
    let listed = at(&listed, path).clone();
    assert_eq!(
        listed, created,
        "case {case:?}: ListWorkgroups must report what the create stored for {path}, \
         expected {created:?}, got {listed:?}"
    );
    created
}

// ---------------------------------------------------------------------------
// The release, the patch level, the track and the price-performance target
// ---------------------------------------------------------------------------

/// One row: the case name, the extra members the create carries, and the members the workgroup
/// must then report.
type DerivedCase = (&'static str, Value, Vec<(&'static str, Value)>);

/// A create that names none of the server-derived members still reports every one of them, and
/// a create that names one reports what it named. The rows differ along what the rule
/// generalises over: whether the member was supplied, and the value and form it was supplied
/// in.
#[tokio::test]
async fn a_workgroup_reports_its_release_track_and_price_performance_target() {
    let router = emulator_with_ec2();
    // `pricePerformanceTarget` levels from `PerformanceTarget$level`'s own documentation
    // ("valid values include 1, 25, 50, 75, and 100") and its `@range(min: 1, max: 100)`;
    // the track names from `CreateWorkgroup`'s `trackName` doc and the `TrackName` pattern.
    let rows: [DerivedCase; 6] = [
        (
            "no server-derived member named",
            json!({}),
            vec![
                ("trackName", json!("current")),
                ("pricePerformanceTarget", json!({ "status": "DISABLED" })),
            ],
        ),
        (
            "a track the request pinned",
            json!({ "trackName": "trailing" }),
            vec![("trackName", json!("trailing"))],
        ),
        (
            "a track name carrying the pattern's underscore",
            json!({ "trackName": "custom_track_1" }),
            vec![("trackName", json!("custom_track_1"))],
        ),
        (
            "a target turned on at the lowest level the member admits",
            json!({ "pricePerformanceTarget": { "status": "ENABLED", "level": 1 } }),
            vec![(
                "pricePerformanceTarget",
                json!({ "status": "ENABLED", "level": 1 }),
            )],
        ),
        (
            "a target turned on at the highest level the member admits",
            json!({ "pricePerformanceTarget": { "status": "ENABLED", "level": 100 } }),
            vec![(
                "pricePerformanceTarget",
                json!({ "status": "ENABLED", "level": 100 }),
            )],
        ),
        (
            "a target the request turned off itself",
            json!({ "pricePerformanceTarget": { "status": "DISABLED" } }),
            vec![("pricePerformanceTarget", json!({ "status": "DISABLED" }))],
        ),
    ];

    let mut releases: Vec<(String, String)> = Vec::new();
    for (index, (case, extra, expected)) in rows.into_iter().enumerate() {
        // A namespace carries ONE workgroup, so every row stands up its own namespace to hold
        // the workgroup the row creates.
        let namespace_name = format!("derived-ns-{index}");
        create_namespace(&router, &namespace_name).await;
        let workgroup_name = format!("derived-wg-{index}");
        let mut request = json!({
            "workgroupName": workgroup_name,
            "namespaceName": namespace_name,
        });
        for (member, value) in extra.as_object().cloned().unwrap_or_default() {
            request[member] = value;
        }
        let created = ok(&router, "CreateWorkgroup", request).await;
        let created = created["workgroup"].clone();

        for (member, want) in expected {
            let reported =
                reported_by_every_reader(&router, case, &workgroup_name, &created, member).await;
            assert_eq!(
                reported, want,
                "case {case:?}: expected {member} {want:?}, got {reported:?}"
            );
        }

        // The release and the patch level are the serving release's own, so EVERY workgroup
        // reports both, in the published form: a three-part release version and a numeric
        // patch level.
        let release = created["workgroupVersion"].as_str().unwrap_or("").to_string();
        let patch = created["patchVersion"].as_str().unwrap_or("").to_string();
        assert!(
            is_release_version(&release),
            "case {case:?}: expected workgroupVersion <major>.<minor>.<build>, got {release:?}"
        );
        assert!(
            is_number(&patch),
            "case {case:?}: expected a numeric patchVersion, got {patch:?}"
        );
        for member in ["workgroupVersion", "patchVersion"] {
            let _ = reported_by_every_reader(&router, case, &workgroup_name, &created, member)
                .await;
        }
        releases.push((release, patch));
    }

    // One emulator build serves one release, so no two workgroups of this emulator report a
    // different release or patch level.
    let first = releases.first().cloned().unwrap_or_default();
    for (index, reported) in releases.iter().enumerate() {
        assert_eq!(
            reported, &first,
            "case {index}: every workgroup reports the serving release, expected {first:?}, \
             got {reported:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// The managed VPC endpoint
// ---------------------------------------------------------------------------

/// A workgroup's endpoint reports the managed VPC endpoint the account holds for it: one
/// endpoint in the VPC its subnets belong to, with ONE network interface per subnet — each
/// naming that subnet, the zone Amazon EC2 reports it in and an address out of its own range.
/// The rows differ in how many subnets the workgroup was created with.
#[tokio::test]
async fn the_endpoint_reports_one_network_interface_per_subnet_of_the_workgroup() {
    for subnet_count in [1usize, 2, 3] {
        let case = format!("{subnet_count} subnets");
        let router = emulator_with_ec2();
        let ec2 = ec2(&router);
        let (vpc_id, subnets) = network(&ec2, subnet_count).await;
        create_namespace(&router, "endpoint-ns").await;
        let created = ok(
            &router,
            "CreateWorkgroup",
            json!({
                "workgroupName": "endpoint-wg",
                "namespaceName": "endpoint-ns",
                "subnetIds": subnets.iter().map(|subnet| subnet.id.clone()).collect::<Vec<_>>(),
            }),
        )
        .await;
        let created = created["workgroup"].clone();

        let endpoints = at(&created, "endpoint.vpcEndpoints")
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            endpoints.len(),
            1,
            "case {case:?}: expected one managed VPC endpoint, got {endpoints:?}"
        );
        let endpoint = endpoints.first().cloned().unwrap_or(Value::Null);
        let endpoint_id = endpoint["vpcEndpointId"].as_str().unwrap_or("").to_string();
        assert!(
            endpoint_id.starts_with("vpce-") && !endpoint_id.starts_with("vpce-svc-"),
            "case {case:?}: expected a vpc endpoint id, got {endpoint_id:?}"
        );
        assert_eq!(
            endpoint["vpcId"],
            json!(vpc_id),
            "case {case:?}: the endpoint is in the VPC the subnets belong to, expected \
             {vpc_id:?}, got {:?}",
            endpoint["vpcId"]
        );

        let interfaces = endpoint["networkInterfaces"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            interfaces.len(),
            subnets.len(),
            "case {case:?}: expected one interface per subnet, got {interfaces:?}"
        );
        for (subnet, interface) in subnets.iter().zip(&interfaces) {
            assert_eq!(
                interface["subnetId"],
                json!(subnet.id),
                "case {case:?}: expected the interface of subnet {:?}, got {interface:?}",
                subnet.id
            );
            assert_eq!(
                interface["availabilityZone"],
                json!(subnet.availability_zone),
                "case {case:?}: expected zone {:?}, got {interface:?}",
                subnet.availability_zone
            );
            let interface_id = interface["networkInterfaceId"]
                .as_str()
                .unwrap_or("")
                .to_string();
            assert!(
                interface_id.starts_with("eni-"),
                "case {case:?}: expected a network interface id, got {interface_id:?}"
            );
            let address = interface["privateIpAddress"]
                .as_str()
                .unwrap_or("")
                .to_string();
            assert!(
                address.starts_with(&subnet.address_prefix),
                "case {case:?}: expected an address in {:?}, got {address:?}",
                subnet.address_prefix
            );

            // The interface is not a note on this service's record: Amazon EC2 holds it, in
            // that subnet, with that address.
            let described = ec2
                .describe_network_interfaces()
                .network_interface_ids(&interface_id)
                .send()
                .await
                .expect("DescribeNetworkInterfaces answers");
            let held = described
                .network_interfaces()
                .first()
                .cloned()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: Amazon EC2 must hold the interface {interface_id:?}")
                });
            assert_eq!(
                held.subnet_id(),
                Some(subnet.id.as_str()),
                "case {case:?}: expected EC2 to hold {interface_id:?} in {:?}, got {:?}",
                subnet.id,
                held.subnet_id()
            );
            assert_eq!(
                held.private_ip_address(),
                Some(address.as_str()),
                "case {case:?}: expected EC2 to hold {interface_id:?} at {address:?}, got {:?}",
                held.private_ip_address()
            );
        }

        // Every reader of the workgroup reports the endpoint the create stored.
        let _ = reported_by_every_reader(
            &router,
            &case,
            "endpoint-wg",
            &created,
            "endpoint.vpcEndpoints",
        )
        .await;
    }
}

/// A workgroup no managed endpoint stands behind reports the empty list rather than an endpoint
/// in a VPC nobody confirmed: one created with no subnet at all, and one naming subnets the
/// account holds none of.
///
/// The create itself still succeeds for the third row: this service gates no workgroup on the
/// subnets it names — the crate carries no such refusal, and one is this operation's own gate
/// rather than this projection's — so an id that resolves to no subnet leaves the workgroup
/// reporting no endpoint, which is what the derivation's module header states.
#[tokio::test]
async fn a_workgroup_whose_subnets_resolve_to_nothing_reports_no_managed_endpoint() {
    let router = emulator_with_ec2();
    // (case, the subnets the create names)
    let rows: [(&str, Value); 3] = [
        ("no subnet member at all", json!(null)),
        ("the empty list", json!([])),
        (
            "ids of the published form the account holds no subnet for",
            json!(["subnet-0123456789abcdef0", "subnet-00000000000000001"]),
        ),
    ];
    for (index, (case, subnet_ids)) in rows.into_iter().enumerate() {
        // One workgroup per namespace, so each row gets a namespace of its own.
        let namespace_name = format!("unplaced-ns-{index}");
        create_namespace(&router, &namespace_name).await;
        let workgroup_name = format!("unplaced-wg-{index}");
        let mut request = json!({
            "workgroupName": workgroup_name,
            "namespaceName": namespace_name,
        });
        if !subnet_ids.is_null() {
            request["subnetIds"] = subnet_ids;
        }
        let created = ok(&router, "CreateWorkgroup", request).await;
        let created = created["workgroup"].clone();
        let endpoints = at(&created, "endpoint.vpcEndpoints").clone();
        assert_eq!(
            endpoints,
            json!([]),
            "case {case:?}: expected no managed VPC endpoint, got {endpoints:?}"
        );
        let _ = reported_by_every_reader(
            &router,
            case,
            &workgroup_name,
            &created,
            "endpoint.vpcEndpoints",
        )
        .await;
    }
}

/// The endpoint's interfaces are one per subnet the workgroup carries NOW: moving the workgroup
/// to other subnets moves the endpoint with it, and the endpoint it held is given back — while
/// an update that names none of them, or the same set in another order, leaves it exactly where
/// it is. A move onto subnets the account does not hold stands no endpoint up, so the workgroup
/// reports none.
#[tokio::test]
async fn moving_a_workgroup_to_other_subnets_moves_its_managed_endpoint() {
    let router = emulator_with_ec2();
    let ec2 = ec2(&router);
    let (_, subnets) = network(&ec2, 3).await;
    create_namespace(&router, "moving-ns").await;
    let created = ok(
        &router,
        "CreateWorkgroup",
        json!({
            "workgroupName": "moving-wg",
            "namespaceName": "moving-ns",
            "subnetIds": [subnets[0].id, subnets[1].id],
        }),
    )
    .await;
    let first = at(&created["workgroup"], "endpoint.vpcEndpoints")
        .as_array()
        .and_then(|endpoints| endpoints.first().cloned())
        .unwrap_or(Value::Null);
    let first_id = first["vpcEndpointId"].as_str().unwrap_or("").to_string();

    // An update that touches something else leaves the endpoint alone.
    let updated = ok(
        &router,
        "UpdateWorkgroup",
        json!({ "workgroupName": "moving-wg", "baseCapacity": 64 }),
    )
    .await;
    assert_eq!(
        at(&updated["workgroup"], "endpoint.vpcEndpoints"),
        &json!([first.clone()]),
        "case \"an update naming no subnet\": expected the endpoint it had, got {:?}",
        at(&updated["workgroup"], "endpoint.vpcEndpoints")
    );

    // An update naming the same subnets in the other order asks for the same interfaces in the
    // same subnets, so the endpoint stays exactly where it is.
    let reordered = ok(
        &router,
        "UpdateWorkgroup",
        json!({
            "workgroupName": "moving-wg",
            "subnetIds": [subnets[1].id, subnets[0].id],
        }),
    )
    .await;
    assert_eq!(
        at(&reordered["workgroup"], "endpoint.vpcEndpoints"),
        &json!([first.clone()]),
        "case \"an update reordering the same subnets\": expected the endpoint it had, got {:?}",
        at(&reordered["workgroup"], "endpoint.vpcEndpoints")
    );

    // An update naming other subnets places the endpoint in those.
    let moved = ok(
        &router,
        "UpdateWorkgroup",
        json!({
            "workgroupName": "moving-wg",
            "subnetIds": [subnets[2].id, subnets[0].id],
        }),
    )
    .await;
    let moved_endpoints = at(&moved["workgroup"], "endpoint.vpcEndpoints")
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        moved_endpoints.len(),
        1,
        "case \"an update naming other subnets\": expected one endpoint, got {moved_endpoints:?}"
    );
    let moved_endpoint = moved_endpoints.first().cloned().unwrap_or(Value::Null);
    let placed: Vec<Value> = moved_endpoint["networkInterfaces"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|interface| interface["subnetId"].clone())
        .collect();
    assert_eq!(
        placed,
        vec![json!(subnets[2].id), json!(subnets[0].id)],
        "case \"an update naming other subnets\": expected an interface in each named subnet, \
         got {placed:?}"
    );

    // The endpoint it held is gone from the account: it carried the traffic of a placement the
    // workgroup has left.
    let described = ec2
        .describe_vpc_endpoints()
        .send()
        .await
        .expect("DescribeVpcEndpoints answers");
    let held: Vec<&str> = described
        .vpc_endpoints()
        .iter()
        .filter_map(::aws_sdk_ec2::types::VpcEndpoint::vpc_endpoint_id)
        .collect();
    assert!(
        !held.contains(&first_id.as_str()),
        "case \"an update naming other subnets\": expected {first_id:?} to be given back, \
         got {held:?}"
    );
    let moved_id = moved_endpoint["vpcEndpointId"].as_str().unwrap_or("");
    assert!(
        held.contains(&moved_id),
        "case \"an update naming other subnets\": expected EC2 to hold {moved_id:?}, got {held:?}"
    );
    let moved_id = moved_id.to_string();

    // An update onto ids the account holds no subnet for places the endpoint nowhere: the
    // workgroup carries those ids and reports NO managed endpoint, rather than one whose
    // interfaces sit in subnets it has left — and that endpoint is given back too.
    let unplaced = ok(
        &router,
        "UpdateWorkgroup",
        json!({
            "workgroupName": "moving-wg",
            "subnetIds": ["subnet-0123456789abcdef0"],
        }),
    )
    .await;
    assert_eq!(
        at(&unplaced["workgroup"], "endpoint.vpcEndpoints"),
        &json!([]),
        "case \"an update naming subnets the account does not hold\": expected no managed \
         endpoint, got {:?}",
        at(&unplaced["workgroup"], "endpoint.vpcEndpoints")
    );
    assert_eq!(
        unplaced["workgroup"]["subnetIds"],
        json!(["subnet-0123456789abcdef0"]),
        "case \"an update naming subnets the account does not hold\": expected the subnets it \
         named, got {:?}",
        unplaced["workgroup"]["subnetIds"]
    );
    let described = ec2
        .describe_vpc_endpoints()
        .send()
        .await
        .expect("DescribeVpcEndpoints answers");
    let held: Vec<&str> = described
        .vpc_endpoints()
        .iter()
        .filter_map(::aws_sdk_ec2::types::VpcEndpoint::vpc_endpoint_id)
        .collect();
    assert!(
        !held.contains(&moved_id.as_str()),
        "case \"an update naming subnets the account does not hold\": expected {moved_id:?} to \
         be given back, got {held:?}"
    );
}

/// A deleted workgroup takes its managed endpoint and its interfaces with it: they carried that
/// workgroup's traffic, so the account holds neither afterwards.
#[tokio::test]
async fn deleting_a_workgroup_gives_its_managed_endpoint_back() {
    let router = emulator_with_ec2();
    let ec2 = ec2(&router);
    let (_, subnets) = network(&ec2, 2).await;
    create_namespace(&router, "released-ns").await;
    let created = ok(
        &router,
        "CreateWorkgroup",
        json!({
            "workgroupName": "released-wg",
            "namespaceName": "released-ns",
            "subnetIds": [subnets[0].id, subnets[1].id],
        }),
    )
    .await;
    let endpoint = at(&created["workgroup"], "endpoint.vpcEndpoints")
        .as_array()
        .and_then(|endpoints| endpoints.first().cloned())
        .unwrap_or(Value::Null);
    let endpoint_id = endpoint["vpcEndpointId"].as_str().unwrap_or("").to_string();
    let interface_ids: Vec<String> = endpoint["networkInterfaces"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|interface| {
            interface["networkInterfaceId"]
                .as_str()
                .map(str::to_string)
        })
        .collect();
    assert_eq!(
        interface_ids.len(),
        2,
        "case \"a deleted workgroup\": expected two interfaces to release, got {interface_ids:?}"
    );

    let _ = ok(
        &router,
        "DeleteWorkgroup",
        json!({ "workgroupName": "released-wg" }),
    )
    .await;

    let described = ec2
        .describe_vpc_endpoints()
        .send()
        .await
        .expect("DescribeVpcEndpoints answers");
    let held: Vec<&str> = described
        .vpc_endpoints()
        .iter()
        .filter_map(::aws_sdk_ec2::types::VpcEndpoint::vpc_endpoint_id)
        .collect();
    assert!(
        !held.contains(&endpoint_id.as_str()),
        "case \"a deleted workgroup\": expected {endpoint_id:?} to be given back, got {held:?}"
    );
    let described = ec2
        .describe_network_interfaces()
        .send()
        .await
        .expect("DescribeNetworkInterfaces answers");
    let interfaces: Vec<&str> = described
        .network_interfaces()
        .iter()
        .filter_map(::aws_sdk_ec2::types::NetworkInterface::network_interface_id)
        .collect();
    for interface_id in &interface_ids {
        assert!(
            !interfaces.contains(&interface_id.as_str()),
            "case \"a deleted workgroup\": expected {interface_id:?} to be given back, \
             got {interfaces:?}"
        );
    }
}
