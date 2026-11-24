// SPDX-License-Identifier: CC0-1.0

//! The ONE framing seam between an H.264 byte stream and the MP4 file that carries it,
//! for every service that produces video in this process.
//!
//! An H.264 encoder answers an ANNEX B byte stream: coded units delimited by start codes
//! and nothing else (ISO/IEC 14496-10 Annex B). An MP4 track carries the same units
//! LENGTH-PREFIXED, one sample per coded picture, with the sequence and picture parameter
//! sets lifted out of the stream and into the track's own `avcC` configuration record
//! (ISO/IEC 14496-15). A decoder wants the Annex B form back. That translation, in both
//! directions, plus the write of the file around it, is what this module holds — so the
//! services that render or re-render video state it once:
//!
//! * [`annex_b_units`] and [`parameter_set`] read an encoder's byte stream;
//! * [`avcc_samples`] frames its coded pictures the way a track carries them;
//! * [`mux_avc`] writes the file, through the maintained MP4 writer, and
//!   [`mux_avc_init`] writes the INITIALIZATION of the same track on its own — the
//!   `ftyp` and `moov` atoms a player loads before any media;
//! * [`annex_b_from_avcc`] turns a track's sample back into the stream a decoder reads.
//!
//! Nothing here decodes or encodes pictures: the codec is the maintained encoder each
//! caller drives, and this module only frames what it produced.

use ::std::io::Cursor;

/// The `nal_unit_type` of a coded slice of an instantaneous decoding refresh picture: the
/// unit a player may start decoding at, which is what an MP4 sample reports as a sync
/// sample.
pub const IDR_PICTURE: u8 = 5;

/// The `nal_unit_type` of a sequence parameter set — the geometry and profile of the
/// coded video, which an MP4 track states in its configuration record rather than in a
/// sample.
pub const SEQUENCE_PARAMETER_SET: u8 = 7;

/// The `nal_unit_type` of a picture parameter set, which a track states the same way.
pub const PICTURE_PARAMETER_SET: u8 = 8;

/// One network abstraction layer unit of an H.264 stream: its type, and its payload with
/// no start code.
#[derive(Debug, Clone)]
pub struct NalUnit<'a> {
    /// The `nal_unit_type` field, which is what says whether the unit is a parameter set
    /// or a coded picture.
    pub kind: u8,
    /// The unit's own bytes, start code excluded.
    pub bytes: &'a [u8],
}

/// Split an Annex B byte stream into its units.
///
/// Annex B delimits each unit with a three- or four-byte start code and nothing else, which
/// is the whole framing: the units are handed to the MP4 writer, which frames them by
/// length instead.
pub fn annex_b_units(stream: &[u8]) -> Vec<NalUnit<'_>> {
    // Where each unit's own bytes begin, which is just past its start code.
    let mut payloads: Vec<usize> = Vec::new();
    let mut at = 0usize;
    while at + 3 <= stream.len() {
        if stream[at..at + 3] == [0, 0, 1] {
            payloads.push(at + 3);
            at += 3;
        } else {
            at += 1;
        }
    }
    let mut units = Vec::with_capacity(payloads.len());
    for (index, payload) in payloads.iter().enumerate() {
        let end = payloads
            .get(index + 1)
            .map_or(stream.len(), |next| trimmed_end(stream, *next - 3));
        let bytes = &stream[*payload..end.max(*payload)];
        if let Some(header) = bytes.first() {
            units.push(NalUnit {
                kind: header & 0x1F,
                bytes,
            });
        }
    }
    units
}

/// Where a unit ends when the next start code is a four-byte one: the leading zero of
/// `00 00 00 01` belongs to the delimiter, not to the unit before it.
fn trimmed_end(stream: &[u8], next_start: usize) -> usize {
    if next_start > 0 && stream.get(next_start - 1) == Some(&0) {
        next_start - 1
    } else {
        next_start
    }
}

/// The first unit of `kind` in `units`, which is how a parameter set is lifted out of a
/// byte stream and into a track's configuration record.
pub fn parameter_set(units: &[NalUnit<'_>], kind: u8) -> Option<Vec<u8>> {
    units
        .iter()
        .find(|unit| unit.kind == kind)
        .map(|unit| unit.bytes.to_vec())
}

/// Whether a unit is a coded slice of a picture, which is what starts a new sample.
fn is_picture(kind: u8) -> bool {
    (1..=5).contains(&kind)
}

/// One coded picture as an MP4 track carries it.
#[derive(Debug, Clone)]
pub struct Sample {
    /// Its units, each framed by its length rather than by a start code.
    pub bytes: Vec<u8>,
    /// Whether a player may start decoding here, which holds for a refresh picture.
    pub sync: bool,
}

/// One sample per coded picture, in the length-prefixed framing an MP4 track carries:
/// every unit that is not a parameter set, with the units that precede a picture attached
/// to it.
pub fn avcc_samples(units: &[NalUnit<'_>]) -> Vec<Sample> {
    let mut samples: Vec<Sample> = Vec::new();
    let mut pending: Vec<u8> = Vec::new();
    for unit in units {
        if unit.kind == SEQUENCE_PARAMETER_SET || unit.kind == PICTURE_PARAMETER_SET {
            continue;
        }
        let length = u32::try_from(unit.bytes.len()).unwrap_or(u32::MAX);
        pending.extend_from_slice(&length.to_be_bytes());
        pending.extend_from_slice(unit.bytes);
        if is_picture(unit.kind) {
            samples.push(Sample {
                bytes: ::std::mem::take(&mut pending),
                sync: unit.kind == IDR_PICTURE,
            });
        }
    }
    if !pending.is_empty() {
        if let Some(last) = samples.last_mut() {
            last.bytes.extend_from_slice(&pending);
        }
    }
    samples
}

/// The ONE video track an [`mux_avc`] file carries: the geometry it states, the units the
/// units of its samples are decoded against, and the clock its sample times are counted
/// in.
#[derive(Debug, Clone)]
pub struct VideoTrack {
    /// The VISIBLE width the file states, in pixels.
    pub width: u16,
    /// The VISIBLE height the file states, in pixels.
    pub height: u16,
    /// How many units of time one second holds, which every sample's duration below is
    /// counted in.
    pub timescale: u32,
    /// The sequence parameter set of the coded video.
    pub seq_param_set: Vec<u8>,
    /// Its picture parameter set.
    pub pic_param_set: Vec<u8>,
}

/// One sample of the written track: the coded picture, and how long it is shown for in the
/// track's own timescale.
#[derive(Debug, Clone)]
pub struct TimedSample {
    /// The coded picture, length-prefixed as [`avcc_samples`] framed it.
    pub sample: Sample,
    /// How long it lasts, in the track's timescale.
    pub duration: u32,
}

/// Write `samples` into an MP4 file carrying one AVC video track.
///
/// `Err` carries what the writer refused, for the caller to report: a file that could not
/// be written is reported as a failure rather than as bytes that are not a container.
pub fn mux_avc(track: &VideoTrack, samples: &[TimedSample]) -> Result<Vec<u8>, String> {
    if samples.is_empty() {
        return Err("the encoded stream carries no coded picture".to_string());
    }
    written_mp4(track, samples)
}

/// Write the INITIALIZATION of `track`: the same file [`mux_avc`] writes, carrying the
/// track's declaration and NO media.
///
/// What an HLS or MPEG-DASH player loads before the first media fragment — the atoms that
/// state the geometry and the parameter sets its decoder is set up from, and nothing a
/// fragment carries. `Err` carries what the writer refused, as [`mux_avc`] does.
pub fn mux_avc_init(track: &VideoTrack) -> Result<Vec<u8>, String> {
    written_mp4(track, &[])
}

/// The MP4 file carrying `track` and `samples`, written by the maintained writer.
///
/// The one write both entry points share, so an initialization and the media fragments
/// beside it declare the same track.
fn written_mp4(track: &VideoTrack, samples: &[TimedSample]) -> Result<Vec<u8>, String> {
    let config = ::mp4::Mp4Config {
        major_brand: ::mp4::FourCC::from(*b"isom"),
        minor_version: 512,
        compatible_brands: ::std::vec![
            ::mp4::FourCC::from(*b"isom"),
            ::mp4::FourCC::from(*b"iso2"),
            ::mp4::FourCC::from(*b"avc1"),
            ::mp4::FourCC::from(*b"mp41"),
        ],
        timescale: track.timescale,
    };
    let mut writer = ::mp4::Mp4Writer::write_start(Cursor::new(Vec::new()), &config)
        .map_err(|error| format!("the MP4 writer could not be started: {error}"))?;
    let configured = ::mp4::TrackConfig {
        track_type: ::mp4::TrackType::Video,
        timescale: track.timescale,
        language: "und".to_string(),
        media_conf: ::mp4::MediaConfig::AvcConfig(::mp4::AvcConfig {
            width: track.width,
            height: track.height,
            seq_param_set: track.seq_param_set.clone(),
            pic_param_set: track.pic_param_set.clone(),
        }),
    };
    writer
        .add_track(&configured)
        .map_err(|error| format!("the MP4 writer refused the video track: {error}"))?;
    let mut start_time = 0u64;
    for timed in samples {
        writer
            .write_sample(
                1,
                &::mp4::Mp4Sample {
                    start_time,
                    duration: timed.duration,
                    rendering_offset: 0,
                    is_sync: timed.sample.sync,
                    bytes: ::bytes::Bytes::from(timed.sample.bytes.clone()),
                },
            )
            .map_err(|error| format!("a sample could not be written: {error}"))?;
        start_time = start_time.saturating_add(u64::from(timed.duration));
    }
    writer
        .write_end()
        .map_err(|error| format!("the MP4 file could not be finished: {error}"))?;
    Ok(writer.into_writer().into_inner())
}

/// The four-byte Annex B start code, which is what delimits a unit of the byte stream a
/// decoder reads.
const START_CODE: [u8; 4] = [0, 0, 0, 1];

/// The Annex B byte stream of a track's sample, appended to `stream`.
///
/// `length_size` is the width of the length field the track's configuration record states
/// (`lengthSizeMinusOne` plus one, so one to four bytes). A sample whose length fields do
/// not frame its own bytes is REFUSED rather than read past: the bytes come from whatever
/// a producer ingested, so a truncated or mis-framed sample answers `false` and the caller
/// reports media it could not read.
pub fn annex_b_from_avcc(sample: &[u8], length_size: usize, stream: &mut Vec<u8>) -> bool {
    if !(1..=4).contains(&length_size) {
        return false;
    }
    let mut at = 0usize;
    while at < sample.len() {
        let Some(header) = sample.get(at..at + length_size) else {
            return false;
        };
        let length = header
            .iter()
            .fold(0usize, |value, byte| (value << 8) | usize::from(*byte));
        at += length_size;
        let Some(unit) = sample.get(at..at.saturating_add(length)) else {
            return false;
        };
        if unit.is_empty() {
            return false;
        }
        stream.extend_from_slice(&START_CODE);
        stream.extend_from_slice(unit);
        at += length;
    }
    at == sample.len()
}

/// A parameter set as a decoder reads it: the Annex B form of the units an `avcC` record
/// states, which every coded picture of that track is decoded against.
pub fn annex_b_parameter_sets(sets: &[Vec<u8>]) -> Vec<u8> {
    let mut stream = Vec::new();
    for set in sets {
        if set.is_empty() {
            continue;
        }
        stream.extend_from_slice(&START_CODE);
        stream.extend_from_slice(set);
    }
    stream
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unit of `kind` whose payload is `length` bytes long, delimited the way an
    /// encoder's byte stream delimits it.
    fn unit(kind: u8, length: usize) -> Vec<u8> {
        let mut bytes = ::std::vec![0, 0, 0, 1, kind];
        bytes.extend(::std::iter::repeat_n(kind.wrapping_add(0x40), length));
        bytes
    }

    /// One row of the splitting table: a byte stream, and the `(kind, length)` of every
    /// unit it carries.
    struct SplitRow {
        case: &'static str,
        stream: Vec<u8>,
        expected: Vec<(u8, usize)>,
    }

    /// Reading a byte stream back: every unit is found, with its type and its own bytes,
    /// over the two start-code widths Annex B allows.
    #[test]
    fn a_byte_stream_splits_into_the_units_it_carries() {
        let rows = [
            SplitRow {
                case: "an empty stream",
                stream: Vec::new(),
                expected: ::std::vec![],
            },
            SplitRow {
                case: "one parameter set",
                stream: unit(SEQUENCE_PARAMETER_SET, 3),
                expected: ::std::vec![(SEQUENCE_PARAMETER_SET, 4)],
            },
            SplitRow {
                case: "parameter sets then a refresh picture",
                stream: [
                    unit(SEQUENCE_PARAMETER_SET, 3),
                    unit(PICTURE_PARAMETER_SET, 1),
                    unit(IDR_PICTURE, 9),
                ]
                .concat(),
                expected: ::std::vec![
                    (SEQUENCE_PARAMETER_SET, 4),
                    (PICTURE_PARAMETER_SET, 2),
                    (IDR_PICTURE, 10),
                ],
            },
            SplitRow {
                case: "a three-byte start code",
                stream: ::std::vec![0, 0, 1, 1, 0xAA, 0, 0, 1, 1, 0xBB],
                expected: ::std::vec![(1, 2), (1, 2)],
            },
        ];
        for SplitRow {
            case,
            stream,
            expected,
        } in rows
        {
            let read: Vec<(u8, usize)> = annex_b_units(&stream)
                .iter()
                .map(|unit| (unit.kind, unit.bytes.len()))
                .collect();
            assert_eq!(
                read, expected,
                "case {case}: expected the units {expected:?}, got {read:?}"
            );
        }
    }

    /// One sample per coded picture, parameter sets left out of them, and a refresh
    /// picture reported as a sync sample.
    #[test]
    fn coded_pictures_are_framed_one_sample_each() {
        let stream = [
            unit(SEQUENCE_PARAMETER_SET, 3),
            unit(PICTURE_PARAMETER_SET, 1),
            unit(IDR_PICTURE, 5),
            unit(1, 7),
        ]
        .concat();
        let units = annex_b_units(&stream);
        let samples = avcc_samples(&units);
        let framing: Vec<(usize, bool)> = samples
            .iter()
            .map(|sample| (sample.bytes.len(), sample.sync))
            .collect();
        assert_eq!(
            framing,
            ::std::vec![(10, true), (12, false)],
            "case two pictures: expected a length-prefixed sample each, got {framing:?}"
        );
    }

    /// One row of the framing table: a track's sample, the width of its length fields, and
    /// the byte stream it reads back as (or `None` where it is refused).
    struct FramingRow {
        case: &'static str,
        sample: Vec<u8>,
        length_size: usize,
        expected: Option<Vec<u8>>,
    }

    /// A sample reads back into the byte stream it was framed from, and a sample whose
    /// length fields do not frame its bytes is refused.
    #[test]
    fn a_framed_sample_reads_back_as_a_byte_stream() {
        let rows = [
            FramingRow {
                case: "one unit",
                sample: ::std::vec![0, 0, 0, 2, 0x65, 0x11],
                length_size: 4,
                expected: Some(::std::vec![0, 0, 0, 1, 0x65, 0x11]),
            },
            FramingRow {
                case: "two units",
                sample: ::std::vec![0, 0, 0, 1, 0x65, 0, 0, 0, 2, 0x41, 0x22],
                length_size: 4,
                expected: Some(::std::vec![0, 0, 0, 1, 0x65, 0, 0, 0, 1, 0x41, 0x22]),
            },
            FramingRow {
                case: "a two-byte length field",
                sample: ::std::vec![0, 2, 0x65, 0x11],
                length_size: 2,
                expected: Some(::std::vec![0, 0, 0, 1, 0x65, 0x11]),
            },
            FramingRow {
                case: "a length past the sample",
                sample: ::std::vec![0, 0, 0, 9, 0x65],
                length_size: 4,
                expected: None,
            },
        ];
        for FramingRow {
            case,
            sample,
            length_size,
            expected,
        } in rows
        {
            let mut stream = Vec::new();
            let read = annex_b_from_avcc(&sample, length_size, &mut stream);
            let actual = read.then_some(stream);
            assert_eq!(
                actual, expected,
                "case {case}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}
