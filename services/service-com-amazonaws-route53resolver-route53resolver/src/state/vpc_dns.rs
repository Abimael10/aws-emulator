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
//! [`ResponseCode::NxDomain`], a name it holds other types at is answered with no records,
//! and a name under NO zone of the VPC is one this emulator has no answer for
//! ([`ResponseCode::ServFail`]): a query it cannot answer from the account's own records
//! would have to leave this process to be answered, and nothing here does that.
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

use ::service_com_amazonaws_route53_awsdnsv20130401::ops::types::shared_types::ResourceRecordSet;
use ::service_com_amazonaws_route53_awsdnsv20130401::state as route53;

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
        let Some(server) = self
            .servers
            .iter()
            .find(|held| held.address == address)
        else {
            return Resolution::NoServer;
        };
        let admitted = if protocol.eq_ignore_ascii_case(PROTOCOL_TCP) {
            server.admits_tcp
        } else {
            server.admits_udp
        };
        if !admitted {
            return Resolution::Dropped;
        }
        Resolution::Answered(self.resolved(name, record_type))
    }

    /// The address a client of this guest queries when it names no server itself: the one the
    /// guest's resolver configuration holds.
    #[must_use]
    pub fn nameserver(&self) -> String {
        self.nameserver.to_string()
    }

    /// The answer itself: the zone the name falls in, and what that zone holds for it.
    fn resolved(&self, name: &str, record_type: &str) -> Answer {
        let asked = route53::canonical_dns_name(name);
        let record_type = record_type.trim().to_ascii_uppercase();
        let Some(zone) = self.zone_for(&asked) else {
            return Answer {
                code: ResponseCode::ServFail,
                authoritative: false,
                records: Vec::new(),
            };
        };
        let mut records: Vec<AnsweredRecord> = Vec::new();
        // The name being resolved, and the name the answer is reported UNDER. They part company
        // at an ALIAS: an alias is answered with the referenced record set's data under the
        // ALIAS's own name — which is what makes an alias invisible to the client — while a
        // `CNAME` is itself a record of the answer, and what follows it is reported under the
        // name it pointed at.
        let mut owner = asked.clone();
        let mut reported_under = asked;
        for _hop in 0..MAX_CNAME_HOPS {
            if let Some(set) = zone.records.get(&(owner.clone(), record_type.clone())) {
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
                return Answer {
                    code: ResponseCode::NoError,
                    authoritative: true,
                    records,
                };
            }
            // "If … a CNAME record, Resolver returns the CNAME and then resolves the name it
            // points at" — which is why a query for an address is answered through an alias.
            let Some(alias) = zone
                .records
                .get(&(owner.clone(), TYPE_CNAME.to_string()))
                .filter(|_| record_type != TYPE_CNAME)
            else {
                let exists = zone
                    .records
                    .keys()
                    .any(|(held_name, _)| held_name == &owner);
                return Answer {
                    // A name the zone holds some other type at exists: the answer is the one a
                    // resolver gives for a type that name carries none of, which is no record
                    // and no error.
                    code: if exists || !records.is_empty() {
                        ResponseCode::NoError
                    } else {
                        ResponseCode::NxDomain
                    },
                    authoritative: true,
                    records,
                };
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
        Answer {
            code: ResponseCode::NoError,
            authoritative: true,
            records,
        }
    }

    /// The zone of this view a query for `asked` is answered out of: the one whose name is the
    /// LONGEST suffix of the name, and `None` when no zone of the VPC holds it.
    ///
    /// Whether a name is OF a zone is Route 53's own reading of it
    /// ([`route53::name_is_in_zone`], which compares whole labels), so this resolver and the
    /// zone's own operations cannot disagree about which names a zone owns.
    fn zone_for(&self, asked: &str) -> Option<&ViewZone> {
        self.zones
            .iter()
            .filter(|zone| route53::name_is_in_zone(asked, &zone.name))
            .max_by_key(|zone| zone.name.len())
    }
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
    let client = client_of(engine, instance_id)?;
    let endpoints = inbound_endpoint_addresses(engine, &client.vpc_id);
    let resolver_addresses = vpc_resolver_addresses(engine, &client.vpc_id);
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
    servers.extend(admitted_endpoint_servers(engine, &client, &endpoints));
    Some(VpcDnsView {
        vpc_id: client.vpc_id.clone(),
        source_ip: client.source_ip.clone(),
        nameserver,
        servers,
        zones: zones_of_vpc(engine, &client.vpc_id),
    })
}

/// The client a query comes from: where it is and what it is a member of.
#[derive(Debug, Clone)]
struct DnsClient {
    /// The VPC the instance is in.
    vpc_id: String,
    /// Its private address, which is the source of its queries.
    source_ip: String,
    /// The security groups the instance is a member of, by id — what a rule whose source is a
    /// security group is matched against.
    groups: Vec<String>,
}

/// The client `instance_id` names, read off Amazon EC2's own store.
fn client_of<Eng: Send + Sync + 'static>(engine: &Eng, instance_id: &str) -> Option<DnsClient> {
    let state = ec2::service_state(engine)?;
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

/// The addresses the VPC's OWN resolver answers at: the link-local one, and
/// [`VPC_RESOLVER_OFFSET`] past the base of each IPv4 range the VPC holds.
///
/// NONE for a VPC whose `enableDnsSupport` attribute is off: that attribute is what decides
/// whether the Amazon-provided DNS server resolves queries in the VPC at all
/// (`ModifyVpcAttribute`), and it is read through Amazon EC2's own accessor for it.
fn vpc_resolver_addresses<Eng: Send + Sync + 'static>(
    engine: &Eng,
    vpc_id: &str,
) -> Vec<Ipv4Addr> {
    let Some(state) = ec2::service_state(engine) else {
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

/// Every address an OPERATIONAL inbound Resolver endpoint of `vpc_id` has attached.
fn inbound_endpoint_addresses<Eng: Send + Sync + 'static>(
    engine: &Eng,
    vpc_id: &str,
) -> Vec<EndpointAddress> {
    let Some(state) = super::service_state(engine) else {
        return Vec::new();
    };
    let now = Instant::now();
    let store = state.lock();
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
fn admitted_endpoint_servers<Eng: Send + Sync + 'static>(
    engine: &Eng,
    client: &DnsClient,
    addresses: &[EndpointAddress],
) -> Vec<DnsServer> {
    if addresses.is_empty() {
        return Vec::new();
    }
    let Some(state) = ec2::service_state(engine) else {
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
/// weighted, failover, multivalue or geolocation group, whose members share a name and type and
/// differ by `SetIdentifier`) is answered here from the group's FIRST member in that order,
/// which is the member Route 53's own answer path selects for a group carrying no policy. Which
/// member a policy selects is that path's own decision (`TestDNSAnswer`), and it is not
/// re-decided here.
fn zones_of_vpc<Eng: Send + Sync + 'static>(engine: &Eng, vpc_id: &str) -> Vec<ViewZone> {
    let Some(state) = route53::service_state(engine) else {
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
                "a name under no zone of the VPC",
                &link_local,
                "example.com",
                TYPE_A,
                PROTOCOL_UDP,
                Resolution::Answered(Answer {
                    code: ResponseCode::ServFail,
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
