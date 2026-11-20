// SPDX-License-Identifier: CC0-1.0

//! The inbound rules of the security group a directory's create provisions for its servers, as
//! the service that OWNS security groups reports them.
//!
//! RULE: the security group Directory Service provisions for the servers it places in the
//! caller's VPC is authored REACHABLE — it carries the inbound rules of the directory protocol
//! traffic those servers answer, every one of them sourced from the block of the VPC the
//! servers were placed in rather than from a wider range — and the all-traffic outbound rule
//! Amazon EC2 gives a new group is left as it was. The rule set is documented rather than
//! modelled: the Smithy model says only that
//! `DirectoryVpcSettingsDescription$SecurityGroupId` is "the domain controller security group
//! identifier for the directory", while the Directory Service Administration Guide's "what
//! gets created" for a directory placed in a VPC lists the protocols and ports of the group it
//! authors. A group with no inbound rule admits nothing, so a client reading the directory's
//! group to reason about reachability, to diff the AWS-authored baseline or to revoke one of
//! its rules would find a directory that says its own servers cannot be reached.
//!
//! This suite lives beside the router because the rules are EC2's: a security group rule is an
//! Amazon EC2 resource, so EC2 is mounted on the SAME engine that serves Directory Service and
//! every rule asserted below is READ BACK from EC2's own reports — through
//! `DescribeSecurityGroups`, which reports a group's rules aggregated as `IpPermissions`, and
//! through `DescribeSecurityGroupRules`, which reports each stored rule on its own. A rule this
//! service only recorded would appear in neither, and the revoke below would have nothing to
//! take away. Nothing reaches into either store.
//!
//! The rows differ along what the rule generalises over: the directory type, and so which
//! create provisioned the group and which placement member reports it (Managed Microsoft AD,
//! Simple AD, AD Connector); the block of the VPC the servers sit on (`/16`, `/20`, `/24`,
//! two of them private 10-space and one in another range), which is what every rule's source
//! has to be; the blocks and zones of the chosen subnets; and the order the create names them
//! in.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{block_of, element, elements, form_encode, items_of, opt_element};

/// The Region every request below is signed for, and so the Region the network lives in.
const REGION: &str = "us-west-2";

/// The EC2 API version a client sends in an ec2Query body.
const EC2_API_VERSION: &str = "2016-11-15";

/// The `X-Amz-Target` prefix Directory Service names an operation under: its service shape's
/// name.
const DS_TARGET_PREFIX: &str = "DirectoryService_20150416";

/// A password the modelled `Password` pattern admits.
const PASSWORD: &str = "Passw0rd!2345";

/// The `CustomerDnsIps` an AD Connector forwards to: addresses of a self-managed network this
/// emulator does not hold, which is what a connector's create names.
const CUSTOMER_DNS_IPS: [&str; 2] = ["10.9.0.10", "10.9.1.10"];

/// The range a rule must NOT be scoped to: every IPv4 address there is.
const ALL_IPV4: &str = "0.0.0.0/0";

/// The `IpProtocol` spelling of "every protocol", which an outbound rule EC2 mints carries and
/// no inbound rule of the authored set does.
const ALL_PROTOCOLS: &str = "-1";

/// How long a polled read may take to report the directory in service before a case fails: far
/// longer than any span this service publishes, and far longer than the whole suite needs on an
/// idle host, so a loaded or faster host changes nothing about the outcome.
const SETTLE_DEADLINE: Duration = Duration::from_secs(180);

/// How often such a read asks again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The stage a directory reports once the build its create owes has finished: the `Active`
/// value of the modelled `DirectoryStage` enum.
const STAGE_ACTIVE: &str = "Active";

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

/// One ec2Query action, answered with its status and its XML body.
async fn ec2(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
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
    (status, text)
}

/// One ec2Query action that must succeed, answered with its XML body.
async fn ec2_ok(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let (status, text) = ec2(router, action, params).await;
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

/// One network of the account, as EC2 reported it when it minted it: the VPC, the block EC2
/// holds for it, and the subnets cut out of it.
#[derive(Debug, Clone)]
struct Network {
    vpc_id: String,
    /// The VPC's own block, READ BACK from the owner of VPCs rather than from the request that
    /// asked for it: it is the source every authored inbound rule has to carry.
    vpc_cidr: String,
    subnet_ids: Vec<String>,
}

/// Build one VPC of the account carrying one subnet per entry of `specs`, through EC2's own
/// operations — so every id and every block below is one EC2 handed out.
async fn network(router: &MultiServiceRouter, vpc_cidr: &str, specs: &[SubnetSpec]) -> Network {
    let created = ec2_ok(router, "CreateVpc", &[("CidrBlock", vpc_cidr)]).await;
    let vpc_id = element(&created, "vpcId");
    let mut subnet_ids = Vec::new();
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
        subnet_ids.push(element(&answer, "subnetId"));
    }
    // The VPC's own block, as the OWNER of VPCs reports it: it is what every inbound rule the
    // create authorizes has to be scoped to. Checked against the block this network asked for,
    // so the read below cannot be reading some other value of the answer.
    let described = ec2_ok(
        router,
        "DescribeVpcs",
        &[("Filter.1.Name", "vpc-id"), ("Filter.1.Value.1", &vpc_id)],
    )
    .await;
    let reported_blocks = elements(&described, "cidrBlock");
    assert!(
        !reported_blocks.is_empty() && reported_blocks.iter().all(|block| block == vpc_cidr),
        "EC2 must report {vpc_cidr} as the block of {vpc_id}, got {described}"
    );
    Network {
        vpc_cidr: reported_blocks[0].clone(),
        vpc_id,
        subnet_ids,
    }
}

/// One rule of one direction of a group, as `DescribeSecurityGroups` aggregates it: the
/// protocol, the ports, and the IPv4 ranges the rule admits traffic from.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Permission {
    protocol: String,
    from_port: Option<String>,
    to_port: Option<String>,
    cidrs: Vec<String>,
}

/// The permissions one direction of a reported group carries, in the order the answer reports
/// them.
///
/// `direction` is the member of `SecurityGroup` that holds that side's permissions
/// (`ipPermissions` / `ipPermissionsEgress`). Each permission's ports and ranges are read out of
/// the entries of its OWN permission, so a rule's ranges cannot be paired with another rule's.
///
/// Local to this suite rather than in `query_wire`: the sibling reader there
/// (`cloudformation_ec2_ebs_instance_estate::permissions_of`) answers a different question —
/// it pairs one protocol with one range by position, for rows that declare exactly one range
/// each — while the rule this suite asserts is about every range of every permission. Both read
/// the list through the shared `items_of`.
fn permissions_of(reported: &str, direction: &str) -> Vec<Permission> {
    let Some(block) = block_of(reported, direction) else {
        return Vec::new();
    };
    // The entries are read through the shared `items_of`, so this suite scans tags exactly as
    // every other one does. A permission's `ipRanges` is a list INSIDE a list entry, so that
    // reader yields the permission's own members as one entry and each of its ranges as a
    // further entry: an entry that names a protocol opens a permission, and the range entries
    // that follow it belong to it. The serializer writes a permission's members in the order
    // the model declares them (`ipProtocol`, `fromPort`, `toPort`, `groups`, `ipRanges`), so a
    // permission's ranges always follow the entry that opened it.
    let mut permissions: Vec<Permission> = Vec::new();
    for entry in items_of(block) {
        if let Some(protocol) = opt_element(entry, "ipProtocol") {
            permissions.push(Permission {
                protocol,
                from_port: opt_element(entry, "fromPort"),
                to_port: opt_element(entry, "toPort"),
                cidrs: Vec::new(),
            });
        } else if let Some(cidr) = opt_element(entry, "cidrIp") {
            if let Some(current) = permissions.last_mut() {
                current.cidrs.push(cidr);
            }
        }
    }
    permissions
}

/// The `(protocol, fromPort, toPort)` triple of each permission, for comparing a reported set
/// against the documented one.
fn protocol_ports(permissions: &[Permission]) -> BTreeSet<(String, String, String)> {
    permissions
        .iter()
        .map(|permission| {
            (
                permission.protocol.clone(),
                permission.from_port.clone().unwrap_or_default(),
                permission.to_port.clone().unwrap_or_default(),
            )
        })
        .collect()
}

/// Inbound rules the group a directory's create provisions has to carry.
///
/// Source: the MODEL. `AddIpRoutesRequest$UpdateSecurityGroupForDirectoryControllers` writes
/// out the rules of "the security group that has the description: 'Amazon Web Services created
/// security group for <directory ID> directory controllers'", each sourced from the directory
/// VPC's IPv4 CIDR, with all traffic outbound
/// (`smithy_query(query="doc",shape="AddIpRoutesRequest",member="UpdateSecurityGroupForDirectoryControllers",model_path="services/service-com-amazonaws-directoryservice-directoryservice-20150416/smithy-model/com-amazonaws-directoryservice-directoryservice-20150416.json")`)
/// — Kerberos (88), time (123), the NetBIOS datagram service (138), LDAP (389), the Kerberos
/// change/set-password service (464), SMB (445), the RPC endpoint mapper (135) and the range it
/// hands a client into (1024-65535), LDAPS (636), the global catalog (3268-3269, the model's
/// "3268-33269" ending at the published 3269), DNS (53) and all ICMP.
const DOCUMENTED_INGRESS: &[(&str, &str, &str)] = &[
    ("udp", "88", "88"),
    ("udp", "123", "123"),
    ("udp", "138", "138"),
    ("udp", "389", "389"),
    ("udp", "464", "464"),
    ("udp", "445", "445"),
    ("tcp", "88", "88"),
    ("tcp", "135", "135"),
    ("tcp", "445", "445"),
    ("tcp", "464", "464"),
    ("tcp", "636", "636"),
    ("tcp", "1024", "65535"),
    ("tcp", "3268", "3269"),
    ("udp", "53", "53"),
    ("tcp", "53", "53"),
    ("tcp", "389", "389"),
    ("icmp", "-1", "-1"),
];

/// Which create a row drives, and so which placement member reports the group it provisioned.
#[derive(Debug, Clone, Copy)]
enum Kind {
    /// `CreateMicrosoftAD`, reported as `VpcSettings`.
    MicrosoftAd,
    /// `CreateDirectory` (Simple AD), reported as `VpcSettings`.
    SimpleAd,
    /// `ConnectDirectory` (AD Connector), reported as `ConnectSettings`.
    Connector,
}

impl Kind {
    /// The operation this kind of directory is created with.
    fn operation(self) -> &'static str {
        match self {
            Kind::MicrosoftAd => "CreateMicrosoftAD",
            Kind::SimpleAd => "CreateDirectory",
            Kind::Connector => "ConnectDirectory",
        }
    }

    /// The member of the description this kind of directory reports its placement — and so its
    /// security group — in.
    fn placement(self) -> &'static str {
        match self {
            Kind::MicrosoftAd | Kind::SimpleAd => "VpcSettings",
            Kind::Connector => "ConnectSettings",
        }
    }

    /// The body a create of this kind is sent with, placing the directory in `subnet_ids` of
    /// `vpc_id`.
    fn body(self, name: &str, vpc_id: &str, subnet_ids: &[String]) -> Value {
        match self {
            Kind::MicrosoftAd => json!({
                "Name": name,
                "Password": PASSWORD,
                "VpcSettings": { "VpcId": vpc_id, "SubnetIds": subnet_ids },
            }),
            Kind::SimpleAd => json!({
                "Name": name,
                "Password": PASSWORD,
                "Size": "Small",
                "VpcSettings": { "VpcId": vpc_id, "SubnetIds": subnet_ids },
            }),
            Kind::Connector => json!({
                "Name": name,
                "Password": PASSWORD,
                "Size": "Small",
                "ConnectSettings": {
                    "VpcId": vpc_id,
                    "SubnetIds": subnet_ids,
                    "CustomerDnsIps": CUSTOMER_DNS_IPS,
                    "CustomerUserName": "ad-connector-svc",
                },
            }),
        }
    }
}

/// One row: the network a directory is placed in, which of its subnets the create names and in
/// what order, and which create places it.
struct Row {
    case: &'static str,
    name: &'static str,
    kind: Kind,
    network: usize,
    chosen: &'static [usize],
}

/// The networks every row draws its subnets from, differing in the size and base of the VPC's
/// own block — which is what every authored rule's source has to be — and in the blocks and
/// zones of the subnets cut out of it.
const NETWORKS: [(&str, &[SubnetSpec]); 3] = [
    (
        "10.60.0.0/16",
        &[
            SubnetSpec {
                cidr: "10.60.1.0/24",
                zone: "us-west-2a",
            },
            SubnetSpec {
                cidr: "10.60.2.0/24",
                zone: "us-west-2b",
            },
        ],
    ),
    (
        "10.61.0.0/20",
        &[
            SubnetSpec {
                cidr: "10.61.0.16/28",
                zone: "us-west-2a",
            },
            SubnetSpec {
                cidr: "10.61.1.0/24",
                zone: "us-west-2b",
            },
        ],
    ),
    (
        "172.20.8.0/24",
        &[
            SubnetSpec {
                cidr: "172.20.8.0/26",
                zone: "us-west-2a",
            },
            SubnetSpec {
                cidr: "172.20.8.64/26",
                zone: "us-west-2c",
            },
        ],
    ),
];

const ROWS: &[Row] = &[
    Row {
        case: "a Managed Microsoft AD in a /16 VPC",
        name: "alpha.example.com",
        kind: Kind::MicrosoftAd,
        network: 0,
        chosen: &[0, 1],
    },
    Row {
        case: "a Simple AD in a /20 VPC, in a /28 and a /24",
        name: "beta.example.com",
        kind: Kind::SimpleAd,
        network: 1,
        chosen: &[0, 1],
    },
    Row {
        case: "a Managed Microsoft AD in a /24 VPC outside 10-space, subnets named in reverse",
        name: "gamma.example.com",
        kind: Kind::MicrosoftAd,
        network: 2,
        chosen: &[1, 0],
    },
    Row {
        case: "an AD Connector sharing the /16 VPC with another directory's group",
        name: "delta.example.com",
        kind: Kind::Connector,
        network: 0,
        chosen: &[1, 0],
    },
];

/// The group id the directory `directory_id` reports in `placement`.
async fn reported_group(
    router: &MultiServiceRouter,
    directory_id: &str,
    placement: &str,
    case: &str,
) -> String {
    let described = describe_one(router, directory_id, case).await;
    described[placement]["SecurityGroupId"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a {placement}.SecurityGroupId, got {described}")
        })
        .to_string()
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

/// Wait until the directory reports itself in service — the build a create owes falls due on
/// the clock, so a client reads the settled directory by asking again.
async fn in_service(router: &MultiServiceRouter, directory_id: &str, case: &str) {
    let started = Instant::now();
    loop {
        let described = describe_one(router, directory_id, case).await;
        if described["Stage"] == json!(STAGE_ACTIVE) {
            return;
        }
        assert!(
            started.elapsed() < SETTLE_DEADLINE,
            "case {case:?}: expected {directory_id} to report itself in service within \
             {SETTLE_DEADLINE:?}, last observed {described}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The group `group_id` as EC2 reports it, failing the case when EC2 holds no such group.
async fn group_in_ec2(router: &MultiServiceRouter, group_id: &str, case: &str) -> String {
    let reported = ec2_ok(router, "DescribeSecurityGroups", &[("GroupId.1", group_id)]).await;
    assert_eq!(
        element(&reported, "groupId"),
        group_id,
        "case {case:?}: expected EC2 to hold the group the directory reports, got {reported}"
    );
    reported
}

/// Create the directory a row describes and answer its id.
async fn create(
    router: &MultiServiceRouter,
    kind: Kind,
    name: &str,
    network: &Network,
    chosen: &[usize],
    case: &str,
) -> String {
    let subnet_ids: Vec<String> = chosen
        .iter()
        .map(|index| network.subnet_ids[*index].clone())
        .collect();
    let (status, created) = ds(
        router,
        kind.operation(),
        kind.body(name, &network.vpc_id, &subnet_ids),
    )
    .await;
    assert_eq!(
        status,
        200,
        "case {case:?}: {} must be served, got {status} with {created}",
        kind.operation()
    );
    created["DirectoryId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a DirectoryId, got {created}"))
        .to_string()
}

/// Assert the inbound rules of `reported` are the authored baseline: non-empty, carrying every
/// documented protocol and port range, each one sourced from `vpc_cidr` alone.
fn assert_authored_ingress(reported: &str, vpc_cidr: &str, case: &str) {
    let ingress = permissions_of(reported, "ipPermissions");
    assert!(
        !ingress.is_empty(),
        "case {case:?}: expected the group to carry inbound rules, so the directory's servers \
         can be reached, got {reported}"
    );

    let present = protocol_ports(&ingress);
    let documented: BTreeSet<(String, String, String)> = DOCUMENTED_INGRESS
        .iter()
        .map(|(protocol, from_port, to_port)| {
            (
                (*protocol).to_string(),
                (*from_port).to_string(),
                (*to_port).to_string(),
            )
        })
        .collect();
    assert_eq!(
        present, documented,
        "case {case:?}: expected the group's inbound rules to be the documented set, got \
         {present:?}"
    );

    for permission in &ingress {
        assert_ne!(
            permission.protocol, ALL_PROTOCOLS,
            "case {case:?}: expected each inbound rule to name the protocol it admits, got \
             {permission:?}"
        );
        assert!(
            !permission.cidrs.is_empty(),
            "case {case:?}: expected each inbound rule to name the range it admits traffic \
             from, got {permission:?}"
        );
        for cidr in &permission.cidrs {
            assert_eq!(
                cidr, vpc_cidr,
                "case {case:?}: expected every inbound rule to be scoped to the directory \
                 VPC's own block ({vpc_cidr}), got {permission:?}"
            );
            assert_ne!(
                cidr, ALL_IPV4,
                "case {case:?}: expected no inbound rule open to every address, got \
                 {permission:?}"
            );
        }
    }
}

/// RULE: a create that places a directory's servers in the caller's VPC provisions their
/// security group with the inbound rules of the directory protocol traffic those servers
/// answer, every rule sourced from the block of that VPC — and leaves the all-traffic outbound
/// rule EC2 gives a new group alone.
#[tokio::test]
async fn a_directorys_controllers_group_is_authored_reachable_and_vpc_scoped() {
    let router = emulator();
    let mut networks: Vec<Network> = Vec::new();
    for (vpc_cidr, specs) in NETWORKS {
        networks.push(network(&router, vpc_cidr, specs).await);
    }

    for row in ROWS {
        let case = row.case;
        let network = &networks[row.network];
        let directory_id = create(&router, row.kind, row.name, network, row.chosen, case).await;
        let group_id = reported_group(&router, &directory_id, row.kind.placement(), case).await;

        // The rules are part of the network the create provisions, so they are there from the
        // create on, before the build the directory owes has finished.
        let at_create = group_in_ec2(&router, &group_id, case).await;
        assert_authored_ingress(&at_create, &network.vpc_cidr, case);

        // The outbound side is the one EC2 mints with a new group: all traffic to every
        // address. Nothing above changed it.
        let egress = permissions_of(&at_create, "ipPermissionsEgress");
        assert_eq!(
            egress,
            vec![Permission {
                protocol: ALL_PROTOCOLS.to_string(),
                from_port: None,
                to_port: None,
                cidrs: vec![ALL_IPV4.to_string()],
            }],
            "case {case:?}: expected the group's outbound side to be the all-traffic rule EC2 \
             mints, got {egress:?} from {at_create}"
        );

        // ...and the same rules are still there once the directory is in service: a read
        // authorizes nothing and revokes nothing.
        in_service(&router, &directory_id, case).await;
        let in_service_report = group_in_ec2(&router, &group_id, case).await;
        assert_eq!(
            permissions_of(&in_service_report, "ipPermissions"),
            permissions_of(&at_create, "ipPermissions"),
            "case {case:?}: expected the inbound rules at create to be the ones in service, \
             got {in_service_report}"
        );

        // Each permission is a STORED rule of this group, as the owner reports them one by one:
        // every one of them inbound and sourced from the VPC's own block, which is what a
        // client that diffs or revokes the AWS-authored baseline works from.
        let rules = ec2_ok(
            &router,
            "DescribeSecurityGroupRules",
            &[("Filter.1.Name", "group-id"), ("Filter.1.Value.1", &group_id)],
        )
        .await;
        let ingress_rules: Vec<&str> = items_of(&rules)
            .into_iter()
            .filter(|item| opt_element(item, "isEgress").as_deref() == Some("false"))
            .collect();
        assert!(
            !ingress_rules.is_empty(),
            "case {case:?}: expected EC2 to hold inbound rules of {group_id}, got {rules}"
        );
        for rule in &ingress_rules {
            assert_eq!(
                opt_element(rule, "cidrIpv4").as_deref(),
                Some(network.vpc_cidr.as_str()),
                "case {case:?}: expected every stored inbound rule of {group_id} to carry the \
                 VPC's own block ({}), got {rule}",
                network.vpc_cidr
            );
        }
    }
}

/// RULE: the authored rules are real rules of the group, so the AWS-authored baseline is
/// something a client can manage: revoking ONE of them takes exactly that rule away and leaves
/// every other rule of the group, and of every other directory's group, as it was.
#[tokio::test]
async fn one_authored_rule_can_be_revoked_and_the_rest_stand() {
    let router = emulator();
    let (vpc_cidr, specs) = NETWORKS[0];
    let network = network(&router, vpc_cidr, specs).await;

    let case = "revoking the LDAP rule of a Managed Microsoft AD's group";
    let directory_id = create(
        &router,
        Kind::MicrosoftAd,
        "revoke.example.com",
        &network,
        &[0, 1],
        case,
    )
    .await;
    let group_id = reported_group(&router, &directory_id, "VpcSettings", case).await;
    let reported = group_in_ec2(&router, &group_id, case).await;
    assert_authored_ingress(&reported, &network.vpc_cidr, case);
    let before = permissions_of(&reported, "ipPermissions");

    // One rule of the authored set, revoked the way a client manages its own: by protocol, port
    // range and source, which is what the group has to be holding for the revoke to find it.
    let revoked = ec2_ok(
        &router,
        "RevokeSecurityGroupIngress",
        &[
            ("GroupId", group_id.as_str()),
            ("IpPermissions.1.IpProtocol", "tcp"),
            ("IpPermissions.1.FromPort", "389"),
            ("IpPermissions.1.ToPort", "389"),
            ("IpPermissions.1.IpRanges.1.CidrIp", network.vpc_cidr.as_str()),
        ],
    )
    .await;
    assert!(
        revoked.contains("<return>true</return>"),
        "case {case:?}: expected the revoke of an authored rule to succeed, got {revoked}"
    );

    let after = permissions_of(&group_in_ec2(&router, &group_id, case).await, "ipPermissions");
    let taken: Vec<&Permission> = before
        .iter()
        .filter(|permission| !after.contains(permission))
        .collect();
    assert_eq!(
        taken.len(),
        1,
        "case {case:?}: expected exactly the revoked rule to be gone, got {taken:?}"
    );
    assert_eq!(
        (
            taken[0].protocol.as_str(),
            taken[0].from_port.as_deref(),
            taken[0].to_port.as_deref()
        ),
        ("tcp", Some("389"), Some("389")),
        "case {case:?}: expected the LDAP rule to be the one revoked, got {taken:?}"
    );
    assert_eq!(
        after.len(),
        before.len() - 1,
        "case {case:?}: expected every other rule of the group to stand, got {after:?}"
    );
}
