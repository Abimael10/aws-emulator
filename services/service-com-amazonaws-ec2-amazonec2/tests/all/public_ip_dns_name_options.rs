// SPDX-License-Identifier: CC0-1.0

//! The public-hostname members an EC2 instance and its network interfaces report.
//!
//! The rule under test: a reported resource that holds a public IPv4 address reports the
//! PUBLIC HOSTNAME of that address and the public-hostname options naming it
//! (`PublicIpDnsNameOptions`: `dnsHostnameType` `public-ipv4-dns-name` and `publicIpv4DnsName`,
//! the hostname itself), and a resource that holds no public hostname reports the EMPTY
//! hostname and no options at all. The hostname belongs to the address under the VPC's
//! `enableDnsHostnames` attribute — "this name is only available if you've enabled DNS
//! hostnames for your VPC" (`Instance$PublicDnsName`) — so the members follow both the address
//! the resource holds and that attribute, and every report of one resource carries the same
//! answer: the instance's own `dnsName`/`publicIpDnsNameOptions`, the same two members on each
//! interface the instance reports, and the interface read on its own through
//! `DescribeNetworkInterfaces`.
//!
//! The rows span what that rule generalises over: the REPORT PATH (a launch reply, the
//! instance read back, the interface read on its own), the VPC attribute (on, off, on again)
//! and whether the resource holds a public address at all.
//!
//! The HOSTNAME half of that rule — that the name is the VPC's to give, and that it is the
//! name of the address the resource holds — is already pinned by
//! [`super::instance_public_hostnames_and_filters`], which drives the same two axes against
//! the instance's `dnsName` and the associations under it. What this suite adds is the half
//! that suite does not read, and the half this model revision declares no member for: the
//! `publicIpDnsNameOptions` of the instance and of each interface, and the interface's own
//! `publicDnsName` under the instance that holds it. The axes are driven again rather than
//! asserted once, because the two halves are one answer: the members are DERIVED from that
//! hostname, so a suite that moved the hostname without re-reading them would not catch them
//! drifting apart.

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, primary_network_interface};
use super::wire::{child, child_text, document};

/// The Region every call of this suite is signed for.
const REGION: &str = "us-west-2";

/// An AMI id of the right FORM: which image a launch names is not the subject here.
const AMI: &str = "ami-0123456789abcdef0";

/// The `PublicIpDnsOption` an IPv4-only resource's hostname type is, written out as the model
/// spells it (`smithy_query(query="shape",shape="PublicIpDnsOption")` reports the values
/// `public-dual-stack-dns-name | public-ipv4-dns-name | public-ipv6-dns-name`).
const IPV4_HOSTNAME_TYPE: &str = "public-ipv4-dns-name";

/// That word is one the model declares, so the catalog the crate generated from the same
/// enum has to hold it: a type the emulator reports has to be a modelled one.
#[test]
fn the_ipv4_hostname_type_is_a_modelled_option() {
    assert!(
        ec2::ops::types::PUBLIC_IP_DNS_OPTION_VALUES.contains(&IPV4_HOSTNAME_TYPE),
        "case {IPV4_HOSTNAME_TYPE:?}: expected a member of the modelled PublicIpDnsOption \
         catalog, got {:?}",
        ec2::ops::types::PUBLIC_IP_DNS_OPTION_VALUES
    );
}

fn p(name: &str, value: &str) -> (String, String) {
    (name.to_string(), value.to_string())
}

/// The public hostname AWS gives the public IPv4 address `public_ip` in `REGION`:
/// `ec2-<dash-separated address>` under the Region's public DNS domain (Amazon EC2 User
/// Guide, "Amazon EC2 instance hostname types"). Written out here rather than read from the
/// crate, so the test pins the FORM a client resolves rather than echoing the emulator.
fn expected_hostname(public_ip: &str) -> String {
    format!(
        "ec2-{host}.{REGION}.compute.amazonaws.com",
        host = public_ip.replace('.', "-")
    )
}

/// The public-hostname members one reported resource carries: the hostname member, and the
/// `(dnsHostnameType, publicIpv4DnsName)` of the options naming it.
///
/// `None` for the hostname means the element was ABSENT; `Some("")` means present and empty —
/// the two cases the rule tells apart.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PublicHostname {
    hostname: Option<String>,
    options: Option<(Option<String>, Option<String>)>,
}

/// The public-hostname members of `node`, read through the given hostname member name:
/// `dnsName` on an instance, `publicDnsName` on a network interface.
fn public_hostname_of(node: roxmltree::Node<'_, '_>, hostname_member: &str) -> PublicHostname {
    PublicHostname {
        hostname: child_text(node, hostname_member),
        options: child(node, "publicIpDnsNameOptions").map(|options| {
            (
                child_text(options, "dnsHostnameType"),
                child_text(options, "publicIpv4DnsName"),
            )
        }),
    }
}

/// What one instance report says: the public address the instance holds, the instance's own
/// public-hostname members, and the members of its PRIMARY network interface.
#[derive(Debug, Clone)]
struct ReportedInstance {
    public_ip: Option<String>,
    instance: PublicHostname,
    interface: PublicHostname,
}

/// The one instance a `RunInstances` or `DescribeInstances` body reports, read off the element
/// that carries an `<instanceId>` and an `<instanceState>` of its own — so a member is never
/// read out of a shape that merely shares its name.
fn reported_instance(case: &str, body: &str) -> ReportedInstance {
    let parsed = document(case, body);
    let instance = parsed
        .descendants()
        .find(|node| {
            node.is_element()
                && child(*node, "instanceId").is_some()
                && child(*node, "instanceState").is_some()
        })
        .unwrap_or_else(|| panic!("case {case:?}: no reported instance in {body}"));
    let interface = child(instance, "networkInterfaceSet")
        .and_then(|set| child(set, "item"))
        .unwrap_or_else(|| panic!("case {case:?}: the instance reports no interface in {body}"));
    ReportedInstance {
        public_ip: child_text(instance, "ipAddress"),
        instance: public_hostname_of(instance, "dnsName"),
        interface: public_hostname_of(interface, "publicDnsName"),
    }
}

/// The public-hostname members `DescribeNetworkInterfaces` reports for `network_interface_id`.
async fn reported_interface(
    router: &MultiServiceRouter,
    case: &str,
    network_interface_id: &str,
) -> PublicHostname {
    let (status, body) = call(
        router,
        REGION,
        "DescribeNetworkInterfaces",
        &[p("NetworkInterfaceId.1", network_interface_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeNetworkInterfaces must report {network_interface_id}, got \
         {status} with body {body}"
    );
    let parsed = document(case, &body);
    let interface = parsed
        .descendants()
        .find(|node| node.is_element() && child(*node, "networkInterfaceId").is_some())
        .unwrap_or_else(|| panic!("case {case:?}: no reported interface in {body}"));
    public_hostname_of(interface, "publicDnsName")
}

/// Launch one instance into the account's default network, and answer the launch reply beside
/// the instance's id.
async fn launch(router: &MultiServiceRouter, case: &str) -> (String, String) {
    let (status, body) = call(
        router,
        REGION,
        "RunInstances",
        &[p("ImageId", AMI), p("MinCount", "1"), p("MaxCount", "1")],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the launch must be served, got {status} with body {body}"
    );
    let instance_id = reported_instance_id(case, &body);
    (instance_id, body)
}

/// The id of the one instance a body reports.
fn reported_instance_id(case: &str, body: &str) -> String {
    let parsed = document(case, body);
    parsed
        .descendants()
        .find(|node| {
            node.is_element()
                && child(*node, "instanceId").is_some()
                && child(*node, "instanceState").is_some()
        })
        .and_then(|instance| child_text(instance, "instanceId"))
        .unwrap_or_else(|| panic!("case {case:?}: no instanceId in {body}"))
}

/// The `DescribeInstances` body for one instance.
async fn describe(router: &MultiServiceRouter, case: &str, instance_id: &str) -> String {
    let (status, body) = call(
        router,
        REGION,
        "DescribeInstances",
        &[p("InstanceId.1", instance_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeInstances must report {instance_id}, got {status} with body \
         {body}"
    );
    body
}

/// The members a resource holding the public hostname `hostname` must report.
fn carried(hostname: &str) -> PublicHostname {
    PublicHostname {
        hostname: Some(hostname.to_string()),
        options: Some((
            Some(IPV4_HOSTNAME_TYPE.to_string()),
            Some(hostname.to_string()),
        )),
    }
}

/// The members a resource holding NO public hostname must report: the empty hostname, and no
/// options to name one with.
fn no_hostname() -> PublicHostname {
    PublicHostname {
        hostname: Some(String::new()),
        options: None,
    }
}

/// The VPC the one instance of a report runs in.
fn reported_vpc_id(case: &str, body: &str) -> String {
    let parsed = document(case, body);
    parsed
        .descendants()
        .find(|node| {
            node.is_element()
                && child(*node, "instanceId").is_some()
                && child(*node, "instanceState").is_some()
        })
        .and_then(|instance| child_text(instance, "vpcId"))
        .unwrap_or_else(|| panic!("case {case:?}: no vpcId on the instance in {body}"))
}

/// Every report of one instance that holds a public address carries the hostname of THAT
/// address and the options naming it — in the launch reply, in the instance read back, and on
/// the interface read on its own.
#[tokio::test]
async fn every_report_of_a_public_instance_carries_the_hostname_of_its_address() {
    let (router, _engine) = super::ec2_query::emulator();
    let (instance_id, launched) = launch(&router, "the launch reply").await;
    let described = describe(&router, "the instance read back", &instance_id).await;
    let network_interface_id =
        primary_network_interface(&router, REGION, "the interface read on its own", &instance_id)
            .await;

    // (case, the body that reported the instance)
    let rows = [
        ("the launch reply", launched),
        ("the instance read back", described),
    ];
    let mut hostnames = Vec::new();
    for (case, body) in rows {
        let reported = reported_instance(case, &body);
        let public_ip = reported.public_ip.clone().unwrap_or_else(|| {
            panic!(
                "case {case:?}: an instance of the default network holds a public address, got \
                 {reported:?}"
            )
        });
        let expected = carried(&expected_hostname(&public_ip));
        assert_eq!(
            reported.instance, expected,
            "case {case:?}: expected the instance to report {expected:?} for {public_ip}, got \
             {:?}",
            reported.instance
        );
        assert_eq!(
            reported.interface, expected,
            "case {case:?}: expected the interface under the instance to report {expected:?} \
             for {public_ip}, got {:?}",
            reported.interface
        );
        hostnames.push((case, expected));
    }

    let case = "the interface read on its own";
    let on_its_own = reported_interface(&router, case, &network_interface_id).await;
    let (_, expected) = hostnames
        .first()
        .expect("the rows above reported at least one hostname");
    assert_eq!(
        &on_its_own, expected,
        "case {case:?}: expected {network_interface_id} read on its own to report \
         {expected:?}, got {on_its_own:?}"
    );
    // And the two report paths agree with each other, which is what makes the hostname one
    // fact rather than two.
    for (case, hostname) in &hostnames {
        assert_eq!(
            hostname, expected,
            "case {case:?}: every report of one instance carries one hostname, got \
             {hostname:?} against {expected:?}"
        );
    }
}

/// The members follow the VPC's `enableDnsHostnames` attribute: turning it off leaves the
/// instance holding its public address with no hostname and no options, and turning it back on
/// brings both back — through every report of the resource at once.
#[tokio::test]
async fn the_members_follow_the_vpc_dns_hostnames_attribute() {
    let (router, _engine) = super::ec2_query::emulator();
    let (instance_id, launched) = launch(&router, "the launch").await;
    let vpc_id = reported_vpc_id("the launch", &launched);
    let network_interface_id =
        primary_network_interface(&router, REGION, "the launch", &instance_id).await;
    let public_ip = reported_instance("the launch", &launched)
        .public_ip
        .expect("an instance of the default network holds a public address");

    // (case, what the VPC attribute is set to, what every report must then carry)
    let rows = [
        (
            "the default network, which hands out public hostnames",
            None,
            carried(&expected_hostname(&public_ip)),
        ),
        (
            "public hostnames turned off",
            Some("false"),
            no_hostname(),
        ),
        (
            "public hostnames turned on again",
            Some("true"),
            carried(&expected_hostname(&public_ip)),
        ),
    ];
    for (case, attribute, expected) in rows {
        if let Some(value) = attribute {
            let (status, body) = call(
                &router,
                REGION,
                "ModifyVpcAttribute",
                &[
                    p("VpcId", &vpc_id),
                    p("EnableDnsHostnames.Value", value),
                ],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: ModifyVpcAttribute must be served, got {status} with body {body}"
            );
        }
        let body = describe(&router, case, &instance_id).await;
        let reported = reported_instance(case, &body);
        assert_eq!(
            reported.public_ip.as_deref(),
            Some(public_ip.as_str()),
            "case {case:?}: the attribute changes the hostname, never the address, got {:?}",
            reported.public_ip
        );
        assert_eq!(
            reported.instance, expected,
            "case {case:?}: expected the instance to report {expected:?}, got {:?}",
            reported.instance
        );
        assert_eq!(
            reported.interface, expected,
            "case {case:?}: expected the interface under the instance to report {expected:?}, \
             got {:?}",
            reported.interface
        );
        let on_its_own = reported_interface(&router, case, &network_interface_id).await;
        assert_eq!(
            on_its_own, expected,
            "case {case:?}: expected {network_interface_id} read on its own to report \
             {expected:?}, got {on_its_own:?}"
        );
    }
}

/// And they follow the ADDRESS: an interface AWS's mapped public address has been taken off
/// holds no hostname, so every report of it carries the empty hostname and no options.
#[tokio::test]
async fn the_members_follow_the_address_the_interface_holds() {
    let (router, _engine) = super::ec2_query::emulator();
    let (instance_id, launched) = launch(&router, "the launch").await;
    let network_interface_id =
        primary_network_interface(&router, REGION, "the launch", &instance_id).await;
    let public_ip = reported_instance("the launch", &launched)
        .public_ip
        .expect("an instance of the default network holds a public address");

    // (case, whether the interface is to hold a public address, what every report must carry)
    let rows = [
        (
            "the address the launch assigned",
            None,
            carried(&expected_hostname(&public_ip)),
            Some(public_ip.clone()),
        ),
        (
            "the mapped address taken off the interface",
            Some("false"),
            no_hostname(),
            None,
        ),
        (
            "a fresh mapped address put back on it",
            Some("true"),
            carried(&expected_hostname(&public_ip)),
            Some(public_ip.clone()),
        ),
    ];
    for (case, associate, expected, address) in rows {
        if let Some(value) = associate {
            let (status, body) = call(
                &router,
                REGION,
                "ModifyNetworkInterfaceAttribute",
                &[
                    p("NetworkInterfaceId", &network_interface_id),
                    p("AssociatePublicIpAddress", value),
                ],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: ModifyNetworkInterfaceAttribute must be served, got {status} \
                 with body {body}"
            );
        }
        let body = describe(&router, case, &instance_id).await;
        let reported = reported_instance(case, &body);
        // A released address is one Amazon's pool takes back, so the address assigned again
        // need not be the one released: what is asserted is that the hostname NAMES whatever
        // address the interface holds.
        let expected = match (&address, &reported.public_ip) {
            (Some(_), Some(held)) => carried(&expected_hostname(held)),
            _ => expected,
        };
        assert_eq!(
            reported.public_ip.is_some(),
            address.is_some(),
            "case {case:?}: expected the instance to hold an address: {}, got {:?}",
            address.is_some(),
            reported.public_ip
        );
        assert_eq!(
            reported.instance, expected,
            "case {case:?}: expected the instance to report {expected:?}, got {:?}",
            reported.instance
        );
        assert_eq!(
            reported.interface, expected,
            "case {case:?}: expected the interface under the instance to report {expected:?}, \
             got {:?}",
            reported.interface
        );
        let on_its_own = reported_interface(&router, case, &network_interface_id).await;
        assert_eq!(
            on_its_own, expected,
            "case {case:?}: expected {network_interface_id} read on its own to report \
             {expected:?}, got {on_its_own:?}"
        );
    }
}
