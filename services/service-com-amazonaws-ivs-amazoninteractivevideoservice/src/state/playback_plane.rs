// SPDX-License-Identifier: CC0-1.0

//! The PLAYBACK plane of this service: the HLS playlists and media segments a player
//! fetches from the `playbackUrl` every channel announces, packaged from the media the
//! contribute plane really ingested.
//!
//! RULE: a URL a service announces for a client to FETCH is served by this emulator at the
//! address it announced it on. `Channel$playbackUrl` is the "Channel playback URL" and
//! `Stream$playbackUrl` is the "URL of the master playlist, required by the video player to
//! play the HLS stream" (`smithy_query(query="shape",shape="Channel,Stream")`), so the
//! address [`super::announced_playback_url`] composes has to answer an HLS multivariant
//! playlist while the channel is broadcasting — and answer nothing before the first
//! segments of that broadcast are packaged, and nothing again once it has ended, because
//! there is no playlist for a stream that is not live.
//!
//! What it serves is MEASURED, never invented. A segment's bytes are the audio and video
//! messages the publishing encoder sent, in the FLV container they arrived in
//! ([`ChannelPlaylist::append`] — this emulator ingests and packages, it does not
//! transcode), its `#EXTINF` duration is the span of the timestamps those messages carried,
//! and the multivariant playlist's `BANDWIDTH` is the rate the packaged segments really
//! carry. A channel that nothing is publishing to has no playlist at all.
//!
//! Three paths make up one channel's playlist, all three under the one path layout AWS lays
//! a channel's manifest out at ([`super::playback_path`]) and all three answered from this
//! store:
//!
//! * the multivariant playlist the announced `playbackUrl` names;
//! * the media playlist it points at;
//! * one path per packaged segment.
//!
//! Nothing here connects anywhere: it answers requests that arrive at this emulator's own
//! endpoint, out of the bytes the contribute plane handed it.

use std::collections::VecDeque;

use super::channel_endpoints::{channel_of_arn, PLAYBACK_PATH_PREFIX};
use super::*;

/// The media type an HLS playlist is served as (RFC 8216 §4).
pub const PLAYLIST_MEDIA_TYPE: &str = "application/vnd.apple.mpegurl";

/// The media type a packaged segment is served as: the container the ingested media
/// arrived in and is served back in.
pub const SEGMENT_MEDIA_TYPE: &str = "video/x-flv";

/// How long one packaged segment aims to run.
///
/// The emulator's own choice — AWS publishes no segment duration for IVS playback — kept
/// short so a broadcast that has just started has a playlist to serve within a few seconds
/// of its first frame, which is what a player polling the announced URL waits for. A
/// segment is closed at the first keyframe at or after this, so the real durations are the
/// publisher's keyframe spacing rather than this number.
const SEGMENT_TARGET: Duration = Duration::from_secs(2);

/// How many packaged segments one channel keeps: the live window a player reads.
///
/// A bound rather than a history, because a broadcast runs for as long as the publisher
/// keeps sending and this store lives for the life of the process. Three is the window RFC
/// 8216 §6.3.3 expects a player to be able to start from. The oldest segment is dropped as
/// the next is packaged, and `#EXT-X-MEDIA-SEQUENCE` says where the window starts, which is
/// what RFC 8216 §6.2.2 describes for a live playlist.
const MAX_SEGMENTS: usize = 3;

/// The most media bytes one open segment accumulates before it is closed whatever the
/// timestamps say, so one publisher holds a bounded amount of this process's memory
/// however it encodes — at most this times [`MAX_SEGMENTS`] per broadcasting channel.
const MAX_SEGMENT_BYTES: usize = 1024 * 1024;

/// The FLV file header every packaged segment begins with: the signature `FLV`, version 1,
/// the audio and video flags, the 9-byte header length, and the first back-pointer — the
/// layout of the container the ingested messages are the tag bodies of.
const FLV_HEADER: [u8; 13] = [
    b'F', b'L', b'V', 1, 0x05, 0, 0, 0, 9, // signature, version, audio+video, header size
    0, 0, 0, 0, // the back-pointer before the first tag
];

/// The FLV tag type of an audio message.
const FLV_TAG_AUDIO: u8 = 8;

/// The FLV tag type of a video message.
const FLV_TAG_VIDEO: u8 = 9;

/// Which wire a message arrived on, which is the FLV tag type it is packaged as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    /// An audio message.
    Audio,
    /// A video message.
    Video,
}

impl MediaKind {
    /// The FLV tag type this message is packaged as.
    const fn tag_type(self) -> u8 {
        match self {
            Self::Audio => FLV_TAG_AUDIO,
            Self::Video => FLV_TAG_VIDEO,
        }
    }
}

/// One packaged segment of a broadcast.
#[derive(Debug, Clone)]
pub struct PackagedSegment {
    /// Its media sequence number, which is what the playlist and its path name it by.
    pub sequence: u64,
    /// How long it runs, as the span of the timestamps its messages carried.
    pub duration: Duration,
    /// The segment itself: an FLV file carrying the ingested messages.
    pub media: Vec<u8>,
}

/// The media one channel's CURRENT broadcast has been packaged into.
///
/// Held per channel for as long as that broadcast lasts ([`super::end_stream`] drops it),
/// so a playlist is never served for a stream that is not live.
#[derive(Debug)]
pub struct ChannelPlaylist {
    /// The broadcast these segments belong to, so a playlist is never read as a later
    /// broadcast's.
    stream_id: String,
    /// The segments in the live window, oldest first.
    segments: VecDeque<PackagedSegment>,
    /// The sequence number the next closed segment takes.
    next_sequence: u64,
    /// The messages accumulated since the last segment closed.
    open: Vec<u8>,
    /// How many bytes [`Self::open`] holds, as FLV tags.
    open_bytes: usize,
    /// The timestamp of the first message in the open segment, and of the last one.
    open_span: Option<(u32, u32)>,
    /// The decoder configuration the publisher sent for each wire, prepended to every
    /// closed segment so each one can be decoded on its own — which is what a player
    /// joining a live window mid-broadcast needs. One per wire: a publisher that sends its
    /// configuration again (a resolution change) replaces the one before it.
    video_header: Option<Vec<u8>>,
    /// The audio half of the same.
    audio_header: Option<Vec<u8>>,
    /// The encoder's stated resolution, reported on the multivariant playlist when it
    /// stated one.
    resolution: Option<(u32, u32)>,
}

impl ChannelPlaylist {
    /// A playlist for the broadcast `stream_id`, with nothing packaged yet.
    #[must_use]
    pub fn new(stream_id: &str) -> Self {
        Self {
            stream_id: stream_id.to_string(),
            segments: VecDeque::new(),
            next_sequence: 0,
            open: Vec::new(),
            open_bytes: 0,
            open_span: None,
            video_header: None,
            audio_header: None,
            resolution: None,
        }
    }

    /// The broadcast this playlist belongs to.
    #[must_use]
    pub fn stream_id(&self) -> &str {
        &self.stream_id
    }

    /// Report the resolution the publisher's own metadata stated, so the multivariant
    /// playlist can carry it.
    pub fn set_resolution(&mut self, width: Option<u32>, height: Option<u32>) {
        self.resolution = width.zip(height).filter(|(w, h)| *w > 0 && *h > 0);
    }

    /// Package one ingested message: append it to the open segment as an FLV tag, and
    /// close that segment when it has run long enough to be one.
    ///
    /// A decoder configuration message — the first message of each wire, which carries the
    /// publisher's codec setup rather than a frame — is kept aside instead, because every
    /// segment begins with it.
    pub fn append(&mut self, kind: MediaKind, timestamp: u32, payload: &[u8]) {
        // A message no segment of this plane could hold is packaged nowhere: the window's
        // bound is what keeps one publisher's memory footprint bounded, so a message past
        // it is dropped rather than making a segment of its own.
        if payload.is_empty() || payload.len() > MAX_SEGMENT_BYTES {
            return;
        }
        let tag = flv_tag(kind, timestamp, payload);
        if is_sequence_header(kind, payload) {
            match kind {
                MediaKind::Video => self.video_header = Some(tag),
                MediaKind::Audio => self.audio_header = Some(tag),
            }
            return;
        }
        let span = match self.open_span {
            Some((first, _last)) => {
                self.open_span = Some((first, timestamp));
                timestamp.saturating_sub(first)
            }
            None => {
                self.open_span = Some((timestamp, timestamp));
                0
            }
        };
        let long_enough = u128::from(span) >= SEGMENT_TARGET.as_millis();
        // A segment starts at a keyframe, so one is closed at the keyframe that follows
        // its target duration — and, whatever the publisher's keyframe spacing, once it
        // holds as much as one segment of this plane may hold.
        if (long_enough && is_keyframe(kind, payload) && !self.open.is_empty())
            || self.open_bytes + tag.len() > MAX_SEGMENT_BYTES
        {
            self.close_segment(timestamp);
            self.open_span = Some((timestamp, timestamp));
        }
        self.open_bytes += tag.len();
        self.open.extend_from_slice(&tag);
    }

    /// Close the open segment: package what has accumulated as one FLV file and put it in
    /// the live window, dropping the oldest segment when the window is full.
    fn close_segment(&mut self, closed_at: u32) {
        if self.open.is_empty() {
            return;
        }
        let (first, _last) = self.open_span.unwrap_or((closed_at, closed_at));
        let duration = Duration::from_millis(u64::from(closed_at.saturating_sub(first)));
        let mut media = Vec::with_capacity(FLV_HEADER.len() + self.open_bytes);
        media.extend_from_slice(&FLV_HEADER);
        for header in [self.video_header.as_ref(), self.audio_header.as_ref()]
            .into_iter()
            .flatten()
        {
            media.extend_from_slice(header);
        }
        media.append(&mut self.open);
        self.segments.push_back(PackagedSegment {
            sequence: self.next_sequence,
            duration,
            media,
        });
        self.next_sequence += 1;
        self.open_bytes = 0;
        while self.segments.len() > MAX_SEGMENTS {
            self.segments.pop_front();
        }
    }

    /// Whether any segment has been packaged yet: until one has, there is no playlist to
    /// serve and the announced URL answers nothing.
    #[must_use]
    pub fn has_segments(&self) -> bool {
        !self.segments.is_empty()
    }

    /// The segment `sequence` names, while it is still in the live window.
    #[must_use]
    pub fn segment(&self, sequence: u64) -> Option<&PackagedSegment> {
        self.segments
            .iter()
            .find(|segment| segment.sequence == sequence)
    }

    /// The rate the packaged segments really carry, in bits per second, which is what the
    /// multivariant playlist's `BANDWIDTH` states.
    ///
    /// Measured over the window: the bytes of its segments against the time they span. A
    /// window whose segments span no time at all yields none, and there is then no
    /// multivariant playlist to serve — `BANDWIDTH` is REQUIRED on `EXT-X-STREAM-INF`
    /// (RFC 8216 §4.3.4.2), and a playlist stating a rate nothing measured, or none at all,
    /// is one no player can start from.
    #[must_use]
    pub fn measured_bandwidth(&self) -> Option<u64> {
        let bytes: usize = self.segments.iter().map(|segment| segment.media.len()).sum();
        let millis: u128 = self
            .segments
            .iter()
            .map(|segment| segment.duration.as_millis())
            .sum();
        if millis == 0 {
            return None;
        }
        let bits = (bytes as u128).saturating_mul(8).saturating_mul(1_000);
        u64::try_from(bits / millis).ok()
    }

    /// The longest segment in the window, rounded up to whole seconds, which is what
    /// `#EXT-X-TARGETDURATION` must state (RFC 8216 §4.3.3.1).
    fn target_duration_secs(&self) -> u64 {
        let longest = self
            .segments
            .iter()
            .map(|segment| segment.duration.as_millis())
            .max()
            .unwrap_or_default();
        (longest.div_ceil(1_000) as u64).max(1)
    }
}

/// What a request on the playback path addresses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaybackTarget {
    /// The Region segment of the channel's ARN.
    pub region: String,
    /// The account segment.
    pub account: String,
    /// The channel's own id.
    pub channel_id: String,
    /// Which of the channel's three playback paths this is.
    pub kind: PlaybackKind,
}

/// Which playback path a request named.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackKind {
    /// The multivariant playlist the announced `playbackUrl` names.
    Multivariant,
    /// The media playlist it points at.
    Media,
    /// One packaged segment.
    Segment(u64),
}

/// The playback path a request addressed, or `None` for a path that is not one.
///
/// The layout is the one AWS's playback URLs carry and this service announces
/// ([`super::playback_path`]): the playlist prefix, then the channel's whole identity —
/// its Region, its account and its id, none of which carries a `.` — then what under that
/// channel is being fetched.
#[must_use]
pub fn parse_playback_path(path: &str) -> Option<PlaybackTarget> {
    let name = path.strip_prefix(PLAYBACK_PATH_PREFIX)?.strip_prefix('/')?;
    if name.contains('/') {
        return None;
    }
    let fields: Vec<&str> = name.split('.').collect();
    let [region, account, resource, channel_id, rest @ ..] = fields.as_slice() else {
        return None;
    };
    if *resource != CHANNEL_RESOURCE_TYPE
        || region.is_empty()
        || account.is_empty()
        || channel_id.is_empty()
    {
        return None;
    }
    let kind = match rest {
        ["m3u8"] => PlaybackKind::Multivariant,
        [MEDIA_PLAYLIST_LABEL, "m3u8"] => PlaybackKind::Media,
        [sequence, SEGMENT_EXTENSION] => PlaybackKind::Segment(sequence.parse().ok()?),
        _ => return None,
    };
    Some(PlaybackTarget {
        region: (*region).to_string(),
        account: (*account).to_string(),
        channel_id: (*channel_id).to_string(),
        kind,
    })
}

/// The label that distinguishes the media playlist's path from the multivariant playlist's.
const MEDIA_PLAYLIST_LABEL: &str = "media";

/// The extension a packaged segment's path carries: the container it is served in.
const SEGMENT_EXTENSION: &str = "flv";

/// The ARN of the channel `target` names, as this store holds it, or `None` when no channel
/// of this store has that identity.
///
/// Resolved by COMPARING the identity the path carries with the identity each stored
/// channel's own ARN carries ([`super::channel_of_arn`], the one reader of that form)
/// rather than by rebuilding an ARN string, so the path and the store cannot disagree
/// about how an ARN is spelled.
#[must_use]
pub fn playback_channel_arn(
    state: &ComAmazonawsIvsAmazoninteractivevideoserviceState,
    target: &PlaybackTarget,
) -> Option<String> {
    state
        .channels
        .keys()
        .find(|arn| {
            channel_of_arn(arn).is_some_and(|(region, account, id)| {
                region == target.region && account == target.account && id == target.channel_id
            })
        })
        .cloned()
}

/// What this plane answers one playback request with.
#[derive(Debug)]
pub enum PlaybackAnswer {
    /// The bytes to serve, and what they are.
    Served {
        /// The playlist or segment itself.
        content: Vec<u8>,
        /// Its media type.
        media_type: &'static str,
    },
    /// Nothing is served there: the channel is not broadcasting, the first segments of its
    /// broadcast are not packaged yet, or the segment asked for has left the live window.
    NotFound,
    /// A method a viewer does not read with.
    MethodRefused,
    /// The path names no channel THIS store holds, so this plane announced no such address
    /// and answers nothing about it — the request is left to whoever claims it.
    UnknownChannel,
}

/// The methods the playback plane serves: a viewer reads, it does not write.
pub const PLAYBACK_METHODS: &str = "GET, HEAD";

/// Serve one request on the playback path, out of what the contribute plane has packaged.
///
/// A READ of this store: it answers the playlists and segments the broadcast has produced
/// and writes nothing, so a player polling the announced URL cannot itself change what a
/// later caller — or the control plane — sees.
#[must_use]
pub fn serve_playback(
    state: &ComAmazonawsIvsAmazoninteractivevideoserviceState,
    target: &PlaybackTarget,
    method: &str,
) -> PlaybackAnswer {
    if method != "GET" && method != "HEAD" {
        return PlaybackAnswer::MethodRefused;
    }
    let Some(channel_arn) = playback_channel_arn(state, target) else {
        return PlaybackAnswer::UnknownChannel;
    };
    let Some(playlist) = state.playlists.get(&channel_arn) else {
        return PlaybackAnswer::NotFound;
    };
    if !playlist.has_segments() {
        return PlaybackAnswer::NotFound;
    }
    match target.kind {
        PlaybackKind::Multivariant => match playlist.measured_bandwidth() {
            Some(bandwidth) => PlaybackAnswer::Served {
                content: multivariant_playlist(target, playlist, bandwidth).into_bytes(),
                media_type: PLAYLIST_MEDIA_TYPE,
            },
            // No rate was measured over the window, so there is no multivariant playlist
            // to serve rather than a malformed one ([`ChannelPlaylist::measured_bandwidth`]).
            None => PlaybackAnswer::NotFound,
        },
        PlaybackKind::Media => PlaybackAnswer::Served {
            content: media_playlist(target, playlist).into_bytes(),
            media_type: PLAYLIST_MEDIA_TYPE,
        },
        PlaybackKind::Segment(sequence) => match playlist.segment(sequence) {
            Some(segment) => PlaybackAnswer::Served {
                content: segment.media.clone(),
                media_type: SEGMENT_MEDIA_TYPE,
            },
            None => PlaybackAnswer::NotFound,
        },
    }
}

/// The multivariant playlist of one broadcast: the one variant this plane packages, named
/// by a URI relative to the playlist's own, with the rate that variant really carries
/// (`bandwidth`, which RFC 8216 §4.3.4.2 requires) and the resolution its publisher stated.
fn multivariant_playlist(
    target: &PlaybackTarget,
    playlist: &ChannelPlaylist,
    bandwidth: u64,
) -> String {
    let mut attributes = vec![format!("BANDWIDTH={bandwidth}")];
    if let Some((width, height)) = playlist.resolution {
        attributes.push(format!("RESOLUTION={width}x{height}"));
    }
    let mut rendered = String::from("#EXTM3U\n#EXT-X-VERSION:3\n");
    rendered.push_str("#EXT-X-STREAM-INF:");
    rendered.push_str(&attributes.join(","));
    rendered.push('\n');
    rendered.push_str(&media_playlist_name(target));
    rendered.push('\n');
    rendered
}

/// The media playlist of one broadcast: the live window, each segment with the duration it
/// really runs for, and no end marker, because the broadcast has not ended.
fn media_playlist(target: &PlaybackTarget, playlist: &ChannelPlaylist) -> String {
    let first = playlist
        .segments
        .front()
        .map_or(0, |segment| segment.sequence);
    let mut rendered = format!(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:{}\n#EXT-X-MEDIA-SEQUENCE:{first}\n",
        playlist.target_duration_secs()
    );
    for segment in &playlist.segments {
        rendered.push_str(&format!(
            "#EXTINF:{:.3},\n{}\n",
            segment.duration.as_secs_f64(),
            segment_name(target, segment.sequence)
        ));
    }
    rendered
}

/// The channel's identity, as every one of its playback paths carries it.
fn channel_label(target: &PlaybackTarget) -> String {
    format!(
        "{}.{}.{CHANNEL_RESOURCE_TYPE}.{}",
        target.region, target.account, target.channel_id
    )
}

/// The media playlist's own file name, which is the URI the multivariant playlist names it
/// by: a RELATIVE one, so a player resolves it against whatever address it fetched the
/// multivariant playlist from.
fn media_playlist_name(target: &PlaybackTarget) -> String {
    format!("{}.{MEDIA_PLAYLIST_LABEL}.m3u8", channel_label(target))
}

/// One segment's file name, by the same rule.
fn segment_name(target: &PlaybackTarget, sequence: u64) -> String {
    format!("{}.{sequence}.{SEGMENT_EXTENSION}", channel_label(target))
}

/// One ingested message as an FLV tag: the tag type, the body's length, the timestamp the
/// message carried, the stream id every tag of a single-stream file carries, the body
/// itself, and the back-pointer the next tag is read against.
fn flv_tag(kind: MediaKind, timestamp: u32, payload: &[u8]) -> Vec<u8> {
    let size = u32::try_from(payload.len()).unwrap_or(u32::MAX);
    let mut tag = Vec::with_capacity(payload.len() + 15);
    tag.push(kind.tag_type());
    tag.extend_from_slice(&size.to_be_bytes()[1..]);
    tag.extend_from_slice(&timestamp.to_be_bytes()[1..]);
    tag.push(timestamp.to_be_bytes()[0]);
    tag.extend_from_slice(&[0, 0, 0]);
    tag.extend_from_slice(payload);
    let tag_size = u32::try_from(payload.len() + 11).unwrap_or(u32::MAX);
    tag.extend_from_slice(&tag_size.to_be_bytes());
    tag
}

/// Whether this message is the publisher's DECODER CONFIGURATION rather than a frame.
///
/// Both wires carry it the same way: the codec in the first byte's high nibble (video) or
/// low nibble (audio), and a packet type of 0 in the second byte for the configuration
/// record that precedes the frames (the FLV/AVC packaging an RTMP publisher sends).
fn is_sequence_header(kind: MediaKind, payload: &[u8]) -> bool {
    let [first, second, ..] = payload else {
        return false;
    };
    match kind {
        MediaKind::Video => *second == 0 && first & 0x0f >= CONFIGURED_VIDEO_CODEC,
        MediaKind::Audio => *second == 0 && first >> 4 == CONFIGURED_AUDIO_CODEC,
    }
}

/// The lowest FLV video codec id whose packets carry a configuration record and a packet
/// type (AVC and the codecs after it).
const CONFIGURED_VIDEO_CODEC: u8 = 7;

/// The FLV audio codec id whose packets carry a configuration record and a packet type
/// (AAC).
const CONFIGURED_AUDIO_CODEC: u8 = 10;

/// Whether this message is a video KEYFRAME, which is where a segment may begin: the frame
/// type is the high nibble of an FLV video packet's first byte, and 1 is a keyframe.
fn is_keyframe(kind: MediaKind, payload: &[u8]) -> bool {
    kind == MediaKind::Video && payload.first().is_some_and(|first| first >> 4 == 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One row of the path-reading table: the case, the path, and the target it names (or
    /// none) as the Region, the account, the channel id and which playlist.
    type PathRow = (
        &'static str,
        &'static str,
        Option<(&'static str, &'static str, &'static str, PlaybackKind)>,
    );

    /// RULE: a playback path is read as the channel identity AWS lays a manifest out
    /// under, plus which of that channel's playlists or segments is being fetched; anything
    /// else is no playback path at all.
    #[test]
    fn a_playback_path_names_a_channel_and_one_of_its_playlists() {
        // The rows span what the reading generalises over: the three paths one channel
        // serves, the Region and id shapes an ARN admits, and the paths that are none of
        // them (another prefix, a missing field, a resource that is not a channel, a
        // sequence that is not a number, a nested path).
        let rows: &[PathRow] = &[
            (
                "the multivariant playlist the channel announces",
                "/api/video/v1/us-west-2.000000000000.channel.96b292210807.m3u8",
                Some((
                    "us-west-2",
                    "000000000000",
                    "96b292210807",
                    PlaybackKind::Multivariant,
                )),
            ),
            (
                "its media playlist",
                "/api/video/v1/us-west-2.000000000000.channel.96b292210807.media.m3u8",
                Some((
                    "us-west-2",
                    "000000000000",
                    "96b292210807",
                    PlaybackKind::Media,
                )),
            ),
            (
                "one of its segments",
                "/api/video/v1/a.1.channel.A-9.12.flv",
                Some(("a", "1", "A-9", PlaybackKind::Segment(12))),
            ),
            (
                "the first segment of a broadcast",
                "/api/video/v1/ap-southeast-2.123456789012.channel.c-1.0.flv",
                Some((
                    "ap-southeast-2",
                    "123456789012",
                    "c-1",
                    PlaybackKind::Segment(0),
                )),
            ),
            ("another prefix", "/video/v1/a.1.channel.c.m3u8", None),
            ("no channel id", "/api/video/v1/a.1.channel.m3u8", None),
            (
                "another resource type",
                "/api/video/v1/a.1.stream-key.c.m3u8",
                None,
            ),
            (
                "a sequence that is not a number",
                "/api/video/v1/a.1.channel.c.latest.flv",
                None,
            ),
            (
                "a nested path",
                "/api/video/v1/a.1.channel.c/segment.flv",
                None,
            ),
            ("the prefix alone", "/api/video/v1", None),
        ];
        for (case, path, expected) in rows {
            let parsed = parse_playback_path(path);
            let wanted = expected.map(|(region, account, channel_id, kind)| PlaybackTarget {
                region: region.to_string(),
                account: account.to_string(),
                channel_id: channel_id.to_string(),
                kind,
            });
            assert_eq!(
                parsed, wanted,
                "case {case:?}: expected {wanted:?}, got {parsed:?}"
            );
        }
    }

    /// RULE: a segment is closed at the first keyframe at or after the target duration, and
    /// what it carries is the ingested messages in an FLV file that begins with the
    /// publisher's own decoder configuration.
    #[test]
    fn packaging_closes_a_segment_on_the_keyframe_after_its_target_duration() {
        let mut playlist = ChannelPlaylist::new("st-00000000000000000000000");
        // The publisher's configuration records, which are not frames.
        playlist.append(MediaKind::Video, 0, &[0x17, 0x00, 0x01, 0x02]);
        playlist.append(MediaKind::Audio, 0, &[0xaf, 0x00, 0x12, 0x10]);
        assert!(
            !playlist.has_segments(),
            "a configuration record is no segment: got {playlist:?}"
        );
        // Frames up to the target duration: still one open segment.
        playlist.append(MediaKind::Video, 0, &[0x17, 0x01, 0x00, 0x00, 0x00]);
        playlist.append(MediaKind::Audio, 500, &[0xaf, 0x01, 0x21]);
        playlist.append(MediaKind::Video, 1_000, &[0x27, 0x01, 0x00, 0x00, 0x00]);
        assert!(
            !playlist.has_segments(),
            "no segment is closed before its target duration: got {playlist:?}"
        );
        // The keyframe at the target duration closes it.
        playlist.append(MediaKind::Video, 2_000, &[0x17, 0x01, 0x00, 0x00, 0x00]);
        let segment = playlist
            .segment(0)
            .unwrap_or_else(|| panic!("the first segment is packaged: {playlist:?}"));
        assert_eq!(
            segment.duration,
            Duration::from_secs(2),
            "expected the span of the packaged timestamps, got {:?}",
            segment.duration
        );
        assert!(
            segment.media.starts_with(&FLV_HEADER),
            "expected a packaged segment to be an FLV file, got {:?}",
            &segment.media[..segment.media.len().min(16)]
        );
        assert!(
            segment.media.windows(4).any(|window| window == [0x17, 0x00, 0x01, 0x02]),
            "expected the publisher's video configuration in every segment, got {:?}",
            segment.media
        );
        assert!(
            playlist.measured_bandwidth().is_some_and(|rate| rate > 0),
            "expected a measured bandwidth over the packaged window, got {:?}",
            playlist.measured_bandwidth()
        );
    }

    /// RULE: the live window holds at most [`MAX_SEGMENTS`] segments, and the media
    /// playlist states where that window starts.
    #[test]
    fn the_live_window_is_bounded_and_states_where_it_starts() {
        let mut playlist = ChannelPlaylist::new("st-00000000000000000000000");
        for second in 0..(MAX_SEGMENTS as u32 + 3) {
            playlist.append(
                MediaKind::Video,
                second * 2_000,
                &[0x17, 0x01, 0x00, 0x00, 0x00],
            );
            playlist.append(MediaKind::Audio, second * 2_000 + 500, &[0xaf, 0x01, 0x21]);
        }
        assert_eq!(
            playlist.segments.len(),
            MAX_SEGMENTS,
            "expected the window to be bounded at {MAX_SEGMENTS}, got {}",
            playlist.segments.len()
        );
        let target = PlaybackTarget {
            region: "us-west-2".to_string(),
            account: "000000000000".to_string(),
            channel_id: "abc".to_string(),
            kind: PlaybackKind::Media,
        };
        let rendered = media_playlist(&target, &playlist);
        let first = playlist
            .segments
            .front()
            .map_or(0, |segment| segment.sequence);
        assert!(
            rendered.contains(&format!("#EXT-X-MEDIA-SEQUENCE:{first}")),
            "expected the window's first sequence in the playlist, got {rendered:?}"
        );
        assert!(
            rendered.starts_with("#EXTM3U\n"),
            "expected an HLS playlist, got {rendered:?}"
        );
    }
}
