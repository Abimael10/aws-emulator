// SPDX-License-Identifier: CC0-1.0

//! ONE Availability Zone per Amazon EVS environment, for every resource that environment
//! places in the account's Amazon EC2 store.
//!
//! RULE: an environment's Availability Zone is the zone of the subnet the caller gave it
//! for service access, and the Region's first zone when the account's store holds no such
//! subnet — "Amazon EVS requires that all VPC subnets exist in a single Availability Zone
//! in a Region where the service is available" (`vpcId` doc), and the service access
//! subnet is one of those subnets. So every resource an Amazon EVS operation places for
//! one environment is placed in that ONE zone, whichever operation places it: the VLAN
//! subnets `CreateEnvironment` provisions, the instance each host the environment is
//! created WITH runs on, and the instance of a host `CreateEnvironmentHost` adds later.
//! A client therefore cannot read two zones for one environment, whichever of the three
//! it asks about.
//!
//! The rows differ along what the rule generalises over — the Region, and so the zone's
//! own Region; WHICH of that Region's published zones the service access subnet is in
//! (its first, its last, one in the middle); and whether the subnet's zone was named by
//! the caller at all or picked by `CreateSubnet` for a request that named none. The
//! recorded case (a subnet in the first zone of `us-west-2`) is one row.
//!
//! Both services are mounted on ONE shared engine, the way the assembled router mounts
//! them, and every call goes over the wire a client speaks — awsJson1_0 for Amazon EVS,
//! ec2Query for Amazon EC2 — so what is asserted is what a client reads from the two
//! services about one environment.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{block_of, element, elements, form_encode, opt_element};

/// The EC2 API version a client sends in an ec2Query body.
const EC2_API_VERSION: &str = "2016-11-15";

/// The bare metal instance type every host below runs on.
const INSTANCE_TYPE: &str = "i4i.metal";

/// The SSH key pair name every host is created with.
const KEY_NAME: &str = "evs-zone-key";

/// The `KeyType` the account's key pair is minted as: the cheap member of Amazon EC2's
/// modelled `KeyType` catalog, a host caring only that the ACCOUNT holds the pair its
/// `keyName` names.
const KEY_TYPE: &str = "ed25519";

/// The code Amazon EC2 answers a create of a key pair name the account already holds
/// (`InvalidKeyPair.Duplicate`), which is how [`ensure_key_pair`] stays idempotent.
const DUPLICATE_KEY_PAIR_CODE: &str = "InvalidKeyPair.Duplicate";

/// The VCF version the environments are created with: one the version catalogue offers and
/// one that installs VCF, which is the mode in which an environment adds the `hosts` it was
/// created with.
const VCF_VERSION: &str = "VCF-5.2.1";

/// The `Support` plan this account holds: an environment is created only for an account
/// subscribed to Premium Support, a fact the Support store owns.
const QUALIFYING_SUPPORT_PLAN: &str = "enterprise";

/// The `EnvironmentState` a list of an environment's resources is answered in, and the
/// state a host can be added in: "this action can only be used after the Amazon EVS
/// environment is deployed" (`CreateEnvironmentHost` doc).
const ENVIRONMENT_CREATED: &str = "CREATED";

/// The `Subnet$Type` Amazon EC2 reports for a subnet Amazon EVS provisioned: "Possible
/// values are `Elastic VMware Service` or no value" (`Subnet$Type` doc, EC2's model).
const EVS_SUBNET_TYPE: &str = "Elastic VMware Service";

/// The ten `InitialVlans` members, in the order the model declares them.
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

/// How many hosts an environment is created with: `CreateEnvironmentRequest$hosts` takes
/// exactly four entries.
const HOSTS_AT_CREATE: usize = 4;

/// How long a poll waits for a state the clock owes before it fails the test: a generous
/// deadline, not a measurement — nothing below asserts how long a transition took.
const POLL_DEADLINE: Duration = Duration::from_secs(120);

/// How long a poll waits between two looks.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// One emulator carrying Amazon EVS and Amazon EC2 over ONE shared engine, as the
/// production router mounts them — the point of this suite, because the service access
/// subnet, the VLAN subnets and the instance each host runs on are all records of the
/// account's own EC2 store.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // This suite exercises behaviour, not signing.
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

/// A SigV4 `Authorization` header whose credential scope names `signing_name` and
/// `region`, which is what routes the request and which Region it is served in; the
/// signature is never checked.
fn authorization(region: &str, signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// POST one ec2Query action to `region`, as its HTTP status and XML body.
async fn ec2_call(
    router: &MultiServiceRouter,
    region: &str,
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
        .header("host", format!("ec2.{region}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(region, "ec2"))
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
    region: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let (status, body) = ec2_call(router, region, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: EC2 {action} expected HTTP 200, got {status} with {body}"
    );
    body
}

/// POST one Amazon EVS operation to `region` over awsJson1_0, as its HTTP status and
/// parsed body.
async fn evs_call(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("evs.{region}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header(
            "x-amz-target",
            format!("AmazonElasticVMwareService.{operation}"),
        )
        .header("authorization", authorization(region, "evs"))
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
async fn evs(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, parsed) = evs_call(router, region, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: EVS {operation} expected HTTP 200, got {status} with {parsed}"
    );
    parsed
}

/// A VPC and one subnet inside it, created through Amazon EC2 ITSELF, so both ids name
/// resources the account really holds.
///
/// `base` is the first two octets of the VPC's range, so each environment gets its own
/// address space, and `zone` is the zone the service access subnet is created in — `None`
/// leaves the zone to `CreateSubnet`, which picks one for a request that names none.
async fn create_network(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    base: &str,
    zone: Option<&str>,
) -> (String, String) {
    let created = ec2(
        router,
        case,
        region,
        "CreateVpc",
        &[("CidrBlock", &format!("{base}.0.0/16"))],
    )
    .await;
    let vpc_id = element(&created, "vpcId");
    // High in the VPC's range, clear of the VLAN blocks an environment is provisioned with
    // below.
    let cidr = format!("{base}.200.0/24");
    let mut params = vec![("VpcId", vpc_id.as_str()), ("CidrBlock", cidr.as_str())];
    if let Some(zone) = zone {
        params.push(("AvailabilityZone", zone));
    }
    let subnet = ec2(router, case, region, "CreateSubnet", &params).await;
    let subnet_id = element(&subnet, "subnetId");
    (vpc_id, subnet_id)
}

/// The ten VLAN subnet blocks an environment is created with, each in its own `/27` of the
/// VPC's range so none overlaps another or the service access subnet.
fn initial_vlans(base: &str) -> Value {
    let mut vlans = json!({});
    for (index, member) in VLAN_MEMBERS.iter().enumerate() {
        vlans[*member] = json!({ "cidr": format!("{base}.{index}.0/27") });
    }
    vlans
}

/// The `(availabilityZone, availabilityZoneId)` Amazon EC2 reports for one subnet, read
/// through a describe FILTERED to that subnet so both values come from it.
async fn subnet_zone(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    subnet_id: &str,
) -> (String, String) {
    let xml = ec2(
        router,
        case,
        region,
        "DescribeSubnets",
        &[("SubnetId.1", subnet_id)],
    )
    .await;
    assert_eq!(
        elements(&xml, "subnetId"),
        vec![subnet_id.to_string()],
        "case {case:?}: the describe is filtered to {subnet_id}, so it must report that \
         subnet and no other, got {xml}"
    );
    (
        opt_element(&xml, "availabilityZone").unwrap_or_default(),
        opt_element(&xml, "availabilityZoneId").unwrap_or_default(),
    )
}

/// The `Subnet$Type` Amazon EC2 reports for one subnet, read through the same filtered
/// describe.
async fn subnet_type(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    subnet_id: &str,
) -> String {
    let xml = ec2(
        router,
        case,
        region,
        "DescribeSubnets",
        &[("SubnetId.1", subnet_id)],
    )
    .await;
    opt_element(&xml, "type").unwrap_or_default()
}

/// The `(availabilityZone, availabilityZoneId)` `DescribeInstances` reports in one
/// instance's `Placement` — WHERE the launch put it — read through a describe filtered to
/// that instance.
async fn instance_zone(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    instance_id: &str,
) -> (String, String) {
    let xml = ec2(
        router,
        case,
        region,
        "DescribeInstances",
        &[("InstanceId.1", instance_id)],
    )
    .await;
    assert_eq!(
        elements(&xml, "instanceId"),
        vec![instance_id.to_string()],
        "case {case:?}: the describe is filtered to {instance_id}, so it must report that \
         instance and no other, got {xml}"
    );
    let placement = block_of(&xml, "placement")
        .unwrap_or_else(|| panic!("case {case:?}: no <placement> for {instance_id} in {xml}"));
    (
        opt_element(placement, "availabilityZone").unwrap_or_default(),
        opt_element(placement, "availabilityZoneId").unwrap_or_default(),
    )
}

/// The zone name Amazon EC2 publishes FIRST for `region`, which is the zone it picks for a
/// placement that names none.
async fn first_zone_of_region(router: &MultiServiceRouter, case: &str, region: &str) -> String {
    let xml = ec2(router, case, region, "DescribeAvailabilityZones", &[]).await;
    let names = elements(&xml, "zoneName");
    names
        .first()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: {region} publishes no zone at all, got {xml}"))
}

/// What one `CreateEnvironment` call below is: where it is served, what the environment is
/// called, the network it is deployed into and the hosts it carries.
struct EnvironmentPlan<'a> {
    /// The case the create belongs to, named in every assertion message.
    case: &'a str,
    /// The Region the request is addressed to.
    region: &'a str,
    /// The `environmentName` the request carries.
    name: &'a str,
    /// The VPC the environment is deployed into.
    vpc_id: &'a str,
    /// The subnet "used to establish connectivity between the Amazon EVS control plane and
    /// VPC" (`serviceAccessSubnetId` doc), whose zone is the environment's zone.
    subnet_id: &'a str,
    /// The first two octets of that VPC's range, which the VLAN blocks are cut from.
    base: &'a str,
    /// The `hosts` the create carries, when it carries any.
    hosts: Option<Value>,
}

/// The SSH key pair the account holds, minted through Amazon EC2's own `CreateKeyPair`.
///
/// A host's `keyName` is "the name of the SSH key that is used to access the host" — an
/// Amazon EC2 key pair of the caller's own account, which Amazon EVS does not create — so
/// every environment and host below names a pair that really is in that account's key pair
/// store. The store holds one key pair per name for the whole account, so minting it in any
/// Region serves every Region; idempotent, a second call drawing Amazon EC2's own duplicate
/// refusal and nothing else.
async fn ensure_key_pair(router: &MultiServiceRouter, case: &str, region: &str) {
    let (status, body) = ec2_call(
        router,
        region,
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

/// Create one environment from `plan` and answer its `environmentId`.
async fn create_environment(router: &MultiServiceRouter, plan: EnvironmentPlan<'_>) -> String {
    let EnvironmentPlan {
        case,
        region,
        name,
        vpc_id,
        subnet_id,
        base,
        hosts,
    } = plan;
    // Every host this environment takes, now or later, names the account's key pair.
    ensure_key_pair(router, case, region).await;
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
    let created = evs(router, case, region, "CreateEnvironment", &request).await;
    created["environment"]["environmentId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no environmentId in {created}"))
        .to_string()
}

/// Look at `environment_id` until it reports the state its resources are listable in,
/// failing only once a generous deadline has passed — the CONDITION is what is waited for,
/// never a span of time.
async fn await_created(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    environment_id: &str,
) {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let got = evs(
            router,
            case,
            region,
            "GetEnvironment",
            &json!({ "environmentId": environment_id }),
        )
        .await;
        let state = got["environment"]["environmentState"]
            .as_str()
            .unwrap_or("<none>")
            .to_string();
        if state == ENVIRONMENT_CREATED {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the environment {environment_id} never reached \
             {ENVIRONMENT_CREATED}; last state {state:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The four `hosts` an environment is created with, named apart from every other
/// environment's: "DNS hostnames for hosts must be unique across Amazon EVS environments"
/// (`HostInfoForCreate$hostName` doc).
fn hosts_at_create(prefix: &str) -> Value {
    Value::Array(
        (0..HOSTS_AT_CREATE)
            .map(|index| {
                json!({
                    "hostName": format!("{prefix}-{index}"),
                    "keyName": KEY_NAME,
                    "instanceType": INSTANCE_TYPE,
                })
            })
            .collect(),
    )
}

/// Add one host to `environment_id` and answer the EC2 instance id that host runs on.
async fn create_host(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    environment_id: &str,
    host_name: &str,
) -> String {
    let created = evs(
        router,
        case,
        region,
        "CreateEnvironmentHost",
        &json!({
            "environmentId": environment_id,
            "host": {
                "hostName": host_name,
                "keyName": KEY_NAME,
                "instanceType": INSTANCE_TYPE,
            },
        }),
    )
    .await;
    created["host"]["ec2InstanceId"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: a host must report the EC2 instance it runs on, got {created}")
        })
        .to_string()
}

/// One row: the network an environment is deployed into, and so the one zone every
/// resource of that environment must be placed in.
struct Row {
    /// What the row is, named in every assertion message.
    case: &'static str,
    /// The Region the row's network, environment and hosts live in.
    region: &'static str,
    /// The zone the service access subnet is created in, or `None` to leave the zone to
    /// `CreateSubnet`.
    subnet_zone: Option<&'static str>,
    /// The first two octets of the row's own VPC range.
    base: &'static str,
    /// The environment's name, and the prefix of its hosts' DNS hostnames.
    name: &'static str,
}

/// The rows of this suite: the Region, and which of that Region's published zones the
/// service access subnet is in — the Region's zone letters are the ones AWS publishes for
/// it (`service_common::aws_availability_zones`), four for `us-west-2` and three for
/// `eu-west-1`.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a subnet in the Region's FIRST zone",
            region: "us-west-2",
            subnet_zone: Some("us-west-2a"),
            base: "10.20",
            name: "zone-first",
        },
        Row {
            case: "a subnet in the Region's LAST published zone",
            region: "us-west-2",
            subnet_zone: Some("us-west-2d"),
            base: "10.21",
            name: "zone-last",
        },
        Row {
            case: "a subnet in a middle zone of another Region",
            region: "eu-west-1",
            subnet_zone: Some("eu-west-1b"),
            base: "10.22",
            name: "zone-other-region",
        },
        Row {
            case: "a subnet whose zone the caller never named",
            region: "us-west-2",
            subnet_zone: None,
            base: "10.23",
            name: "zone-unnamed",
        },
    ]
}

/// RULE: every resource an Amazon EVS operation places for one environment — each VLAN
/// subnet `CreateEnvironment` provisions, the instance of each host it is created with, and
/// the instance of a host added afterwards — is placed in the zone of that environment's
/// service access subnet, in every Region and whichever of its zones that subnet is in.
#[tokio::test]
async fn every_resource_of_an_environment_is_placed_in_the_service_access_subnets_zone() {
    let router = emulator();
    let rows = rows();

    // Every environment is created FIRST, so the moves the clock owes them run alongside
    // one another rather than one row at a time.
    let mut created = Vec::with_capacity(rows.len());
    for row in &rows {
        let case = row.case;
        let (vpc_id, subnet_id) =
            create_network(&router, case, row.region, row.base, row.subnet_zone).await;
        // The environment's zone is the SUBNET's zone, so what EC2 reports for that subnet
        // is what every assertion below compares against — read from EC2 rather than
        // assumed, and checked against the zone the row asked for where it named one.
        let (zone_name, zone_id) = subnet_zone(&router, case, row.region, &subnet_id).await;
        assert!(
            !zone_name.is_empty() && !zone_id.is_empty(),
            "case {case:?}: the service access subnet {subnet_id} must report the zone it \
             is in, got {zone_name:?} / {zone_id:?}"
        );
        if let Some(asked) = row.subnet_zone {
            assert_eq!(
                zone_name, asked,
                "case {case:?}: the subnet must be in the zone the request named, got \
                 {zone_name:?}"
            );
        }
        let environment_id = create_environment(
            &router,
            EnvironmentPlan {
                case,
                region: row.region,
                name: row.name,
                vpc_id: &vpc_id,
                subnet_id: &subnet_id,
                base: row.base,
                hosts: Some(hosts_at_create(row.name)),
            },
        )
        .await;
        created.push((row, environment_id, zone_name, zone_id));
    }

    for (row, environment_id, zone_name, zone_id) in &created {
        let case = row.case;
        await_created(&router, case, row.region, environment_id).await;

        // The VLAN subnets `CreateEnvironment` provisioned: Amazon EVS reports the zone,
        // and so does the subnet EC2 holds for it.
        let listed = evs(
            &router,
            case,
            row.region,
            "ListEnvironmentVlans",
            &json!({ "environmentId": environment_id }),
        )
        .await;
        let vlans = listed["environmentVlans"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: no environmentVlans in {listed}"))
            .clone();
        assert_eq!(
            vlans.len(),
            VLAN_MEMBERS.len(),
            "case {case:?}: expected one VLAN per InitialVlans member, got {vlans:?}"
        );
        for vlan in &vlans {
            let function = vlan["functionName"].as_str().unwrap_or("<none>");
            assert_eq!(
                vlan["availabilityZone"],
                json!(zone_name),
                "case {case:?}: {function}: expected the environment's zone \
                 {zone_name:?}, got {got}",
                got = vlan["availabilityZone"]
            );
            let subnet_id = vlan["subnetId"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: {function}: no subnetId in {vlan}"));
            let got = subnet_zone(&router, case, row.region, subnet_id).await;
            assert_eq!(
                got,
                (zone_name.clone(), zone_id.clone()),
                "case {case:?}: {function}: expected the VLAN subnet {subnet_id} to be in \
                 the environment's zone ({zone_name:?} / {zone_id:?}), got {got:?}"
            );
            let kind = subnet_type(&router, case, row.region, subnet_id).await;
            assert_eq!(
                kind, EVS_SUBNET_TYPE,
                "case {case:?}: {function}: the subnet {subnet_id} must be reported as an \
                 Amazon EVS subnet, got {kind:?}"
            );
        }

        // The instance each host the environment was CREATED WITH runs on: placed in that
        // same one zone.
        let listed = evs(
            &router,
            case,
            row.region,
            "ListEnvironmentHosts",
            &json!({ "environmentId": environment_id }),
        )
        .await;
        let hosts = listed["environmentHosts"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: no environmentHosts in {listed}"))
            .clone();
        assert_eq!(
            hosts.len(),
            HOSTS_AT_CREATE,
            "case {case:?}: the environment must hold the hosts it was created with, got \
             {hosts:?}"
        );
        for host in &hosts {
            let host_name = host["hostName"].as_str().unwrap_or("<none>");
            let instance_id = host["ec2InstanceId"].as_str().unwrap_or_else(|| {
                panic!("case {case:?}: {host_name}: no ec2InstanceId in {host}")
            });
            let got = instance_zone(&router, case, row.region, instance_id).await;
            assert_eq!(
                got,
                (zone_name.clone(), zone_id.clone()),
                "case {case:?}: {host_name}: expected the instance {instance_id} to be \
                 placed in the environment's zone ({zone_name:?} / {zone_id:?}), got {got:?}"
            );
        }

        // And the instance of a host added by the OTHER operation that adds one, to the
        // same environment: the same zone again.
        let added = format!("{}-added", row.name);
        let instance_id = create_host(&router, case, row.region, environment_id, &added).await;
        let got = instance_zone(&router, case, row.region, &instance_id).await;
        assert_eq!(
            got,
            (zone_name.clone(), zone_id.clone()),
            "case {case:?}: {added}: expected the instance {instance_id} of a host added \
             after the create to be placed in the environment's zone ({zone_name:?} / \
             {zone_id:?}), got {got:?}"
        );
    }
}

/// RULE, at the fallback half of the same rule: an environment whose service access subnet
/// the account's store no longer holds takes the Region's FIRST zone — the zone EC2 itself
/// picks for a placement that names none — and the host operation takes it from the same
/// place the VLAN subnets do, so the two cannot fall back differently.
#[tokio::test]
async fn a_host_of_an_environment_whose_service_access_subnet_is_gone_takes_the_regions_first_zone()
{
    let router = emulator();
    let case = "the service access subnet deleted after the create";
    let region = "us-west-2";
    // Deliberately NOT the Region's first zone, so the fallback is distinguishable from
    // the zone the subnet carried.
    let subnet_zone_name = "us-west-2c";
    let base = "10.24";
    let (vpc_id, subnet_id) =
        create_network(&router, case, region, base, Some(subnet_zone_name)).await;
    let environment_id = create_environment(
        &router,
        EnvironmentPlan {
            case,
            region,
            name: "zone-fallback",
            vpc_id: &vpc_id,
            subnet_id: &subnet_id,
            base,
            hosts: None,
        },
    )
    .await;
    await_created(&router, case, region, &environment_id).await;

    let first_zone = first_zone_of_region(&router, case, region).await;
    assert_ne!(
        first_zone, subnet_zone_name,
        "case {case:?}: the row only says something where the subnet's zone is not the \
         Region's first one"
    );

    // The caller's own subnet, removed by the caller: the environment keeps naming an id
    // the store no longer holds.
    let deleted = ec2(
        &router,
        case,
        region,
        "DeleteSubnet",
        &[("SubnetId", subnet_id.as_str())],
    )
    .await;
    assert!(
        deleted.contains("<return>true</return>"),
        "case {case:?}: DeleteSubnet must remove the service access subnet {subnet_id}, \
         got {deleted}"
    );

    let instance_id = create_host(&router, case, region, &environment_id, "zone-fallback-0").await;
    let (zone_name, _zone_id) = instance_zone(&router, case, region, &instance_id).await;
    assert_eq!(
        zone_name, first_zone,
        "case {case:?}: expected the instance {instance_id} to be placed in the Region's \
         first zone ({first_zone}), got {zone_name:?}"
    );
}
