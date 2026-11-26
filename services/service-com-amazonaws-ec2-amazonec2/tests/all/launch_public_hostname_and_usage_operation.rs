// SPDX-License-Identifier: CC0-1.0

//! When an instance's PUBLIC HOSTNAME comes to exist, and the instant its usage operation was
//! last updated — end to end over the ec2Query wire, through the crate's shared
//! [`super::ec2_query`] harness.
//!
//! Two rules, driven over one table of launches:
//!
//! 1. THE PUBLIC DNS HOSTNAME OF AN INSTANCE IS ASSIGNED BY THE LAUNCH THAT CREATES IT, so a
//!    launch that is still running has assigned none: while the instance reports `pending`,
//!    its own `dnsName` is the EMPTY name and every nested member that would name a hostname is
//!    absent — the `publicDnsName` of the interface the instance reports, the `publicDnsName` of
//!    the association on that interface and on the private address the public one is bound to,
//!    and the `publicIpDnsNameOptions` naming it on either — while the public ADDRESS the launch
//!    gave the instance is reported beside them. From the instant the launch has carried the
//!    instance to `running`, every one of those members names THAT address:
//!    `ec2-<dash-separated address>` under the Region's public DNS domain (Amazon EC2 User
//!    Guide, "Amazon EC2 instance hostname types").
//! 2. `usageOperationUpdateTime` — "the time that the usage operation was last updated"
//!    (`Instance$UsageOperationUpdateTime`) — is reported for EVERY instance, as the instant
//!    that billing attribution was settled, which is when the instance was launched: it is the
//!    instance's own `launchTime`, whichever AMI the launch named.
//!
//! The rows span what the rules generalise over: whether the launch is given a public address
//! at all, HOW it is given one (the subnet's own `mapPublicIpOnLaunch` attribute, or the
//! request's `AssociatePublicIpAddress`), the VPC the instance runs in (the account's default
//! one and a VPC a caller created and enabled public hostnames in), and whether the AMI the
//! launch names is one this emulator's catalog publishes or an id it holds no image for — the
//! two inputs rule 2 generalises over.

use std::time::{Duration, Instant};

use ec2::state::ComAmazonawsEc2Amazonec2State as Ec2State;
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element};
use super::wire::{Param, child, child_text, document, param};

/// The Region every call here is signed for: the account's default VPC and its per-zone
/// subnets are seeded for the emulator's default Region, and two of the rows below launch into
/// them.
const REGION: &str = "us-east-1";

/// The public DNS domain of [`REGION`]: AWS uses `compute-1.amazonaws.com` in `us-east-1` and
/// `<region>.compute.amazonaws.com` in every other Region (Amazon EC2 User Guide, "Amazon EC2
/// instance hostname types"). Written out here rather than read from the crate, so the test
/// pins the name a client resolves rather than echoing the emulator's own derivation.
const PUBLIC_DNS_DOMAIN: &str = "compute-1.amazonaws.com";

/// A well-formed AMI id this emulator's catalog holds no image for: the launch is served all
/// the same, and rule 2 holds for the instance it mints.
const UNPUBLISHED_AMI: &str = "ami-0123456789abcdef0";

/// How long a poll here keeps looking: a large multiple of the service's own transition span,
/// so a loaded or slow host is never the reason an assertion fails. Nothing is waited out —
/// the CONDITION is looked for again until it holds.
fn poll_deadline() -> Duration {
    ec2::state::TRANSITION_DELAY * 40
}

/// One look inside that deadline, so the deadline costs many looks rather than one.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// One call that must answer HTTP 200, as its body.
async fn ok(router: &MultiServiceRouter, case: &str, action: &str, params: &[Param]) -> String {
    let (status, body) = call(router, REGION, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// The public hostname AWS gives the public IPv4 address `public_ip` in [`REGION`].
fn expected_hostname(public_ip: &str) -> String {
    format!(
        "ec2-{host}.{PUBLIC_DNS_DOMAIN}",
        host = public_ip.replace('.', "-")
    )
}

/// Every member of one reported instance the two rules read.
///
/// `None` means the element was ABSENT; `Some("")` means present and empty — the two cases
/// rule 1 tells apart for the instance's own `dnsName`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Reported {
    state: Option<String>,
    public_ip: Option<String>,
    dns_name: Option<String>,
    options: Option<String>,
    interface_dns_name: Option<String>,
    interface_options: Option<String>,
    association_public_ip: Option<String>,
    association_dns_name: Option<String>,
    address_association_dns_name: Option<String>,
    launch_time: Option<String>,
    usage_operation_update_time: Option<String>,
}

/// The value at `path` inside `node`, each step a child element and the last one's text.
fn at(node: roxmltree::Node<'_, '_>, path: &[&str]) -> Option<String> {
    let (last, parents) = path.split_last()?;
    let mut current = node;
    for name in parents {
        current = child(current, name)?;
    }
    child_text(current, last)
}

/// What the one instance of a `RunInstances` or `DescribeInstances` body reports — read off the
/// element carrying both an `<instanceId>` and an `<instanceState>`, so a member is never read
/// out of a shape that merely shares its name.
fn reported(case: &str, body: &str) -> Reported {
    let parsed = document(case, body);
    let instance = parsed
        .descendants()
        .find(|node| {
            node.is_element()
                && child(*node, "instanceId").is_some()
                && child(*node, "instanceState").is_some()
        })
        .unwrap_or_else(|| panic!("case {case:?}: no reported instance in {body}"));
    let interface = &["networkInterfaceSet", "item"];
    let nested = |path: &[&str]| -> Option<String> {
        let mut full: Vec<&str> = interface.to_vec();
        full.extend_from_slice(path);
        at(instance, &full)
    };
    Reported {
        state: at(instance, &["instanceState", "name"]),
        public_ip: child_text(instance, "ipAddress"),
        dns_name: child_text(instance, "dnsName"),
        options: at(instance, &["publicIpDnsNameOptions", "publicIpv4DnsName"]),
        interface_dns_name: nested(&["publicDnsName"]),
        interface_options: nested(&["publicIpDnsNameOptions", "publicIpv4DnsName"]),
        association_public_ip: nested(&["association", "publicIp"]),
        association_dns_name: nested(&["association", "publicDnsName"]),
        address_association_dns_name: nested(&[
            "privateIpAddressesSet",
            "item",
            "association",
            "publicDnsName",
        ]),
        launch_time: child_text(instance, "launchTime"),
        usage_operation_update_time: child_text(instance, "usageOperationUpdateTime"),
    }
}

/// `DescribeInstances` for one instance, as its body.
async fn describe(router: &MultiServiceRouter, case: &str, instance_id: &str) -> String {
    ok(
        router,
        case,
        "DescribeInstances",
        &[param("InstanceId.1", instance_id)],
    )
    .await
}

/// Look again until `instance_id` reports `running`, or [`poll_deadline`] passes: the launch
/// owes that move to the clock, so what is waited on is the STATE and never a span of time.
async fn poll_until_running(router: &MultiServiceRouter, case: &str, instance_id: &str) -> String {
    let deadline = Instant::now() + poll_deadline();
    loop {
        let body = describe(router, case, instance_id).await;
        let state = reported(case, &body).state.unwrap_or_default();
        if state == "running" {
            return body;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {instance_id} never reached running, last state {state:?} in {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One launch of this table, and what it must report.
struct Row {
    /// What the assertions name this launch.
    case: &'static str,
    /// The extra request members the launch carries beside its image and its counts.
    params: Vec<Param>,
    /// Whether the launch is given a public IPv4 address at all.
    public_address: bool,
}

/// A `SubnetId` of the account's default VPC — a subnet whose own `mapPublicIpOnLaunch`
/// attribute gives the instances launched in it a public IPv4 address.
fn a_default_subnet(engine: &engine::InMemoryEngine) -> String {
    engine
        .state::<Ec2State>()
        .lock()
        .subnets
        .keys()
        .next()
        .cloned()
        .expect("a fresh account has the default VPC's subnets")
}

/// An `ImageId` this emulator's catalog PUBLISHES, so the table drives rule 2 over an AMI the
/// store holds a record for as well as over one it holds none for.
async fn a_published_image(router: &MultiServiceRouter, case: &str) -> String {
    let body = ok(
        router,
        case,
        "DescribeImages",
        &[param("Owner.1", "amazon")],
    )
    .await;
    element(&body, "imageId")
        .unwrap_or_else(|| panic!("case {case:?}: DescribeImages published no image: {body}"))
}

/// A VPC of the caller's own, with one subnet in it and public hostnames enabled — the second
/// place a launch of the table below runs in.
async fn a_caller_network(router: &MultiServiceRouter, case: &str) -> String {
    let body = ok(router, case, "CreateVpc", &[param("CidrBlock", "10.90.0.0/16")]).await;
    let vpc_id = element(&body, "vpcId")
        .unwrap_or_else(|| panic!("case {case:?}: CreateVpc reported no VPC: {body}"));
    ok(
        router,
        case,
        "ModifyVpcAttribute",
        &[
            param("VpcId", &vpc_id),
            param("EnableDnsHostnames.Value", "true"),
        ],
    )
    .await;
    let body = ok(
        router,
        case,
        "CreateSubnet",
        &[param("VpcId", &vpc_id), param("CidrBlock", "10.90.1.0/24")],
    )
    .await;
    element(&body, "subnetId")
        .unwrap_or_else(|| panic!("case {case:?}: CreateSubnet reported no subnet: {body}"))
}

/// RULE 1 and RULE 2 over every row of the table: the launch reply reports the address with no
/// hostname for it yet, the instance reports the hostname of that address once the launch has
/// finished, and both reports carry the instant the usage operation was settled.
#[tokio::test]
async fn a_launch_assigns_the_public_hostname_as_it_completes_and_dates_the_usage_operation() {
    let (router, engine) = super::ec2_query::emulator();
    let default_subnet = a_default_subnet(&engine);
    let published_image = a_published_image(&router, "a published image").await;
    let caller_subnet = a_caller_network(&router, "a caller's own network").await;

    let rows = [
        Row {
            case: "the default network's subnet assigns the address",
            params: vec![param("SubnetId", &default_subnet)],
            public_address: true,
        },
        Row {
            case: "a launch from a published AMI in that same subnet",
            params: vec![param("SubnetId", &default_subnet)],
            public_address: true,
        },
        Row {
            case: "the request asks its primary interface for the address",
            params: vec![
                param("NetworkInterface.1.DeviceIndex", "0"),
                param("NetworkInterface.1.SubnetId", &caller_subnet),
                param("NetworkInterface.1.AssociatePublicIpAddress", "true"),
            ],
            public_address: true,
        },
        Row {
            case: "a launch given no public address at all",
            params: vec![
                param("NetworkInterface.1.DeviceIndex", "0"),
                param("NetworkInterface.1.SubnetId", &caller_subnet),
            ],
            public_address: false,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let image_id = if index == 1 {
            published_image.as_str()
        } else {
            UNPUBLISHED_AMI
        };
        let mut params = vec![
            param("ImageId", image_id),
            param("MinCount", "1"),
            param("MaxCount", "1"),
        ];
        params.extend(row.params.iter().cloned());
        let launched = ok(&router, case, "RunInstances", &params).await;
        let instance_id = element(&launched, "instanceId")
            .unwrap_or_else(|| panic!("case {case:?}: RunInstances reported no instance: {launched}"));

        // RULE 1, while the launch is still running: the address is reported and no hostname
        // for it is.
        let at_launch = reported(case, &launched);
        assert_eq!(
            at_launch.state.as_deref(),
            Some("pending"),
            "case {case:?}: a launch answers with the instance it is still creating, got \
             {at_launch:?}"
        );
        assert_eq!(
            at_launch.public_ip.is_some(),
            row.public_address,
            "case {case:?}: expected the launch to report a public address: {}, got \
             {at_launch:?}",
            row.public_address
        );
        assert_eq!(
            at_launch.association_public_ip, at_launch.public_ip,
            "case {case:?}: the interface's association reports the same address as the \
             instance, got {at_launch:?}"
        );
        assert_eq!(
            at_launch.dns_name.as_deref(),
            Some(""),
            "case {case:?}: expected the empty dnsName from a launch that has assigned no \
             hostname yet, got {at_launch:?}"
        );
        for (member, value) in [
            ("publicIpDnsNameOptions.publicIpv4DnsName", &at_launch.options),
            (
                "networkInterfaceSet.item.publicDnsName",
                &at_launch.interface_dns_name,
            ),
            (
                "networkInterfaceSet.item.publicIpDnsNameOptions.publicIpv4DnsName",
                &at_launch.interface_options,
            ),
            (
                "networkInterfaceSet.item.association.publicDnsName",
                &at_launch.association_dns_name,
            ),
            (
                "networkInterfaceSet.item.privateIpAddressesSet.item.association.publicDnsName",
                &at_launch.address_association_dns_name,
            ),
        ] {
            assert_eq!(
                value.as_deref(),
                None,
                "case {case:?}: expected {member} to be absent from a launch that has assigned \
                 no hostname yet, got {value:?} in {at_launch:?}"
            );
        }
        // RULE 2 holds of the launch reply too: the usage operation was settled when the
        // instance was launched.
        assert!(
            at_launch.usage_operation_update_time.is_some()
                && at_launch.usage_operation_update_time == at_launch.launch_time,
            "case {case:?}: expected usageOperationUpdateTime to be the instance's launchTime, \
             got {at_launch:?}"
        );

        // RULE 1, from the instant the launch has finished: every one of those members names
        // the address the instance holds.
        let running = poll_until_running(&router, case, &instance_id).await;
        let settled = reported(case, &running);
        assert_eq!(
            settled.public_ip, at_launch.public_ip,
            "case {case:?}: finishing the launch does not change which address the instance \
             holds, got {settled:?}"
        );
        let hostname = settled.public_ip.as_deref().map(expected_hostname);
        assert_eq!(
            settled.dns_name,
            Some(hostname.clone().unwrap_or_default()),
            "case {case:?}: expected dnsName {:?} for the address {:?}, got {settled:?}",
            hostname.clone().unwrap_or_default(),
            settled.public_ip
        );
        for (member, value) in [
            (
                "publicIpDnsNameOptions.publicIpv4DnsName",
                &settled.options,
            ),
            (
                "networkInterfaceSet.item.publicIpDnsNameOptions.publicIpv4DnsName",
                &settled.interface_options,
            ),
            (
                "networkInterfaceSet.item.association.publicDnsName",
                &settled.association_dns_name,
            ),
            (
                "networkInterfaceSet.item.privateIpAddressesSet.item.association.publicDnsName",
                &settled.address_association_dns_name,
            ),
        ] {
            assert_eq!(
                value, &hostname,
                "case {case:?}: expected {member} to name the instance's own address as \
                 {hostname:?}, got {value:?} in {settled:?}"
            );
        }
        // The interface's own hostname member is reported for every interface the instance
        // holds, EMPTY for one holding no public address, so it is read against the name
        // rather than against its absence.
        assert_eq!(
            settled.interface_dns_name,
            Some(hostname.clone().unwrap_or_default()),
            "case {case:?}: expected networkInterfaceSet.item.publicDnsName {:?}, got \
             {settled:?}",
            hostname.unwrap_or_default()
        );
        assert_eq!(
            settled.usage_operation_update_time, settled.launch_time,
            "case {case:?}: expected usageOperationUpdateTime to stay the instance's \
             launchTime, got {settled:?}"
        );
    }
}
