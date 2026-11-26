// SPDX-License-Identifier: CC0-1.0

//! The Traffic Mirror session store: the ids, bounds, byte charge and error channel the
//! Traffic Mirror session operations share.
//!
//! The sessions themselves live on the service's one state struct
//! ([`ComAmazonawsEc2Amazonec2State::traffic_mirror_sessions`]) as the MODELLED
//! [`TrafficMirrorSession`] shape, the way [`super::traffic_mirror_filters`] holds a
//! filter, so a create, a modify and a describe cannot report one session differently;
//! only the primitives over that store live here.

use super::{tag_list_bytes, ComAmazonawsEc2Amazonec2State};
use crate::ops::types::TrafficMirrorSession;
use crate::scaffolded::Scaffolded;

/// The id prefix a Traffic Mirror session is minted under.
///
/// EC2 names the resource's ids `tms-<hex>`, the same `<prefix>-<hex>` form every other
/// EC2 resource id takes, so [`ComAmazonawsEc2Amazonec2State::next_id`] mints them.
pub const TRAFFIC_MIRROR_SESSION_ID_PREFIX: &str = "tms";

/// The `ResourceType` a Traffic Mirror session is tagged under, spelled as the modelled
/// `ResourceType` enum spells it (`smithy_query(query="shape",shape="ResourceType")`).
pub const TRAFFIC_MIRROR_SESSION_TAGGABLE_RESOURCE: &str = "traffic-mirror-session";

/// How many Traffic Mirror sessions this emulator stores before a create is refused.
///
/// CHOSEN, like [`super::MAX_TRAFFIC_MIRROR_FILTERS`] and for the same reason: nothing in
/// this emulator reclaims the id of a session a delete removed, so the store needs a
/// ceiling that is not the process's memory. A count ceiling is not a memory bound on its
/// own — see [`super::MAX_STORED_BYTES`] — so the caller-chosen bytes of each record are charged
/// against that budget too ([`traffic_mirror_session_payload_bytes`]).
pub const MAX_TRAFFIC_MIRROR_SESSIONS: usize = 1_000;

/// The largest page `DescribeTrafficMirrorSessions` serves, and the page it serves when the
/// request names no size.
///
/// The top of the modelled `MaxResults` range, `TrafficMirroringMaxResults`'s
/// `range 5..1000`
/// (`smithy_query(query="operation",operation="DescribeTrafficMirrorSessions")`), so a
/// request that names no page size is answered with the largest page the operation can be
/// ASKED for and the `NextToken` path behaves the same either way — the same contract as
/// [`super::MAX_TRAFFIC_MIRROR_FILTERS_PAGE_SIZE`].
pub const MAX_TRAFFIC_MIRROR_SESSIONS_PAGE_SIZE: usize = 1_000;

/// EC2's code for a Traffic Mirror session id that names no session of the account.
///
/// EC2's model declares no error shape at all (see [`crate::scaffolded::ServiceError`]),
/// and the service answers `Invalid<Resource>Id.NotFound` for an id of a resource it
/// cannot resolve — `InvalidTrafficMirrorFilterId.NotFound`,
/// `InvalidLaunchTemplateId.NotFound` — so that is the code this resource's id gets.
pub const INVALID_TRAFFIC_MIRROR_SESSION_ID_NOT_FOUND: &str =
    "InvalidTrafficMirrorSessionId.NotFound";

/// EC2's quota code for one Traffic Mirror session too many, named the way every other
/// per-resource EC2 quota code is (`VolumeLimitExceeded`,
/// [`super::TRAFFIC_MIRROR_FILTER_LIMIT_EXCEEDED`]).
pub const TRAFFIC_MIRROR_SESSION_LIMIT_EXCEEDED: &str = "TrafficMirrorSessionLimitExceeded";

/// The values `SessionNumber` takes, inclusive: "Valid values are 1-32766"
/// (`CreateTrafficMirrorSessionRequest$SessionNumber`). The member targets a plain
/// `Integer`, so the model refuses nothing on its own and these published bounds are the
/// ones this service holds the value to.
pub const TRAFFIC_MIRROR_SESSION_NUMBER_RANGE: (i32, i32) = (1, 32_766);

/// The values `PacketLength` takes, inclusive: "Valid values are 1-8500. Setting a
/// `PacketLength` greater than 8500 will result in an error response"
/// (`CreateTrafficMirrorSessionRequest$PacketLength`).
pub const TRAFFIC_MIRROR_PACKET_LENGTH_RANGE: (i32, i32) = (1, 8_500);

/// The `PacketLength` a session gets when the request names none and the session mirrors to
/// a Network Load Balancer: "For sessions with Network Load Balancer (NLB) Traffic Mirror
/// targets the default `PacketLength` will be set to 8500"
/// (`CreateTrafficMirrorSessionRequest$PacketLength`).
pub const NETWORK_LOAD_BALANCER_DEFAULT_PACKET_LENGTH: i32 = 8_500;

/// The values `VirtualNetworkId` takes, inclusive.
///
/// "The VXLAN ID for the Traffic Mirror session. For more information about the VXLAN
/// protocol, see RFC 7348" (`CreateTrafficMirrorSessionRequest$VirtualNetworkId`): a VXLAN
/// network identifier is the 24-bit VNI field of that header, so the widest value one
/// carries is 2^24 - 1 and a value outside that width names no VXLAN network at all.
pub const TRAFFIC_MIRROR_VIRTUAL_NETWORK_ID_RANGE: (i32, i32) = (0, 16_777_215);

/// The caller-chosen bytes one stored Traffic Mirror session holds for as long as the
/// store holds it: its tags, its description and the client token the token map keeps for
/// it.
///
/// Everything else on the record — its own id, the ids of the filter, the target and the
/// network interface it names, the account, and the bounded integers — is derived by this
/// crate or bounded by the model, so it is not counted here. The ONE definition of that
/// charge, so a writer cannot store bytes the store's budget never saw and a removal gives
/// back exactly what the insert took.
pub fn traffic_mirror_session_payload_bytes(
    session: &TrafficMirrorSession,
    client_token: Option<&str>,
) -> usize {
    tag_list_bytes(session.tags.as_deref().unwrap_or_default())
        .saturating_add(session.description.as_ref().map_or(0, String::len))
        .saturating_add(client_token.map_or(0, str::len))
}

/// Refuse a create that would take the store past [`MAX_TRAFFIC_MIRROR_SESSIONS`] or past
/// the store's byte budget: HTTP 400 [`TRAFFIC_MIRROR_SESSION_LIMIT_EXCEEDED`].
pub fn traffic_mirror_session_limit_exceeded<E>(message: impl Into<String>) -> Scaffolded<E> {
    Scaffolded::service_error(TRAFFIC_MIRROR_SESSION_LIMIT_EXCEEDED, 400, message)
}

/// The value `value` if the published bounds `(min, max)` admit it, or the refusal EC2
/// answers for a member's value its parameter layer cannot take.
///
/// The bounds are checked and the refusal worded by the crate's one reading of a
/// hand-enforced published range ([`super::require_within_published_range`]); this only
/// widens a member published in `i32` into it, so the create and a later modify cannot hold
/// one member to two different ranges or word the same refusal two ways.
fn require_in_range<E>(
    value: i32,
    (min, max): (i32, i32),
    member: &str,
) -> Result<i32, Scaffolded<E>> {
    super::require_within_published_range(
        i64::from(value),
        (i64::from(min), i64::from(max)),
        member,
    )?;
    Ok(value)
}

/// The `SessionNumber` a request named, held to [`TRAFFIC_MIRROR_SESSION_NUMBER_RANGE`].
pub fn require_traffic_mirror_session_number<E>(session_number: i32) -> Result<i32, Scaffolded<E>> {
    require_in_range(
        session_number,
        TRAFFIC_MIRROR_SESSION_NUMBER_RANGE,
        "sessionNumber",
    )
}

/// The `PacketLength` a request named, held to [`TRAFFIC_MIRROR_PACKET_LENGTH_RANGE`].
pub fn require_traffic_mirror_packet_length<E>(packet_length: i32) -> Result<i32, Scaffolded<E>> {
    require_in_range(
        packet_length,
        TRAFFIC_MIRROR_PACKET_LENGTH_RANGE,
        "packetLength",
    )
}

/// The `VirtualNetworkId` a request named, held to
/// [`TRAFFIC_MIRROR_VIRTUAL_NETWORK_ID_RANGE`].
pub fn require_traffic_mirror_virtual_network_id<E>(
    virtual_network_id: i32,
) -> Result<i32, Scaffolded<E>> {
    require_in_range(
        virtual_network_id,
        TRAFFIC_MIRROR_VIRTUAL_NETWORK_ID_RANGE,
        "virtualNetworkId",
    )
}

/// Refuse a request that named a Traffic Mirror session id this account holds no session
/// for: HTTP 400 [`INVALID_TRAFFIC_MIRROR_SESSION_ID_NOT_FOUND`].
///
/// `TrafficMirrorSessionId` publishes no pattern
/// (`smithy_query(query="shape",shape="TrafficMirrorSessionId")`), so there is no spelling
/// to call an id malformed by, and the recorded catalog this emulator is measured against
/// shows EC2 answering this one code for an id in the long resource-id form that names
/// nothing: one code covers an id of any shape, because an id this service never minted
/// names no session of the account whether or not it reads like one. The message names the
/// id the REQUEST gave, so a client reading the text sees which id it was.
pub fn invalid_traffic_mirror_session_id_not_found<E>(
    traffic_mirror_session_id: &str,
) -> Scaffolded<E> {
    Scaffolded::service_error(
        INVALID_TRAFFIC_MIRROR_SESSION_ID_NOT_FOUND,
        400,
        format!("{traffic_mirror_session_id} does not exist"),
    )
}

/// Resolve `traffic_mirror_session_id` against the store, or refuse the request with
/// [`invalid_traffic_mirror_session_id_not_found`].
///
/// The ONE place an id of this kind is resolved, so no two operations can disagree about
/// which ids name a session of the account.
pub fn require_known_traffic_mirror_session<E>(
    store: &ComAmazonawsEc2Amazonec2State,
    traffic_mirror_session_id: &str,
) -> Result<(), Scaffolded<E>> {
    if store
        .traffic_mirror_sessions
        .contains_key(traffic_mirror_session_id)
    {
        return Ok(());
    }
    Err(invalid_traffic_mirror_session_id_not_found(
        traffic_mirror_session_id,
    ))
}

/// What the store answers a create asking whether it can hold one more Traffic Mirror
/// session: room, or the reason there is none.
///
/// A create has to word a different refusal for each reason, and the store has to keep ONE
/// definition of "has room", so the reason travels with the answer rather than being decided
/// a second time by the caller — the same seam [`super::TrafficMirrorTargetRoom`] is for a
/// target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrafficMirrorSessionRoom {
    /// The store can hold the session.
    Available,
    /// The store already holds [`MAX_TRAFFIC_MIRROR_SESSIONS`] sessions.
    AtCeiling,
    /// Holding the session's caller-chosen bytes would take the store past
    /// [`super::MAX_STORED_BYTES`]: `requested` is what the record would hold and `held` what
    /// the store holds already.
    OverByteBudget { requested: usize, held: usize },
}

impl ComAmazonawsEc2Amazonec2State {
    /// Whether the store can hold one more Traffic Mirror session, and when it cannot, which
    /// of its two bounds says so.
    ///
    /// A create asks this first so it can refuse with EC2's own quota code, worded for the
    /// bound that refused it, instead of overshooting; the count ceiling is asked before the
    /// byte budget because a record count on its own admits gigabytes.
    pub fn traffic_mirror_session_room(
        &self,
        session: &TrafficMirrorSession,
        client_token: Option<&str>,
    ) -> TrafficMirrorSessionRoom {
        if self.traffic_mirror_sessions.len() >= MAX_TRAFFIC_MIRROR_SESSIONS {
            return TrafficMirrorSessionRoom::AtCeiling;
        }
        let requested = traffic_mirror_session_payload_bytes(session, client_token);
        if self.would_exceed_stored_bytes(requested) {
            return TrafficMirrorSessionRoom::OverByteBudget {
                requested,
                held: self.stored_bytes(),
            };
        }
        TrafficMirrorSessionRoom::Available
    }

    /// Whether another Traffic Mirror session fits inside the store's ceiling
    /// ([`MAX_TRAFFIC_MIRROR_SESSIONS`]) and its byte budget.
    ///
    /// The same contract as [`Self::has_traffic_mirror_filter_capacity`], and the yes-or-no
    /// form of [`Self::traffic_mirror_session_room`], so there is one definition of "has
    /// room": a create asks first so it can refuse with EC2's own quota code instead of
    /// overshooting, and [`Self::insert_traffic_mirror_session`] asks it again for itself.
    pub fn has_traffic_mirror_session_capacity(
        &self,
        session: &TrafficMirrorSession,
        client_token: Option<&str>,
    ) -> bool {
        self.traffic_mirror_session_room(session, client_token)
            == TrafficMirrorSessionRoom::Available
    }

    /// A `VirtualNetworkId` no session of this account carries, for a create that named none:
    /// "If you do not specify a VirtualNetworkId, an account-wide unique ID is chosen"
    /// (`CreateTrafficMirrorSessionRequest$VirtualNetworkId`) — `None` only when every value
    /// a VXLAN header carries ([`TRAFFIC_MIRROR_VIRTUAL_NETWORK_ID_RANGE`]) is already taken.
    ///
    /// Uniqueness across the account's sessions is the property a caller can rely on, so it
    /// is what this mints: the lowest identifier a VXLAN header carries that no stored session
    /// uses. Nothing is read off a clock or a random source, so the same sequence of calls
    /// mints the same identifiers — the contract [`Self::next_id`] holds for an id.
    pub fn mint_traffic_mirror_virtual_network_id(&self) -> Option<i32> {
        let taken: std::collections::BTreeSet<i32> = self
            .traffic_mirror_sessions
            .values()
            .filter_map(|session| session.virtual_network_id)
            .collect();
        // Zero names the VXLAN network a header with an unset VNI would name, so the lowest
        // minted identifier is one, leaving a minted value distinguishable from an unset one.
        let (_, widest) = TRAFFIC_MIRROR_VIRTUAL_NETWORK_ID_RANGE;
        (1..=widest).find(|candidate| !taken.contains(candidate))
    }

    /// Store `session` under a freshly minted `TrafficMirrorSessionId`, charge the
    /// caller-chosen bytes it retains against the store's byte budget, and answer the id it
    /// was given — or `None`, having stored NOTHING, when the store is at a ceiling.
    ///
    /// The id is minted HERE rather than by the writer, so a session never shares an id
    /// with another resource of this store, and the record is written with that id on it so
    /// no reader has to learn the key twice. The ceilings are enforced here as well as
    /// asked by the writer ([`Self::has_traffic_mirror_session_capacity`]), the way
    /// [`Self::insert_coip_pool`] enforces its own, so a writer that forgets to ask cannot
    /// grow the store without bound.
    ///
    /// `client_token` is the `ClientToken` the create carried, if any: it is remembered as
    /// naming the session this insert minted, so a repeat of that request reports this
    /// session rather than minting a second one, and its bytes are charged with the record's.
    pub fn insert_traffic_mirror_session(
        &mut self,
        mut session: TrafficMirrorSession,
        client_token: Option<&str>,
    ) -> Option<String> {
        if !self.has_traffic_mirror_session_capacity(&session, client_token) {
            return None;
        }
        let traffic_mirror_session_id = self.next_id(TRAFFIC_MIRROR_SESSION_ID_PREFIX);
        session.traffic_mirror_session_id = Some(traffic_mirror_session_id.clone());
        self.add_stored_bytes(traffic_mirror_session_payload_bytes(&session, client_token));
        if let Some(token) = client_token {
            self.traffic_mirror_session_client_tokens
                .insert(token.to_string(), traffic_mirror_session_id.clone());
        }
        self.traffic_mirror_sessions
            .insert(traffic_mirror_session_id.clone(), session);
        Some(traffic_mirror_session_id)
    }

    /// Remove the Traffic Mirror session `traffic_mirror_session_id` names and answer it as
    /// it was last reported, or `None` when the store holds no such session.
    ///
    /// The answer is the record the removal took OUT of the store, so what a delete reports
    /// is what the readers reported — never a second copy built beside it. The token that
    /// named this session stops naming it, so a later create carrying it mints a new session
    /// rather than reporting one nothing holds, and the bytes the record held go back to the
    /// store's budget on exactly the terms [`Self::insert_traffic_mirror_session`] took them
    /// on ([`traffic_mirror_session_payload_bytes`]).
    pub fn remove_traffic_mirror_session(
        &mut self,
        traffic_mirror_session_id: &str,
    ) -> Option<TrafficMirrorSession> {
        let session = self
            .traffic_mirror_sessions
            .remove(traffic_mirror_session_id)?;
        let client_token = self
            .traffic_mirror_session_client_tokens
            .iter()
            .find(|(_, named)| named.as_str() == traffic_mirror_session_id)
            .map(|(token, _)| token.clone());
        if let Some(token) = client_token.as_deref() {
            self.traffic_mirror_session_client_tokens.remove(token);
        }
        self.release_stored_bytes(traffic_mirror_session_payload_bytes(
            &session,
            client_token.as_deref(),
        ));
        Some(session)
    }
}
