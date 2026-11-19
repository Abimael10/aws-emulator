// SPDX-License-Identifier: CC0-1.0

//! The VPC a function is ATTACHED to, as the run inside it addresses that VPC: the private
//! address its traffic leaves from, and the VPC's own DNS resolver.
//!
//! "To connect a function to a VPC, Lambda creates an elastic network interface for each
//! subnet in your function's VPC configuration" ([`crate::state::vpc_interfaces`] stands those
//! interfaces up in the account's own Amazon EC2 inventory, each holding a private address out
//! of its subnet's range). That attachment is what a run addresses the VPC through, and this
//! module is what the run reads off it:
//!
//! * the SOURCE address a datagram or a connection toward an address of the VPC leaves from —
//!   the private address of the interface the route resolves through, which is the interface in
//!   the destination's own subnet when there is one and otherwise the first the function holds.
//!   An address OUTSIDE the VPC's ranges has no route through the attachment, so it is answered
//!   the way a host with no route to an address answers one;
//! * the VPC's own DNS resolver, which answers at the second address of the VPC's IPv4 range —
//!   "the Amazon DNS server … is located at the base of your VPC network range plus two"
//!   (Amazon VPC User Guide, "Amazon DNS server") — and at the LINK-LOCAL address every VPC's
//!   resolver also answers at ([`LINK_LOCAL_RESOLVER`]), reachable from any execution
//!   environment the VPC holds an interface for. It answers the queries the function sends it
//!   while the VPC's `enableDnsSupport` attribute is on: "If … enableDnsSupport is false, the
//!   Amazon Route 53 Resolver server cannot resolve … DNS hostnames"
//!   (`ModifyVpcAttributeRequest$EnableDnsSupport`).
//!
//! What that resolver answers is the VPC's OWN internal zone — the Region's private DNS domain
//! (`ec2.internal` in `us-east-1`, `<region>.compute.internal` elsewhere), whose
//! Amazon-provided hostnames name an address in the VPC. So a query for such a hostname of an
//! address in this VPC is answered with that address, a reverse query for such an address with
//! that hostname, a name of the internal zone that names no address in the VPC with
//! `NXDOMAIN`, and a name of any other zone — one the resolver would have to recurse for,
//! which reaches nothing from this emulator — with `SERVFAIL`. The DNS messages themselves are
//! read and written by the `simple-dns` crate, so what travels is a real DNS exchange rather
//! than a shape of this module's own.
//!
//! Before the resolver reads anything for a query, the DNS FIREWALL filtering that VPC decides
//! it: the rule groups that reach the VPC — its own associations, and the Route 53 Profiles
//! associated with it — are the records of the service that OWNS DNS Firewall, so the decision
//! is read out of the register that service records itself in
//! ([`::service_common::vpc_dns_firewall`]) rather than out of a copy of the rules held here. A
//! name a matched `BLOCK` rule covers is answered that rule's `BlockResponse` way instead of
//! out of the zone; an engine no such service is mounted on filters nothing.
//!
//! The attachment is read out of the EC2 inventory ONCE, when the invocation's seam is built
//! ([`attachment_of`]), which is the point the execution environment is given its place in the
//! VPC; nothing here writes to that inventory. A function attached to no VPC has no attachment
//! at all, and its datagrams reach only the in-process X-Ray daemon
//! ([`super::xray_daemon`]) — the position every function was in before this module.

use ::std::net::Ipv4Addr;

use ::ipnet::Ipv4Net;
use ::service_com_amazonaws_ec2_amazonec2::state as ec2;
use ::service_common::vpc_dns_firewall::{
    FirewallDecision, VpcQueryFilters, OVERRIDE_DNS_TYPE_CNAME,
};
use ::simple_dns::rdata::{RData, A, CNAME, PTR};
use ::simple_dns::{
    Name, Packet, PacketFlag, QCLASS, Question, ResourceRecord, CLASS, QTYPE, RCODE, TYPE,
};

/// How far into the VPC's own IPv4 range the Amazon-provided DNS server sits: "the Amazon DNS
/// server … is located at the base of your VPC network range plus two" (Amazon VPC User Guide,
/// "Amazon DNS server").
const RESOLVER_OFFSET: u32 = 2;

/// The port that server answers on, which is the port DNS is served at (`domain`, 53).
pub const RESOLVER_PORT: u16 = 53;

/// The LINK-LOCAL address the Amazon-provided DNS server of every VPC also answers at: the
/// Amazon-provided DNS server is reached either at "the 169.254.169.253 IP address, or the
/// reserved IP address at the base of the VPC network range plus two"
/// (`ModifyVpcAttributeRequest$EnableDnsSupport`'s own account of the two addresses).
///
/// A link-local address is not part of the VPC's own ranges, so the route to it is the
/// attachment's rather than one of a range it holds: an execution environment with an
/// interface in the VPC reaches it, one attached to no VPC does not.
pub const LINK_LOCAL_RESOLVER: Ipv4Addr = Ipv4Addr::new(169, 254, 169, 253);

/// How long an answer of that resolver may be held: the TTL every record below carries.
///
/// CHOSEN rather than taken from a published figure: AWS documents no TTL for the
/// Amazon-provided hostnames of a VPC. One minute is short enough that a client holds no
/// answer of this emulator past the resource it names.
const ANSWER_TTL_SECONDS: u32 = 60;

/// The prefix an Amazon-provided private hostname of an IPv4 address carries: "ip-" followed
/// by the address with its dots replaced by dashes (`ip-10-0-1-5.<region>.compute.internal`),
/// which is the `ip-name` hostname EC2 gives a private address
/// (`ec2::private_dns_name`).
const IP_NAME_PREFIX: &str = "ip-";

/// The id a lookup made THROUGH this module carries ([`VpcAttachment::addresses_of`]): one
/// query is built, handed to the resolver and read back before the next is built, so the id
/// matches no other exchange and is the same for every one of them.
const LOOKUP_QUERY_ID: u16 = 1;

/// The zone a reverse lookup of an IPv4 address is asked in (RFC 1035's `IN-ADDR.ARPA`
/// domain).
const REVERSE_ZONE: &str = "in-addr.arpa";

/// How many questions of ONE message this resolver answers.
///
/// A client asks one question per message — "the question section … usually contains exactly
/// one entry" (RFC 1035 §4.1.2) — and a datagram is bounded only by how much it can carry, so
/// this is the bound on how much work one datagram can ask for: the number of questions it may
/// multiply the firewall's decisions and the zone's lookups by. A message past it is dropped,
/// as one that carries no readable DNS message is. Well above the one question a client asks.
const MAX_QUESTIONS_PER_MESSAGE: usize = 16;

/// The response codes this resolver answers with, as RFC 1035 §4.1.1 numbers them: no error,
/// a server failure (what a resolver answers for a question it cannot have answered), and a
/// name that does not exist.
const RESPONSE_NO_ERROR: u8 = 0;
/// See [`RESPONSE_NO_ERROR`].
const RESPONSE_SERVER_FAILURE: u8 = 2;
/// See [`RESPONSE_NO_ERROR`].
const RESPONSE_NAME_ERROR: u8 = 3;

/// ONE interface of the attachment: the private address EC2 handed it, and the range of the
/// subnet it sits in.
#[derive(Debug, Clone, Copy)]
struct AttachedInterface {
    /// `NetworkInterface$PrivateIpAddress` of the interface.
    address: Ipv4Addr,
    /// `Subnet$CidrBlock` of the subnet it was placed in, which is what decides whether a
    /// destination is reached through this interface rather than another.
    subnet: Ipv4Net,
}

/// The VPC attachment one invocation runs with: where its traffic leaves from, and which
/// addresses it reaches.
#[derive(Debug, Clone)]
pub struct VpcAttachment {
    /// The interfaces the function holds, in the order of the subnets they sit in.
    interfaces: Vec<AttachedInterface>,
    /// Every IPv4 range of the VPC those interfaces sit in — the address space a route
    /// through them reaches.
    ranges: Vec<Ipv4Net>,
    /// Whether the VPC's `enableDnsSupport` attribute is on, which decides whether its DNS
    /// server answers at all.
    dns_support: bool,
    /// The Region the function runs in, whose private DNS domain is the suffix of every
    /// Amazon-provided hostname the resolver answers for.
    region: String,
    /// The VPC those interfaces sit in, which is what the DNS Firewall filtering the run's
    /// queries is keyed by.
    vpc_id: String,
    /// Where a query's firewall decision is read from, and `None` for an engine that vends no
    /// such register — then nothing filters the run's queries.
    filters: Option<VpcQueryFilters>,
}

/// The attachment of the function whose interfaces are `interface_ids`, read out of the
/// account's own EC2 inventory.
///
/// `None` for a function attached to no VPC — one whose configuration named no subnet, so
/// Lambda stood no interface up for it — and for one whose interfaces that inventory no longer
/// holds, which is a function with nothing to reach a VPC through either way. Read-only: the
/// inventory is the EC2 service's, and this only reads the records that service wrote.
#[must_use]
pub fn attachment_of<Eng: Send + Sync + 'static>(
    engine: &Eng,
    region: &str,
    interface_ids: &[String],
) -> Option<VpcAttachment> {
    if interface_ids.is_empty() {
        return None;
    }
    let state = ec2::service_state(engine)?;
    let store = state.lock();
    let mut interfaces: Vec<AttachedInterface> = Vec::with_capacity(interface_ids.len());
    let mut held_vpc: Option<String> = None;
    for interface_id in interface_ids {
        let Some(interface) = store.network_interfaces.get(interface_id.as_str()) else {
            continue;
        };
        let address = interface
            .private_ip_address
            .as_deref()
            .and_then(|address| address.parse::<Ipv4Addr>().ok());
        let subnet = interface
            .subnet_id
            .as_deref()
            .and_then(|subnet_id| store.subnets.get(subnet_id))
            .and_then(|subnet| subnet.cidr_block.as_deref())
            .and_then(|cidr| cidr.parse::<Ipv4Net>().ok());
        if let (Some(address), Some(subnet)) = (address, subnet) {
            interfaces.push(AttachedInterface { address, subnet });
            if held_vpc.is_none() {
                held_vpc = interface.vpc_id.clone();
            }
        }
    }
    let vpc_id = held_vpc?;
    if interfaces.is_empty() {
        return None;
    }
    // The VPC's own ranges: the block it was created with and every block later associated
    // with it, as EC2's record of the VPC reports them.
    let vpc = store.vpcs.get(vpc_id.as_str())?;
    let mut ranges: Vec<Ipv4Net> = vpc
        .cidr_block
        .as_deref()
        .into_iter()
        .chain(
            vpc.cidr_block_association_set
                .iter()
                .flatten()
                .filter_map(|association| association.cidr_block.as_deref()),
        )
        .filter_map(|cidr| cidr.parse::<Ipv4Net>().ok())
        .collect();
    ranges.sort_by_key(|range| (u32::from(range.network()), range.prefix_len()));
    ranges.dedup();
    if ranges.is_empty() {
        return None;
    }
    let dns_support = store
        .vpc_attributes
        .get(vpc_id.as_str())
        .is_some_and(|attributes| attributes.enable_dns_support);
    // The EC2 inventory is done with: nothing below reads it.
    drop(store);
    Some(VpcAttachment {
        interfaces,
        ranges,
        dns_support,
        region: region.to_string(),
        filters: VpcQueryFilters::of(engine),
        vpc_id,
    })
}

impl VpcAttachment {
    /// The private address a datagram or a connection toward `destination` leaves from, and
    /// `None` for a destination the attachment has no route to.
    ///
    /// The route resolves through the interface in the destination's OWN subnet when the
    /// function holds one there, and otherwise through the first interface it holds — which is
    /// how an address elsewhere in the VPC is reached, through the VPC's own router, and how
    /// the link-local address of the VPC's resolver is reached, which belongs to no subnet.
    #[must_use]
    pub fn source_toward(&self, destination: Ipv4Addr) -> Option<Ipv4Addr> {
        if !self.holds(destination) && destination != LINK_LOCAL_RESOLVER {
            return None;
        }
        self.interfaces
            .iter()
            .find(|interface| interface.subnet.contains(&destination))
            .or_else(|| self.interfaces.first())
            .map(|interface| interface.address)
    }

    /// The private address the function's traffic carries INSIDE the VPC it is attached to:
    /// the address of the first interface Lambda stood up for it, which is the one a
    /// destination elsewhere in the VPC is reached through.
    ///
    /// What a plane of that VPC states its client as ([`super::guest_socket`]), where the
    /// transport that carried the connection has an address of its own.
    #[must_use]
    pub fn private_address(&self) -> Option<Ipv4Addr> {
        self.interfaces.first().map(|interface| interface.address)
    }

    /// Whether `destination` is an address of the VPC this function is attached to.
    #[must_use]
    pub fn holds(&self, destination: Ipv4Addr) -> bool {
        self.ranges.iter().any(|range| range.contains(&destination))
    }

    /// The address of the VPC's own DNS server: the second address of its first IPv4 range.
    #[must_use]
    pub fn resolver(&self) -> Option<Ipv4Addr> {
        let range = self.ranges.first()?;
        let address = Ipv4Addr::from(u32::from(range.network()).checked_add(RESOLVER_OFFSET)?);
        range.contains(&address).then_some(address)
    }

    /// Whether `destination`:`port` is where that server answers: either address it is reached
    /// at, on the DNS port.
    #[must_use]
    pub fn is_resolver(&self, destination: Ipv4Addr, port: u16) -> bool {
        port == RESOLVER_PORT
            && (destination == LINK_LOCAL_RESOLVER || self.resolver() == Some(destination))
    }

    /// The reply the VPC's DNS server answers `query` with, or `None` when it answers nothing
    /// at all: a VPC whose `enableDnsSupport` is off has no resolver to reply with, a datagram
    /// that carries no readable DNS message is one the server drops, and so is one asking more
    /// questions than [`MAX_QUESTIONS_PER_MESSAGE`].
    #[must_use]
    pub fn resolved(&self, query: &[u8]) -> Option<Vec<u8>> {
        if !self.dns_support {
            return None;
        }
        let asked = Packet::parse(query).ok()?;
        if asked.questions.len() > MAX_QUESTIONS_PER_MESSAGE {
            return None;
        }
        let mut reply = Packet::new_reply(asked.id());
        if asked.has_flags(PacketFlag::RECURSION_DESIRED) {
            reply.set_flags(PacketFlag::RECURSION_DESIRED);
        }
        // The server is a resolver: it answers for the zone it holds and recurses for the
        // rest.
        reply.set_flags(PacketFlag::RECURSION_AVAILABLE);
        // What the firewall decides about this message's questions, read ONCE for the whole
        // message however many questions it carries.
        let mut filtered = self.filtered(&asked.questions);
        let mut code = RESPONSE_NO_ERROR;
        for question in &asked.questions {
            reply.questions.push(question.clone());
            // The firewall decides a query before the resolver reads anything for it, so a
            // blocked name is answered the rule's way whatever the zone holds.
            match filtered.next().flatten() {
                Some(Filtered::NxDomain) => {
                    code = RESPONSE_NAME_ERROR;
                    continue;
                }
                Some(Filtered::NoData) => continue,
                Some(Filtered::Record(record)) => {
                    reply.answers.push(*record);
                    continue;
                }
                None => {}
            }
            match self.answer_for(&question.qname, question.qtype) {
                Answer::Found(record) => {
                    // The zone is the VPC's own, so the answer is authoritative.
                    reply.set_flags(PacketFlag::AUTHORITATIVE_ANSWER);
                    reply.answers.push(*record);
                }
                Answer::Empty => {
                    reply.set_flags(PacketFlag::AUTHORITATIVE_ANSWER);
                }
                Answer::Absent => {
                    reply.set_flags(PacketFlag::AUTHORITATIVE_ANSWER);
                    code = RESPONSE_NAME_ERROR;
                }
                Answer::Elsewhere => {
                    code = RESPONSE_SERVER_FAILURE;
                }
            }
        }
        let mut bytes = reply.build_bytes_vec().ok()?;
        // The RESPONSE CODE is the low four bits of the second flag byte of the message
        // header (RFC 1035 §4.1.1). The crate writes every other field of the reply and reads
        // this one back (`Packet::rcode`), but exposes no setter for it, so it is written here
        // over the header the crate itself just built.
        let flags = bytes.get_mut(3)?;
        *flags = (*flags & 0xF0) | (code & 0x0F);
        Some(bytes)
    }

    /// What the DNS Firewall of this VPC answers each of `questions` with, in the order they
    /// were asked: `None` for a question it lets through — one no rule matches, and one a
    /// matched `ALLOW` or `ALERT` rule permits.
    ///
    /// The rules are read ONCE for the whole message ([`VpcQueryFilters::decisions`]), so a
    /// message of several questions costs one reading of them and not one per question.
    fn filtered(&self, questions: &[Question<'_>]) -> ::std::vec::IntoIter<Option<Filtered>> {
        let Some(filters) = self.filters.as_ref() else {
            return Vec::new().into_iter();
        };
        let asked: Vec<(String, Option<u16>)> = questions
            .iter()
            .map(|question| {
                (
                    question.qname.to_string(),
                    Some(u16::from(question.qtype)),
                )
            })
            .collect();
        filters
            .decisions(&self.vpc_id, &asked)
            .into_iter()
            .zip(questions)
            .map(|(decided, question)| {
                decided.and_then(|decision| filtered_answer(&question.qname, decision))
            })
            .collect::<Vec<Option<Filtered>>>()
            .into_iter()
    }

    /// The IPv4 addresses the VPC's own DNS server answers for `name`, ASKED OF IT the way a
    /// client asks: one `A` question, carried to [`Self::resolved`] as a real DNS message and
    /// read back off the reply that server answered with.
    ///
    /// Empty for a name it holds no address under — a name of its own zone that names no
    /// address in the VPC, a name of another zone it would have to recurse for, a name the
    /// VPC's DNS Firewall blocks, and every name at all while the VPC's `enableDnsSupport` is
    /// off, which leaves it with no server to answer. So a name service that asks this
    /// ([`super::guest_resolver`]) is answered by the same server, and with the same records, as
    /// a handler that sends the query itself.
    #[must_use]
    pub fn addresses_of(&self, name: &str) -> Vec<Ipv4Addr> {
        let Ok(asked) = Name::new(name) else {
            return Vec::new();
        };
        let mut query = Packet::new_query(LOOKUP_QUERY_ID);
        query.set_flags(PacketFlag::RECURSION_DESIRED);
        query.questions.push(Question::new(
            asked.into_owned(),
            QTYPE::TYPE(TYPE::A),
            QCLASS::CLASS(CLASS::IN),
            false,
        ));
        let Ok(bytes) = query.build_bytes_vec() else {
            return Vec::new();
        };
        let Some(reply) = self.resolved(&bytes) else {
            return Vec::new();
        };
        let Ok(answered) = Packet::parse(&reply) else {
            return Vec::new();
        };
        if answered.rcode() != RCODE::NoError {
            return Vec::new();
        }
        answered
            .answers
            .iter()
            .filter_map(|record| match &record.rdata {
                RData::A(A { address }) => Some(Ipv4Addr::from(*address)),
                _other => None,
            })
            .collect()
    }

    /// What the resolver holds for ONE question.
    fn answer_for(&self, asked: &Name<'_>, kind: QTYPE) -> Answer {
        let name = asked.to_string().trim_end_matches('.').to_ascii_lowercase();
        let internal = ec2::private_dns_domain(&self.region).to_ascii_lowercase();
        if let Some(labels) = name.strip_suffix(&format!(".{REVERSE_ZONE}")) {
            return self.reverse_answer(asked, labels, kind, &internal);
        }
        let Some(host) = name.strip_suffix(&format!(".{internal}")) else {
            // A name of another zone: the resolver would have to recurse for it, and nothing
            // of this emulator answers out there.
            return Answer::Elsewhere;
        };
        let Some(address) = address_in_name(host).filter(|address| self.holds(*address)) else {
            return Answer::Absent;
        };
        if kind != QTYPE::TYPE(TYPE::A) {
            // The name exists, so a question about a record type the internal zone holds none
            // of is answered empty rather than as a name that is not there.
            return Answer::Empty;
        }
        Answer::Found(Box::new(ResourceRecord::new(
            asked.clone().into_owned(),
            CLASS::IN,
            ANSWER_TTL_SECONDS,
            RData::A(A {
                address: u32::from(address),
            }),
        )))
    }

    /// What the resolver holds for a REVERSE question: `labels` is what was asked under the
    /// `in-addr.arpa` zone, in that zone's reversed order.
    fn reverse_answer(
        &self,
        asked: &Name<'_>,
        labels: &str,
        kind: QTYPE,
        internal: &str,
    ) -> Answer {
        let Some(address) = reversed_address(labels).filter(|address| self.holds(*address)) else {
            return Answer::Absent;
        };
        if kind != QTYPE::TYPE(TYPE::PTR) {
            return Answer::Empty;
        }
        let hostname = ec2::private_dns_name(&address.to_string(), &self.region);
        // Built from the Region's own private domain, so a hostname that does not end in it is
        // not one this zone answers.
        if !hostname.to_ascii_lowercase().ends_with(internal) {
            return Answer::Absent;
        }
        match Name::new(&hostname) {
            Ok(answered) => Answer::Found(Box::new(ResourceRecord::new(
                asked.clone().into_owned(),
                CLASS::IN,
                ANSWER_TTL_SECONDS,
                RData::PTR(PTR(answered.into_owned())),
            ))),
            Err(_unnamed) => Answer::Absent,
        }
    }
}

/// What one question came to in the DNS Firewall of the VPC.
enum Filtered {
    /// `BLOCK` with `BlockResponse` `NXDOMAIN`: the name is answered as one that does not
    /// exist, with no record.
    NxDomain,
    /// `BLOCK` with `BlockResponse` `NODATA`: a successful answer carrying no record.
    NoData,
    /// `BLOCK` with `BlockResponse` `OVERRIDE`: the rule's own record answers the question.
    Record(Box<ResourceRecord<'static>>),
}

/// How one question is answered once the firewall has decided it, or `None` for a decision
/// that lets the query through to the zone.
fn filtered_answer(asked: &Name<'_>, decision: FirewallDecision) -> Option<Filtered> {
    match decision {
        FirewallDecision::Allow => None,
        FirewallDecision::BlockNxDomain => Some(Filtered::NxDomain),
        FirewallDecision::BlockNoData => Some(Filtered::NoData),
        FirewallDecision::BlockOverride {
            domain,
            dns_type,
            ttl,
        } => {
            // The override answers with the rule's OWN record: the one type such a rule may
            // name is a `CNAME`, so a rule naming another carries no record this resolver can
            // write, and the query is answered as the block with no record it is.
            if !dns_type.eq_ignore_ascii_case(OVERRIDE_DNS_TYPE_CNAME) {
                return Some(Filtered::NoData);
            }
            let Ok(target) = Name::new(&domain) else {
                return Some(Filtered::NoData);
            };
            Some(Filtered::Record(Box::new(ResourceRecord::new(
                asked.clone().into_owned(),
                CLASS::IN,
                ttl,
                RData::CNAME(CNAME(target.into_owned())),
            ))))
        }
    }
}

/// What one question came to in the resolver's own zone: a record, a name of its own zone with
/// no record of the type asked about, a name of that zone it holds nothing under, or a name of
/// another zone.
enum Answer {
    /// The record itself.
    Found(Box<ResourceRecord<'static>>),
    /// The name exists in the zone, but carries no record of the type asked about.
    Empty,
    /// The zone is the resolver's and holds no such name.
    Absent,
    /// The name belongs to another zone, which this resolver would have to recurse for.
    Elsewhere,
}

/// The address an Amazon-provided `ip-name` hostname names: `ip-10-0-1-5` is `10.0.1.5`.
///
/// `None` for a label of any other form, which names no address at all.
fn address_in_name(host: &str) -> Option<Ipv4Addr> {
    if host.contains('.') {
        return None;
    }
    host.strip_prefix(IP_NAME_PREFIX)?
        .replace('-', ".")
        .parse::<Ipv4Addr>()
        .ok()
}

/// The address a reverse question names: the four labels of `in-addr.arpa`, in the reverse of
/// the address's own order (`5.1.0.10` is `10.0.1.5`).
fn reversed_address(labels: &str) -> Option<Ipv4Addr> {
    let mut octets: Vec<&str> = labels.split('.').collect();
    if octets.len() != 4 {
        return None;
    }
    octets.reverse();
    octets.join(".").parse::<Ipv4Addr>().ok()
}

#[cfg(test)]
pub(crate) mod tests;
