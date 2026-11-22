// SPDX-License-Identifier: CC0-1.0

//! The DNS DATA PLANE a query asked from INSIDE a VPC reaches: which addresses answer one
//! there, whether the query is admitted to them, and what the answer is.
//!
//! # What answers a query inside a VPC
//!
//! A client in a VPC addresses the resolver at the link-local
//! [`VPC_RESOLVER_LINK_LOCAL_OCTETS`], or [`VPC_RESOLVER_OFFSET`] past the base of the VPC's
//! own address range ("the Amazon DNS server … the base of the VPC network range plus two").
//! An INBOUND Resolver endpoint adds addresses of its own, in the subnets the endpoint was
//! created in: "inbound endpoints … forward DNS queries from your network to Route 53
//! Resolver", so a query arriving at one of its addresses is answered by the same Resolver.
//! An outbound endpoint takes no query: it is where the Resolver sends one.
//!
//! A VPC whose DNS SUPPORT is turned off has no such resolver at all: that attribute is what
//! decides whether the Amazon-provided DNS server resolves queries in the VPC
//! (`ModifyVpcAttribute`), so neither of those two addresses answers in it, and an endpoint's
//! own addresses are what is left.
//!
//! # What gates a query
//!
//! An endpoint's addresses are addresses of ELASTIC NETWORK INTERFACES, and an interface is
//! behind the security groups the endpoint was created with
//! (`CreateResolverEndpointRequest$SecurityGroupIds`: "the security group … must include one
//! or more inbound rules … for DNS"). So a query reaching an endpoint address is delivered
//! only where one of those groups admits its protocol and its port from the querying address
//! ([`ec2::ComAmazonawsEc2Amazonec2State::security_groups_admit_ingress`], which is Amazon
//! EC2's own decision rather than a second copy of it here). A query no rule admits is
//! DROPPED — a security group has no deny rule, so nothing comes back at all and the client
//! waits — which is the one thing that makes revoking a rule observable from inside the VPC.
//! The VPC's own resolver has no interface and no security group: it answers every client of
//! the VPC.
//!
//! # What the answer is
//!
//! The record sets of the PRIVATE hosted zones associated with that VPC, read through Route
//! 53's own store: a query is answered out of the zone whose name is the longest suffix of
//! the name asked for ("Route 53 … uses the most specific zone"), authoritatively, since the
//! Resolver is the authority for a private zone. A name that zone holds nothing at is
//! [`ResponseCode::NxDomain`], a name it holds other types at is answered with no records, and
//! a name no name of the zone stands at is still answered by a WILDCARD record set of the zone
//! that encloses it, which is what such a record set is for ([`set_for`]).
//!
//! A name under NO zone of the VPC is one the Resolver recurses for — which leaves this process,
//! and nothing here does that — so what it is answered with is the only part of the public
//! namespace this emulator can read: a name a DNS STANDARD reserves for documentation is answered
//! out of the one reading of those names this tree holds
//! (`service_common::public_dns_standard`), a name the public namespace establishes does not
//! exist ([`::service_common::vpc_private_dns::established_not_to_exist`]) is
//! [`ResponseCode::NxDomain`], and every other one is a query this resolver could not answer
//! ([`ResponseCode::ServFail`]).
//!
//! # What is recorded about a query
//!
//! A query this plane answered is RECORDED in each of the account's Resolver query logging
//! configurations associated with the VPC ([`super::query_log_delivery`]) — the configurations,
//! their destinations and the form of a record are this service's own, so the component carrying
//! a query hands the query over and renders nothing itself.
//!
//! # A VIEW, not a live resolver
//!
//! Everything a query needs is read ONCE, into [`VpcDnsView`] — the instance the query comes
//! from, the addresses that answer, what the security groups admit, and the zones. The view
//! is taken by the component that reaches into the guest, before it runs anything there, so
//! answering a query holds no store guard at all; it is one instant's reading of the three
//! stores that own those facts, which is what one command's run is.

use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::time::Instant;

use ::service_com_amazonaws_cloudwatchlogs_logs_20140328::state as logs;
use ::service_com_amazonaws_route53_awsdnsv20130401::ops::types::shared_types::ResourceRecordSet;
use ::service_com_amazonaws_route53_awsdnsv20130401::state as route53;

use ::service_common::vpc_dns_firewall::FirewallDecision;
use ::service_common::vpc_private_dns::{
    established_not_to_exist, InVpcQuerySource, LoggedAnswer, LoggedQuery, CLASS_INTERNET,
    SOURCE_ID_INSTANCE,
};

use super::dns_firewall::{qtype_number, FirewallView};
use super::{ec2, ENDPOINT_STATUS_OPERATIONAL, IP_STATUS_ATTACHED};

/// The link-local address every VPC's own Resolver answers at: "the Route 53 Resolver …
/// 169.254.169.253" (Amazon VPC DNS). Held as its four octets because nothing here DIALS it —
/// this emulator serves it to its own guests — and the one use it has is comparing the address
/// a client asked.
pub const VPC_RESOLVER_LINK_LOCAL_OCTETS: [u8; 4] = [169, 254, 169, 253];

/// How far past the base of a VPC's address range that same resolver also answers: "the IP
/// address of the DNS server is the base of the VPC network range plus two" (Amazon VPC DNS).
pub const VPC_RESOLVER_OFFSET: u32 = 2;

/// The port DNS is served at (IANA: `domain`, 53 over both UDP and TCP), which is the port a
/// security group on a Resolver endpoint has to admit for a query to reach it.
pub const DNS_PORT: u16 = 53;

/// The protocol keyword of a datagram query, as a security-group rule states one.
pub const PROTOCOL_UDP: &str = "udp";

/// The protocol keyword of a query carried on a connection, as a rule states one.
pub const PROTOCOL_TCP: &str = "tcp";

/// The `Direction` of an endpoint that TAKES queries: "inbound (resolve DNS queries sent to
/// your network from your VPC)" is what answers at an address of the VPC.
pub const DIRECTION_INBOUND: &str = "INBOUND";

/// How many private hosted zones of one VPC a view carries, and how many record sets of each.
///
/// The view is one reading of stores a client fills, so what it can hold is bounded here
/// rather than by how many zones the account created: a query is answered out of the most
/// specific zone, and these are far above any VPC's own DNS.
const MAX_VIEW_ZONES: usize = 64;

/// See [`MAX_VIEW_ZONES`].
const MAX_VIEW_RECORDS_PER_ZONE: usize = 512;

/// How many VALUES of one record set the view carries, and so how many records one name can be
/// answered with: the other factor in what one query costs, bounded for the same reason
/// ([`MAX_VIEW_ZONES`]) — a record set is written by a caller, so the number of values in one is
/// caller-grown too. Well above the eight a multivalue answer carries.
const MAX_VIEW_VALUES_PER_RECORD_SET: usize = 32;

/// How many `CNAME` records one query follows before the answer is given up on: "if the
/// record is a CNAME, Resolver … follows the chain", and a chain this long is a loop rather
/// than a delegation.
const MAX_CNAME_HOPS: usize = 8;

/// The owner label of a WILDCARD record set, as this store canonicalises one: a record set
/// whose name's leftmost label is `*` answers every name under its parent that no closer name of
/// the zone answers (RFC 1034 §4.3.3), and Route 53 writes that label as its octal escape code
/// (`route53::canonical_dns_name`).
const WILDCARD_LABEL: &str = "\\052.";

/// The most octets a NAME a query asks about may carry: "the total length of a domain name (i.e.,
/// label octets and label length octets) is restricted to 255 octets or less" (RFC 1035 §2.3.4).
///
/// A value past it is no domain name, so no zone of the account can hold it and nothing here
/// resolves it ([`a_resolvable_name`]) — which is also what keeps the work of one query bounded
/// by this limit rather than by the length of the string a client sent.
const MAX_NAME_OCTETS: usize = 255;

/// The most octets ONE label of such a name may carry: "labels must be 63 characters or less"
/// (RFC 1035 §2.3.4). See [`MAX_NAME_OCTETS`].
const MAX_LABEL_OCTETS: usize = 63;

/// The `A` record type, which is the one a client asking for an address asks for.
pub const TYPE_A: &str = "A";

/// The `AAAA` record type.
pub const TYPE_AAAA: &str = "AAAA";

/// The `CNAME` record type, whose value is another NAME: an answer carries the alias and then
/// what the alias resolves to.
pub const TYPE_CNAME: &str = "CNAME";

/// What one query came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Nothing of this VPC answers DNS at that address, so the query reaches no resolver.
    NoServer,
    /// The address IS one a resolver of this VPC answers at, but the security groups on its
    /// interface admit no such query from this client: the packet is dropped, so the client is
    /// answered with nothing at all.
    Dropped,
    /// A resolver answered.
    Answered(Answer),
}

/// What a resolver answered: the status, whether it is the authority for the name, and the
/// records of the answer section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    /// The response code the header carries.
    pub code: ResponseCode,
    /// Whether the answer is authoritative, which it is for a private hosted zone of the VPC.
    pub authoritative: bool,
    /// The answer section, in the order a resolver puts it: an alias before what it resolves
    /// to.
    pub records: Vec<AnsweredRecord>,
}

/// One record of an answer section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnsweredRecord {
    /// The owner name, in the canonical fully qualified form Route 53 holds it in.
    pub name: String,
    /// The record set's TTL, in seconds.
    pub ttl: i64,
    /// The record type (`A`, `CNAME`, …).
    pub record_type: String,
    /// One record's value, as the record set holds it.
    pub value: String,
}

/// The response codes a query of this data plane is answered with (RFC 1035 header `RCODE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseCode {
    /// The name exists: the answer section holds what was asked for, or nothing when the name
    /// carries no record of that type.
    NoError,
    /// No such name in the zone the query was answered out of.
    NxDomain,
    /// The resolver has no answer for the name: it is under no hosted zone this VPC is
    /// associated with, and nothing of this emulator resolves a name outside the account's own
    /// records.
    ServFail,
}

impl ResponseCode {
    /// The word a resolver client prints for this code, which is the mnemonic the header's
    /// value is published under.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoError => "NOERROR",
            Self::NxDomain => "NXDOMAIN",
            Self::ServFail => "SERVFAIL",
        }
    }
}

/// One instant's reading of what DNS a client inside a VPC can reach, and of what it answers.
#[derive(Debug, Clone)]
pub struct VpcDnsView {
    /// The VPC the client is in, which is what decides both halves: which addresses answer,
    /// and which zones they answer out of.
    pub vpc_id: String,
    /// The client's own address, which is the SOURCE a security group admits or does not.
    pub source_ip: String,
    /// The address the guest's own resolver configuration names, which is the one a client
    /// that names no server queries: the VPC's own resolver, which is what the VPC's DHCP
    /// option set publishes ("`domain-name-servers` … `AmazonProvidedDNS`").
    nameserver: Ipv4Addr,
    /// The addresses of this VPC that answer DNS, with what each admits.
    servers: Vec<DnsServer>,
    /// The private hosted zones associated with the VPC, most specific first.
    zones: Vec<ViewZone>,
    /// The DNS Firewall filtering this VPC's queries, which decides a query BEFORE any zone
    /// is read for it ([`super::dns_firewall`]).
    firewall: FirewallView,
    /// The security groups the client is a member of, by id — carried so a query answered out of
    /// this view is RECORDED against the same client every other reading of it is made for.
    groups: Vec<String>,
    /// Where a query this view answers is recorded ([`super::query_log_delivery`]), and `None`
    /// for a view built for a test of the answer paths alone.
    recorder: Option<StoredVpcDns>,
    /// The Amazon EC2 instance the client runs on, where the view was taken for one — which is
    /// how a record of its queries names their source.
    instance_id: Option<String>,
}

/// One address of the VPC that answers DNS.
#[derive(Debug, Clone)]
struct DnsServer {
    /// The address itself.
    address: Ipv4Addr,
    /// Whether a datagram query from the client is admitted to it.
    admits_udp: bool,
    /// Whether a query on a connection from the client is admitted to it.
    admits_tcp: bool,
}

/// One zone of the view, with the record sets it answers out of.
#[derive(Debug, Clone)]
struct ViewZone {
    /// The zone's own name, canonical and fully qualified.
    name: String,
    /// Its record sets, by canonical owner name and then record type — the order a lookup
    /// reads them in.
    records: BTreeMap<(String, String), ViewRecordSet>,
}

/// One record set of a zone, as a query is answered out of it.
#[derive(Debug, Clone)]
struct ViewRecordSet {
    /// The TTL the record set carries, or the zone's own default where it carries none (an
    /// alias record set has no TTL of its own).
    ttl: i64,
    /// The values of the records, in the order the record set holds them.
    values: Vec<String>,
    /// Whether this is an ALIAS record set whose target names another NAME rather than an
    /// address: Route 53 answers such a record set with "one value from the referenced resource
    /// record set" rather than with the name itself, so the name in [`Self::values`] is a name
    /// to resolve on, the way an alias is followed.
    alias_to_name: bool,
}

/// The TTL a record set that carries none is answered with: Route 53 answers an ALIAS record
/// with the TTL of the resource it points at, and this emulator holds no such resource's own
/// TTL, so an alias is answered with the one minute Route 53 documents for an alias to a
/// resource whose TTL it cannot read ("Route 53 uses the TTL of … 60 seconds").
const DEFAULT_TTL: i64 = 60;

impl VpcDnsView {
    /// What this VPC's DNS answers a query for `name` of `record_type`, sent to `server` over
    /// `protocol`.
    ///
    /// `server` is the address the client addressed, as it wrote it; a spelling that is not an
    /// address of this VPC's DNS reaches no resolver.
    #[must_use]
    pub fn answer(
        &self,
        server: &str,
        name: &str,
        record_type: &str,
        protocol: &str,
    ) -> Resolution {
        let Ok(address) = server.trim().parse::<Ipv4Addr>() else {
            return Resolution::NoServer;
        };
        match self.admitted(address, protocol) {
            None => Resolution::NoServer,
            Some(false) => Resolution::Dropped,
            Some(true) => {
                let answer = self.resolved(name, record_type);
                self.recorded(name, record_type, protocol, &answer);
                Resolution::Answered(answer)
            }
        }
    }

    /// Record a query this view ANSWERED in the query logging configurations of its VPC, which is
    /// the owning slice's own write ([`super::query_log_delivery`]).
    ///
    /// A query that reached no resolver, and one no security group admitted, is not a query the
    /// Resolver answered, so neither is recorded.
    fn recorded(&self, name: &str, record_type: &str, protocol: &str, answer: &Answer) {
        let Some(recorder) = self.recorder.as_ref() else {
            return;
        };
        let answers: Vec<LoggedAnswer> = answer
            .records
            .iter()
            .map(|record| LoggedAnswer {
                rdata: record.value.clone(),
                record_type: record.record_type.clone(),
                class: CLASS_INTERNET.to_string(),
            })
            .collect();
        let source_ids: Vec<(&str, &str)> = self
            .instance_id
            .as_deref()
            .map(|instance_id| (SOURCE_ID_INSTANCE, instance_id))
            .into_iter()
            .collect();
        recorder.record_query(
            &InVpcQuerySource {
                vpc_id: &self.vpc_id,
                source_ip: &self.source_ip,
                security_group_ids: &self.groups,
            },
            &LoggedQuery {
                name,
                record_type,
                class: CLASS_INTERNET,
                response_code: answer.code.as_str(),
                answers: &answers,
                transport: protocol,
                source_ids: &source_ids,
            },
        );
    }

    /// Whether an address of this VPC answers DNS at `server` and, if it does, whether a query
    /// carried over `protocol` from this view's own client is ADMITTED to it — `None` for an
    /// address nothing of the VPC answers DNS at.
    ///
    /// The VPC's own resolver is behind no interface of the account, so it admits every client
    /// of the VPC; an endpoint's address is behind the endpoint's security groups, and a query
    /// no rule of them admits is dropped rather than refused.
    #[must_use]
    pub fn admitted(&self, server: Ipv4Addr, protocol: &str) -> Option<bool> {
        let held = self.servers.iter().find(|held| held.address == server)?;
        Some(if protocol.eq_ignore_ascii_case(PROTOCOL_TCP) {
            held.admits_tcp
        } else {
            held.admits_udp
        })
    }

    /// The address a client of this guest queries when it names no server itself: the one the
    /// guest's resolver configuration holds.
    #[must_use]
    pub fn nameserver(&self) -> String {
        self.nameserver.to_string()
    }

    /// The answer itself: what the DNS Firewall decides about the name, and otherwise the zone
    /// the name falls in and what that zone holds for it.
    ///
    /// A name NO zone of this VPC holds is one the resolver recurses for, and what a recursion
    /// comes to here is the only part of the public namespace this emulator can read: a name
    /// that namespace establishes does not exist
    /// ([`::service_common::vpc_private_dns::established_not_to_exist`], the one reading of that
    /// question every in-VPC resolver takes) is answered as one that does not, and every other
    /// name is one this resolver could not answer ([`ResponseCode::ServFail`]).
    fn resolved(&self, name: &str, record_type: &str) -> Answer {
        let asked = route53::canonical_dns_name(name);
        let record_type = record_type.trim().to_ascii_uppercase();
        // A value that is no DOMAIN NAME is a query nothing can answer: no rule of the firewall
        // covers it, no zone holds a record set at it, and nothing recursed for it would be
        // answered either ([`a_resolvable_name`]). Answered before anything walks it, so what one
        // query costs is bounded by the name's own published limits.
        if !a_resolvable_name(&asked) {
            return Answer {
                code: ResponseCode::ServFail,
                authoritative: false,
                records: Vec::new(),
            };
        }
        // The firewall decides a query before anything resolves the name: a blocked name is
        // answered the rule's way whether or not any zone of this VPC holds it.
        if let Some(blocked) = self.blocked(&asked, &record_type) {
            return blocked;
        }
        self.zone_answer(&asked, &record_type)
            .or_else(|| standard_public_answer(&asked, &record_type))
            .unwrap_or_else(|| Answer {
                code: if established_not_to_exist(&asked) {
                    ResponseCode::NxDomain
                } else {
                    ResponseCode::ServFail
                },
                authoritative: false,
                records: Vec::new(),
            })
    }

    /// What the PRIVATE HOSTED ZONES of this VPC hold for the canonical name `asked` of
    /// `record_type`, with no firewall decision applied — `None` when no zone of the VPC holds
    /// the name, which is a question those zones do not answer at all rather than one they
    /// answer with no record.
    ///
    /// Asked on its own by a component that has already read the firewall's decision for the
    /// query it is carrying ([`::service_common::vpc_private_dns`]), so the rules are read once
    /// per query however many planes the query passes through.
    #[must_use]
    pub fn zone_answer(&self, asked: &str, record_type: &str) -> Option<Answer> {
        answer_from_zones(&self.zones, asked, record_type)
    }
}

/// [`VpcDnsView::zone_answer`] over the zones alone, so a caller holding nothing but a reading of
/// the zones ([`StoredVpcDns::private_zone_answer`]) is answered by the same lookup.
fn answer_from_zones(zones: &[ViewZone], asked: &str, record_type: &str) -> Option<Answer> {
    {
        let asked = route53::canonical_dns_name(asked);
        // A value that is no DOMAIN NAME is one no zone of the account can hold a record set at,
        // so nothing here resolves it — and the work of one query is bounded by the name's own
        // published limits rather than by the length of the string a client sent
        // ([`a_resolvable_name`]).
        if !a_resolvable_name(&asked) {
            return None;
        }
        let record_type = record_type.trim().to_ascii_uppercase();
        let zone = zone_for(zones, &asked)?;
        let mut records: Vec<AnsweredRecord> = Vec::new();
        // The name being resolved, and the name the answer is reported UNDER. They part company
        // at an ALIAS: an alias is answered with the referenced record set's data under the
        // ALIAS's own name — which is what makes an alias invisible to the client — while a
        // `CNAME` is itself a record of the answer, and what follows it is reported under the
        // name it pointed at.
        let mut owner = asked.clone();
        let mut reported_under = asked;
        for _hop in 0..MAX_CNAME_HOPS {
            if let Some(set) = set_for(zone, &owner, &record_type) {
                // An ALIAS to another NAME answers with what that name's own record set holds,
                // not with the name: "one value from the referenced resource record set"
                // (`RecordDataEntry`).
                if set.alias_to_name {
                    let Some(target) = set
                        .values
                        .first()
                        .map(|target| route53::canonical_dns_name(target))
                    else {
                        break;
                    };
                    if target == owner {
                        break;
                    }
                    owner = target;
                    continue;
                }
                records.extend(answered(&reported_under, &record_type, set));
                return Some(Answer {
                    code: ResponseCode::NoError,
                    authoritative: true,
                    records,
                });
            }
            // "If … a CNAME record, Resolver returns the CNAME and then resolves the name it
            // points at" — which is why a query for an address is answered through an alias.
            let Some(alias) =
                set_for(zone, &owner, TYPE_CNAME).filter(|_| record_type != TYPE_CNAME)
            else {
                return Some(Answer {
                    // A name the zone holds some other type at exists: the answer is the one a
                    // resolver gives for a type that name carries none of, which is no record
                    // and no error.
                    code: if holds_name(zone, &owner) || !records.is_empty() {
                        ResponseCode::NoError
                    } else {
                        ResponseCode::NxDomain
                    },
                    authoritative: true,
                    records,
                });
            };
            records.extend(answered(&reported_under, TYPE_CNAME, alias));
            let Some(target) = alias
                .values
                .first()
                .map(|target| route53::canonical_dns_name(target))
            else {
                break;
            };
            if target == owner {
                break;
            }
            owner = target.clone();
            reported_under = target;
        }
        Some(Answer {
            code: ResponseCode::NoError,
            authoritative: true,
            records,
        })
    }
}

/// Whether `asked` — a name already in this store's canonical form — is a name a query can be
/// resolved for at all: one inside the LIMITS the domain-name format publishes
/// ([`MAX_NAME_OCTETS`], [`MAX_LABEL_OCTETS`]).
///
/// A value past either limit is no domain name: no zone of the account could hold a record set at
/// it, and nothing recursed for it would be answered either, so a query for one is answered the
/// way a query this resolver has no answer for is. Answered here rather than resolved, which also
/// bounds what ONE query costs: every lookup below walks the name's labels, and the name is a
/// caller's string.
fn a_resolvable_name(asked: &str) -> bool {
    !asked.is_empty()
        && asked.len() <= MAX_NAME_OCTETS
        && asked
            .split('.')
            .all(|label| label.len() <= MAX_LABEL_OCTETS)
}

/// The record set of `zone` that answers a query for the canonical name `owner` of
/// `record_type`: the set standing AT that name, and otherwise the closest WILDCARD set of the
/// zone that covers it.
///
/// "You can create a wildcard record … `*` must replace the leftmost label in a domain name"
/// (`ResourceRecordSet$Name`), and such a record set answers every name under its parent that no
/// closer name of the zone answers — which is the closest-encloser rule for a wildcard (RFC 1034
/// §4.3.3): a name the zone holds a record set at is answered by that set, never by a wildcard,
/// and the wildcard that answers is the one with the LONGEST parent still enclosing the name.
fn set_for<'a>(zone: &'a ViewZone, owner: &str, record_type: &str) -> Option<&'a ViewRecordSet> {
    if let Some(set) = zone.records.get(&(owner.to_string(), record_type.to_string())) {
        return Some(set);
    }
    // The parents of the name, closest first, each asked for the wildcard standing under it.
    // A wildcard only covers names BELOW its own parent, so the name itself is not a parent of
    // the search, and the zone's own apex is the last one tried.
    enclosers(zone, owner).find_map(|parent| {
        zone.records
            .get(&(format!("{WILDCARD_LABEL}{parent}"), record_type.to_string()))
    })
}

/// Whether the zone holds the canonical name `owner` at all — at any record type, or through a
/// wildcard of the zone that covers it, which is a name that EXISTS as far as a query for
/// another type of it is concerned.
fn holds_name(zone: &ViewZone, owner: &str) -> bool {
    if zone
        .records
        .keys()
        .any(|(held_name, _type)| held_name == owner)
    {
        return true;
    }
    enclosers(zone, owner).any(|parent| {
        let wildcard = format!("{WILDCARD_LABEL}{parent}");
        zone.records
            .keys()
            .any(|(held_name, _type)| held_name == &wildcard)
    })
}

/// The names a wildcard answering `owner` could stand under, closest first: every suffix of the
/// name from its parent down to the zone's own name.
fn enclosers<'a>(zone: &'a ViewZone, owner: &'a str) -> impl Iterator<Item = &'a str> + 'a {
    let zone_name = zone.name.as_str();
    ::std::iter::successors(parent_of(owner), |name| parent_of(name))
        .take_while(move |name| name.len() >= zone_name.len())
}

/// The name one label up from the canonical name `name`, and `None` for the root.
fn parent_of(name: &str) -> Option<&str> {
    let (_label, parent) = name.split_once('.')?;
    (!parent.is_empty()).then_some(parent)
}

/// The records a query for a name whose addresses a DNS STANDARD fixes is answered with, and
/// `None` for a name no standard fixes — which is a name no zone of the account holds and this
/// resolver cannot have recursed for.
///
/// Which names those are, and which addresses they stand for, is the one reading of them this
/// tree holds (`service_common::public_dns_standard`). The answer is NOT authoritative: the
/// Resolver is the authority for a private zone and recurses for everything else.
fn standard_public_answer(asked: &str, record_type: &str) -> Option<Answer> {
    if !::service_common::public_dns_standard::names_a_fixed_address(asked) {
        return None;
    }
    Some(Answer {
        code: ResponseCode::NoError,
        authoritative: false,
        records: ::service_common::public_dns_standard::fixed_addresses_of_type(
            asked,
            record_type,
        )
        .into_iter()
        .map(|address| AnsweredRecord {
            name: asked.to_string(),
            ttl: DEFAULT_TTL,
            record_type: if address.is_ipv4() {
                TYPE_A.to_string()
            } else {
                TYPE_AAAA.to_string()
            },
            value: address.to_string(),
        })
        .collect(),
    })
}

impl VpcDnsView {
    /// What the DNS Firewall answers a query for the canonical name `asked` of `record_type`
    /// with, or `None` for a query it lets through — one no rule of the view matches, and one
    /// a matched `ALLOW` or `ALERT` rule permits.
    ///
    /// A block is the resolver's own answer rather than a zone's, so it is not authoritative:
    /// `NXDOMAIN` carries no record, `NODATA` is a successful answer with no record, and
    /// `OVERRIDE` carries the rule's own record under the name that was asked for.
    fn blocked(&self, asked: &str, record_type: &str) -> Option<Answer> {
        if self.firewall.is_empty() {
            return None;
        }
        // A spelling this emulator cannot number is still a query the firewall decides: a rule
        // that names no `Qtype` evaluates every type, so the number is passed as what it is —
        // unknown — rather than taking the query out of the firewall's reach.
        let qtype = qtype_number(record_type);
        match self.firewall.decision(asked, qtype)? {
            FirewallDecision::Allow => None,
            FirewallDecision::BlockNxDomain => Some(Answer {
                code: ResponseCode::NxDomain,
                authoritative: false,
                records: Vec::new(),
            }),
            FirewallDecision::BlockNoData => Some(Answer {
                code: ResponseCode::NoError,
                authoritative: false,
                records: Vec::new(),
            }),
            FirewallDecision::BlockOverride {
                domain,
                dns_type,
                ttl,
            } => Some(Answer {
                code: ResponseCode::NoError,
                authoritative: false,
                records: vec![AnsweredRecord {
                    name: asked.to_string(),
                    ttl: i64::from(ttl),
                    record_type: dns_type,
                    value: route53::canonical_dns_name(&domain),
                }],
            }),
        }
    }

}

/// The zone of `zones` a query for `asked` is answered out of: the one whose name is the
/// LONGEST suffix of the name, and `None` when no zone of the VPC holds it.
///
/// Whether a name is OF a zone is Route 53's own reading of it
/// ([`route53::name_is_in_zone`], which compares whole labels), so this resolver and the
/// zone's own operations cannot disagree about which names a zone owns.
fn zone_for<'a>(zones: &'a [ViewZone], asked: &str) -> Option<&'a ViewZone> {
    zones
        .iter()
        .filter(|zone| route53::name_is_in_zone(asked, &zone.name))
        .max_by_key(|zone| zone.name.len())
}

/// One record set as the answer section carries it: one record per value.
fn answered(owner: &str, record_type: &str, set: &ViewRecordSet) -> Vec<AnsweredRecord> {
    set.values
        .iter()
        .map(|value| AnsweredRecord {
            name: owner.to_string(),
            ttl: set.ttl,
            record_type: record_type.to_string(),
            value: value.clone(),
        })
        .collect()
}

/// The DNS a client running INSIDE the instance `instance_id` names can reach, read off the
/// three stores that own those facts — or `None` when this engine holds no Amazon EC2 store,
/// or that store holds no such instance, or the instance is in no VPC.
///
/// Call it with NO store guard held: it takes each of the three in turn and releases it before
/// the next.
pub fn view_from_instance<Eng: Send + Sync + 'static>(
    engine: &Eng,
    instance_id: &str,
) -> Option<VpcDnsView> {
    StoredVpcDns::of(engine)?.view_from_instance(instance_id)
}

/// The store handles this data plane is read off: this service's own Resolver endpoints, the
/// Amazon EC2 inventory the client, the subnets and the security groups are facts of, Route 53's
/// own hosted zones, and the DNS Firewall filtering the VPC.
///
/// A handle is an `Arc` on the state itself rather than on the engine, so what holds one keeps no
/// engine alive and reads the SAME records the services' own operations write — which is what
/// lets this data plane be REGISTERED for the components of other crates that carry an in-VPC
/// query ([`super::vpc_dns_register`]).
#[derive(Clone)]
pub struct StoredVpcDns {
    /// This service's own store, which holds the Resolver endpoints.
    pub(crate) resolver:
        ::engine::ServiceState<super::ComAmazonawsRoute53resolverRoute53resolverState>,
    /// Amazon EC2's store, or `None` for an engine that vends none — then there is no VPC, no
    /// subnet and no security group to read.
    ec2: Option<::engine::ServiceState<ec2::ComAmazonawsEc2Amazonec2State>>,
    /// Route 53's store, or `None` for an engine that vends none — then no hosted zone answers.
    route53: Option<::engine::ServiceState<route53::ComAmazonawsRoute53Awsdnsv20130401State>>,
    /// The DNS Firewall filtering a VPC's queries, read off this service's own stores.
    pub(crate) firewall: Option<super::dns_firewall::StoredFirewall>,
    /// CloudWatch Logs' store, or `None` for an engine that vends none — then a query logging
    /// configuration delivering into a log group has no group to deliver into
    /// ([`super::query_log_delivery`]).
    pub(crate) logs:
        Option<::engine::ServiceState<logs::ComAmazonawsCloudwatchlogsLogs20140328State>>,
}

/// Written by hand rather than derived: the state handles inside carry no `Debug`, and what is
/// diagnosable about this plane is which stores it reads.
impl ::std::fmt::Debug for StoredVpcDns {
    fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        formatter
            .debug_struct("StoredVpcDns")
            .field("ec2", &self.ec2.is_some())
            .field("route53", &self.route53.is_some())
            .finish()
    }
}

impl StoredVpcDns {
    /// The data plane reading the stores of `engine`, or `None` for an engine that vends this
    /// service none.
    #[must_use]
    pub fn of<Eng: Send + Sync + 'static>(engine: &Eng) -> Option<Self> {
        Some(Self {
            resolver: super::service_state(engine)?,
            ec2: ec2::service_state(engine),
            route53: route53::service_state(engine),
            firewall: super::dns_firewall::StoredFirewall::of(engine),
            logs: logs::service_state(engine),
        })
    }

    /// Whether the AUTODEFINED rules for reverse DNS lookups are in force in `vpc_id`: the VPC's
    /// own Resolver behaviour configuration, as of now.
    ///
    /// "Resolver … create autodefined rules for reverse DNS lookups"
    /// (`UpdateResolverConfigRequest$AutodefinedReverseFlag`), and a configuration whose
    /// `AutodefinedReverse` has SETTLED on disabled has none: the status it reports while the
    /// move is still in flight is "being disabled but are not complete"
    /// (`ResolverConfig$AutodefinedReverse`), so the rules are still answering until the move the
    /// clock owes falls due. Every VPC the store holds no record for reports the documented
    /// default, which is enabled.
    ///
    /// Call it with NO store guard held.
    #[must_use]
    pub fn autodefined_reverse_answers(&self, vpc_id: &str) -> bool {
        let store = self.resolver.lock();
        store
            .resolver_config(vpc_id)
            .autodefined_reverse_at(Instant::now())
            != super::AUTODEFINED_REVERSE_DISABLED
    }

    /// The DNS a client running inside the instance `instance_id` names can reach, and `None`
    /// when no store of this plane holds such an instance in a VPC.
    ///
    /// Call it with NO store guard held: it takes each store in turn and releases it before the
    /// next.
    #[must_use]
    pub fn view_from_instance(&self, instance_id: &str) -> Option<VpcDnsView> {
        let client = self.client_of(instance_id)?;
        let mut view = self.view_for(&client);
        // A query made on an instance is recorded as the INSTANCE's, which is how a query log
        // names the source of one.
        view.instance_id = Some(instance_id.to_string());
        Some(view)
    }

    /// The DNS a client of `vpc_id` at `source_ip`, a member of `security_group_ids`, can reach.
    ///
    /// What a client IS differs by the service that runs it — an instance, an execution
    /// environment's network interface — so a caller that holds those three facts itself asks
    /// with them rather than through a resource id this plane would have to resolve.
    #[must_use]
    pub fn view_for_client(
        &self,
        vpc_id: &str,
        source_ip: &str,
        security_group_ids: &[String],
    ) -> VpcDnsView {
        self.view_for(&DnsClient {
            vpc_id: vpc_id.to_string(),
            source_ip: source_ip.to_string(),
            groups: security_group_ids.to_vec(),
        })
    }

    /// What the private hosted zones associated with `vpc_id` hold for `name` of `record_type`,
    /// read WITHOUT the rest of the view: a caller that has already decided WHERE its query went
    /// asks only the zones ([`super::vpc_dns_register`]), so no Resolver endpoint is enumerated
    /// and no security-group decision is made for it.
    ///
    /// `None` when no zone of that VPC holds the name at all — a question those zones do not
    /// answer, rather than one they answer with no record.
    #[must_use]
    pub fn private_zone_answer(
        &self,
        vpc_id: &str,
        name: &str,
        record_type: &str,
    ) -> Option<Answer> {
        answer_from_zones(&self.zones_of_vpc(vpc_id), name, record_type)
    }

    /// One instant's reading of what `client` can reach and of what it answers.
    fn view_for(&self, client: &DnsClient) -> VpcDnsView {
        let endpoints = self.inbound_endpoint_addresses(&client.vpc_id);
        let resolver_addresses = self.vpc_resolver_addresses(&client.vpc_id);
        // What the VPC's DHCP option set publishes to its instances is the VPC's own resolver, at
        // the base of the range plus two where that is known and at the link-local address
        // otherwise — both of which answer.
        let nameserver = resolver_addresses
            .iter()
            .find(|address| address.octets() != VPC_RESOLVER_LINK_LOCAL_OCTETS)
            .copied()
            .unwrap_or_else(|| Ipv4Addr::from(VPC_RESOLVER_LINK_LOCAL_OCTETS));
        let mut servers: Vec<DnsServer> = resolver_addresses
            .into_iter()
            .map(|address| DnsServer {
                address,
                // The VPC's own resolver is behind no interface of the account, so no security
                // group stands between a client of the VPC and it.
                admits_udp: true,
                admits_tcp: true,
            })
            .collect();
        servers.extend(self.admitted_endpoint_servers(client, &endpoints));
        VpcDnsView {
            vpc_id: client.vpc_id.clone(),
            source_ip: client.source_ip.clone(),
            nameserver,
            servers,
            zones: self.zones_of_vpc(&client.vpc_id),
            firewall: self
                .firewall
                .as_ref()
                .map(|firewall| firewall.view_for_vpc(&client.vpc_id))
                .unwrap_or_default(),
            groups: client.groups.clone(),
            // The same stores this view was read off are where a query it answers is recorded.
            recorder: Some(self.clone()),
            instance_id: None,
        }
    }

    /// The client `instance_id` names, read off Amazon EC2's own store.
    fn client_of(&self, instance_id: &str) -> Option<DnsClient> {
        let state = self.ec2.as_ref()?;
        let store = state.lock();
        let instance = store.instances.get(instance_id)?;
        let subnet_id = instance.subnet_id.clone()?;
        let vpc_id = store.subnets.get(&subnet_id)?.vpc_id.clone()?;
        let groups = instance
            .security_groups
            .iter()
            .filter_map(|(id, name)| match (id, name) {
                (Some(id), _) => Some(id.clone()),
                (None, Some(name)) => store.security_group_id_for_name(&vpc_id, name),
                (None, None) => None,
            })
            .collect();
        Some(DnsClient {
            vpc_id,
            source_ip: instance.private_ip.clone(),
            groups,
        })
    }
}

/// The client a query comes from: where it is and what it is a member of.
#[derive(Debug, Clone)]
struct DnsClient {
    /// The VPC the client is in.
    vpc_id: String,
    /// Its private address, which is the source of its queries.
    source_ip: String,
    /// The security groups it is a member of, by id — what a rule whose source is a security
    /// group is matched against.
    groups: Vec<String>,
}

impl StoredVpcDns {
    /// The addresses the VPC's OWN resolver answers at: the link-local one, and
    /// [`VPC_RESOLVER_OFFSET`] past the base of each IPv4 range the VPC holds.
    ///
    /// NONE for a VPC whose `enableDnsSupport` attribute is off: that attribute is what decides
    /// whether the Amazon-provided DNS server resolves queries in the VPC at all
    /// (`ModifyVpcAttribute`), and it is read through Amazon EC2's own accessor for it.
    fn vpc_resolver_addresses(&self, vpc_id: &str) -> Vec<Ipv4Addr> {
        let Some(state) = self.ec2.as_ref() else {
            // No Amazon EC2 store on this engine is no VPC to read the attribute of, so the
            // link-local address every VPC's resolver answers at is all there is.
            return vec![Ipv4Addr::from(VPC_RESOLVER_LINK_LOCAL_OCTETS)];
        };
        let store = state.lock();
        let Some(vpc) = store.vpcs.get(vpc_id) else {
            return Vec::new();
        };
        if !store
            .vpc_attributes_of(vpc_id)
            .is_none_or(|attributes| attributes.enable_dns_support)
        {
            return Vec::new();
        }
        let mut addresses = vec![Ipv4Addr::from(VPC_RESOLVER_LINK_LOCAL_OCTETS)];
        let mut blocks: Vec<String> = vpc.cidr_block.clone().into_iter().collect();
        for association in vpc.cidr_block_association_set.iter().flatten() {
            if let Some(block) = association.cidr_block.clone() {
                blocks.push(block);
            }
        }
        for block in blocks {
            if let Some(address) = resolver_address_of_block(&block) {
                if !addresses.contains(&address) {
                    addresses.push(address);
                }
            }
        }
        addresses
    }
}

/// The resolver address of one IPv4 range: its base plus [`VPC_RESOLVER_OFFSET`].
///
/// `None` for text that is not an address range, and for one so high in the space that the
/// offset would run off its end — a range no VPC is created with, and not an address to
/// report.
fn resolver_address_of_block(block: &str) -> Option<Ipv4Addr> {
    let network: ::ipnet::Ipv4Net = block.trim().parse().ok()?;
    let base = u32::from(network.network());
    Some(Ipv4Addr::from(base.checked_add(VPC_RESOLVER_OFFSET)?))
}

/// One inbound endpoint address of the VPC: the address, and the security groups on the
/// interface holding it.
#[derive(Debug, Clone)]
struct EndpointAddress {
    /// The address itself.
    address: Ipv4Addr,
    /// The endpoint's security groups, which are the groups on its interfaces.
    groups: Vec<String>,
}

impl StoredVpcDns {
    /// Every address an OPERATIONAL inbound Resolver endpoint of `vpc_id` has attached.
    fn inbound_endpoint_addresses(&self, vpc_id: &str) -> Vec<EndpointAddress> {
        let now = Instant::now();
        let store = self.resolver.lock();
        store
            .resolver_endpoints
            .values()
            .filter(|endpoint| endpoint.host_vpc_id.as_deref() == Some(vpc_id))
            .filter(|endpoint| endpoint.direction.eq_ignore_ascii_case(DIRECTION_INBOUND))
            // An endpoint that has not reached its terminal status yet has no address serving
            // anything: `ResolverEndpointStatus` `OPERATIONAL` is "the Resolver endpoint is
            // ready to be used".
            .filter(|endpoint| endpoint.status_at(now) == ENDPOINT_STATUS_OPERATIONAL)
            .flat_map(|endpoint| {
                endpoint
                    .ip_addresses
                    .iter()
                    .filter(|held| held.status_at(now) == IP_STATUS_ATTACHED)
                    .filter_map(|held| held.ip.as_deref()?.trim().parse::<Ipv4Addr>().ok())
                    .map(|address| EndpointAddress {
                        address,
                        groups: endpoint.security_group_ids.clone(),
                    })
                    .collect::<Vec<EndpointAddress>>()
            })
            .collect()
    }

    /// Each endpoint address as a server of the view, with what its security groups admit from
    /// this client — Amazon EC2's own decision over the rules it holds.
    fn admitted_endpoint_servers(
        &self,
        client: &DnsClient,
        addresses: &[EndpointAddress],
    ) -> Vec<DnsServer> {
        if addresses.is_empty() {
            return Vec::new();
        }
        let Some(state) = self.ec2.as_ref() else {
            return Vec::new();
        };
        let store = state.lock();
        addresses
            .iter()
            .map(|held| DnsServer {
                address: held.address,
                admits_udp: store.security_groups_admit_ingress(
                    &held.groups,
                    PROTOCOL_UDP,
                    DNS_PORT,
                    &client.source_ip,
                    &client.groups,
                ),
                admits_tcp: store.security_groups_admit_ingress(
                    &held.groups,
                    PROTOCOL_TCP,
                    DNS_PORT,
                    &client.source_ip,
                    &client.groups,
                ),
            })
            .collect()
    }

    /// The private hosted zones associated with `vpc_id`, with the record sets each answers out
    /// of, read through Route 53's own store.
    ///
    /// One entry per owner name and type: a name and type a zone holds a ROUTING GROUP at (a
    /// weighted, failover, multivalue or geolocation group, whose members share a name and type
    /// and differ by `SetIdentifier`) is answered here from the group's FIRST member in that
    /// order, which is the member Route 53's own answer path selects for a group carrying no
    /// policy. Which member a policy selects is that path's own decision (`TestDNSAnswer`), and
    /// it is not re-decided here.
    fn zones_of_vpc(&self, vpc_id: &str) -> Vec<ViewZone> {
        let Some(state) = self.route53.as_ref() else {
            return Vec::new();
        };
        let store = state.lock();
        store
            .zones
            .values()
            .filter(|zone| zone.private_zone)
            .filter(|zone| {
                zone.vpcs
                    .iter()
                    .any(|vpc| vpc.vpc_id.as_deref() == Some(vpc_id))
            })
            .take(MAX_VIEW_ZONES)
            .map(|zone| {
                let mut records: BTreeMap<(String, String), ViewRecordSet> = BTreeMap::new();
                for record in zone.records.values().take(MAX_VIEW_RECORDS_PER_ZONE) {
                    let key = (
                        route53::canonical_dns_name(&record.name),
                        record.r#type.to_ascii_uppercase(),
                    );
                    records.entry(key).or_insert_with(|| view_record_set(record));
                }
                ViewZone {
                    name: route53::canonical_dns_name(&zone.name),
                    records,
                }
            })
            .collect()
    }
}

/// One stored record set as the view holds it.
///
/// An ALIAS record set carries no records of its own: its target either names an ADDRESS, which
/// is the value a query for that type is answered with (`RecordDataEntry`: "an IP address …
/// for the … resource", read by Route 53's own [`route53::dns_name_address`]), or names another
/// NAME, in which case the value is the name the answer is resolved ON rather than the answer
/// itself ([`ViewRecordSet::alias_to_name`]).
fn view_record_set(record: &ResourceRecordSet) -> ViewRecordSet {
    let mut values: Vec<String> = record
        .resource_records
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|value| value.value.clone())
        .collect();
    let mut alias_to_name = false;
    if let Some(target) = record.alias_target.as_ref() {
        match route53::dns_name_address(&target.dns_name) {
            Some(address) => values.push(address.to_string()),
            None => {
                values.push(target.dns_name.clone());
                alias_to_name = true;
            }
        }
    }
    values.truncate(MAX_VIEW_VALUES_PER_RECORD_SET);
    ViewRecordSet {
        ttl: record.ttl.unwrap_or(DEFAULT_TTL),
        values,
        alias_to_name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A view holding one zone and one server, for the rows below.
    fn view() -> VpcDnsView {
        let mut records = BTreeMap::new();
        records.insert(
            ("app.example.internal.".to_string(), TYPE_A.to_string()),
            ViewRecordSet {
                ttl: 300,
                values: vec!["192.0.2.10".to_string()],
                alias_to_name: false,
            },
        );
        records.insert(
            ("www.example.internal.".to_string(), TYPE_CNAME.to_string()),
            ViewRecordSet {
                ttl: 60,
                values: vec!["app.example.internal".to_string()],
                alias_to_name: false,
            },
        );
        records.insert(
            ("mail.example.internal.".to_string(), "MX".to_string()),
            ViewRecordSet {
                ttl: 60,
                values: vec!["10 mx.example.internal".to_string()],
                alias_to_name: false,
            },
        );
        // An ALIAS record set whose target names another NAME of the zone: answered with that
        // name's own data, under the alias's own name.
        records.insert(
            ("alias.example.internal.".to_string(), TYPE_A.to_string()),
            ViewRecordSet {
                ttl: DEFAULT_TTL,
                values: vec!["app.example.internal".to_string()],
                alias_to_name: true,
            },
        );
        // One whose target named an ADDRESS, which is the value it answers with.
        records.insert(
            ("served.example.internal.".to_string(), TYPE_A.to_string()),
            ViewRecordSet {
                ttl: DEFAULT_TTL,
                values: vec!["192.0.2.20".to_string()],
                alias_to_name: false,
            },
        );
        VpcDnsView {
            vpc_id: "vpc-00000000000000001".to_string(),
            source_ip: "10.91.1.7".to_string(),
            nameserver: "10.91.0.2".parse().expect("an address"),
            servers: vec![
                DnsServer {
                    address: Ipv4Addr::from(VPC_RESOLVER_LINK_LOCAL_OCTETS),
                    admits_udp: true,
                    admits_tcp: true,
                },
                DnsServer {
                    address: "10.91.1.10".parse().expect("an address"),
                    admits_udp: false,
                    admits_tcp: true,
                },
            ],
            zones: vec![ViewZone {
                name: "example.internal.".to_string(),
                records,
            }],
            // The rows below are about the zones; a view whose VPC no rule group reaches
            // filters nothing, which is every row here.
            firewall: FirewallView::default(),
            groups: Vec::new(),
            // The rows below are about what is ANSWERED; a view built over no store records
            // nothing, which is what a reading taken for a test alone does.
            recorder: None,
            instance_id: None,
        }
    }

    /// The wildcard record set the rows below ask for, added to [`view`]'s own zone: a record set
    /// standing at the name whose leftmost label is the asterisk, which is how Route 53 holds a
    /// wildcard record set and how this store canonicalises its name.
    fn view_with_a_wildcard() -> VpcDnsView {
        let mut view = view();
        let zone = view.zones.first_mut().expect("the view holds one zone");
        zone.records.insert(
            (
                format!("{WILDCARD_LABEL}wild.example.internal."),
                TYPE_A.to_string(),
            ),
            ViewRecordSet {
                ttl: 120,
                values: vec!["192.0.2.30".to_string()],
                alias_to_name: false,
            },
        );
        zone.records.insert(
            (
                "exact.wild.example.internal.".to_string(),
                TYPE_A.to_string(),
            ),
            ViewRecordSet {
                ttl: 120,
                values: vec!["192.0.2.31".to_string()],
                alias_to_name: false,
            },
        );
        view
    }

    /// RULE: a WILDCARD record set of the zone answers every name under its parent that no closer
    /// name of the zone answers, under the name that was ASKED for — and answers nothing for a
    /// name outside its parent, or for a type it holds no record of.
    ///
    /// The rows span what the rule generalises over: a name one label under the wildcard's
    /// parent, a name several labels under it, the parent itself, a name the zone holds a record
    /// set of its own at (which the wildcard must not answer), a type the wildcard holds no
    /// record of, and a name outside the wildcard's parent.
    #[test]
    fn a_wildcard_record_set_answers_the_names_under_its_parent() {
        let view = view_with_a_wildcard();
        let link_local = Ipv4Addr::from(VPC_RESOLVER_LINK_LOCAL_OCTETS).to_string();
        // (case, the name asked for, the type, the response code, the values answered)
        let rows: &[(&str, &str, &str, ResponseCode, &[&str])] = &[
            (
                "a name one label under the wildcard's parent",
                "app.wild.example.internal",
                TYPE_A,
                ResponseCode::NoError,
                &["192.0.2.30"],
            ),
            (
                "a name several labels under it",
                "db.inner.wild.example.internal",
                TYPE_A,
                ResponseCode::NoError,
                &["192.0.2.30"],
            ),
            (
                "the wildcard's parent itself, which a wildcard does not cover",
                "wild.example.internal",
                TYPE_A,
                ResponseCode::NxDomain,
                &[],
            ),
            (
                "a name the zone holds its own record set at",
                "exact.wild.example.internal",
                TYPE_A,
                ResponseCode::NoError,
                &["192.0.2.31"],
            ),
            (
                "a type the wildcard holds no record of",
                "app.wild.example.internal",
                TYPE_AAAA,
                ResponseCode::NoError,
                &[],
            ),
            (
                "a name outside the wildcard's parent",
                "elsewhere.example.internal",
                TYPE_A,
                ResponseCode::NxDomain,
                &[],
            ),
        ];
        for (case, name, record_type, code, values) in rows {
            let answered = view.answer(&link_local, name, record_type, PROTOCOL_UDP);
            let Resolution::Answered(answer) = answered else {
                panic!("case {case:?}: expected an answer, got {answered:?}");
            };
            assert_eq!(
                answer.code, *code,
                "case {case:?}: expected {code:?}, got {answer:?}"
            );
            let rendered: Vec<&str> = answer
                .records
                .iter()
                .map(|record| record.value.as_str())
                .collect();
            assert_eq!(
                rendered, *values,
                "case {case:?}: expected the values {values:?}, got {answer:?}"
            );
            // A wildcard's answer is reported under the name that was asked for, not under the
            // wildcard's own name (RFC 1034 §4.3.3).
            for record in &answer.records {
                assert_eq!(
                    record.name,
                    route53::canonical_dns_name(name),
                    "case {case:?}: expected the answer under the name asked for, got {record:?}"
                );
            }
        }
    }

    /// RULE: a name whose addresses a DNS standard fixes is answered out of that standard, and
    /// every other name under no zone of the VPC is one this resolver has no answer for.
    ///
    /// The rows span what the rule generalises over: each address family of such a name, a type
    /// it holds no record of, a label under the same domain that no standard fixes, and an
    /// ordinary public name.
    #[test]
    fn a_name_a_standard_fixes_is_answered_and_every_other_public_name_is_not() {
        let view = view();
        let link_local = Ipv4Addr::from(VPC_RESOLVER_LINK_LOCAL_OCTETS).to_string();
        // (case, the name asked for, the type, the response code, the values answered)
        let rows: &[(&str, &str, &str, ResponseCode, &[&str])] = &[
            (
                "the address of a reserved documentation domain",
                "example.net",
                TYPE_A,
                ResponseCode::NoError,
                &["192.0.2.1"],
            ),
            (
                "its IPv6 address",
                "example.net",
                TYPE_AAAA,
                ResponseCode::NoError,
                &["2001:db8::1"],
            ),
            (
                "the host label published beside one",
                "www.example.org",
                TYPE_A,
                ResponseCode::NoError,
                &["192.0.2.1"],
            ),
            (
                "a type such a name holds no record of",
                "example.com",
                TYPE_CNAME,
                ResponseCode::NoError,
                &[],
            ),
            (
                "a label under the same domain no standard fixes",
                "broker.example.com",
                TYPE_A,
                ResponseCode::ServFail,
                &[],
            ),
            (
                "an ordinary public name",
                "aws.amazon.com",
                TYPE_A,
                ResponseCode::ServFail,
                &[],
            ),
        ];
        for (case, name, record_type, code, values) in rows {
            let answered = view.answer(&link_local, name, record_type, PROTOCOL_UDP);
            let Resolution::Answered(answer) = answered else {
                panic!("case {case:?}: expected an answer, got {answered:?}");
            };
            assert_eq!(
                answer.code, *code,
                "case {case:?}: expected {code:?}, got {answer:?}"
            );
            assert!(
                !answer.authoritative,
                "case {case:?}: a recursed answer is not this resolver's own, got {answer:?}"
            );
            let rendered: Vec<&str> = answer
                .records
                .iter()
                .map(|record| record.value.as_str())
                .collect();
            assert_eq!(
                rendered, *values,
                "case {case:?}: expected the values {values:?}, got {answer:?}"
            );
        }
    }

    /// RULE: a value that is no DOMAIN NAME is a query this resolver has no answer for, whatever
    /// zone of the VPC its suffix falls in — so neither the published length of a name nor the
    /// length of one of its labels can be exceeded by what a client sends.
    ///
    /// The rows span each published bound ([`MAX_NAME_OCTETS`], [`MAX_LABEL_OCTETS`]): a name at
    /// it, and one past it, under a zone of the VPC and under no zone at all.
    #[test]
    fn a_value_that_is_no_domain_name_is_answered_by_nothing() {
        let view = view();
        let link_local = Ipv4Addr::from(VPC_RESOLVER_LINK_LOCAL_OCTETS).to_string();
        let zone = "example.internal";
        // The longest label the format admits, and one octet past it.
        let longest_label = "a".repeat(MAX_LABEL_OCTETS);
        let overlong_label = "a".repeat(MAX_LABEL_OCTETS + 1);
        // A name of labels of the longest admitted length, grown past the whole name's limit.
        let overlong_name = {
            let mut name = String::new();
            while name.len() + longest_label.len() + zone.len() + 2 <= MAX_NAME_OCTETS {
                name.push_str(&longest_label);
                name.push('.');
            }
            format!("{name}{longest_label}.{zone}")
        };
        // (case, the name asked for, the response code it is answered with)
        let rows: &[(&str, String, ResponseCode)] = &[
            (
                "a name under the zone at the longest admitted label length",
                format!("{longest_label}.{zone}"),
                ResponseCode::NxDomain,
            ),
            (
                "the same name one octet past that label length",
                format!("{overlong_label}.{zone}"),
                ResponseCode::ServFail,
            ),
            (
                "a name under the zone past the whole name's limit",
                overlong_name.clone(),
                ResponseCode::ServFail,
            ),
            (
                "a name under no zone past the whole name's limit",
                format!("{overlong_name}.example.com"),
                ResponseCode::ServFail,
            ),
        ];
        for (case, name, code) in rows {
            let answered = view.answer(&link_local, name, TYPE_A, PROTOCOL_UDP);
            let Resolution::Answered(answer) = answered else {
                panic!("case {case:?}: expected an answer, got {answered:?}");
            };
            assert_eq!(
                answer.code, *code,
                "case {case:?}: expected {code:?} for a name of {} octets, got {answer:?}",
                name.len()
            );
            assert!(
                answer.records.is_empty(),
                "case {case:?}: expected no record, got {answer:?}"
            );
        }
    }

    /// RULE: a query is answered out of the VPC's own zones when it reaches an address a
    /// resolver answers at and the groups on that address admit it; otherwise it reaches no
    /// server, or is dropped.
    ///
    /// The rows span what the reading generalises over: the server (the link-local resolver, an
    /// endpoint address, an address nothing answers at), the protocol one admits and the other
    /// does not, and the name asked for (one the zone holds, an alias chain, a name of another
    /// type, a name the zone holds nothing at, a name under no zone of the VPC).
    #[test]
    fn a_query_is_answered_by_the_zone_the_name_falls_in() {
        let view = view();
        let link_local = Ipv4Addr::from(VPC_RESOLVER_LINK_LOCAL_OCTETS).to_string();
        let rows: &[(&str, &str, &str, &str, &str, Resolution)] = &[
            (
                "a name the zone holds",
                &link_local,
                "app.example.internal",
                TYPE_A,
                PROTOCOL_UDP,
                Resolution::Answered(Answer {
                    code: ResponseCode::NoError,
                    authoritative: true,
                    records: vec![AnsweredRecord {
                        name: "app.example.internal.".to_string(),
                        ttl: 300,
                        record_type: TYPE_A.to_string(),
                        value: "192.0.2.10".to_string(),
                    }],
                }),
            ),
            (
                "an alias chased to the address it names",
                &link_local,
                "www.example.internal.",
                TYPE_A,
                PROTOCOL_UDP,
                Resolution::Answered(Answer {
                    code: ResponseCode::NoError,
                    authoritative: true,
                    records: vec![
                        AnsweredRecord {
                            name: "www.example.internal.".to_string(),
                            ttl: 60,
                            record_type: TYPE_CNAME.to_string(),
                            value: "app.example.internal".to_string(),
                        },
                        AnsweredRecord {
                            name: "app.example.internal.".to_string(),
                            ttl: 300,
                            record_type: TYPE_A.to_string(),
                            value: "192.0.2.10".to_string(),
                        },
                    ],
                }),
            ),
            (
                "a name the zone holds another type at",
                &link_local,
                "mail.example.internal",
                TYPE_A,
                PROTOCOL_UDP,
                Resolution::Answered(Answer {
                    code: ResponseCode::NoError,
                    authoritative: true,
                    records: Vec::new(),
                }),
            ),
            (
                "a name the zone holds nothing at",
                &link_local,
                "absent.example.internal",
                TYPE_A,
                PROTOCOL_UDP,
                Resolution::Answered(Answer {
                    code: ResponseCode::NxDomain,
                    authoritative: true,
                    records: Vec::new(),
                }),
            ),
            (
                "a name under a zone somebody owns, which the resolver recurses for",
                &link_local,
                "broker.example.com",
                TYPE_A,
                PROTOCOL_UDP,
                Resolution::Answered(Answer {
                    code: ResponseCode::ServFail,
                    authoritative: false,
                    records: Vec::new(),
                }),
            ),
            (
                // The public namespace establishes that a name under one of the top-level
                // domains the root zone never delegates does not exist, which is what the
                // recursion for it comes to.
                "a name under a top-level domain the root zone never delegates",
                &link_local,
                "ledger.nothing-here.invalid",
                TYPE_A,
                PROTOCOL_UDP,
                Resolution::Answered(Answer {
                    code: ResponseCode::NxDomain,
                    authoritative: false,
                    records: Vec::new(),
                }),
            ),
            (
                "a name under another of them, fully qualified",
                &link_local,
                "db.probe.example.test.",
                TYPE_A,
                PROTOCOL_UDP,
                Resolution::Answered(Answer {
                    code: ResponseCode::NxDomain,
                    authoritative: false,
                    records: Vec::new(),
                }),
            ),
            (
                "an endpoint address the groups admit on this protocol",
                "10.91.1.10",
                "app.example.internal",
                TYPE_A,
                PROTOCOL_TCP,
                Resolution::Answered(Answer {
                    code: ResponseCode::NoError,
                    authoritative: true,
                    records: vec![AnsweredRecord {
                        name: "app.example.internal.".to_string(),
                        ttl: 300,
                        record_type: TYPE_A.to_string(),
                        value: "192.0.2.10".to_string(),
                    }],
                }),
            ),
            (
                "the same address on the protocol no rule admits",
                "10.91.1.10",
                "app.example.internal",
                TYPE_A,
                PROTOCOL_UDP,
                Resolution::Dropped,
            ),
            (
                "an address nothing of this VPC answers at",
                "10.91.9.9",
                "app.example.internal",
                TYPE_A,
                PROTOCOL_UDP,
                Resolution::NoServer,
            ),
            (
                "an alias to another name of the zone",
                &link_local,
                "alias.example.internal",
                TYPE_A,
                PROTOCOL_UDP,
                Resolution::Answered(Answer {
                    code: ResponseCode::NoError,
                    authoritative: true,
                    // The referenced record set's data, under the ALIAS's own name: an alias is
                    // not a record the client is answered with.
                    records: vec![AnsweredRecord {
                        name: "alias.example.internal.".to_string(),
                        ttl: 300,
                        record_type: TYPE_A.to_string(),
                        value: "192.0.2.10".to_string(),
                    }],
                }),
            ),
            (
                "an alias whose target named an address",
                &link_local,
                "served.example.internal",
                TYPE_A,
                PROTOCOL_UDP,
                Resolution::Answered(Answer {
                    code: ResponseCode::NoError,
                    authoritative: true,
                    records: vec![AnsweredRecord {
                        name: "served.example.internal.".to_string(),
                        ttl: DEFAULT_TTL,
                        record_type: TYPE_A.to_string(),
                        value: "192.0.2.20".to_string(),
                    }],
                }),
            ),
        ];
        for (case, server, name, record_type, protocol, expected) in rows {
            let answered = view.answer(server, name, record_type, protocol);
            assert_eq!(
                &answered, expected,
                "case {case:?}: expected {expected:?}, got {answered:?}"
            );
        }
    }

    /// RULE: the DNS Firewall of the VPC decides a query BEFORE any zone is read for it, so a
    /// name a matched `BLOCK` rule covers is answered that rule's `BlockResponse` way whether
    /// or not a zone of the VPC holds the name, while a name no rule covers — and one a
    /// matched `ALLOW` rule permits — is answered out of the zones exactly as before.
    ///
    /// The rows differ along what the rule generalises over: the `BlockResponse` (`NXDOMAIN`,
    /// `NODATA`, `OVERRIDE`), whether the blocked name is one the VPC's own zone holds, the
    /// action of the rule that matched, and the record type asked for — including one this
    /// emulator cannot number, which a rule naming no query type still decides.
    #[test]
    fn a_blocked_name_is_answered_by_the_firewall_rather_than_by_the_zone() {
        let link_local = Ipv4Addr::from(VPC_RESOLVER_LINK_LOCAL_OCTETS).to_string();
        // One row: the case, the firewall the VPC is filtered by, the name asked for, the record
        // type asked of it, and the answer that query must come back with.
        let rows: &[(&str, FirewallView, &str, &str, Answer)] = &[
            (
                "a name of the VPC's own zone, blocked with NXDOMAIN",
                FirewallView::of_rule(
                    &["app.example.internal"],
                    None,
                    FirewallDecision::BlockNxDomain,
                ),
                "app.example.internal",
                TYPE_A,
                Answer {
                    code: ResponseCode::NxDomain,
                    authoritative: false,
                    records: Vec::new(),
                },
            ),
            (
                "a name under no zone of the VPC, blocked with NXDOMAIN",
                FirewallView::of_rule(&["example.com"], None, FirewallDecision::BlockNxDomain),
                "example.com",
                TYPE_A,
                Answer {
                    code: ResponseCode::NxDomain,
                    authoritative: false,
                    records: Vec::new(),
                },
            ),
            (
                "a blocked name whose rule answers NODATA",
                FirewallView::of_rule(
                    &["app.example.internal"],
                    None,
                    FirewallDecision::BlockNoData,
                ),
                "app.example.internal",
                TYPE_A,
                Answer {
                    code: ResponseCode::NoError,
                    authoritative: false,
                    records: Vec::new(),
                },
            ),
            (
                "a blocked name whose rule answers its own record",
                FirewallView::of_rule(
                    &["app.example.internal"],
                    None,
                    FirewallDecision::BlockOverride {
                        domain: "safe.example.net".to_string(),
                        dns_type: TYPE_CNAME.to_string(),
                        ttl: 30,
                    },
                ),
                "app.example.internal",
                TYPE_A,
                Answer {
                    code: ResponseCode::NoError,
                    authoritative: false,
                    records: vec![AnsweredRecord {
                        name: "app.example.internal.".to_string(),
                        ttl: 30,
                        record_type: TYPE_CNAME.to_string(),
                        value: "safe.example.net.".to_string(),
                    }],
                },
            ),
            (
                "a name a matched ALLOW rule permits",
                FirewallView::of_rule(&["app.example.internal"], None, FirewallDecision::Allow),
                "app.example.internal",
                TYPE_A,
                Answer {
                    code: ResponseCode::NoError,
                    authoritative: true,
                    records: vec![AnsweredRecord {
                        name: "app.example.internal.".to_string(),
                        ttl: 300,
                        record_type: TYPE_A.to_string(),
                        value: "192.0.2.10".to_string(),
                    }],
                },
            ),
            (
                "a name the firewall's rule does not cover",
                FirewallView::of_rule(&["blocked.example.com"], None, FirewallDecision::BlockNxDomain),
                "app.example.internal",
                TYPE_A,
                Answer {
                    code: ResponseCode::NoError,
                    authoritative: true,
                    records: vec![AnsweredRecord {
                        name: "app.example.internal.".to_string(),
                        ttl: 300,
                        record_type: TYPE_A.to_string(),
                        value: "192.0.2.10".to_string(),
                    }],
                },
            ),
            (
                "a blocked name asked of a record type this emulator cannot number, whose rule \
                 names no query type",
                FirewallView::of_rule(
                    &["app.example.internal"],
                    None,
                    FirewallDecision::BlockNxDomain,
                ),
                "app.example.internal",
                "HTTPS",
                Answer {
                    code: ResponseCode::NxDomain,
                    authoritative: false,
                    records: Vec::new(),
                },
            ),
        ];
        for (case, firewall, name, record_type, expected) in rows {
            let mut filtered = view();
            filtered.firewall = firewall.clone();
            let answered = filtered.answer(&link_local, name, record_type, PROTOCOL_UDP);
            assert_eq!(
                answered,
                Resolution::Answered(expected.clone()),
                "case {case:?}: expected {expected:?} for {name} ({record_type}), got {answered:?}"
            );
        }
    }

    /// The resolver address of a VPC's range is the base plus two, whatever the prefix length,
    /// and nothing at all for a block that is not one.
    #[test]
    fn the_vpc_resolver_address_is_the_base_of_the_range_plus_two() {
        let rows: &[(&str, &str, Option<&str>)] = &[
            ("a /16", "10.91.0.0/16", Some("10.91.0.2")),
            ("a /24", "10.91.3.0/24", Some("10.91.3.2")),
            ("a /20 that is not on its own boundary", "172.31.16.0/20", Some("172.31.16.2")),
            ("a block that is not one", "not-a-block", None),
            // A range at the very top of the space has no base-plus-two address inside it, so
            // none is reported rather than one that wrapped around.
            ("a range the offset runs off the end of", "255.255.255.255/32", None),
        ];
        for (case, block, expected) in rows {
            let address = resolver_address_of_block(block).map(|address| address.to_string());
            assert_eq!(
                address.as_deref(),
                *expected,
                "case {case:?}: expected {expected:?}, got {address:?}"
            );
        }
    }
}
