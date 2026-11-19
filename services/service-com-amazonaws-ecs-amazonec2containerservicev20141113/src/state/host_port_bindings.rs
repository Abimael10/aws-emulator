// SPDX-License-Identifier: CC0-1.0

//! The HOST PORTS a task's containers are bound to on the container instance that runs them —
//! `Container$networkBindings`, "the network bindings associated with the container".
//!
//! # The rule this module implements
//!
//! A container is reached at a port of the HOST it runs on, and which port that is depends on
//! the network mode the revision declares. `PortMapping$hostPort`: "If you use containers in a
//! task with the `awsvpc` or `host` network mode, the `hostPort` can either be left blank or
//! set to the same value as the `containerPort`" — a STATIC mapping, which
//! [`super::is_static_port_mapping`] is the one reading of — while "if you use containers in a
//! task with the `bridge` network mode, you can specify a non-reserved host port for your
//! container port mapping ... or you can omit the `hostPort` (or set it to `0`) while
//! specifying a `containerPort` and your container automatically receives a port in the
//! ephemeral port range for your container instance operating system and Docker version". The
//! range that assignment comes out of is that member's own figure: "the default ephemeral port
//! range from 49153 through 65535 is used" ([`EPHEMERAL_HOST_PORT_RANGE`]).
//!
//! So a task placed ON A CONTAINER INSTANCE holds one binding per declared port mapping of
//! each of its containers, and those bindings are what the host offers: a port carries at most
//! one container of the host at a time, so an assignment skips every port a task already there
//! holds. A task in the `awsvpc` network mode is reached at its OWN interface instead and
//! reports no network binding at all — "tasks that use the `awsvpc` network mode are associated
//! with an elastic network interface, not an Amazon EC2 instance"
//! (`LoadBalancer$targetGroupArn`) — which is also why such a task is registered as an `ip`
//! target rather than an `instance` one ([`super::load_balancer_targets`]).
//!
//! The bindings are recorded ON THE TASK where it is placed, because that is where the host is
//! chosen: every path that places one asks for them once the instance is known, so a task's
//! bindings are stable for as long as it runs and two reads of the same task report the same
//! ports.
//!
//! A mapping that declares a `containerPortRange` instead of a single port carries no single
//! host port — `PortMapping$hostPort` says to "leave this field empty" for one — so no binding
//! is recorded for it rather than one being invented.
//!
//! # Bounds
//!
//! Both are this emulator's and neither is sized by a request: how many mappings of one
//! container are bound ([`MAX_BOUND_PORT_MAPPINGS`]), and how many of the ports a registration
//! then reports a target answering on (`super::MAX_SERVED_PORT_RANGES`, the figure the declared
//! ports of a revision are already held to). A DYNAMIC assignment carries its position in
//! [`EPHEMERAL_HOST_PORT_RANGE`] forward across the whole placement, so the mappings of one task
//! cost ONE walk of that range between them rather than one walk each.

use super::*;

/// The ephemeral port range a DYNAMIC host-port assignment comes out of.
///
/// `PortMapping$hostPort`'s own figures: "the default ephemeral port range from 49153 through
/// 65535 is used" where the container instance's kernel parameter for it is unavailable, which
/// is the range this emulator assigns from — it holds no instance's
/// `/proc/sys/net/ipv4/ip_local_port_range`.
pub const EPHEMERAL_HOST_PORT_RANGE: (i32, i32) = (49153, 65535);

/// The address a binding of a container instance's host port is made on.
///
/// `NetworkBinding$bindIP` is "the IP address that the container is bound to on the container
/// instance", and a mapping that names no address of the host is published on every address it
/// has, which is the unspecified IPv4 address.
pub const UNSPECIFIED_BIND_ADDRESS: &str = "0.0.0.0";

/// How many port mappings of ONE container are bound.
///
/// A bound on the work one placement costs, not an Amazon ECS quota:
/// `PortMapping$containerPortRange` states "you can specify a maximum of 100 port ranges per
/// container", so that is the most one container can legitimately declare, while `portMappings`
/// carries no length of its own. A definition past this bound has its first this many mappings
/// bound and no more, so the list stored on a task is held to a figure this service fixes
/// rather than to one a request chose — the same bound `super::MAX_SERVED_PORT_RANGES` holds
/// the declared ports to.
pub const MAX_BOUND_PORT_MAPPINGS: usize = 100;

impl ComAmazonawsEcsAmazonec2containerservicev20141113State {
    /// Record the host-port bindings of the containers of a task placed on
    /// `container_instance_arn`, in place on `containers`.
    ///
    /// Nothing is recorded for a task placed on no container instance at all (a Fargate
    /// placement, whose host this emulator holds no record of) or for one whose revision gives
    /// it an interface of its own, for the reason the module docs give.
    pub fn bind_task_host_ports(
        &self,
        container_instance_arn: Option<&str>,
        network_mode: &str,
        definitions: &[ContainerDefinition],
        containers: &mut [ContainerRecord],
    ) {
        let Some(instance_arn) = container_instance_arn else {
            return;
        };
        if network_mode == NETWORK_MODE_AWSVPC {
            return;
        }
        let mut taken = self.host_ports_in_use(instance_arn);
        // Where the next DYNAMIC assignment starts looking. Carried across the whole placement
        // and only ever moved forward, so the ports of one task cost one walk of
        // [`EPHEMERAL_HOST_PORT_RANGE`] between them rather than one walk each.
        let mut cursor = EPHEMERAL_HOST_PORT_RANGE.0;
        for container in containers.iter_mut() {
            let Some(definition) = definitions
                .iter()
                .find(|declared| declared.name == container.name)
            else {
                continue;
            };
            let mut bindings = Vec::new();
            for mapping in definition
                .port_mappings
                .iter()
                .flatten()
                .take(MAX_BOUND_PORT_MAPPINGS)
            {
                let Some(container_port) = mapping.container_port else {
                    continue;
                };
                let Some(host_port) = assigned_host_port(
                    mapping.host_port,
                    container_port,
                    network_mode,
                    &taken,
                    &mut cursor,
                ) else {
                    continue;
                };
                taken.insert(host_port);
                bindings.push(NetworkBinding {
                    bind_ip: Some(UNSPECIFIED_BIND_ADDRESS.to_string()),
                    container_port: Some(container_port),
                    host_port: Some(host_port),
                    protocol: mapping.protocol.clone(),
                    container_port_range: None,
                    host_port_range: None,
                });
            }
            container.network_bindings = bindings;
        }
    }

    /// The host ports the tasks ALREADY on `instance_arn` hold: a port carries at most one
    /// container of the host at a time, so an assignment skips every one of these.
    ///
    /// A task that has stopped holds none: its containers are gone, so the ports they were
    /// bound to are the host's again.
    fn host_ports_in_use(&self, instance_arn: &str) -> HashSet<i32> {
        self.tasks
            .values()
            .filter(|task| task.container_instance_arn.as_deref() == Some(instance_arn))
            .filter(|task| task.last_status != STATUS_STOPPED)
            .flat_map(|task| task.containers.iter())
            .flat_map(|container| container.network_bindings.iter())
            .filter_map(|binding| binding.host_port)
            .collect()
    }

    /// The host port the container `container_name` of `task` had `container_port` bound to,
    /// and `None` for a task holding no such binding.
    ///
    /// What a target of a load-balanced service's task is reached at where the task is an
    /// `instance` target: the load balancer sends traffic to the HOST, so the port is the
    /// host's and not the container's ([`super::load_balancer_targets`]).
    pub fn bound_host_port(
        &self,
        task: &TaskRecord,
        container_name: Option<&str>,
        container_port: Option<i32>,
    ) -> Option<i32> {
        task.containers
            .iter()
            .filter(|container| {
                container_name.is_none_or(|name| container.name.as_deref() == Some(name))
            })
            .flat_map(|container| container.network_bindings.iter())
            .find(|binding| container_port.is_none() || binding.container_port == container_port)
            .and_then(|binding| binding.host_port)
    }

    /// Every host port the containers of `task` are bound to, as the ports a target reached at
    /// that task's HOST answers on.
    ///
    /// BOUNDED by `super::MAX_SERVED_PORT_RANGES`, the same figure the declared ports of a
    /// revision are held to: the list a registration carries is sized by this service and never
    /// by the length of a list a caller chose to register.
    pub fn task_bound_host_ports(
        &self,
        task: &TaskRecord,
    ) -> Vec<::std::ops::RangeInclusive<i32>> {
        task.containers
            .iter()
            .flat_map(|container| container.network_bindings.iter())
            .filter_map(|binding| binding.host_port.map(|port| port..=port))
            .take(super::MAX_SERVED_PORT_RANGES)
            .collect()
    }
}

/// The host port one mapping is bound to: the one it DECLARES, the container port itself under
/// a static mapping, and otherwise the first port at or after `cursor` that the host does not
/// already hold.
///
/// `cursor` is the placement's own position in [`EPHEMERAL_HOST_PORT_RANGE`] and is only ever
/// moved FORWARD: an assignment starts where the last one stopped, so the mappings of one
/// placement cost ONE walk of that range between them rather than one walk each — which is what
/// keeps the work a placement does independent of how many mappings its revision declares.
///
/// `None` once the range holds no free port at or after `cursor` — the host has nothing left to
/// bind, so no binding is claimed for the mapping.
fn assigned_host_port(
    declared: Option<i32>,
    container_port: i32,
    network_mode: &str,
    taken: &HashSet<i32>,
    cursor: &mut i32,
) -> Option<i32> {
    let last = EPHEMERAL_HOST_PORT_RANGE.1;
    // "you can omit the hostPort (or set it to 0)" is what asks for an assignment, so every
    // other value is a port the mapping declares.
    match declared {
        Some(port) if port != DYNAMIC_HOST_PORT => Some(port),
        _ if super::is_static_port_mapping(network_mode) => Some(container_port),
        _ => {
            let assigned = (*cursor..=last).find(|port| !taken.contains(port))?;
            *cursor = assigned.saturating_add(1);
            Some(assigned)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One row of the table below: what one mapping declares, the mode it is bound under, the
    /// ports the host already holds, and the port the binding takes.
    struct Row<'a> {
        case: &'a str,
        /// `PortMapping$hostPort` as the mapping states it.
        declared: Option<i32>,
        container_port: i32,
        network_mode: &'a str,
        /// The ports the host already holds, which an assignment skips.
        taken: &'a HashSet<i32>,
        bound: Option<i32>,
    }

    /// RULE: a declared port is the port bound; a static mapping takes the container port; and
    /// a dynamic one takes a port of the ephemeral range the host does not hold.
    ///
    /// The rows span the two static network modes and the dynamic one, a declared port, the two
    /// spellings that ask for an assignment, a host already holding the start of the range, and
    /// a host holding the whole of it.
    #[test]
    fn a_mapping_is_bound_to_the_port_its_mode_and_its_declaration_decide() {
        let (first, last) = EPHEMERAL_HOST_PORT_RANGE;
        let none: HashSet<i32> = HashSet::new();
        let held: HashSet<i32> = (first..first + 3).collect();
        let whole: HashSet<i32> = (first..=last).collect();
        let rows = [
            Row {
                case: "a declared host port under bridge",
                declared: Some(8080),
                container_port: 80,
                network_mode: NETWORK_MODE_BRIDGE,
                taken: &none,
                bound: Some(8080),
            },
            Row {
                case: "no host port under host: the container port",
                declared: None,
                container_port: 80,
                network_mode: NETWORK_MODE_HOST,
                taken: &none,
                bound: Some(80),
            },
            Row {
                case: "no host port under bridge: the start of the ephemeral range",
                declared: None,
                container_port: 80,
                network_mode: NETWORK_MODE_BRIDGE,
                taken: &none,
                bound: Some(first),
            },
            Row {
                case: "a zero host port under bridge: an assignment, as omitting it is",
                declared: Some(DYNAMIC_HOST_PORT),
                container_port: 80,
                network_mode: NETWORK_MODE_BRIDGE,
                taken: &none,
                bound: Some(first),
            },
            Row {
                case: "a host already holding the start of the range",
                declared: None,
                container_port: 80,
                network_mode: NETWORK_MODE_BRIDGE,
                taken: &held,
                bound: Some(first + 3),
            },
            Row {
                case: "a host holding the whole range",
                declared: None,
                container_port: 80,
                network_mode: NETWORK_MODE_BRIDGE,
                taken: &whole,
                bound: None,
            },
        ];
        for row in &rows {
            // Each row is judged on its own, so every one starts the walk where a placement
            // starts it.
            let mut cursor = first;
            let bound = assigned_host_port(
                row.declared,
                row.container_port,
                row.network_mode,
                row.taken,
                &mut cursor,
            );
            assert_eq!(
                bound, row.bound,
                "case {:?}: expected {:?}, got {bound:?}",
                row.case, row.bound
            );
        }
    }

    /// RULE: the assignments of ONE placement never revisit a port, so the walk over the
    /// ephemeral range is shared between the mappings rather than repeated for each.
    #[test]
    fn the_assignments_of_one_placement_walk_the_range_once() {
        let (first, _) = EPHEMERAL_HOST_PORT_RANGE;
        let taken: HashSet<i32> = HashSet::new();
        let mut cursor = first;
        let assigned: Vec<Option<i32>> = (0..4)
            .map(|index| {
                assigned_host_port(
                    None,
                    80 + index,
                    NETWORK_MODE_BRIDGE,
                    &taken,
                    &mut cursor,
                )
            })
            .collect();
        let expected: Vec<Option<i32>> = (0..4).map(|index| Some(first + index)).collect();
        assert_eq!(
            assigned, expected,
            "case \"four dynamic mappings of one placement\": expected {expected:?}, got \
             {assigned:?}"
        );
    }
}
