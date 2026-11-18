// SPDX-License-Identifier: CC0-1.0

//! The network presence a Directory Service directory really has in Amazon EC2: the domain
//! controller security group its servers sit behind, the network interface each of them
//! answers on, and the release of both when the directory is deleted.
//!
//! RULE: a create that places a directory's servers in the caller's subnets provisions that
//! placement in the service that OWNS those resources — ONE security group for the
//! directory's controllers, in the VPC EC2 reports for the resolved subnets and carrying the
//! directory's own identifier in its name, reported back as the placement's `SecurityGroupId`
//! (`DirectoryVpcSettingsDescription$SecurityGroupId`, "The domain controller security group
//! identifier for the directory"; `DirectoryConnectSettingsDescription$SecurityGroupId` is
//! the same member for an AD Connector), plus ONE network interface per resolved subnet, in
//! that subnet, naming the directory in its description, carrying that group and holding an
//! address out of that subnet's own block ("Directory Service creates a directory server and
//! a DNS server in each of these subnets", `DirectoryVpcSettings$SubnetIds`) — and
//! `DeleteDirectory` RELEASES both there, after which EC2 answers for each released
//! identifier the way it answers for one it holds nothing under:
//! `InvalidNetworkInterfaceID.NotFound` for the interface and `InvalidGroup.NotFound` for the
//! group, which is what lets the caller take its own subnets and VPC away again.
//!
//! Why this suite lives beside the router rather than in one service's crate: both resources
//! are Amazon EC2's, so the rule is only observable with EC2 mounted beside Directory Service
//! on ONE engine, the way the router mounts them. Every value asserted below is one EC2 itself
//! answered — the VPC, the subnets, the group and the interfaces — so a group this emulator
//! only recorded would be in no listing of the owner's and could not pass, and one it only
//! pretended to release would still be there.
//!
//! The rows differ along what the rule generalises over: which create places the servers, and
//! so which member the placement is reported under (`CreateMicrosoftAD`, `CreateDirectory`,
//! `ConnectDirectory`), the edition a Managed Microsoft AD runs, how many subnets the create
//! names (two and three), which of the row's zone-spread subnets those are, and the order the
//! request names them in. Each row builds its OWN VPC, so one row's network can never be read
//! as another's.
//!
//! Model sources: `smithy_query(query="shape", shape="DirectoryVpcSettingsDescription,\
//! DirectoryConnectSettingsDescription,SecurityGroupId")` for the reported member and its
//! pattern, and `smithy_query(query="shape", shape="NetworkInterface,SecurityGroup")` for the
//! EC2 members read back below.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{block_of, element, elements, form_encode, opt_element};

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

/// The three Availability Zones each row spreads its subnets over, one subnet per zone: a
/// directory's servers "must be in different Availability Zones"
/// (`DirectoryVpcSettings$SubnetIds`), so every row's placement spans at least two.
const ZONES: [&str; 3] = ["us-west-2a", "us-west-2b", "us-west-2c"];

/// EC2's own code for an identifier it holds no network interface under, which is what a
/// released interface's identifier must answer with.
const INTERFACE_NOT_FOUND: &str = "InvalidNetworkInterfaceID.NotFound";

/// EC2's own code for an identifier it holds no security group under.
const GROUP_NOT_FOUND: &str = "InvalidGroup.NotFound";

/// How long a polled read may take to report the directory's group before a case fails: far
/// longer than any span either service publishes, and well inside what a client's own waiter
/// allows, so a loaded or faster host changes nothing.
const POLL_DEADLINE: Duration = Duration::from_secs(30);

/// How often such a read asks again, which only bounds how often the store is asked.
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
/// routes the request here; the signature is never checked.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One ec2Query action: its status and the XML body EC2 answered.
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
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One ec2Query action that must be served, answered with its XML body.
async fn ec2_ok(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let (status, text) = ec2(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: EC2 {action} must be served, got {status} with {text}"
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

/// One row of the table: one create, in one network of its own.
struct Row {
    case: &'static str,
    /// The operation that places the directory's servers in the caller's subnets.
    operation: &'static str,
    /// The member the placement of that directory is reported under.
    placement: &'static str,
    /// The second octet of the row's OWN VPC block, so one row's network is never another's.
    octet: u8,
    /// Which of the row's three zone-spread subnets the create names, in the order it names
    /// them.
    subnets: &'static [usize],
    /// The Managed Microsoft AD edition the create asks for, where the operation takes one.
    edition: Option<&'static str>,
}

/// The rows: every create that places servers in a VPC, over placements that differ in how
/// many subnets they name, which ones and in which order.
const ROWS: &[Row] = &[
    Row {
        case: "a Standard Managed Microsoft AD in two subnets in two zones",
        operation: "CreateMicrosoftAD",
        placement: "VpcSettings",
        octet: 81,
        subnets: &[0, 1],
        edition: Some("Standard"),
    },
    Row {
        case: "an Enterprise Managed Microsoft AD in three subnets, named in reverse order",
        operation: "CreateMicrosoftAD",
        placement: "VpcSettings",
        octet: 82,
        subnets: &[2, 1, 0],
        edition: Some("Enterprise"),
    },
    Row {
        case: "a Simple AD in the second and third subnets",
        operation: "CreateDirectory",
        placement: "VpcSettings",
        octet: 83,
        subnets: &[1, 2],
        edition: None,
    },
    Row {
        case: "an AD Connector, whose placement is reported under ConnectSettings",
        operation: "ConnectDirectory",
        placement: "ConnectSettings",
        octet: 84,
        subnets: &[0, 2],
        edition: None,
    },
];

/// The create body of a row, placing its servers in `vpc_id`'s `subnets`.
fn create_body(row: &Row, name: &str, vpc_id: &str, subnets: &[String]) -> Value {
    let settings = json!({ "VpcId": vpc_id, "SubnetIds": subnets });
    match row.operation {
        "CreateDirectory" => json!({
            "Name": name,
            "Password": PASSWORD,
            "Size": "Small",
            "VpcSettings": settings,
        }),
        "CreateMicrosoftAD" => {
            let mut body = json!({
                "Name": name,
                "Password": PASSWORD,
                "VpcSettings": settings,
            });
            if let Some(edition) = row.edition {
                body["Edition"] = json!(edition);
            }
            body
        }
        "ConnectDirectory" => json!({
            "Name": name,
            "Password": PASSWORD,
            "Size": "Small",
            "ConnectSettings": {
                "VpcId": vpc_id,
                "SubnetIds": subnets,
                "CustomerDnsIps": CUSTOMER_DNS_IPS,
                "CustomerUserName": "Admin",
            },
        }),
        other => panic!("setup: {other} places no directory servers in a VPC"),
    }
}

/// The group the directory `directory_id` reports for its servers, polled through
/// `DescribeDirectories` until it names one.
///
/// Polled rather than read once: the group appears while the directory is still being built,
/// so a client — and this suite — waits for the CONDITION rather than for a span it would have
/// to know. Nothing is nudged between reads.
async fn poll_placement_group(
    router: &MultiServiceRouter,
    row: &Row,
    directory_id: &str,
) -> String {
    let case = row.case;
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let (status, body) = ds(
            router,
            "DescribeDirectories",
            json!({ "DirectoryIds": [directory_id] }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeDirectories must be served, got {status} with {body}"
        );
        let described = body["DirectoryDescriptions"][0].clone();
        if let Some(group_id) = described[row.placement]["SecurityGroupId"].as_str() {
            return group_id.to_string();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {directory_id} to report a {} SecurityGroupId, last read \
             {described}",
            row.placement
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Assert `group_id` is a value the modelled `SecurityGroupId` admits: `sg-` followed by 8 or
/// 17 lower-case hex digits (`^(sg-[0-9a-f]{8}|sg-[0-9a-f]{17})$`).
fn assert_group_id_shape(case: &str, group_id: &str) {
    let digits = group_id.strip_prefix("sg-").unwrap_or_else(|| {
        panic!("case {case:?}: expected a SecurityGroupId starting with sg-, got {group_id:?}")
    });
    assert!(
        (digits.len() == 8 || digits.len() == 17)
            && digits
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "case {case:?}: expected sg- and 8 or 17 lower-case hex digits, got {group_id:?}"
    );
}

/// The `Message` of an ec2Query refusal, with the entities the XML serializer escaped decoded
/// back — so the sentence compares as EC2 worded it rather than as its escaping.
fn refusal_message(text: &str) -> Option<String> {
    elements(text, "Message").into_iter().next()
}

/// Every `groupId` of the one `groupSet` of an ec2Query answer: the security groups the
/// interface that answer describes carries.
fn group_ids(body: &str) -> Vec<String> {
    block_of(body, "groupSet")
        .map(|block| elements(block, "groupId"))
        .unwrap_or_default()
}

/// Every network interface EC2 holds in `vpc_id`, as `(interface id, the body EC2 answered
/// for that one interface)`.
///
/// Each interface is described on its own rather than read out of the filtered listing: an
/// interface's own body nests lists of its addresses, groups and tags, so a per-interface read
/// is what scopes `subnetId` and `description` to the interface they belong to. The listing is
/// narrowed by the `vpc-id` filter that operation documents, which is how a client finds the
/// interfaces of one network.
async fn interfaces_in_vpc(
    router: &MultiServiceRouter,
    case: &str,
    vpc_id: &str,
) -> Vec<(String, String)> {
    let listed = ec2_ok(
        router,
        case,
        "DescribeNetworkInterfaces",
        &[("Filter.1.Name", "vpc-id"), ("Filter.1.Value.1", vpc_id)],
    )
    .await;
    let mut held: Vec<(String, String)> = Vec::new();
    for interface_id in elements(&listed, "networkInterfaceId") {
        let described = ec2_ok(
            router,
            case,
            "DescribeNetworkInterfaces",
            &[("NetworkInterfaceId.1", interface_id.as_str())],
        )
        .await;
        held.push((interface_id, described));
    }
    held
}

/// The interfaces of `vpc_id` that belong to the directory `directory_id`: the ones whose
/// description names it, which is how a client reconciling a directory against the owner of
/// network interfaces finds them.
async fn directory_interfaces(
    router: &MultiServiceRouter,
    case: &str,
    vpc_id: &str,
    directory_id: &str,
) -> Vec<(String, String)> {
    interfaces_in_vpc(router, case, vpc_id)
        .await
        .into_iter()
        .filter(|(_, body)| {
            opt_element(body, "description")
                .unwrap_or_default()
                .contains(directory_id)
        })
        .collect()
}

/// The leading octets every address of a `/24` shares, `10.81.1.0/24` -> `10.81.1.`.
///
/// Every subnet below is a `/24`, which is what makes a prefix comparison the whole block; a
/// wider block would share fewer octets, so the width is asserted here rather than weakening
/// the comparison silently.
fn block_prefix(cidr_block: &str) -> String {
    assert!(
        cidr_block.ends_with("/24"),
        "this reader covers a /24 block, got {cidr_block:?}"
    );
    let (leading, _) = cidr_block
        .rsplit_once('.')
        .unwrap_or_else(|| panic!("a dotted-quad block, got {cidr_block:?}"));
    format!("{leading}.")
}

/// What one row's directory ended up holding in EC2, so the release half can check that
/// tearing one directory down leaves every other directory's network alone.
#[derive(Debug, Clone)]
struct Held {
    case: &'static str,
    directory_id: String,
    vpc_id: String,
    group_id: String,
    interface_ids: BTreeSet<String>,
}

#[tokio::test]
async fn a_directorys_group_and_subnet_interfaces_are_real_in_ec2_and_released_with_it() {
    let router = emulator();
    let mut held: Vec<Held> = Vec::new();

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;

        // ------------------------------------------------------------------
        // The row's own network, built through EC2 itself: one /24 per zone.
        // ------------------------------------------------------------------
        let vpc_cidr = format!("10.{}.0.0/16", row.octet);
        let vpc_id = element(
            &ec2_ok(
                &router,
                case,
                "CreateVpc",
                &[("CidrBlock", vpc_cidr.as_str())],
            )
            .await,
            "vpcId",
        );
        // (subnet id, the block EC2 created it with)
        let mut subnets: Vec<(String, String)> = Vec::new();
        for (position, zone) in ZONES.iter().enumerate() {
            let block = format!("10.{}.{}.0/24", row.octet, position + 1);
            let created = ec2_ok(
                &router,
                case,
                "CreateSubnet",
                &[
                    ("VpcId", vpc_id.as_str()),
                    ("CidrBlock", block.as_str()),
                    ("AvailabilityZone", zone),
                ],
            )
            .await;
            subnets.push((element(&created, "subnetId"), block));
        }
        let named: Vec<String> = row
            .subnets
            .iter()
            .map(|at| subnets[*at].0.clone())
            .collect();

        // ------------------------------------------------------------------
        // The create, and the group it reports for the directory's servers.
        // ------------------------------------------------------------------
        let name = format!("network{index}.example.com");
        let (status, created) = ds(
            &router,
            row.operation,
            create_body(row, &name, &vpc_id, &named),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: {} must be served, got {status} with {created}",
            row.operation
        );
        let directory_id = created["DirectoryId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a DirectoryId, got {created}"))
            .to_string();

        let group_id = poll_placement_group(&router, row, &directory_id).await;
        assert_group_id_shape(case, &group_id);

        // That group is one the OWNER of security groups really holds: in the VPC the
        // subnets belong to, and named after the directory it was provisioned for.
        let described_group = ec2_ok(
            &router,
            case,
            "DescribeSecurityGroups",
            &[("GroupId.1", group_id.as_str())],
        )
        .await;
        // One `<groupName>` is one group: a group with no rule naming another group carries
        // that element exactly once.
        let group_names = elements(&described_group, "groupName");
        assert_eq!(
            group_names.len(),
            1,
            "case {case:?}: exactly one group must answer for {group_id}, got \
             {group_names:?} in {described_group}"
        );
        assert!(
            group_names[0].contains(&directory_id),
            "case {case:?}: the group's name must carry {directory_id}, got {:?}",
            group_names[0]
        );
        assert_eq!(
            element(&described_group, "groupId"),
            group_id,
            "case {case:?}: expected the group the directory reports, got {described_group}"
        );
        assert_eq!(
            element(&described_group, "vpcId"),
            vpc_id,
            "case {case:?}: the group must sit in the VPC of the subnets the create named, \
             got {described_group}"
        );

        // ------------------------------------------------------------------
        // One interface per named subnet, in that subnet, carrying that group.
        // ------------------------------------------------------------------
        let mine = directory_interfaces(&router, case, &vpc_id, &directory_id).await;
        assert_eq!(
            mine.len(),
            named.len(),
            "case {case:?}: expected one interface of {directory_id} per named subnet \
             ({named:?}), got {mine:?}"
        );
        let mut placed_subnets: BTreeSet<String> = BTreeSet::new();
        let mut interface_ids: BTreeSet<String> = BTreeSet::new();
        for (interface_id, body) in &mine {
            let subnet_id = element(body, "subnetId");
            let block = &subnets
                .iter()
                .find(|(id, _)| *id == subnet_id)
                .unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: {interface_id} sits in {subnet_id}, which this row's \
                         network does not hold: {body}"
                    )
                })
                .1;
            assert!(
                named.contains(&subnet_id),
                "case {case:?}: {interface_id} sits in {subnet_id}, which the create never \
                 named ({named:?}): {body}"
            );
            assert!(
                placed_subnets.insert(subnet_id.clone()),
                "case {case:?}: expected one interface per subnet, got a second in \
                 {subnet_id}: {body}"
            );
            assert!(
                group_ids(body).contains(&group_id),
                "case {case:?}: {interface_id} must carry the directory's group {group_id}, \
                 got {body}"
            );
            // "The IPv4 address of the network interface within the subnet"
            // (`NetworkInterface$PrivateIpAddress`): out of the block of the subnet it was
            // placed in, not merely of the VPC.
            let address = element(body, "privateIpAddress");
            let prefix = block_prefix(block);
            assert!(
                address.starts_with(&prefix),
                "case {case:?}: expected {interface_id} to hold an address out of {block} of \
                 {subnet_id}, got {address:?}"
            );
            assert!(
                interface_ids.insert(interface_id.clone()),
                "case {case:?}: expected an interface of this directory's own, got \
                 {interface_id} again"
            );
        }
        assert_eq!(
            placed_subnets,
            named.iter().cloned().collect::<BTreeSet<String>>(),
            "case {case:?}: expected every named subnet to hold one interface of \
             {directory_id}"
        );

        held.push(Held {
            case,
            directory_id,
            vpc_id,
            group_id,
            interface_ids,
        });
    }

    // The group and the interfaces belong to ONE directory, never to the account: no two
    // rows were given the same ones.
    let groups: BTreeSet<&str> = held.iter().map(|row| row.group_id.as_str()).collect();
    assert_eq!(
        groups.len(),
        held.len(),
        "every directory must be given a group of its own, got {held:?}"
    );
    let every_interface: BTreeSet<&str> = held
        .iter()
        .flat_map(|row| row.interface_ids.iter().map(String::as_str))
        .collect();
    assert_eq!(
        every_interface.len(),
        held.iter().map(|row| row.interface_ids.len()).sum::<usize>(),
        "every server must be given an interface of its own, got {held:?}"
    );

    // ----------------------------------------------------------------------
    // The teardown releases both, in the owner, and only this directory's.
    // ----------------------------------------------------------------------
    for position in 0..held.len() {
        let Held {
            case,
            directory_id,
            vpc_id,
            group_id,
            interface_ids,
        } = held[position].clone();

        let (status, deleted) = ds(
            &router,
            "DeleteDirectory",
            json!({ "DirectoryId": directory_id }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DeleteDirectory must be served, got {status} with {deleted}"
        );
        assert_eq!(
            deleted["DirectoryId"].as_str(),
            Some(directory_id.as_str()),
            "case {case:?}: the delete must answer the directory it tore down, got {deleted}"
        );

        // Nothing of this directory is left on the caller's network...
        let remaining = directory_interfaces(&router, case, &vpc_id, &directory_id).await;
        assert!(
            remaining.is_empty(),
            "case {case:?}: expected EC2 to hold no interface of the deleted {directory_id}, \
             got {remaining:?}"
        );
        // ...and each released identifier is refused the way one the owner holds nothing
        // under is.
        for interface_id in &interface_ids {
            let (status, text) = ec2(
                &router,
                "DescribeNetworkInterfaces",
                &[("NetworkInterfaceId.1", interface_id.as_str())],
            )
            .await;
            assert_eq!(
                (status, opt_element(&text, "Code").as_deref()),
                (400, Some(INTERFACE_NOT_FOUND)),
                "case {case:?}: expected {INTERFACE_NOT_FOUND} for the released \
                 {interface_id}, got {status} with {text}"
            );
            assert_eq!(
                refusal_message(&text).as_deref(),
                Some(format!("The networkInterface ID '{interface_id}' does not exist").as_str()),
                "case {case:?}: expected the not-found message to name {interface_id}, got \
                 {text}"
            );
        }
        // The group those interfaces carried goes with them: they are released first, so the
        // group's own delete is not held by them.
        let (status, text) = ec2(
            &router,
            "DescribeSecurityGroups",
            &[("GroupId.1", group_id.as_str())],
        )
        .await;
        assert_eq!(
            (status, opt_element(&text, "Code").as_deref()),
            (400, Some(GROUP_NOT_FOUND)),
            "case {case:?}: expected {GROUP_NOT_FOUND} for the released {group_id}, got \
             {status} with {text}"
        );
        assert_eq!(
            refusal_message(&text).as_deref(),
            Some(format!("The security group '{group_id}' does not exist").as_str()),
            "case {case:?}: expected the not-found message to name {group_id}, got {text}"
        );

        // Only this directory's network went: every directory not yet deleted still holds
        // its own group and its own interfaces.
        for other in held.iter().skip(position + 1) {
            let still: BTreeSet<String> =
                directory_interfaces(&router, other.case, &other.vpc_id, &other.directory_id)
                    .await
                    .into_iter()
                    .map(|(interface_id, _)| interface_id)
                    .collect();
            assert_eq!(
                still, other.interface_ids,
                "case {case:?}: expected the interfaces of {} to survive the teardown of \
                 {directory_id}",
                other.directory_id
            );
            let described = ec2_ok(
                &router,
                other.case,
                "DescribeSecurityGroups",
                &[("GroupId.1", other.group_id.as_str())],
            )
            .await;
            assert_eq!(
                opt_element(&described, "groupId").as_deref(),
                Some(other.group_id.as_str()),
                "case {case:?}: expected the group of {} to survive the teardown of \
                 {directory_id}, got {described}",
                other.directory_id
            );
        }
    }
}
