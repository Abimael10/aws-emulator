// SPDX-License-Identifier: CC0-1.0

//! The PLAYLISTS and the MANIFEST of a streaming session: the text a media player
//! reads the session's media out of.
//!
//! Three documents, one per root a player can start from, all rendered from the
//! session the minting call recorded and the fragments the archive still holds
//! ([`crate::service::playback_session::PlaybackView`]):
//!
//! * the HLS MASTER playlist, which "contains a URL for the GetHLSMediaPlaylist
//!   action for each track, and additional metadata for the media player,
//!   including estimated bitrate and resolution";
//! * the HLS MEDIA playlist, which "contains a URL to access the MP4
//!   initialization fragment with the GetMP4InitFragment action, and URLs to
//!   access the MP4 media fragments with the GetMP4MediaFragment actions ... also
//!   contains metadata about the stream that the player needs to play it, such as
//!   whether the PlaybackMode is LIVE or ON_DEMAND";
//! * the MPEG-DASH manifest, whose `S` elements carry the fragment's own number
//!   and timestamp when the session was minted with `DisplayFragmentNumber` /
//!   `DisplayFragmentTimestamp` of `ALWAYS`.
//!
//! The tags are RFC 8216's (HTTP Live Streaming) and the manifest's elements are
//! ISO/IEC 23009-1's (MPEG-DASH); every value in them is the session's or the
//! archive's. Each resource is named RELATIVE to the document naming it, so a
//! player resolves it against the URL it was handed and stays on the endpoint it
//! reached this emulator on.

use crate::service::playback_session::{PlayedFragment, PlaybackView};
use crate::state::{
    to_date_time, FRAGMENT_NUMBER_FIELD, HLS_MEDIA_PLAYLIST_RESOURCE, MP4_INIT_FRAGMENT_RESOURCE,
    MP4_MEDIA_FRAGMENT_RESOURCE, TRACK_NUMBER_FIELD, TS_FRAGMENT_RESOURCE,
};

/// The media type an HLS playlist is served as: the type registered for one
/// (RFC 8216 §4).
pub(crate) const HLS_PLAYLIST_MEDIA_TYPE: &str = "application/vnd.apple.mpegurl";

/// The media type an MPEG-DASH manifest is served as (ISO/IEC 23009-1 §B.1).
pub(crate) const DASH_MANIFEST_MEDIA_TYPE: &str = "application/dash+xml";

/// The `ContainerFormat` whose media is MP4 fragments: "Specifying the
/// FRAGMENTED_MP4 container format packages the media into MP4 fragments (fMP4 or
/// CMAF)".
pub(crate) const FRAGMENTED_MP4: &str = "FRAGMENTED_MP4";

/// The `PlaybackMode` whose playlist is complete, so it is the only one closed with
/// an end tag: "The HLS media playlist is typically static for sessions with a
/// PlaybackType of ON_DEMAND. The HLS media playlist is continually updated with
/// new fragments for sessions with a PlaybackType of LIVE."
const ON_DEMAND: &str = "ON_DEMAND";

/// The value of a session parameter that asks for something on every fragment.
const ALWAYS: &str = "ALWAYS";

/// The `DiscontinuityMode` that marks only a real break in the timeline: "a
/// discontinuity marker is placed between fragments that have a gap or overlap of
/// more than 50 milliseconds".
const ON_DISCONTINUITY: &str = "ON_DISCONTINUITY";

/// How far apart two fragments have to be for `ON_DISCONTINUITY` to mark the break:
/// the "more than 50 milliseconds" of that mode's own documentation.
const DISCONTINUITY_TOLERANCE_MILLIS: i64 = 50;

/// The HLS protocol version a playlist of MP4 fragments declares: `EXT-X-MAP`
/// "MUST NOT appear in a Media Playlist with compatibility version less than 6"
/// when its segments are fragmented MP4 (RFC 8216 §7).
const HLS_VERSION_FRAGMENTED_MP4: u8 = 6;

/// The version a playlist of MPEG TS segments declares: the one that admits a
/// fractional `EXTINF` duration (RFC 8216 §7).
const HLS_VERSION_MPEG_TS: u8 = 3;

/// How many units of time one second of a manifest's timeline holds: the
/// millisecond, the precision the archive stamps fragments and durations at.
const MANIFEST_TIMESCALE: u32 = 1_000;

/// How long a player should buffer before it starts, as the manifest's
/// `minBufferTime` states it. The emulator's own choice, in the ISO 8601 duration
/// form that attribute takes.
const MANIFEST_MIN_BUFFER: &str = "PT2S";

/// The namespace the `kvs:`-prefixed attributes of a manifest's `S` elements are
/// bound to. The attribute NAMES are the archived-media documentation's
/// (`kvs:fn`, `kvs:ts`); the binding is this emulator's, because a prefixed
/// attribute needs one to be well-formed XML.
const KVS_NAMESPACE: &str = "urn:aws:amazon:kinesisvideo";

/// The HLS master playlist of `view`: the one variant this session serves, the
/// metadata a player chooses it by, and the URL of that variant's media playlist.
pub(crate) fn master_playlist(view: &PlaybackView<'_>) -> String {
    let track = view.track_number.to_string();
    format!(
        "#EXTM3U\n\
         #EXT-X-VERSION:{version}\n\
         #EXT-X-INDEPENDENT-SEGMENTS\n\
         #EXT-X-STREAM-INF:BANDWIDTH={bandwidth},CODECS=\"{codecs}\",RESOLUTION={width}x{height}\n\
         {media_playlist}\n",
        version = hls_version(view),
        bandwidth = peak_bits_per_second(&view.fragments),
        codecs = avc_codec(view),
        width = view.track.width,
        height = view.track.height,
        media_playlist =
            view.sibling(HLS_MEDIA_PLAYLIST_RESOURCE, &[(TRACK_NUMBER_FIELD, &track)]),
    )
}

/// The HLS media playlist of `view`: the fragments the session serves, each with
/// the duration the archive records for it, and the tags the session's own
/// parameters ask for.
pub(crate) fn media_playlist(view: &PlaybackView<'_>) -> String {
    let session = view.session;
    let track = view.track_number.to_string();
    let fragmented_mp4 = session.container_format == FRAGMENTED_MP4;
    let mut playlist = format!(
        "#EXTM3U\n\
         #EXT-X-VERSION:{}\n\
         #EXT-X-TARGETDURATION:{}\n\
         #EXT-X-MEDIA-SEQUENCE:0\n",
        hls_version(view),
        target_duration_seconds(&view.fragments),
    );
    // "The HLS media playlist is typically static for sessions with a PlaybackType
    // of ON_DEMAND": a complete playlist is the one a player may treat as a
    // recording, and the only one closed with `EXT-X-ENDLIST` below.
    if session.playback_mode == ON_DEMAND {
        playlist.push_str("#EXT-X-PLAYLIST-TYPE:VOD\n");
    }
    // The MP4 pair carries its initialization in a resource of its own, which is
    // what `EXT-X-MAP` names (RFC 8216 §4.3.2.5); an MPEG TS fragment carries
    // "both initialization and media data", so it needs none.
    if fragmented_mp4 {
        playlist.push_str(&format!(
            "#EXT-X-MAP:URI=\"{}\"\n",
            view.sibling(MP4_INIT_FRAGMENT_RESOURCE, &[(TRACK_NUMBER_FIELD, &track)])
        ));
    }

    let mut previous: Option<&PlayedFragment> = None;
    for fragment in &view.fragments {
        if marks_discontinuity(session.discontinuity_mode.as_deref(), previous, fragment) {
            playlist.push_str("#EXT-X-DISCONTINUITY\n");
        }
        // "Specifies when the fragment start timestamps should be included in the
        // HLS media playlist", which is the wall-clock tag of RFC 8216 §4.3.2.6.
        if session.display_fragment_timestamp == ALWAYS {
            playlist.push_str(&format!(
                "#EXT-X-PROGRAM-DATE-TIME:{}\n",
                reported_instant(fragment.start_millis)
            ));
        }
        playlist.push_str(&format!(
            "#EXTINF:{},\n{}\n",
            seconds(fragment.duration_millis),
            media_resource(view, fragment, &track),
        ));
        previous = Some(fragment);
    }
    if session.playback_mode == ON_DEMAND {
        playlist.push_str("#EXT-X-ENDLIST\n");
    }
    playlist
}

/// The MPEG-DASH manifest of `view`: one period, one adaptation set and the one
/// representation this session serves, listing its initialization and every
/// fragment of its playlist.
pub(crate) fn dash_manifest(view: &PlaybackView<'_>) -> String {
    let session = view.session;
    let track = view.track_number.to_string();
    let first = view.first_start_millis();
    let total: u64 = view
        .fragments
        .iter()
        .map(|fragment| fragment.duration_millis)
        .sum();

    let mut timeline = String::new();
    let mut segments = String::new();
    for fragment in &view.fragments {
        let at = fragment.start_millis.saturating_sub(first).max(0);
        let mut element = format!("          <S t=\"{at}\" d=\"{}\"", fragment.duration_millis);
        // "If DisplayFragmentNumber is set to ALWAYS, the Kinesis Video Streams
        // fragment number is added to each S element in the manifest file with the
        // attribute name kvs:fn."
        if session.display_fragment_number.as_deref() == Some(ALWAYS) {
            element.push_str(&format!(" kvs:fn=\"{}\"", escaped(&fragment.number)));
        }
        // "If DisplayFragmentTimestamp is set to ALWAYS, the accurate fragment
        // timestamp is added to each S element in the manifest file with the
        // attribute name kvs:ts."
        if session.display_fragment_timestamp == ALWAYS {
            element.push_str(&format!(
                " kvs:ts=\"{}\"",
                escaped(&reported_instant(fragment.start_millis))
            ));
        }
        element.push_str("/>\n");
        timeline.push_str(&element);
        segments.push_str(&format!(
            "        <SegmentURL media=\"{}\"/>\n",
            escaped(&media_resource(view, fragment, &track))
        ));
    }

    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
         <MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\" xmlns:kvs=\"{KVS_NAMESPACE}\" \
         profiles=\"urn:mpeg:dash:profile:isoff-main:2011\" type=\"{presentation}\" \
         minBufferTime=\"{MANIFEST_MIN_BUFFER}\" mediaPresentationDuration=\"{duration}\">\n\
         \x20 <Period id=\"1\" start=\"PT0S\">\n\
         \x20   <AdaptationSet mimeType=\"video/mp4\" segmentAlignment=\"true\" \
         startWithSAP=\"1\">\n\
         \x20     <Representation id=\"{track}\" codecs=\"{codecs}\" width=\"{width}\" \
         height=\"{height}\" bandwidth=\"{bandwidth}\">\n\
         \x20       <SegmentList timescale=\"{MANIFEST_TIMESCALE}\">\n\
         \x20         <Initialization sourceURL=\"{initialization}\"/>\n\
         \x20         <SegmentTimeline>\n\
         {timeline}\
         \x20         </SegmentTimeline>\n\
         {segments}\
         \x20       </SegmentList>\n\
         \x20     </Representation>\n\
         \x20   </AdaptationSet>\n\
         \x20 </Period>\n\
         </MPD>\n",
        // A complete presentation is static; one a player re-reads for new
        // fragments is dynamic, which is what the two live modes serve.
        presentation = match session.playback_mode == ON_DEMAND {
            true => "static",
            false => "dynamic",
        },
        duration = iso_duration(total),
        codecs = avc_codec(view),
        width = view.track.width,
        height = view.track.height,
        bandwidth = peak_bits_per_second(&view.fragments),
        initialization = escaped(
            &view.sibling(MP4_INIT_FRAGMENT_RESOURCE, &[(TRACK_NUMBER_FIELD, &track)])
        ),
    )
}

/// The resource one fragment's media is fetched at, as this session's
/// `ContainerFormat` names it: the MP4 media fragment, or the MPEG TS fragment
/// that is served "instead of GetMP4InitFragment and GetMP4MediaFragment".
fn media_resource(view: &PlaybackView<'_>, fragment: &PlayedFragment, track: &str) -> String {
    let resource = match view.session.container_format == FRAGMENTED_MP4 {
        true => MP4_MEDIA_FRAGMENT_RESOURCE,
        false => TS_FRAGMENT_RESOURCE,
    };
    view.sibling(
        resource,
        &[
            (FRAGMENT_NUMBER_FIELD, &fragment.number),
            (TRACK_NUMBER_FIELD, track),
        ],
    )
}

/// Whether a discontinuity marker goes between `previous` and `fragment`, as the
/// session's `DiscontinuityMode` asks.
///
/// `ALWAYS` places one "between every fragment in the HLS media playlist",
/// `ON_DISCONTINUITY` only "between fragments that have a gap or overlap of more
/// than 50 milliseconds", and `NEVER` places none. The first fragment has nothing
/// before it, so no marker precedes it under any mode.
fn marks_discontinuity(
    mode: Option<&str>,
    previous: Option<&PlayedFragment>,
    fragment: &PlayedFragment,
) -> bool {
    let Some(previous) = previous else {
        return false;
    };
    match mode {
        Some(ALWAYS) => true,
        Some(ON_DISCONTINUITY) => {
            let ends = previous.start_millis.saturating_add(
                i64::try_from(previous.duration_millis).unwrap_or(i64::MAX),
            );
            fragment
                .start_millis
                .saturating_sub(ends)
                .saturating_abs()
                > DISCONTINUITY_TOLERANCE_MILLIS
        }
        _ => false,
    }
}

/// The HLS compatibility version a session's playlists declare, which the
/// container of its segments decides.
fn hls_version(view: &PlaybackView<'_>) -> u8 {
    match view.session.container_format == FRAGMENTED_MP4 {
        true => HLS_VERSION_FRAGMENTED_MP4,
        false => HLS_VERSION_MPEG_TS,
    }
}

/// The codec the one variant declares: `avc1` and the profile, profile
/// compatibility and level of the coded video, as RFC 6381 §3.3 spells an AVC
/// codec identifier.
///
/// Those three bytes are the ones that follow the sequence parameter set's own
/// unit header, so they come from the parameter set the archived track declared.
fn avc_codec(view: &PlaybackView<'_>) -> String {
    let profile = view.track.seq_param_set.get(1..4).unwrap_or_default();
    let mut codec = String::from("avc1");
    for byte in profile {
        codec.push_str(&format!("{byte:02x}"));
    }
    codec
}

/// The peak segment bit rate of the fragments, in bits per second, which is what
/// `BANDWIDTH` "represents ... of the Variant Stream" (RFC 8216 §4.3.4.2) and what
/// a representation's `bandwidth` states.
///
/// The PEAK rather than the mean, because that is what the tag is defined as: the
/// largest rate any one fragment of the playlist carries. A fragment the archive
/// records no duration for states no rate of its own; a playlist of nothing but
/// those reports one bit per second, the smallest positive rate, because the
/// attribute is required.
fn peak_bits_per_second(fragments: &[PlayedFragment]) -> u64 {
    fragments
        .iter()
        .filter(|fragment| fragment.duration_millis > 0)
        .map(|fragment| {
            fragment
                .size_bytes
                .saturating_mul(8)
                .saturating_mul(1_000)
                / fragment.duration_millis
        })
        .max()
        .unwrap_or(1)
        .max(1)
}

/// The `EXT-X-TARGETDURATION` of a playlist: "the maximum Media Segment duration"
/// in whole seconds (RFC 8216 §4.3.3.1), which every segment's own duration has to
/// fit inside.
fn target_duration_seconds(fragments: &[PlayedFragment]) -> u64 {
    fragments
        .iter()
        .map(|fragment| fragment.duration_millis.div_ceil(1_000))
        .max()
        .unwrap_or(1)
        .max(1)
}

/// `millis` as the seconds an `EXTINF` duration states, at the millisecond
/// precision the archive records.
fn seconds(millis: u64) -> String {
    format!("{}.{:03}", millis / 1_000, millis % 1_000)
}

/// `millis` as the ISO 8601 duration a manifest's `mediaPresentationDuration`
/// takes.
fn iso_duration(millis: u64) -> String {
    format!("PT{}S", seconds(millis))
}

/// The instant `millis` milliseconds from the epoch names, in the ISO 8601 form a
/// playlist's wall-clock tag and a manifest's timestamp attribute carry.
fn reported_instant(millis: i64) -> String {
    to_date_time(crate::state::from_epoch_millis(millis))
        .fmt(::aws_smithy_types::date_time::Format::DateTime)
        .unwrap_or_default()
}

/// `value` with the characters an XML attribute cannot carry literally replaced by
/// their entities — the `&` of a query string above all.
///
/// The framework's own escaper, which owns this rule for every XML envelope this
/// emulator writes, rather than a second copy of the entity table.
fn escaped(value: &str) -> String {
    ::aws_smithy_schema_server::xml_escape(value)
}
