// SPDX-License-Identifier: CC0-1.0

//! WHO reaches a task at the address of its own elastic network interface.
//!
//! # The rule this module implements
//!
//! A task that uses the `awsvpc` network mode "is allocated its own elastic network interface"
//! (`RegisterTaskDefinition$networkMode`) in the subnet its launch named, "with the launch's
//! security groups" (`AwsVpcConfiguration$securityGroups`) — and a security group is a filter
//! on that interface in BOTH directions: an inbound rule names a protocol, a port range and a
//! source, an outbound rule names a protocol, a port range and a destination, "all rules are
//! permissive", and traffic no rule of any group on the interface names is simply DROPPED. So
//! the workload at one address reaches the workload at another exactly when an outbound rule of
//! the SENDER's own groups lets the packet out and a group on the DESTINATION's interface names
//! that packet, and nothing comes back for one neither does: a dropped packet is not refused,
//! it is unanswered.
//!
//! That is what makes the groups the control over who reaches a task: revoking the rule that
//! named one client stops that client while a client another rule still names keeps getting
//! through, and authorizing it again lets it back in — with nothing of the task itself changed.
//!
//! # Where the decision comes from
//!
//! Both halves are Amazon VPC's own records, read in Amazon EC2's store in the shared engine:
//! the interface holding an address and the groups on it, and the rules those groups hold. The
//! rules themselves are read through the ONE place that crate decides each question
//! ([`ComAmazonawsEc2Amazonec2State::security_groups_admit_ingress`] and
//! [`ComAmazonawsEc2Amazonec2State::security_groups_admit_outbound`]), so this module holds no
//! second reading of what a rule admits — what is its own is only that the packet is a
//! container's request to another workload's address.
//!
//! An address no interface of the account holds is governed by no group of it, so no rule can
//! be read for it either way ([`Reach::OffNetwork`]): that is a destination this emulator holds
//! no network for, which the caller reports as a request it did not make rather than as one
//! that was dropped.
//!
//! LOCK ORDER: the Amazon EC2 store's guard is taken and RELEASED inside each call here, and no
//! guard of this service's store is held across one — the same contract every other read of
//! that store from this crate has ([`super::awsvpc_interfaces`]).

use ::service_com_amazonaws_ec2_amazonec2::state as ec2;
use ::service_com_amazonaws_ec2_amazonec2::ComAmazonawsEc2Amazonec2State;

/// The protocol a request a container's HTTP client makes is carried over, as a security group
/// rule states one (`IpPermission$IpProtocol`: "tcp | udp | icmp | icmpv6").
pub const TCP_PROTOCOL: &str = "tcp";

/// What the account's own network does to one packet addressed at another workload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// An outbound rule of the sender's own groups lets the packet out and a rule of a group on
    /// the destination's interface names it: it arrives.
    Admitted,
    /// Every rule of every group on either interface is read and none names it, so it is
    /// dropped and nothing comes back.
    Dropped,
    /// No interface of this account holds that address, so no security group of it governs
    /// what reaches there: a destination on no network this emulator holds.
    OffNetwork,
}

/// The account's own network, as the question "does this packet arrive?" is asked of it.
///
/// A handle on the Amazon EC2 store in the SAME shared engine this service's own store lives in
/// — clone-cheap and `'static`, so a container's run can keep one beside the session it
/// performs, which outlives the request that launched the task.
#[derive(Clone)]
pub struct TaskNetwork(::engine::ServiceState<ComAmazonawsEc2Amazonec2State>);

/// Written by hand rather than derived: the store behind the handle carries no `Debug`, and
/// what is diagnosable about this handle is that it is one — never an account's interfaces or
/// the addresses its workloads hold in a log line.
impl ::std::fmt::Debug for TaskNetwork {
    fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        formatter.debug_struct("TaskNetwork").finish()
    }
}

impl TaskNetwork {
    /// The network of the SAME shared engine this service's store lives in, or `None` for an
    /// engine that vends none — on which no address resolves to an interface at all.
    #[must_use]
    pub fn of<Eng: Send + Sync + 'static>(engine: &Eng) -> Option<Self> {
        ec2::service_state(engine).map(Self)
    }

    /// What this network does to a TCP packet sent from `source` to `port` of `destination`:
    /// out through the sender's own groups, then in through the destination's.
    ///
    /// A READ: deciding whether a packet arrives moves no record of either workload and
    /// nothing of the network. The guard is released before the answer is returned, so no
    /// caller holds it while the server at the destination answers.
    #[must_use]
    pub fn reach(&self, source: &str, destination: &str, port: i32) -> Reach {
        let Ok(port) = u16::try_from(port) else {
            // No rule can name a port outside the range a port is in, so nothing admits it.
            return Reach::Dropped;
        };
        let store = self.0.lock();
        let Some(destination_groups) = interface_group_ids(&store, destination) else {
            return Reach::OffNetwork;
        };
        // The groups the SENDER's own interface carries, which decide two things: a rule whose
        // source is "the security group itself" names a member of one of them, and the packet has
        // to LEAVE that interface in the first place. An address this account holds no interface
        // for carries no groups, so it is a member of none and nothing of it filters what leaves.
        let sender = interface_group_ids(&store, source);
        let source_groups = sender.clone().unwrap_or_default();
        // A security group filters BOTH directions, so a packet no outbound rule of the sender's
        // own groups names never reaches the destination's filter at all — read through the same
        // crate's one reading of that half
        // ([`ComAmazonawsEc2Amazonec2State::security_groups_admit_outbound`]).
        if sender.is_some_and(|groups| {
            !store.security_groups_admit_outbound(&groups, TCP_PROTOCOL, port)
        }) {
            return Reach::Dropped;
        }
        if store.security_groups_admit_ingress(
            &destination_groups,
            TCP_PROTOCOL,
            port,
            source,
            &source_groups,
        ) {
            Reach::Admitted
        } else {
            Reach::Dropped
        }
    }
}

/// The `GroupId` of each security group on the interface of this account that holds `address`
/// as a private IPv4 address — `None` when no interface of it holds that address.
///
/// The interface is found through that store's own reader of which interface holds an address
/// ([`ComAmazonawsEc2Amazonec2State::interface_holding_private_ip`]), so an address a task's
/// attachment reports and an address Amazon EC2 reports for its interface resolve to the one
/// record.
fn interface_group_ids(
    store: &ComAmazonawsEc2Amazonec2State,
    address: &str,
) -> Option<Vec<String>> {
    let (network_interface_id, _) = store.interface_holding_private_ip(address)?;
    Some(
        store
            .network_interfaces
            .get(&network_interface_id)?
            .groups
            .iter()
            .flatten()
            .filter_map(|group| group.group_id.clone())
            .collect(),
    )
}

/// The networks the unit tests of this crate read a reach against: two workloads of one
/// account, each on its own interface, with the destination's security group carrying the
/// rules one case is about.
///
/// Built through the Amazon EC2 store's OWN writers, so what is read back is what that store
/// holds for a group and an interface rather than a record shaped to the answer. Shared with
/// [`super::container_http_client`], whose client reads a reach through this same module: one
/// network, so the two cannot be tested against different accounts.
#[cfg(test)]
pub(crate) mod fixtures {
    use ::service_com_amazonaws_ec2_amazonec2::ops::types::{
        GroupIdentifier, IpPermission, IpRange, UserIdGroupPair,
    };
    use ::service_com_amazonaws_ec2_amazonec2::state::{
        allow_all_egress, NetworkInterfaceSpec, SecurityGroupSpec,
    };

    use super::ec2;

    /// The VPC and the subnet range the two workloads below sit in.
    pub(crate) const VPC_CIDR: &str = "10.70.0.0/16";
    pub(crate) const SUBNET_CIDR: &str = "10.70.1.0/24";
    const REGION: &str = "us-west-2";

    /// The addresses those two workloads hold.
    pub(crate) const CLIENT_ADDRESS: &str = "10.70.1.11";
    pub(crate) const SERVER_ADDRESS: &str = "10.70.1.12";

    /// The group id a case writes in a rule whose source is the CLIENT's own group, replaced
    /// by the id the store minted for that group ([`two_workloads`]).
    pub(crate) const SOURCE_GROUP_PLACEHOLDER: &str = "sg-the-clients-own-group";

    /// One inbound rule, as a group holds it.
    pub(crate) fn permission(
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

    /// An account holding two workloads: a VPC, a group for the client, a group for the server
    /// carrying `rules`, and an interface for each of them at its own address.
    ///
    /// Each group carries the one documented allow-all OUTBOUND rule a created group comes with
    /// (`::service_com_amazonaws_ec2_amazonec2::state::allow_all_egress`, which that service's
    /// own `CreateSecurityGroup` writes), so what a case varies is the destination's INBOUND
    /// rules alone. `with_client_group` leaves the client's interface carrying NO group, which
    /// is the case a rule naming a source group has nothing to name.
    pub(crate) fn two_workloads(
        rules: Vec<IpPermission>,
        with_client_group: bool,
    ) -> ::engine::InMemoryEngine {
        two_workloads_with_client_egress(rules, with_client_group, vec![allow_all_egress()])
    }

    /// The same account, with the CLIENT's group carrying `client_egress` as its outbound rules
    /// — the case a client's own egress decides.
    pub(crate) fn two_workloads_with_client_egress(
        rules: Vec<IpPermission>,
        with_client_group: bool,
        client_egress: Vec<IpPermission>,
    ) -> ::engine::InMemoryEngine {
        let engine = ::engine::InMemoryEngine::new();
        let state = ec2::service_state(&engine).expect("the engine vends the Amazon EC2 store");
        let mut store = state.lock();
        // The tenancy and state a VPC of the account carries, as that store's own writer takes
        // them.
        let vpc_id = store.insert_vpc(VPC_CIDR, false, "default", "available", REGION, Vec::new());
        let client_group = store.insert_security_group(SecurityGroupSpec {
            vpc_id: vpc_id.clone(),
            group_name: "client".to_string(),
            description: "the client's group".to_string(),
            region: REGION.to_string(),
            tags: Vec::new(),
            ip_permissions: Vec::new(),
            ip_permissions_egress: client_egress,
        });
        // A rule the case wrote against the CLIENT's own group names the group the store just
        // minted for it, so the row's rule is about the client that is really there.
        let rules: Vec<IpPermission> = rules
            .into_iter()
            .map(|mut rule| {
                for pair in rule.user_id_group_pairs.iter_mut().flatten() {
                    if pair.group_id.as_deref() == Some(SOURCE_GROUP_PLACEHOLDER) {
                        pair.group_id = Some(client_group.clone());
                    }
                }
                rule
            })
            .collect();
        let server_group = store.insert_security_group(SecurityGroupSpec {
            vpc_id: vpc_id.clone(),
            group_name: "server".to_string(),
            description: "the server's group".to_string(),
            region: REGION.to_string(),
            tags: Vec::new(),
            ip_permissions: rules,
            ip_permissions_egress: vec![allow_all_egress()],
        });
        store.insert_network_interface(NetworkInterfaceSpec {
            vpc_id: vpc_id.clone(),
            region: REGION.to_string(),
            private_ip_addresses: vec![CLIENT_ADDRESS.to_string()],
            groups: if with_client_group {
                vec![group_identifier(&client_group)]
            } else {
                Vec::new()
            },
            ..NetworkInterfaceSpec::default()
        });
        store.insert_network_interface(NetworkInterfaceSpec {
            vpc_id,
            region: REGION.to_string(),
            private_ip_addresses: vec![SERVER_ADDRESS.to_string()],
            groups: vec![group_identifier(&server_group)],
            ..NetworkInterfaceSpec::default()
        });
        drop(store);
        engine
    }

    /// One group as an interface's record carries it.
    fn group_identifier(group_id: &str) -> GroupIdentifier {
        GroupIdentifier {
            group_id: Some(group_id.to_string()),
            ..GroupIdentifier::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use ::service_com_amazonaws_ec2_amazonec2::ops::types::IpPermission;

    use super::fixtures::{
        permission, two_workloads, CLIENT_ADDRESS, SERVER_ADDRESS, SOURCE_GROUP_PLACEHOLDER,
        SUBNET_CIDR,
    };
    use super::*;

    /// The port the server in the rows below listens on.
    const SERVED_PORT: i32 = 8080;

    /// RULE: a packet reaches another workload's address exactly when one inbound rule of a
    /// security group on that workload's interface names its protocol, its port and its source
    /// — and is dropped otherwise; an address no interface of the account holds is on no
    /// network at all.
    ///
    /// The rows span what the rule generalises over: the KIND of source a rule names (the
    /// client's own security group, an address range holding it, a range that does not hold it,
    /// a group the client is not in), the port (the one served, each bound of a range, one past
    /// each bound), the protocol (the one served, another one, every protocol), and a group
    /// carrying no rule at all.
    #[test]
    fn a_packet_reaches_another_workload_only_where_a_rule_of_its_groups_names_it() {
        let rows: &[(&str, Vec<IpPermission>, i32, Reach)] = &[
            (
                "a rule naming the client's own security group on the served port",
                vec![permission(
                    "tcp",
                    Some((SERVED_PORT, SERVED_PORT)),
                    None,
                    Some(SOURCE_GROUP_PLACEHOLDER),
                )],
                SERVED_PORT,
                Reach::Admitted,
            ),
            (
                "a rule naming the range the client's address is in",
                vec![permission(
                    "tcp",
                    Some((SERVED_PORT, SERVED_PORT)),
                    Some(SUBNET_CIDR),
                    None,
                )],
                SERVED_PORT,
                Reach::Admitted,
            ),
            (
                "a rule whose range does not hold the client's address",
                vec![permission(
                    "tcp",
                    Some((SERVED_PORT, SERVED_PORT)),
                    Some("10.80.0.0/16"),
                    None,
                )],
                SERVED_PORT,
                Reach::Dropped,
            ),
            (
                "a rule naming a security group the client is not in",
                vec![permission(
                    "tcp",
                    Some((SERVED_PORT, SERVED_PORT)),
                    None,
                    Some("sg-00000000000000009"),
                )],
                SERVED_PORT,
                Reach::Dropped,
            ),
            (
                "a rule on another port only",
                vec![permission("tcp", Some((80, 80)), Some(SUBNET_CIDR), None)],
                SERVED_PORT,
                Reach::Dropped,
            ),
            (
                "the low bound of a rule's range",
                vec![permission("tcp", Some((8080, 8090)), Some(SUBNET_CIDR), None)],
                8080,
                Reach::Admitted,
            ),
            (
                "the high bound of that range",
                vec![permission("tcp", Some((8080, 8090)), Some(SUBNET_CIDR), None)],
                8090,
                Reach::Admitted,
            ),
            (
                "one port below that range",
                vec![permission("tcp", Some((8080, 8090)), Some(SUBNET_CIDR), None)],
                8079,
                Reach::Dropped,
            ),
            (
                "one port above that range",
                vec![permission("tcp", Some((8080, 8090)), Some(SUBNET_CIDR), None)],
                8091,
                Reach::Dropped,
            ),
            (
                "a rule on another protocol",
                vec![permission(
                    "udp",
                    Some((SERVED_PORT, SERVED_PORT)),
                    Some(SUBNET_CIDR),
                    None,
                )],
                SERVED_PORT,
                Reach::Dropped,
            ),
            (
                "a rule on every protocol",
                vec![permission("-1", None, Some(SUBNET_CIDR), None)],
                SERVED_PORT,
                Reach::Admitted,
            ),
            (
                "two rules, the second of which names the packet",
                vec![
                    permission("tcp", Some((80, 80)), Some(SUBNET_CIDR), None),
                    permission(
                        "tcp",
                        Some((SERVED_PORT, SERVED_PORT)),
                        None,
                        Some(SOURCE_GROUP_PLACEHOLDER),
                    ),
                ],
                SERVED_PORT,
                Reach::Admitted,
            ),
            (
                "a group carrying no inbound rule at all",
                Vec::new(),
                SERVED_PORT,
                Reach::Dropped,
            ),
        ];
        for (case, rules, port, expected) in rows {
            let engine = two_workloads(rules.clone(), true);
            let network = TaskNetwork::of(&engine).expect("the engine vends the network");
            let reached = network.reach(CLIENT_ADDRESS, SERVER_ADDRESS, *port);
            assert_eq!(
                reached, *expected,
                "case {case:?}: expected {expected:?}, got {reached:?}"
            );
        }
        // An address no interface of the account holds is on no network of it, whatever the
        // rules of the groups the account does hold say.
        let engine = two_workloads(vec![permission("-1", None, Some("0.0.0.0/0"), None)], true);
        let network = TaskNetwork::of(&engine).expect("the engine vends the network");
        let reached = network.reach(CLIENT_ADDRESS, "10.99.99.99", SERVED_PORT);
        assert_eq!(
            reached,
            Reach::OffNetwork,
            "case \"an address no interface holds\": expected it on no network, got {reached:?}"
        );
        // A client whose own interface carries no group is named by no rule whose source is a
        // group — and still by one naming the range its address is in.
        let engine = two_workloads(
            vec![permission(
                "tcp",
                Some((SERVED_PORT, SERVED_PORT)),
                None,
                Some(SOURCE_GROUP_PLACEHOLDER),
            )],
            false,
        );
        let network = TaskNetwork::of(&engine).expect("the engine vends the network");
        let reached = network.reach(CLIENT_ADDRESS, SERVER_ADDRESS, SERVED_PORT);
        assert_eq!(
            reached,
            Reach::Dropped,
            "case \"a client in no group, against a rule naming one\": expected it dropped, \
             got {reached:?}"
        );
        // An engine that vends no Amazon EC2 store resolves no address to an interface at all.
        let network = TaskNetwork::of(&::std::sync::Arc::new(7_u8));
        assert!(
            network.is_none(),
            "case \"an engine vending no network\": expected no network handle"
        );
    }

    /// RULE, the other direction: a security group filters what LEAVES its interface too, so a
    /// packet no outbound rule of the SENDER's own groups names is dropped however widely the
    /// destination admits it — and one such a rule names goes on to the destination's own filter.
    ///
    /// The rows span the outbound rules a sender's group may carry: the allow-all rule a created
    /// group comes with, a rule naming the destination's port, a rule naming another port, and no
    /// outbound rule at all.
    #[test]
    fn a_packet_no_outbound_rule_of_the_senders_groups_names_leaves_nothing() {
        use super::fixtures::two_workloads_with_client_egress;

        // The destination admits everything, so what each row decides is the sender's own egress.
        let admits_everything = || vec![permission("-1", None, Some("0.0.0.0/0"), None)];
        let rows: &[(&str, Vec<IpPermission>, Reach)] = &[
            (
                "the allow-all outbound rule a created group comes with",
                vec![permission("-1", None, Some("0.0.0.0/0"), None)],
                Reach::Admitted,
            ),
            (
                "an outbound rule naming the port the request is made to",
                vec![permission(
                    "tcp",
                    Some((SERVED_PORT, SERVED_PORT)),
                    Some("0.0.0.0/0"),
                    None,
                )],
                Reach::Admitted,
            ),
            (
                "an outbound rule naming another port only",
                vec![permission("tcp", Some((80, 80)), Some("0.0.0.0/0"), None)],
                Reach::Dropped,
            ),
            ("no outbound rule at all", Vec::new(), Reach::Dropped),
        ];
        for (case, egress, expected) in rows {
            let engine =
                two_workloads_with_client_egress(admits_everything(), true, egress.clone());
            let network = TaskNetwork::of(&engine).expect("the engine vends the network");
            let reached = network.reach(CLIENT_ADDRESS, SERVER_ADDRESS, SERVED_PORT);
            assert_eq!(
                reached, *expected,
                "case {case:?}: expected {expected:?}, got {reached:?}"
            );
        }
        // A sender this account holds no interface for carries no groups, so nothing of it
        // filters what leaves: the destination's own rules decide alone.
        let engine = two_workloads_with_client_egress(admits_everything(), true, Vec::new());
        let network = TaskNetwork::of(&engine).expect("the engine vends the network");
        let reached = network.reach("10.70.9.9", SERVER_ADDRESS, SERVED_PORT);
        assert_eq!(
            reached,
            Reach::Admitted,
            "case \"a sender no interface of the account holds\": expected the destination's own \
             rules to decide, got {reached:?}"
        );
    }
}
