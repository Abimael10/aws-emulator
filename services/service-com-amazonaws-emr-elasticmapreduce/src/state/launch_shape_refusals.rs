// SPDX-License-Identifier: CC0-1.0

//! The cluster SHAPES a launch may ask for, and how Amazon EMR refuses the ones it does
//! not provision.
//!
//! One concern, split out of [`super`]: a `RunJobFlow` request describes a cluster, and a
//! description Amazon EMR cannot provision is refused at the API rather than turned into a
//! cluster record no read could ever report truthfully. Each rule below is stated once,
//! with the sentence it is refused by, so the launch and any later caller of the same rule
//! word one refusal the same way:
//!
//! * an instance fleet has to target capacity it can provision, and the fleet that runs
//!   the primary nodes buys it in ONE market ([`instance_fleet_shape_refusal`]);
//! * a launch describes its capacity in ONE of the three forms `JobFlowInstancesConfig`
//!   offers ([`multiple_capacity_forms_refusal`]);
//! * a placement group is a placement group of the PRIMARY nodes of a multi-primary
//!   cluster, spread across the hardware ([`placement_group_refusal`]);
//! * a cluster with more than one primary node keeps its file system on core nodes
//!   ([`highly_available_core_group_refusal`]);
//! * more than one primary node is a capability of the RELEASE the cluster runs
//!   ([`multi_primary_release_refusal`]);
//! * a configuration classification is one its parent holds — the cluster at the top of a
//!   tree, the enclosing configuration below it ([`launch_classification_refusal`]).
//!
//! The sentences are recorded behavior: `RunJobFlow` models only `InternalServerError`, so
//! the class these refusals travel under is the request-validation envelope
//! [`crate::scaffolded::Scaffolded::validation`] builds, and what each sentence's FIXED
//! text is — and which parts of it come out of the request — is written on the constant or
//! the builder that renders it.

use std::borrow::Cow;

use super::*;

/// The instance groups a launch really creates: the ones it named, or the ones its uniform
/// `MasterInstanceType` / `SlaveInstanceType` / `InstanceCount` form implies
/// ([`implied_instance_group_configs`]).
///
/// Every rule that reads a launch's instance groups reads them through this, so a request
/// stating its capacity the uniform way is judged on the groups it provisions rather than
/// on the empty list it carried. `requested` is the group list the launch named, which the
/// handler has already taken off the request; the implied groups are derived only when
/// they are the ones the launch provisions, and never copied otherwise.
pub fn launch_instance_groups<'a>(
    instances: &JobFlowInstancesConfig,
    requested: &'a [InstanceGroupConfig],
) -> Cow<'a, [InstanceGroupConfig]> {
    if requested.is_empty() {
        return Cow::Owned(implied_instance_group_configs(instances));
    }
    Cow::Borrowed(requested)
}

/// How Amazon EMR names the instance fleet that hosts a node type, in a refusal about that
/// fleet: `MasterInstanceFleet`, `CoreInstanceFleet`, `TaskInstanceFleet`.
///
/// A fleet configuration carries an optional friendly `Name` (`InstanceFleetConfig$Name`),
/// so it is the node type the fleet HOSTS — "valid values are MASTER, CORE, and TASK"
/// (`InstanceFleetConfig$InstanceFleetType` doc) — that names the fleet in a refusal every
/// request earns the same way.
pub fn instance_fleet_display_name(instance_fleet_type: &str) -> String {
    let mut name = String::with_capacity(instance_fleet_type.len() + "InstanceFleet".len());
    for (index, character) in instance_fleet_type.chars().enumerate() {
        if index == 0 {
            name.extend(character.to_uppercase());
        } else {
            name.extend(character.to_lowercase());
        }
    }
    name.push_str("InstanceFleet");
    name
}

/// How Amazon EMR refuses an instance fleet that asks for no capacity in either market.
///
/// Recorded behavior for the text. Its fixed part is the whole sentence but the fleet's
/// name, which is the node type the fleet hosts ([`instance_fleet_display_name`]) — the one
/// thing a caller needs to know which of its fleets to give capacity to.
pub fn empty_instance_fleet_capacity_message(instance_fleet_type: &str) -> String {
    format!(
        "The instance fleet ({}) should have a value of one or greater for either \
         targetOnDemandCapacity or targetSpotCapacity.",
        instance_fleet_display_name(instance_fleet_type)
    )
}

/// How Amazon EMR refuses a PRIMARY instance fleet that asks for capacity in both markets.
///
/// Recorded behavior for the text, which is entirely fixed: nothing of the request is
/// interpolated into it.
pub const MASTER_FLEET_SINGLE_MARKET_MESSAGE: &str =
    "Configure the master instance fleet using either targetOnDemandCapacity or \
     targetSpotCapacity, but not both.";

/// How Amazon EMR refuses a launch that describes its capacity in more than one of the
/// forms `JobFlowInstancesConfig` offers.
///
/// Recorded behavior for the text, which is entirely fixed and names the three forms
/// themselves: nothing of the request is interpolated into it.
pub const INSTANCE_CAPACITY_SINGLE_FORM_MESSAGE: &str =
    "Please configure instances using one and only one of the following: instance groups; \
     instance fleets; instance count, master and slave instance type.";

/// How many of the three capacity forms `instances` describes the cluster with.
///
/// A `JobFlowInstancesConfig` states a cluster's capacity in one of three vocabularies:
/// `InstanceGroups` ("configuration for the instance groups in a cluster"),
/// `InstanceFleets` ("describes the Amazon EC2 instances and instance configurations for
/// clusters that use the instance fleet configuration") and the uniform form, whose
/// `MasterInstanceType` and `SlaveInstanceType` name "the Amazon EC2 instance type of the
/// master node" and "of the core and task nodes" (`JobFlowInstancesConfig` member docs). A
/// collection the request carried EMPTY describes no capacity in that vocabulary, and
/// neither does an empty instance type, so neither counts as a form the launch used.
///
/// `InstanceCount` is not counted on its own: it is the number of instances of the uniform
/// form, which names no node type to provision by itself.
fn stated_capacity_forms(instances: &JobFlowInstancesConfig) -> usize {
    let named = |instance_type: &Option<String>| {
        instance_type
            .as_deref()
            .is_some_and(|instance_type| !instance_type.is_empty())
    };
    let forms = [
        instances
            .instance_groups
            .as_ref()
            .is_some_and(|groups| !groups.is_empty()),
        instances
            .instance_fleets
            .as_ref()
            .is_some_and(|fleets| !fleets.is_empty()),
        named(&instances.master_instance_type) || named(&instances.slave_instance_type),
    ];
    forms.into_iter().filter(|stated| *stated).count()
}

/// The refusal a cluster launch earns for describing its capacity in more than one of the
/// three forms ([`stated_capacity_forms`]), or `None` when it uses at most one of them.
///
/// "The `RunJobFlow` request can contain `InstanceFleets` parameters or `InstanceGroups`
/// parameters, but not both" (`RunJobFlow` doc) is the rule for two of the three, and the
/// third — the uniform instance-count-and-node-types form — describes the same capacity in
/// its own vocabulary, so a request that mixes ANY two of them describes one cluster twice
/// and Amazon EMR cannot tell which description to provision. A launch that uses exactly
/// one form is then judged BY that form (a groups launch names one primary group,
/// [`super::primary_instance_group_refusal`]; a fleets launch's fleets target capacity they
/// can provision, [`instance_fleet_shape_refusal`]).
pub fn multiple_capacity_forms_refusal(
    instances: &JobFlowInstancesConfig,
) -> Option<&'static str> {
    (stated_capacity_forms(instances) > 1).then_some(INSTANCE_CAPACITY_SINGLE_FORM_MESSAGE)
}

/// The refusal one instance fleet of a cluster launch earns for the capacity it targets, or
/// `None` when the fleet can be provisioned as configured.
///
/// Two rules, both from the target-capacity members' own documentation. "At least one of
/// `TargetSpotCapacity` and `TargetOnDemandCapacity` should be greater than 0"
/// (`InstanceFleetConfig$TargetOnDemandCapacity` / `$TargetSpotCapacity` docs): a fleet
/// targeting no units of either market provisions nothing at all, whatever instance types
/// it offers, so no cluster can be launched with it. And "for a master instance fleet, only
/// one of `TargetSpotCapacity` and `TargetOnDemandCapacity` can be specified" (the same
/// docs): the fleet that runs the cluster's primary nodes buys them in ONE market, so a
/// primary fleet naming units of both describes a mixed-market primary nothing provisions.
///
/// A capacity the request leaves out asks for no units of that market — "if not specified
/// or set to 0, only Spot Instances are provisioned for the instance fleet" reads the
/// absent value and the explicit zero as one and the same request — and the emptiness rule
/// is read first, so a fleet that asks for nothing is told that rather than which market to
/// choose.
pub fn instance_fleet_shape_refusal(config: &InstanceFleetConfig) -> Option<String> {
    let on_demand = i64::from(config.target_on_demand_capacity.unwrap_or(0));
    let spot = i64::from(config.target_spot_capacity.unwrap_or(0));
    if on_demand < 1 && spot < 1 {
        return Some(empty_instance_fleet_capacity_message(
            &config.instance_fleet_type,
        ));
    }
    if config.instance_fleet_type == instance_group_type::MASTER && on_demand >= 1 && spot >= 1 {
        return Some(MASTER_FLEET_SINGLE_MARKET_MESSAGE.to_string());
    }
    None
}

/// `com.amazonaws.emr#PlacementGroupStrategy`'s "spread across distinct hardware" value,
/// the one strategy a cluster's primary nodes are placed with.
pub const PLACEMENT_GROUP_STRATEGY_SPREAD: &str = "SPREAD";

/// The placement strategies a placement group of the primary nodes may be configured with:
/// `SPREAD`, which places them on distinct underlying hardware, and
/// [`PLACEMENT_GROUP_STRATEGY_NONE`], which asks for no placement group at all.
///
/// "Starting with Amazon EMR release 5.23.0, the only supported placement strategy is
/// `SPREAD` for the `MASTER` instance role" (`PlacementGroupConfig$PlacementStrategy` doc),
/// and the enum's other values (`PARTITION`, `CLUSTER`) place instances in ways a cluster's
/// primaries are not placed in.
pub const MASTER_PLACEMENT_GROUP_STRATEGIES: &[&str] =
    &[PLACEMENT_GROUP_STRATEGY_SPREAD, PLACEMENT_GROUP_STRATEGY_NONE];

/// The instance roles a placement group may be configured for: the primary nodes alone.
///
/// "Starting with Amazon EMR release 5.23.0, the only supported instance role is `MASTER`"
/// (`PlacementGroupConfig$InstanceRole` doc) — the other roles of `InstanceRoleType`
/// (`CORE`, `TASK`) hold a cluster's workers, which Amazon EMR does not place.
pub const PLACEMENT_GROUP_INSTANCE_ROLES: &[&str] = &[instance_group_type::MASTER];

/// How Amazon EMR refuses a placement group configured for a role it does not place.
///
/// Recorded behavior for the text, which is entirely fixed — it names the roles that ARE
/// supported rather than the one the request asked for.
pub const PLACEMENT_GROUP_INSTANCE_ROLE_MESSAGE: &str =
    "Placement group supports only the following instance role: [MASTER]";

/// How Amazon EMR refuses a launch that configures ONE instance role with more than one
/// placement group entry.
///
/// Recorded behavior for the text: Amazon EMR states the rule — one placement strategy per
/// instance role — and then names the role the request configured more than once, which is
/// the one variable part of the sentence.
pub fn multiple_placement_strategies_message(instance_role: &str) -> String {
    format!(
        "Placement group supports single placement strategy for an instance role. Found \
         multiple strategy for the following instance role: {instance_role}"
    )
}

/// The instance role a launch's `PlacementGroupConfigs` configure more than once, in the
/// order the request spelled the entries, or `None` when every entry names a role of its
/// own.
///
/// "The configuration specifies the placement strategy that can be applied to instance
/// roles during cluster creation" (`PlacementGroupConfig` doc) — one role is placed under
/// one strategy ([`super::cluster_instances`] places it that way too), so a second entry
/// for a role the request already configured names a second strategy for it, whichever
/// strategies the two entries carry.
fn role_configured_twice(configs: &[PlacementGroupConfig]) -> Option<&str> {
    configs.iter().enumerate().find_map(|(at, config)| {
        configs[..at]
            .iter()
            .any(|earlier| earlier.instance_role == config.instance_role)
            .then_some(config.instance_role.as_str())
    })
}

/// How Amazon EMR refuses a primary placement group configured with a strategy it does not
/// place primaries with. Recorded behavior, and entirely fixed text.
pub const MASTER_PLACEMENT_GROUP_STRATEGY_MESSAGE: &str =
    "Placement group for master node supports only the following placement strategy: \
     [SPREAD, NONE]";

/// How Amazon EMR refuses a primary placement group on a cluster that is not multi-primary.
/// Recorded behavior, and entirely fixed text: the same sentence whether the cluster has
/// fewer primary nodes than a multi-primary cluster or more.
pub const MASTER_PLACEMENT_GROUP_MULTI_MASTER_MESSAGE: &str =
    "Placement group support for master node is available only for multi-master cluster \
     and not for single-master cluster.";

/// The primary nodes a MULTI-PRIMARY cluster runs: the topology Amazon EMR provisions a
/// placement group of the primaries for.
///
/// Amazon EMR runs one primary node, or the three of a multi-primary cluster — one per
/// Availability Zone of the placement group that spreads them — so a count that is neither
/// is not a cluster whose primaries can be placed.
pub const MULTI_PRIMARY_NODES: i64 = 3;

/// The refusal a launch's `PlacementGroupConfigs` earn, or `None` when every entry
/// configures a placement group this cluster can hold.
///
/// Four readings, in the order a caller can act on them. WHICH ROLE may be placed at all
/// ([`PLACEMENT_GROUP_INSTANCE_ROLES`]), whether that role is configured ONCE
/// ([`role_configured_twice`]) and WHICH STRATEGY places it
/// ([`MASTER_PLACEMENT_GROUP_STRATEGIES`]) are facts about the entries themselves, so they
/// are read before the cluster is looked at; an entry that names no strategy names none to
/// reject, and still asks for a placement group. WHETHER THE CLUSTER HAS primaries to place
/// is the cross-field reading: a placement group of the primary nodes exists to spread the
/// primaries of a multi-primary cluster ([`MULTI_PRIMARY_NODES`]) over distinct hardware, so
/// a cluster with any other primary count is offered the configuration at all — which is
/// what the sentence states — and a launch that CARRIES one for it is refused whichever
/// strategy the entry names, the `NONE` strategy included: placing the primary role is a
/// capability of the cluster's topology rather than of the strategy asked for, and an entry
/// asking for `NONE` still configures that role (it is only the GROUP that is then not
/// created, [`created_placement_groups`]).
///
/// A role configured more than once is read before the strategies those entries carry: the
/// request has asked for two placements of one role, which is what a caller fixes, and no
/// single strategy of theirs is the one to report on.
///
/// `primary_nodes` is the primary count of the cluster the launch describes, counted off
/// the groups and fleets it provisions ([`primary_node_count`]).
pub fn placement_group_refusal(
    configs: &[PlacementGroupConfig],
    primary_nodes: i64,
) -> Option<String> {
    if configs
        .iter()
        .any(|config| !PLACEMENT_GROUP_INSTANCE_ROLES.contains(&config.instance_role.as_str()))
    {
        return Some(PLACEMENT_GROUP_INSTANCE_ROLE_MESSAGE.to_string());
    }
    if let Some(role) = role_configured_twice(configs) {
        return Some(multiple_placement_strategies_message(role));
    }
    if configs.iter().any(|config| {
        config
            .placement_strategy
            .as_deref()
            .is_some_and(|strategy| !MASTER_PLACEMENT_GROUP_STRATEGIES.contains(&strategy))
    }) {
        return Some(MASTER_PLACEMENT_GROUP_STRATEGY_MESSAGE.to_string());
    }
    if !configs.is_empty() && primary_nodes != MULTI_PRIMARY_NODES {
        return Some(MASTER_PLACEMENT_GROUP_MULTI_MASTER_MESSAGE.to_string());
    }
    None
}

/// How Amazon EMR refuses a highly available cluster that carries no core instance group.
/// Recorded behavior, and entirely fixed text: it names what is missing and what to do.
pub const HIGHLY_AVAILABLE_CORE_GROUP_MESSAGE: &str =
    "A HA based cluster must have core instance group. Please specify a core instance \
     group and resubmit.";

/// The refusal a highly available launch earns for carrying no core instance group, or
/// `None` when the cluster it describes can hold its file system.
///
/// A cluster with more than one primary node ([`MIN_HIGHLY_AVAILABLE_PRIMARY_NODES`]) is a
/// highly available cluster, and such a cluster keeps the file system its primaries
/// coordinate on CORE nodes — "valid values are MASTER, CORE or TASK"
/// (`InstanceGroupConfig$InstanceRole` doc), and the core role is the one that holds
/// storage — so a launch that describes several primaries and no core group describes a
/// cluster with nowhere to put its data.
///
/// `groups` is what the launch provisions ([`launch_instance_groups`]). A launch stating its
/// capacity as instance FLEETS states its roles in that other vocabulary and carries no
/// instance group at all, so the reading is asked of the groups form, which is also the
/// vocabulary the sentence speaks.
pub fn highly_available_core_group_refusal(
    groups: &[InstanceGroupConfig],
) -> Option<&'static str> {
    let primary_nodes = primary_node_count(groups, &[]);
    if primary_nodes < MIN_HIGHLY_AVAILABLE_PRIMARY_NODES {
        return None;
    }
    let holds_core = groups
        .iter()
        .any(|group| group.instance_role == instance_group_type::CORE);
    (!holds_core).then_some(HIGHLY_AVAILABLE_CORE_GROUP_MESSAGE)
}

/// The first Amazon EMR release that provisions more than one primary node.
///
/// CATALOG DATA, from the Release Guide's multiple-primary-node page — the same release the
/// model's own members date the capability from: "starting with Amazon EMR release 5.23.0,
/// the only supported instance role is `MASTER`" and the placement strategy sentence beside
/// it (`PlacementGroupConfig` member docs) both name the release that introduced the
/// multi-primary topology, which is the release the placement group of the primaries came
/// with.
pub const FIRST_MULTI_PRIMARY_RELEASE: (u32, u32, u32) = (5, 23, 0);

/// How Amazon EMR refuses more than one primary node on a release that provisions one.
/// Recorded behavior, and entirely fixed text: it states the shape the release does
/// provision.
pub const SINGLE_PRIMARY_INSTANCE_GROUP_MESSAGE: &str =
    "A master instance group must specify a single instance.";

/// The refusal a launch earns for asking an older release for several primary nodes, or
/// `None` when the release it names provisions the cluster it describes.
///
/// More than one primary node is a capability of the RELEASE the cluster runs
/// ([`FIRST_MULTI_PRIMARY_RELEASE`]), and "the Amazon EMR release label … determines the
/// version of open-source application packages installed on the cluster"
/// (`RunJobFlow$ReleaseLabel` doc) is the one thing in the request that names it, so a
/// primary group of several instances on a release published before the capability
/// describes a cluster that release cannot run.
///
/// The release is the one the LABEL names ([`release_version`]), which is a reading of the
/// label alone: which patch releases this service's catalog publishes is a different
/// question, asked of a launch separately, so the capability a label dates itself to is
/// settled whether or not that catalog holds the label. A label in no `emr-x.y.z` form, and
/// a launch that names no release label at all, name no release to read — the Spark release
/// family carries its own prefix and every release of it is later than the capability — so
/// neither is judged here.
pub fn multi_primary_release_refusal(
    release_label: Option<&str>,
    groups: &[InstanceGroupConfig],
) -> Option<&'static str> {
    if primary_node_count(groups, &[]) <= 1 {
        return None;
    }
    let release = release_label.and_then(release_version)?;
    (release < FIRST_MULTI_PRIMARY_RELEASE).then_some(SINGLE_PRIMARY_INSTANCE_GROUP_MESSAGE)
}

/// How Amazon EMR refuses a configuration classification nested under a parent that does
/// not hold it.
///
/// Recorded behavior for the text. Its fixed parts are the three fragments below; its
/// variable parts are the two classification names, both taken from the request's own
/// configuration tree — the child that was nested and the parent it was nested under — so a
/// caller is told exactly which pair of its tree to separate.
pub fn invalid_nested_classification_message(child: &str, parent: &str) -> String {
    format!("Classification '{child}' is not valid for parent classification '{parent}'.")
}

/// The suffix of the classification of an application's ENVIRONMENT file, whose
/// configuration is the exported variables of one of its sections.
const ENVIRONMENT_CLASSIFICATION_SUFFIX: &str = "-env";

/// The one child classification an environment file holds: the section whose properties
/// become exported variables.
///
/// CATALOG DATA, from the Release Guide's "Configuring applications" page, every example of
/// which nests exactly this classification under an `…-env` classification.
const EXPORT_CLASSIFICATION: &str = "export";

/// Whether the classification `parent` holds a nested configuration of the classification
/// `child`.
///
/// "A list of additional configurations to apply within a configuration object"
/// (`Configuration$Configurations` doc) is a configuration INSIDE another one, so which
/// pairs are valid follows from what a classification names:
///
/// * an APPLICATION classification — a bare name with no file part, such as the one a
///   cluster-wide application property is set under — holds the configuration files of that
///   same application, which are the classifications named `<application>-<file>`;
/// * an ENVIRONMENT file ([`ENVIRONMENT_CLASSIFICATION_SUFFIX`]) holds the section whose
///   properties it exports ([`EXPORT_CLASSIFICATION`]);
/// * every other classification is a property file, which holds properties and no further
///   configuration.
fn parent_holds_classification(parent: &str, child: &str) -> bool {
    if parent.ends_with(ENVIRONMENT_CLASSIFICATION_SUFFIX) {
        return child == EXPORT_CLASSIFICATION;
    }
    if !parent.contains('-') {
        return child
            .strip_prefix(parent)
            .is_some_and(|file| file.starts_with('-') && file.len() > 1);
    }
    false
}

/// How deep a configuration tree this reading walks.
///
/// CHOSEN, not modelled: `ConfigurationList` carries no depth bound, so the walk is bounded
/// by construction the way the served tree's is ([`served_configurations`]) and a request
/// nested deeper than any configuration a cluster holds cannot drive work without end. A
/// tree is judged to this depth and the levels below it are not read.
const MAX_VALIDATED_CONFIGURATION_DEPTH: usize = 100;

/// The refusal a configuration tree earns for a classification its parent does not hold, or
/// `None` when every classification in it is one Amazon EMR applies there.
///
/// Two readings of one rule — a classification is applied only where its parent holds it.
/// At the TOP of a tree the parent is the cluster itself, which holds exactly the
/// classifications Amazon EMR publishes ([`is_published_classification`]), and the refusal
/// names that parent as [`super::ABSENT_VALUE`] because the configuration has none; BELOW
/// the top the parent is the enclosing configuration ([`parent_holds_classification`]).
///
/// A configuration that names no classification names nothing to apply: neither it nor its
/// children are judged against a parent, so the reading is of the classifications the tree
/// states.
pub fn nested_classification_refusal(configurations: &[Configuration]) -> Option<String> {
    for configuration in configurations {
        let Some(classification) = configuration.classification.as_deref() else {
            continue;
        };
        if !is_published_classification(classification) {
            return Some(invalid_nested_classification_message(
                classification,
                super::ABSENT_VALUE,
            ));
        }
    }
    nested_classification_refusal_within(configurations, MAX_VALIDATED_CONFIGURATION_DEPTH)
}

/// [`nested_classification_refusal`], with `depth` levels of the tree left to read.
fn nested_classification_refusal_within(
    configurations: &[Configuration],
    depth: usize,
) -> Option<String> {
    let remaining = depth.checked_sub(1)?;
    for configuration in configurations {
        let children = configuration.configurations.as_deref().unwrap_or_default();
        if let Some(parent) = configuration.classification.as_deref() {
            for child in children {
                let Some(classification) = child.classification.as_deref() else {
                    continue;
                };
                if !parent_holds_classification(parent, classification) {
                    return Some(invalid_nested_classification_message(classification, parent));
                }
            }
        }
        if let Some(message) = nested_classification_refusal_within(children, remaining) {
            return Some(message);
        }
    }
    None
}

/// The refusal a cluster launch earns for any configuration tree it carries, or `None` when
/// every classification in them is one its parent holds.
///
/// A launch states configurations in every place a cluster holds them — for the cluster
/// itself (`RunJobFlow$Configurations`), for one instance group
/// (`InstanceGroupConfig$Configurations`) and for one instance type of an instance fleet
/// (`InstanceTypeConfig$Configurations`) — and they are all the same shape applied to the
/// same software, so one reading judges them all and no path into the record carries a
/// nesting the others refuse.
pub fn launch_classification_refusal(
    configurations: &[Configuration],
    groups: &[InstanceGroupConfig],
    fleets: &[InstanceFleetConfig],
) -> Option<String> {
    if let Some(message) = nested_classification_refusal(configurations) {
        return Some(message);
    }
    for group in groups {
        let configurations = group.configurations.as_deref().unwrap_or_default();
        if let Some(message) = nested_classification_refusal(configurations) {
            return Some(message);
        }
    }
    for fleet in fleets {
        for config in fleet.instance_type_configs.iter().flatten() {
            let configurations = config.configurations.as_deref().unwrap_or_default();
            if let Some(message) = nested_classification_refusal(configurations) {
                return Some(message);
            }
        }
    }
    None
}
