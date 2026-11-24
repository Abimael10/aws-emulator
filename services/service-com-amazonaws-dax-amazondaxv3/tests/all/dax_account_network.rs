// SPDX-License-Identifier: CC0-1.0

//! The ACCOUNT's network, and the subnet groups this service builds out of it.
//!
//! A subnet is not DAX's resource: a client builds its network with
//! `ec2:CreateVpc`/`ec2:CreateSubnet`, or uses the default VPC Amazon EC2 holds in every
//! Region of the account, and then names those subnets in `CreateSubnetGroup$SubnetIds`.
//! So the suites below mount DAX BESIDE Amazon EC2 on one engine — the way the assembled
//! emulator mounts them — ask EC2 which subnets the account has, and assert that a subnet
//! group is built from exactly those and reports the VPC and Availability Zone EC2 holds
//! for each of them.
//!
//! This module also owns the wire harness its sibling suites share (`emulator`, `call`,
//! `error_code`, `error_message`), so there is one request builder for this crate's
//! network-aware suites rather than one per file. Every case drives `POST /` with
//! `X-Amz-Target: AmazonDAXV3.<Operation>` through the same `MultiServiceRouter` the
//! production router builds, so what is asserted is the HTTP behaviour a DAX SDK client
//! sees.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_dax_amazondaxv3::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region every request below is signed for: the Region whose network a subnet
/// identifier is resolved in.
pub const REGION: &str = "us-west-2";

/// A Region of the account OTHER than [`REGION`], whose subnets are therefore not ones a
/// group created in `REGION` may name.
pub const OTHER_REGION: &str = "eu-west-1";

/// A router carrying this service BESIDE Amazon EC2 over ONE engine, so each test starts
/// from an empty store and the subnets a request names are the ones the owner of subnets
/// really holds.
pub fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router
}

/// Call one DAX operation signed for `region` and return its HTTP status and parsed JSON
/// body.
pub async fn call_in(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AmazonDAXV3.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/dax/aws4_request, \
                 SignedHeaders=host;x-amz-target, Signature=0000"
            ),
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// Call one DAX operation signed for [`REGION`].
pub async fn call(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    call_in(router, REGION, operation, body).await
}

/// The wire error code of a failed call: the `__type` member, without any namespace.
pub fn error_code(body: &Value) -> String {
    body["__type"]
        .as_str()
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The wire error message of a failed call, under either casing an error envelope uses.
pub fn error_message(body: &Value) -> String {
    body["message"]
        .as_str()
        .or_else(|| body["Message"].as_str())
        .unwrap_or_default()
        .to_string()
}

/// How long a case waits for a status change the CLOCK owes before it gives up, and how
/// often it looks again while it waits.
///
/// Generous on purpose: a case waits for the CONDITION it needs rather than for a fixed
/// time, so what it asserts does not depend on how fast or how loaded the host is.
pub const SETTLE_DEADLINE: Duration = Duration::from_secs(30);
/// See [`SETTLE_DEADLINE`].
pub const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(25);

/// The cluster `name`, read back through `DescribeClusters` once it reports `status`.
///
/// Looks again until the clock has made the move due, or [`SETTLE_DEADLINE`] passes. The
/// one place this crate's suites wait for a clock-owed cluster transition.
pub async fn cluster_in_status(router: &MultiServiceRouter, name: &str, status: &str) -> Value {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (code, body) = call(
            router,
            "DescribeClusters",
            json!({ "ClusterNames": [name] }),
        )
        .await;
        let cluster = body["Clusters"][0].clone();
        if code == 200 && cluster["Status"] == json!(status) {
            return cluster;
        }
        assert!(
            Instant::now() < deadline,
            "case {name:?}: the cluster never reached {status:?}; the last DescribeClusters \
             answered {code} {body:?}"
        );
        ::tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}

/// An EC2 client for `region` over the in-process callback this router routes on: no
/// socket is opened, and the endpoint URL is only the address that transport routes by.
///
/// The very seam this service's own subnet resolution reaches EC2 through, so a suite
/// that reads the account's network — or builds one with `ec2:CreateVpc`/`ec2:CreateSubnet`
/// — sees exactly what a placement will look for.
pub fn ec2(router: &MultiServiceRouter, region: &str) -> ::aws_sdk_ec2::Client {
    let callback = ::smithy_server_runtime::EmbeddedEmulator::from_router(router.clone());
    let config = ::aws_sdk_ec2::Config::builder()
        .behavior_version(::aws_sdk_ec2::config::BehaviorVersion::latest())
        .region(::aws_sdk_ec2::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://ec2.{region}.amazonaws.com"))
        .credentials_provider(callback.aws_credentials_provider())
        .http_client(callback.http_client())
        .retry_config(::aws_sdk_ec2::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_ec2::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_ec2::config::IdentityCache::no_cache())
        .stalled_stream_protection(::aws_sdk_ec2::config::StalledStreamProtectionConfig::disabled())
        .build();
    ::aws_sdk_ec2::Client::from_conf(config)
}

/// One subnet the ACCOUNT has, as the service that owns subnets reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountSubnet {
    pub id: String,
    pub zone: String,
    pub vpc: String,
}

/// Every subnet the account has in `region`, as EC2 reports it, in Availability Zone
/// order.
///
/// ASKED, never assumed: these are the ids the owner of subnets answers with for this
/// account, which is also what the service under test resolves a placement against.
pub async fn account_subnets(router: &MultiServiceRouter, region: &str) -> Vec<AccountSubnet> {
    let described = ec2(router, region)
        .describe_subnets()
        .send()
        .await
        .expect("EC2 must report the account's subnets");
    let mut reported: Vec<AccountSubnet> = described
        .subnets()
        .iter()
        .filter_map(|subnet| {
            Some(AccountSubnet {
                id: subnet.subnet_id()?.to_string(),
                zone: subnet.availability_zone()?.to_string(),
                vpc: subnet.vpc_id()?.to_string(),
            })
        })
        .collect();
    reported.sort_by(|left, right| left.zone.cmp(&right.zone));
    reported
}

/// The identifiers of [`account_subnets`].
pub async fn account_subnet_ids(router: &MultiServiceRouter, region: &str) -> Vec<String> {
    account_subnets(router, region)
        .await
        .into_iter()
        .map(|subnet| subnet.id)
        .collect()
}

/// One subnet the account has in [`REGION`].
pub async fn an_account_subnet(router: &MultiServiceRouter) -> String {
    account_subnet_ids(router, REGION)
        .await
        .into_iter()
        .next()
        .expect("the account has subnets in its default VPC")
}

/// A VPC of the client's OWN, with one subnet in it, both minted through EC2 — the other
/// way an account comes to have a subnet besides the default VPC.
async fn client_subnet(router: &MultiServiceRouter, zone: &str) -> AccountSubnet {
    let client = ec2(router, REGION);
    let vpc = client
        .create_vpc()
        .cidr_block("10.42.0.0/16")
        .send()
        .await
        .expect("CreateVpc must mint a VPC")
        .vpc
        .as_ref()
        .and_then(::aws_sdk_ec2::types::Vpc::vpc_id)
        .expect("CreateVpc must report the VPC it minted")
        .to_string();
    let subnet = client
        .create_subnet()
        .vpc_id(&vpc)
        .cidr_block("10.42.1.0/24")
        .availability_zone(zone)
        .send()
        .await
        .expect("CreateSubnet must mint a subnet")
        .subnet
        .expect("CreateSubnet must report the subnet it minted");
    AccountSubnet {
        id: subnet
            .subnet_id()
            .expect("a minted subnet carries an id")
            .to_string(),
        zone: subnet
            .availability_zone()
            .unwrap_or_default()
            .to_string(),
        vpc,
    }
}

/// The `Subnets` a subnet group reports, as `(SubnetIdentifier, SubnetAvailabilityZone)`.
fn reported_subnets(group: &Value) -> Vec<(String, String)> {
    group["Subnets"]
        .as_array()
        .map(|subnets| {
            subnets
                .iter()
                .map(|subnet| {
                    (
                        subnet["SubnetIdentifier"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                        subnet["SubnetAvailabilityZone"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Rule: `CreateSubnetGroup` resolves every identifier in `SubnetIds` against the subnets
// the ACCOUNT has in the Region the request was signed for — as the service that OWNS
// subnets reports them — and stores the group with the VPC and the Availability Zone that
// owner holds for each of them. Whichever way a subnet came to be the account's (a
// default subnet of the default VPC, or one minted in a VPC of the client's own) naming
// it is accepted.
// Source: `CreateSubnetGroup$SubnetIds` ("A list of VPC subnet IDs for the subnet group"),
// `SubnetGroup$VpcId` / `Subnet$SubnetAvailabilityZone`, and EC2's own `DescribeSubnets`.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_subnet_group_is_built_from_the_subnets_the_account_has() {
    let router = emulator();
    let held = account_subnets(&router, REGION).await;
    assert!(
        held.len() >= 3,
        "the account's default VPC must hold a subnet per zone, got {held:?}"
    );
    let own = client_subnet(&router, &held[0].zone).await;

    // Rows differ along what the rule generalises over: how many subnets are named, where
    // they come from (the default VPC, or a VPC the client created), the order they are
    // named in, and whether one is named twice.
    let ids: Vec<String> = held.iter().map(|subnet| subnet.id.clone()).collect();
    let cases: [(&str, Vec<String>); 6] = [
        ("one-default-subnet", vec![ids[0].clone()]),
        ("two-default-subnets", vec![ids[0].clone(), ids[1].clone()]),
        ("every-default-subnet", ids.clone()),
        ("reversed", ids.iter().rev().cloned().collect()),
        ("repeated", vec![ids[1].clone(), ids[1].clone()]),
        ("a-vpc-of-the-clients-own", vec![own.id.clone()]),
    ];
    // What the owner of subnets holds for every id these cases name.
    let owner: Vec<AccountSubnet> = held.iter().cloned().chain([own.clone()]).collect();

    for (case, subnet_ids) in cases {
        let (status, body) = call(
            &router,
            "CreateSubnetGroup",
            json!({ "SubnetGroupName": case, "SubnetIds": subnet_ids.clone() }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: {subnet_ids:?} are subnets the account has, got {status} {body:?}"
        );
        let group = &body["SubnetGroup"];

        // The group's VPC is the VPC the owner holds those subnets in, never one built
        // from the Region name.
        let expected_vpc = owner
            .iter()
            .find(|subnet| subnet.id == subnet_ids[0])
            .map(|subnet| subnet.vpc.clone());
        assert_eq!(
            group["VpcId"].as_str().map(str::to_string),
            expected_vpc,
            "case {case:?}: the group must report the VPC EC2 holds its subnets in, \
             got {group:?}"
        );
        // Every subnet reports the zone the owner holds it in.
        for (id, zone) in reported_subnets(group) {
            let expected_zone = owner
                .iter()
                .find(|subnet| subnet.id == id)
                .map(|subnet| subnet.zone.clone());
            assert_eq!(
                Some(zone.clone()),
                expected_zone,
                "case {case:?}: subnet {id} must report the zone EC2 holds it in, got {zone:?}"
            );
        }
        // Round trip: the stored group reads back exactly as it was answered.
        let (status, read) = call(
            &router,
            "DescribeSubnetGroups",
            json!({ "SubnetGroupNames": [case] }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeSubnetGroups got {status} {read:?}"
        );
        assert_eq!(
            read["SubnetGroups"][0], body["SubnetGroup"],
            "case {case:?}: the stored group must read back as created, got {read:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule: a subnet group reports ONE `VpcId`, so every subnet it names must belong to the
// same VPC — "All of the subnets that you specify in a subnet group must exist in the same
// VPC" (`CreateCluster$SubnetGroupName`) — and a request spanning two of the account's VPCs
// is refused with the modelled `InvalidSubnet`, storing nothing.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_subnet_group_whose_subnets_span_two_vpcs_is_refused() {
    let router = emulator();
    let held = account_subnets(&router, REGION).await;
    let own = client_subnet(&router, &held[0].zone).await;
    assert_ne!(
        own.vpc, held[0].vpc,
        "the client's own VPC must differ from the default one, got {own:?} and {:?}",
        held[0]
    );

    // Rows differ in how the two VPCs are mixed: the default VPC's subnet first, the
    // client's first, and the client's subnet among several of the default VPC's.
    let cases: [(&str, Vec<String>); 3] = [
        ("default-then-own", vec![held[0].id.clone(), own.id.clone()]),
        ("own-then-default", vec![own.id.clone(), held[0].id.clone()]),
        (
            "own-among-defaults",
            vec![held[0].id.clone(), own.id.clone(), held[1].id.clone()],
        ),
    ];

    for (case, subnet_ids) in cases {
        let (status, body) = call(
            &router,
            "CreateSubnetGroup",
            json!({ "SubnetGroupName": case, "SubnetIds": subnet_ids.clone() }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: {subnet_ids:?} span two VPCs, expected a refusal, \
             got {status} {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidSubnet",
            "case {case:?}: expected InvalidSubnet, got {body:?}"
        );
        assert_eq!(
            error_message(&body),
            "All of the subnets that you specify in a subnet group must exist in the same \
             VPC.",
            "case {case:?}: the message must state the rule, got {body:?}"
        );
        // Nothing was stored: the group does not exist.
        let (status, read) = call(
            &router,
            "DescribeSubnetGroups",
            json!({ "SubnetGroupNames": [case] }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: a refused create leaves nothing behind, got {status} {read:?}"
        );
        assert_eq!(
            error_code(&read),
            "SubnetGroupNotFoundFault",
            "case {case:?}: expected SubnetGroupNotFoundFault, got {read:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule: an identifier the owner of subnets holds no subnet of is the ONE case a subnet
// group is refused for — HTTP 400 `InvalidParameterValueException`, listing the
// identifiers that resolved to nothing — and such a request stores nothing. Nothing is
// cached: a subnet deleted through EC2 stops resolving at once.
// Source: the recorded AWS answer for a `SubnetIds` naming a subnet the account does not
// have, and `services/common/src/network.rs` (the owner of subnets is the one answer).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_subnet_the_owner_does_not_hold_is_refused_and_stores_nothing() {
    let router = emulator();
    let here = account_subnet_ids(&router, REGION).await;
    let elsewhere = account_subnet_ids(&router, OTHER_REGION).await;
    assert!(
        !elsewhere.is_empty() && !elsewhere.iter().any(|id| here.contains(id)),
        "another Region's subnets are other subnets, got {elsewhere:?} beside {here:?}"
    );

    // A subnet the account HAD and deleted: the same identifier, no longer held.
    let gone = client_subnet(&router, &account_subnets(&router, REGION).await[0].zone).await;
    ec2(&router, REGION)
        .delete_subnet()
        .subnet_id(&gone.id)
        .send()
        .await
        .expect("DeleteSubnet must remove the subnet it was given");

    // Rows differ in WHY an identifier resolves to nothing: the long EC2 id form that
    // names no subnet, the short form, a string that is not a subnet id at all, a real
    // subnet of ANOTHER Region, a subnet of this Region that was deleted, and one bad
    // identifier hidden among good ones.
    let cases: [(&str, Vec<String>, Vec<String>); 6] = [
        (
            "unheld-long-form",
            vec!["subnet-0000000000deadbee".to_string()],
            vec!["subnet-0000000000deadbee".to_string()],
        ),
        (
            "unheld-short-form",
            vec!["subnet-0a1b2c3d".to_string()],
            vec!["subnet-0a1b2c3d".to_string()],
        ),
        (
            "not-a-subnet-id",
            vec!["potato".to_string()],
            vec!["potato".to_string()],
        ),
        (
            "another-regions-subnet",
            vec![elsewhere[0].clone()],
            vec![elsewhere[0].clone()],
        ),
        ("a-deleted-subnet", vec![gone.id.clone()], vec![gone.id]),
        (
            "one-bad-among-good",
            vec![
                here[0].clone(),
                "subnet-0000000000deadbee".to_string(),
                here[1].clone(),
            ],
            vec!["subnet-0000000000deadbee".to_string()],
        ),
    ];

    for (case, subnet_ids, unresolved) in cases {
        let (status, body) = call(
            &router,
            "CreateSubnetGroup",
            json!({ "SubnetGroupName": case, "SubnetIds": subnet_ids.clone() }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: {subnet_ids:?} names a subnet the account does not have, \
             expected a refusal, got {status} {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidParameterValueException",
            "case {case:?}: expected InvalidParameterValueException, got {body:?}"
        );
        assert_eq!(
            error_message(&body),
            format!(
                "Some input subnets in :[{}] are invalid.",
                unresolved.join(", ")
            ),
            "case {case:?}: the message must list the identifiers that resolve to nothing, \
             got {body:?}"
        );
        // Nothing was stored: the group does not exist.
        let (status, read) = call(
            &router,
            "DescribeSubnetGroups",
            json!({ "SubnetGroupNames": [case] }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: a refused create leaves nothing behind, got {status} {read:?}"
        );
        assert_eq!(
            error_code(&read),
            "SubnetGroupNotFoundFault",
            "case {case:?}: expected SubnetGroupNotFoundFault, got {read:?}"
        );
    }
}
