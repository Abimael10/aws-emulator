// SPDX-License-Identifier: CC0-1.0

//! Whether the CONNECTION a load balancer opens to a registered target can ARRIVE at all —
//! the question a `forward` of a client's request asks before anything behind the
//! registration answers it.
//!
//! A forwarded request reaches the target exactly the way a health check of that same
//! registration does: as a connection the load balancer opens from its OWN network interfaces
//! to the address the registration names, on the port that registration receives traffic on
//! ("the port on which the target is listening", `TargetDescription$Port`). So the security
//! groups governing that target decide both — a security group "controls the traffic that is
//! allowed to reach and leave the resources that it is associated with", and its inbound rules
//! "control the incoming traffic that's allowed to reach the instance" (Amazon VPC User Guide,
//! "Security groups") — and a target no inbound rule admits the load balancer to on that port
//! is a target no request of it can be served by, however healthy anything considers it.
//!
//! The rule itself is read through the ONE reading of it this crate has
//! ([`super::health_check_firewall`]), which is also what decides whether a HEALTH CHECK of the
//! registration arrives ([`super::host_counts`]) — so the check that reports a target
//! unreachable and the forward that cannot reach it cannot drift apart. What the load balancer
//! answers for a connection it cannot establish is its OWN status
//! ([`crate::service::data_plane::TARGET_UNREACHABLE_STATUS`]) and never the target's response:
//! nothing of the target's was asked.
//!
//! Where the governing groups come from is where a health check reads them too: the ones the
//! scheduler running the workload DECLARED ([`GuardingGroups`]) — an Amazon ECS task's
//! `AwsVpcConfiguration$securityGroups` — else the ones the account's own inventory reports for
//! the resource the registration NAMES, the interfaces attached to an instance it names and the
//! interface holding an address it names. Those records are Amazon EC2's and are read there,
//! exactly as every other fact this service does not own is ([`super::network`]).
//!
//! Nothing is decided from silence: a target whose groups are neither declared nor readable, a
//! group whose rules this service cannot read, and a load balancer whose own groups and
//! addresses are unknown all leave the question UNDECIDED, and an undecided question blocks no
//! request.
//!
//! Every read here is made with NO guard of this service's store held and changes nothing —
//! asking whether a connection is admitted moves no status, consumes no quota and bumps no
//! counter — exactly as the reading of the SERVER behind a registration is
//! ([`super::target_servers`]).

use ::std::collections::HashMap;

use ::service_com_amazonaws_ec2_amazonec2::ops::types as ec2_types;
use ::service_com_amazonaws_ec2_amazonec2::state as ec2;

use super::health_check_firewall::{interface_admits_check, CheckSource, GuardingGroups};
use super::host_counts::{interface_addresses_of, interface_group_ids};
use super::target_servers::INSTANCE_TARGET_TYPE;

/// Where a connection to a target comes FROM: the load balancer that opens it, as the inbound
/// rules of the target's own security groups can name it ([`CheckSource`] is what those rules
/// are read against).
#[derive(Debug, Default, Clone)]
pub struct ConnectionSource {
    /// The `GroupId` of each security group the load balancer is associated with.
    pub group_ids: Vec<String>,
    /// The `NetworkInterfaceId` of each of the load balancer's own interfaces, which hold the
    /// addresses its connections come from ([`super::service_interfaces`]).
    pub interface_ids: Vec<String>,
    /// Whether those groups were READ off the load balancer's own record: with them read, an
    /// empty list is a load balancer associated with no security group, and without them the
    /// question a rule naming a source group poses cannot be read at all
    /// ([`CheckSource::groups_resolved`]).
    pub groups_resolved: bool,
}

impl ConnectionSource {
    /// The source a `forward` this load balancer performs comes from, as this store holds it:
    /// the groups it is associated with and the interfaces it holds.
    pub fn of_load_balancer(load_balancer: &super::LoadBalancerRecord) -> Self {
        Self {
            group_ids: load_balancer.security_groups.clone(),
            interface_ids: load_balancer.network_interface_ids.clone(),
            // The record answered which groups it is associated with — none of them, where it
            // carries none.
            groups_resolved: true,
        }
    }
}

/// Whether the security groups governing the registration `target_id` of a `target_type` group
/// admit a connection from `source` to `port`: `Some(true)` when an inbound rule of one of them
/// names it, `Some(false)` when every rule of every one of them is known and none does, and
/// `None` when the question is undecided.
///
/// `declared` is what the scheduler running the workload behind the registration says its
/// groups are, and empty for a registration nothing declares them for — whose groups are read
/// off the resource the registration names instead ([`governing_groups`]).
pub fn admits_connection<Eng: Send + Sync + 'static>(
    engine: &Eng,
    target_type: &str,
    target_id: &str,
    port: i32,
    declared: &GuardingGroups,
    source: &ConnectionSource,
) -> Option<bool> {
    // Nothing of this emulator holds the account's network, so no rule can be read either way.
    let state = ec2::service_state(engine)?;
    let store = state.lock();
    let governing = if declared.is_empty() {
        governing_groups(&store, target_type, target_id)?
    } else {
        declared.to_vec()
    };
    // Only the rules of the groups GOVERNING this one target are read, so the work a request
    // costs is those groups' rules rather than every group the account holds. A group absent
    // from the map is one whose rules are unknown, which is what
    // [`interface_admits_check`] leaves undecided.
    let ingress: HashMap<String, Vec<ec2_types::IpPermission>> = governing
        .iter()
        .filter_map(|group_id| {
            let group = store.security_groups.get(group_id)?;
            Some((
                group_id.clone(),
                group.ip_permissions.clone().unwrap_or_default(),
            ))
        })
        .collect();
    let source = CheckSource {
        group_ids: source.group_ids.clone(),
        // A rule may name its source group by NAME rather than by id, so the names of the
        // load balancer's own groups are read beside their ids.
        group_names: source
            .group_ids
            .iter()
            .filter_map(|group_id| {
                store
                    .security_groups
                    .get(group_id)
                    .and_then(|group| group.group_name.clone())
            })
            .collect(),
        // And the addresses its connections come from are the ones its own interfaces hold.
        addresses: source
            .interface_ids
            .iter()
            .filter_map(|interface_id| store.network_interfaces.get(interface_id))
            .flat_map(interface_addresses_of)
            .collect(),
        groups_resolved: source.groups_resolved,
    };
    interface_admits_check(&governing, &ingress, port, &source)
}

/// Whether this connection is one the account's records say NOTHING admits — the condition a
/// load balancer answers its own status for rather than serving what is behind the
/// registration.
///
/// `false` wherever the question is undecided ([`admits_connection`]): nothing is refused on
/// silence.
pub fn connection_is_blocked<Eng: Send + Sync + 'static>(
    engine: &Eng,
    target_type: &str,
    target_id: &str,
    port: i32,
    declared: &GuardingGroups,
    source: &ConnectionSource,
) -> bool {
    admits_connection(engine, target_type, target_id, port, declared, source) == Some(false)
}

/// The security groups GOVERNING traffic to what one registration names, as the account's own
/// inventory reports them — `None` when the inventory reports nothing about it, which decides
/// nothing.
///
/// A security group is associated with a network INTERFACE, so the groups governing a
/// registration are those of the interfaces belonging to the resource it names: an `instance`
/// registration names the instance, whose traffic arrives on the interfaces attached to it, and
/// every other registration names an address, which one interface holds. Read the same way the
/// periodic health check reads them (`super::host_counts`), through the same readers of an
/// interface's groups, so the same workload registered by its instance id and by its address is
/// governed by the same rules.
///
/// The walk is over the account's interfaces in the one order it holds them in, by
/// `NetworkInterfaceId`, so two reads of one registration answer the same groups; an address is
/// held by one interface, so which interface that order reaches first does not decide the
/// verdict.
fn governing_groups(
    store: &ec2::ComAmazonawsEc2Amazonec2State,
    target_type: &str,
    target_id: &str,
) -> Option<Vec<String>> {
    if target_type == INSTANCE_TARGET_TYPE {
        let attached = store.network_interfaces.values().filter(|interface| {
            interface
                .attachment
                .as_ref()
                .and_then(|attachment| attachment.instance_id.as_deref())
                == Some(target_id)
        });
        let mut groups: Vec<String> = Vec::new();
        let mut holds_an_interface = false;
        for interface in attached {
            // One interface of the instance admitting the connection is enough, exactly as one
            // group of an interface is, so every attached interface's groups govern it.
            holds_an_interface = true;
            groups.extend(interface_group_ids(interface));
        }
        return holds_an_interface.then_some(groups);
    }
    store
        .network_interfaces
        .values()
        .find(|interface| {
            interface_addresses_of(interface)
                .iter()
                .any(|held| held == target_id)
        })
        .map(interface_group_ids)
}
