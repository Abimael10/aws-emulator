// SPDX-License-Identifier: CC0-1.0

//! The SDP OFFER a storage session sends to the video producing device that joined it.
//!
//! The WebRTC ingestion handshake is offer-driven: the device joins the channel's storage
//! session and the session offers it a session description, which the device answers before
//! it starts sending media. `JoinStorageSession`'s own modelled docs state the rule —
//! "Idempotent - This API is not idempotent. Retry behavior - This is counted as a new API
//! call. Concurrent calls - Concurrent calls are allowed. An offer is sent once per each
//! call." — so EVERY call produces an offer of its own, and this module is where one is
//! generated.
//!
//! An offer travels the signaling plane as a base64 `messagePayload` carrying the JSEP
//! description envelope `{"type":"offer","sdp":…}`, whose `sdp` is an RFC 4566 session
//! description. The description itself is built and marshalled by the `sdp` crate — a
//! maintained implementation of that format — so no line of it is written by hand here.
//!
//! What the description says is this operation's own documented contract: "a video
//! producing device is able to ingest both audio and video media into a stream… Both audio
//! and video tracks are currently required for WebRTC ingestion. Current requirements:
//! Video track: H.264 Audio track: Opus". So the offer carries exactly those two media
//! sections, and the storage session only RECEIVES them, which is what `recvonly` states.
//!
//! The offer is a function of the JOIN it belongs to: the session description's version
//! field is the session's own join generation ([`crate::state::StorageSession::join_count`]
//! at that call), so the N-th call's offer is not the (N-1)-th call's.

use ::sdp::description::media::MediaDescription;
use ::sdp::description::session::SessionDescription;

/// The `type` the JSEP description envelope of an offer states.
///
/// The envelope is the `RTCSessionDescription` form every Kinesis Video Streams WebRTC
/// client reads a `messagePayload` as: a JSON object of a `type` and the `sdp` text.
pub const OFFER_DESCRIPTION_TYPE: &str = "offer";

/// The envelope member carrying [`OFFER_DESCRIPTION_TYPE`].
const DESCRIPTION_TYPE_MEMBER: &str = "type";

/// The envelope member carrying the session description text.
const DESCRIPTION_SDP_MEMBER: &str = "sdp";

/// The media type of the offer's audio section.
const AUDIO_MEDIA: &str = "audio";

/// The media type of the offer's video section.
const VIDEO_MEDIA: &str = "video";

/// The `a=mid` of the audio section, and the first id the bundle group names.
const AUDIO_MID: &str = "0";

/// The `a=mid` of the video section.
const VIDEO_MID: &str = "1";

/// The attribute that bundles the two sections onto one transport, as a WebRTC offer does.
const BUNDLE_GROUP_ATTRIBUTE: &str = "group";

/// The direction the storage session negotiates: it RECEIVES the device's media and sends
/// none, the one-way video ingestion this operation documents.
const RECEIVE_ONLY_ATTRIBUTE: &str = "recvonly";

/// RTP and RTCP share one port, as every WebRTC endpoint negotiates.
const RTCP_MUX_ATTRIBUTE: &str = "rtcp-mux";

/// The audio codec the offer carries: "Audio track: Opus"
/// (`smithy_query(query="doc",shape="JoinStorageSession")`).
const OPUS_ENCODING: &str = "opus";

/// Opus is carried at 48 kHz in two channels, the only sampling RFC 7587 defines for its
/// RTP payload format.
const OPUS_CLOCK_RATE: u32 = 48_000;

/// See [`OPUS_CLOCK_RATE`].
const OPUS_CHANNELS: u16 = 2;

/// A dynamic RTP payload type (RFC 3551 assigns 96-127 dynamically) for the Opus track.
const OPUS_PAYLOAD_TYPE: u8 = 111;

/// The Opus format parameters a WebRTC offer states: the shortest packet it accepts, in
/// milliseconds, and that in-band forward error correction may be used (RFC 7587 §7.1).
const OPUS_FORMAT_PARAMETERS: &str = "minptime=10;useinbandfec=1";

/// The video codec the offer carries: "Video track: H.264"
/// (`smithy_query(query="doc",shape="JoinStorageSession")`).
const H264_ENCODING: &str = "H264";

/// Every RTP video format is carried at 90 kHz (RFC 3551 §6).
const H264_CLOCK_RATE: u32 = 90_000;

/// A video codec's RTP payload format states no channel count.
const NO_CHANNELS: u16 = 0;

/// A dynamic RTP payload type for the H.264 track; see [`OPUS_PAYLOAD_TYPE`].
const H264_PAYLOAD_TYPE: u8 = 96;

/// The H.264 format parameters a WebRTC offer states (RFC 6184 §8.1): non-interleaved
/// packetization, and the Constrained Baseline profile at level 3.1 every WebRTC endpoint
/// supports.
const H264_FORMAT_PARAMETERS: &str =
    "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f";

/// The session description one join is offered, as RFC 4566 text.
///
/// `join_generation` is the join this offer belongs to — the session's `join_count` at that
/// call — and becomes the description's session VERSION, which is the field RFC 4566 gives
/// for "a version number for this session description": a later offer for the same session
/// carries a later version, so no two calls are offered the same description.
pub fn offer_sdp(join_generation: u64) -> String {
    let audio = MediaDescription::new_jsep_media_description(AUDIO_MEDIA.to_string(), vec![])
        .with_codec(
            OPUS_PAYLOAD_TYPE,
            OPUS_ENCODING.to_string(),
            OPUS_CLOCK_RATE,
            OPUS_CHANNELS,
            OPUS_FORMAT_PARAMETERS.to_string(),
        )
        .with_value_attribute("mid".to_string(), AUDIO_MID.to_string())
        .with_property_attribute(RTCP_MUX_ATTRIBUTE.to_string())
        .with_property_attribute(RECEIVE_ONLY_ATTRIBUTE.to_string());
    let video = MediaDescription::new_jsep_media_description(VIDEO_MEDIA.to_string(), vec![])
        .with_codec(
            H264_PAYLOAD_TYPE,
            H264_ENCODING.to_string(),
            H264_CLOCK_RATE,
            NO_CHANNELS,
            H264_FORMAT_PARAMETERS.to_string(),
        )
        .with_value_attribute("mid".to_string(), VIDEO_MID.to_string())
        .with_property_attribute(RTCP_MUX_ATTRIBUTE.to_string())
        .with_property_attribute(RECEIVE_ONLY_ATTRIBUTE.to_string());

    let mut description = SessionDescription::new_jsep_session_description(false);
    description.origin.session_version = join_generation;
    description
        .with_value_attribute(
            BUNDLE_GROUP_ATTRIBUTE.to_string(),
            format!("BUNDLE {AUDIO_MID} {VIDEO_MID}"),
        )
        .with_media(audio)
        .with_media(video)
        .marshal()
}

/// The `messagePayload` one join's offer is delivered as: base64 of the JSEP description
/// envelope around [`offer_sdp`].
///
/// Base64 is the encoding every signaling payload is carried in, and the codec is the one
/// the wire serializer already uses for a Smithy blob, so this crate adds none of its own.
pub fn offer_payload(join_generation: u64) -> String {
    let mut envelope = ::serde_json::Map::new();
    envelope.insert(
        DESCRIPTION_TYPE_MEMBER.to_string(),
        ::serde_json::Value::String(OFFER_DESCRIPTION_TYPE.to_string()),
    );
    envelope.insert(
        DESCRIPTION_SDP_MEMBER.to_string(),
        ::serde_json::Value::String(offer_sdp(join_generation)),
    );
    ::aws_smithy_types::base64::encode(::serde_json::Value::Object(envelope).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The offer is a real session description: it round-trips through the same
    /// implementation that marshalled it, and carries the two media sections WebRTC
    /// ingestion requires.
    #[test]
    fn an_offer_is_a_session_description_carrying_both_ingestion_tracks() {
        // The rows span the joins a session serves: its first, a later one, and one far
        // along.
        for generation in [1_u64, 2, 97] {
            let sdp = offer_sdp(generation);
            let mut reader = ::std::io::Cursor::new(sdp.as_bytes());
            let parsed = SessionDescription::unmarshal(&mut reader).unwrap_or_else(|error| {
                panic!("case generation {generation}: the offer must parse as SDP ({error}): {sdp}")
            });
            assert_eq!(
                parsed.version, 0,
                "case generation {generation}: expected the RFC 4566 version line 'v=0', got {:?}",
                parsed.version
            );
            assert_eq!(
                parsed.origin.session_version, generation,
                "case generation {generation}: the description's version must be the join's, \
                 got {:?}",
                parsed.origin.session_version
            );
            let media: Vec<String> = parsed
                .media_descriptions
                .iter()
                .map(|media| media.media_name.media.clone())
                .collect();
            assert_eq!(
                media,
                vec![AUDIO_MEDIA.to_string(), VIDEO_MEDIA.to_string()],
                "case generation {generation}: expected an audio and a video media line, \
                 got {media:?}"
            );
        }
    }

    /// Two joins are offered two descriptions, which is what "an offer is sent once per
    /// each call" means for a client that has to tell them apart.
    #[test]
    fn each_join_is_offered_its_own_payload() {
        let payloads: Vec<String> = (1..=4).map(offer_payload).collect();
        for (index, payload) in payloads.iter().enumerate() {
            assert!(
                !payload.is_empty(),
                "case join {}: the payload must not be empty",
                index + 1
            );
            for (other, than) in payloads.iter().enumerate() {
                assert!(
                    index == other || payload != than,
                    "case join {} against join {}: each call's offer must be its own, got \
                     {payload:?} twice",
                    index + 1,
                    other + 1
                );
            }
        }
    }
}
