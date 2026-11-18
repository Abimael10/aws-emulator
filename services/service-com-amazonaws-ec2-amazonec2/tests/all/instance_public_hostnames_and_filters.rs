// SPDX-License-Identifier: CC0-1.0

//! The public hostname an instance reports for a public IPv4 address, and the filter names
//! `DescribeInstances` resolves — end to end over the ec2Query wire, through the crate's
//! shared [`super::ec2_query`] harness.
//!
//! Three rules, each with a table of its own:
//!
//! 1. A PUBLIC HOSTNAME IS THE VPC'S TO GIVE: "this name is only available if you've enabled
//!    DNS hostnames for your VPC" (`Instance$PublicDnsName`), so an instance holding a public
//!    IPv4 address in a VPC whose `enableDnsHostnames` attribute is off reports the EMPTY
//!    name — reported rather than left out — and reports the Amazon hostname of that address
//!    once the attribute is on, wherever that name appears: the instance's `dnsName`, its
//!    interface's association and the association of the private address the public one is
//!    bound to. The rows move the attribute both ways and cover an instance holding no public
//!    address at all.
//! 2. A PUBLIC ADDRESS IS REPORTED AGAINST THE PRIVATE ADDRESS IT IS BOUND TO: an address AWS
//!    assigns an interface maps onto that interface's primary private address, so the
//!    `privateIpAddressesSet` entry for it carries the same association the interface itself
//!    reports — and an interface holding no public address carries none anywhere.
//! 3. EVERY FILTER NAME THE OPERATION DOCUMENTS IS RESOLVED, against the value the operation
//!    REPORTS for that member: a request carrying one is answered with the instances whose
//!    value matches and with an empty reservation set when none does, and only a name the
//!    operation does not document is refused. The rows span the whole documented catalog, and
//!    a second table selects on the reported value of a scalar, an enum, a boolean, a
//!    timestamp (through a wildcard), a nested option set, a volume of the instance and a
//!    member of its network interface.

use std::time::{Duration, Instant};

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, filters, indexed};
use super::wire::{Param, child, child_text, document, param, set_items};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// The AMI every launch here names: `RunInstances` only needs an image id that resolves, and
/// the instance reports the facts of the image this crate publishes by default for one its
/// catalog does not hold — which is what the filter rows below read.
const AMI: &str = "ami-0123456789abcdef0";

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

/// The value at `path` inside `node`, each step a child element and the last one's text.
fn at(node: roxmltree::Node<'_, '_>, path: &[&str]) -> Option<String> {
    let (last, parents) = path.split_last()?;
    let mut current = node;
    for name in parents {
        current = child(current, name)?;
    }
    child_text(current, last)
}

/// The one instance a `DescribeInstances` body reports, as the element carrying both its id
/// and its state — never a nested element of the same name.
fn described<'a, 'i>(
    case: &str,
    parsed: &'a roxmltree::Document<'i>,
    body: &str,
) -> roxmltree::Node<'a, 'i> {
    parsed
        .descendants()
        .find(|node| {
            node.is_element()
                && child(*node, "instanceId").is_some()
                && child(*node, "instanceState").is_some()
        })
        .unwrap_or_else(|| panic!("case {case:?}: no instance in {body}"))
}

/// `DescribeInstances` for one instance, as its body.
async fn describe(router: &MultiServiceRouter, case: &str, instance_id: &str) -> String {
    ok(
        router,
        case,
        "DescribeInstances",
        &indexed("InstanceId", &[instance_id]),
    )
    .await
}

/// Look again until `instance_id` reports `running`, or a deadline far past the service's own
/// transition span passes: the launch owes that move to the clock, so what is polled for is
/// the state rather than a span of time.
async fn poll_until_running(router: &MultiServiceRouter, case: &str, instance_id: &str) -> String {
    let deadline = Instant::now() + poll_deadline();
    loop {
        let body = describe(router, case, instance_id).await;
        let parsed = document(case, &body);
        let instance = described(case, &parsed, &body);
        let state = child(instance, "instanceState")
            .and_then(|state| child_text(state, "name"))
            .unwrap_or_default();
        if state == "running" {
            drop(parsed);
            return body;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {instance_id} never reached running, last state {state:?} in {body}"
        );
        drop(parsed);
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The three places one public address's hostname is reported: the instance's own `dnsName`,
/// its interface's association and the association of the private address the public one is
/// bound to.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReportedHostnames {
    public_ip: Option<String>,
    instance: Option<String>,
    interface: Option<String>,
    address: Option<String>,
}

/// What `DescribeInstances` reports for `instance_id`'s public address and its hostname.
async fn reported_hostnames(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
) -> ReportedHostnames {
    let body = describe(router, case, instance_id).await;
    let parsed = document(case, &body);
    let instance = described(case, &parsed, &body);
    ReportedHostnames {
        public_ip: child_text(instance, "ipAddress"),
        instance: child_text(instance, "dnsName"),
        interface: at(
            instance,
            &["networkInterfaceSet", "item", "association", "publicDnsName"],
        ),
        address: at(
            instance,
            &[
                "networkInterfaceSet",
                "item",
                "privateIpAddressesSet",
                "item",
                "association",
                "publicDnsName",
            ],
        ),
    }
}

/// One VPC of the caller's own with one subnet in it, and the router they live in.
struct Network {
    router: MultiServiceRouter,
    vpc_id: String,
    subnet_id: String,
}

/// Create that network: a VPC a CALLER creates, which "DNS attributes in your VPC" gives
/// `enableDnsHostnames` false — the state every row below starts from.
async fn network(case: &str) -> Network {
    let (router, _engine) = super::ec2_query::emulator();
    // The engine is dropped here only because the router holds its own handle on it; every
    // call below reaches the same store.
    let body = ok(&router, case, "CreateVpc", &[param("CidrBlock", "10.90.0.0/16")]).await;
    let vpc_id = element(&body, "vpcId")
        .unwrap_or_else(|| panic!("case {case:?}: CreateVpc reported no VPC: {body}"));
    let body = ok(
        &router,
        case,
        "CreateSubnet",
        &[param("VpcId", &vpc_id), param("CidrBlock", "10.90.1.0/24")],
    )
    .await;
    let subnet_id = element(&body, "subnetId")
        .unwrap_or_else(|| panic!("case {case:?}: CreateSubnet reported no subnet: {body}"));
    Network {
        router,
        vpc_id,
        subnet_id,
    }
}

/// Launch one instance into `subnet_id`, asking its primary interface for a public IPv4
/// address or not, and answer its id.
async fn launch(
    router: &MultiServiceRouter,
    case: &str,
    subnet_id: &str,
    public_address: bool,
) -> String {
    let mut params = vec![
        param("ImageId", AMI),
        param("MinCount", "1"),
        param("MaxCount", "1"),
        param("NetworkInterface.1.DeviceIndex", "0"),
        param("NetworkInterface.1.SubnetId", subnet_id),
    ];
    if public_address {
        params.push(param("NetworkInterface.1.AssociatePublicIpAddress", "true"));
    }
    let body = ok(router, case, "RunInstances", &params).await;
    element(&body, "instanceId")
        .unwrap_or_else(|| panic!("case {case:?}: RunInstances reported no instance: {body}"))
}

/// Turn one VPC attribute on or off. `member` is the request member that carries it
/// (`ModifyVpcAttributeRequest$EnableDnsHostnames` and the two beside it), which is the
/// spelling the ec2Query form field takes.
async fn modify_vpc_attribute(
    router: &MultiServiceRouter,
    case: &str,
    vpc_id: &str,
    member: &str,
    value: bool,
) {
    ok(
        router,
        case,
        "ModifyVpcAttribute",
        &[
            param("VpcId", vpc_id),
            param(
                &format!("{member}.Value"),
                if value { "true" } else { "false" },
            ),
        ],
    )
    .await;
}

/// The request member `ModifyVpcAttribute` carries the `enableDnsHostnames` attribute in.
const ENABLE_DNS_HOSTNAMES: &str = "EnableDnsHostnames";

/// RULE 1: the public hostname of an address follows the `enableDnsHostnames` attribute of the
/// VPC the address is held in, in every place that hostname is reported.
#[tokio::test]
async fn the_public_hostname_of_an_address_follows_its_vpcs_dns_hostnames_attribute() {
    let case = "an instance holding a public address";
    let network = network(case).await;
    let instance_id = launch(&network.router, case, &network.subnet_id, true).await;
    poll_until_running(&network.router, case, &instance_id).await;
    let held = reported_hostnames(&network.router, case, &instance_id).await;
    let public_ip = held
        .public_ip
        .clone()
        .unwrap_or_else(|| panic!("case {case:?}: the launch assigned no public address: {held:?}"));
    // The name that address has in a VPC that hands out public hostnames: AWS builds it as
    // `ec2-<dash-separated address>` under the Region's public DNS domain (Amazon EC2 User
    // Guide, "Amazon EC2 instance hostname types").
    let hostname = format!(
        "ec2-{}.{REGION}.compute.amazonaws.com",
        public_ip.replace('.', "-")
    );

    // (case, the attribute to move first, the name every report has to carry)
    let rows: [(&str, Option<bool>, &str); 4] = [
        ("a VPC a caller created, which hands out none", None, ""),
        ("that same VPC once DNS hostnames are enabled", Some(true), &hostname),
        ("and once they are disabled again", Some(false), ""),
        ("enabled once more, for the same address", Some(true), &hostname),
    ];
    for (row, attribute, expected) in rows {
        if let Some(value) = attribute {
            modify_vpc_attribute(
                &network.router,
                row,
                &network.vpc_id,
                ENABLE_DNS_HOSTNAMES,
                value,
            )
            .await;
        }
        let held = reported_hostnames(&network.router, row, &instance_id).await;
        assert_eq!(
            held.public_ip.as_deref(),
            Some(public_ip.as_str()),
            "case {row:?}: the attribute does not change which address the instance holds, \
             got {held:?}"
        );
        for (member, reported) in [
            ("dnsName", held.instance.as_deref()),
            ("networkInterfaceSet.item.association.publicDnsName", held.interface.as_deref()),
            (
                "networkInterfaceSet.item.privateIpAddressesSet.item.association.publicDnsName",
                held.address.as_deref(),
            ),
        ] {
            assert_eq!(
                reported,
                Some(expected),
                "case {row:?}: expected {member} {expected:?}, got {reported:?} in {held:?}"
            );
        }
    }

    // An instance holding NO public address has no hostname to report in any of those places,
    // whatever the attribute says — the member is the empty name rather than absent.
    let case = "an instance holding no public address, DNS hostnames enabled";
    let private_only = launch(&network.router, case, &network.subnet_id, false).await;
    poll_until_running(&network.router, case, &private_only).await;
    let held = reported_hostnames(&network.router, case, &private_only).await;
    assert_eq!(
        held.public_ip, None,
        "case {case:?}: the launch asked for no public address, got {held:?}"
    );
    assert_eq!(
        held.instance.as_deref(),
        Some(""),
        "case {case:?}: an instance with no public address reports the empty hostname, got \
         {held:?}"
    );
    assert_eq!(
        held.interface, None,
        "case {case:?}: an interface holding no public address reports no association at all, \
         got {held:?}"
    );
    assert_eq!(
        held.address, None,
        "case {case:?}: no private address is bound to a public one, so none reports an \
         association, got {held:?}"
    );
}

/// RULE 2: the public address AWS assigns an interface is reported against the PRIVATE address
/// it is bound to, with the same members the interface's own association carries.
#[tokio::test]
async fn an_assigned_public_address_is_reported_against_the_bound_private_address() {
    for (case, public_address) in [
        ("an eth0 that asked for a public address", true),
        ("an eth0 that asked for none", false),
    ] {
        let network = network(case).await;
        let instance_id = launch(&network.router, case, &network.subnet_id, public_address).await;
        let body = poll_until_running(&network.router, case, &instance_id).await;
        let parsed = document(case, &body);
        let instance = described(case, &parsed, &body);
        let addresses = child(instance, "networkInterfaceSet")
            .and_then(|set| child(set, "item"))
            .map(|interface| set_items(interface, "privateIpAddressesSet"))
            .unwrap_or_default();
        assert_eq!(
            addresses.len(),
            1,
            "case {case:?}: the launch's interface holds its one primary address, got {body}"
        );
        let address = addresses[0];
        assert_eq!(
            child_text(address, "primary").as_deref(),
            Some("true"),
            "case {case:?}: that address is the interface's primary one, got {body}"
        );
        let held = child(address, "association");
        let interface_association = at(instance, &["networkInterfaceSet", "item", "association"]);
        if !public_address {
            assert!(
                held.is_none() && interface_association.is_none(),
                "case {case:?}: an interface holding no public address reports no association, \
                 got {body}"
            );
            continue;
        }
        let held = held.unwrap_or_else(|| {
            panic!(
                "case {case:?}: the private address the public one is bound to reports it, \
                 got {body}"
            )
        });
        for member in ["publicIp", "ipOwnerId", "publicDnsName"] {
            assert_eq!(
                child_text(held, member),
                at(
                    instance,
                    &["networkInterfaceSet", "item", "association", member]
                ),
                "case {case:?}: the address's association reports the interface's own {member}, \
                 got {body}"
            );
        }
        assert_eq!(
            child_text(held, "publicIp"),
            child_text(instance, "ipAddress"),
            "case {case:?}: that association names the address the instance holds, got {body}"
        );
        // "An address AWS assigned itself" is owned by `amazon`, where an Elastic IP is owned
        // by the account that allocated it.
        assert_eq!(
            child_text(held, "ipOwnerId").as_deref(),
            Some("amazon"),
            "case {case:?}: the address came out of Amazon's own pool, got {body}"
        );
    }
}

/// RULE 3, the catalog half: every filter name the operation documents is RESOLVED — answered
/// with the instances it selects, none here — and only a name it does not document is refused.
///
/// The names come from the crate's own catalogs, whose source is the `Filters` member
/// documentation of `com.amazonaws.ec2#DescribeInstancesRequest`, so the table cannot drift
/// from what the operation resolves.
#[tokio::test]
async fn describe_instances_resolves_every_documented_filter_name() {
    let case = "the documented filter catalog";
    let network = network(case).await;
    let instance_id = launch(&network.router, case, &network.subnet_id, true).await;
    poll_until_running(&network.router, case, &instance_id).await;

    // A value no member of any instance carries, so a resolved name selects nothing and the
    // answer is the empty reservation set rather than a refusal.
    const UNHELD: &str = "no-instance-carries-this-value";
    let documented = ec2::state::SUPPORTED_INSTANCE_FILTERS
        .iter()
        .chain(ec2::state::REPORTED_INSTANCE_FILTERS.iter())
        .copied()
        .chain(["tag:Name"]);
    for name in documented {
        let (status, body) = call(
            &network.router,
            REGION,
            "DescribeInstances",
            &filters(&[(name, &[UNHELD])]),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {name:?}: a documented filter name is resolved, got {status} with body {body}"
        );
        assert!(
            super::ec2_query::elements(&body, "instanceId").is_empty(),
            "case {name:?}: no instance carries {UNHELD:?}, so none is selected, got {body}"
        );
    }

    // A name the operation does not document is refused instead: answering 200 with an empty
    // set would report a misspelling as absence. Each row is a misspelling of a name above.
    for name in [
        "dns-names",
        "root-device",
        "metadata-options.http-token",
        "network-interface.association.public-ipv4",
        "tag:",
    ] {
        let (status, body) = call(
            &network.router,
            REGION,
            "DescribeInstances",
            &filters(&[(name, &["x"])]),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {name:?}: an undocumented filter name is refused, got {status} with body {body}"
        );
        assert_eq!(
            super::ec2_query::error_code(&body),
            "InvalidParameterValue",
            "case {name:?}: expected InvalidParameterValue, got {body}"
        );
        assert!(
            super::ec2_query::error_message(&body).contains(&format!("The filter '{name}' is invalid")),
            "case {name:?}: the refusal names the filter, got {body}"
        );
    }
}

/// RULE 3, the value half: a documented filter selects an instance by the value the operation
/// REPORTS for that member, and selects nothing for a value it does not report.
///
/// Each row names the filter and the path to the member it reads in the instance report, so
/// the expected value is the one the service itself answered with rather than a literal.
#[tokio::test]
async fn a_documented_filter_selects_on_the_value_the_instance_report_carries() {
    let case = "filters on reported members";
    let network = network(case).await;
    // The VPC hands out public hostnames, so the `dnsName` row below has a name to select on.
    modify_vpc_attribute(
        &network.router,
        case,
        &network.vpc_id,
        ENABLE_DNS_HOSTNAMES,
        true,
    )
    .await;
    let instance_id = launch(&network.router, case, &network.subnet_id, true).await;
    let body = poll_until_running(&network.router, case, &instance_id).await;

    // (the filter name, the path to the member of the instance report it reads)
    let rows: [(&str, &[&str]); 21] = [
        ("dns-name", &["dnsName"]),
        ("root-device-name", &["rootDeviceName"]),
        ("root-device-type", &["rootDeviceType"]),
        ("hypervisor", &["hypervisor"]),
        ("virtualization-type", &["virtualizationType"]),
        ("ena-support", &["enaSupport"]),
        ("boot-mode", &["bootMode"]),
        ("current-instance-boot-mode", &["currentInstanceBootMode"]),
        ("platform-details", &["platformDetails"]),
        ("usage-operation", &["usageOperation"]),
        ("tenancy", &["placement", "tenancy"]),
        ("metadata-options.http-tokens", &["metadataOptions", "httpTokens"]),
        // The IPv4 metadata endpoint IS the instance's metadata HTTP endpoint, so the filter
        // selects on the state that endpoint reports.
        (
            "metadata-options.http-protocol-ipv4",
            &["metadataOptions", "httpEndpoint"],
        ),
        ("metadata-options.state", &["metadataOptions", "state"]),
        ("maintenance-options.auto-recovery", &["maintenanceOptions", "autoRecovery"]),
        ("hibernation-options.configured", &["hibernationOptions", "configured"]),
        ("operator.managed", &["operator", "managed"]),
        (
            "private-dns-name-options.hostname-type",
            &["privateDnsNameOptions", "hostnameType"],
        ),
        ("block-device-mapping.device-name", &["blockDeviceMapping", "item", "deviceName"]),
        (
            "block-device-mapping.volume-id",
            &["blockDeviceMapping", "item", "ebs", "volumeId"],
        ),
        (
            "network-interface.addresses.association.public-ip",
            &[
                "networkInterfaceSet",
                "item",
                "privateIpAddressesSet",
                "item",
                "association",
                "publicIp",
            ],
        ),
    ];
    for (name, path) in rows {
        let parsed = document(case, &body);
        let instance = described(case, &parsed, &body);
        let reported = at(instance, path).unwrap_or_else(|| {
            panic!("case {name:?}: the report carries no {path:?} to filter on: {body}")
        });
        assert!(
            !reported.is_empty(),
            "case {name:?}: the report's {path:?} is empty, so the row selects on nothing: \
             {body}"
        );
        drop(parsed);
        let hit = ok(
            &network.router,
            name,
            "DescribeInstances",
            &filters(&[(name, &[reported.as_str()])]),
        )
        .await;
        assert_eq!(
            super::ec2_query::elements(&hit, "instanceId"),
            vec![instance_id.clone()],
            "case {name:?}: the filter must select the instance reporting {reported:?}, got {hit}"
        );
        let miss = ok(
            &network.router,
            name,
            "DescribeInstances",
            &filters(&[(name, &[&format!("{reported}-and-more")])]),
        )
        .await;
        assert!(
            super::ec2_query::elements(&miss, "instanceId").is_empty(),
            "case {name:?}: a value the instance does not report selects nothing, got {miss}"
        );
    }

    // The timestamp member, through the wildcard its own documentation gives: "you can use a
    // wildcard (*), for example, 2021-09-29T*, which matches an entire day"
    // (`DescribeInstancesRequest$Filters`).
    let parsed = document(case, &body);
    let instance = described(case, &parsed, &body);
    let launch_time = child_text(instance, "launchTime")
        .unwrap_or_else(|| panic!("case {case:?}: the report carries no launchTime: {body}"));
    let day = launch_time
        .split_once('T')
        .map(|(day, _)| format!("{day}T*"))
        .unwrap_or_else(|| panic!("case {case:?}: launchTime {launch_time:?} is not ISO 8601"));
    drop(parsed);
    let hit = ok(
        &network.router,
        "launch-time",
        "DescribeInstances",
        &filters(&[("launch-time", &[day.as_str()])]),
    )
    .await;
    assert_eq!(
        super::ec2_query::elements(&hit, "instanceId"),
        vec![instance_id.clone()],
        "case \"launch-time\": {day:?} must match the day the instance was launched, got {hit}"
    );

    // The EC2-Classic group filters select nothing for a VPC instance, whose groups live on
    // its primary network interface and are selected by `instance.group-id` beside them.
    let group_id = at(
        described(case, &document(case, &body), &body),
        &["groupSet", "item", "groupId"],
    )
    .unwrap_or_else(|| panic!("case {case:?}: the instance reports no security group: {body}"));
    for (name, expected) in [
        ("instance.group-id", vec![instance_id.clone()]),
        ("group-id", Vec::new()),
    ] {
        let selected = ok(
            &network.router,
            name,
            "DescribeInstances",
            &filters(&[(name, &[group_id.as_str()])]),
        )
        .await;
        assert_eq!(
            super::ec2_query::elements(&selected, "instanceId"),
            expected,
            "case {name:?}: expected {expected:?} for the group {group_id}, got {selected}"
        );
    }
}

/// The id of the network interface the instance `instance_id` holds at device index 0.
async fn primary_interface(router: &MultiServiceRouter, case: &str, instance_id: &str) -> String {
    let body = describe(router, case, instance_id).await;
    let parsed = document(case, &body);
    let instance = described(case, &parsed, &body);
    at(
        instance,
        &["networkInterfaceSet", "item", "networkInterfaceId"],
    )
    .unwrap_or_else(|| panic!("case {case:?}: {instance_id} reports no interface: {body}"))
}

/// The Amazon public hostname of `public_ip` in the Region this suite signs for: AWS builds it
/// as `ec2-<dash-separated address>` under the Region's public DNS domain (Amazon EC2 User
/// Guide, "Amazon EC2 instance hostname types").
fn hostname_of(public_ip: &str) -> String {
    format!(
        "ec2-{}.{REGION}.compute.amazonaws.com",
        public_ip.replace('.', "-")
    )
}

/// RULE 1, where one interface holds TWO public addresses: each association names the address
/// IT holds, so turning the VPC's attribute on gives each one its own hostname rather than one
/// name to all of them.
///
/// An interface reaches that state through the two writers of an association: the launch
/// assigns the address AWS owns to the interface's primary private address, and
/// `AssociateAddress` then binds an Elastic IP of the account to a SECONDARY private address
/// of the same interface.
#[tokio::test]
async fn each_association_of_an_interface_names_the_public_address_it_holds() {
    let case = "an interface holding an AWS address and an Elastic IP";
    let network = network(case).await;
    let instance_id = launch(&network.router, case, &network.subnet_id, true).await;
    poll_until_running(&network.router, case, &instance_id).await;
    let assigned = reported_hostnames(&network.router, case, &instance_id)
        .await
        .public_ip
        .unwrap_or_else(|| panic!("case {case:?}: the launch assigned no public address"));
    let network_interface_id = primary_interface(&network.router, case, &instance_id).await;

    // A secondary private address on that same interface, and an Elastic IP of the account
    // bound to it.
    let body = ok(
        &network.router,
        case,
        "AssignPrivateIpAddresses",
        &[
            param("NetworkInterfaceId", &network_interface_id),
            param("SecondaryPrivateIpAddressCount", "1"),
        ],
    )
    .await;
    let parsed = document(case, &body);
    let secondary = set_items(parsed.root_element(), "assignedPrivateIpAddressesSet")
        .into_iter()
        .filter_map(|item| child_text(item, "privateIpAddress"))
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: no secondary address assigned: {body}"));
    drop(parsed);
    let allocated = ok(&network.router, case, "AllocateAddress", &[]).await;
    let allocation_id = element(&allocated, "allocationId")
        .unwrap_or_else(|| panic!("case {case:?}: no allocationId in {allocated}"));
    let elastic_ip = element(&allocated, "publicIp")
        .unwrap_or_else(|| panic!("case {case:?}: no publicIp in {allocated}"));
    // "The Elastic IP address must be allocated in the same Region and the VPC must have an
    // internet gateway attached" — the precondition `AssociateAddress` holds the VPC to.
    let gateway = ok(&network.router, case, "CreateInternetGateway", &[]).await;
    let gateway_id = element(&gateway, "internetGatewayId")
        .unwrap_or_else(|| panic!("case {case:?}: no internetGatewayId in {gateway}"));
    ok(
        &network.router,
        case,
        "AttachInternetGateway",
        &[
            param("InternetGatewayId", &gateway_id),
            param("VpcId", &network.vpc_id),
        ],
    )
    .await;
    ok(
        &network.router,
        case,
        "AssociateAddress",
        &[
            param("AllocationId", &allocation_id),
            param("NetworkInterfaceId", &network_interface_id),
            param("PrivateIpAddress", &secondary),
        ],
    )
    .await;

    // The attribute the hostnames follow, turned on with both addresses already held.
    modify_vpc_attribute(
        &network.router,
        case,
        &network.vpc_id,
        ENABLE_DNS_HOSTNAMES,
        true,
    )
    .await;

    let body = describe(&network.router, case, &instance_id).await;
    let parsed = document(case, &body);
    let instance = described(case, &parsed, &body);
    let interface = child(instance, "networkInterfaceSet")
        .and_then(|set| child(set, "item"))
        .unwrap_or_else(|| panic!("case {case:?}: no interface in {body}"));
    // Every association the interface reports, as `(publicIp, publicDnsName)`: its own, and
    // one per private address bound to a public one.
    let mut reported: Vec<(String, String)> = Vec::new();
    if let Some(held) = child(interface, "association") {
        reported.push((
            child_text(held, "publicIp").unwrap_or_default(),
            child_text(held, "publicDnsName").unwrap_or_default(),
        ));
    }
    for address in set_items(interface, "privateIpAddressesSet") {
        if let Some(held) = child(address, "association") {
            reported.push((
                child_text(held, "publicIp").unwrap_or_default(),
                child_text(held, "publicDnsName").unwrap_or_default(),
            ));
        }
    }
    drop(parsed);
    assert_eq!(
        reported.len(),
        3,
        "case {case:?}: the interface reports its own association and one per bound address, \
         got {reported:?}"
    );
    for (public_ip, name) in &reported {
        assert_eq!(
            name,
            &hostname_of(public_ip),
            "case {case:?}: the hostname beside {public_ip} must name that address, got \
             {reported:?}"
        );
    }
    // And both addresses are there: the one AWS assigned the interface, bound to its primary
    // private address, and the Elastic IP bound to the secondary one.
    for address in [assigned.as_str(), elastic_ip.as_str()] {
        assert!(
            reported.iter().any(|(public_ip, _)| public_ip == address),
            "case {case:?}: {address} must still be reported, got {reported:?}"
        );
    }
}

/// RULES 1 and 2 through `ModifyNetworkInterfaceAttribute`: the attribute that assigns an
/// interface a public IPv4 address, and the one that takes it away, move it everywhere it is
/// reported — the interface's association, the private address it is bound to, and the address
/// and hostname the INSTANCE holding that interface reports.
#[tokio::test]
async fn the_public_address_attribute_moves_the_address_everywhere_it_is_reported() {
    let case = "an attached eth0 given a public address after its launch";
    let network = network(case).await;
    // The VPC hands out hostnames, so the name is visible as soon as the address is.
    modify_vpc_attribute(
        &network.router,
        case,
        &network.vpc_id,
        ENABLE_DNS_HOSTNAMES,
        true,
    )
    .await;
    let instance_id = launch(&network.router, case, &network.subnet_id, false).await;
    poll_until_running(&network.router, case, &instance_id).await;
    let network_interface_id = primary_interface(&network.router, case, &instance_id).await;
    let held = reported_hostnames(&network.router, case, &instance_id).await;
    assert_eq!(
        (held.public_ip.as_deref(), held.instance.as_deref()),
        (None, Some("")),
        "case {case:?}: the launch asked for no address, so there is none to name, got {held:?}"
    );

    for (row, enabled) in [("enabled", true), ("disabled", false)] {
        ok(
            &network.router,
            row,
            "ModifyNetworkInterfaceAttribute",
            &[
                param("NetworkInterfaceId", &network_interface_id),
                param("AssociatePublicIpAddress", if enabled { "true" } else { "false" }),
            ],
        )
        .await;
        let held = reported_hostnames(&network.router, row, &instance_id).await;
        if !enabled {
            assert_eq!(
                (held.public_ip.as_deref(), held.instance.as_deref()),
                (None, Some("")),
                "case {row:?}: an address nothing holds any more is not the instance's, got \
                 {held:?}"
            );
            assert_eq!(
                (held.interface.as_deref(), held.address.as_deref()),
                (None, None),
                "case {row:?}: the interface and its address report no association, got {held:?}"
            );
            continue;
        }
        let public_ip = held.public_ip.clone().unwrap_or_else(|| {
            panic!("case {row:?}: the instance must report the address its interface was given: {held:?}")
        });
        let expected = hostname_of(&public_ip);
        for (member, reported) in [
            ("dnsName", held.instance.as_deref()),
            (
                "networkInterfaceSet.item.association.publicDnsName",
                held.interface.as_deref(),
            ),
            (
                "networkInterfaceSet.item.privateIpAddressesSet.item.association.publicDnsName",
                held.address.as_deref(),
            ),
        ] {
            assert_eq!(
                reported,
                Some(expected.as_str()),
                "case {row:?}: expected {member} {expected:?}, got {reported:?} in {held:?}"
            );
        }
    }
}
