// SPDX-License-Identifier: CC0-1.0

//! The Amazon VPC networking an `awsvpc` placement NAMES, resolved against the task
//! definition it places.
//!
//! A task definition in the `awsvpc` network mode "is allocated its own elastic network
//! interface", and that interface is placed in the subnets the request names
//! (`AwsVpcConfiguration$subnets`: "The IDs of the subnets associated with the task or
//! service") with the security groups it names — so a placement of such a revision has to
//! carry a `networkConfiguration` that says where the interface goes, and a placement of a
//! revision in any other mode has no interface of its own to configure.
//!
//! Every operation that places a task from a stored revision — `RunTask`, `StartTask`,
//! `CreateService`, `UpdateService`, `CreateTaskSet` — is held to the one rule here, so none
//! of them can answer differently about the same pair.

use super::*;

/// `InvalidParameterException$message` for an `awsvpcConfiguration` whose `subnets` list is
/// empty. `subnets` is "The IDs of the subnets associated with the task or service", and the
/// interface an `awsvpc` task is given is created IN one of them, so a list naming none
/// configures no interface at all. The whole sentence is fixed text, naming the member and
/// nothing of the request.
pub const AWSVPC_SUBNETS_EMPTY_MESSAGE: &str = "subnets can not be empty.";

/// How many subnets ONE `awsvpcConfiguration` may name.
///
/// `AwsVpcConfiguration$subnets`: "The IDs of the subnets associated with the task or
/// service. There's a limit of 16 subnets that can be specified."
pub const MAX_AWSVPC_SUBNETS: usize = 16;

/// How many security groups ONE `awsvpcConfiguration` may name.
///
/// `AwsVpcConfiguration$securityGroups`: "The IDs of the security groups associated with
/// the task or service ... There's a limit of 5 security groups that can be specified."
/// The groups govern the ONE elastic network interface an `awsvpc` task is allocated, so a
/// placement naming more of them than may be associated with that interface describes a
/// network no task of it could be reached on.
pub const MAX_AWSVPC_SECURITY_GROUPS: usize = 5;

/// The noun `AwsVpcConfiguration$subnets` names its entries by, for the refusal that states
/// the bound.
const AWSVPC_SUBNETS_SUBJECT: &str = "subnets";

/// The noun `AwsVpcConfiguration$securityGroups` names its entries by ("The IDs of the
/// security groups associated with the task or service").
const AWSVPC_SECURITY_GROUPS_SUBJECT: &str = "security groups";

/// The refusal a launch earns when its `networkConfiguration` does not agree with the
/// network mode of the task definition it runs, or `None` when the two agree.
///
/// The rule, which the model cannot express because `networkConfiguration` is optional on
/// every launch: a task definition in the `awsvpc` network mode gets its own elastic
/// network interface, so the launch has to say which subnets and security groups that
/// interface takes ([`AWSVPC_NETWORK_CONFIGURATION_REQUIRED_MESSAGE`]) — and a task
/// definition in any other mode has no interface of its own to configure, so a network
/// configuration is not a thing that launch can carry
/// ([`NETWORK_CONFIGURATION_NOT_VALID_MESSAGE`]). Saying which subnets the interface takes
/// means naming at least one of them: an empty list names none, so there is nowhere for the
/// interface to be created ([`AWSVPC_SUBNETS_EMPTY_MESSAGE`]) — and no more of them than the
/// member's own doc allows, since each of its lists states a limit on what ONE interface can
/// be given ([`MAX_AWSVPC_SUBNETS`], [`MAX_AWSVPC_SECURITY_GROUPS`]): a list past that bound
/// configures an interface that could not be created, so the launch is refused and nothing is
/// stored for it.
///
/// Shared by every operation that launches a task from a stored revision, so `RunTask`
/// and `CreateService` cannot answer differently about the same pair.
pub fn network_configuration_violation(
    network_mode: &str,
    network_configuration: Option<&NetworkConfiguration>,
) -> Option<String> {
    // `NetworkConfiguration` carries exactly one member, `awsvpcConfiguration`, so a
    // configuration without it configures no interface and is not one.
    let awsvpc = network_configuration
        .and_then(|configuration| configuration.awsvpc_configuration.as_ref());
    match (network_mode == NETWORK_MODE_AWSVPC, awsvpc.is_some()) {
        (true, false) => return Some(AWSVPC_NETWORK_CONFIGURATION_REQUIRED_MESSAGE.to_string()),
        (false, true) => return Some(NETWORK_CONFIGURATION_NOT_VALID_MESSAGE.to_string()),
        _ => {}
    }
    let awsvpc = awsvpc?;
    if awsvpc.subnets.is_empty() {
        return Some(AWSVPC_SUBNETS_EMPTY_MESSAGE.to_string());
    }
    if awsvpc.subnets.len() > MAX_AWSVPC_SUBNETS {
        return Some(at_most_items_message(
            AWSVPC_SUBNETS_SUBJECT,
            MAX_AWSVPC_SUBNETS,
        ));
    }
    let groups = awsvpc.security_groups.as_deref().unwrap_or_default();
    (groups.len() > MAX_AWSVPC_SECURITY_GROUPS).then(|| {
        at_most_items_message(
            AWSVPC_SECURITY_GROUPS_SUBJECT,
            MAX_AWSVPC_SECURITY_GROUPS,
        )
    })
}

/// The Amazon EC2 error code for a subnet id no subnet of the account carries, as
/// `DescribeSubnets` answers it.
pub const SUBNET_NOT_FOUND_ERROR_CODE: &str = "InvalidSubnetID.NotFound";

/// `InvalidParameterException$message` for a placement whose `awsvpcConfiguration` names a
/// subnet Amazon VPC holds none of.
///
/// Amazon ECS states the lookup it made and the answer Amazon VPC gave it: the subnets it
/// asked about, then Amazon EC2's own sentence for the first id that resolved to nothing
/// ("The subnet ID '<id>' does not exist") and the error code that sentence came with. Every
/// variable part is taken from the request — the ids it named, in the order it named them —
/// and from the lookup's answer.
pub fn subnet_lookup_message(requested: &[String], missing: &str) -> String {
    format!(
        "Error retrieving subnet information for [{}]: The subnet ID '{missing}' does not \
         exist (ErrorCode: {SUBNET_NOT_FOUND_ERROR_CODE})",
        requested.join(", ")
    )
}

/// The refusal a placement earns when a subnet its `awsvpcConfiguration` names is one
/// Amazon VPC holds no subnet for in `region`, or `None` when every named subnet resolves.
///
/// The interface an `awsvpc` task is allocated is created IN one of the subnets the
/// placement named, so a placement naming a subnet the account does not hold cannot be made:
/// Amazon ECS looks the subnets up in Amazon VPC and answers the lookup failure it got back
/// ([`subnet_lookup_message`]). The subnets of the account are Amazon EC2's records, so they
/// are read out of the Amazon EC2 service's own store through the one shared engine.
///
/// The lookup is made in the Region the request was signed for: a subnet is in one
/// Availability Zone, and so in one Region (`Subnet$availabilityZone`), so a placement in
/// one Region cannot use a subnet of another, and an id that names a subnet of another
/// Region names none of this one.
///
/// LOCK ORDER: this takes the Amazon EC2 store's lock, holds no other while it does, and
/// hands back an owned message — so it is called with NO guard of this service's store held,
/// which is what keeps a lock cycle between the two stores unreachable. The same contract
/// [`evaluate_image_pulls`] has.
pub fn unresolved_subnet_refusal<Eng: Send + Sync + 'static>(
    engine: &Eng,
    region: &str,
    configuration: Option<&NetworkConfiguration>,
) -> Option<String> {
    let requested = configuration
        .and_then(|configuration| configuration.awsvpc_configuration.as_ref())
        .map(|awsvpc| awsvpc.subnets.clone())
        .unwrap_or_default();
    if requested.is_empty() {
        return None;
    }
    let ec2 = ::service_com_amazonaws_ec2_amazonec2::state::service_state(engine)?;
    let missing = {
        let store = ec2.lock();
        requested
            .iter()
            .find(|subnet| {
                store
                    .subnets
                    .get(subnet.as_str())
                    .and_then(|subnet| subnet.availability_zone.as_deref())
                    .is_none_or(|zone| !zone.starts_with(region))
            })
            .cloned()
    };
    missing.map(|missing| subnet_lookup_message(&requested, &missing))
}

/// `InvalidParameterException$message` for a placement whose `awsvpcConfiguration` pairs a
/// security group with subnets of another network.
///
/// The interface is created by Amazon EC2, which answers this sentence when a request pairs
/// resources of two networks, and Amazon ECS reports that answer, the way it reports Amazon
/// EC2's sentence for a subnet that resolves to nothing ([`subnet_lookup_message`]).
/// `AwsVpcConfiguration$securityGroups` states the rule the sentence reports: "All specified
/// security groups must be from the same VPC" — and a group governs the one interface the
/// placement's subnets create, so it has to be of that interface's network too. The whole
/// sentence is fixed text, naming nothing of the request.
pub const AWSVPC_RESOURCES_IN_DIFFERENT_NETWORKS_MESSAGE: &str =
    "You have specified two resources that belong to different networks.";

/// `InvalidParameterException$message` for a placement whose `awsvpcConfiguration` names
/// `subnets` of more than one VPC.
///
/// Amazon ECS resolves the subnets itself before any interface is created, so it answers its
/// OWN sentence for this one rather than Amazon EC2's pairing sentence
/// ([`AWSVPC_RESOURCES_IN_DIFFERENT_NETWORKS_MESSAGE`]): the rule the member states
/// (`AwsVpcConfiguration$subnets`: "All specified subnets must be from the same VPC"), worded
/// for the input it refuses. The whole sentence is fixed text, naming nothing of the request.
pub const AWSVPC_SUBNETS_OF_DIFFERENT_VPCS_MESSAGE: &str =
    "All input subnets must be from the same VPC.";

/// The refusal a placement earns when the subnets and security groups its
/// `awsvpcConfiguration` names are not all in ONE VPC, or `None` when they are.
///
/// The rule: an `awsvpc` placement configures ONE elastic network interface, which is created
/// in one subnet of one VPC and governed by the groups of that same VPC — "All specified
/// subnets must be from the same VPC" (`AwsVpcConfiguration$subnets`) and "All specified
/// security groups must be from the same VPC" (`$securityGroups`), and a security group belongs
/// to exactly one VPC (`SecurityGroup$vpcId`), so a group outside the subnets' VPC could never
/// be applied to that interface. A placement spread across two networks describes an interface
/// that could not be created, so it is refused and nothing of it is stored.
///
/// The two faults are reported by different sentences, because they are answered by different
/// steps: Amazon ECS resolves the SUBNETS of the request itself and states the member's own
/// rule for a list that spans two of them ([`AWSVPC_SUBNETS_OF_DIFFERENT_VPCS_MESSAGE`]),
/// while a group paired with subnets of another network is a pairing only Amazon EC2 rejects,
/// so Amazon ECS reports that service's sentence
/// ([`AWSVPC_RESOURCES_IN_DIFFERENT_NETWORKS_MESSAGE`]). The subnets are judged first, since
/// where the interface goes is decided before what governs it.
///
/// Which VPC each named resource is in is Amazon EC2's record, so both are read out of that
/// service's own store: a subnet or a group Amazon VPC holds no VPC id for places the interface
/// in no network this can compare against, so it decides nothing here — and an id that resolves
/// to no group at all is a lookup of its own rather than this rule.
///
/// Shared by every operation that places a task from a stored revision, so `RunTask` and
/// `CreateService` cannot answer differently about the same configuration.
///
/// LOCK ORDER: this takes the Amazon EC2 store's lock, holds no other while it does, and hands
/// back an owned message — so it is called with NO guard of this service's store held, exactly
/// as [`unresolved_subnet_refusal`] is.
pub fn misplaced_network_resource_refusal<Eng: Send + Sync + 'static>(
    engine: &Eng,
    configuration: Option<&NetworkConfiguration>,
) -> Option<String> {
    let awsvpc = configuration?.awsvpc_configuration.as_ref()?;
    if awsvpc.subnets.is_empty() {
        return None;
    }
    let groups = awsvpc.security_groups.clone().unwrap_or_default();
    let ec2 = ::service_com_amazonaws_ec2_amazonec2::state::service_state(engine)?;
    let store = ec2.lock();
    // Which network each named subnet is in, in the order the placement named them: the
    // interface is created in the FIRST of them Amazon VPC holds a network for, so that
    // subnet's VPC is the one every other resource the placement names has to be in.
    let mut named_networks = awsvpc.subnets.iter().filter_map(|subnet| {
        store
            .subnets
            .get(subnet.as_str())
            .and_then(|held| held.vpc_id.clone())
    });
    let vpc_id = named_networks.next()?;
    // Subnets of two VPCs leave no ONE network for the interface: the list itself is the bad
    // parameter, so the refusal states what that member requires.
    if named_networks.any(|other| other != vpc_id) {
        return Some(AWSVPC_SUBNETS_OF_DIFFERENT_VPCS_MESSAGE.to_string());
    }
    // And a group outside the network the subnets name could never govern the interface
    // Amazon EC2 would create in one of them, which is the pairing that service refuses.
    let group_of_another_network = groups.iter().any(|group| {
        store
            .security_groups
            .get(group.as_str())
            .and_then(|held| held.vpc_id.as_deref())
            .is_some_and(|held| held != vpc_id)
    });
    group_of_another_network.then(|| AWSVPC_RESOURCES_IN_DIFFERENT_NETWORKS_MESSAGE.to_string())
}

/// Where ONE subnet of the account is, as Amazon VPC holds it: what a placement into that
/// subnet puts the task — and the interface it is allocated — in.
#[derive(Debug, Clone)]
pub struct SubnetPlacement {
    /// `Subnet$availabilityZone`: the zone the subnet is in, which is the zone of every
    /// task placed into it.
    pub availability_zone: String,
    /// `Subnet$cidrBlock`: "The IPv4 CIDR block assigned to the subnet", and `None` for a
    /// subnet Amazon VPC holds none for — the range an address inside the subnet is read
    /// against.
    pub cidr_block: Option<String>,
}

/// Where each of `subnets` is, as Amazon VPC holds it ([`SubnetPlacement`]) — one pair per
/// subnet that resolves, and nothing for an id Amazon VPC holds no subnet for.
///
/// `Task$availabilityZone` is "the Availability Zone for the task", and the zone of a task
/// that was allocated its own elastic network interface is the zone of the subnet that
/// interface was created in — a subnet is in exactly one zone, so the placement's first
/// subnet decides it. The same subnet's CIDR block is what the interface's own address comes
/// out of. Both are Amazon EC2's records, so they are read out of that service's own store
/// through the one shared engine and kept on this store for the placements the clock makes
/// later ([`ComAmazonawsEcsAmazonec2containerservicev20141113State::record_subnet_placements`]).
///
/// LOCK ORDER: this takes the Amazon EC2 store's lock, holds no other while it does, and
/// hands back owned values — so it is called with NO guard of this service's store held,
/// exactly as [`unresolved_subnet_refusal`] is.
pub fn subnet_placements<Eng: Send + Sync + 'static>(
    engine: &Eng,
    subnets: &[String],
) -> Vec<(String, SubnetPlacement)> {
    if subnets.is_empty() {
        return Vec::new();
    }
    let Some(ec2) = ::service_com_amazonaws_ec2_amazonec2::state::service_state(engine) else {
        return Vec::new();
    };
    let store = ec2.lock();
    subnets
        .iter()
        .filter_map(|subnet| {
            let held = store.subnets.get(subnet.as_str())?;
            Some((
                subnet.clone(),
                SubnetPlacement {
                    availability_zone: held.availability_zone.clone()?,
                    cidr_block: held.cidr_block.clone(),
                },
            ))
        })
        .collect()
}

/// The subnets an `awsvpc` placement names, in the order it named them, and an empty list
/// for a placement that names none — the list [`subnet_placements`] is asked about.
pub fn configured_subnets(configuration: Option<&NetworkConfiguration>) -> Vec<String> {
    configuration
        .and_then(|configuration| configuration.awsvpc_configuration.as_ref())
        .map(|awsvpc| awsvpc.subnets.clone())
        .unwrap_or_default()
}
