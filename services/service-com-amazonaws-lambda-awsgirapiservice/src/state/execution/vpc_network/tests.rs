// SPDX-License-Identifier: CC0-1.0

//! The unit tests of the VPC attachment: the route a destination resolves through, the
//! address the VPC's own DNS server answers at, and what that server answers.

use ::std::net::Ipv4Addr;

use super::{attachment_of, VpcAttachment, RESOLVER_PORT};
use ::service_com_amazonaws_ec2_amazonec2::ops::types as ec2_types;
use ::service_com_amazonaws_ec2_amazonec2::state as ec2;
use ::simple_dns::rdata::RData;
use ::simple_dns::{Name, Packet, Question, QCLASS, QTYPE, RCODE, TYPE};

/// The Region the rows below run in, whose private DNS domain is
/// `<region>.compute.internal`.
pub(crate) const REGION: &str = "us-west-2";

/// The id every query below carries, which its reply has to answer under.
const QUERY_ID: u16 = 0x4d2;

/// One VPC of `cidr`, with one subnet per entry of `subnets` and one Lambda interface in each,
/// written into the EC2 store the way that service holds them — so the attachment below is
/// read off the records EC2 itself keeps.
pub(crate) fn attached(
    engine: &::engine::InMemoryEngine,
    cidr: &str,
    subnets: &[(&str, &str)],
    dns_support: bool,
) -> VpcAttachment {
    let state = ec2::service_state(engine).expect("the engine carries an EC2 store");
    let mut interface_ids: Vec<String> = Vec::new();
    {
        let mut store = state.lock();
        let vpc_id = "vpc-0123456789abcdef0".to_string();
        store.vpcs.insert(
            vpc_id.clone(),
            ec2_types::Vpc {
                vpc_id: Some(vpc_id.clone()),
                cidr_block: Some(cidr.to_string()),
                ..Default::default()
            },
        );
        store.vpc_attributes.insert(
            vpc_id.clone(),
            ec2::VpcAttributes {
                enable_dns_support: dns_support,
                enable_dns_hostnames: false,
                enable_network_address_usage_metrics: false,
            },
        );
        for (index, (subnet_cidr, address)) in subnets.iter().enumerate() {
            let subnet_id = format!("subnet-{index:017x}");
            store.subnets.insert(
                subnet_id.clone(),
                ec2_types::Subnet {
                    subnet_id: Some(subnet_id.clone()),
                    vpc_id: Some(vpc_id.clone()),
                    cidr_block: Some((*subnet_cidr).to_string()),
                    ..Default::default()
                },
            );
            let interface_id = format!("eni-{index:017x}");
            store.network_interfaces.insert(
                interface_id.clone(),
                ec2_types::NetworkInterface {
                    network_interface_id: Some(interface_id.clone()),
                    subnet_id: Some(subnet_id),
                    vpc_id: Some(vpc_id.clone()),
                    private_ip_address: Some((*address).to_string()),
                    ..Default::default()
                },
            );
            interface_ids.push(interface_id);
        }
    }
    attachment_of(engine, REGION, &interface_ids).expect("the interfaces stand for an attachment")
}

/// The attachment of a function in one VPC of `10.7.0.0/16` with an interface in each of two
/// subnets.
pub(crate) fn two_subnet_attachment(engine: &::engine::InMemoryEngine) -> VpcAttachment {
    attached(
        engine,
        "10.7.0.0/16",
        &[("10.7.1.0/24", "10.7.1.37"), ("10.7.2.0/24", "10.7.2.19")],
        true,
    )
}

/// RULE: a route toward an address of the VPC resolves through the interface in that
/// address's own subnet, and through the first interface the function holds for an address
/// elsewhere in the VPC; an address outside the VPC's ranges has no route at all.
///
/// The rows differ along where the destination sits: in each of the two subnets an interface
/// is in, elsewhere in the VPC, at the VPC's own resolver, and outside the VPC.
#[test]
fn a_route_resolves_through_the_interface_of_the_destination_s_own_subnet() {
    let engine = ::engine::InMemoryEngine::new();
    let attachment = two_subnet_attachment(&engine);
    let rows: [(&str, &str, Option<&str>); 6] = [
        ("the first interface's own subnet", "10.7.1.200", Some("10.7.1.37")),
        ("the second interface's own subnet", "10.7.2.200", Some("10.7.2.19")),
        ("elsewhere in the VPC", "10.7.9.9", Some("10.7.1.37")),
        ("the VPC's resolver", "10.7.0.2", Some("10.7.1.37")),
        ("an address of another VPC's range", "10.8.0.5", None),
        ("a public address", "198.51.100.7", None),
    ];
    for (case, destination, expected) in rows {
        let destination: Ipv4Addr = destination.parse().expect("a test address");
        let source = attachment.source_toward(destination);
        let expected = expected.map(|address| address.parse::<Ipv4Addr>().expect("a test address"));
        assert_eq!(
            source, expected,
            "case {case:?}: expected the source toward {destination} to be {expected:?}, got \
             {source:?}"
        );
    }
}

/// RULE: the VPC's DNS server answers at the second address of the VPC's own IPv4 range, on
/// the DNS port.
///
/// The rows differ along the range the VPC was created with, which is what the address is
/// read off.
#[test]
fn the_resolver_sits_at_the_second_address_of_the_vpc_range() {
    let rows: [(&str, &str, &str, &str); 4] = [
        ("a /16", "10.7.0.0/16", "10.7.1.0/24", "10.7.0.2"),
        ("another /16", "172.31.0.0/16", "172.31.4.0/24", "172.31.0.2"),
        ("a /24", "192.168.10.0/24", "192.168.10.0/24", "192.168.10.2"),
        ("a /20", "10.132.16.0/20", "10.132.16.0/24", "10.132.16.2"),
    ];
    for (case, cidr, subnet, expected) in rows {
        let engine = ::engine::InMemoryEngine::new();
        let address = format!("{}.37", subnet.trim_end_matches(".0/24"));
        let attachment = attached(&engine, cidr, &[(subnet, address.as_str())], true);
        let resolver = attachment.resolver();
        let expected: Ipv4Addr = expected.parse().expect("a test address");
        assert_eq!(
            resolver,
            Some(expected),
            "case {case:?}: expected the resolver of {cidr} at {expected}, got {resolver:?}"
        );
        assert!(
            attachment.is_resolver(expected, RESOLVER_PORT),
            "case {case:?}: expected {expected}:{RESOLVER_PORT} to be the resolver"
        );
        assert!(
            !attachment.is_resolver(expected, RESOLVER_PORT + 1),
            "case {case:?}: expected no resolver on any other port"
        );
    }
}

/// One question, as a client frames it on the wire.
fn query(name: &str, kind: TYPE) -> Vec<u8> {
    let mut packet = Packet::new_query(QUERY_ID);
    packet.set_flags(::simple_dns::PacketFlag::RECURSION_DESIRED);
    packet.questions.push(Question::new(
        Name::new(name).expect("a test name"),
        QTYPE::TYPE(kind),
        QCLASS::CLASS(::simple_dns::CLASS::IN),
        false,
    ));
    packet.build_bytes_vec().expect("the query encodes")
}

/// RULE: the VPC's DNS server answers a query for an Amazon-provided hostname of an address in
/// the VPC with that address, a reverse query for such an address with that hostname, a name
/// of the VPC's internal zone that names no address in it with NXDOMAIN, and a name of any
/// other zone with SERVFAIL.
///
/// The rows differ along what is asked: the hostname of an address in each subnet, a hostname
/// of an address outside the VPC, a hostname of no such form, a reverse question, and a name
/// of another zone.
#[test]
fn the_vpc_resolver_answers_the_names_of_its_own_zone() {
    let engine = ::engine::InMemoryEngine::new();
    let attachment = two_subnet_attachment(&engine);
    let rows: [(&str, &str, TYPE, RCODE, Option<&str>); 7] = [
        (
            "a hostname of an address in the first subnet",
            "ip-10-7-1-37.us-west-2.compute.internal",
            TYPE::A,
            RCODE::NoError,
            Some("10.7.1.37"),
        ),
        (
            "a hostname of an address in the second subnet",
            "ip-10-7-2-19.us-west-2.compute.internal",
            TYPE::A,
            RCODE::NoError,
            Some("10.7.2.19"),
        ),
        (
            "a hostname of an address elsewhere in the VPC",
            "ip-10-7-200-1.us-west-2.compute.internal",
            TYPE::A,
            RCODE::NoError,
            Some("10.7.200.1"),
        ),
        (
            "a hostname of an address outside the VPC",
            "ip-10-9-0-1.us-west-2.compute.internal",
            TYPE::A,
            RCODE::NameError,
            None,
        ),
        (
            "a name of the internal zone of no hostname form",
            "database.us-west-2.compute.internal",
            TYPE::A,
            RCODE::NameError,
            None,
        ),
        (
            "a reverse question about an address in the VPC",
            "37.1.7.10.in-addr.arpa",
            TYPE::PTR,
            RCODE::NoError,
            Some("ip-10-7-1-37.us-west-2.compute.internal"),
        ),
        (
            "a name of another zone",
            "example.com",
            TYPE::A,
            RCODE::ServerFailure,
            None,
        ),
    ];
    for (case, name, kind, code, answered) in rows {
        let asked = query(name, kind);
        let reply = attachment
            .resolved(&asked)
            .unwrap_or_else(|| panic!("case {case:?}: the resolver must reply"));
        let reply = Packet::parse(&reply)
            .unwrap_or_else(|error| panic!("case {case:?}: the reply must parse: {error}"));
        assert!(
            reply.has_flags(::simple_dns::PacketFlag::RESPONSE),
            "case {case:?}: expected a response, got {reply:?}"
        );
        assert_eq!(
            reply.id(),
            QUERY_ID,
            "case {case:?}: expected the question's own id, got {}",
            reply.id()
        );
        assert_eq!(
            reply.rcode(),
            code,
            "case {case:?}: expected {code:?}, got {:?}",
            reply.rcode()
        );
        let rendered: Vec<String> = reply
            .answers
            .iter()
            .map(|record| match &record.rdata {
                RData::A(address) => ::std::net::Ipv4Addr::from(address.address).to_string(),
                RData::PTR(name) => name.0.to_string(),
                other => format!("{other:?}"),
            })
            .collect();
        let expected: Vec<String> = answered.into_iter().map(str::to_string).collect();
        assert_eq!(
            rendered, expected,
            "case {case:?}: expected the answers {expected:?}, got {rendered:?}"
        );
    }
}

/// RULE: a VPC whose `enableDnsSupport` attribute is off has no resolver, so nothing replies
/// to a query sent to that address.
#[test]
fn a_vpc_with_dns_support_off_answers_no_query() {
    let engine = ::engine::InMemoryEngine::new();
    let attachment = attached(
        &engine,
        "10.7.0.0/16",
        &[("10.7.1.0/24", "10.7.1.37")],
        false,
    );
    let asked = query("ip-10-7-1-37.us-west-2.compute.internal", TYPE::A);
    let reply = attachment.resolved(&asked);
    assert!(
        reply.is_none(),
        "expected no reply from a VPC with DNS support off, got {} bytes",
        reply.unwrap_or_default().len()
    );
}

/// RULE: a datagram that carries no readable DNS message is dropped rather than answered.
#[test]
fn a_datagram_that_is_no_dns_message_is_dropped() {
    let engine = ::engine::InMemoryEngine::new();
    let attachment = two_subnet_attachment(&engine);
    let rows: [(&str, &[u8]); 3] = [
        ("an empty datagram", b""),
        ("a truncated header", b"\x04\xd2\x01"),
        ("text", b"not a dns message at all"),
    ];
    for (case, payload) in rows {
        let reply = attachment.resolved(payload);
        assert!(
            reply.is_none(),
            "case {case:?}: expected the datagram to be dropped, got a reply of {} bytes",
            reply.unwrap_or_default().len()
        );
    }
}

/// RULE: a function whose configuration named no subnet has no attachment at all, and neither
/// has one whose interfaces the inventory no longer holds.
#[test]
fn a_function_with_no_interfaces_has_no_attachment() {
    let engine = ::engine::InMemoryEngine::new();
    let rows: [(&str, Vec<String>); 2] = [
        ("no interface at all", Vec::new()),
        (
            "an interface the inventory does not hold",
            vec!["eni-0000000000bad0bad".to_string()],
        ),
    ];
    for (case, interface_ids) in rows {
        let attachment = attachment_of(&engine, REGION, &interface_ids);
        assert!(
            attachment.is_none(),
            "case {case:?}: expected no attachment, got {attachment:?}"
        );
    }
}
