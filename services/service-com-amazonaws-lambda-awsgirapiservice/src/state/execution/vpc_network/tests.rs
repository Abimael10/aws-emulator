// SPDX-License-Identifier: CC0-1.0

//! The unit tests of the VPC attachment: the route a destination resolves through, the
//! address the VPC's own DNS server answers at, and what that server answers.

use ::std::net::Ipv4Addr;

use super::{
    attachment_of, VpcAttachment, LINK_LOCAL_RESOLVER, MAX_QUESTIONS_PER_MESSAGE, RESOLVER_PORT,
};
use ::service_com_amazonaws_ec2_amazonec2::ops::types as ec2_types;
use ::service_com_amazonaws_ec2_amazonec2::state as ec2;
use ::service_common::vpc_dns_firewall::FirewallDecision;
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
/// elsewhere in the VPC or for the link-local address of the VPC's resolver, which is in no
/// subnet; an address outside the VPC's ranges has no route at all.
///
/// The rows differ along where the destination sits: in each of the two subnets an interface
/// is in, elsewhere in the VPC, at each address the VPC's own resolver answers at, and outside
/// the VPC.
#[test]
fn a_route_resolves_through_the_interface_of_the_destination_s_own_subnet() {
    let engine = ::engine::InMemoryEngine::new();
    let attachment = two_subnet_attachment(&engine);
    let link_local = LINK_LOCAL_RESOLVER.to_string();
    let rows: [(&str, &str, Option<&str>); 8] = [
        ("the first interface's own subnet", "10.7.1.200", Some("10.7.1.37")),
        ("the second interface's own subnet", "10.7.2.200", Some("10.7.2.19")),
        ("elsewhere in the VPC", "10.7.9.9", Some("10.7.1.37")),
        ("the VPC's resolver", "10.7.0.2", Some("10.7.1.37")),
        (
            "the link-local address that resolver also answers at",
            link_local.as_str(),
            Some("10.7.1.37"),
        ),
        (
            "another link-local address, which nothing of the VPC answers at",
            "169.254.11.22",
            None,
        ),
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

/// RULE: the VPC's DNS server answers at the second address of the VPC's own IPv4 range and at
/// the link-local address every VPC's resolver answers at, on the DNS port and on no other.
///
/// The rows differ along the range the VPC was created with, which is what the first of those
/// two addresses is read off.
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
        for (where_at, address) in [
            ("the base of the range plus two", expected),
            ("the link-local address", LINK_LOCAL_RESOLVER),
        ] {
            assert!(
                attachment.is_resolver(address, RESOLVER_PORT),
                "case {case:?}: expected {address}:{RESOLVER_PORT} ({where_at}) to be the \
                 resolver"
            );
            assert!(
                !attachment.is_resolver(address, RESOLVER_PORT + 1),
                "case {case:?}: expected no resolver at {address} on any other port"
            );
        }
    }
}

/// One question, as a client frames it on the wire.
fn query(name: &str, kind: TYPE) -> Vec<u8> {
    questions(&[(name, kind)])
}

/// A message asking every one of `asked`, as a client frames it on the wire.
fn questions(asked: &[(&str, TYPE)]) -> Vec<u8> {
    let mut packet = Packet::new_query(QUERY_ID);
    packet.set_flags(::simple_dns::PacketFlag::RECURSION_DESIRED);
    for (name, kind) in asked {
        packet.questions.push(Question::new(
            Name::new(name).expect("a test name"),
            QTYPE::TYPE(*kind),
            QCLASS::CLASS(::simple_dns::CLASS::IN),
            false,
        ));
    }
    packet.build_bytes_vec().expect("the query encodes")
}

/// RULE: this resolver answers every question of a message up to the number it answers
/// ([`MAX_QUESTIONS_PER_MESSAGE`]), which is what bounds the work one datagram can ask of it; a
/// message asking more is dropped, as one carrying no readable DNS message is.
///
/// The rows differ along how many questions the message carries: one, the bound itself, and one
/// past it.
#[test]
fn a_message_asking_more_questions_than_the_resolver_answers_is_dropped() {
    let engine = ::engine::InMemoryEngine::new();
    let attachment = two_subnet_attachment(&engine);
    let rows: [(&str, usize, Option<usize>); 3] = [
        ("one question", 1, Some(1)),
        ("the bound itself", MAX_QUESTIONS_PER_MESSAGE, Some(MAX_QUESTIONS_PER_MESSAGE)),
        ("one question past the bound", MAX_QUESTIONS_PER_MESSAGE + 1, None),
    ];
    for (case, count, answered) in rows {
        let asked: Vec<(&str, TYPE)> =
            (0..count).map(|_| ("ip-10-7-1-37.us-west-2.compute.internal", TYPE::A)).collect();
        let reply = attachment.resolved(&questions(&asked));
        match answered {
            Some(expected) => {
                let reply = reply
                    .unwrap_or_else(|| panic!("case {case:?}: the resolver must reply"));
                let reply = Packet::parse(&reply)
                    .unwrap_or_else(|error| panic!("case {case:?}: the reply must parse: {error}"));
                assert_eq!(
                    reply.questions.len(),
                    expected,
                    "case {case:?}: expected the reply to carry {expected} questions, got {}",
                    reply.questions.len()
                );
                assert_eq!(
                    reply.answers.len(),
                    expected,
                    "case {case:?}: expected one answer per question, got {}",
                    reply.answers.len()
                );
            }
            None => assert!(
                reply.is_none(),
                "case {case:?}: expected the message to be dropped, got a reply of {} bytes",
                reply.unwrap_or_default().len()
            ),
        }
    }
}

/// RULE: the VPC's DNS server answers a query for an Amazon-provided hostname of an address in
/// the VPC with that address, a reverse query for such an address with that hostname, a name
/// of the VPC's internal zone that names no address in it with NXDOMAIN, a public name whose
/// addresses a DNS standard fixes out of that standard, and a name of any other zone with
/// SERVFAIL.
///
/// The rows differ along what is asked: the hostname of an address in each subnet, a hostname
/// of an address outside the VPC, a hostname of no such form, a reverse question, a name of
/// another zone, and a public name a standard fixes.
#[test]
fn the_vpc_resolver_answers_the_names_of_its_own_zone() {
    let engine = ::engine::InMemoryEngine::new();
    let attachment = two_subnet_attachment(&engine);
    let rows: [(&str, &str, TYPE, RCODE, Option<&str>); 8] = [
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
            "broker.example.com",
            TYPE::A,
            RCODE::ServerFailure,
            None,
        ),
        (
            // A name whose addresses a DNS standard fixes is answered out of that standard
            // rather than recursed for (`service_common::public_dns_standard`).
            "a public name a standard fixes the address of",
            "example.com",
            TYPE::A,
            RCODE::NoError,
            Some("192.0.2.1"),
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

/// A filter blocking ONE name of every VPC the `decision` way, for the rows below: the stand-in
/// for the service that owns DNS Firewall, which records its own filter in this register when it
/// is mounted on the engine.
struct OneBlockedName {
    /// The name it decides.
    name: &'static str,
    /// What it decides about that name.
    decision: FirewallDecision,
}

impl ::service_common::vpc_dns_firewall::VpcQueryFilter for OneBlockedName {
    fn decisions(
        &self,
        _vpc_id: &str,
        asked: &[(String, Option<u16>)],
    ) -> Vec<Option<FirewallDecision>> {
        asked
            .iter()
            .map(|(name, _qtype)| {
                (name.trim_end_matches('.') == self.name).then(|| self.decision.clone())
            })
            .collect()
    }
}

/// One row of the table below: the case, what the registered filter decides about the name, the
/// name asked for, the response code the reply must carry, and the answers it must hold.
type FirewallRow = (
    &'static str,
    Option<FirewallDecision>,
    &'static str,
    RCODE,
    Vec<String>,
);

/// RULE: the DNS Firewall filtering the VPC decides a query BEFORE the resolver reads its own
/// zone for it, so a name a matched `BLOCK` rule covers is answered that rule's `BlockResponse`
/// way whether or not the VPC's zone holds the name, while a name no rule covers — and one a
/// matched `ALLOW` rule permits — is answered out of the zone exactly as it was.
///
/// The rows differ along what the firewall decided (`NXDOMAIN`, `NODATA`, an `OVERRIDE` record,
/// `ALLOW`, no decision at all) and along whether the name is one the VPC's own zone holds.
#[test]
fn a_query_the_vpcs_firewall_blocks_is_answered_the_way_that_block_states() {
    let in_vpc = "ip-10-7-1-37.us-west-2.compute.internal";
    let rows: [FirewallRow; 7] = [
        (
            "a name of the VPC's own zone, blocked with NXDOMAIN",
            Some(FirewallDecision::BlockNxDomain),
            in_vpc,
            RCODE::NameError,
            Vec::new(),
        ),
        (
            "a name of another zone, blocked with NXDOMAIN",
            Some(FirewallDecision::BlockNxDomain),
            "blocked.example.com",
            RCODE::NameError,
            Vec::new(),
        ),
        (
            "a blocked name whose rule answers NODATA",
            Some(FirewallDecision::BlockNoData),
            in_vpc,
            RCODE::NoError,
            Vec::new(),
        ),
        (
            "a blocked name whose rule answers its own record",
            Some(FirewallDecision::BlockOverride {
                domain: "safe.example.net".to_string(),
                dns_type: "CNAME".to_string(),
                ttl: 30,
            }),
            "blocked.example.com",
            RCODE::NoError,
            vec!["safe.example.net".to_string()],
        ),
        (
            "a blocked name whose rule names a record type an override may not carry",
            Some(FirewallDecision::BlockOverride {
                domain: "safe.example.net".to_string(),
                dns_type: "TXT".to_string(),
                ttl: 30,
            }),
            "blocked.example.com",
            RCODE::NoError,
            Vec::new(),
        ),
        (
            "a name a matched ALLOW rule permits",
            Some(FirewallDecision::Allow),
            in_vpc,
            RCODE::NoError,
            vec!["10.7.1.37".to_string()],
        ),
        (
            "a name the firewall decides nothing about",
            None,
            in_vpc,
            RCODE::NoError,
            vec!["10.7.1.37".to_string()],
        ),
    ];
    for (case, decided, name, code, answered) in rows {
        let engine = ::engine::InMemoryEngine::new();
        if let Some(decision) = decided {
            ::service_common::vpc_dns_firewall::VpcQueryFilters::of(&engine)
                .expect("the engine vends the register")
                .registering(::std::sync::Arc::new(OneBlockedName {
                    name: name.trim_end_matches('.'),
                    decision,
                }));
        }
        // The attachment is read AFTER the filter is recorded, as an invocation's seam is.
        let attachment = two_subnet_attachment(&engine);
        let asked = query(name, TYPE::A);
        let reply = attachment
            .resolved(&asked)
            .unwrap_or_else(|| panic!("case {case:?}: the resolver must reply"));
        let reply = Packet::parse(&reply)
            .unwrap_or_else(|error| panic!("case {case:?}: the reply must parse: {error}"));
        assert_eq!(
            reply.rcode(),
            code,
            "case {case:?}: expected {code:?} for {name}, got {:?}",
            reply.rcode()
        );
        let rendered: Vec<String> = reply
            .answers
            .iter()
            .map(|record| match &record.rdata {
                RData::A(address) => ::std::net::Ipv4Addr::from(address.address).to_string(),
                RData::CNAME(name) => name.0.to_string(),
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(
            rendered, answered,
            "case {case:?}: expected the answers {answered:?} for {name}, got {rendered:?}"
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
