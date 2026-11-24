// SPDX-License-Identifier: CC0-1.0

//! The ADDRESSES a meeting's `MediaPlacement` announces, composed here and nowhere else so
//! that every operation projecting a `Meeting` announces one set.
//!
//! `MediaPlacement` is "A set of endpoints used by clients to connect to the media service
//! group for an Amazon Chime SDK meeting"
//! (`smithy_query(query="shape",shape="MediaPlacement")`), and each member is an address a
//! client then DIALS to join the meeting's media, signaling, screen or event-ingestion
//! session. So the HOST of every one of them is the `host[:port]` of the one endpoint this
//! emulator serves its whole surface at ([`announced_authority`]) rather than a name
//! belonging to someone else: an address under a third party's domain sends the client —
//! with the join token and the credentials this emulator minted — off this emulator
//! entirely, and nothing it reaches there knows the meeting.
//!
//! What each member keeps is everything else a client parses out of it: its PROTOCOL — the
//! scheme (`wss` for the signaling, audio-fallback and screen endpoints, `https` for the
//! TURN-control and event-ingestion endpoints, and none at all for the audio host, which is
//! a bare `host:port`) — and the PATH and QUERY that endpoint is dialed at, which is the
//! service's own for that endpoint, carrying the `MeetingId` where the endpoint belongs to
//! one meeting and carrying none where it belongs to the deployment. A client reads these
//! strings apart, so only the authority is this deployment's; the rest of each value is the
//! endpoint's identity.
//!
//! The set is composed at PROJECTION time, per request
//! ([`StoredMeeting::announced_meeting`]), and none of it is stored: the address this process
//! is reached at is not a property of the meeting, and a read composes the values without
//! writing anything.
//!
//! Nothing here is ever CONNECTED to. This module formats strings into a response and holds
//! no outbound client, socket or resolver; every address it composes is one of this
//! process's own inbound addresses, so no value it reports leads off this machine.
//!
//! What ANSWERS when a client dials one of these addresses is [`crate::service::media_plane`],
//! which reads the same path and query BACK ([`dialed_media_meeting`]) to resolve the meeting
//! whose session was dialled. Composing and reading an address are two directions of one
//! format, so both live here: a member whose path moved would otherwise be published at one
//! address and served at another.

use super::*;

/// The scheme of a member a client opens a WebSocket session on. `wss`, not `ws`: this
/// emulator serves TLS on the endpoint these addresses name.
const WEBSOCKET_SCHEME: &str = "wss";

/// The scheme of a member a client sends HTTP requests to, on the same TLS endpoint.
const HTTPS_SCHEME: &str = "https";

/// The path a meeting's audio fallback session is dialed at, ahead of the `MeetingId`.
pub const AUDIO_FALLBACK_PATH: &str = "/calls";

/// The path a meeting's signaling (control) session is dialed at, ahead of the `MeetingId`.
pub const SIGNALING_PATH: &str = "/control";

/// The path a TURN session is requested at. It belongs to the deployment rather than to one
/// meeting, so it carries no `MeetingId`.
const TURN_CONTROL_PATH: &str = "/v2/turn_sessions";

/// The path a meeting's screen session — the one both the screen DATA and the screen SHARING
/// member name — is dialed at, ahead of the `MeetingId`.
pub const SCREEN_PATH: &str = "/v2/screen";

/// The path a screen VIEWER connects at, which carries its parameters in the query string
/// rather than the `MeetingId` in the path.
pub const SCREEN_VIEWING_PATH: &str = "/ws/connect";

/// The query string that connect URL carries ahead of the meeting's call id: a `passcode`
/// and a `viewer_uuid` that no caller supplies — so both read `null` — which are part of the
/// fixed text of that member rather than per-run values. A client parses this string, so
/// none of its parameters may be dropped.
const VIEWER_CONNECT_PARAMETERS: &str = "passcode=null&viewer_uuid=null";

/// The query parameter that connect URL carries the meeting's own id in.
pub const CALL_ID_PARAMETER: &str = "X-BitHub-Call-Id";

/// The path client events are ingested at. Deployment-wide, like [`TURN_CONTROL_PATH`].
const EVENT_INGESTION_PATH: &str = "/v1/client-events";

impl StoredMeeting {
    /// The `Meeting` this record is REPORTED as to the request in `ctx`: the stored record,
    /// with its `MediaPlacement` composed on the address this emulator answers on.
    ///
    /// Every operation that answers a `Meeting` — both creates and every read — projects it
    /// through here, which is what makes one meeting report one, mutually consistent set of
    /// endpoints however it is read.
    #[must_use]
    pub fn announced_meeting(
        &self,
        ctx: &::smithy_server_runtime::OperationContext<'_>,
    ) -> Meeting {
        let mut meeting = self.meeting.clone();
        meeting.media_placement = Some(media_placement(
            &announced_authority(ctx),
            meeting.meeting_id.as_deref().unwrap_or_default(),
        ));
        meeting
    }
}

/// The media endpoints reported for the meeting `meeting_id`, each on `authority` — a
/// `host[:port]` of this emulator's own ([`announced_authority`]).
///
/// Every URL is a pure function of that address and the `MeetingId`, so two projections of
/// one meeting are equal — which is what makes `GetMeeting` report what `CreateMeeting`
/// returned.
#[must_use]
pub fn media_placement(authority: &str, meeting_id: &str) -> MediaPlacement {
    MediaPlacement {
        // The audio host is the one member that is a bare `host:port` and carries no path,
        // so it names the address itself: which meeting a session belongs to is carried by
        // the attendee's join token, not by this value.
        audio_host_url: Some(authority.to_string()),
        audio_fallback_url: Some(format!(
            "{WEBSOCKET_SCHEME}://{authority}{AUDIO_FALLBACK_PATH}/{meeting_id}"
        )),
        signaling_url: Some(format!(
            "{WEBSOCKET_SCHEME}://{authority}{SIGNALING_PATH}/{meeting_id}"
        )),
        turn_control_url: Some(format!("{HTTPS_SCHEME}://{authority}{TURN_CONTROL_PATH}")),
        screen_data_url: Some(format!(
            "{WEBSOCKET_SCHEME}://{authority}{SCREEN_PATH}/{meeting_id}"
        )),
        // The same address as the screen DATA endpoint: one screen session serves both, which
        // is how the service reports the pair.
        screen_sharing_url: Some(format!(
            "{WEBSOCKET_SCHEME}://{authority}{SCREEN_PATH}/{meeting_id}"
        )),
        screen_viewing_url: Some(format!(
            "{WEBSOCKET_SCHEME}://{authority}{SCREEN_VIEWING_PATH}\
             ?{VIEWER_CONNECT_PARAMETERS}&{CALL_ID_PARAMETER}={meeting_id}"
        )),
        event_ingestion_url: Some(format!("{HTTPS_SCHEME}://{authority}{EVENT_INGESTION_PATH}")),
    }
}

/// The `MeetingId` a DIALLED address names — read back out of the very path and query
/// [`media_placement`] composed, for the four members a client opens a SESSION on: the
/// signaling, audio-fallback and screen endpoints, which carry the id in their path, and the
/// screen-viewing endpoint, which carries it in its own query parameter. (`ScreenDataUrl` and
/// `ScreenSharingUrl` name one screen session, so they read back alike.)
///
/// `None` for a request that addresses no such member, which is every other path on this
/// emulator's endpoint — the control plane's own routes among them, so this reader claims none
/// of them — and for one whose id is not of the modelled form ([`is_meeting_id`]), which is no
/// address this service ever published.
///
/// The two `https` members are not read back: `TurnControlUrl` and `EventIngestionUrl` carry no
/// `MeetingId` at all, so a request to either names no meeting to resolve.
///
/// The authority is NOT compared, because it is not part of the identity of these addresses:
/// the host a client reached this emulator at is whatever its own resolver answered
/// ([`announced_authority`] publishes one of them), while what says WHICH session was dialled
/// is the path — and, for the viewer endpoint, the one query parameter that carries the
/// meeting's id.
#[must_use]
pub fn dialed_media_meeting<'r>(path: &'r str, query: Option<&'r str>) -> Option<&'r str> {
    for dialed_at in [SIGNALING_PATH, AUDIO_FALLBACK_PATH, SCREEN_PATH] {
        if let Some(meeting_id) = meeting_id_under(path, dialed_at) {
            return Some(meeting_id);
        }
    }
    if path == SCREEN_VIEWING_PATH {
        let meeting_id = ::service_common::query_parameter(query?, CALL_ID_PARAMETER)?;
        if is_meeting_id(meeting_id) {
            return Some(meeting_id);
        }
    }
    None
}

/// The ONE path segment `path` carries under `dialed_at`, when it is a `MeetingId`: that is
/// where a meeting-scoped member carries one.
///
/// `None` when the path is not under that endpoint, carries nothing under it, or carries more
/// than one segment: the published form is exactly `<endpoint>/<MeetingId>`, and a longer path
/// is a different address.
fn meeting_id_under<'r>(path: &'r str, dialed_at: &str) -> Option<&'r str> {
    let meeting_id = path.strip_prefix(dialed_at)?.strip_prefix('/')?;
    (!meeting_id.contains('/') && is_meeting_id(meeting_id)).then_some(meeting_id)
}

/// Whether `value` is of the form a `MeetingId` takes, which is what every address composed
/// above carries.
///
/// From the model: `MeetingId` targets `GuidString`, `@length(36..36)` with the pattern
/// `^[a-fA-F0-9]{8}(?:-[a-fA-F0-9]{4}){3}-[a-fA-F0-9]{12}$`
/// (`smithy_query(query="shape",shape="GuidString")`) — the form this store MINTS a meeting's
/// id in. Read as the five hyphen-separated hex groups that pattern states,
/// rather than with a regular expression, so no pattern engine runs on a caller's path.
///
/// It is what keeps these generic endpoint paths from reading ANY segment as a meeting of this
/// service: a value that cannot be a `MeetingId` is no address this service published.
#[must_use]
pub fn is_meeting_id(value: &str) -> bool {
    let mut groups = value.split('-');
    let lengths = [8usize, 4, 4, 4, 12];
    for length in lengths {
        match groups.next() {
            Some(group) => {
                if group.len() != length || !group.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                    return false;
                }
            }
            None => return false,
        }
    }
    groups.next().is_none()
}

/// The `host[:port]` every address this service announces is reached at: the ONE endpoint
/// this emulator serves its whole surface at.
///
/// Read through the tree's one statement of that address — the front door's own published
/// base ([`::service_common::data_plane_addressing::emulator_endpoint_authority`], "the
/// `host[:port]` of the ONE endpoint this emulator serves its whole surface at … what a
/// DISCOVERY operation publishes for a plane the client then DIALS") — and handed on
/// unchanged, because a `MediaPlacement` member is exactly that: an address the caller is
/// told to dial, on a plane served at the front door rather than on a listener of its own.
///
/// It does not depend on which host the request that read the meeting carried, so every
/// reader of one meeting is told the same endpoints — which is what makes `GetMeeting` equal
/// to what `CreateMeeting` returned, and what lets one client hand a `MeetingId` to another.
#[must_use]
pub fn announced_authority(ctx: &::smithy_server_runtime::OperationContext<'_>) -> String {
    ::service_common::data_plane_addressing::emulator_endpoint_authority(ctx)
}
