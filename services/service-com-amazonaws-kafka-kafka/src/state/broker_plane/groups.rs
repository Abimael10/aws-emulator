// SPDX-License-Identifier: CC0-1.0

//! The CONSUMER GROUPS a cluster's brokers coordinate, and the protocol APIs a
//! consuming client drives them with.
//!
//! A client that consumes as part of a group does not read a partition it picked: it
//! joins the group, is told which partitions it owns, heartbeats while it holds them
//! and commits how far it has read. The broker is the group's COORDINATOR, so that
//! membership and those committed offsets live here, in this service's state: a
//! consumer that rejoins is given the generation the group is at, and one that asks for
//! its committed offsets is told the ones it committed.
//!
//! One socket serves the cluster, so this coordinator sees every member of a group: the
//! member that joins first is the group's LEADER and computes the assignment (which is
//! the client's half of the protocol), and `SyncGroup` hands each member the assignment
//! the leader computed for it.

use std::collections::BTreeMap;

use ::bytes::Bytes;
use ::kafka_protocol::messages::{
    join_group_response::JoinGroupResponseMember,
    offset_commit_response::{OffsetCommitResponsePartition, OffsetCommitResponseTopic},
    offset_fetch_response::{OffsetFetchResponsePartition, OffsetFetchResponseTopic},
    HeartbeatRequest, HeartbeatResponse, JoinGroupRequest, JoinGroupResponse, LeaveGroupRequest,
    LeaveGroupResponse, OffsetCommitRequest, OffsetCommitResponse, OffsetFetchRequest,
    OffsetFetchResponse, SyncGroupRequest, SyncGroupResponse, TopicName,
};
use ::kafka_protocol::protocol::StrBytes;

/// The committed offset a partition no member of the group has committed carries
/// (Apache Kafka's `OffsetFetch`: a partition with no committed offset answers `-1`).
pub const NO_COMMITTED_OFFSET: i64 = -1;

/// The most members one group holds, and the most offsets it keeps. CHOSEN as
/// process-safety bounds, not Apache Kafka settings: both come from clients, so without
/// a ceiling one client would size the store.
pub const MAX_MEMBERS_PER_GROUP: usize = 64;
/// See [`MAX_MEMBERS_PER_GROUP`].
pub const MAX_COMMITTED_PARTITIONS_PER_GROUP: usize = 512;
/// The most groups ONE CLUSTER's endpoint coordinates, and the most bytes a group id
/// may carry. CHOSEN for the same reason: a group id is the client's own string, so a
/// client that names a new group on every call must not be able to grow this store
/// without end.
pub const MAX_GROUPS_PER_CLUSTER: usize = 256;
/// See [`MAX_GROUPS_PER_CLUSTER`].
pub const MAX_GROUP_ID_BYTES: usize = 256;

/// The `OffsetFetch` answer for a group this endpoint holds no record of: every
/// partition the request named, each with no committed offset — which is what a group
/// nobody has joined has committed, and what lets the read register no group.
pub fn uncommitted_offsets(request: &OffsetFetchRequest) -> OffsetFetchResponse {
    let topics = request
        .topics
        .as_ref()
        .map(|topics| {
            topics
                .iter()
                .map(|topic| {
                    OffsetFetchResponseTopic::default()
                        .with_name(topic.name.clone())
                        .with_partitions(
                            topic
                                .partition_indexes
                                .iter()
                                .take(MAX_COMMITTED_PARTITIONS_PER_GROUP)
                                .map(|partition| {
                                    OffsetFetchResponsePartition::default()
                                        .with_partition_index(*partition)
                                        .with_committed_offset(NO_COMMITTED_OFFSET)
                                })
                                .collect(),
                        )
                })
                .collect()
        })
        .unwrap_or_default();
    OffsetFetchResponse::default().with_topics(topics)
}

/// One group this endpoint coordinates.
#[derive(Debug, Default)]
pub struct ConsumerGroup {
    /// The generation the group is at: every join advances it, which is what tells a
    /// member its assignment is stale.
    generation: i32,
    /// The members currently in the group, in join order — the first of them is the
    /// leader the assignment comes from.
    members: Vec<GroupMember>,
    /// How many member ids this group has minted, so each one is distinct.
    minted: u64,
    /// The offsets the group has committed, by topic and partition.
    offsets: BTreeMap<(String, i32), i64>,
}

/// One member of a group.
#[derive(Debug, Clone)]
struct GroupMember {
    /// The id the coordinator gave the member, which it quotes in every later call.
    id: String,
    /// The subscription the member joined with, which the leader reads to compute the
    /// assignment.
    metadata: Bytes,
}

impl ConsumerGroup {
    /// Forget every offset this group committed for the topic `topic`.
    ///
    /// A topic that has left the cluster took its partitions with it, so the offsets
    /// committed into them are offsets into nothing: a group reading a name created
    /// again starts where a group that has committed nothing starts, rather than at a
    /// position in a log that no longer exists.
    pub fn release_topic(&mut self, topic: &str) {
        self.offsets.retain(|(held, _), _| held != topic);
    }

    /// `JoinGroup`: admit the member — minting its id when it joined without one — and
    /// answer the group's generation, its leader and its members.
    ///
    /// The group's generation advances on every join, as a rebalance does.
    pub fn join(&mut self, request: &JoinGroupRequest) -> JoinGroupResponse {
        let protocol = request
            .protocols
            .first()
            .map(|protocol| protocol.name.to_string())
            .unwrap_or_default();
        let metadata = request
            .protocols
            .first()
            .map(|protocol| protocol.metadata.clone())
            .unwrap_or_default();
        let member_id = match request.member_id.is_empty() {
            false => request.member_id.to_string(),
            true => {
                self.minted += 1;
                format!("{}-{}", request.group_id.0.as_str(), self.minted)
            }
        };
        if !self.members.iter().any(|member| member.id == member_id) {
            if self.members.len() >= MAX_MEMBERS_PER_GROUP {
                return JoinGroupResponse::default().with_error_code(
                    ::kafka_protocol::ResponseError::GroupMaxSizeReached.code(),
                );
            }
            self.members.push(GroupMember {
                id: member_id.clone(),
                metadata,
            });
        } else if let Some(member) = self.members.iter_mut().find(|member| member.id == member_id)
        {
            member.metadata = metadata;
        }
        self.generation += 1;
        // The first member of the group is its leader, and the leader is the member
        // that is told about every other one: it computes the assignment.
        let leader = self
            .members
            .first()
            .map(|member| member.id.clone())
            .unwrap_or_default();
        let members = match member_id == leader {
            true => self
                .members
                .iter()
                .map(|member| {
                    JoinGroupResponseMember::default()
                        .with_member_id(StrBytes::from_string(member.id.clone()))
                        .with_metadata(member.metadata.clone())
                })
                .collect(),
            false => Vec::new(),
        };
        JoinGroupResponse::default()
            .with_generation_id(self.generation)
            // The protocol the group runs — the type the members joined with and the
            // assignor they named — is reported back, because a member checks that the
            // group it was admitted to runs the protocol it asked for.
            .with_protocol_type(Some(request.protocol_type.clone()))
            .with_protocol_name(Some(StrBytes::from_string(protocol)))
            .with_leader(StrBytes::from_string(leader))
            .with_member_id(StrBytes::from_string(member_id))
            .with_members(members)
    }

    /// `SyncGroup`: hand the member the assignment the group's leader computed for it.
    pub fn sync(&self, request: &SyncGroupRequest) -> SyncGroupResponse {
        let assignment = request
            .assignments
            .iter()
            .find(|assignment| assignment.member_id == request.member_id)
            .map(|assignment| assignment.assignment.clone())
            .unwrap_or_default();
        SyncGroupResponse::default()
            .with_protocol_type(request.protocol_type.clone())
            .with_protocol_name(request.protocol_name.clone())
            .with_assignment(assignment)
    }

    /// `Heartbeat`: a member that holds the group's current generation keeps its
    /// assignment; one quoting an older generation is told to rejoin, which is what
    /// `ILLEGAL_GENERATION` says.
    pub fn heartbeat(&self, request: &HeartbeatRequest) -> HeartbeatResponse {
        let error_code = match request.generation_id == self.generation {
            true => 0,
            false => ::kafka_protocol::ResponseError::IllegalGeneration.code(),
        };
        HeartbeatResponse::default().with_error_code(error_code)
    }

    /// `LeaveGroup`: take the member out, so a later join starts a new generation
    /// without it.
    pub fn leave(&mut self, request: &LeaveGroupRequest) -> LeaveGroupResponse {
        let leaving = request.member_id.to_string();
        self.members.retain(|member| member.id != leaving);
        for member in request.members.iter() {
            let leaving = member.member_id.to_string();
            self.members.retain(|member| member.id != leaving);
        }
        LeaveGroupResponse::default()
    }

    /// `OffsetCommit`: store how far the group has read each partition, for at most
    /// `max_entries` (topic, partition) entries of one request — the list comes from
    /// the client.
    pub fn commit(
        &mut self,
        request: &OffsetCommitRequest,
        max_entries: usize,
    ) -> OffsetCommitResponse {
        let mut topics = Vec::new();
        let mut entries = 0usize;
        for topic in request.topics.iter() {
            let name = topic.name.0.to_string();
            let mut partitions = Vec::new();
            for partition in topic.partitions.iter() {
                if entries >= max_entries {
                    continue;
                }
                entries += 1;
                let key = (name.clone(), partition.partition_index);
                let error_code = match self.offsets.contains_key(&key)
                    || self.offsets.len() < MAX_COMMITTED_PARTITIONS_PER_GROUP
                {
                    true => {
                        self.offsets.insert(key, partition.committed_offset);
                        0
                    }
                    false => ::kafka_protocol::ResponseError::OffsetMetadataTooLarge.code(),
                };
                partitions.push(
                    OffsetCommitResponsePartition::default()
                        .with_partition_index(partition.partition_index)
                        .with_error_code(error_code),
                );
            }
            topics.push(
                OffsetCommitResponseTopic::default()
                    .with_name(TopicName(StrBytes::from_string(name)))
                    .with_partitions(partitions),
            );
        }
        OffsetCommitResponse::default().with_topics(topics)
    }

    /// `OffsetFetch`: the offsets the group committed for the partitions the request
    /// names, or for every partition it has committed when it names none — at most
    /// [`MAX_COMMITTED_PARTITIONS_PER_GROUP`] of them, which is what the group can hold.
    pub fn fetch_offsets(&self, request: &OffsetFetchRequest) -> OffsetFetchResponse {
        let wanted: Vec<(String, Vec<i32>)> = match request.topics.as_ref() {
            Some(topics) => topics
                .iter()
                .map(|topic| {
                    (
                        topic.name.0.to_string(),
                        topic.partition_indexes.clone(),
                    )
                })
                .collect(),
            None => {
                let mut every: BTreeMap<String, Vec<i32>> = BTreeMap::new();
                for (topic, partition) in self.offsets.keys() {
                    every.entry(topic.clone()).or_default().push(*partition);
                }
                every.into_iter().collect()
            }
        };
        let topics = wanted
            .into_iter()
            .map(|(name, partitions)| {
                let reported = partitions
                    .into_iter()
                    .take(MAX_COMMITTED_PARTITIONS_PER_GROUP)
                    .map(|partition| {
                        let committed = self
                            .offsets
                            .get(&(name.clone(), partition))
                            .copied()
                            .unwrap_or(NO_COMMITTED_OFFSET);
                        OffsetFetchResponsePartition::default()
                            .with_partition_index(partition)
                            .with_committed_offset(committed)
                    })
                    .collect();
                OffsetFetchResponseTopic::default()
                    .with_name(TopicName(StrBytes::from_string(name)))
                    .with_partitions(reported)
            })
            .collect();
        OffsetFetchResponse::default().with_topics(topics)
    }
}
