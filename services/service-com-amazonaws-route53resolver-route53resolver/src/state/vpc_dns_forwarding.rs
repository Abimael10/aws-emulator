// SPDX-License-Identifier: CC0-1.0

//! The RESOLVER RULES in force in a VPC, as the DNS data plane of that VPC applies them to a
//! query asked inside it.
//!
//! # Which rule decides a query
//!
//! A rule is in force in a VPC once an association between the two has SETTLED
//! ([`super::RULE_ASSOCIATION_STATUS_COMPLETE`]): `AssociateResolverRule` "associates a Resolver
//! rule with a VPC … the Resolver uses the rule to route DNS queries", so an association still
//! being created routes nothing yet and one the account removed routes nothing any more.
//!
//! Of the rules in force, the one that decides a query is the one whose `DomainName` is the
//! name's LONGEST whole-label suffix: "if a query matches multiple Resolver rules (example.com
//! and www.example.com), outbound DNS queries are routed using the Resolver rule that contains
//! the most specific domain name" (`ResolverRule$RuleType`). Whether a name is UNDER a domain is
//! Route 53's own label-by-label reading of that question
//! ([`route53::name_is_in_zone`]), so a rule for `example.com` never covers `evil-example.com`.
//! Among rules of equally specific domains the EARLIEST created one decides, so the answer does
//! not depend on the order a store hands its records out in.
//!
//! # What the deciding rule does
//!
//! `FORWARD` and `DELEGATE` send the query AWAY: "when you want to forward DNS queries for
//! specified domain name to resolvers on your network, specify `FORWARD` or `DELEGATE`" — so the
//! query is not answered out of the VPC's own zones at all, and what comes back is what answered
//! at the rule's `TargetIps`. Every other type leaves the query to the VPC's own resolver, which
//! is what `SYSTEM` is for: "when you have a forwarding rule … and you want Resolver to process
//! queries for a subdomain of that domain, specify `SYSTEM`".
//!
//! # Where a sent-away query goes
//!
//! To the addresses the rule names, through the OUTBOUND endpoint it names
//! (`ResolverRule$ResolverEndpointId`: "the ID of the endpoint that the rule is associated
//! with"). The resolvers on a customer's own network are outside this emulator, so the one thing
//! that answers DNS at such an address here is an INBOUND Resolver endpoint of the account —
//! "inbound endpoints … forward DNS queries from your network to Route 53 Resolver" — which
//! answers out of the private hosted zones associated with ITS own VPC. That is what makes a
//! forwarding rule between two VPCs resolve, and what makes a rule pointing at an address
//! nothing answers DNS at come back with no answer at all.
//!
//! A query arriving at an inbound endpoint's address is a packet arriving at an ELASTIC NETWORK
//! INTERFACE, so it is delivered only where a security group of that endpoint admits it from the
//! address it came from — the outbound endpoint's own address, a member of that endpoint's groups
//! — which is Amazon EC2's own decision over its own rules
//! (`security_groups_admit_ingress`, the one reading of that question this tree holds) and not a
//! second copy of it here.
//!
//! The target resolver answers out of its VPC's zones alone: the rules of ITS VPC are not applied
//! to a query that arrives already forwarded, so no chain of rules can send one query around in a
//! circle.
//!
//! LOCK ORDER: every function here is called with NO store guard held, and takes this service's,
//! Amazon EC2's and Route 53's stores in turn, releasing each before the next — the order
//! [`super::vpc_dns`] takes them in.

use std::cmp::Reverse;
use std::net::Ipv4Addr;
use std::time::Instant;

use ::service_com_amazonaws_route53_awsdnsv20130401::state as route53;
use ::service_common::vpc_private_dns::established_not_to_exist;

use super::vpc_dns::{
    Answer, ResponseCode, StoredVpcDns, DIRECTION_INBOUND, DNS_PORT, PROTOCOL_UDP,
};
use super::{
    ENDPOINT_DIRECTION_OUTBOUND, ENDPOINT_STATUS_OPERATIONAL, IP_STATUS_ATTACHED,
    RULE_ASSOCIATION_STATUS_COMPLETE, RULE_TYPE_DELEGATE, RULE_TYPE_FORWARD,
};

/// The port a `TargetIps` entry that names none is reached at, which is the port DNS is served
/// at (`domain`, 53) — the port an inbound Resolver endpoint answers on.
const DEFAULT_TARGET_PORT: u16 = DNS_PORT;

/// One Resolver rule in force in a VPC, as an answer path reads it.
#[derive(Debug, Clone)]
pub struct ViewResolverRule {
    /// Where the rule stands in the store's creation order, which is what breaks a tie between
    /// two equally specific domains.
    pub seq: u64,
    /// The rule's `DomainName`, in the canonical fully qualified form a rule is stored in
    /// ([`super::canonical_domain_name`]).
    pub domain_name: String,
    /// The rule's `RuleType` ([`RULE_TYPE_FORWARD`], [`super::RULE_TYPE_SYSTEM`], …).
    pub rule_type: String,
    /// Where a query this rule sends away goes: one entry per `TargetIps` address.
    pub targets: Vec<ForwardTarget>,
    /// The OUTBOUND endpoint the query leaves through, and `None` for a rule that names none.
    pub endpoint_id: Option<String>,
}

/// One address a rule sends a query to: the address, and the port at it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ForwardTarget {
    /// The address itself.
    pub address: Ipv4Addr,
    /// The port at that address the rule names, or [`DEFAULT_TARGET_PORT`].
    pub port: u16,
}

impl ViewResolverRule {
    /// How many labels this rule's domain carries, which is how specific it is: the rule with the
    /// most labels still enclosing the name is the one that decides a query.
    fn specificity(&self) -> usize {
        self.domain_name
            .trim_end_matches('.')
            .split('.')
            .filter(|label| !label.is_empty())
            .count()
    }

    /// Whether this rule SENDS a query away instead of leaving it to the VPC's own resolver:
    /// the two types the doc of `ResolverRule$RuleType` names together — "when you want to
    /// forward DNS queries for specified domain name to resolvers on your network, specify
    /// `FORWARD` or `DELEGATE`". Every other type leaves the query here, which is what `SYSTEM`
    /// is for.
    fn sends_the_query_away(&self) -> bool {
        self.rule_type.eq_ignore_ascii_case(RULE_TYPE_FORWARD)
            || self.rule_type.eq_ignore_ascii_case(RULE_TYPE_DELEGATE)
    }
}

/// The rule of `rules` that decides a query for the canonical name `asked`: the one whose domain
/// is the name's longest whole-label suffix, and the earliest created of those where two domains
/// are equally specific. `None` when no rule's domain encloses the name.
#[must_use]
pub fn rule_for<'a>(rules: &'a [ViewResolverRule], asked: &str) -> Option<&'a ViewResolverRule> {
    rules
        .iter()
        .filter(|rule| route53::name_is_in_zone(asked, &rule.domain_name))
        .max_by_key(|rule| (rule.specificity(), Reverse(rule.seq)))
}

/// What a query a rule SENT AWAY came back with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForwardedTo {
    /// Nothing answered at any address the rule names, so no reply came back for the query at
    /// all.
    Nothing,
    /// A resolver at one of those addresses answered, with what it holds for the name.
    Resolver(Answer),
}

impl ForwardedTo {
    /// What the CLIENT that asked the query is answered with: the answer the resolver that was
    /// reached gave, and a server failure carrying no record where none was — which is what a
    /// resolver answers for a query its forwarders left unanswered.
    #[must_use]
    pub fn answer(self) -> Answer {
        match self {
            Self::Nothing => Answer {
                code: ResponseCode::ServFail,
                authoritative: false,
                records: Vec::new(),
            },
            Self::Resolver(answered) => answered,
        }
    }
}

/// What the Resolver rules `rules` do with a query for `asked` of `record_type`: what the query
/// came back with once a rule has sent it away, and `None` for a query that is left to the VPC's
/// own resolver — one no rule's domain covers, and one the deciding rule leaves here.
///
/// `stores` is where a sent-away query is CARRIED to: the reading of the account's own records
/// that says what answers DNS at the rule's targets. Without one — a reading taken over no store
/// at all — nothing answers at any target, which is what a rule pointing into a network this
/// emulator cannot reach comes to.
#[must_use]
pub fn forwarded_answer(
    rules: &[ViewResolverRule],
    stores: Option<&StoredVpcDns>,
    asked: &str,
    record_type: &str,
) -> Option<ForwardedTo> {
    let rule = rule_for(rules, asked)?;
    if !rule.sends_the_query_away() {
        return None;
    }
    Some(
        stores
            .and_then(|stores| stores.answered_by_a_target(rule, asked, record_type))
            .map_or(ForwardedTo::Nothing, ForwardedTo::Resolver),
    )
}

/// One address of the account that answers DNS for a FORWARDED query: an attached address of an
/// operational INBOUND Resolver endpoint.
#[derive(Debug, Clone)]
struct InboundServer {
    /// The address the endpoint answers at.
    address: Ipv4Addr,
    /// The VPC the endpoint is in, whose private hosted zones it answers out of.
    vpc_id: String,
    /// The endpoint's security groups, which are the groups on its interfaces.
    groups: Vec<String>,
}

/// Where a sent-away query leaves FROM: the addresses of the rule's outbound endpoint, and the
/// security groups those interfaces carry.
#[derive(Debug, Clone, Default)]
struct ForwardSource {
    /// The addresses of the outbound endpoint, which are the sources of the forwarded query.
    addresses: Vec<String>,
    /// That endpoint's security groups, matched by a rule whose source is a security group.
    groups: Vec<String>,
}

impl StoredVpcDns {
    /// The Resolver rules in force in `vpc_id`: the rules whose association with that VPC has
    /// settled, as of now.
    ///
    /// Call it with NO store guard held.
    #[must_use]
    pub fn rules_of_vpc(&self, vpc_id: &str) -> Vec<ViewResolverRule> {
        let now = Instant::now();
        let store = self.resolver.lock();
        store
            .resolver_rule_associations
            .values()
            .filter(|association| association.vpc_id == vpc_id)
            .filter(|association| {
                association.status_at(now) == RULE_ASSOCIATION_STATUS_COMPLETE
            })
            .filter_map(|association| store.resolver_rules.get(&association.resolver_rule_id))
            .filter_map(|rule| {
                Some(ViewResolverRule {
                    seq: rule.seq,
                    domain_name: rule.domain_name.clone()?,
                    rule_type: rule.rule_type.clone(),
                    targets: rule.target_ips.iter().filter_map(target_of).collect(),
                    endpoint_id: rule.resolver_endpoint_id.clone(),
                })
            })
            .collect()
    }

    /// What the Resolver rules in force in `vpc_id` do with a query for `name` of `record_type`,
    /// read off this plane's own stores ([`forwarded_answer`]).
    ///
    /// Call it with NO store guard held.
    #[must_use]
    pub fn forwarded_query_answer(
        &self,
        vpc_id: &str,
        name: &str,
        record_type: &str,
    ) -> Option<ForwardedTo> {
        let rules = self.rules_of_vpc(vpc_id);
        if rules.is_empty() {
            return None;
        }
        forwarded_answer(
            &rules,
            Some(self),
            &route53::canonical_dns_name(name),
            record_type,
        )
    }

    /// What answered a query `rule` sent away, and `None` when nothing answers DNS at any address
    /// the rule names — an address no inbound Resolver endpoint of the account holds, one at a
    /// port DNS is not served at, and one whose endpoint admits no such query from the outbound
    /// endpoint the rule sends through.
    ///
    /// The first address that answers is the one the query is answered out of, as a resolver
    /// asking its forwarders in turn is answered by the first that replies.
    fn answered_by_a_target(
        &self,
        rule: &ViewResolverRule,
        asked: &str,
        record_type: &str,
    ) -> Option<Answer> {
        if rule.targets.is_empty() {
            return None;
        }
        let source = self.forward_source(rule.endpoint_id.as_deref());
        if source.addresses.is_empty() {
            // The query has no interface to leave the VPC through, so it reaches no target at
            // all: a rule whose outbound endpoint is gone, or has not become operational yet,
            // forwards nothing.
            return None;
        }
        let servers = self.inbound_servers();
        let reached = rule.targets.iter().find_map(|target| {
            let server = servers
                .iter()
                .find(|server| server.address == target.address && target.port == DNS_PORT)?;
            self.admits_forwarded_query(server, &source)
                .then_some(server)
        })?;
        // The resolver that was reached answers out of the private hosted zones of ITS own VPC,
        // and a name none of them holds is one it would recurse for — which nothing of this
        // emulator does, so it comes to the same reading of the public namespace every in-VPC
        // resolver takes.
        let answered = match self.private_zone_answer(&reached.vpc_id, asked, record_type) {
            Some(answered) => answered,
            None => Answer {
                code: if established_not_to_exist(asked) {
                    ResponseCode::NxDomain
                } else {
                    ResponseCode::ServFail
                },
                authoritative: false,
                records: Vec::new(),
            },
        };
        Some(Answer {
            // The answer is another resolver's, not the authority this VPC's resolver holds for
            // its own zones.
            authoritative: false,
            ..answered
        })
    }

    /// Where a query the rule sends away leaves from: the attached addresses of the OUTBOUND
    /// endpoint `endpoint_id` names, with the security groups on its interfaces.
    ///
    /// Empty for a rule that names no endpoint, for an endpoint the store no longer holds, for
    /// one that is not operational yet, and for one that is not an outbound endpoint — none of
    /// which carries a query out of the VPC.
    fn forward_source(&self, endpoint_id: Option<&str>) -> ForwardSource {
        let Some(endpoint_id) = endpoint_id else {
            return ForwardSource::default();
        };
        let now = Instant::now();
        let store = self.resolver.lock();
        let Some(endpoint) = store
            .resolver_endpoints
            .get(endpoint_id)
            .filter(|endpoint| {
                endpoint
                    .direction
                    .eq_ignore_ascii_case(ENDPOINT_DIRECTION_OUTBOUND)
            })
            .filter(|endpoint| endpoint.status_at(now) == ENDPOINT_STATUS_OPERATIONAL)
        else {
            return ForwardSource::default();
        };
        ForwardSource {
            addresses: endpoint
                .ip_addresses
                .iter()
                .filter(|held| held.status_at(now) == IP_STATUS_ATTACHED)
                .filter_map(|held| held.ip.clone())
                .collect(),
            groups: endpoint.security_group_ids.clone(),
        }
    }

    /// Every address of the account an operational INBOUND Resolver endpoint answers DNS at.
    fn inbound_servers(&self) -> Vec<InboundServer> {
        let now = Instant::now();
        let store = self.resolver.lock();
        store
            .resolver_endpoints
            .values()
            .filter(|endpoint| endpoint.direction.eq_ignore_ascii_case(DIRECTION_INBOUND))
            .filter(|endpoint| endpoint.status_at(now) == ENDPOINT_STATUS_OPERATIONAL)
            .flat_map(|endpoint| {
                let vpc_id = endpoint.host_vpc_id.clone().unwrap_or_default();
                endpoint
                    .ip_addresses
                    .iter()
                    .filter(|held| held.status_at(now) == IP_STATUS_ATTACHED)
                    .filter_map(|held| held.ip.as_deref()?.trim().parse::<Ipv4Addr>().ok())
                    .map(|address| InboundServer {
                        address,
                        vpc_id: vpc_id.clone(),
                        groups: endpoint.security_group_ids.clone(),
                    })
                    .collect::<Vec<InboundServer>>()
            })
            .filter(|server| !server.vpc_id.is_empty())
            .collect()
    }

    /// Whether a security group of `server` admits the forwarded query from `source`: a datagram
    /// query at the port DNS is served at, from an address of the outbound endpoint, which is a
    /// member of that endpoint's own groups.
    ///
    /// Amazon EC2's own decision over its own rules; an engine that vends no Amazon EC2 store
    /// holds no security group, and a filter this emulator has no rules of admits nothing.
    fn admits_forwarded_query(&self, server: &InboundServer, source: &ForwardSource) -> bool {
        let Some(state) = self.ec2.as_ref() else {
            return false;
        };
        let store = state.lock();
        source.addresses.iter().any(|address| {
            store.security_groups_admit_ingress(
                &server.groups,
                PROTOCOL_UDP,
                DNS_PORT,
                address,
                &source.groups,
            )
        })
    }
}

/// One stored `TargetAddress` as an address a query is sent to, and `None` for one whose `Ip` is
/// no IPv4 address this plane can reach — an entry naming only an IPv6 address among them, which
/// nothing of this emulator answers DNS at.
fn target_of(target: &super::TargetAddress) -> Option<ForwardTarget> {
    let address = target.ip.as_deref()?.trim().parse::<Ipv4Addr>().ok()?;
    let port = target
        .port
        .and_then(|port| u16::try_from(port).ok())
        .unwrap_or(DEFAULT_TARGET_PORT);
    Some(ForwardTarget { address, port })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A rule of `domain_name` and `rule_type`, for the rows below.
    fn rule(seq: u64, domain_name: &str, rule_type: &str) -> ViewResolverRule {
        ViewResolverRule {
            seq,
            domain_name: super::super::canonical_domain_name(domain_name),
            rule_type: rule_type.to_string(),
            targets: vec![ForwardTarget {
                address: "10.0.1.250".parse().expect("an address"),
                port: DNS_PORT,
            }],
            endpoint_id: Some("rslvr-out-1".to_string()),
        }
    }

    /// RULE: the rule that decides a query is the one whose domain is the name's longest
    /// whole-label suffix, and a query it sends away is answered out of no zone of the VPC.
    #[test]
    fn the_most_specific_rule_enclosing_the_name_decides_the_query() {
        let rules = vec![
            rule(1, "example.internal", RULE_TYPE_FORWARD),
            rule(2, "inner.example.internal", super::super::RULE_TYPE_SYSTEM),
            rule(3, "deep.inner.example.internal", RULE_TYPE_FORWARD),
        ];
        // (case, the name asked for, the domain of the rule that must decide it, whether the
        // query is sent away)
        let rows: &[(&str, &str, Option<&str>, bool)] = &[
            (
                "a name under the forwarding rule's domain",
                "host.example.internal.",
                Some("example.internal."),
                true,
            ),
            (
                "the forwarding rule's domain itself",
                "example.internal.",
                Some("example.internal."),
                true,
            ),
            (
                "a name under the more specific SYSTEM rule",
                "host.inner.example.internal.",
                Some("inner.example.internal."),
                false,
            ),
            (
                "a name under a forwarding rule below that one",
                "host.deep.inner.example.internal.",
                Some("deep.inner.example.internal."),
                true,
            ),
            (
                "a name whose label only looks like the domain's",
                "host.evil-example.internal.",
                None,
                false,
            ),
            ("a name under no rule's domain", "aws.amazon.com.", None, false),
        ];
        for (case, asked, domain, sent_away) in rows {
            let decided = rule_for(&rules, asked);
            assert_eq!(
                decided.map(|rule| rule.domain_name.as_str()),
                *domain,
                "case {case:?}: expected the rule for {domain:?} to decide {asked}, got \
                 {decided:?}"
            );
            // No store is read here, so a query that IS sent away comes back unanswered, and one
            // that is not is left to the zones.
            let answered = forwarded_answer(&rules, None, asked, "A");
            assert_eq!(
                answered.is_some(),
                *sent_away,
                "case {case:?}: expected sent away to be {sent_away}, got {answered:?}"
            );
            if let Some(answered) = answered {
                assert_eq!(
                    answered,
                    ForwardedTo::Nothing,
                    "case {case:?}: expected no answer from an unreachable target, got \
                     {answered:?}"
                );
                let served = answered.answer();
                assert_eq!(
                    served.code,
                    ResponseCode::ServFail,
                    "case {case:?}: expected the client to be answered with a server failure, \
                     got {served:?}"
                );
                assert!(
                    served.records.is_empty(),
                    "case {case:?}: expected no record from an unreachable target, got {served:?}"
                );
            }
        }
    }

    /// RULE: a rule whose type leaves the query to the VPC's own Resolver sends nothing away,
    /// whichever of the two forwarding types the rules beside it carry.
    #[test]
    fn only_a_forwarding_rule_type_sends_a_query_away() {
        // (case, the rule type, whether a query the rule decides is sent away)
        let rows: &[(&str, &str, bool)] = &[
            ("the forwarding type", RULE_TYPE_FORWARD, true),
            ("the delegating type", RULE_TYPE_DELEGATE, true),
            ("the type that leaves the query here", super::super::RULE_TYPE_SYSTEM, false),
            ("the recursive type", super::super::RULE_TYPE_RECURSIVE, false),
            ("the forwarding type spelled in lower case", "forward", true),
        ];
        for (case, rule_type, sent_away) in rows {
            let rules = vec![rule(1, "example.internal", rule_type)];
            let answered = forwarded_answer(&rules, None, "host.example.internal.", "A");
            assert_eq!(
                answered.is_some(),
                *sent_away,
                "case {case:?}: expected sent away to be {sent_away}, got {answered:?}"
            );
        }
    }

    /// RULE: a `TargetIps` entry is read as the address it names at the port it names, and the
    /// port DNS is served at where it names none.
    #[test]
    fn a_target_address_carries_the_port_the_rule_names() {
        // (case, the stored target, what it is read as)
        let rows: &[(&str, super::super::TargetAddress, Option<ForwardTarget>)] = &[
            (
                "an address with no port",
                target_address(Some("10.0.1.250"), None),
                Some(ForwardTarget {
                    address: "10.0.1.250".parse().expect("an address"),
                    port: DNS_PORT,
                }),
            ),
            (
                "an address with a port of its own",
                target_address(Some("10.0.1.250"), Some(5353)),
                Some(ForwardTarget {
                    address: "10.0.1.250".parse().expect("an address"),
                    port: 5353,
                }),
            ),
            ("an entry naming no IPv4 address", target_address(None, Some(53)), None),
            (
                "an entry whose address is no address",
                target_address(Some("not-an-address"), None),
                None,
            ),
        ];
        for (case, stored, expected) in rows {
            let read = target_of(stored);
            assert_eq!(
                &read, expected,
                "case {case:?}: expected {expected:?}, got {read:?}"
            );
        }
    }

    /// A stored `TargetAddress` naming `ip` at `port`.
    fn target_address(ip: Option<&str>, port: Option<i32>) -> super::super::TargetAddress {
        super::super::TargetAddress {
            ip: ip.map(str::to_string),
            port,
            ipv6: None,
            protocol: None,
            server_name_indication: None,
        }
    }
}
