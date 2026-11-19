// SPDX-License-Identifier: CC0-1.0

//! Engine state for the com-amazonaws-kinesisvideowebrtcstorage-awsacuityroutingservicelambda service.
//!
//! The service binds two operations — `JoinStorageSession` (the video producing
//! device, i.e. the master) and `JoinStorageSessionAsViewer` (a playback viewer) —
//! and both outputs are empty structures, so everything a caller can observe about
//! a join is carried here: the WebRTC storage session each signaling channel has.
//!
//! WHICH signaling channels exist is NOT held here. A channel is created by
//! `kinesisvideo:CreateSignalingChannel` and deleted by its `DeleteSignalingChannel`,
//! so its existence is a fact of the control plane that mints it; both joins resolve a
//! `channelArn` against the ONE registry that create populates ([`knows_channel`])
//! rather than against a set of this crate's own, which no create ever writes to.

use std::collections::HashMap;
use std::time::SystemTime;

/// The SDP offer one join of a storage session is sent.
pub mod sdp_offer;

pub use sdp_offer::{offer_payload, offer_sdp, OFFER_DESCRIPTION_TYPE};

/// The resource type a Kinesis Video *signaling channel* ARN carries.
///
/// The modelled `@pattern` on `ChannelArn`
/// (`^arn:(aws[a-zA-Z-]*):kinesisvideo:[a-z0-9-]+:[0-9]+:[a-z]+/[a-zA-Z0-9_.-]+/[0-9]+$`)
/// admits ANY lowercase resource type, so `…:stream/name/1700000000000` satisfies
/// the schema while naming a Kinesis video stream rather than a signaling channel.
/// That is the semantic check the schema cannot express, and the handlers refuse it
/// with the modelled `InvalidArgumentException`.
pub const SIGNALING_CHANNEL_RESOURCE_TYPE: &str = "channel";

/// The longest `channelArn` this service accepts.
///
/// Source: `com.amazonaws.kinesisvideo#ResourceARN` — the shape the Kinesis Video
/// control-plane model gives this very ARN — is `@length(min: 1, max: 1024)`
/// (`smithy_query(query="shape",shape="ResourceARN",model_path="services/service-com-amazonaws-kinesisvideo-kinesisvideo-20170930/smithy-model/com-amazonaws-kinesisvideo-kinesisvideo-20170930.json")`).
/// This service's own `ChannelArn` declares no length, so the bound is applied
/// here: a longer value is not an ARN this service can be naming a channel with,
/// and the store never keys an entry on an unbounded string.
pub const MAX_CHANNEL_ARN_LENGTH: usize = 1024;

/// The longest signaling channel NAME a `channelArn` may carry.
///
/// Source: `com.amazonaws.kinesisvideo#ChannelName` is `@length(min: 1, max: 256)`
/// — the name `kinesisvideo:CreateSignalingChannel` accepts, so no channel can
/// exist with a longer one.
pub const MAX_CHANNEL_NAME_LENGTH: usize = 256;

/// How many signaling channels this service tracks at once.
///
/// Set to the Kinesis Video Streams account quota for signaling channels (10,000
/// per account per Region). The point of the constant is the bound itself: the
/// keys of [`ComAmazonawsKinesisvideowebrtcstorageAwsacuityroutingservicelambdaState::sessions`]
/// come from the request, so without a cap a caller could grow the store one
/// entry per distinct ARN for as long as the process runs. A join that would
/// exceed it is refused with the modelled `ClientLimitExceededException`.
pub const MAX_SIGNALING_CHANNELS: usize = 10_000;

/// How many offers one session's log keeps.
///
/// Not an AWS quota — a chosen bound. Every `JoinStorageSession` call pushes an offer of
/// its own ("an offer is sent once per each call"), so without a cap a caller looping that
/// one route would grow this process without limit. At the cap the OLDEST offer is dropped:
/// the log is what the session has recently offered, not an archive.
pub const MAX_OFFERS_PER_SESSION: usize = 128;

/// How many viewers one channel's session admits at once.
///
/// A `SINGLE_MASTER` signaling channel — the only type that ingests WebRTC media,
/// per this operation's own modelled docs — carries one video producing device and
/// the viewers of its STORAGE session. The model states no number ("Multiple viewers
/// are able to playback real-time media"); recorded behaviour does: the third
/// distinct `clientId` is admitted and the fourth is refused, where plain signaling
/// admits ten viewers per channel. A viewer join past it is refused with the modelled
/// `ClientLimitExceededException`, which also bounds the per-session viewer map,
/// whose keys are the caller's `clientId`.
pub const MAX_VIEWERS_PER_SESSION: usize = 3;

/// What the service answers a viewer join the session has no slot left for: fixed
/// text, with nothing of the request or of the store in it.
pub const VIEWER_LIMIT_MESSAGE: &str = "Maximum number of viewers connected to the session!";

/// Why a join could not be admitted: one of the limits above is already reached.
///
/// Both are the modelled `ClientLimitExceededException` (HTTP 400) on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinRefused {
    /// [`MAX_SIGNALING_CHANNELS`] channels are already tracked and the join names
    /// a new one.
    ChannelLimit,
    /// The channel's session already holds [`MAX_VIEWERS_PER_SESSION`] viewers and
    /// the join names a new `clientId`.
    ViewerLimit,
}

impl JoinRefused {
    /// The refusal, worded for the caller: which limit, and what its value is.
    pub fn message(self) -> String {
        match self {
            Self::ChannelLimit => format!(
                "the limit of {MAX_SIGNALING_CHANNELS} signaling channels with a storage session has been reached"
            ),
            Self::ViewerLimit => VIEWER_LIMIT_MESSAGE.to_string(),
        }
    }
}

/// A `kinesisvideo` ARN split into its segments, borrowed from the ARN string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelArnParts<'a> {
    /// The partition (`aws`, `aws-cn`, `aws-us-gov`, …).
    pub partition: &'a str,
    /// The region segment.
    pub region: &'a str,
    /// The account id segment.
    pub account_id: &'a str,
    /// The resource type — `channel` for a signaling channel.
    pub resource_type: &'a str,
    /// The resource name.
    pub name: &'a str,
    /// The channel's creation-time suffix (the trailing `/<digits>`).
    pub creation_time: &'a str,
}

/// Split a `kinesisvideo` ARN of the form
/// `arn:<partition>:kinesisvideo:<region>:<account>:<type>/<name>/<creation-time>`.
///
/// `None` when the string is not such an ARN. Modelled constraints are refused
/// before a handler runs, so a handler normally gets a string this parses; the
/// handlers still answer `None` with a modelled `InvalidArgumentException` rather
/// than assuming the shape.
pub fn parse_channel_arn(arn: &str) -> Option<ChannelArnParts<'_>> {
    if arn.len() > MAX_CHANNEL_ARN_LENGTH {
        return None;
    }
    let mut fields = arn.splitn(6, ':');
    if fields.next()? != "arn" {
        return None;
    }
    let partition = fields.next()?;
    if fields.next()? != "kinesisvideo" {
        return None;
    }
    let region = fields.next()?;
    let account_id = fields.next()?;
    let resource: Vec<&str> = fields.next()?.split('/').collect();
    let [resource_type, name, creation_time] = resource[..] else {
        return None;
    };
    // The accept-set is the modelled `@pattern` on `ChannelArn`, segment by
    // segment: `arn:(aws[a-zA-Z-]*):kinesisvideo:[a-z0-9-]+:[0-9]+:[a-z]+/[a-zA-Z0-9_.-]+/[0-9]+`,
    // with the channel name additionally bounded by `ChannelName`'s length.
    let well_formed = partition.starts_with("aws")
        && !region.is_empty()
        && region
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !account_id.is_empty()
        && account_id.bytes().all(|b| b.is_ascii_digit())
        && !resource_type.is_empty()
        && resource_type.bytes().all(|b| b.is_ascii_lowercase())
        && (1..=MAX_CHANNEL_NAME_LENGTH).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b'-')
        && !creation_time.is_empty()
        && creation_time.bytes().all(|b| b.is_ascii_digit());
    well_formed.then_some(ChannelArnParts {
        partition,
        region,
        account_id,
        resource_type,
        name,
        creation_time,
    })
}

/// The one video producing device (master) currently associated with a session.
///
/// Modelled docs: "Assume that only one video producing device client can be
/// associated with a session for the channel. If more than one client joins the
/// session of a specific channel as a video producing device, the most recent
/// client request takes precedence."
#[derive(Debug, Clone)]
pub struct ProducerSession {
    /// When this producer took the session over.
    pub joined_at: SystemTime,
    /// The session's [`StorageSession::join_count`] at which it took over, so a
    /// later join demonstrably supersedes an earlier one.
    pub generation: u64,
}

/// One SDP offer a session pushed to the channel's video producing device.
///
/// Modelled docs: "Concurrent calls are allowed. An offer is sent once per each call" — so
/// one of these records exactly one `JoinStorageSession` call, and the payload it carries is
/// the one that was handed to the channel's signaling plane.
#[derive(Debug, Clone)]
pub struct SessionOffer {
    /// The producer join this offer belongs to: the session's [`StorageSession::join_count`]
    /// at that call.
    pub generation: u64,
    /// The offer as it was pushed: base64 of the JSEP description envelope
    /// ([`sdp_offer::offer_payload`]).
    pub message_payload: String,
    /// When the session offered it.
    pub offered_at: SystemTime,
    /// Whether the channel's signaling plane had a master peer to hand it to. An offer
    /// nobody was attached to receive is still the offer this call sent.
    pub delivered: bool,
}

/// A playback viewer registered on a session under its `clientId`.
#[derive(Debug, Clone)]
pub struct ViewerSession {
    /// The `clientId` this viewer joined with. A denormalised copy of this
    /// viewer's key in [`StorageSession::viewers`], so a viewer read out of the
    /// map on its own still carries its id; only [`StorageSession::join_as_viewer`]
    /// writes either, and it writes both from the same value.
    pub client_id: String,
    /// When this viewer last joined. Modelled docs: "If there's an existing
    /// session with the same clientId that's found in the join session request,
    /// the new request takes precedence."
    pub joined_at: SystemTime,
    /// How many times this `clientId` has joined. The API is not idempotent, so a
    /// repeat join is a new join of the same viewer rather than a no-op.
    pub join_count: u64,
}

/// The WebRTC storage/streaming session of one signaling channel.
#[derive(Debug, Clone)]
pub struct StorageSession {
    /// The signaling channel ARN, exactly as the request supplied it.
    pub channel_arn: String,
    /// The incumbent video producing device, if one has joined.
    pub producer: Option<ProducerSession>,
    /// Playback viewers keyed by `clientId`.
    pub viewers: HashMap<String, ViewerSession>,
    /// When the session was created — by the first join of either kind.
    pub created_at: SystemTime,
    /// Producer joins seen by this session. Modelled docs: "This API is not
    /// idempotent … An offer is sent once per each call."
    pub join_count: u64,
    /// The offers this session has pushed to the channel's video producing device, oldest
    /// first and bounded at [`MAX_OFFERS_PER_SESSION`].
    pub offers: Vec<SessionOffer>,
}

impl StorageSession {
    /// A session with no participants yet.
    fn new(channel_arn: &str, created_at: SystemTime) -> Self {
        Self {
            channel_arn: channel_arn.to_string(),
            producer: None,
            viewers: HashMap::new(),
            created_at,
            join_count: 0,
            offers: Vec::new(),
        }
    }

    /// Record the offer this session pushed for one join, dropping the oldest records past
    /// [`MAX_OFFERS_PER_SESSION`] so a looping caller cannot grow the log without end.
    pub fn record_offer(&mut self, offer: SessionOffer) {
        let over = (self.offers.len() + 1).saturating_sub(MAX_OFFERS_PER_SESSION);
        self.offers.drain(..over.min(self.offers.len()));
        self.offers.push(offer);
    }

    /// The offer this session pushed for the join `generation`, if its record is still in
    /// the log.
    #[must_use]
    pub fn offer_for(&self, generation: u64) -> Option<&SessionOffer> {
        self.offers
            .iter()
            .find(|offer| offer.generation == generation)
    }

    /// Record a video-producing-device join: count the call and make this the
    /// incumbent producer, replacing any earlier one. Returns the new generation.
    pub fn join_as_producer(&mut self, at: SystemTime) -> u64 {
        self.join_count += 1;
        self.producer = Some(ProducerSession {
            joined_at: at,
            generation: self.join_count,
        });
        self.join_count
    }

    /// Record a viewer join under `client_id`: a first join registers the viewer,
    /// a repeat join refreshes it instead of duplicating the entry. Returns how
    /// many times that `clientId` has joined, or [`JoinRefused::ViewerLimit`] when
    /// the session already holds [`MAX_VIEWERS_PER_SESSION`] other viewers.
    pub fn join_as_viewer(
        &mut self,
        client_id: &str,
        at: SystemTime,
    ) -> Result<u64, JoinRefused> {
        if !self.viewers.contains_key(client_id) && self.viewers.len() >= MAX_VIEWERS_PER_SESSION {
            return Err(JoinRefused::ViewerLimit);
        }
        let viewer = self
            .viewers
            .entry(client_id.to_string())
            .or_insert_with(|| ViewerSession {
                client_id: client_id.to_string(),
                joined_at: at,
                join_count: 0,
            });
        viewer.joined_at = at;
        viewer.join_count += 1;
        Ok(viewer.join_count)
    }
}

/// Per-service state for com-amazonaws-kinesisvideowebrtcstorage-awsacuityroutingservicelambda. A handler reaches it through [`service_state`].
#[derive(Debug, Default)]
pub struct ComAmazonawsKinesisvideowebrtcstorageAwsacuityroutingservicelambdaState {
    /// Storage sessions keyed by signaling channel ARN.
    ///
    /// A session is created by the FIRST join of either kind on a channel the
    /// control plane holds ("Both a video producing device and viewers can join a
    /// session first and wait for other participants"), so this map holds a session
    /// only for channels that exist; the existence question itself is answered by
    /// [`knows_channel`], not from here.
    ///
    /// Bounded at [`MAX_SIGNALING_CHANNELS`]: the keys come from the request, so
    /// every path that may insert one passes [`Self::reserve_channel`] first.
    pub sessions: HashMap<String, StorageSession>,
}

impl ComAmazonawsKinesisvideowebrtcstorageAwsacuityroutingservicelambdaState {
    /// The channel's session, if it has one.
    pub fn session(&self, channel_arn: &str) -> Option<&StorageSession> {
        self.sessions.get(channel_arn)
    }

    /// How many signaling channels have a session — the value
    /// [`MAX_SIGNALING_CHANNELS`] bounds.
    pub fn channel_count(&self) -> usize {
        self.sessions.len()
    }

    /// Record a video-producing-device join on the channel's session, creating the
    /// session when the channel has none yet — "If there's no existing session for
    /// the channel, a new streaming session needs to be created". Returns the
    /// producer's new generation.
    ///
    /// Refused with [`JoinRefused::ChannelLimit`] when the channel has no session
    /// yet and the store already holds [`MAX_SIGNALING_CHANNELS`] of them.
    ///
    /// The caller decides whether the channel EXISTS ([`knows_channel`]); that is a
    /// different answer (`ResourceNotFoundException`) from a limit.
    pub fn join_as_producer(
        &mut self,
        channel_arn: &str,
        now: SystemTime,
    ) -> Result<u64, JoinRefused> {
        self.reserve_channel(channel_arn)?;
        Ok(self.session_or_create(channel_arn, now).join_as_producer(now))
    }

    /// Record on the channel's session the offer this emulator pushed for one producer
    /// join; `false` when the channel has no session (it was deleted in between), in which
    /// case there is nothing the record belongs to.
    pub fn record_offer(&mut self, channel_arn: &str, offer: SessionOffer) -> bool {
        match self.sessions.get_mut(channel_arn) {
            Some(session) => {
                session.record_offer(offer);
                true
            }
            None => false,
        }
    }

    /// Record a viewer join under `client_id` on the channel's session, creating
    /// the session when the channel has none yet. Returns how many times that
    /// `clientId` has joined, or the limit that refused it.
    ///
    /// The caller decides whether the channel EXISTS ([`knows_channel`]); that is a
    /// different answer (`ResourceNotFoundException`) from a limit.
    pub fn join_as_viewer(
        &mut self,
        channel_arn: &str,
        client_id: &str,
        now: SystemTime,
    ) -> Result<u64, JoinRefused> {
        self.reserve_channel(channel_arn)?;
        self.session_or_create(channel_arn, now)
            .join_as_viewer(client_id, now)
    }

    /// `Ok` when `channel_arn` already has a session, or when another one fits
    /// under [`MAX_SIGNALING_CHANNELS`].
    fn reserve_channel(&self, channel_arn: &str) -> Result<(), JoinRefused> {
        if !self.sessions.contains_key(channel_arn)
            && self.sessions.len() >= MAX_SIGNALING_CHANNELS
        {
            return Err(JoinRefused::ChannelLimit);
        }
        Ok(())
    }

    /// The channel's session, creating it when the channel has none yet. Private:
    /// every caller must pass [`reserve_channel`](Self::reserve_channel) first, so
    /// no path inserts a session without the bound.
    fn session_or_create(&mut self, channel_arn: &str, now: SystemTime) -> &mut StorageSession {
        self.sessions
            .entry(channel_arn.to_string())
            .or_insert_with(|| StorageSession::new(channel_arn, now))
    }
}

/// Whether IAM lets a request perform `kinesisvideo:<operation>` on the channel ARN it
/// addresses, and the sentence that says why not when it does not.
///
/// Both operations model `AccessDeniedException` — "You do not have required permissions
/// to perform this operation" — so a join is authorized BEFORE it reads or writes
/// anything. The decision needs the SESSION a credential speaks for (STS's store) and the
/// identity-based POLICIES in force on that principal (IAM's), and the one reading of
/// those two stores for this service family's `kinesisvideo` actions already serves the
/// signaling data plane: it is shared from there rather than copied, so one action prefix,
/// one principal resolution and one refusal sentence serve every plane of this family.
///
/// Called BEFORE this service's own store is locked, so no two stores are held at once.
pub use ::service_com_amazonaws_kinesisvideosignaling_awsacuitysignalingservice::state::authorization_refusal;

/// Whether `channel_arn` names a signaling channel THIS emulator holds.
///
/// A signaling channel is a resource of the Kinesis Video Streams control plane:
/// `CreateSignalingChannel` mints it and `DeleteSignalingChannel` removes it. Both are
/// in another crate, and each keeps the ONE registry every plane of this service family
/// resolves a channel ARN against, so existence is read from there — a registry of this
/// crate's own would answer `ResourceNotFoundException` for a channel a caller just
/// created and read back as `ACTIVE`.
///
/// Read BEFORE this service's own store is locked, so the two stores are never held at
/// once.
pub fn knows_channel<Eng: Send + Sync + 'static>(engine: &Eng, channel_arn: &str) -> bool {
    ::service_com_amazonaws_kinesisvideosignaling_awsacuitysignalingservice::state::knows_channel_of(
        engine,
        channel_arn,
    )
}

/// This service's store in the shared engine: call it with a handler's `&*self.engine`.
///
/// Answer `None` with a modelled server error, never with a fresh engine.
pub fn service_state<Eng: Send + Sync + 'static>(
    engine: &Eng,
) -> Option<::engine::ServiceState<ComAmazonawsKinesisvideowebrtcstorageAwsacuityroutingservicelambdaState>> {
    (engine as &dyn ::std::any::Any)
        .downcast_ref::<::engine::InMemoryEngine>()
        .map(::engine::InMemoryEngine::state::<ComAmazonawsKinesisvideowebrtcstorageAwsacuityroutingservicelambdaState>)
}
