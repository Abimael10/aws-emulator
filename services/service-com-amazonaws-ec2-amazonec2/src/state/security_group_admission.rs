// SPDX-License-Identifier: CC0-1.0

//! Whether the security groups an interface carries ADMIT one packet — arriving at it, or
//! leaving it.
//!
//! A security group is a stateful filter on an elastic network interface: "security groups
//! act as a firewall for associated instances, controlling both inbound and outbound traffic
//! at the instance level" — an inbound rule names a protocol, a port range and a source, an
//! outbound rule names a protocol, a port range and a destination, and traffic passes when a
//! rule of ANY group on the interface names it. There is no deny
//! rule: "all rules are permissive", so a packet no rule names is simply dropped, which is
//! why a client whose query is not admitted sees nothing come back rather than a refusal.
//!
//! This is the ONE place this crate decides that question, so every service whose resource is
//! reached through an interface of this account — a Resolver endpoint's addresses as much as
//! an instance's, and the agent inside an instance's guest that calls a service endpoint —
//! asks it here rather than reading the rules a second time.

use super::{cidr_block_contains_address, ComAmazonawsEc2Amazonec2State};
use crate::ops::types::{IpPermission, SecurityGroup};

/// The `IpProtocol` value that stands for EVERY protocol: "use `-1` to specify all
/// protocols" (`IpPermission$IpProtocol`), which also admits every port, since a rule on all
/// protocols carries no port range.
pub const ALL_PROTOCOLS: &str = "-1";

/// The spelling of that same rule the service reports for a group rule written with it
/// (`SecurityGroupRule$IpProtocol`): "if the protocol is `-1` … the rule applies to all
/// protocols".
pub const ALL_PROTOCOLS_WORD: &str = "all";

impl ComAmazonawsEc2Amazonec2State {
    /// Whether any group of `group_ids` admits a packet of `protocol` arriving at `port` from
    /// `source_ip`, which is a member of the groups `source_groups` names.
    ///
    /// `protocol` is the IANA keyword a rule states (`tcp`, `udp`, `icmp`) and `source_groups`
    /// are the security groups of the interface the packet came FROM, so a rule whose source
    /// is another security group ("the source is the security group itself") admits a packet
    /// from a member of it. A group id this store holds no group for admits nothing: a filter
    /// this emulator has no rules of is not a filter that lets everything through.
    pub fn security_groups_admit_ingress(
        &self,
        group_ids: &[String],
        protocol: &str,
        port: u16,
        source_ip: &str,
        source_groups: &[String],
    ) -> bool {
        group_ids.iter().any(|group_id| {
            self.security_groups
                .get(group_id)
                .is_some_and(|group| group_admits_ingress(group, protocol, port, source_ip, source_groups))
        })
    }

    /// Whether any group of `group_ids` lets a packet of `protocol` to `port` LEAVE the
    /// interface for an address beyond this account's own network — the shape of a call an
    /// agent inside a guest makes to a service endpoint.
    ///
    /// A security group filters both directions — "controlling both inbound and outbound
    /// traffic at the instance level" — and an outbound rule names a protocol, a port range
    /// and a DESTINATION the same way an inbound one names a source, so the packet leaves
    /// when one outbound rule of any group on the interface names it
    /// ([`group_admits_outbound`]). A group id this store holds no group for lets nothing
    /// out, for the reason the inbound reading gives.
    pub fn security_groups_admit_outbound(
        &self,
        group_ids: &[String],
        protocol: &str,
        port: u16,
    ) -> bool {
        group_ids.iter().any(|group_id| {
            self.security_groups
                .get(group_id)
                .is_some_and(|group| group_admits_outbound(group, protocol, port))
        })
    }
}

/// Whether one of `group`'s own OUTBOUND rules lets a packet of `protocol` to `port` leave
/// for an address beyond this account's own network.
///
/// The destination is not compared against an address, because the endpoint such a packet is
/// bound for is outside every network this store holds and so has no address here: what the
/// rule has to name is the protocol, the port and a destination of its own — the all-traffic
/// rule a new group carries names `0.0.0.0/0`, and a rule naming none names no traffic at
/// all, exactly as an inbound rule without a source does.
#[must_use]
pub fn group_admits_outbound(group: &SecurityGroup, protocol: &str, port: u16) -> bool {
    group
        .ip_permissions_egress
        .as_deref()
        .unwrap_or_default()
        .iter()
        .any(|permission| {
            protocol_admits(permission.ip_protocol.as_deref(), protocol)
                && port_admits(permission, port)
                && names_a_peer(permission)
        })
}

/// Whether a rule names a peer at all: one of the four kinds of source — or destination —
/// an [`IpPermission`] can carry ("a CIDR range, security group, or prefix list",
/// `RevokeSecurityGroupIngress`, and the IPv6 ranges beside the IPv4 ones).
fn names_a_peer(permission: &IpPermission) -> bool {
    let ranges = permission
        .ip_ranges
        .as_deref()
        .unwrap_or_default()
        .iter()
        .any(|range| range.cidr_ip.as_deref().is_some_and(|cidr| !cidr.is_empty()));
    let ipv6 = permission
        .ipv6_ranges
        .as_deref()
        .unwrap_or_default()
        .iter()
        .any(|range| {
            range
                .cidr_ipv6
                .as_deref()
                .is_some_and(|cidr| !cidr.is_empty())
        });
    let prefix_lists = !permission
        .prefix_list_ids
        .as_deref()
        .unwrap_or_default()
        .is_empty();
    let groups = !permission
        .user_id_group_pairs
        .as_deref()
        .unwrap_or_default()
        .is_empty();
    ranges || ipv6 || prefix_lists || groups
}

/// Whether `group`'s own inbound rules admit that packet.
#[must_use]
pub fn group_admits_ingress(
    group: &SecurityGroup,
    protocol: &str,
    port: u16,
    source_ip: &str,
    source_groups: &[String],
) -> bool {
    group
        .ip_permissions
        .as_deref()
        .unwrap_or_default()
        .iter()
        .any(|permission| {
            permission_admits(permission, protocol, port, source_ip, source_groups)
        })
}

/// Whether ONE inbound rule names that packet: its protocol, its port and its source all
/// have to be named by the same rule, which is what makes a rule a grant of exactly one thing.
#[must_use]
pub fn permission_admits(
    permission: &IpPermission,
    protocol: &str,
    port: u16,
    source_ip: &str,
    source_groups: &[String],
) -> bool {
    protocol_admits(permission.ip_protocol.as_deref(), protocol)
        && port_admits(permission, port)
        && source_admits(permission, source_ip, source_groups)
}

/// Whether a rule written for `stated` applies to a packet of `protocol`: the same protocol,
/// or the rule that applies to all of them.
fn protocol_admits(stated: Option<&str>, protocol: &str) -> bool {
    let Some(stated) = stated else {
        // A rule that names no protocol names no traffic: the member is what a rule grants on.
        return false;
    };
    let stated = stated.trim();
    stated == ALL_PROTOCOLS
        || stated.eq_ignore_ascii_case(ALL_PROTOCOLS_WORD)
        || stated.eq_ignore_ascii_case(protocol)
}

/// Whether the rule's port range covers `port`.
///
/// A rule on a protocol that has no ports — and the rule on ALL protocols — carries no range
/// and applies to every port; a rule that states one applies "from" through "to" inclusive
/// (`IpPermission$FromPort`, `IpPermission$ToPort`). The sentinel the store admits a rule
/// under is honoured as well: [`super::ALL_PORTS`] on either end is the value that stands for
/// "all values" there, so a rule stored with it applies to every port rather than to a range
/// no port is in.
fn port_admits(permission: &IpPermission, port: u16) -> bool {
    match (permission.from_port, permission.to_port) {
        (Some(from), Some(to)) if from != super::ALL_PORTS && to != super::ALL_PORTS => {
            let port = i32::from(port);
            from <= port && port <= to
        }
        _ => true,
    }
}

/// Whether the rule's SOURCE names the sender: an address range that holds it, or a security
/// group the sender is a member of.
fn source_admits(permission: &IpPermission, source_ip: &str, source_groups: &[String]) -> bool {
    let in_range = permission
        .ip_ranges
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter_map(|range| range.cidr_ip.as_deref())
        .any(|block| cidr_block_contains_address(block, source_ip));
    if in_range {
        return true;
    }
    permission
        .ipv6_ranges
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter_map(|range| range.cidr_ipv6.as_deref())
        .any(|block| cidr_block_contains_address(block, source_ip))
        || permission
            .user_id_group_pairs
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter_map(|pair| pair.group_id.as_deref())
            .any(|group_id| source_groups.iter().any(|held| held == group_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::types::{IpRange, UserIdGroupPair};

    /// One rule, as a group holds it.
    fn permission(
        protocol: &str,
        ports: Option<(i32, i32)>,
        cidr: Option<&str>,
        group: Option<&str>,
    ) -> IpPermission {
        IpPermission {
            ip_protocol: Some(protocol.to_string()),
            from_port: ports.map(|(from, _)| from),
            to_port: ports.map(|(_, to)| to),
            ip_ranges: cidr.map(|cidr| {
                vec![IpRange {
                    cidr_ip: Some(cidr.to_string()),
                    ..IpRange::default()
                }]
            }),
            user_id_group_pairs: group.map(|group| {
                vec![UserIdGroupPair {
                    group_id: Some(group.to_string()),
                    ..UserIdGroupPair::default()
                }]
            }),
            ..IpPermission::default()
        }
    }

    /// RULE: a packet is admitted when one rule of the group names its protocol, its port and
    /// its source — and dropped otherwise.
    ///
    /// The rows span what the rule generalises over: the protocol (the one named, another one,
    /// every protocol), the port (inside the range, each bound, one past each bound), and the
    /// source (a range that holds the address, one that does not, a group the sender is in, a
    /// group it is not in).
    #[test]
    fn one_rule_naming_the_protocol_the_port_and_the_source_admits_the_packet() {
        let rows: &[(&str, IpPermission, &str, u16, &str, bool)] = &[
            (
                "the protocol and port the rule names",
                permission("udp", Some((53, 53)), Some("10.91.0.0/16"), None),
                "udp",
                53,
                "10.91.1.7",
                true,
            ),
            (
                "another protocol on the same port",
                permission("udp", Some((53, 53)), Some("10.91.0.0/16"), None),
                "tcp",
                53,
                "10.91.1.7",
                false,
            ),
            (
                "a rule on every protocol",
                permission("-1", None, Some("10.91.0.0/16"), None),
                "udp",
                53,
                "10.91.1.7",
                true,
            ),
            (
                "the low bound of the range",
                permission("tcp", Some((53, 100)), Some("0.0.0.0/0"), None),
                "tcp",
                53,
                "10.91.1.7",
                true,
            ),
            (
                "the high bound of the range",
                permission("tcp", Some((53, 100)), Some("0.0.0.0/0"), None),
                "tcp",
                100,
                "10.91.1.7",
                true,
            ),
            (
                "one port below the range",
                permission("tcp", Some((53, 100)), Some("0.0.0.0/0"), None),
                "tcp",
                52,
                "10.91.1.7",
                false,
            ),
            (
                "one port above the range",
                permission("tcp", Some((53, 100)), Some("0.0.0.0/0"), None),
                "tcp",
                101,
                "10.91.1.7",
                false,
            ),
            (
                "an address outside the range",
                permission("udp", Some((53, 53)), Some("10.91.0.0/16"), None),
                "udp",
                53,
                "10.92.1.7",
                false,
            ),
            (
                "a source group the sender is a member of",
                permission("udp", Some((53, 53)), None, Some("sg-00000000000000001")),
                "udp",
                53,
                "10.92.1.7",
                true,
            ),
            (
                "a source group the sender is not a member of",
                permission("udp", Some((53, 53)), None, Some("sg-00000000000000009")),
                "udp",
                53,
                "10.92.1.7",
                false,
            ),
            // The sentinel the store admits a rule under: `-1` on either end stands for every
            // value there, so such a rule admits every port.
            (
                "a rule whose range is the all-values sentinel",
                permission("udp", Some((-1, -1)), Some("0.0.0.0/0"), None),
                "udp",
                53,
                "10.91.1.7",
                true,
            ),
        ];
        let source_groups = vec!["sg-00000000000000001".to_string()];
        for (case, rule, protocol, port, source, expected) in rows {
            let group = SecurityGroup {
                ip_permissions: Some(vec![rule.clone()]),
                ..SecurityGroup::default()
            };
            let admitted =
                group_admits_ingress(&group, protocol, *port, source, &source_groups);
            assert_eq!(
                admitted, *expected,
                "case {case:?}: expected admitted={expected}, got {admitted}"
            );
        }
        // A group with no rules at all admits nothing, which is what a new group is.
        let empty = SecurityGroup::default();
        assert!(
            !group_admits_ingress(&empty, "udp", 53, "10.91.1.7", &source_groups),
            "case \"a group carrying no rule\": expected the packet to be dropped"
        );
    }

    /// RULE: a packet LEAVES when one outbound rule of the group names its protocol, its port
    /// and a destination — and is dropped otherwise; a group's inbound rules say nothing
    /// about what leaves it.
    ///
    /// The rows span what the rule generalises over: the protocol (the one named, another
    /// one, every protocol), the port (inside the range, each bound, one past each bound),
    /// the kind of destination (an IPv4 range, an IPv6 range, a prefix list, another group,
    /// none at all) and the direction the rule was written in.
    #[test]
    fn one_outbound_rule_naming_the_protocol_the_port_and_a_destination_lets_the_packet_out() {
        // One row: what it is, the group's outbound rules, the protocol and port of the
        // packet, and whether it leaves.
        let rows: &[(&str, Vec<IpPermission>, &str, u16, bool)] = &[
            (
                "the all-traffic rule a new group carries",
                vec![permission("-1", None, Some("0.0.0.0/0"), None)],
                "tcp",
                443,
                true,
            ),
            (
                "the protocol and port the rule names",
                vec![permission("tcp", Some((443, 443)), Some("0.0.0.0/0"), None)],
                "tcp",
                443,
                true,
            ),
            (
                "another protocol on the same port",
                vec![permission("udp", Some((443, 443)), Some("0.0.0.0/0"), None)],
                "tcp",
                443,
                false,
            ),
            (
                "the low bound of the range",
                vec![permission("tcp", Some((443, 465)), Some("0.0.0.0/0"), None)],
                "tcp",
                443,
                true,
            ),
            (
                "the high bound of the range",
                vec![permission("tcp", Some((443, 465)), Some("0.0.0.0/0"), None)],
                "tcp",
                465,
                true,
            ),
            (
                "one port below the range",
                vec![permission("tcp", Some((443, 465)), Some("0.0.0.0/0"), None)],
                "tcp",
                442,
                false,
            ),
            (
                "one port above the range",
                vec![permission("tcp", Some((443, 465)), Some("0.0.0.0/0"), None)],
                "tcp",
                466,
                false,
            ),
            (
                "a rule on another port only",
                vec![permission("tcp", Some((80, 80)), Some("0.0.0.0/0"), None)],
                "tcp",
                443,
                false,
            ),
            (
                "a destination naming another security group",
                vec![permission(
                    "tcp",
                    Some((443, 443)),
                    None,
                    Some("sg-00000000000000001"),
                )],
                "tcp",
                443,
                true,
            ),
            (
                "a rule naming no destination at all",
                vec![permission("tcp", Some((443, 443)), None, None)],
                "tcp",
                443,
                false,
            ),
            (
                "two rules, the second of which names the packet",
                vec![
                    permission("tcp", Some((80, 80)), Some("0.0.0.0/0"), None),
                    permission("tcp", Some((443, 443)), Some("10.0.0.0/8"), None),
                ],
                "tcp",
                443,
                true,
            ),
            ("no outbound rule at all", Vec::new(), "tcp", 443, false),
        ];
        for (case, rules, protocol, port, expected) in rows {
            let group = SecurityGroup {
                ip_permissions_egress: Some(rules.clone()),
                ..SecurityGroup::default()
            };
            let left = group_admits_outbound(&group, protocol, *port);
            assert_eq!(
                left, *expected,
                "case {case:?}: expected admitted={expected}, got {left}"
            );
        }
        // An INBOUND rule for the same protocol and port says nothing about what leaves.
        let inbound_only = SecurityGroup {
            ip_permissions: Some(vec![permission(
                "tcp",
                Some((443, 443)),
                Some("0.0.0.0/0"),
                None,
            )]),
            ..SecurityGroup::default()
        };
        assert!(
            !group_admits_outbound(&inbound_only, "tcp", 443),
            "case \"an inbound rule only\": expected the packet not to leave"
        );
        // An IPv6 range and a prefix list are destinations too.
        for (case, permission) in [
            (
                "an IPv6 destination range",
                IpPermission {
                    ip_protocol: Some("tcp".to_string()),
                    from_port: Some(443),
                    to_port: Some(443),
                    ipv6_ranges: Some(vec![crate::ops::types::Ipv6Range {
                        cidr_ipv6: Some("::/0".to_string()),
                        ..crate::ops::types::Ipv6Range::default()
                    }]),
                    ..IpPermission::default()
                },
            ),
            (
                "a prefix-list destination",
                IpPermission {
                    ip_protocol: Some("tcp".to_string()),
                    from_port: Some(443),
                    to_port: Some(443),
                    prefix_list_ids: Some(vec![crate::ops::types::PrefixListId {
                        prefix_list_id: Some("pl-00000000000000001".to_string()),
                        ..crate::ops::types::PrefixListId::default()
                    }]),
                    ..IpPermission::default()
                },
            ),
        ] {
            let group = SecurityGroup {
                ip_permissions_egress: Some(vec![permission]),
                ..SecurityGroup::default()
            };
            assert!(
                group_admits_outbound(&group, "tcp", 443),
                "case {case:?}: expected the packet to leave"
            );
        }
    }
}
