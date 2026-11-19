// SPDX-License-Identifier: CC0-1.0

//! The path through EC2's topology tree a resource's hardware hangs off, as every topology
//! report names its nodes.
//!
//! EC2 reports where a resource sits in the Amazon Web Services network as a list of node
//! ids, from the node of the Availability Zone's network at the top down to the hosts the
//! resource's own capacity is on. "The nodes are hashed based on your account. Instances
//! from different accounts running under the same server will return a different hashed list
//! of strings" (`InstanceTopology$NetworkNodes`, and the same sentence on
//! `CapacityReservationTopology$NetworkNodes`), so a node id is a pure function of the
//! owning account and of every level of the path down to it: nothing is stored, every read
//! answers the same ids, and two resources share exactly the levels at which their
//! placements agree.
//!
//! Both reports of the tree read it from here, so the instance report and the Capacity
//! Reservation report cannot disagree about the id of a node the two resources share — a
//! reservation and an instance placed in the same zone on the same instance type hang off
//! the same ancestors — and only WHICH levels place a resource stays with the operation
//! that reports it.
//!
//! ## Which instance types have a topology at all
//!
//! "Instance topology is supported for specific instance types only. For more information,
//! see Prerequisites for Amazon EC2 instance topology in the Amazon EC2 User Guide"
//! (`DescribeInstanceTopology`). That page lists the types as the HPC families and the
//! accelerated-computing families built for tightly coupled workloads, enumerating every
//! size each of those families has — so the fact is one of the FAMILY, which is how
//! [`instance_type_supports_topology`] reads it: a size of a listed family that the page's
//! snapshot does not name is still a type of a supported family, and no family outside the
//! list becomes supported by having a large size.

/// The instance-type families AWS supports instance topology for, from "Prerequisites for
/// Amazon EC2 instance topology" (Amazon EC2 User Guide), the page
/// `DescribeInstanceTopology`'s own documentation points at: the HPC families (`hpc6a`,
/// `hpc6id`, `hpc7a`, `hpc7g`) and the accelerated-computing families for tightly coupled
/// workloads (`p3dn`, `p4d`, `p4de`, `p5`, `p5e`, `p5en`, `trn1`, `trn1n`, `trn2`, `trn2u`).
///
/// A family, not a type list: the page enumerates the sizes each listed family has, and the
/// modelled `InstanceType` catalog carries sizes of those same families the enumeration
/// predates (`p5.4xlarge`, `trn2.3xlarge`), which are types of a supported family rather
/// than types of a new one.
pub const TOPOLOGY_SUPPORTED_INSTANCE_FAMILIES: [&str; 14] = [
    "hpc6a", "hpc6id", "hpc7a", "hpc7g", "p3dn", "p4d", "p4de", "p5", "p5e", "p5en", "trn1",
    "trn1n", "trn2", "trn2u",
];

/// Does AWS support instance topology for `instance_type`?
///
/// The family [`super::InstanceTypeName`] parses out of the name, read against
/// [`TOPOLOGY_SUPPORTED_INSTANCE_FAMILIES`] — and against the family's base for a name whose
/// family carries a sub-model (`p6-b200`), since a sub-model is a member of its family rather
/// than a family of its own. A value that is not an instance type name at all names no
/// supported family.
pub fn instance_type_supports_topology(instance_type: &str) -> bool {
    super::InstanceTypeName::parse(instance_type).is_some_and(|name| {
        let base = name.family.split('-').next().unwrap_or(name.family);
        TOPOLOGY_SUPPORTED_INSTANCE_FAMILIES.contains(&name.family)
            || TOPOLOGY_SUPPORTED_INSTANCE_FAMILIES.contains(&base)
    })
}

/// The id prefix EC2 gives one node of its topology tree: `nn-` and the 17 lowercase
/// hexadecimal digits every long-form EC2 resource id carries
/// ([`super::prefixed_id_is_well_formed`]), which is the form the node ids in the Amazon EC2
/// API Reference's own topology examples take.
pub const TOPOLOGY_NETWORK_NODE_ID_PREFIX: &str = "nn";

/// The nodes of the path `levels` names, from the tree's root down: one node per level, each
/// named by the account and by every level above it as well as its own, so two resources
/// share a node exactly when they agree on every level down to it.
///
/// `owner_id` is the account the resource belongs to and `levels` the placement facts that
/// put it where it is, outermost first.
pub fn topology_network_nodes(owner_id: &str, levels: &[&str]) -> Vec<String> {
    let mut path: Vec<&str> = Vec::with_capacity(levels.len() + 1);
    path.push(owner_id);
    let mut nodes: Vec<String> = Vec::with_capacity(levels.len());
    for level in levels {
        path.push(level);
        nodes.push(topology_network_node_id(&path));
    }
    nodes
}

/// The id of the topology node `path` names: [`TOPOLOGY_NETWORK_NODE_ID_PREFIX`] and the
/// digest of the account and of every level of the path down to it, in the 17 digits a
/// long-form EC2 id carries — whose leading digit is `0`, because that form holds 68 bits
/// for a 64-bit identifier, which is also what
/// [`super::ComAmazonawsEc2Amazonec2State::next_id`] mints.
///
/// The standard library's hasher, seeded by its own default rather than randomly, so a node
/// id is a pure function of its path: every read of one placement answers the same id, and
/// two placements that differ at any level get different ones. Nothing here is a secret and
/// nothing depends on a node id being unforgeable — it is an opaque name for a position in
/// the tree.
pub fn topology_network_node_id(path: &[&str]) -> String {
    use ::std::hash::{Hash, Hasher};

    let mut hasher = ::std::hash::DefaultHasher::new();
    for level in path {
        level.hash(&mut hasher);
    }
    format!("{TOPOLOGY_NETWORK_NODE_ID_PREFIX}-{:017x}", hasher.finish())
}
