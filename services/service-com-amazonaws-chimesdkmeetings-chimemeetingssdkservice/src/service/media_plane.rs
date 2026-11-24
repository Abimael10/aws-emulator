// SPDX-License-Identifier: CC0-1.0

//! The MEDIA PLANE of a meeting: what answers when a client dials one of the addresses
//! `Meeting.MediaPlacement` published to it.
//!
//! `MediaPlacement` is "A set of endpoints used by clients to connect to the media service
//! group for an Amazon Chime SDK meeting"
//! (`smithy_query(query="shape",shape="MediaPlacement")`), so every member of it is an
//! address a client is INVITED to open a session on — and each of them is composed on this
//! emulator's own endpoint ([`crate::state::media_placement`]), because an address under
//! another party's domain would send the client, holding this emulator's join token, off this
//! emulator entirely. An address this service publishes therefore arrives HERE, and nothing
//! else of this emulator can place such a request: it names no modelled operation of any
//! service, and a client opening a media session presents no SigV4 credential scope to route
//! it by, so the front door could resolve no service for the one address this service told
//! the caller to dial. This plane claims it ([`claims_parts`], read by
//! `Service::can_handle`) and serves it ([`MediaPlane`], returned by
//! `Service::override_operation`).
//!
//! # What it answers, and why each answer is the one it is
//!
//! A session on one of these addresses is opened with an RFC 6455 WebSocket handshake
//! (`wss`), and what this plane decides about that handshake is the part of the join that is
//! decided by this service's own state:
//!
//! 1. the MEETING the dialled address names — the `MeetingId` the published path (or, for the
//!    viewer endpoint, its one query parameter) carries. A handshake for a meeting this store
//!    does not hold is refused with the status the protocol gives an address that names no
//!    resource, carrying the sentence this service words that absence with for every operation
//!    that resolves a `MeetingId` — so the same id reads the same whether a client asked the
//!    control plane or dialled the meeting's media;
//! 2. whether the request IS a session handshake at all: these members are dialled as
//!    WebSocket sessions, and an ordinary request to one is answered with the status that asks
//!    the client to switch protocols;
//! 3. the ATTENDEE's credential. `Attendee` is documented "Includes a unique `AttendeeId` and
//!    `JoinToken`. The `JoinToken` allows a client to authenticate and join as the specified
//!    attendee. The `JoinToken` expires when the meeting ends, or when `DeleteAttendee` is
//!    called. After that, the attendee is unable to join the meeting"
//!    (`smithy_query(query="shape",shape="Attendee")`). So a handshake that presents no join
//!    token this meeting currently holds for one of its attendees is not authenticated, and is
//!    refused as this service refuses a client whose access to an attendee's session is not
//!    its to have. Nothing is stored or consumed by that check: the join token is matched
//!    against the attendees the store holds, and an attendee a `DeleteAttendee` has removed
//!    (or a meeting a `DeleteMeeting` has ended) authenticates nobody, exactly as the
//!    documentation states.
//!
//! A handshake that passes all three is one this emulator cannot carry any further: the media
//! session behind these addresses is served by the Amazon Chime SDK media service, which this
//! process does not run — so it is answered with the status for a request whose protocol the
//! server does not implement, naming that, rather than with a socket that would accept the
//! session and then answer no frame of it. What the plane does NOT do is leave the address
//! dead: every one of those four answers is an HTTP/1.1 answer on the address the control
//! plane published, so a client that dials what `CreateMeeting` handed it learns which of
//! those four situations it is in instead of a transport error.
//!
//! WHERE it is answered is the endpoint this emulator serves its whole surface at — the one the
//! published address names ([`crate::state::announced_authority`]) — whose requests are
//! dispatched through the router, and so through the two seams above, whatever fields they
//! carry. The crate's own suite drives that path on a socket as well as in process
//! (`tests/all/meeting_projection.rs`), because a published address is only served if the bytes
//! a client writes reach this plane.
//!
//! Serving one of these requests READS: it resolves a meeting, its attendees and the
//! credential presented, and writes nothing — so a client dialling a published address leaves
//! every later caller seeing exactly what it saw.

use ::std::sync::Arc;

use ::aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse, StatusCode};
use ::aws_smithy_types::body::SdkBody;
use ::smithy_server_runtime::{http, BoxFuture, ErasedOperation, OperationContext, Service};

use crate::state::{dialed_media_meeting, service_state, StoredMeeting};

/// What this plane answers a handshake for a meeting this store does not hold: the status for
/// an address that names no resource, which is the status this service's own
/// `NotFoundException` carries for the same `MeetingId`.
const NO_SUCH_MEETING_STATUS: u16 = 404;

/// What it answers an ordinary request to an address dialled as a WebSocket session: the
/// status that names the protocol the client has to switch to (RFC 7231, 426 Upgrade
/// Required).
const UPGRADE_REQUIRED_STATUS: u16 = 426;

/// What it answers a handshake presenting no join token of an attendee of the meeting: the
/// status this service's `ForbiddenException` carries, which it already answers a client
/// reaching for an attendee's record that is not its to read.
const NOT_AUTHENTICATED_STATUS: u16 = 403;

/// What it answers a handshake this emulator cannot carry on with: the status for a request
/// whose protocol this server does not implement. The Amazon Chime SDK media session behind
/// these addresses is not served by this process.
const NO_MEDIA_SERVICE_STATUS: u16 = 501;

/// What it answers when this service's own store cannot be reached at all — the one case no
/// answer can be computed from state.
const STORE_UNAVAILABLE_STATUS: u16 = 500;

/// The media type of every answer this plane renders. Plain text, not this service's restJson1
/// envelope: what arrives here is a WebSocket handshake rather than a modelled operation, and
/// a client reading it reads the status line.
const PLAIN_TEXT: &str = "text/plain; charset=utf-8";

/// What it answers a request that does not address a published media endpoint after all.
const NOT_A_MEDIA_ADDRESS: &str =
    "This address serves the media sessions of this emulator's Amazon Chime SDK meetings.\n";

/// What it answers an ordinary request to one of them.
const UPGRADE_REQUIRED: &str =
    "A meeting's media session is opened as a WebSocket session on this address.\n";

/// What it answers a handshake that presents no join token of an attendee of the meeting.
const JOIN_TOKEN_REQUIRED: &str =
    "A meeting's media session is joined with the join token of one of its attendees.\n";

/// What it answers an authenticated handshake: the media session itself is the Amazon Chime SDK
/// media service's, and this process runs none.
const NO_MEDIA_SERVICE: &str =
    "This emulator runs no Amazon Chime SDK media service, so it carries no media session for \
     this meeting.\n";

/// What it answers when this service's store cannot be reached.
const STORE_UNAVAILABLE: &str = "This meeting's media session could not be resolved.\n";

/// The handshake field a WebSocket client offers its SUBPROTOCOLS in, which is one of the two
/// places a credential travels in a handshake (the other being the query string of the dialled
/// URL). Read case-insensitively by `http`'s own header map.
const SUBPROTOCOL_FIELD: &str = "sec-websocket-protocol";

/// How many presented values one handshake is matched against the meeting's attendees.
///
/// A client joins as ONE attendee and so presents one join token; this bound is what keeps the
/// match from scaling with the length of a field the caller chose, since both places a
/// credential travels — the subprotocols offered and the query string — are lists the client
/// writes. A handshake that buries its token past this many items is read as presenting none.
const MAX_PRESENTED_CREDENTIALS: usize = 32;

/// Whether this plane serves the request `parts` addresses.
///
/// Two conjuncts. The path (with the query, for the viewer endpoint) has to address a member of
/// a `MediaPlacement` this service publishes, carrying a `MeetingId` of the modelled form
/// ([`dialed_media_meeting`]) — and then either that id names a meeting this store HOLDS, which
/// is a request for an address this service itself handed out, or the request is a WebSocket
/// handshake, which no modelled operation of any service is. The first keeps the claim narrow
/// for ordinary traffic; the second is what lets a client dialling a meeting that has ENDED be
/// told so, instead of being answered as though the address had never been published. Both are
/// under the `MeetingId` form, so a path that merely shares one of these generic endpoint
/// prefixes with another service is not claimed here.
pub fn claims_parts<Eng: Send + Sync + 'static>(
    engine: &Eng,
    parts: &http::request::Parts,
) -> bool {
    let sent = |name: &str| parts.headers.get(name).and_then(|value| value.to_str().ok());
    claims(
        engine,
        parts.uri.path(),
        parts.uri.query(),
        ::emulator_server::is_websocket_upgrade(
            sent("connection"),
            sent("upgrade"),
            sent(":protocol"),
        ),
    )
}

/// [`claims_parts`] for the runtime-API request the serving seam is handed, read by the same
/// rule, so the claim and the serving cannot disagree about which requests are this plane's.
pub fn claims_request<Eng: Send + Sync + 'static>(engine: &Eng, req: &RtRequest) -> bool {
    claims(
        engine,
        ::service_common::request_path(req.uri()),
        ::service_common::request_query(req.uri()),
        handshake(req),
    )
}

/// The one rule both of those read.
///
/// What keeps it from straddling another service's path space is the `MeetingId`: the three
/// meeting-scoped endpoints are only read where their one segment is of that form, and the
/// viewer endpoint only where this service's own query parameter carries one. A sibling plane
/// of this emulator keys its own handshake on its own resource (a Kinesis Video signaling
/// channel on `X-Amz-ChannelARN`, an API Gateway WebSocket api on the api it holds), so no
/// request answered here is one another service would claim.
fn claims<Eng: Send + Sync + 'static>(
    engine: &Eng,
    path: &str,
    query: Option<&str>,
    handshake: bool,
) -> bool {
    let Some(meeting_id) = dialed_media_meeting(path, query) else {
        return false;
    };
    handshake || holds_meeting(engine, meeting_id)
}

/// Whether this service's store holds a meeting under `meeting_id` right now.
fn holds_meeting<Eng: Send + Sync + 'static>(engine: &Eng, meeting_id: &str) -> bool {
    service_state(engine).is_some_and(|state| state.lock().meetings.contains_key(meeting_id))
}

/// Whether `req` is a WebSocket handshake, by the core's own reading of the three fields that
/// say so.
fn handshake(req: &RtRequest) -> bool {
    ::emulator_server::is_websocket_upgrade(
        req.headers().get("connection"),
        req.headers().get("upgrade"),
        req.headers().get(":protocol"),
    )
}

/// The media plane as the ROUTER serves it: the operation `Service::override_operation` hands
/// back for a request addressed to a media endpoint this service published.
pub struct MediaPlane<Eng> {
    /// The ONE shared engine this service's operations read and write, so a handshake served
    /// here resolves the meeting and the attendees the control plane persisted.
    engine: Arc<Eng>,
}

impl<Eng: Send + Sync + 'static> MediaPlane<Eng> {
    /// Mount the plane over the shared engine.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self { engine }
    }

    /// Whether this plane serves `req`, by the one rule the claim uses.
    pub fn claims(&self, req: &RtRequest) -> bool {
        claims_request(&*self.engine, req)
    }
}

impl<Eng: Send + Sync + 'static> ErasedOperation for MediaPlane<Eng> {
    fn run<'r>(
        &'r self,
        _svc: &'r dyn Service,
        req: &'r RtRequest,
        _ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        Box::pin(async move { answered(&*self.engine, req) })
    }

    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        // A client opening a meeting's media session authenticates with the attendee's join
        // token, not with an AWS signature: it has no credential to sign a handshake with. The
        // join token is what this plane checks, below.
        Some(&[])
    }
}

/// What this plane answers one request to a published media address.
fn answered<Eng: Send + Sync + 'static>(engine: &Eng, req: &RtRequest) -> RtResponse {
    let path = ::service_common::request_path(req.uri());
    let query = ::service_common::request_query(req.uri());
    let Some(meeting_id) = dialed_media_meeting(path, query) else {
        return refusal(NO_SUCH_MEETING_STATUS, NOT_A_MEDIA_ADDRESS.to_string());
    };
    let Some(state) = service_state(engine) else {
        return refusal(STORE_UNAVAILABLE_STATUS, STORE_UNAVAILABLE.to_string());
    };
    let store = state.lock();
    let Some(meeting) = store.meetings.get(meeting_id) else {
        // The store's own sentence for a `MeetingId` it holds no meeting under — the one that
        // tells a meeting that has ENDED from an id that never named one here.
        let absent = store.meeting_absent(meeting_id);
        let sentence = absent.message.unwrap_or_default();
        return refusal(NO_SUCH_MEETING_STATUS, format!("{sentence}\n"));
    };
    if !handshake(req) {
        return refusal(UPGRADE_REQUIRED_STATUS, UPGRADE_REQUIRED.to_string());
    }
    if !authenticates_an_attendee(meeting, &presented_credentials(req, query)) {
        return refusal(NOT_AUTHENTICATED_STATUS, JOIN_TOKEN_REQUIRED.to_string());
    }
    refusal(NO_MEDIA_SERVICE_STATUS, NO_MEDIA_SERVICE.to_string())
}

/// The credentials a handshake PRESENTS, in the two places a WebSocket client can carry one:
/// the subprotocols it offers ([`SUBPROTOCOL_FIELD`], a comma-separated list) and the values of
/// the query parameters of the URL it dialled.
///
/// Both are read because the handshake has no request body and no other field a client of a
/// published URL controls; which of the two a given client uses is the client's own business,
/// and a join token is matched by VALUE either way. At most [`MAX_PRESENTED_CREDENTIALS`] items
/// of the two lists are read at all — the cap is taken before they are trimmed, so what one
/// handshake costs does not grow with the length of the fields it chose to send.
fn presented_credentials<'r>(req: &'r RtRequest, query: Option<&'r str>) -> Vec<&'r str> {
    let offered = req
        .headers()
        .get(SUBPROTOCOL_FIELD)
        .unwrap_or_default()
        .split(',');
    let in_query = query
        .unwrap_or_default()
        .split('&')
        .filter_map(|parameter| parameter.split_once('='))
        .map(|(_name, value)| value);
    offered
        .chain(in_query)
        .take(MAX_PRESENTED_CREDENTIALS)
        .map(str::trim)
        .filter(|presented| !presented.is_empty())
        .collect()
}

/// Whether any presented credential is the `JoinToken` of an attendee this meeting holds.
///
/// The comparison is over the meeting's OWN attendees, so one meeting's join token never joins
/// another's session, and an attendee `DeleteAttendee` has removed is no longer there to
/// authenticate anybody — which is what the `Attendee` documentation states about the token's
/// lifetime.
///
/// A plain byte comparison, deliberately: the tokens are this emulator's own, minted for a test
/// double that holds no real credential, and the timing of a comparison is not one of the
/// properties this process keeps.
fn authenticates_an_attendee(meeting: &StoredMeeting, presented: &[&str]) -> bool {
    meeting.attendees.values().any(|attendee| {
        attendee
            .join_token
            .as_deref()
            .is_some_and(|token| presented.contains(&token))
    })
}

/// One answer of this plane's own: a status and the sentence that words it.
fn refusal(code: u16, sentence: String) -> RtResponse {
    let body = sentence.into_bytes();
    let length = body.len();
    let mut answer = RtResponse::new(status(code), SdkBody::from(body));
    answer
        .headers_mut()
        .insert("content-type", PLAIN_TEXT.to_string());
    answer.headers_mut().insert("content-length", length.to_string());
    answer
}

/// A status this plane answers with. Every call site passes one of the constants above, so the
/// conversion cannot fail; an impossible value is reported as the one case that computes no
/// answer at all.
fn status(code: u16) -> StatusCode {
    StatusCode::try_from(code).unwrap_or_else(|_| {
        StatusCode::try_from(STORE_UNAVAILABLE_STATUS).expect("a valid status")
    })
}
