// SPDX-License-Identifier: CC0-1.0

//! The references a self-deployed create's `hosts` carry, over awsJson1_0.
//!
//! THE RULE under test: a `CreateEnvironment` member the chosen `vcfVersion` does not
//! support is accepted and reported back as supplied, and nothing the service derives
//! reads a fact out of it — so no reference such a member carries is resolved against
//! another service's store and no uniqueness rule is applied to it; only the shape's own
//! structural constraints still judge it.
//!
//! `hosts` is "The ESX hosts to add to the environment … Not supported when
//! `vcfVersion` is `SELF_DEPLOYED`. In that case, you can add hosts using
//! `CreateEnvironmentHost` after the environment is created."
//! (`smithy_query(query="doc", shape="CreateEnvironmentRequest", member="hosts")`), and in
//! that mode "Amazon EVS provisions only the VLAN subnets; no hosts are added and no VCF
//! installation is performed" (`CreateEnvironment` doc) — so an entry's
//! `placementGroupId` ("The unique ID of the placement group where the host is placed"),
//! its `dedicatedHostId` ("The unique ID of the Amazon EC2 Dedicated Host") and its
//! `keyName` ("The name of the SSH key that is used to access the host") name nothing a
//! host is placed on or reached with, because no host is added at all.
//!
//! What still judges such an entry is the SHAPE's own rule: "You cannot use
//! `dedicatedHostId` and `placementGroupId` together in the same `HostInfoForCreate`
//! object. This results in a `ValidationException` response." (`HostInfoForCreate` doc),
//! which holds whenever the object carries both members, in any mode.
//!
//! The controls keep the resolution itself honest: in a mode that DOES install VCF the
//! hosts are added, so a reference the account does not hold is refused with the member
//! named, and one it holds is accepted and reported back on the host.
//!
//! Both services are mounted on ONE shared engine and every call goes over the wire a
//! client makes it on — awsJson1_0 for Amazon EVS, ec2Query for Amazon EC2 — so the VPC,
//! the subnet, the key pair, the placement groups and the Dedicated Hosts are Amazon EC2's
//! own, and the instances a host runs on are read back from Amazon EC2 itself.

use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::create_environment_validation::environment_count;
use crate::environment_lifecycle::{
    call, error_type, vlans_with_prefix, POLL_DEADLINE, REGION, ZONE,
};
use crate::vlan_subnet_propagation::{call_ec2, element, elements, emulator as evs_with_ec2};

/// How long a poll waits between two looks for the state change the clock owes a new
/// environment. The deadline itself is [`POLL_DEADLINE`], far longer than the span, so a
/// loaded host cannot fail a test that waits for a CONDITION.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// The VPC every environment below is deployed into, and the service access subnet inside
/// it — placed clear of the `10.0.<even>.0` blocks [`vlans_with_prefix`] gives the VLAN
/// subnets, which "must not overlap with other subnets in the VPC" (`InitialVlans` doc).
const VPC_CIDR: &str = "10.0.0.0/16";
/// See [`VPC_CIDR`].
const SERVICE_ACCESS_SUBNET_CIDR: &str = "10.0.240.0/24";

/// The VLAN subnet prefix length every create below carries: a /24 leaves room for every
/// host a row adds.
const VLAN_PREFIX_LEN: u8 = 24;

/// The EC2 bare metal instance type every host below runs on, and the type the Dedicated
/// Hosts are allocated for.
const INSTANCE_TYPE: &str = "i4i.metal";

/// The `VcfVersion` values, from the model's own enumeration
/// (`smithy_query(query="shape", shape="VcfVersion")`): the one mode in which Amazon EVS
/// installs no VCF, and the two versions it installs.
const SELF_DEPLOYED: &str = "SELF_DEPLOYED";
/// See [`SELF_DEPLOYED`].
const INSTALLED_VERSIONS: [&str; 2] = ["VCF-5.2.1", "VCF-5.2.2"];

/// Placement ids of the two forms the members' own patterns publish —
/// `^pg-[a-f0-9]{8}([a-f0-9]{9})?$` and `^h-[a-f0-9]{8}([a-f0-9]{9})?$`
/// (`smithy_query(query="shape", shape="HostInfoForCreate")`) — that NOTHING holds: the
/// short eight-digit form and the long seventeen-digit one.
const UNHELD_GROUP_SHORT: &str = "pg-00000000";
/// See [`UNHELD_GROUP_SHORT`].
const UNHELD_GROUP_LONG: &str = "pg-0123456789abcdef0";
/// See [`UNHELD_GROUP_SHORT`].
const UNHELD_HOST_SHORT: &str = "h-00000000";
/// See [`UNHELD_GROUP_SHORT`].
const UNHELD_HOST_LONG: &str = "h-0123456789abcdef0";

/// The SSH key pair every `hosts` entry below names, and the name nothing holds a key
/// pair for: `keyName` is one more reference an entry carries, so the first table holds
/// the pair and varies the placement members, and the second table holds none.
const HELD_KEY_NAME: &str = "evs-key";
/// See [`HELD_KEY_NAME`].
const UNHELD_KEY_NAME: &str = "no-such-key";

/// The placement reference a row's chosen `hosts` entries carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reference {
    /// No placement member at all.
    None,
    /// A placement group the account really holds, minted through Amazon EC2 itself.
    HeldGroup,
    /// A Dedicated Host the account really holds, allocated through Amazon EC2 itself.
    HeldDedicatedHost,
    /// A `placementGroupId` of this form that nothing holds.
    UnheldGroup(&'static str),
    /// A `dedicatedHostId` of this form that nothing holds.
    UnheldDedicatedHost(&'static str),
    /// Both placement members in one entry — the shape's own rule, whatever the mode.
    BothMembers,
}

/// What a row expects of the create.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// Served: HTTP 200, and the environment holds exactly this many hosts — which is
    /// also how many EC2 instances the account's own store then holds.
    Served { hosts: usize },
    /// Refused with a `ValidationException` whose `fieldList` names exactly these
    /// members, and nothing is created.
    Refused(&'static [&'static str]),
}

/// A VPC and a subnet inside it, minted through Amazon EC2's own `CreateVpc` and
/// `CreateSubnet` — the network `CreateEnvironment` resolves a request's `vpcId` and
/// `serviceAccessSubnetId` against.
async fn fresh_network(router: &MultiServiceRouter, case: &str) -> (String, String) {
    let (status, xml) = call_ec2(router, REGION, "CreateVpc", &[("CidrBlock", VPC_CIDR)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateVpc expected HTTP 200, got {status} {xml}"
    );
    let vpc_id = element(&xml, "vpcId").unwrap_or_else(|| panic!("case {case:?}: no vpcId in {xml}"));
    let (status, xml) = call_ec2(
        router,
        REGION,
        "CreateSubnet",
        &[
            ("VpcId", &vpc_id),
            ("CidrBlock", SERVICE_ACCESS_SUBNET_CIDR),
            ("AvailabilityZone", ZONE),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateSubnet expected HTTP 200, got {status} {xml}"
    );
    let subnet_id =
        element(&xml, "subnetId").unwrap_or_else(|| panic!("case {case:?}: no subnetId in {xml}"));
    (vpc_id, subnet_id)
}

/// A placement group the account really holds, minted by Amazon EC2's own
/// `CreatePlacementGroup`, and its `GroupId`.
async fn placement_group(router: &MultiServiceRouter, case: &str) -> String {
    let (status, xml) = call_ec2(
        router,
        REGION,
        "CreatePlacementGroup",
        &[("GroupName", "evs-cluster"), ("Strategy", "cluster")],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreatePlacementGroup expected HTTP 200, got {status} {xml}"
    );
    element(&xml, "groupId").unwrap_or_else(|| panic!("case {case:?}: no groupId in {xml}"))
}

/// A Dedicated Host the account really holds, allocated by Amazon EC2's own
/// `AllocateHosts`, and its `HostId`.
async fn dedicated_host(router: &MultiServiceRouter, case: &str) -> String {
    let (status, xml) = call_ec2(
        router,
        REGION,
        "AllocateHosts",
        &[
            ("InstanceType", INSTANCE_TYPE),
            ("AvailabilityZone", ZONE),
            ("Quantity", "1"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: AllocateHosts expected HTTP 200, got {status} {xml}"
    );
    let allocated = elements(&xml, "item");
    assert_eq!(
        allocated.len(),
        1,
        "case {case:?}: expected one allocated Dedicated Host, got {allocated:?}"
    );
    allocated[0].clone()
}

/// A key pair the account really holds, minted by Amazon EC2's own `CreateKeyPair` — the
/// store a host's `keyName` is resolved in.
async fn key_pair(router: &MultiServiceRouter, case: &str, key_name: &str) {
    let (status, xml) = call_ec2(router, REGION, "CreateKeyPair", &[("KeyName", key_name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateKeyPair {key_name:?} expected HTTP 200, got {status} {xml}"
    );
}

/// Every instance id the account's own Amazon EC2 store reports — one per ESX host that
/// was really added, because "An ESX host that runs on an Amazon EC2 bare metal instance"
/// (`Host` doc).
async fn instance_ids(router: &MultiServiceRouter, case: &str) -> Vec<String> {
    let (status, xml) = call_ec2(router, REGION, "DescribeInstances", &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeInstances expected HTTP 200, got {status} {xml}"
    );
    let mut ids = elements(&xml, "instanceId");
    ids.sort();
    ids
}

/// Four `HostInfoForCreate` entries (`hosts` is modelled `len 4..4`), named `esx-1` …
/// `esx-4` so the request order is the `hostName` order `ListEnvironmentHosts` reports,
/// each naming `key_name`, with `extra` merged into the entry at each index of `at`.
fn four_hosts(key_name: &str, extra: &Value, at: &[usize]) -> Value {
    let mut hosts: Vec<Value> = (1..=4)
        .map(|index| {
            json!({
                "hostName": format!("esx-{index}"),
                "keyName": key_name,
                "instanceType": INSTANCE_TYPE,
            })
        })
        .collect();
    for index in at {
        for (member, value) in extra.as_object().expect("the extra members are an object") {
            hosts[*index][member] = value.clone();
        }
    }
    Value::Array(hosts)
}

/// A create request that satisfies every other rule, in a network this account holds.
fn create_request(vpc_id: &str, subnet_id: &str, vcf_version: &str, extra: Value) -> Value {
    let mut request = json!({
        "environmentName": "placement-inertness",
        "vpcId": vpc_id,
        "serviceAccessSubnetId": subnet_id,
        "vcfVersion": vcf_version,
        "termsAccepted": true,
        "initialVlans": vlans_with_prefix(VLAN_PREFIX_LEN),
    });
    for (member, value) in extra.as_object().expect("the extra members are an object") {
        request[member] = value.clone();
    }
    request
}

/// Assert the refusal rule on one response: HTTP 400, a `ValidationException`, and one
/// `fieldList` entry per expected member and no other.
///
/// Not the sibling `create_environment_validation::assert_aggregated_refusal`: that one
/// also requires every clause to NAME the member it refuses, and the clauses Amazon EVS
/// words for an unresolved Amazon EC2 resource name the id or the name that did not
/// resolve instead ("The placement group id … is not valid or does not exist"). Those
/// clauses are pinned where their own rule lives
/// (`tests/all/host_placement_references.rs`, `tests/all/host_key_pair_references.rs`);
/// what this suite asserts is WHICH members a mode draws a clause for.
fn assert_refused(case: &str, status: u16, body: &Value, expected_members: &[&str]) {
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} ({}) {body}",
        error_type(body)
    );
    assert_eq!(
        error_type(body),
        "ValidationException",
        "case {case:?}: expected a ValidationException, got {body}"
    );
    let named: std::collections::BTreeSet<&str> = body["fieldList"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: no fieldList in {body}"))
        .iter()
        .filter_map(|entry| entry["name"].as_str())
        .collect();
    let expected: std::collections::BTreeSet<&str> = expected_members.iter().copied().collect();
    assert_eq!(
        named, expected,
        "case {case:?}: expected one fieldList entry per failed check ({expected:?}), got \
         {named:?} in {body}"
    );
}

/// The environment `GetEnvironment` reports, which is how a create is read back.
async fn get_environment(router: &MultiServiceRouter, case: &str, environment_id: &str) -> Value {
    let (status, body) = call(
        router,
        "GetEnvironment",
        &json!({ "environmentId": environment_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetEnvironment expected HTTP 200, got {status} ({}) {body}",
        error_type(&body)
    );
    body["environment"].clone()
}

/// Every host of the environment, as `ListEnvironmentHosts` reports them, looked for
/// again and again until the environment is past the deployment the clock owes it: a list
/// addressed to an environment that is still initializing is refused
/// (`tests/all/list_while_initializing.rs` owns that rule).
async fn listed_hosts(router: &MultiServiceRouter, case: &str, environment_id: &str) -> Vec<Value> {
    let deadline = std::time::Instant::now() + POLL_DEADLINE;
    loop {
        let (status, body) = call(
            router,
            "ListEnvironmentHosts",
            &json!({ "environmentId": environment_id }),
        )
        .await;
        if status == 200 {
            return body["environmentHosts"]
                .as_array()
                .unwrap_or_else(|| panic!("case {case:?}: no environmentHosts in {body}"))
                .clone();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: ListEnvironmentHosts never answered for {environment_id}; the last \
             answer was {status} ({}) {body}",
            error_type(&body)
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE: a reference an entry of `hosts` carries is resolved against the account's own
/// Amazon EC2 store only in the mode that ADDS those hosts.
///
/// The rows differ along what the rule generalises over: the `vcfVersion` (the mode that
/// installs no VCF, and both versions the model declares that do), which placement member
/// carries the reference, whether the account holds what it names, the id form (the short
/// and long forms both members' patterns publish), and how many entries carry it — so the
/// id an issue cited is one row among many. Every entry of this table names a key pair the
/// account holds, so the only reference a row varies is the placement one; the second test
/// varies the `keyName` as well. The shape's own both-members rule is a row of its own, in
/// the mode that adds no host at all.
#[tokio::test]
async fn a_host_placement_reference_is_resolved_only_where_the_mode_adds_the_hosts() {
    let rows: Vec<(&str, &str, Reference, &[usize], Expected)> = vec![
        (
            "self-deployed, no placement reference at all",
            SELF_DEPLOYED,
            Reference::None,
            &[],
            Expected::Served { hosts: 0 },
        ),
        (
            "self-deployed, a short-form placement group id nothing holds",
            SELF_DEPLOYED,
            Reference::UnheldGroup(UNHELD_GROUP_SHORT),
            &[1],
            Expected::Served { hosts: 0 },
        ),
        (
            "self-deployed, a long-form placement group id nothing holds on the last entry",
            SELF_DEPLOYED,
            Reference::UnheldGroup(UNHELD_GROUP_LONG),
            &[3],
            Expected::Served { hosts: 0 },
        ),
        (
            "self-deployed, a short-form Dedicated Host id nothing holds",
            SELF_DEPLOYED,
            Reference::UnheldDedicatedHost(UNHELD_HOST_SHORT),
            &[0],
            Expected::Served { hosts: 0 },
        ),
        (
            "self-deployed, a long-form Dedicated Host id nothing holds on two entries",
            SELF_DEPLOYED,
            Reference::UnheldDedicatedHost(UNHELD_HOST_LONG),
            &[1, 2],
            Expected::Served { hosts: 0 },
        ),
        (
            "self-deployed, every entry naming a placement group id nothing holds",
            SELF_DEPLOYED,
            Reference::UnheldGroup(UNHELD_GROUP_SHORT),
            &[0, 1, 2, 3],
            Expected::Served { hosts: 0 },
        ),
        (
            "self-deployed, a placement group the account holds",
            SELF_DEPLOYED,
            Reference::HeldGroup,
            &[1],
            Expected::Served { hosts: 0 },
        ),
        (
            "self-deployed, a Dedicated Host the account holds",
            SELF_DEPLOYED,
            Reference::HeldDedicatedHost,
            &[1],
            Expected::Served { hosts: 0 },
        ),
        (
            // The shape's own rule, which holds whenever the object carries both
            // members — the mode decides nothing about it.
            "self-deployed, one entry naming both placement members",
            SELF_DEPLOYED,
            Reference::BothMembers,
            &[1],
            Expected::Refused(&["hosts[1]"]),
        ),
        // The controls: where Amazon EVS INSTALLS VCF the hosts are added, so the
        // references are resolved exactly as before.
        (
            "an installed version, a short-form placement group id nothing holds",
            INSTALLED_VERSIONS[0],
            Reference::UnheldGroup(UNHELD_GROUP_SHORT),
            &[1],
            Expected::Refused(&["hosts[1].placementGroupId"]),
        ),
        (
            "the other installed version, a long-form Dedicated Host id nothing holds on two \
             entries",
            INSTALLED_VERSIONS[1],
            Reference::UnheldDedicatedHost(UNHELD_HOST_LONG),
            &[1, 2],
            Expected::Refused(&["hosts[1].dedicatedHostId", "hosts[2].dedicatedHostId"]),
        ),
        (
            "an installed version, a placement group the account holds",
            INSTALLED_VERSIONS[0],
            Reference::HeldGroup,
            &[1],
            Expected::Served { hosts: 4 },
        ),
        (
            "an installed version, a Dedicated Host the account holds",
            INSTALLED_VERSIONS[0],
            Reference::HeldDedicatedHost,
            &[1],
            Expected::Served { hosts: 4 },
        ),
    ];

    for (case, vcf_version, reference, at, expected) in rows {
        // One store per row: every environment is the only one of its account, so a
        // refused create can be shown to have created nothing at all.
        let router = evs_with_ec2();
        let (vpc_id, subnet_id) = fresh_network(&router, case).await;
        // The SSH key pair every entry of this table names, so a row varies the placement
        // reference alone.
        key_pair(&router, case, HELD_KEY_NAME).await;
        let extra = match reference {
            Reference::None => json!({}),
            Reference::HeldGroup => {
                json!({ "placementGroupId": placement_group(&router, case).await })
            }
            Reference::HeldDedicatedHost => {
                json!({ "dedicatedHostId": dedicated_host(&router, case).await })
            }
            Reference::UnheldGroup(group_id) => json!({ "placementGroupId": group_id }),
            Reference::UnheldDedicatedHost(host_id) => json!({ "dedicatedHostId": host_id }),
            Reference::BothMembers => json!({
                "placementGroupId": placement_group(&router, case).await,
                "dedicatedHostId": UNHELD_HOST_SHORT,
            }),
        };
        let hosts = four_hosts(HELD_KEY_NAME, &extra, at);
        let request = create_request(
            &vpc_id,
            &subnet_id,
            vcf_version,
            json!({ "hosts": hosts.clone() }),
        );
        let (status, body) = call(&router, "CreateEnvironment", &request).await;

        match expected {
            Expected::Served {
                hosts: expected_hosts,
            } => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected HTTP 200, got {status} ({}) {body}",
                    error_type(&body)
                );
                let environment_id = body["environment"]["environmentId"]
                    .as_str()
                    .unwrap_or_else(|| panic!("case {case:?}: no environmentId in {body}"))
                    .to_string();
                assert_eq!(
                    environment_count(&router).await,
                    1,
                    "case {case:?}: expected the served create to have stored its environment"
                );
                // Read back through the service's own reader: the environment the create
                // minted is the one stored, in the mode the request asked for.
                let reported = get_environment(&router, case, &environment_id).await;
                assert_eq!(
                    reported["environmentId"],
                    json!(environment_id),
                    "case {case:?}: GetEnvironment reports {}, expected {environment_id}",
                    reported["environmentId"]
                );
                assert_eq!(
                    reported["vcfVersion"],
                    json!(vcf_version),
                    "case {case:?}: expected vcfVersion {vcf_version}, got {}",
                    reported["vcfVersion"]
                );

                let listed = listed_hosts(&router, case, &environment_id).await;
                assert_eq!(
                    listed.len(),
                    expected_hosts,
                    "case {case:?}: expected the environment to hold {expected_hosts} host(s), \
                     got {listed:?}"
                );
                // What is added carries the references it was asked for; what is not
                // added places nothing, so the account's EC2 store holds one instance per
                // host really added and none at all in the mode that adds none.
                let supplied = hosts.as_array().expect("the hosts are an array");
                for (position, host) in listed.iter().enumerate() {
                    for member in ["placementGroupId", "dedicatedHostId"] {
                        let asked = supplied[position]
                            .get(member)
                            .cloned()
                            .unwrap_or(Value::Null);
                        assert_eq!(
                            host[member], asked,
                            "case {case:?}: host {position}: {member}: expected {asked}, got {}",
                            host[member]
                        );
                    }
                }
                let instances = instance_ids(&router, case).await;
                assert_eq!(
                    instances.len(),
                    expected_hosts,
                    "case {case:?}: expected one EC2 instance per host added \
                     ({expected_hosts}), got {instances:?}"
                );
            }
            Expected::Refused(members) => {
                assert_refused(case, status, &body, members);
                assert_eq!(
                    environment_count(&router).await,
                    0,
                    "case {case:?}: a refused create must store no environment"
                );
                let instances = instance_ids(&router, case).await;
                assert!(
                    instances.is_empty(),
                    "case {case:?}: a refused create must launch no instance, got {instances:?}"
                );
            }
        }
    }
}

/// RULE, at the scope it holds: ONE request carrying every member the self-deployed mode
/// does not support — each with a fact that WOULD be refused where Amazon EVS installs
/// VCF — is served, and the same request in a mode that installs VCF draws one entry per
/// such fact.
///
/// The two rows are the same request in the two kinds of mode, which is what makes this a
/// test of the mode gate rather than of any one member: the placement references, the SSH
/// key pair names, the repeated host DNS hostname, the repeated `vcfHostnames` hostname
/// and the unheld route server peers are all inert in the one mode and all refused in the
/// other.
#[tokio::test]
async fn every_member_the_self_deployed_mode_does_not_support_is_inert_in_one_request() {
    let site_id = json!("broadcom-site-1");
    let license_info = json!([{
        "solutionKey": "AAAAA-BBBBB-CCCCC-DDDDD-EEEEE",
        "vsanKey": "FFFFF-GGGGG-HHHHH-IIIII-JJJJJ",
    }]);
    // `nsx` repeats the hostname `vCenter` already claimed: "Each hostname must be
    // unique" (`VcfHostnames` doc).
    let vcf_hostnames = json!({
        "vCenter": "vcenter", "nsx": "vcenter", "nsxManager1": "nsxm1", "nsxManager2": "nsxm2",
        "nsxManager3": "nsxm3", "nsxEdge1": "nsxe1", "nsxEdge2": "nsxe2",
        "sddcManager": "sddc", "cloudBuilder": "builder",
    });
    // Two peers, as the member's own `@length(2..2)` requires, neither of which the
    // account holds.
    let connectivity_info = json!({
        "privateRouteServerPeerings": ["rsp-00000000", "rsp-00000001"],
    });
    // Four entries: every one naming an SSH key pair the account holds none of, one also
    // naming a placement group nothing holds, one also naming a Dedicated Host nothing
    // holds, and one repeating the DNS hostname of the first.
    let hosts = json!([
        { "hostName": "esx-1", "keyName": UNHELD_KEY_NAME, "instanceType": INSTANCE_TYPE },
        {
            "hostName": "esx-2", "keyName": UNHELD_KEY_NAME, "instanceType": INSTANCE_TYPE,
            "placementGroupId": UNHELD_GROUP_SHORT,
        },
        { "hostName": "esx-1", "keyName": UNHELD_KEY_NAME, "instanceType": INSTANCE_TYPE },
        {
            "hostName": "esx-4", "keyName": UNHELD_KEY_NAME, "instanceType": INSTANCE_TYPE,
            "dedicatedHostId": UNHELD_HOST_LONG,
        },
    ]);

    let rows: Vec<(&str, &str, Option<&[&str]>)> = vec![
        (
            "the mode that installs no VCF reads a fact out of none of them",
            SELF_DEPLOYED,
            None,
        ),
        (
            "the same request where Amazon EVS installs VCF",
            INSTALLED_VERSIONS[0],
            Some(&[
                "hosts[0].keyName",
                "hosts[1].keyName",
                "hosts[2].keyName",
                "hosts[3].keyName",
                "hosts[1].placementGroupId",
                "hosts[2].hostName",
                "hosts[3].dedicatedHostId",
                "vcfHostnames.nsx",
                "connectivityInfo.privateRouteServerPeerings[0]",
                "connectivityInfo.privateRouteServerPeerings[1]",
            ]),
        ),
    ];

    for (case, vcf_version, refused_members) in rows {
        let router = evs_with_ec2();
        let (vpc_id, subnet_id) = fresh_network(&router, case).await;
        let request = create_request(
            &vpc_id,
            &subnet_id,
            vcf_version,
            json!({
                "siteId": site_id.clone(),
                "licenseInfo": license_info.clone(),
                "vcfHostnames": vcf_hostnames.clone(),
                "connectivityInfo": connectivity_info.clone(),
                "hosts": hosts.clone(),
            }),
        );
        let (status, body) = call(&router, "CreateEnvironment", &request).await;

        let Some(members) = refused_members else {
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200, got {status} ({}) {body}",
                error_type(&body)
            );
            let environment_id = body["environment"]["environmentId"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: no environmentId in {body}"))
                .to_string();
            // Every member the mode does not support is reported back exactly as
            // supplied, and nothing was derived from one: no host was added, so no
            // instance was launched and no placement was made.
            let reported = get_environment(&router, case, &environment_id).await;
            for (member, asked) in [
                ("siteId", &site_id),
                ("licenseInfo", &license_info),
                ("vcfHostnames", &vcf_hostnames),
                ("connectivityInfo", &connectivity_info),
            ] {
                assert_eq!(
                    &reported[member], asked,
                    "case {case:?}: expected {member} to be reported as supplied ({asked}), got {}",
                    reported[member]
                );
            }
            let listed = listed_hosts(&router, case, &environment_id).await;
            assert!(
                listed.is_empty(),
                "case {case:?}: a mode that adds no host holds none, got {listed:?}"
            );
            let instances = instance_ids(&router, case).await;
            assert!(
                instances.is_empty(),
                "case {case:?}: a mode that adds no host launches no instance, got {instances:?}"
            );
            continue;
        };
        assert_refused(case, status, &body, members);
        assert_eq!(
            environment_count(&router).await,
            0,
            "case {case:?}: a refused create must store no environment"
        );
    }
}
