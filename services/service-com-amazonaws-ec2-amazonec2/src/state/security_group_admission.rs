// SPDX-License-Identifier: CC0-1.0

//! Whether the security groups an interface carries ADMIT one arriving packet.
//!
//! A security group is a stateful filter on an elastic network interface: "security groups
//! act as a firewall for associated instances, controlling both inbound and outbound traffic
//! at the instance level" — an inbound rule names a protocol, a port range and a source, and
//! traffic is admitted when a rule of ANY group on the interface names it. There is no deny
//! rule: "all rules are permissive", so a packet no rule names is simply dropped, which is
//! why a client whose query is not admitted sees nothing come back rather than a refusal.
//!
//! This is the ONE place this crate decides that question, so every service whose resource is
//! reached through an interface of this account — a Resolver endpoint's addresses as much as
//! an instance's — asks it here rather than reading the rules a second time.

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
}
