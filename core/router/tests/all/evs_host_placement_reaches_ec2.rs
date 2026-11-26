// SPDX-License-Identifier: CC0-1.0

//! Where an Amazon EVS host is PLACED is recorded on the Amazon EC2 instance that host IS.
//!
//! RULE: a service that launches a resource into another service's store records the
//! placement its request named on the record it writes there, in the members the owning
//! service records a launch's placement in, and the members that owning service DERIVES
//! from them follow. An Amazon EVS host is "an ESX host that runs on an Amazon EC2 bare
//! metal instance" (`Host` doc) placed by "the unique ID of the placement group where the
//! host is placed" (`HostInfoForCreate$placementGroupId`) or "the unique ID of the Amazon
//! EC2 Dedicated Host" (`$dedicatedHostId`) — resources of the caller's own account that
//! Amazon EVS does not create — so the instance it launches reports that Dedicated Host as
//! its `Placement$HostId` and that group as its `Placement$GroupName` / `$GroupId`, and the
//! Dedicated Host's own `Instances` ("the IDs and instance type that are currently running
//! on the Dedicated Host", `Host$Instances`) reports that instance. The dependency a
//! release has to honour reads that one list — "you must stop or terminate all instances on
//! a host before it can be released" (`ReleaseHosts`) — so a Dedicated Host carrying an ESX
//! host is not released under it.
//!
//! Both services are mounted on ONE shared engine, the way the assembled router mounts
//! them, and every call goes over the wire a client speaks — awsJson1_0 for Amazon EVS,
//! ec2Query for Amazon EC2 — so what is asserted is what a client reads from the two
//! services about one host.
//!
//! The rows span the placement a request can name: each of the two mechanisms, the two
//! placement group strategies Amazon EVS names for the group one ("you can use the
//! placementGroupId parameter to specify a cluster or partition placement group",
//! `CreateEnvironmentHost` doc), a second Dedicated Host so one host's instance never
//! reaches another's list, and a host that names no placement at all — and the two
//! operations that add a host, `CreateEnvironmentHost` and the `hosts` an environment is
//! created with.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{block_of, element, elements, form_encode, items_of, opt_element};

/// The Region every request below is signed for, and so the Region both services serve it
/// in.
const REGION: &str = "us-west-2";

/// The single Availability Zone the network and the Dedicated Hosts live in: "Amazon EVS
/// requires that all VPC subnets exist in a single Availability Zone" (`vpcId` doc).
const ZONE: &str = "us-west-2a";

/// The EC2 API version a client sends in an ec2Query body.
const EC2_API_VERSION: &str = "2016-11-15";

/// The bare metal instance type every host below runs on, and every Dedicated Host is
/// allocated for.
const INSTANCE_TYPE: &str = "i4i.metal";

/// The SSH key pair name every host is created with.
const KEY_NAME: &str = "evs-key";

/// The `KeyType` the account's key pair is minted as: the cheap member of Amazon EC2's
/// modelled `KeyType` catalog, a host caring only that the ACCOUNT holds the pair its
/// `keyName` names.
const KEY_TYPE: &str = "ed25519";

/// The code Amazon EC2 answers a create of a key pair name the account already holds
/// (`InvalidKeyPair.Duplicate`), which is how [`ensure_key_pair`] stays idempotent.
const DUPLICATE_KEY_PAIR_CODE: &str = "InvalidKeyPair.Duplicate";

/// The VCF version the environments are created with, one the version catalogue offers and
/// one that installs VCF — the mode in which an environment adds the `hosts` it was created
/// with.
const VCF_VERSION: &str = "VCF-5.2.1";

/// The `Support` plan this account holds: an Amazon EVS environment is created only for an
/// account subscribed to Premium Support, a fact the Support store owns.
const QUALIFYING_SUPPORT_PLAN: &str = "enterprise";

/// The `EnvironmentState` a host can be added to: "this action can only be used after the
/// Amazon EVS environment is deployed" (`CreateEnvironmentHost` doc).
const ENVIRONMENT_CREATED: &str = "CREATED";

/// The `UnsuccessfulItemError$Code` Amazon EC2 reports for a Dedicated Host it will not
/// release because instances are still running on it.
const HOST_OCCUPIED_CODE: &str = "Client.InvalidHost.Occupied";

/// The `AllocationState` a released Dedicated Host reports.
const HOST_STATE_RELEASED: &str = "released";

/// The ten `InitialVlans` members an environment is provisioned with, in the order the
/// model declares them.
const VLAN_MEMBERS: [&str; 10] = [
    "vmkManagement",
    "vmManagement",
    "vMotion",
    "vSan",
    "vTep",
    "edgeVTep",
    "nsxUplink",
    "hcx",
    "expansionVlan1",
    "expansionVlan2",
];

/// How long a poll waits for a state the clock owes before it fails the test: a generous
/// deadline, not a measurement — nothing below asserts how long a transition took.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits between two looks.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// One emulator carrying Amazon EVS and Amazon EC2 over ONE shared engine, as the
/// production router mounts them — the whole point, because the instance an Amazon EVS host
/// runs on, the placement group it is in and the Dedicated Host it is on are all records of
/// the account's own EC2 store.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // These suites exercise behaviour, not signing.
    router.set_request_authentication(false);
    // The account's Support plan, in the store that owns it: an operator grants it out of
    // band, and Amazon EVS reads it from there when it creates an environment.
    service_com_amazonaws_support_awssupport_20130415::state::service_state(&*engine)
        .expect("the in-memory engine carries the Support store")
        .lock()
        .support_plan = Some(QUALIFYING_SUPPORT_PLAN.to_string());
    router.register_service(
        service_com_amazonaws_evs_amazonelasticvmwareservice::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`, which is
/// what routes the request and which Region it is served in; the signature is never
/// checked.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// POST one ec2Query action, as its HTTP status and XML body.
async fn ec2_call(
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
        .expect("a well-formed ec2Query request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// One ec2Query action that must be served, as its XML body.
async fn ec2(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let (status, body) = ec2_call(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: EC2 {action} expected HTTP 200, got {status} with {body}"
    );
    body
}

/// POST one Amazon EVS operation over awsJson1_0, as its HTTP status and parsed body.
async fn evs_call(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("evs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header(
            "x-amz-target",
            format!("AmazonElasticVMwareService.{operation}"),
        )
        .header("authorization", authorization("evs"))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed awsJson1_0 request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One Amazon EVS operation that must be served, as its parsed body.
async fn evs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = evs_call(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: EVS {operation} expected HTTP 200, got {status} with {parsed}"
    );
    parsed
}

/// A VPC and one subnet inside it, created through Amazon EC2 ITSELF, so both ids name
/// resources the account really holds; `base` is the first two octets of the VPC's range,
/// so each environment gets its own address space.
async fn create_network(router: &MultiServiceRouter, case: &str, base: &str) -> (String, String) {
    let created = ec2(
        router,
        case,
        "CreateVpc",
        &[("CidrBlock", &format!("{base}.0.0/16"))],
    )
    .await;
    let vpc_id = element(&created, "vpcId");
    let subnet = ec2(
        router,
        case,
        "CreateSubnet",
        &[
            ("VpcId", &vpc_id),
            // High in the VPC's range, clear of the VLAN blocks an environment is
            // provisioned with below.
            ("CidrBlock", &format!("{base}.200.0/24")),
            ("AvailabilityZone", ZONE),
        ],
    )
    .await;
    let subnet_id = element(&subnet, "subnetId");
    (vpc_id, subnet_id)
}

/// The ten VLAN subnet blocks an environment is created with, each in its own `/28` of the
/// VPC's range so none overlaps another or the service access subnet.
fn initial_vlans(base: &str) -> Value {
    let mut vlans = json!({});
    for (index, member) in VLAN_MEMBERS.iter().enumerate() {
        vlans[*member] = json!({ "cidr": format!("{base}.{index}.0/27") });
    }
    vlans
}

/// One Dedicated Host of the account, allocated through Amazon EC2's own `AllocateHosts`,
/// as its `HostId`.
async fn allocate_dedicated_host(router: &MultiServiceRouter, case: &str) -> String {
    let allocated = ec2(
        router,
        case,
        "AllocateHosts",
        &[
            ("InstanceType", INSTANCE_TYPE),
            ("AvailabilityZone", ZONE),
            ("Quantity", "1"),
        ],
    )
    .await;
    let ids = elements(&allocated, "item");
    assert_eq!(
        ids.len(),
        1,
        "case {case:?}: expected one allocated Dedicated Host, got {ids:?} in {allocated}"
    );
    ids[0].clone()
}

/// One placement group of the account, created through Amazon EC2's own
/// `CreatePlacementGroup`, as its `GroupId`.
///
/// `partition_count` is given only for the `partition` strategy, which is the only strategy
/// that has partitions ("valid only when Strategy is set to partition",
/// `CreatePlacementGroupRequest$PartitionCount`).
async fn create_placement_group(
    router: &MultiServiceRouter,
    case: &str,
    group_name: &str,
    strategy: &str,
    partition_count: Option<&str>,
) -> String {
    let mut params = vec![("GroupName", group_name), ("Strategy", strategy)];
    if let Some(count) = partition_count {
        params.push(("PartitionCount", count));
    }
    let created = ec2(router, case, "CreatePlacementGroup", &params).await;
    let group_id = element(&created, "groupId");
    assert_eq!(
        element(&created, "groupName"),
        group_name,
        "case {case:?}: the group created must carry the name asked for, got {created}"
    );
    group_id
}

/// The SSH key pair the account holds, minted through Amazon EC2's own `CreateKeyPair`.
///
/// A host's `keyName` is "the name of the SSH key that is used to access the host" — an
/// Amazon EC2 key pair of the caller's own account, which Amazon EVS does not create — so
/// every environment and host below names a pair that really is in that account's key pair
/// store. Idempotent: a second call for a name the account already holds is Amazon EC2's
/// own duplicate refusal and nothing else, so a test that deploys several environments
/// mints the pair once.
async fn ensure_key_pair(router: &MultiServiceRouter, case: &str) {
    let (status, body) = ec2_call(
        router,
        "CreateKeyPair",
        &[("KeyName", KEY_NAME), ("KeyType", KEY_TYPE)],
    )
    .await;
    if status == 200 {
        return;
    }
    assert!(
        body.contains(DUPLICATE_KEY_PAIR_CODE),
        "case {case:?}: CreateKeyPair {KEY_NAME:?} expected HTTP 200 or \
         {DUPLICATE_KEY_PAIR_CODE}, got {status} with {body}"
    );
}

/// An Amazon EVS environment deployed into its own network, waited for until it reports the
/// state a host can be added to; `hosts` is the `hosts` member the create carries, when it
/// carries one.
async fn deployed_environment(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    base: &str,
    hosts: Option<Value>,
) -> String {
    let (vpc_id, subnet_id) = create_network(router, case, base).await;
    // Every host this environment takes, now or later, names the account's key pair.
    ensure_key_pair(router, case).await;
    let mut request = json!({
        "environmentName": name,
        "vpcId": vpc_id,
        "serviceAccessSubnetId": subnet_id,
        "vcfVersion": VCF_VERSION,
        "termsAccepted": true,
        "initialVlans": initial_vlans(base),
    });
    if let Some(hosts) = hosts {
        request["hosts"] = hosts;
    }
    let created = evs(router, case, "CreateEnvironment", &request).await;
    let environment_id = created["environment"]["environmentId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no environmentId in {created}"))
        .to_string();

    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let got = evs(
            router,
            case,
            "GetEnvironment",
            &json!({ "environmentId": &environment_id }),
        )
        .await;
        let state = got["environment"]["environmentState"]
            .as_str()
            .unwrap_or("<none>")
            .to_string();
        if state == ENVIRONMENT_CREATED {
            return environment_id;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the environment {environment_id} never reached \
             {ENVIRONMENT_CREATED}; last state {state:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Add one host to `environment_id` and answer the `Host` Amazon EVS reported for it.
///
/// `placement` carries the placement members of the request's `HostInfoForCreate`, which is
/// what the rows below differ in.
async fn create_host(
    router: &MultiServiceRouter,
    case: &str,
    environment_id: &str,
    host_name: &str,
    placement: &Value,
) -> Value {
    let mut host = json!({
        "hostName": host_name,
        "keyName": KEY_NAME,
        "instanceType": INSTANCE_TYPE,
    });
    for (member, value) in placement.as_object().into_iter().flatten() {
        host[member] = value.clone();
    }
    let created = evs(
        router,
        case,
        "CreateEnvironmentHost",
        &json!({ "environmentId": environment_id, "host": host }),
    )
    .await;
    created["host"].clone()
}

/// What `DescribeInstances` reports in one instance's `Placement`: where the LAUNCH put it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Placement {
    /// `Placement$HostId` — the Dedicated Host the instance resides on, absent for an
    /// instance on none.
    host_id: Option<String>,
    /// `Placement$GroupName` — "the name of the placement group that the instance is in",
    /// the EMPTY string for an instance in none.
    group_name: String,
    /// `Placement$GroupId` — that same group by id, absent for an instance in none.
    group_id: Option<String>,
}

/// The placement `DescribeInstances` reports for `instance_id`, read through a describe
/// FILTERED to that id, so every member comes from that one instance.
async fn placement_of(router: &MultiServiceRouter, case: &str, instance_id: &str) -> Placement {
    let xml = ec2(
        router,
        case,
        "DescribeInstances",
        &[("InstanceId.1", instance_id)],
    )
    .await;
    let reported: BTreeSet<String> = elements(&xml, "instanceId").into_iter().collect();
    assert_eq!(
        reported,
        BTreeSet::from([instance_id.to_string()]),
        "case {case:?}: the describe is filtered to {instance_id}, so it must report that \
         instance and no other, got {reported:?} in {xml}"
    );
    let placement = block_of(&xml, "placement")
        .unwrap_or_else(|| panic!("case {case:?}: no <placement> for {instance_id} in {xml}"));
    Placement {
        host_id: opt_element(placement, "hostId"),
        group_name: opt_element(placement, "groupName").unwrap_or_default(),
        group_id: opt_element(placement, "groupId"),
    }
}

/// The `(instanceId, instanceType)` pairs the Dedicated Host `host_id` reports as running
/// on it, as `DescribeHosts` reports them.
async fn host_instances(
    router: &MultiServiceRouter,
    case: &str,
    host_id: &str,
) -> BTreeSet<(String, String)> {
    let xml = ec2(router, case, "DescribeHosts", &[("HostId.1", host_id)]).await;
    assert_eq!(
        elements(&xml, "hostId"),
        vec![host_id.to_string()],
        "case {case:?}: the describe is filtered to {host_id}, so it must report that host \
         and no other, got {xml}"
    );
    let Some(instances) = block_of(&xml, "instances") else {
        return BTreeSet::new();
    };
    items_of(instances)
        .into_iter()
        .map(|item| {
            (
                opt_element(item, "instanceId").unwrap_or_default(),
                opt_element(item, "instanceType").unwrap_or_default(),
            )
        })
        .collect()
}

/// The `(code, message)` `ReleaseHosts` reported for `host_id` in `Unsuccessful`, or `None`
/// when it released the host.
async fn release_host(
    router: &MultiServiceRouter,
    case: &str,
    host_id: &str,
) -> Option<(String, String)> {
    let xml = ec2(router, case, "ReleaseHosts", &[("HostId.1", host_id)]).await;
    let unsuccessful = block_of(&xml, "unsuccessful")?;
    let item = items_of(unsuccessful).into_iter().next()?;
    assert_eq!(
        opt_element(item, "resourceId").as_deref(),
        Some(host_id),
        "case {case:?}: an unsuccessful item must name the host it is about, got {xml}"
    );
    Some((
        opt_element(item, "code").unwrap_or_default(),
        opt_element(item, "message").unwrap_or_default(),
    ))
}

/// The `AllocationState` `DescribeHosts` reports for `host_id`.
async fn host_state(router: &MultiServiceRouter, case: &str, host_id: &str) -> String {
    let xml = ec2(router, case, "DescribeHosts", &[("HostId.1", host_id)]).await;
    let host = block_of(&xml, "item")
        .unwrap_or_else(|| panic!("case {case:?}: no host {host_id} in {xml}"));
    opt_element(host, "state").unwrap_or_default()
}

/// The `ec2InstanceId` an Amazon EVS `Host` reports, which a created host must carry.
fn instance_of(host: &Value, case: &str) -> String {
    host["ec2InstanceId"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: a host must report the EC2 instance it runs on, got {host}")
        })
        .to_string()
}

/// One row: the placement a request names for one host, and what the instance that host IS
/// must then report.
struct Row {
    /// What the row is, named in every assertion message.
    case: &'static str,
    /// The DNS hostname the row's host is created with.
    host_name: &'static str,
    /// The placement members the request carries.
    placement: Value,
    /// What `DescribeInstances` must report for the instance of this host.
    expected: Placement,
}

/// The rows of this suite, built from placement resources the account really holds.
async fn rows(router: &MultiServiceRouter) -> (Vec<Row>, String, String, String) {
    let setup = "the account's placement resources";
    // Two Dedicated Hosts, so a host placed on one never reaches the other's list.
    let first_host = allocate_dedicated_host(router, setup).await;
    let second_host = allocate_dedicated_host(router, setup).await;
    // A host allocated and placed on by nothing, which is what keeps the list derived
    // rather than assumed.
    let empty_host = allocate_dedicated_host(router, setup).await;
    // The two strategies Amazon EVS names for a host's group: "you can use the
    // placementGroupId parameter to specify a cluster or partition placement group"
    // (`CreateEnvironmentHost` doc). The partition group is created with a count inside the
    // published range ("between 1 and 7", `CreatePlacementGroupRequest$PartitionCount`).
    let cluster_name = "evs-cluster-group";
    let cluster_group = create_placement_group(router, setup, cluster_name, "cluster", None).await;
    let partition_name = "evs_partition-group-2";
    let partition_group =
        create_placement_group(router, setup, partition_name, "partition", Some("3")).await;

    let rows = vec![
        Row {
            case: "a host on a Dedicated Host",
            host_name: "esx-dedicated-1",
            placement: json!({ "dedicatedHostId": first_host.clone() }),
            expected: Placement {
                host_id: Some(first_host.clone()),
                group_name: String::new(),
                group_id: None,
            },
        },
        Row {
            case: "a host on a second Dedicated Host",
            host_name: "esx-dedicated-2",
            placement: json!({ "dedicatedHostId": second_host.clone() }),
            expected: Placement {
                host_id: Some(second_host.clone()),
                group_name: String::new(),
                group_id: None,
            },
        },
        Row {
            case: "a host in a cluster placement group",
            host_name: "esx-cluster",
            placement: json!({ "placementGroupId": cluster_group.clone() }),
            expected: Placement {
                host_id: None,
                group_name: cluster_name.to_string(),
                group_id: Some(cluster_group.clone()),
            },
        },
        Row {
            case: "a host in a partition placement group",
            host_name: "esx-partition",
            placement: json!({ "placementGroupId": partition_group.clone() }),
            expected: Placement {
                host_id: None,
                group_name: partition_name.to_string(),
                group_id: Some(partition_group.clone()),
            },
        },
        Row {
            case: "a host that names no placement",
            host_name: "esx-unplaced",
            placement: json!({}),
            expected: Placement::default(),
        },
    ];
    (rows, first_host, second_host, empty_host)
}

/// RULE, at `CreateEnvironmentHost`: the instance a host IS reports the placement the
/// request named for that host, and the Dedicated Host's own `Instances` follows.
#[tokio::test]
async fn a_created_hosts_placement_reaches_the_instance_and_its_dedicated_host() {
    let router = emulator();
    let (rows, first_host, second_host, empty_host) = rows(&router).await;
    let environment = deployed_environment(
        &router,
        "placement propagation",
        "placement-propagation",
        "10.0",
        None,
    )
    .await;

    let mut instances: Vec<String> = Vec::new();
    for row in &rows {
        let case = row.case;
        let host = create_host(
            &router,
            case,
            &environment,
            row.host_name,
            &row.placement,
        )
        .await;
        // Amazon EVS reports the placement it was given: the two services are compared on
        // the same host, so this is one half of "the two stores agree".
        for member in ["dedicatedHostId", "placementGroupId"] {
            let asked = row.placement.get(member).cloned();
            let reported = host.get(member).cloned().filter(|value| !value.is_null());
            assert_eq!(
                reported, asked,
                "case {case:?}: the host must report the {member} its request named, got \
                 {host}"
            );
        }
        let instance_id = instance_of(&host, case);
        let got = placement_of(&router, case, &instance_id).await;
        assert_eq!(
            got, row.expected,
            "case {case:?}: expected the instance {instance_id} to report {:?}, got {got:?}",
            row.expected
        );
        instances.push(instance_id);
    }

    // The Dedicated Host's `Instances` is RECOMPUTED from where the account's instances
    // were launched, so each host reports exactly the ESX host instance placed on it — with
    // the instance type that instance runs — and the host nothing was placed on reports
    // nothing.
    for (host_id, expected) in [
        (&first_host, Some(instances[0].clone())),
        (&second_host, Some(instances[1].clone())),
        (&empty_host, None),
    ] {
        let case = "the Dedicated Hosts' own instances";
        let got = host_instances(&router, case, host_id).await;
        let wanted: BTreeSet<(String, String)> = expected
            .into_iter()
            .map(|instance_id| (instance_id, INSTANCE_TYPE.to_string()))
            .collect();
        assert_eq!(
            got, wanted,
            "case {case:?}: {host_id} must report exactly {wanted:?}, got {got:?}"
        );
    }

    // And that one list is what a release honours: a Dedicated Host carrying a running ESX
    // host is reported in `Unsuccessful` instead of released under it, while the host
    // carrying nothing is released.
    let occupied = release_host(&router, "releasing an occupied host", &first_host).await;
    let (code, message) = occupied.unwrap_or_else(|| {
        panic!(
            "a Dedicated Host running the ESX host instance {} must not be released",
            instances[0]
        )
    });
    assert_eq!(
        code, HOST_OCCUPIED_CODE,
        "expected {HOST_OCCUPIED_CODE} for the occupied host {first_host}, got \
         {code:?} with {message:?}"
    );
    assert!(
        message.contains(&first_host),
        "the refusal must name the host it is about, got {message:?}"
    );
    assert_ne!(
        host_state(&router, "releasing an occupied host", &first_host).await,
        HOST_STATE_RELEASED,
        "an occupied Dedicated Host must stay allocated"
    );
    assert_eq!(
        release_host(&router, "releasing an empty host", &empty_host).await,
        None,
        "a Dedicated Host carrying nothing must be released"
    );
    assert_eq!(
        host_state(&router, "releasing an empty host", &empty_host).await,
        HOST_STATE_RELEASED,
        "the released Dedicated Host must report that it was released"
    );
}

/// RULE, at the other operation that adds hosts: the `hosts` an environment is CREATED with
/// record their placement on their own instances in the same way.
///
/// "The ESX hosts to add to the environment" (`CreateEnvironmentRequest$hosts`) takes
/// exactly four entries, so the four span the placement forms: each mechanism, a second
/// group strategy, and no placement at all.
#[tokio::test]
async fn the_hosts_an_environment_is_created_with_record_their_own_placement() {
    let router = emulator();
    let setup = "the account's placement resources";
    let dedicated_host = allocate_dedicated_host(&router, setup).await;
    let cluster_name = "env-cluster-group";
    let cluster_group = create_placement_group(&router, setup, cluster_name, "cluster", None).await;
    let partition_name = "env-partition-group";
    let partition_group =
        create_placement_group(&router, setup, partition_name, "partition", None).await;

    let case = "an environment's own hosts";
    let hosts = json!([
        {
            "hostName": "esx-env-0",
            "keyName": KEY_NAME,
            "instanceType": INSTANCE_TYPE,
            "placementGroupId": cluster_group,
        },
        {
            "hostName": "esx-env-1",
            "keyName": KEY_NAME,
            "instanceType": INSTANCE_TYPE,
            "dedicatedHostId": dedicated_host,
        },
        {
            "hostName": "esx-env-2",
            "keyName": KEY_NAME,
            "instanceType": INSTANCE_TYPE,
            "placementGroupId": partition_group,
        },
        {
            "hostName": "esx-env-3",
            "keyName": KEY_NAME,
            "instanceType": INSTANCE_TYPE,
        },
    ]);
    let environment =
        deployed_environment(&router, case, "placement-at-create", "10.1", Some(hosts)).await;

    let listed = evs(
        &router,
        case,
        "ListEnvironmentHosts",
        &json!({ "environmentId": environment }),
    )
    .await;
    let listed = listed["environmentHosts"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: no environmentHosts in {listed}"))
        .clone();
    assert_eq!(
        listed.len(),
        4,
        "case {case:?}: the environment must hold the four hosts it was created with, got \
         {listed:?}"
    );

    let expected: Vec<(&str, Placement)> = vec![
        (
            "esx-env-0",
            Placement {
                host_id: None,
                group_name: cluster_name.to_string(),
                group_id: Some(cluster_group.clone()),
            },
        ),
        (
            "esx-env-1",
            Placement {
                host_id: Some(dedicated_host.clone()),
                group_name: String::new(),
                group_id: None,
            },
        ),
        (
            "esx-env-2",
            Placement {
                host_id: None,
                group_name: partition_name.to_string(),
                group_id: Some(partition_group.clone()),
            },
        ),
        ("esx-env-3", Placement::default()),
    ];
    let mut on_the_dedicated_host: BTreeSet<(String, String)> = BTreeSet::new();
    for (host_name, want) in expected {
        let host = listed
            .iter()
            .find(|host| host["hostName"] == json!(host_name))
            .unwrap_or_else(|| panic!("case {host_name:?}: no such host in {listed:?}"));
        let instance_id = instance_of(host, host_name);
        let got = placement_of(&router, host_name, &instance_id).await;
        assert_eq!(
            got, want,
            "case {host_name:?}: expected the instance {instance_id} to report {want:?}, got \
             {got:?}"
        );
        if want.host_id.is_some() {
            on_the_dedicated_host.insert((instance_id, INSTANCE_TYPE.to_string()));
        }
    }
    assert_eq!(
        host_instances(&router, case, &dedicated_host).await,
        on_the_dedicated_host,
        "case {case:?}: {dedicated_host} must report exactly the host placed on it"
    );
}
