// SPDX-License-Identifier: CC0-1.0

//! The MEDIA a streaming session's playback plane serves: the MP4 initialization
//! fragment, one MP4 media fragment per archived fragment, and the MPEG TS
//! fragment an `MPEG_TS` session serves instead of those two.
//!
//! Everything here is written FROM the archived container
//! ([`crate::service::archived_container`]): the frames a producer ingested, the
//! parameter sets its track declared, and the duration the archive recorded for
//! the fragment. Nothing is transcoded and nothing is synthesised — a fragment
//! whose container cannot be read, or that declares no H.264 video track, serves
//! no media at all, which is the fault the caller reports.
//!
//! The three resources are what the archived-media documentation says a player
//! fetches:
//!
//! * `GetMP4InitFragment` "contains only the codec private data for the stream
//!   and respective track, which the media player needs to decode the media
//!   frames", so it is the track's own declaration with no media in it;
//! * `GetMP4MediaFragment` carries "the encoded fragment's media frames and their
//!   timestamps", so it is one archived fragment's frames, on the timeline the
//!   fragment's own duration gives them;
//! * `GetTSFragment` carries "both initialization and media data", so the
//!   transport stream repeats the parameter sets ahead of every coded picture and
//!   is served for a session whose `ContainerFormat` is `MPEG_TS`, "instead of
//!   GetMP4InitFragment and GetMP4MediaFragment".

use ::service_common::h264_mp4::{self, TimedSample, VideoTrack};

use crate::service::archived_container::{
    avcc_sample, read_codec_private, read_fragment, video_track, ArchivedFragment, AvcRecord,
    ContainerFault,
};

/// The media type an MP4 initialization or media fragment is served as — the type
/// `GetClip` reports for the MP4 it builds from the same archive.
pub(crate) const MP4_MEDIA_TYPE: &str = "video/mp4";

/// The media type an MPEG TS fragment is served as: the type registered for an
/// MPEG-2 transport stream (RFC 3555 §4.2.11).
pub(crate) const TS_MEDIA_TYPE: &str = "video/MP2T";

/// How long the LAST sample of a fragment lasts when the fragment's own duration
/// ends at or before that sample's instant: one tick of the written timescale
/// ([`crate::service::archived_container::MEDIA_TIMESCALE`]), the shortest span a
/// sample can be shown for.
const MINIMUM_SAMPLE_TICKS: u32 = 1;

/// The most frames of one archived fragment a served media answer carries.
///
/// A ceiling on the WORK one playback request can ask for, beside the byte ceiling
/// below: a fragment is seconds of video, so no fragment a producer wrote reaches
/// it, and a container crafted to declare a hundred thousand tiny blocks
/// (the element budget [`crate::service::archived_container`] reads one under)
/// cannot multiply the per-picture parameter sets of the transport stream by that
/// many. It is the per-fragment analogue of the clip's own sample ceiling.
const MAX_SERVED_FRAMES: usize = 10_000;

/// The ceiling on the bytes ONE served media answer assembles.
///
/// The smaller of the two ceilings this service already publishes: the bytes one
/// archived-media response may assemble ([`crate::state::MAX_PAYLOAD_BYTES`],
/// which `GetClip` enforces for the clip it builds from the same archive) and the
/// media one stream's WHOLE archive may hold
/// ([`crate::state::MAX_ARCHIVED_MEDIA_BYTES_PER_STREAM`]) — one fragment of that
/// archive cannot honestly assemble more than the archive itself can hold.
const MAX_SERVED_BYTES: usize =
    if crate::state::MAX_ARCHIVED_MEDIA_BYTES_PER_STREAM < crate::state::MAX_PAYLOAD_BYTES {
        crate::state::MAX_ARCHIVED_MEDIA_BYTES_PER_STREAM
    } else {
        crate::state::MAX_PAYLOAD_BYTES
    };

/// The clock a transport stream states its presentation timestamps in: 90 kHz
/// (ISO/IEC 13818-1).
const TS_CLOCK_HZ: i64 = 90_000;

/// The transport-stream id the written stream carries. One program, one stream, so
/// the id distinguishes nothing and is the first.
const TS_TRANSPORT_STREAM_ID: u16 = 1;

/// The program number of the one program the written stream carries.
const TS_PROGRAM_NUMBER: u16 = 1;

/// The packet id the program map table is carried on. Not fixed by the standard —
/// it is announced in the program association table, which is — and this is the
/// value the reference multiplexers use.
const TS_PMT_PID: u16 = 0x1000;

/// The packet id the video elementary stream is carried on, announced in the
/// program map table.
const TS_VIDEO_PID: u16 = 0x0100;

/// The stream id of an MPEG video elementary stream in a PES header (ISO/IEC
/// 13818-1 table 2-22: `1110 xxxx` is "MPEG video stream number xxxx").
const TS_VIDEO_STREAM_ID: u8 = 0xE0;

/// How many bytes of payload one transport packet carries when it carries nothing
/// but payload: the 188-byte packet less its 4-byte header.
const TS_PAYLOAD_BYTES: usize = 184;

/// The `nal_unit_type` of FILLER DATA (ISO/IEC 14496-10 table 7-1), the unit an
/// elementary stream is padded with: it carries no coded picture, so a decoder
/// skips it, which is what makes it the right padding for a packet that would
/// otherwise be part full.
const FILLER_DATA_UNIT: u8 = 0x0C;

/// The shortest filler unit that can be written: the four-byte start code and the
/// unit header.
const SHORTEST_FILLER_BYTES: usize = 5;

/// How many bytes the PES header of one coded picture spends inside the packet
/// that opens it: the packet start code, the stream id, the length and flag
/// fields, and the five bytes of a presentation timestamp (ISO/IEC 13818-1
/// table 2-21).
const PES_HEADER_BYTES: usize = 14;

/// How many bytes of elementary stream the packet that OPENS a coded picture
/// carries, the rest of it going to that picture's PES header.
const PES_FIRST_PAYLOAD_BYTES: usize = TS_PAYLOAD_BYTES - PES_HEADER_BYTES;

/// The video track one archived fragment declares.
pub(crate) struct FragmentTrack {
    /// The track as written media declares it: geometry, parameter sets, timescale.
    pub(crate) track: VideoTrack,
    /// The `TrackNumber` the archived container declares for it, which is the
    /// track a playlist addresses.
    pub(crate) number: u64,
}

/// The track one archived fragment declares, or the fault that stands in the way
/// of reading it.
pub(crate) fn fragment_track(media: &[u8]) -> Result<FragmentTrack, ContainerFault> {
    let read = read_fragment(media)?;
    let record = read_codec_private(&read.track)?;
    let track = video_track(&read.track, &record)?;
    Ok(FragmentTrack {
        track,
        number: read.track.number,
    })
}

/// Whether a media answer of `frames` frames assembling `bytes` of media is inside
/// the ceilings one playback request may ask for.
///
/// Checked on the PRODUCT the answer would assemble, before anything is written:
/// the frame count and the bytes of one fragment each have a ceiling of their own
/// in the archive, but a stream repeats a track's parameter sets per coded picture
/// and frames each picture in whole packets, so the two multiply and the product
/// is what has to be bounded.
fn within_ceilings(what: &str, frames: usize, bytes: usize) -> Result<(), ContainerFault> {
    if frames > MAX_SERVED_FRAMES {
        return Err(ContainerFault::Unwritable(format!(
            "{what} carries {frames} frames, over the {MAX_SERVED_FRAMES} frames one served \
             fragment may carry"
        )));
    }
    if bytes > MAX_SERVED_BYTES {
        return Err(ContainerFault::Unwritable(format!(
            "{what} would assemble {bytes} bytes, over the {MAX_SERVED_BYTES} bytes one served \
             fragment may assemble"
        )));
    }
    Ok(())
}

/// The MP4 initialization fragment of `track`: the atoms a player sets its decoder
/// up from, carrying no media.
pub(crate) fn mp4_init_fragment(track: &VideoTrack) -> Result<Vec<u8>, ContainerFault> {
    h264_mp4::mux_avc_init(track).map_err(ContainerFault::Unwritable)
}

/// The MP4 media fragment of ONE archived fragment: its own frames, in the order
/// the container carried them, over the span the archive recorded for it.
pub(crate) fn mp4_media_fragment(
    media: &[u8],
    duration_millis: u64,
) -> Result<Vec<u8>, ContainerFault> {
    let read = read_fragment(media)?;
    let record = read_codec_private(&read.track)?;
    let track = video_track(&read.track, &record)?;
    within_ceilings(
        "an MP4 media fragment",
        read.frames.len(),
        read.frames
            .iter()
            .fold(0usize, |sum, frame| sum.saturating_add(frame.bytes.len())),
    )?;
    let samples = timed_samples(&read, &record, duration_millis);
    if samples.is_empty() {
        return Err(ContainerFault::NoFrames);
    }
    h264_mp4::mux_avc(&track, &samples).map_err(ContainerFault::Unwritable)
}

/// One sample per frame of the fragment's video track.
///
/// Each sample lasts until the next one is shown and the last until the fragment's
/// recorded duration ends — never less than one tick, so no sample is written with
/// a duration a player would read as none.
fn timed_samples(
    read: &ArchivedFragment,
    record: &AvcRecord,
    duration_millis: u64,
) -> Vec<TimedSample> {
    let ends_at = i64::try_from(duration_millis).unwrap_or(i64::MAX);
    read.frames
        .iter()
        .enumerate()
        .map(|(position, frame)| {
            let until = read
                .frames
                .get(position + 1)
                .map_or(ends_at, |next| next.offset_millis);
            let ticks = u32::try_from(until.saturating_sub(frame.offset_millis)).unwrap_or(u32::MAX);
            TimedSample {
                sample: avcc_sample(frame, record.length_size),
                duration: ticks.max(MINIMUM_SAMPLE_TICKS),
            }
        })
        .collect()
}

/// The MPEG TS fragment of ONE archived fragment: the program tables of a
/// single-program transport stream, then one PES packet per coded picture carrying
/// that picture's parameter sets and frames in the Annex B form a decoder reads.
///
/// `offset_millis` is where the fragment starts on the session's own timeline,
/// which is what the presentation timestamps are counted from, so two fragments of
/// one session do not both claim to start at zero.
pub(crate) fn ts_fragment(
    media: &[u8],
    offset_millis: i64,
) -> Result<Vec<u8>, ContainerFault> {
    use ::mpeg2ts::es::StreamType;
    use ::mpeg2ts::ts::payload::Bytes;
    use ::mpeg2ts::ts::{
        ContinuityCounter, EsInfo, Pid, ProgramAssociation, TransportScramblingControl, TsHeader,
        TsPacket, TsPacketWriter, TsPayload, VersionNumber, WriteTsPacket,
    };

    let read = read_fragment(media)?;
    let record = read_codec_private(&read.track)?;
    if read.frames.is_empty() {
        return Err(ContainerFault::NoFrames);
    }

    // "both initialization and media data": the parameter sets travel with every
    // coded picture, so the stream a fragment assembles is the product of its frame
    // count and the bytes each picture carries — bounded here, before a packet is
    // written. The packet framing adds four bytes per 184 of payload, so bounding
    // the payload bounds the stream within a fiftieth of the ceiling.
    let parameter_sets = h264_mp4::annex_b_parameter_sets(&[
        record.seq_param_set.clone(),
        record.pic_param_set.clone(),
    ]);
    within_ceilings(
        "an MPEG TS fragment",
        read.frames.len(),
        read.frames.iter().fold(0usize, |sum, frame| {
            sum.saturating_add(parameter_sets.len())
                .saturating_add(frame.bytes.len())
                // The padding that fills the last packet of one coded picture.
                .saturating_add(TS_PAYLOAD_BYTES)
        }),
    )?;

    let refused = |reason: String| ContainerFault::Unwritable(reason);
    let pid = |raw: u16| Pid::new(raw).map_err(|error| refused(format!("packet id {raw}: {error}")));
    let pmt_pid = pid(TS_PMT_PID)?;
    let video_pid = pid(TS_VIDEO_PID)?;
    let pat_pid = pid(Pid::PAT)?;

    let mut writer = TsPacketWriter::new(Vec::new());
    let mut table_counter = ContinuityCounter::new();
    let header = |pid: Pid, continuity_counter: ContinuityCounter| TsHeader {
        transport_error_indicator: false,
        transport_priority: false,
        pid,
        transport_scrambling_control: TransportScramblingControl::NotScrambled,
        continuity_counter,
    };

    // The program association table: which packet id carries the map of the one
    // program this stream holds.
    writer
        .write_ts_packet(&TsPacket {
            header: header(pat_pid, table_counter),
            adaptation_field: None,
            payload: Some(TsPayload::Pat(::mpeg2ts::ts::payload::Pat {
                transport_stream_id: TS_TRANSPORT_STREAM_ID,
                version_number: VersionNumber::default(),
                table: ::std::vec![ProgramAssociation {
                    program_num: TS_PROGRAM_NUMBER,
                    program_map_pid: pmt_pid,
                }],
            })),
        })
        .map_err(|error| refused(format!("the program association table: {error}")))?;
    table_counter.increment();

    // The program map table: the one elementary stream of that program, declared
    // as the H.264 video it is.
    writer
        .write_ts_packet(&TsPacket {
            header: header(pmt_pid, table_counter),
            adaptation_field: None,
            payload: Some(TsPayload::Pmt(::mpeg2ts::ts::payload::Pmt {
                program_num: TS_PROGRAM_NUMBER,
                pcr_pid: Some(video_pid),
                version_number: VersionNumber::default(),
                program_info: ::std::vec![],
                es_info: ::std::vec![EsInfo {
                    stream_type: StreamType::H264,
                    elementary_pid: video_pid,
                    descriptors: ::std::vec![],
                }],
            })),
        })
        .map_err(|error| refused(format!("the program map table: {error}")))?;

    let mut video_counter = ContinuityCounter::new();
    for frame in &read.frames {
        // The parameter sets ahead of the picture, so a player that joined at any
        // packet can decode what follows.
        let mut unit = parameter_sets.clone();
        if !h264_mp4::annex_b_from_avcc(&frame.bytes, record.length_size, &mut unit) {
            unit.extend_from_slice(&frame.bytes);
        }
        let at_millis = offset_millis.saturating_add(frame.offset_millis);
        // A transport timestamp counts 90 kHz ticks in a field of its own width, so
        // an instant past that field wraps inside it rather than being rejected.
        let ticks = u64::try_from(at_millis.saturating_mul(TS_CLOCK_HZ) / 1_000).unwrap_or(0);
        let pts = ::mpeg2ts::time::Timestamp::new(
            ticks % ::mpeg2ts::time::Timestamp::MAX.saturating_add(1).max(1),
        )
        .map_err(|error| refused(format!("a presentation timestamp: {error}")))?;
        let unit = padded(unit);

        let (opening, continuing) = unit.split_at(PES_FIRST_PAYLOAD_BYTES.min(unit.len()));
        let data = Bytes::new(opening)
            .map_err(|error| refused(format!("the opening packet's payload: {error}")))?;
        writer
            .write_ts_packet(&TsPacket {
                header: header(video_pid, video_counter),
                adaptation_field: None,
                payload: Some(TsPayload::PesStart(::mpeg2ts::ts::payload::Pes {
                    header: ::mpeg2ts::pes::PesHeader {
                        stream_id: ::mpeg2ts::es::StreamId::new(TS_VIDEO_STREAM_ID),
                        priority: false,
                        data_alignment_indicator: true,
                        copyright: false,
                        original_or_copy: true,
                        pts: Some(pts),
                        dts: None,
                        escr: None,
                    },
                    // Zero is what a video PES states when its length is not
                    // declared ahead of the data (ISO/IEC 13818-1 2.4.3.7: "a
                    // value of 0 indicates that the PES packet length is
                    // unbounded"), which is what a picture written as it is read
                    // can state.
                    pes_packet_len: 0,
                    data,
                })),
            })
            .map_err(|error| refused(format!("the opening media packet: {error}")))?;
        video_counter.increment();

        for chunk in continuing.chunks(TS_PAYLOAD_BYTES) {
            let bytes = Bytes::new(chunk)
                .map_err(|error| refused(format!("a packet payload: {error}")))?;
            writer
                .write_ts_packet(&TsPacket {
                    header: header(video_pid, video_counter),
                    adaptation_field: None,
                    payload: Some(TsPayload::PesContinuation(bytes)),
                })
                .map_err(|error| refused(format!("a media packet: {error}")))?;
            video_counter.increment();
        }
    }

    Ok(writer.into_stream())
}

/// `stream` padded with FILLER DATA until it fills whole transport packets.
///
/// A transport packet is a fixed 188 bytes, of which the one that OPENS a coded
/// picture spends [`PES_HEADER_BYTES`] on that picture's own header, so an
/// elementary stream that does not fill its last packet has to be padded. The
/// padding is an H.264 filler data unit, which is what the codec itself specifies
/// for the purpose: it carries no coded picture, so what a decoder sees is exactly
/// the frames the archive holds.
fn padded(mut stream: Vec<u8>) -> Vec<u8> {
    let length = stream.len();
    let mut target = if length <= PES_FIRST_PAYLOAD_BYTES {
        PES_FIRST_PAYLOAD_BYTES
    } else {
        PES_FIRST_PAYLOAD_BYTES
            + (length - PES_FIRST_PAYLOAD_BYTES).div_ceil(TS_PAYLOAD_BYTES) * TS_PAYLOAD_BYTES
    };
    if target == length {
        return stream;
    }
    // A filler unit cannot be shorter than its own header, so a stream that nearly
    // fills its last packet is filled to the end of the NEXT one instead.
    while target - length < SHORTEST_FILLER_BYTES {
        target += TS_PAYLOAD_BYTES;
    }
    stream.extend_from_slice(&[0, 0, 0, 1, FILLER_DATA_UNIT]);
    stream.extend(::std::iter::repeat_n(
        0xFF,
        target - length - SHORTEST_FILLER_BYTES,
    ));
    stream
}

