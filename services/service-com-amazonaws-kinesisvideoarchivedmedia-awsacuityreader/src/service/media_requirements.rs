// SPDX-License-Identifier: CC0-1.0

//! The requirements this plane puts on the media it reads, and the modelled
//! refusals they raise.
//!
//! "An Amazon Kinesis video stream has the following requirements for providing
//! data through HLS": the media must carry H.264 encoded video, so "the codec ID
//! of track 1 should be V_MPEG/ISO/AVC", and "the video track of each fragment
//! must contain codec private data in the Advanced Video Coding (AVC) for H.264
//! format". Three modelled errors state exactly those requirements —
//! `UnsupportedStreamMediaTypeException`, `MissingCodecPrivateDataException` and
//! `InvalidCodecPrivateDataException` — and EVERY operation of this plane that
//! has to read inside a fragment declares all three: the clip `GetClip` muxes
//! and the two streaming sessions `GetHLSStreamingSessionURL` and
//! `GetDASHStreamingSessionURL` mint.
//!
//! So the classification of archived media against those requirements, and the
//! wording each refusal carries, live HERE rather than in one of them: three
//! operations answering for one requirement out of three copies of the rule are
//! three chances to classify the same archive differently.
//!
//! The requirement this plane implements is the H.264 one. The documentation
//! offers the producer a choice of two codecs — H.264 or H.265 — and the whole
//! read path of this plane reads the first of them: the codec ids it accepts
//! ([`crate::service::archived_container::AVC_CODEC_IDS`]), the decoder
//! configuration record it reads a sample description from, and the track every
//! answer it assembles writes are AVC's. Media that declares the H.265 codec id
//! instead carries an HEVC record no reader here can write a track from, so it is
//! media whose type this plane cannot determine — the refusal above — rather than
//! media it serves with an unreadable initialization fragment. Admitting H.265
//! means an HEVC record reader and an HEVC track writer beside the AVC ones, in
//! `archived_container` and `service_common::h264_mp4`, and it changes every
//! answer of this plane rather than this classification alone.

use crate::service::archived_container::ContainerFault;
use crate::service::playback_media::fragment_track;

/// The sentence a request over media whose codec ids name no H.264 video track
/// is refused with: the modelled description of
/// `UnsupportedStreamMediaTypeException`. Entirely fixed text — the error states
/// what could not be determined and from what, naming no stream and no track.
pub(crate) const UNSUPPORTED_MEDIA_MESSAGE: &str =
    "The type of the media (for example, h.264 or h.265 video or ACC or G.711 audio) could not \
     be determined from the codec IDs of the tracks in the first fragment for a playback session.";

/// Which requirement the archived media breaks, and the track it breaks it on.
///
/// One of the three refusals the operations of this plane model for media they
/// cannot read as the H.264 the requirements ask for; the caller maps it onto its
/// own modelled error channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MediaRefusal {
    /// No track of the media declares the H.264 video this plane reads, so the
    /// type of the media could not be determined from its codec ids.
    UnsupportedMediaType,
    /// The video track declares no codec private data, carrying the track number
    /// it is missing for.
    MissingCodecPrivateData(u64),
    /// Its codec private data is not an AVC decoder configuration record, again
    /// carrying the offending track's number.
    InvalidCodecPrivateData(u64),
}

impl MediaRefusal {
    /// The message this refusal is reported with: the modelled sentence for its
    /// error class, with the track number read from the media that broke the
    /// requirement.
    pub(crate) fn message(&self) -> String {
        match self {
            Self::UnsupportedMediaType => UNSUPPORTED_MEDIA_MESSAGE.to_string(),
            Self::MissingCodecPrivateData(track) => {
                format!("Missing codec private data in fragment for track {track}.")
            }
            Self::InvalidCodecPrivateData(track) => {
                format!("Invalid codec private data in fragment for track {track}.")
            }
        }
    }
}

/// The requirement `fault` breaks, or `None` when it breaks none of them — media
/// that stands in the way of the ANSWER rather than of the media type, which is
/// the asking operation's own vocabulary.
pub(crate) fn refusal(fault: &ContainerFault) -> Option<MediaRefusal> {
    match fault {
        ContainerFault::UnsupportedMedia => Some(MediaRefusal::UnsupportedMediaType),
        ContainerFault::MissingCodecPrivateData(track) => {
            Some(MediaRefusal::MissingCodecPrivateData(*track))
        }
        ContainerFault::InvalidCodecPrivateData(track) => {
            Some(MediaRefusal::InvalidCodecPrivateData(*track))
        }
        ContainerFault::Unreadable | ContainerFault::NoFrames | ContainerFault::Unwritable(_) => {
            None
        }
    }
}

/// The requirement the media a streaming session would play breaks, or `None`
/// when a session over it can be played.
///
/// Read on the FIRST fragment the session plays, which is where the requirements
/// are determined: the media type comes "from the codec IDs of the tracks in the
/// first fragment for a playback session", and the codec private data of that
/// fragment's video track is what the session's initialization fragment carries
/// for a player to set its decoder up from.
pub(crate) fn session_media_refusal(media: &[u8]) -> Option<MediaRefusal> {
    let fault = fragment_track(media).err()?;
    // Media whose container does not read at all declares no codec id either, so
    // the type of a session over it could not be determined from codec ids —
    // which is the one refusal of the three a streaming session can report for
    // media it cannot read as media.
    Some(refusal(&fault).unwrap_or(MediaRefusal::UnsupportedMediaType))
}
