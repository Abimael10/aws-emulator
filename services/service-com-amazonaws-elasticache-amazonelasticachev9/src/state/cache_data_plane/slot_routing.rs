// SPDX-License-Identifier: CC0-1.0

//! The SLOT MAP of a partitioned (cluster mode enabled) cache, and how a session on one
//! node answers a command whose keys another node owns.
//!
//! A cluster mode enabled replication group partitions its keyspace across its shards:
//! `NodeGroup$Slots` is "the keyspace for this node group (shard)", written `startkey-endkey`
//! over the [`super::super::KEYSPACE_SLOTS`] hash slots the engine divides a keyspace into,
//! and each shard's members serve ONLY the keys that hash into that shard's range. So the
//! three facts a cluster-aware client needs are facts of the control plane's own records:
//!
//! * WHICH slots a node serves — its shard's `Slots`;
//! * WHERE every other shard is — the address each of its members' node endpoint publishes,
//!   which is the address that member's own socket answers at;
//! * WHAT a command for a key of another shard gets — the engine's own `-MOVED`, naming the
//!   slot and the node that owns it, so the client re-sends it there and remembers the map.
//!
//! This module is that reading: the slot of a key ([`key_slot`], the engine's own CRC16 of
//! the hash tag), the map a group's shards make ([`ShardRouting`]), the refusal a command
//! gets on the wrong node ([`SlotRefusal`]) and the `CLUSTER` answers built from the map.
//! Nothing here holds a copy of the map: it is derived from the group's shards and its
//! members' published endpoints on every command, so a failover, a reshard or a member
//! replaced is in force on the sessions already open.

use ::redis_protocol::redis_keyslot;

use super::resp_frame::Reply;

/// The engine's own `-MOVED` refusal: the slot the command's key belongs to and the node
/// that serves that slot, which is where the client re-sends the command.
pub const MOVED: &str = "MOVED";

/// The engine's own refusal of a command whose keys do not all belong to ONE slot: a
/// partitioned keyspace serves such a command from no single node, so it is refused rather
/// than served in part.
pub const CROSS_SLOT_MESSAGE: &str = "CROSSSLOT Keys in request don't hash to the same slot";

/// The engine's own refusal of a command for a slot NO node of the partition serves: with the
/// keyspace not wholly covered there is no node to be redirected to, and the command is
/// refused rather than served out of a keyspace that does not own the key.
pub const CLUSTER_DOWN_MESSAGE: &str = "CLUSTERDOWN Hash slot not served";

/// How many hex characters the engine's own node name is.
const NODE_NAME_LENGTH: usize = 40;

/// The `role` a shard's member reports in `CLUSTER SHARDS`.
const ROLE_MASTER: &str = "master";
/// See [`ROLE_MASTER`].
const ROLE_REPLICA: &str = "replica";

/// One member of one shard, as a cluster-aware client reaches it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutedNode {
    /// The member cluster this node belongs to, which is what names it in the control
    /// plane.
    pub cache_cluster_id: String,
    /// The address its own socket answers at, as its node endpoint publishes it.
    pub host: String,
    /// See [`RoutedNode::host`].
    pub port: i32,
    /// Whether this node holds the shard's primary role: the node a write goes to.
    pub primary: bool,
}

impl RoutedNode {
    /// The engine's own name for this node: [`NODE_NAME_LENGTH`] hex characters, derived
    /// from the member cluster it is — so it is stable across reads and across sessions,
    /// and two nodes never share one.
    pub fn node_name(&self) -> String {
        node_name(&self.cache_cluster_id)
    }
}

/// One shard of a partitioned cache: the slots it serves and the nodes that serve them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShardRouting {
    /// `NodeGroup$NodeGroupId` — "the identifier for the node group (shard)".
    pub node_group_id: String,
    /// The first and last slot of `NodeGroup$Slots`, inclusive.
    pub slots: (u16, u16),
    /// The shard's members, the primary first.
    pub nodes: Vec<RoutedNode>,
}

impl ShardRouting {
    /// Whether this shard serves `slot`.
    pub fn serves(&self, slot: u16) -> bool {
        slot >= self.slots.0 && slot <= self.slots.1
    }

    /// The node a command for this shard is sent to: its primary, and its first member for
    /// a shard whose primary role no member holds.
    pub fn owner(&self) -> Option<&RoutedNode> {
        self.nodes
            .iter()
            .find(|node| node.primary)
            .or_else(|| self.nodes.first())
    }
}

/// Why a command cannot be served on the node it arrived at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotRefusal {
    /// The command's keys belong to more than one slot.
    CrossSlot,
    /// The command's key belongs to `slot`, which `host`:`port` serves.
    Moved { slot: u16, host: String, port: i32 },
    /// The command's key belongs to a slot no node of the partition serves.
    ClusterDown,
}

impl SlotRefusal {
    /// This refusal as the engine words it.
    pub fn reply(&self) -> Reply {
        match self {
            Self::CrossSlot => Reply::Error(CROSS_SLOT_MESSAGE.to_string()),
            Self::ClusterDown => Reply::Error(CLUSTER_DOWN_MESSAGE.to_string()),
            Self::Moved { slot, host, port } => {
                Reply::Error(format!("{MOVED} {slot} {host}:{port}"))
            }
        }
    }
}

/// The slot the key `key` belongs to: the engine's own CRC16 of its hash tag, modulo the
/// number of slots a keyspace is divided into.
///
/// The crate that frames this wire computes it (`redis_protocol::redis_keyslot`), so a key
/// written `{tag}suffix` lands where its tag lands — which is how a client puts the keys of
/// one multi-key command on one shard — rather than where a second reading of the rule
/// would put it.
#[must_use]
pub fn key_slot(key: &str) -> u16 {
    redis_keyslot(key.as_bytes())
}

/// The engine's own name for the node the member cluster `cache_cluster_id` is:
/// [`NODE_NAME_LENGTH`] hex characters of a digest of the id.
///
/// Derived rather than minted, so the name a node reports is the same on every read and in
/// every session, and distinct for distinct members — which is what a client's slot map
/// keys its nodes by.
#[must_use]
pub fn node_name(cache_cluster_id: &str) -> String {
    let mut digest = super::credential_digest(cache_cluster_id);
    digest.truncate(NODE_NAME_LENGTH);
    digest
}

/// Whether the keys of one command can be served by the shard serving `serving`, and the
/// refusal when they cannot.
///
/// `None` for a command this node serves: one that names no key, and one all of whose keys
/// belong to a slot this node's own shard serves. `serving` is the shard's slot range, and
/// `map` the whole partition, so the `-MOVED` names the node that really owns the slot — and a
/// slot no node of `map` serves is refused ([`SlotRefusal::ClusterDown`]) rather than answered
/// here, because a node that is not the owner holds none of that slot's keys.
#[must_use]
pub fn slot_refusal(
    keys: &[String],
    serving: Option<(u16, u16)>,
    map: &[ShardRouting],
) -> Option<SlotRefusal> {
    let mut slots = keys.iter().map(|key| key_slot(key));
    let slot = slots.next()?;
    // CROSSSLOT first, exactly as the engine answers it: a command whose keys straddle the
    // partition is served by no node, so there is no node to be MOVED to.
    if slots.any(|other| other != slot) {
        return Some(SlotRefusal::CrossSlot);
    }
    if serving.is_some_and(|(start, end)| slot >= start && slot <= end) {
        return None;
    }
    let owner = map
        .iter()
        .find(|shard| shard.serves(slot))
        .and_then(ShardRouting::owner);
    let Some(owner) = owner else {
        return Some(SlotRefusal::ClusterDown);
    };
    Some(SlotRefusal::Moved {
        slot,
        host: owner.host.clone(),
        port: owner.port,
    })
}

/// `CLUSTER SLOTS`: one entry per shard — its first and last slot, then its nodes, the
/// primary first, each as `[address, port, node name, []]`.
///
/// The addresses are the ones the control plane published for those nodes, so the map a
/// client builds from this reply names the very sockets it then connects to.
#[must_use]
pub fn cluster_slots_reply(map: &[ShardRouting]) -> Reply {
    Reply::Array(
        map.iter()
            .map(|shard| {
                let mut entry = vec![
                    Reply::Integer(i64::from(shard.slots.0)),
                    Reply::Integer(i64::from(shard.slots.1)),
                ];
                entry.extend(shard.nodes.iter().map(|node| {
                    Reply::Array(vec![
                        Reply::text(node.host.clone()),
                        Reply::Integer(i64::from(node.port)),
                        Reply::text(node.node_name()),
                        // The networking metadata the engine carries per node; this
                        // emulator publishes no additional names for one.
                        Reply::Array(Vec::new()),
                    ])
                }));
                Reply::Array(entry)
            })
            .collect(),
    )
}

/// `CLUSTER SHARDS`: the same partition as a map per shard — its slots, and its nodes with
/// the role each holds.
#[must_use]
pub fn cluster_shards_reply(map: &[ShardRouting]) -> Reply {
    Reply::Array(
        map.iter()
            .map(|shard| {
                Reply::Array(vec![
                    Reply::text("slots"),
                    Reply::Array(vec![
                        Reply::Integer(i64::from(shard.slots.0)),
                        Reply::Integer(i64::from(shard.slots.1)),
                    ]),
                    Reply::text("nodes"),
                    Reply::Array(
                        shard
                            .nodes
                            .iter()
                            .map(|node| {
                                Reply::Array(vec![
                                    Reply::text("id"),
                                    Reply::text(node.node_name()),
                                    Reply::text("endpoint"),
                                    Reply::text(node.host.clone()),
                                    Reply::text("ip"),
                                    Reply::text(node.host.clone()),
                                    Reply::text("port"),
                                    Reply::Integer(i64::from(node.port)),
                                    Reply::text("role"),
                                    Reply::text(if node.primary {
                                        ROLE_MASTER
                                    } else {
                                        ROLE_REPLICA
                                    }),
                                    Reply::text("health"),
                                    Reply::text("online"),
                                ])
                            })
                            .collect(),
                    ),
                ])
            })
            .collect(),
    )
}

/// `CLUSTER NODES`: one line per node, in the engine's own format —
/// `<name> <address>:<port>@<bus port> <flags> <primary> <ping> <pong> <epoch> <link> <slots>`.
///
/// `myself` marks the node the session is on, which is how a client reading this reply knows
/// where it is.
#[must_use]
pub fn cluster_nodes_reply(map: &[ShardRouting], myself: Option<&str>) -> Reply {
    let mut lines = String::new();
    for (epoch, shard) in map.iter().enumerate() {
        let primary_name = shard.owner().map(RoutedNode::node_name);
        for node in &shard.nodes {
            let name = node.node_name();
            let mine = myself.is_some_and(|id| id == node.cache_cluster_id);
            let role = if node.primary { "master" } else { "slave" };
            let flags = if mine {
                format!("myself,{role}")
            } else {
                role.to_string()
            };
            let follows = if node.primary {
                "-".to_string()
            } else {
                primary_name.clone().unwrap_or_else(|| "-".to_string())
            };
            let slots = if node.primary {
                format!(" {}-{}", shard.slots.0, shard.slots.1)
            } else {
                String::new()
            };
            // The cluster bus port is the client port plus the engine's own offset of
            // 10,000, which is the port it reports beside the client one.
            let bus = i64::from(node.port) + 10_000;
            lines.push_str(&format!(
                "{name} {host}:{port}@{bus} {flags} {follows} 0 0 {epoch} connected{slots}\n",
                host = node.host,
                port = node.port,
            ));
        }
    }
    Reply::Bulk(lines.into_bytes())
}
