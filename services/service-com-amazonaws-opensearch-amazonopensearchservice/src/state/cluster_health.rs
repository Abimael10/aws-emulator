// SPDX-License-Identifier: CC0-1.0

//! The SHARD ALLOCATION of a domain's cluster, and the health colour it leaves the cluster
//! in.
//!
//! An index is cut into primary shards and asks for a number of replica COPIES of each
//! ([`NUMBER_OF_SHARDS`], [`NUMBER_OF_REPLICAS`], both read off the index's own settings).
//! A copy of a shard needs a data node of its own — a replica cannot sit on the node that
//! holds its primary — so the nodes the domain's cluster configuration provisions decide how
//! many of the copies an index asked for can be ASSIGNED, and the rest stay unassigned
//! ([`assigned_replica_copies`]). That one arithmetic is what every reader of the cluster's
//! allocation is computed from, so no two of them can describe one cluster differently: the
//! cluster health a `GET /_cluster/health` answers, an index's `_cat` row, a write's
//! `_shards` summary, a refresh's, and the `AWS/ES` cluster-status series the domain
//! publishes ([`super::cluster_metrics`]).
//!
//! The health colour follows from that allocation alone, exactly as the engine defines it:
//! `red` while a PRIMARY has no node to sit on, `yellow` while every primary is assigned but
//! a replica copy is not, and `green` once every copy of every shard is assigned
//! ([`ShardAllocation::status`]). A provisioned cluster of this emulator always has a data
//! node to hold a primary, so its reading of the red indicator is zero unassigned primaries
//! rather than a colour nothing measured.

use super::{json_from_document, Domain, Index};

/// The most shards and replica copies one index's settings may ask for, as the engine's own
/// `index.number_of_shards` limit (`cluster.max_shards_per_node` times a node) bounds them.
/// A settings value past this is clamped for the allocation arithmetic rather than
/// multiplied out.
pub const MAX_SHARDS_PER_INDEX: i64 = 1_024;

/// The setting naming how many primary shards an index has, and the one naming how many
/// replica copies of each — the two the shard arithmetic reads ([`setting_number`]), named
/// once so no reader of the cluster's allocation spells them for itself.
pub const NUMBER_OF_SHARDS: &str = "number_of_shards";

/// See [`NUMBER_OF_SHARDS`].
pub const NUMBER_OF_REPLICAS: &str = "number_of_replicas";

/// The number an index's `settings` section gives `name`, under the `index` sub-section or
/// directly, as both spellings a client writes it with.
pub fn setting_number(schema: &::aws_smithy_types::Document, name: &str) -> Option<i64> {
    let settings = json_from_document(schema);
    let section = settings.get("settings")?;
    let value = section
        .get("index")
        .and_then(|nested| nested.get(name))
        .or_else(|| section.get(name))?;
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
}

/// How many DATA nodes the domain's cluster has, which is what decides how many copies of
/// a shard can be assigned: one node holds at most one copy of a shard.
pub fn data_nodes(domain: &Domain) -> i32 {
    domain.cluster_config.instance_count.unwrap_or(1).max(1)
}

/// How many of an index's `copies` replica copies per shard the cluster can ASSIGN: a
/// replica needs a node other than the one holding its primary, so the primary's node
/// leaves `data_nodes - 1` nodes for them and a copy past that has nowhere to go.
pub fn assigned_replica_copies(copies: i64, data_nodes: i32) -> i64 {
    copies.min(i64::from(data_nodes).saturating_sub(1).max(0))
}

/// The health `unassigned` unassigned shard copies leave the indexes they belong to in:
/// `yellow` while a copy has nowhere to go, `green` with every copy assigned.
///
/// The colour of ONE index's row, where the primary is always assignable — there is always a
/// data node to hold it — so no such row is `red`.
pub fn health_of(unassigned: i64) -> &'static str {
    if unassigned > 0 {
        ClusterStatus::Yellow.as_str()
    } else {
        ClusterStatus::Green.as_str()
    }
}

/// The three health colours a cluster's shard allocation leaves it in, as the engine names
/// them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ClusterStatus {
    /// Every copy of every shard is assigned to a node.
    Green,
    /// Every primary is assigned, and at least one replica copy is not.
    Yellow,
    /// At least one PRIMARY shard has no node to sit on.
    Red,
}

impl ClusterStatus {
    /// The three colours, healthiest first — the order [`Self::rank`] counts in.
    pub const ALL: [Self; 3] = [Self::Green, Self::Yellow, Self::Red];

    /// The name the engine reports this colour under, which is also the value
    /// `wait_for_status` names it by.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Green => "green",
            Self::Yellow => "yellow",
            Self::Red => "red",
        }
    }

    /// The colour `text` names, and `None` for a value that names none of them.
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|status| status.as_str() == text)
    }

    /// How the three colours order, worst last — what `wait_for_status` compares against.
    pub fn rank(self) -> u8 {
        match self {
            Self::Green => 0,
            Self::Yellow => 1,
            Self::Red => 2,
        }
    }

    /// Whether a cluster whose health is `reached` is of this colour, as the 0/1 INDICATOR a
    /// cluster-status series carries: one gauge per colour, of which exactly one reads 1 at
    /// any instant.
    pub fn indicator(self, reached: Self) -> f64 {
        if self == reached {
            1.0
        } else {
            0.0
        }
    }
}

/// The shard allocation of a set of indexes on one cluster: how many shard copies are
/// assigned to a node and how many have nowhere to go, primaries and replicas apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ShardAllocation {
    /// The primary shards sitting on a node.
    pub assigned_primaries: i64,
    /// The primary shards with no node to sit on.
    pub unassigned_primaries: i64,
    /// The replica copies sitting on a node.
    pub assigned_replicas: i64,
    /// The replica copies with no node to sit on.
    pub unassigned_replicas: i64,
}

impl ShardAllocation {
    /// How `indexes` are allocated over the data nodes `domain`'s cluster configuration
    /// provisions.
    ///
    /// Each index contributes the shards its settings ask for and the replica copies of each
    /// — both clamped before they are multiplied out, since a caller wrote them — and the
    /// copies past what the topology can hold are the unassigned ones. A primary needs only
    /// a data node, any one of them, so a cluster with a node to offer assigns every primary.
    pub fn of<'a>(domain: &Domain, indexes: impl IntoIterator<Item = &'a Index>) -> Self {
        let data_nodes = data_nodes(domain);
        let mut allocation = Self::default();
        for index in indexes {
            let shards = setting_number(&index.schema, NUMBER_OF_SHARDS)
                .unwrap_or(1)
                .clamp(1, MAX_SHARDS_PER_INDEX);
            let copies = setting_number(&index.schema, NUMBER_OF_REPLICAS)
                .unwrap_or(1)
                .clamp(0, MAX_SHARDS_PER_INDEX);
            let assigned = assigned_replica_copies(copies, data_nodes);
            // A primary needs A data node, any one of them, so a cluster with a node to offer
            // assigns every primary. The other branch is the engine's rule for a cluster with
            // no data node at all to put one on: a topology this emulator does not provision
            // ([`data_nodes`] reports at least one), which is why the red indicator its
            // readers publish measures zero rather than being written as one.
            if data_nodes > 0 {
                allocation.assigned_primaries = allocation.assigned_primaries.saturating_add(shards);
            } else {
                allocation.unassigned_primaries =
                    allocation.unassigned_primaries.saturating_add(shards);
            }
            allocation.assigned_replicas = allocation
                .assigned_replicas
                .saturating_add(shards.saturating_mul(assigned));
            allocation.unassigned_replicas = allocation
                .unassigned_replicas
                .saturating_add(shards.saturating_mul(copies.saturating_sub(assigned)));
        }
        allocation
    }

    /// How every index the domain holds is allocated — the whole cluster's allocation, which
    /// is what its health colour and its cluster-status series are read from.
    pub fn of_cluster(domain: &Domain) -> Self {
        Self::of(domain, domain.indexes.values())
    }

    /// The colour this allocation leaves the cluster in: `red` with a primary unassigned,
    /// `yellow` with every primary assigned and a replica copy not, `green` with every copy
    /// assigned — the engine's own definition.
    pub fn status(self) -> ClusterStatus {
        if self.unassigned_primaries > 0 {
            ClusterStatus::Red
        } else if self.unassigned_replicas > 0 {
            ClusterStatus::Yellow
        } else {
            ClusterStatus::Green
        }
    }

    /// The shard copies sitting on a node — `active_shards`.
    pub fn active(self) -> i64 {
        self.assigned_primaries.saturating_add(self.assigned_replicas)
    }

    /// Every shard copy the indexes asked for, assigned or not — `total`.
    pub fn total(self) -> i64 {
        self.active().saturating_add(self.unassigned())
    }

    /// The shard copies with nowhere to go — `unassigned_shards`.
    pub fn unassigned(self) -> i64 {
        self.unassigned_primaries
            .saturating_add(self.unassigned_replicas)
    }

    /// Every primary shard the indexes declare, assigned or not — `active_primary_shards`
    /// counts the assigned ones.
    pub fn primaries(self) -> i64 {
        self.assigned_primaries
            .saturating_add(self.unassigned_primaries)
    }
}
