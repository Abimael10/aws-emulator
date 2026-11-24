// SPDX-License-Identifier: CC0-1.0

//! Reading the container one archived fragment IS, and writing its frames into
//! the MP4 a reader of this service hands a media player.
//!
//! Kinesis Video Streams archives every fragment as a Matroska document of its
//! own (`GetMediaForFragmentList` serves those bytes back as `video/webm`), so
//! every operation of this plane that has to look INSIDE a fragment — the clip
//! `GetClip` muxes, and the MP4 and MPEG TS fragments a streaming session's
//! playback plane serves ([`crate::service::playback_session`]) — reads the same
//! tree: the track declarations of the fragment's header and the frames of its
//! Cluster.
//!
//! That read lives HERE, beside neither of them, because two readers of one
//! container are two chances to disagree about what a producer ingested. The
//! container is read by the maintained Matroska reader and its codec private
//! data by the maintained H.264 reader; what this module adds is the Kinesis
//! Video Streams SHAPE of a fragment and the bounds on the work reading one may
//! ask for.

use ::std::io::Cursor;

use ::service_common::h264_mp4::{self, Sample, VideoTrack};
use ::webm_iterable::matroska_spec::{Master, MatroskaSpec};
use ::webm_iterable::WebmIterator;

/// The Matroska codec ids that name the H.264 video this plane reads.
///
/// `V_MPEG4/ISO/AVC` is the id the Matroska codec registry assigns to H.264 and
/// the one the Kinesis Video Streams producers write; `V_MPEG/ISO/AVC` is how
/// the archived-media documentation prints it ("the codec ID of track 1 should
/// be V_MPEG/ISO/AVC (for h.264)"), so both spellings name the same codec here.
pub(crate) const AVC_CODEC_IDS: &[&str] = &["V_MPEG4/ISO/AVC", "V_MPEG/ISO/AVC"];

/// How many nanoseconds one Matroska timecode tick is worth when a fragment's
/// container declares no `TimestampScale`: the Matroska default of one
/// millisecond.
const DEFAULT_TIMECODE_SCALE_NANOS: u64 = 1_000_000;

/// Nanoseconds in one millisecond, the unit this service records fragment
/// timestamps and durations in.
const NANOS_PER_MILLI: i128 = 1_000_000;

/// How many units of time one second of written media holds: the millisecond,
/// which is the precision the archive stamps its fragments and their durations
/// at, so a sample's time needs no rounding to be written.
pub(crate) const MEDIA_TIMESCALE: u32 = 1_000;

/// The most EBML elements one archived fragment is read for: the step budget of
/// that read, so the work one request can ask for is bounded whatever shape the
/// stored tree has. One fragment is a header and one Cluster of frames, so no
/// media an ingest plane wrote reaches it.
const MAX_ELEMENTS_PER_FRAGMENT: usize = 100_000;

/// How many bytes of a sequence parameter set a sample description reads: the
/// set's own unit header, then the profile, the profile compatibility and the
/// level the coded video declares (ISO/IEC 14496-10).
pub(crate) const SEQUENCE_PARAMETER_SET_HEADER: usize = 4;

/// What archived media could not be read or written as, for the caller to
/// report in its own vocabulary — the modelled refusals of `GetClip`, a status
/// on the playback plane.
#[derive(Debug)]
pub(crate) enum ContainerFault {
    /// The archived bytes do not read as a Matroska document, so the frames
    /// cannot be found in them.
    Unreadable,
    /// No track of the fragment declares the H.264 video this plane reads.
    UnsupportedMedia,
    /// The video track declares no codec private data, so there is no sample
    /// description to carry. The track number is the offending track's.
    MissingCodecPrivateData(u64),
    /// Its codec private data is not an AVC decoder configuration record the
    /// sample description can be written from.
    InvalidCodecPrivateData(u64),
    /// The read fragments carry no frame to put in the written media.
    NoFrames,
    /// The writer refused the container, carrying what it refused.
    Unwritable(String),
}

/// One track as a fragment's `Tracks` declares it.
#[derive(Debug, Default)]
pub(crate) struct TrackDeclaration {
    /// Its `TrackNumber` — the number a frame's block header names, and the one
    /// a codec-private-data refusal reports.
    pub(crate) number: u64,
    /// Its `CodecID`.
    pub(crate) codec_id: String,
    /// Its `CodecPrivate`, when the producer declared one.
    pub(crate) codec_private: Option<Vec<u8>>,
    /// Its `PixelWidth`, when the container declares the track's geometry.
    pub(crate) pixel_width: Option<u64>,
    /// Its `PixelHeight`.
    pub(crate) pixel_height: Option<u64>,
}

impl TrackDeclaration {
    /// Whether this track's codec id names the H.264 video this plane reads.
    pub(crate) fn is_avc(&self) -> bool {
        AVC_CODEC_IDS.contains(&self.codec_id.as_str())
    }
}

/// One frame a fragment carries on its video track.
#[derive(Debug)]
pub(crate) struct ArchivedFrame {
    /// When the frame is shown, in milliseconds from the start of its own
    /// fragment.
    pub(crate) offset_millis: i64,
    /// The frame's own bytes, as the block carried them.
    pub(crate) bytes: Vec<u8>,
    /// Whether a player may start decoding at this frame, as the block's header
    /// says.
    pub(crate) sync: bool,
}

/// What one archived fragment carries: the video track it declares and that
/// track's frames.
#[derive(Debug)]
pub(crate) struct ArchivedFragment {
    pub(crate) track: TrackDeclaration,
    pub(crate) frames: Vec<ArchivedFrame>,
}

/// The video track written media declares, from the track a fragment declared
/// and the record it carries.
///
/// The geometry is the coded video's own, read from the sequence parameter set of
/// that record; a record whose parameter set does not read falls back to the
/// geometry the CONTAINER declares for the track (`PixelWidth`/`PixelHeight`),
/// and codec private data that states neither is not data media can be written
/// from.
pub(crate) fn video_track(
    declared: &TrackDeclaration,
    record: &AvcRecord,
) -> Result<VideoTrack, ContainerFault> {
    let declared_dimensions = match (declared.pixel_width, declared.pixel_height) {
        (Some(width), Some(height)) if width > 0 && height > 0 => Some((width, height)),
        _ => None,
    };
    let dimensions = coded_dimensions(&record.seq_param_set).or(declared_dimensions);
    let Some((width, height)) = dimensions else {
        return Err(ContainerFault::InvalidCodecPrivateData(declared.number));
    };
    Ok(VideoTrack {
        width: u16::try_from(width).unwrap_or(u16::MAX),
        height: u16::try_from(height).unwrap_or(u16::MAX),
        timescale: MEDIA_TIMESCALE,
        seq_param_set: record.seq_param_set.clone(),
        pic_param_set: record.pic_param_set.clone(),
    })
}

/// The coded geometry a sequence parameter set states, read by the maintained
/// H.264 bitstream reader, or `None` when those bytes are not one.
fn coded_dimensions(seq_param_set: &[u8]) -> Option<(u64, u64)> {
    use ::h264_reader::nal::{sps::SeqParameterSet, Nal, RefNal};

    let nal = RefNal::new(seq_param_set, &[], true);
    let sps = SeqParameterSet::from_bits(nal.rbsp_bits()).ok()?;
    let (width, height) = sps.pixel_dimensions().ok()?;
    (width > 0 && height > 0).then_some((u64::from(width), u64::from(height)))
}

/// The AVC decoder configuration record a track's codec private data is, or the
/// fault that stands in its way.
pub(crate) fn read_codec_private(
    declared: &TrackDeclaration,
) -> Result<AvcRecord, ContainerFault> {
    // "The video track of each fragment must contain codec private data in the
    // Advanced Video Coding (AVC) for H.264 format": a track that declares none
    // is the modelled missing-record refusal, naming the track it is missing for.
    let private = declared
        .codec_private
        .as_deref()
        .filter(|bytes| !bytes.is_empty())
        .ok_or(ContainerFault::MissingCodecPrivateData(declared.number))?;
    AvcRecord::read(private).ok_or(ContainerFault::InvalidCodecPrivateData(declared.number))
}

/// The AVC decoder configuration record (ISO/IEC 14496-15) a written sample
/// description comes from.
#[derive(Debug)]
pub(crate) struct AvcRecord {
    /// The record's first sequence parameter set.
    pub(crate) seq_param_set: Vec<u8>,
    /// Its first picture parameter set.
    pub(crate) pic_param_set: Vec<u8>,
    /// How wide the length field framing one unit of a sample is, in bytes:
    /// `lengthSizeMinusOne` plus one.
    pub(crate) length_size: usize,
}

impl AvcRecord {
    /// `bytes` read as the record, or `None` when they are not one.
    ///
    /// The record is read by the maintained H.264 reader, which owns the layout
    /// ISO/IEC 14496-15 fixes — the configuration version, the profile, the
    /// length framing of a sample's units and the length-prefixed parameter sets.
    /// Codec private data it cannot read, or that carries neither kind of
    /// parameter set, is not data a sample description can be written from.
    fn read(bytes: &[u8]) -> Option<Self> {
        use ::h264_reader::avcc::AvcDecoderConfigurationRecord;

        let record = AvcDecoderConfigurationRecord::try_from(bytes).ok()?;
        let seq_param_set = parameter_set_bytes(record.sequence_parameter_sets().next()?.ok()?)?;
        let pic_param_set = parameter_set_bytes(record.picture_parameter_sets().next()?.ok()?)?;
        Some(Self {
            seq_param_set,
            pic_param_set,
            length_size: usize::from(record.length_size_minus_one()) + 1,
        })
    }
}

/// One parameter set of the record as the bytes a sample description carries, or
/// `None` when it states no profile for the written media to declare.
///
/// A sequence parameter set shorter than [`SEQUENCE_PARAMETER_SET_HEADER`] is not
/// one a sample description can be written from: the description states the
/// profile, the profile compatibility and the level of the coded video, and those
/// are the three bytes that follow the set's own unit header (ISO/IEC 14496-10).
fn parameter_set_bytes(set: &[u8]) -> Option<Vec<u8>> {
    (set.len() >= SEQUENCE_PARAMETER_SET_HEADER).then(|| set.to_vec())
}

/// One archived frame in the framing an MP4 track carries its samples in.
///
/// The frames a Kinesis Video Streams producer writes are already the
/// length-prefixed units of the track's record, so they are carried through as
/// they are. A producer that wrote its units DELIMITED instead (the Annex B byte
/// stream, which is what the NAL adaptation flags adapt) is re-framed, and so is
/// one whose record states a length field of another width, so that what the
/// written media declares and what its samples carry agree.
pub(crate) fn avcc_sample(frame: &ArchivedFrame, length_size: usize) -> Sample {
    let delimited = frame.bytes.starts_with(&[0, 0, 1]) || frame.bytes.starts_with(&[0, 0, 0, 1]);
    let mut stream = Vec::new();
    let reframed = if delimited {
        stream.extend_from_slice(&frame.bytes);
        true
    } else if length_size == 4 {
        false
    } else {
        h264_mp4::annex_b_from_avcc(&frame.bytes, length_size, &mut stream)
    };
    if !reframed {
        return Sample {
            bytes: frame.bytes.clone(),
            sync: frame.sync,
        };
    }
    let units = h264_mp4::annex_b_units(&stream);
    let framed = h264_mp4::avcc_samples(&units);
    if framed.is_empty() {
        return Sample {
            bytes: frame.bytes.clone(),
            sync: frame.sync,
        };
    }
    // One block carries one coded picture, so what the re-framing read out of it
    // stays one sample.
    Sample {
        sync: frame.sync || framed.iter().any(|sample| sample.sync),
        bytes: framed
            .into_iter()
            .flat_map(|sample| sample.bytes)
            .collect::<Vec<u8>>(),
    }
}

/// The video track and frames one archived fragment carries.
///
/// The container is read by the maintained Matroska reader: the element tree, its
/// variable-size integers and a block's own header are that crate's business. What
/// this reads out of the tree is the Kinesis Video Streams shape of a fragment —
/// the track declarations of its header and the frames of its Cluster.
pub(crate) fn read_fragment(media: &[u8]) -> Result<ArchivedFragment, ContainerFault> {
    let mut scale_nanos = DEFAULT_TIMECODE_SCALE_NANOS;
    let mut declared: Vec<TrackDeclaration> = Vec::new();
    let mut open: Option<TrackDeclaration> = None;
    let mut cluster_millis = 0i64;
    // (the track the block is on, when it is shown in milliseconds from the start
    // of the fragment, its bytes, whether it is a sync frame)
    let mut blocks: Vec<(u64, i64, Vec<u8>, bool)> = Vec::new();
    let mut elements = 0usize;

    for read in WebmIterator::new(Cursor::new(media), &[]) {
        elements = elements.saturating_add(1);
        if elements > MAX_ELEMENTS_PER_FRAGMENT {
            break;
        }
        let tag = read.map_err(|_| ContainerFault::Unreadable)?;
        match &tag {
            MatroskaSpec::TimestampScale(nanos) => scale_nanos = (*nanos).max(1),
            MatroskaSpec::TrackEntry(Master::Start) => open = Some(TrackDeclaration::default()),
            MatroskaSpec::TrackEntry(Master::End) => {
                if let Some(track) = open.take() {
                    declared.push(track);
                }
            }
            MatroskaSpec::TrackNumber(number) => {
                if let Some(track) = open.as_mut() {
                    track.number = *number;
                }
            }
            MatroskaSpec::CodecID(codec_id) => {
                if let Some(track) = open.as_mut() {
                    track.codec_id = codec_id.clone();
                }
            }
            MatroskaSpec::CodecPrivate(private) => {
                if let Some(track) = open.as_mut() {
                    track.codec_private = Some(private.clone());
                }
            }
            MatroskaSpec::PixelWidth(width) => {
                if let Some(track) = open.as_mut() {
                    track.pixel_width = Some(*width);
                }
            }
            MatroskaSpec::PixelHeight(height) => {
                if let Some(track) = open.as_mut() {
                    track.pixel_height = Some(*height);
                }
            }
            MatroskaSpec::Timestamp(ticks) => {
                cluster_millis = ticks_as_millis(i128::from(*ticks), scale_nanos);
            }
            MatroskaSpec::SimpleBlock(_) => {
                let block = ::webm_iterable::matroska_spec::SimpleBlock::try_from(&tag)
                    .map_err(|_| ContainerFault::Unreadable)?;
                blocks.push((
                    block.track,
                    cluster_millis
                        .saturating_add(ticks_as_millis(i128::from(block.timestamp), scale_nanos)),
                    block.raw_frame_data().to_vec(),
                    block.keyframe,
                ));
            }
            MatroskaSpec::Block(_) => {
                let block = ::webm_iterable::matroska_spec::Block::try_from(&tag)
                    .map_err(|_| ContainerFault::Unreadable)?;
                blocks.push((
                    block.track,
                    cluster_millis
                        .saturating_add(ticks_as_millis(i128::from(block.timestamp), scale_nanos)),
                    block.raw_frame_data().to_vec(),
                    false,
                ));
            }
            _ => {}
        }
    }

    // "the codec ID of track 1 should be V_MPEG/ISO/AVC (for h.264)": the media
    // is read from the track whose codec id names that codec, and media whose
    // declarations name no such track is media whose type this plane cannot
    // determine.
    let track = declared
        .into_iter()
        .find(TrackDeclaration::is_avc)
        .ok_or(ContainerFault::UnsupportedMedia)?;
    // A fragment's frames are shown from the fragment's own start, so the offsets
    // are measured from its first frame rather than from the producer's clock
    // origin — two fragments ingested from one session then sit on the written
    // timeline where the archive's own timestamps put them.
    let first_millis = blocks
        .iter()
        .filter(|(on, _, _, _)| *on == track.number)
        .map(|(_, at, _, _)| *at)
        .min()
        .unwrap_or(0);
    let frames = blocks
        .into_iter()
        .filter(|(on, _, _, _)| *on == track.number)
        .map(|(_, at, bytes, sync)| ArchivedFrame {
            offset_millis: at.saturating_sub(first_millis),
            bytes,
            sync,
        })
        .collect();
    Ok(ArchivedFragment { track, frames })
}

/// `ticks` of a container's timecode scale, in milliseconds.
///
/// `TimestampScale` is "the number of nanoseconds per tick" (Matroska), so a tick
/// is worth `scale_nanos` nanoseconds and the conversion is exact at the
/// millisecond precision this service records.
fn ticks_as_millis(ticks: i128, scale_nanos: u64) -> i64 {
    let nanos = ticks.saturating_mul(i128::from(scale_nanos));
    i64::try_from(nanos / NANOS_PER_MILLI).unwrap_or(i64::MAX)
}
