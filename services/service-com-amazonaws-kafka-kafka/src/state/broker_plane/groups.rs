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
//! One socket per broker serves the cluster, so this coordinator sees every member of a
//! group, and what it runs is the REBALANCE Apache Kafka's group protocol defines rather
//! than a per-connection answer:
//!
//! * a `JoinGroup` from a member the group does not already hold — or from one the group
//!   holds while it is stable — starts a rebalance, which advances the generation ONCE
//!   when the join phase completes and asks every other member to rejoin it. The
//!   coordinator answers the joins only once every member has rejoined (or the rebalance
//!   barrier has run out on the ones that did not), so every member of a generation is
//!   handed the same membership — which is what makes the group's assignment agree across
//!   its members.
//! * the first member to rejoin a generation is its LEADER, and the leader alone is told
//!   the whole membership: computing the assignment is the client's half of the protocol.
//!   `SyncGroup` from the leader STORES the assignment it computed for each member, and
//!   every other member's `SyncGroup` is answered the assignment the leader stored for it
//!   — a follower sends no assignment of its own, so answering it out of its own request
//!   would hand it nothing and leave it consuming no partition at all.
//! * a `Heartbeat` while a rebalance is in flight answers `REBALANCE_IN_PROGRESS`, which
//!   is how a member that is holding an assignment learns to rejoin; one quoting a
//!   generation the group has left answers `ILLEGAL_GENERATION`, and one the group does
//!   not hold answers `UNKNOWN_MEMBER_ID`.
//!
//! A member that stops heartbeating is dropped once its session timeout has run out, and
//! that drop rebalances the group: the removal was owed at the instant the session
//! expired whether or not anybody called, so every read of a group materialises it
//! ([`ConsumerGroup::expire_members`]) rather than keeping a member no client is behind.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

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
use ::kafka_protocol::ResponseError;

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

/// The window a member's session lives for when it names none, and the bounds a named
/// one is read within: Apache Kafka's documented `group.min.session.timeout.ms` (6000)
/// and `group.max.session.timeout.ms` (1800000) ("Broker Configs", Apache Kafka
/// documentation). A member that does not heartbeat within its session timeout has left
/// the group.
const MIN_SESSION_TIMEOUT_MS: i64 = 6_000;
/// See [`MIN_SESSION_TIMEOUT_MS`].
const MAX_SESSION_TIMEOUT_MS: i64 = 1_800_000;

/// The longest the coordinator holds one member's `JoinGroup` while it waits for the
/// group's other members to rejoin the rebalance.
///
/// CHOSEN as a process-safety bound, not an Apache Kafka setting: a client states its own
/// `RebalanceTimeoutMs` (whose default is a client's whole `max.poll.interval.ms`, five
/// minutes), and a member that is never coming back would hold the joining member's
/// connection for all of it. A member that has not rejoined within this window is treated
/// as having left, which starts the generation without it — and since a rejoin follows the
/// member's next heartbeat, this is sized well above the heartbeat intervals clients use.
pub const MAX_REBALANCE_BARRIER: Duration = Duration::from_secs(15);

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

/// Where one group is in the protocol: what a call about it is answered depends on it,
/// exactly as it does on Apache Kafka's own coordinator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GroupState {
    /// No member holds the group.
    #[default]
    Empty,
    /// A rebalance is gathering the group's members at a new generation.
    PreparingRebalance,
    /// Every member has rejoined and the leader has not stated the assignment yet.
    CompletingRebalance,
    /// The leader's assignment is in force and every member holds its own part of it.
    Stable,
}

impl GroupState {
    /// The name Apache Kafka reports this state under (`DescribeGroups`/`ListGroups`
    /// report a group's state as a string).
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Empty => "Empty",
            Self::PreparingRebalance => "PreparingRebalance",
            Self::CompletingRebalance => "CompletingRebalance",
            Self::Stable => "Stable",
        }
    }
}

/// One member of a group as `DescribeGroups` reports it: what it joined with and what it
/// was assigned.
#[derive(Debug, Clone)]
pub struct DescribedMember {
    /// The id the coordinator gave the member.
    pub id: String,
    /// The id the member's own client gave itself — the `client_id` it sent with its
    /// join. What a reader of the group tells one member from another by.
    pub client_id: String,
    /// The subscription it joined with.
    pub metadata: Bytes,
    /// The part of the group's assignment that is its own, empty while the group has
    /// none in force.
    pub assignment: Bytes,
}

/// One group this endpoint coordinates.
#[derive(Debug, Default)]
pub struct ConsumerGroup {
    /// The generation the group is at: every REBALANCE advances it once, which is what
    /// tells a member its assignment is stale.
    generation: i32,
    /// Where the group is in the protocol.
    state: GroupState,
    /// The members currently in the group, in join order.
    members: Vec<GroupMember>,
    /// The member that computes the assignment: the first to rejoin the current
    /// generation.
    leader: String,
    /// The protocol type the group runs ("consumer" for a consuming client), and the
    /// assignor its members named — both reported back, because a member checks that the
    /// group it was admitted to runs the protocol it asked for.
    protocol_type: Option<String>,
    /// See [`ConsumerGroup::protocol_type`].
    protocol_name: Option<String>,
    /// How many member ids this group has minted, so each one is distinct.
    minted: u64,
    /// The offsets the group has committed, by topic and partition.
    offsets: BTreeMap<(String, i32), i64>,
    /// The instant the coordinator stops waiting for members that have not rejoined the
    /// rebalance in flight. Stored, never slept on.
    rebalance_deadline: Option<SystemTime>,
    /// How many members have rejoined the rebalance in flight, which ORDERS them: the
    /// first to rejoin leads the generation it forms.
    rejoins: u64,
}

/// One member of a group.
#[derive(Debug, Clone)]
struct GroupMember {
    /// The id the coordinator gave the member, which it quotes in every later call.
    id: String,
    /// The `client_id` the member sent with its join: the name its own client goes by,
    /// which a reader of the group reports it under.
    client_id: String,
    /// The subscription the member joined with, which the leader reads to compute the
    /// assignment.
    metadata: Bytes,
    /// Where in the rebalance in flight this member rejoined, and `None` while it has
    /// not rejoined it — which is what the coordinator waits for, and what orders the
    /// members so the first to rejoin leads the generation.
    rejoined: Option<u64>,
    /// The part of the leader's assignment that is this member's, once the leader has
    /// stated it.
    assignment: Option<Bytes>,
    /// The instant this member's session runs out unless it is heard from again.
    session_deadline: SystemTime,
    /// How long each call from this member renews that instant by: the session timeout
    /// it joined with.
    session_window: Duration,
}

/// What a member's `JoinGroup` established: the id it is known by, while it waits for
/// the rebalance it is part of to gather the group's members.
#[derive(Debug, Clone)]
pub struct Joining {
    /// The member's id — minted when it joined without one.
    pub member_id: String,
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

    /// Where the group is in the protocol.
    #[must_use]
    pub fn state(&self) -> GroupState {
        self.state
    }

    /// The protocol type the group's members joined with ("consumer" for a consuming
    /// client), and the assignor they named — what a reader of the group reports.
    #[must_use]
    pub fn protocol_type(&self) -> Option<&str> {
        self.protocol_type.as_deref()
    }

    /// See [`Self::protocol_type`].
    #[must_use]
    pub fn protocol_name(&self) -> Option<&str> {
        self.protocol_name.as_deref()
    }

    /// Whether any member holds the group.
    #[must_use]
    pub fn has_members(&self) -> bool {
        !self.members.is_empty()
    }

    /// The group's members as a reader of the group reports them.
    #[must_use]
    pub fn described_members(&self) -> Vec<DescribedMember> {
        self.members
            .iter()
            .map(|member| DescribedMember {
                id: member.id.clone(),
                client_id: member.client_id.clone(),
                metadata: member.metadata.clone(),
                assignment: member.assignment.clone().unwrap_or_default(),
            })
            .collect()
    }

    /// Drop every member whose session the clock has already run out, and rebalance the
    /// group when that leaves it with a membership the generation in force was not
    /// computed for.
    ///
    /// The removal was owed at the instant the session expired whether or not anybody
    /// called, so materialising it on a read reports the group the clock already left
    /// behind rather than keeping a member no client is behind.
    ///
    /// A member whose `JoinGroup` the coordinator is HOLDING is not such a member: it has
    /// rejoined the rebalance in flight and is waiting on the coordinator's own answer,
    /// so it is in contact with it, and what bounds that wait is the rebalance barrier
    /// ([`MAX_REBALANCE_BARRIER`]) rather than the session timeout a client renews by
    /// heartbeating. Expiring it would answer its join `UNKNOWN_MEMBER_ID` for not
    /// heartbeating while the coordinator held the call it was waiting for, which leaves
    /// a client rejoining for as long as the group keeps gathering.
    pub fn expire_members(&mut self, now: SystemTime) {
        let before = self.members.len();
        let gathering = self.state == GroupState::PreparingRebalance;
        self.members.retain(|member| {
            member.session_deadline > now || (gathering && member.rejoined.is_some())
        });
        if self.members.len() == before {
            return;
        }
        if self.members.is_empty() {
            self.state = GroupState::Empty;
            self.leader = String::new();
            self.rebalance_deadline = None;
            return;
        }
        // The assignment in force was computed for a membership the group no longer has,
        // so the survivors are asked to rejoin — which is what the departure of a member
        // does to a group on Apache Kafka's own coordinator.
        if self.state != GroupState::PreparingRebalance {
            self.begin_rebalance(now, MAX_REBALANCE_BARRIER);
        }
    }

    /// Start a rebalance: ask every member to rejoin, and wait for them.
    ///
    /// The generation does NOT move here. It advances when the join phase COMPLETES
    /// ([`Self::form_generation`]), so a member that has not rejoined yet still holds the
    /// generation in force and its heartbeat is answered `REBALANCE_IN_PROGRESS` — which
    /// is what tells it to rejoin — rather than told its generation is illegal.
    fn begin_rebalance(&mut self, now: SystemTime, barrier: Duration) {
        self.state = GroupState::PreparingRebalance;
        self.rejoins = 0;
        for member in &mut self.members {
            member.rejoined = None;
        }
        self.rebalance_deadline = Some(now + barrier);
    }

    /// Complete the join phase: advance the generation once, take the assignment the
    /// previous one ran with off the members, and make the member that rejoined FIRST the
    /// leader of the new generation — the member the whole membership is handed to.
    ///
    /// Completing the phase is what ANSWERS every member's held `JoinGroup`, so each
    /// member's session window starts again here: a client's next call is the `SyncGroup`
    /// that follows the answer it has just been given, not a heartbeat it owed while the
    /// coordinator was holding the call.
    ///
    /// The phase the group enters here is BOUNDED like the one it leaves: the assignment
    /// is the leader's to state, and a leader that never states one would hold every
    /// other member's `SyncGroup` for as long as it stayed silent, so the instant that
    /// wait ends is stored the same way ([`Self::poll_sync`]).
    fn form_generation(&mut self, now: SystemTime) {
        self.generation = self.generation.saturating_add(1);
        self.state = GroupState::CompletingRebalance;
        self.rebalance_deadline = Some(now + MAX_REBALANCE_BARRIER);
        for member in &mut self.members {
            member.assignment = None;
            member.session_deadline = now + member.session_window;
        }
        self.leader = self
            .members
            .iter()
            .filter(|member| member.rejoined.is_some())
            .min_by_key(|member| member.rejoined)
            .map(|member| member.id.clone())
            .unwrap_or_default();
    }

    /// `JoinGroup`: admit the member — minting its id when it joined without one — into
    /// the rebalance its join starts or is part of.
    ///
    /// The answer is not ready here: the generation a member is admitted at is answered
    /// once every member of the group has rejoined it ([`Self::poll_join`]), so every
    /// member of one generation is told the same membership. `Err` carries the protocol
    /// error code the join is refused with.
    pub fn begin_join(
        &mut self,
        request: &JoinGroupRequest,
        client_id: &str,
        now: SystemTime,
    ) -> Result<Joining, i16> {
        self.expire_members(now);
        let member_id = match request.member_id.is_empty() {
            false => request.member_id.to_string(),
            true => {
                self.minted += 1;
                format!("{}-{}", request.group_id.0.as_str(), self.minted)
            }
        };
        let known = self.members.iter().any(|member| member.id == member_id);
        // A member id this group does not hold is one the coordinator has forgotten (its
        // session ran out, or it left): the client is told so and rejoins without one,
        // which is what `UNKNOWN_MEMBER_ID` says.
        if !request.member_id.is_empty() && !known {
            return Err(ResponseError::UnknownMemberId.code());
        }
        let protocol_type = request.protocol_type.to_string();
        // Every member of a group runs the same protocol; a member that asks for another
        // one cannot be assigned beside them.
        if !known
            && self
                .protocol_type
                .as_ref()
                .is_some_and(|held| !self.members.is_empty() && *held != protocol_type)
        {
            return Err(ResponseError::InconsistentGroupProtocol.code());
        }
        if !known && self.members.len() >= MAX_MEMBERS_PER_GROUP {
            return Err(ResponseError::GroupMaxSizeReached.code());
        }
        let metadata = request
            .protocols
            .first()
            .map(|protocol| protocol.metadata.clone())
            .unwrap_or_default();
        let session_window = session_window(request.session_timeout_ms);
        let session_deadline = now + session_window;
        // A join from a member the group holds while nothing is rebalancing is a member
        // asking to be reassigned, which is a rebalance too.
        if self.state != GroupState::PreparingRebalance {
            self.begin_rebalance(now, rebalance_barrier(request.rebalance_timeout_ms));
        }
        // Where this member comes in the rebalance: the first to rejoin leads the
        // generation it forms.
        self.rejoins = self.rejoins.saturating_add(1);
        let rejoined = self.rejoins;
        match self
            .members
            .iter_mut()
            .find(|member| member.id == member_id)
        {
            Some(member) => {
                member.client_id = client_id.to_string();
                member.metadata = metadata;
                member.rejoined = Some(rejoined);
                member.session_deadline = session_deadline;
                member.session_window = session_window;
            }
            None => self.members.push(GroupMember {
                id: member_id.clone(),
                client_id: client_id.to_string(),
                metadata,
                rejoined: Some(rejoined),
                assignment: None,
                session_deadline,
                session_window,
            }),
        }
        self.protocol_type = Some(protocol_type);
        self.protocol_name = request
            .protocols
            .first()
            .map(|protocol| protocol.name.to_string());
        Ok(Joining { member_id })
    }

    /// The `JoinGroup` answer for a member that is waiting on a rebalance, or `None`
    /// while the rebalance is still gathering the group's members.
    ///
    /// The wait ends when every member has rejoined, or when the rebalance barrier runs
    /// out — at which point the members that did not rejoin are treated as having left, so
    /// one stalled consumer cannot keep a group from forming. Forming the generation is
    /// what advances it, so every member of this rebalance is answered the same one.
    pub fn poll_join(&mut self, joining: &Joining, now: SystemTime) -> Option<JoinGroupResponse> {
        self.expire_members(now);
        if !self
            .members
            .iter()
            .any(|member| member.id == joining.member_id)
        {
            return Some(
                JoinGroupResponse::default().with_error_code(ResponseError::UnknownMemberId.code()),
            );
        }
        // The generation this member's rebalance formed is already in force, so its
        // answer is ready: another member's poll completed the join phase.
        if self.state != GroupState::PreparingRebalance {
            return Some(self.join_response(&joining.member_id));
        }
        let gathered = self
            .members
            .iter()
            .all(|member| member.rejoined.is_some());
        let barrier_run_out = self
            .rebalance_deadline
            .is_some_and(|deadline| now >= deadline);
        if !gathered && !barrier_run_out {
            return None;
        }
        if !gathered {
            self.members.retain(|member| member.rejoined.is_some());
        }
        self.form_generation(now);
        Some(self.join_response(&joining.member_id))
    }

    /// The membership one member is told at this generation: the whole of it for the
    /// leader, which computes the assignment, and none of it for every other member.
    fn join_response(&self, member_id: &str) -> JoinGroupResponse {
        let members = match member_id == self.leader {
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
            .with_protocol_type(
                self.protocol_type
                    .clone()
                    .map(StrBytes::from_string),
            )
            .with_protocol_name(
                self.protocol_name
                    .clone()
                    .map(StrBytes::from_string),
            )
            .with_leader(StrBytes::from_string(self.leader.clone()))
            .with_member_id(StrBytes::from_string(member_id.to_string()))
            .with_members(members)
    }

    /// `SyncGroup`: from the LEADER, store the assignment it computed for each member and
    /// put the group into force at this generation; from any other member, nothing — its
    /// answer is the assignment the leader stored for it ([`Self::poll_sync`]).
    ///
    /// `Err` carries the protocol error code the call is refused with.
    pub fn begin_sync(&mut self, request: &SyncGroupRequest, now: SystemTime) -> Result<(), i16> {
        self.expire_members(now);
        let member_id = request.member_id.to_string();
        let refusal = self.member_call_refusal(&member_id, request.generation_id, now);
        if let Some(code) = refusal {
            return Err(code);
        }
        if member_id != self.leader {
            return Ok(());
        }
        let stated: BTreeMap<String, Bytes> = request
            .assignments
            .iter()
            .map(|assignment| {
                (
                    assignment.member_id.to_string(),
                    assignment.assignment.clone(),
                )
            })
            .collect();
        for member in &mut self.members {
            member.assignment = stated.get(&member.id).cloned();
        }
        self.state = GroupState::Stable;
        // The phase is over, so the instant its wait would have ended goes with it.
        self.rebalance_deadline = None;
        Ok(())
    }

    /// The `SyncGroup` answer for a member, or `None` while the group's leader has not
    /// stated the assignment yet.
    ///
    /// That wait is BOUNDED by the barrier the generation was formed with
    /// ([`Self::form_generation`]): past it the member is told to rejoin rather than held
    /// for as long as a leader that states no assignment stays silent, so a client cannot
    /// park a call on this coordinator without end.
    pub fn poll_sync(&mut self, request: &SyncGroupRequest, now: SystemTime) -> Option<SyncGroupResponse> {
        self.expire_members(now);
        let member_id = request.member_id.to_string();
        if let Some(code) = self.member_call_refusal(&member_id, request.generation_id, now) {
            return Some(SyncGroupResponse::default().with_error_code(code));
        }
        if self.state != GroupState::Stable {
            if self
                .rebalance_deadline
                .is_some_and(|deadline| now >= deadline)
            {
                return Some(
                    SyncGroupResponse::default()
                        .with_error_code(ResponseError::RebalanceInProgress.code()),
                );
            }
            return None;
        }
        let assignment = self
            .members
            .iter()
            .find(|member| member.id == member_id)
            .and_then(|member| member.assignment.clone())
            .unwrap_or_default();
        Some(
            SyncGroupResponse::default()
                .with_protocol_type(
                    self.protocol_type
                        .clone()
                        .map(StrBytes::from_string),
                )
                .with_protocol_name(
                    self.protocol_name
                        .clone()
                        .map(StrBytes::from_string),
                )
                .with_assignment(assignment),
        )
    }

    /// `Heartbeat`: a member that holds the group's current generation keeps its
    /// assignment, one the group is rebalancing is told to rejoin
    /// (`REBALANCE_IN_PROGRESS`), one quoting an older generation is told its assignment
    /// is stale (`ILLEGAL_GENERATION`), and one the group does not hold is told so
    /// (`UNKNOWN_MEMBER_ID`).
    pub fn heartbeat(&mut self, request: &HeartbeatRequest, now: SystemTime) -> HeartbeatResponse {
        self.expire_members(now);
        let member_id = request.member_id.to_string();
        let error_code = self
            .member_call_refusal(&member_id, request.generation_id, now)
            .unwrap_or(0);
        HeartbeatResponse::default().with_error_code(error_code)
    }

    /// What a call quoting `member_id` at `generation` is refused with — and `None` when
    /// it is a member of the generation in force, whose session this call renews.
    ///
    /// The one reading, so `SyncGroup`, `Heartbeat` and `OffsetCommit` cannot disagree
    /// about which members and generations the group is answering for.
    fn member_call_refusal(
        &mut self,
        member_id: &str,
        generation: i32,
        now: SystemTime,
    ) -> Option<i16> {
        let Some(member) = self.members.iter_mut().find(|member| member.id == member_id) else {
            return Some(ResponseError::UnknownMemberId.code());
        };
        member.session_deadline = now + member.session_window;
        if generation != self.generation {
            return Some(ResponseError::IllegalGeneration.code());
        }
        match self.state {
            GroupState::PreparingRebalance => {
                Some(ResponseError::RebalanceInProgress.code())
            }
            _ => None,
        }
    }

    /// `LeaveGroup`: take the member out. A group that still holds members rebalances, so
    /// the partitions the departing member held are assigned to the ones that remain.
    pub fn leave(&mut self, request: &LeaveGroupRequest, now: SystemTime) -> LeaveGroupResponse {
        self.expire_members(now);
        let before = self.members.len();
        let leaving = request.member_id.to_string();
        self.members.retain(|member| member.id != leaving);
        for member in request.members.iter() {
            let leaving = member.member_id.to_string();
            self.members.retain(|member| member.id != leaving);
        }
        if self.members.len() == before {
            return LeaveGroupResponse::default();
        }
        match self.members.is_empty() {
            true => {
                self.state = GroupState::Empty;
                self.leader = String::new();
                self.rebalance_deadline = None;
            }
            false => self.begin_rebalance(now, MAX_REBALANCE_BARRIER),
        }
        LeaveGroupResponse::default()
    }

    /// `OffsetCommit`: store how far the group has read each partition, for at most
    /// `max_entries` (topic, partition) entries of one request — the list comes from
    /// the client.
    ///
    /// A commit is FENCED by the group's membership and generation, which is the only
    /// thing that stops a member the coordinator has already removed — one whose session
    /// ran out, or that was evicted by a rebalance — from writing over the position the
    /// member now holding its partitions has committed: a commit quoting a member the
    /// group does not hold is `UNKNOWN_MEMBER_ID`, one quoting another generation is
    /// `ILLEGAL_GENERATION`, and one that arrives while the group is handing out a new
    /// assignment is `REBALANCE_IN_PROGRESS`. A commit that quotes NO generation and no
    /// member is a client storing offsets in the group without joining it, which is
    /// admitted only while no member holds the group.
    pub fn commit(
        &mut self,
        request: &OffsetCommitRequest,
        max_entries: usize,
        now: SystemTime,
    ) -> OffsetCommitResponse {
        self.expire_members(now);
        let refusal = self.commit_refusal(
            &request.member_id.to_string(),
            request.generation_id_or_member_epoch,
        );
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
                let error_code = match refusal {
                    // Nothing is stored for a refused commit: the position the group
                    // holds is the one the member that owns the partition committed.
                    Some(code) => code,
                    None => match self.offsets.contains_key(&key)
                        || self.offsets.len() < MAX_COMMITTED_PARTITIONS_PER_GROUP
                    {
                        true => {
                            self.offsets.insert(key, partition.committed_offset);
                            0
                        }
                        false => ::kafka_protocol::ResponseError::OffsetMetadataTooLarge.code(),
                    },
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

    /// What a commit quoting `member_id` at `generation` is refused with, and `None` when
    /// the group admits it ([`Self::commit`]).
    ///
    /// Read in the coordinator's order: WHO the commit speaks for first, then the
    /// generation it quotes, and only then the group's own state — so a commit from a
    /// member the group does not hold is told that, rather than told to rejoin a
    /// rebalance it is not part of. A commit that quotes neither is a member id the
    /// group does not hold either, so a group that HAS members answers it the same way.
    fn commit_refusal(&self, member_id: &str, generation: i32) -> Option<i16> {
        // A client that joined no group: admitted while no member holds the group, since
        // a group with members is handing its partitions out itself.
        if generation < 0 && member_id.is_empty() && !self.has_members() {
            return None;
        }
        if !self.members.iter().any(|member| member.id == member_id) {
            return Some(ResponseError::UnknownMemberId.code());
        }
        if generation != self.generation {
            return Some(ResponseError::IllegalGeneration.code());
        }
        if self.state == GroupState::CompletingRebalance {
            return Some(ResponseError::RebalanceInProgress.code());
        }
        None
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

/// The window a member's session lives for, from the timeout it named: read within
/// Apache Kafka's documented bounds, and the lower bound for a member that named none.
fn session_window(session_timeout_ms: i32) -> Duration {
    let named = i64::from(session_timeout_ms);
    let bounded = named.clamp(MIN_SESSION_TIMEOUT_MS, MAX_SESSION_TIMEOUT_MS);
    Duration::from_millis(u64::try_from(bounded).unwrap_or(0))
}

/// How long the coordinator waits for the group's other members to rejoin a rebalance
/// this member started: the window it stated, bounded by [`MAX_REBALANCE_BARRIER`].
fn rebalance_barrier(rebalance_timeout_ms: i32) -> Duration {
    match i64::from(rebalance_timeout_ms) {
        stated if stated > 0 => Duration::from_millis(u64::try_from(stated).unwrap_or(0))
            .min(MAX_REBALANCE_BARRIER),
        // A version of the join that states no rebalance timeout at all.
        _ => MAX_REBALANCE_BARRIER,
    }
}
