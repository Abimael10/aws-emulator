// SPDX-License-Identifier: CC0-1.0

//! The network a Managed Microsoft AD's domain controllers sit on, as the service that OWNS
//! that network reports it.
//!
//! RULE: a create that places a directory's servers in the caller's subnets provisions those
//! servers' network in Amazon EC2 — ONE security group for the directory's controllers, in the
//! VPC the chosen subnets belong to and named after the directory, and ONE elastic network
//! interface per chosen subnet, in that subnet, carrying that group and holding an address out
//! of that subnet's own block — and the directory reports exactly those resources back:
//! `DirectoryVpcSettingsDescription$SecurityGroupId` ("the domain controller security group
//! identifier for the directory") names the group EC2 holds, and
//! `DirectoryDescription$DnsIpAddrs` ("the IP addresses of the DNS servers for the directory")
//! holds one address per chosen subnet, each the address the interface placed in THAT subnet
//! really answers on. `DirectoryVpcSettings$SubnetIds`' own documentation is what places them:
//! "Directory Service creates a directory server and a DNS server in each of these subnets".
//!
//! This suite lives beside the router because the resources are EC2's: a security group and a
//! network interface are Amazon EC2 resources, so EC2 is mounted on the SAME engine that serves
//! Directory Service and every assertion below is READ BACK from EC2's own reports. An id this
//! service only recorded would name no group and no interface there and could not pass; an
//! address it invented would be in no interface EC2 holds. Nothing reaches into either store.
//!
//! The rows differ along what the rule generalises over: the size of each chosen subnet's block
//! (`/24`, `/28`, `/20`, `/26`, `/25`) and its base address, the zones they span, the order the
//! create names them in, whether the VPC already holds another directory's controllers — a
//! second directory placed in the SAME two subnets gets its own group, its own interfaces and
//! its own addresses — and whether the VPC holds a further subnet the create does NOT name,
//! which is then given no interface at all.
//!
//! What this suite adds over the Directory Service crate's own placement suites
//! (`directory_security_group`, `directory_server_interfaces`, `directory_dns_addresses`, which
//! span the directory types, the account's default subnets and the teardown) is the ASSEMBLED
//! path: the two services are reached the way a client reaches them, over their own wire
//! protocols — ec2Query for the network, JSON 1.1 for the control plane — through one router,
//! so the create's in-process hop into the owner of these resources is exercised as the running
//! emulator routes it.
//!
//! The readers are the test crate's shared ones (`query_wire`), so this suite parses the
//! ec2Query answers — and judges an address against a subnet's block — the way every other one
//! does.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{block_contains, element, elements, form_encode, opt_element};

/// The Region every request below is signed for, and so the Region the network lives in.
const REGION: &str = "us-west-2";

/// The EC2 API version a client sends in an ec2Query body.
const EC2_API_VERSION: &str = "2016-11-15";

/// The `X-Amz-Target` prefix Directory Service names an operation under: its service shape's
/// name.
const DS_TARGET_PREFIX: &str = "DirectoryService_20150416";

/// A password the modelled `Password` pattern admits.
const PASSWORD: &str = "Passw0rd!2345";

/// How long a polled read may take to report the addresses the create owes before a case
/// fails: far longer than any span this service publishes, so a loaded or faster host changes
/// nothing about the outcome.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How often such a read asks again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Amazon EC2 and Directory Service over ONE shared engine, as the production router mounts
/// them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_directoryservice_directoryservice_20150416::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`, which is what
/// routes the request to that service; the signature is never checked.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One ec2Query action that must succeed, answered with its XML body.
async fn ec2_ok(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
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
        "EC2 {action} must succeed, got {status} with {text}"
    );
    text
}

/// One Directory Service control-plane call: its status and its parsed body.
async fn ds(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ds.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{DS_TARGET_PREFIX}.{operation}"))
        .header("authorization", authorization("ds"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let parsed = if raw.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&raw).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One subnet a row's network is built with.
struct SubnetSpec {
    cidr: &'static str,
    zone: &'static str,
}

/// One subnet of the account, as EC2 reported it when it minted it.
#[derive(Debug, Clone)]
struct Subnet {
    id: String,
    cidr_block: String,
}

/// One network of the account: the VPC and the subnets EC2 minted in it.
#[derive(Debug, Clone)]
struct Network {
    vpc_id: String,
    subnets: Vec<Subnet>,
}

/// Build one VPC of the account carrying one subnet per entry of `specs`, through EC2's own
/// operations — so every id and every block below is one EC2 handed out.
async fn network(router: &MultiServiceRouter, vpc_cidr: &str, specs: &[SubnetSpec]) -> Network {
    let created = ec2_ok(router, "CreateVpc", &[("CidrBlock", vpc_cidr)]).await;
    let vpc_id = element(&created, "vpcId");
    let mut subnets = Vec::new();
    for spec in specs {
        let answer = ec2_ok(
            router,
            "CreateSubnet",
            &[
                ("VpcId", vpc_id.as_str()),
                ("CidrBlock", spec.cidr),
                ("AvailabilityZone", spec.zone),
            ],
        )
        .await;
        subnets.push(Subnet {
            id: element(&answer, "subnetId"),
            cidr_block: element(&answer, "cidrBlock"),
        });
    }
    Network { vpc_id, subnets }
}

/// The description `DescribeDirectories` reports for `directory_id`.
async fn describe_one(router: &MultiServiceRouter, directory_id: &str, case: &str) -> Value {
    let (status, body) = ds(
        router,
        "DescribeDirectories",
        json!({ "DirectoryIds": [directory_id] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeDirectories must succeed, got {status} with {body}"
    );
    body["DirectoryDescriptions"]
        .as_array()
        .and_then(|descriptions| descriptions.first().cloned())
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected {directory_id} to be described, got {body}")
        })
}

/// The `DnsIpAddrs` a description reports, in the order it reports them.
fn dns_ip_addrs(described: &Value) -> Vec<String> {
    described["DnsIpAddrs"]
        .as_array()
        .map(|addresses| {
            addresses
                .iter()
                .filter_map(|address| address.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Wait until the directory reports `expected` DNS addresses — the controllers a create places
/// come up on the clock, so a client reads the settled set by asking again — and answer the
/// whole description it reported them in.
async fn settled_description(
    router: &MultiServiceRouter,
    directory_id: &str,
    expected: usize,
    case: &str,
) -> Value {
    let started = Instant::now();
    loop {
        let described = describe_one(router, directory_id, case).await;
        let addresses = dns_ip_addrs(&described);
        if addresses.len() == expected {
            return described;
        }
        assert!(
            started.elapsed() < SETTLE_DEADLINE,
            "case {case:?}: expected {expected} DNS address(es) for {directory_id} within \
             {SETTLE_DEADLINE:?}, last observed {addresses:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One network interface of the account, as EC2 reports it.
#[derive(Debug, Clone)]
struct Interface {
    id: String,
    subnet_id: String,
    vpc_id: String,
    address: String,
    groups: BTreeSet<String>,
}

/// The interfaces of `vpc_id` that belong to the directory `directory_id`: the ones whose
/// description names it, which is how a client reconciling a directory against the owner of
/// network interfaces finds them.
///
/// Each one is read through EC2's own per-identifier report, so every member below is scoped to
/// the interface it belongs to.
async fn directory_interfaces(
    router: &MultiServiceRouter,
    vpc_id: &str,
    directory_id: &str,
) -> Vec<Interface> {
    let listed = ec2_ok(
        router,
        "DescribeNetworkInterfaces",
        &[("Filter.1.Name", "vpc-id"), ("Filter.1.Value.1", vpc_id)],
    )
    .await;
    let mut found = Vec::new();
    for interface_id in elements(&listed, "networkInterfaceId") {
        let reported = ec2_ok(
            router,
            "DescribeNetworkInterfaces",
            &[("NetworkInterfaceId.1", interface_id.as_str())],
        )
        .await;
        let names_directory = opt_element(&reported, "description")
            .is_some_and(|description| description.contains(directory_id));
        if !names_directory {
            continue;
        }
        found.push(Interface {
            id: interface_id,
            subnet_id: element(&reported, "subnetId"),
            vpc_id: element(&reported, "vpcId"),
            address: element(&reported, "privateIpAddress"),
            groups: elements(&reported, "groupId").into_iter().collect(),
        });
    }
    found
}

/// One row: the network a directory is placed in, which of its subnets the create names and in
/// what order.
struct Row {
    case: &'static str,
    name: &'static str,
    network: usize,
    chosen: &'static [usize],
}

/// The networks every row draws its subnets from, differing in the size and base of the
/// blocks their subnets carry and the zones those subnets are in.
const NETWORKS: [(&str, &[SubnetSpec]); 3] = [
    (
        "10.80.0.0/16",
        &[
            SubnetSpec {
                cidr: "10.80.1.0/24",
                zone: "us-west-2a",
            },
            SubnetSpec {
                cidr: "10.80.2.0/24",
                zone: "us-west-2b",
            },
        ],
    ),
    (
        "10.81.0.0/16",
        &[
            SubnetSpec {
                cidr: "10.81.0.16/28",
                zone: "us-west-2a",
            },
            SubnetSpec {
                cidr: "10.81.16.0/20",
                zone: "us-west-2c",
            },
        ],
    ),
    (
        "10.82.0.0/16",
        &[
            SubnetSpec {
                cidr: "10.82.1.0/24",
                zone: "us-west-2a",
            },
            SubnetSpec {
                cidr: "10.82.2.0/26",
                zone: "us-west-2b",
            },
            SubnetSpec {
                cidr: "10.82.3.128/25",
                zone: "us-west-2c",
            },
        ],
    ),
];

const ROWS: &[Row] = &[
    Row {
        case: "two subnets of one /24 pair, in the order the create names them",
        name: "alpha.example.com",
        network: 0,
        chosen: &[0, 1],
    },
    Row {
        case: "a SECOND directory in the same two subnets, named in the other order",
        name: "beta.example.com",
        network: 0,
        chosen: &[1, 0],
    },
    Row {
        case: "two subnets whose blocks differ in size, the smallest EC2 allows and a /20",
        name: "gamma.example.com",
        network: 1,
        chosen: &[0, 1],
    },
    Row {
        case: "two subnets of a VPC that holds a third the create does not name",
        name: "delta.example.com",
        network: 2,
        chosen: &[2, 1],
    },
];

#[tokio::test]
async fn a_microsoft_ad_provisions_its_controllers_network_in_ec2() {
    let router = emulator();
    let mut networks: Vec<Network> = Vec::new();
    for (vpc_cidr, specs) in NETWORKS {
        networks.push(network(&router, vpc_cidr, specs).await);
    }

    // Every address any directory of one network has been given, so no two servers of the
    // account are reported at one address: a second directory in the SAME subnets gets its own.
    let mut addresses_given: BTreeSet<(String, String)> = BTreeSet::new();

    for row in ROWS {
        let case = row.case;
        let network = &networks[row.network];
        let chosen: Vec<Subnet> = row
            .chosen
            .iter()
            .map(|index| network.subnets[*index].clone())
            .collect();
        let subnet_ids: Vec<String> = chosen.iter().map(|subnet| subnet.id.clone()).collect();

        let (status, created) = ds(
            &router,
            "CreateMicrosoftAD",
            json!({
                "Name": row.name,
                "Password": PASSWORD,
                "VpcSettings": { "VpcId": network.vpc_id, "SubnetIds": subnet_ids },
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateMicrosoftAD must be served, got {status} with {created}"
        );
        let directory_id = created["DirectoryId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a DirectoryId, got {created}"))
            .to_string();

        // ------------------------------------------------------------------
        // One DNS address per chosen subnet, inside THAT subnet's own block.
        // ------------------------------------------------------------------
        let described = settled_description(&router, &directory_id, chosen.len(), case).await;
        let reported_addresses = dns_ip_addrs(&described);
        // (the subnet an address is inside -> that address), built by CONTAINMENT rather than
        // by position, so a case fails rather than mispairing when an address is out of place.
        let mut address_of_subnet: BTreeMap<String, String> = BTreeMap::new();
        for address in &reported_addresses {
            let containing: Vec<&Subnet> = chosen
                .iter()
                .filter(|subnet| block_contains(&subnet.cidr_block, address))
                .collect();
            assert_eq!(
                containing.len(),
                1,
                "case {case:?}: expected {address:?} to be inside exactly one chosen subnet's \
                 block, got {:?}",
                containing
                    .iter()
                    .map(|subnet| (subnet.id.clone(), subnet.cidr_block.clone()))
                    .collect::<Vec<_>>()
            );
            let subnet = containing[0];
            let already = address_of_subnet.insert(subnet.id.clone(), address.clone());
            assert!(
                already.is_none(),
                "case {case:?}: expected one DNS address per chosen subnet, got {already:?} and \
                 {address:?} both inside {} ({})",
                subnet.id,
                subnet.cidr_block
            );
            assert!(
                addresses_given.insert((network.vpc_id.clone(), address.clone())),
                "case {case:?}: expected an address no other server of {} already holds, got \
                 {address:?}",
                network.vpc_id
            );
        }
        assert_eq!(
            address_of_subnet.len(),
            chosen.len(),
            "case {case:?}: expected an address inside every chosen subnet, got \
             {address_of_subnet:?} for {subnet_ids:?}"
        );

        // ------------------------------------------------------------------
        // The controllers' security group, as the owner of security groups
        // reports it: in the VPC the chosen subnets belong to, named after
        // the directory, and the one group of that VPC that is.
        // ------------------------------------------------------------------
        let group_id = described["VpcSettings"]["SecurityGroupId"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected a VpcSettings.SecurityGroupId, got {described}")
            })
            .to_string();
        let reported_group = ec2_ok(
            &router,
            "DescribeSecurityGroups",
            &[("GroupId.1", group_id.as_str())],
        )
        .await;
        assert_eq!(
            element(&reported_group, "groupId"),
            group_id,
            "case {case:?}: expected EC2 to hold the group the directory reports, got \
             {reported_group}"
        );
        assert_eq!(
            element(&reported_group, "vpcId"),
            network.vpc_id,
            "case {case:?}: expected the group in the VPC the chosen subnets belong to, got \
             {reported_group}"
        );
        let group_name = element(&reported_group, "groupName");
        assert!(
            group_name.contains(&directory_id),
            "case {case:?}: expected the group's name to carry {directory_id}, got \
             {group_name:?}"
        );
        let groups_of_vpc = ec2_ok(
            &router,
            "DescribeSecurityGroups",
            &[
                ("Filter.1.Name", "vpc-id"),
                ("Filter.1.Value.1", network.vpc_id.as_str()),
            ],
        )
        .await;
        let named_after_directory: Vec<String> = elements(&groups_of_vpc, "groupName")
            .into_iter()
            .filter(|name| name.contains(&directory_id))
            .collect();
        assert_eq!(
            named_after_directory,
            vec![group_name.clone()],
            "case {case:?}: expected exactly one group of {} named after {directory_id}, got \
             {named_after_directory:?}",
            network.vpc_id
        );

        // ------------------------------------------------------------------
        // One network interface per chosen subnet, as the owner of network
        // interfaces reports it: in that subnet, carrying that group, and
        // answering at the address the directory serves DNS at there.
        // ------------------------------------------------------------------
        let interfaces = directory_interfaces(&router, &network.vpc_id, &directory_id).await;
        assert_eq!(
            interfaces.len(),
            chosen.len(),
            "case {case:?}: expected one interface of {directory_id} per chosen subnet, got \
             {interfaces:?}"
        );
        let placed_in: BTreeSet<String> = interfaces
            .iter()
            .map(|interface| interface.subnet_id.clone())
            .collect();
        assert_eq!(
            placed_in,
            subnet_ids.iter().cloned().collect::<BTreeSet<String>>(),
            "case {case:?}: expected exactly the chosen subnets to hold an interface, got \
             {interfaces:?}"
        );
        let distinct: BTreeSet<String> = interfaces
            .iter()
            .map(|interface| interface.id.clone())
            .collect();
        assert_eq!(
            distinct.len(),
            interfaces.len(),
            "case {case:?}: expected a distinct interface per subnet, got {interfaces:?}"
        );
        for interface in &interfaces {
            assert_eq!(
                interface.vpc_id, network.vpc_id,
                "case {case:?}: expected {} in the VPC the chosen subnets belong to, got {:?}",
                interface.id, interface.vpc_id
            );
            assert!(
                interface.groups.contains(&group_id),
                "case {case:?}: expected {} to carry the controllers' group {group_id}, got {:?}",
                interface.id,
                interface.groups
            );
            let served = address_of_subnet
                .get(&interface.subnet_id)
                .unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: expected the directory to serve DNS in {}, got \
                         {address_of_subnet:?}",
                        interface.subnet_id
                    )
                });
            assert_eq!(
                &interface.address, served,
                "case {case:?}: expected {} to answer at the address the directory serves DNS \
                 at in {}, got {:?}",
                interface.id, interface.subnet_id, interface.address
            );
        }
    }
}
