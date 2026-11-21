// SPDX-License-Identifier: CC0-1.0

//! `Probe` reports what a media file IS, read from the file's own bytes.
//!
//! The rule under test: for each `InputFiles[].FileUrl` the request names, Probe reads the
//! object that URL addresses and reports what it found — the container the BYTES are (not
//! what the key's extension or the object's `Content-Type` claims), the object's own
//! metadata, and one entry per track the container carries with the properties that track
//! states — and reports nothing it could not read. A file it cannot read or cannot
//! recognise is refused with the modelled error for WHY, never described.
//!
//! The probe is a REAL read of a REAL file: the suite mounts S3 beside MediaConvert on one
//! engine, writes media it muxes itself (the ISO base media writer for the MP4 family, the
//! RIFF/WAVE writer for Wave) into a bucket, and asserts what Probe answers over the wire.
//!
//! The rows span what the rule generalises over: the container (MP4, QuickTime, 3GP, 3G2,
//! Wave), which track types a file carries (video and audio, video alone, audio alone),
//! the geometry and frame rate of a video track, the sample rate, channel count and bit
//! depth of an audio track, the form the URL addresses the object in, and how many files
//! one request names.

use serde_json::{json, Value};

use super::job_lifecycle::{Emulator, Reply, REGION};

/// The bucket this suite creates and writes its media into.
pub(crate) const BUCKET: &str = "mediaconvert-probe-media";

/// A bucket nothing ever creates, so S3 answers for it as the account holding nothing.
const ABSENT_BUCKET: &str = "mediaconvert-probe-no-such-bucket";

/// The movie timescale every file this suite muxes is written in. It is the ISO base
/// media writer's own accounting unit: a sample's duration is converted into it as the
/// movie's duration is accumulated, so a timescale every track's sample duration divides
/// exactly is what keeps the container duration below exact rather than truncated.
const MOVIE_TIMESCALE: u32 = 30_000;

/// How close a reported duration has to be to the one the file was written with. The
/// container states its duration in a timescale, so the value is exact to that
/// timescale's resolution rather than to the last bit of a float.
const DURATION_TOLERANCE: f64 = 0.002;

/// An AVC sequence parameter set and picture parameter set, as a muxer stores them in the
/// sample entry. Nothing here decodes them — the probe reads the container, not the codec
/// essence — so they only have to travel.
const SEQUENCE_PARAMETER_SET: &[u8] = &[
    0x67, 0x64, 0x00, 0x28, 0xac, 0xd9, 0x40, 0x78, 0x02, 0x27, 0xe5, 0x84, 0x00, 0x00, 0x03,
    0x00, 0x04, 0x00, 0x00, 0x03, 0x00, 0xca, 0x3c, 0x48, 0x96, 0x11, 0x80,
];
const PICTURE_PARAMETER_SET: &[u8] = &[0x68, 0xeb, 0xe3, 0xcb, 0x22, 0xc0];

/// One coded picture's payload, length-prefixed the way an MP4 track carries it.
const VIDEO_SAMPLE: &[u8] = &[0x00, 0x00, 0x00, 0x04, 0x65, 0x88, 0x80, 0x00];

/// One audio frame's payload.
const AUDIO_SAMPLE: &[u8] = &[0x21, 0x10, 0x05, 0x60];

/// A video track to mux: `frames` samples of `sample_duration` units at `timescale`, so
/// the rate a probe reads back is exactly `timescale / sample_duration`.
///
/// Shared with the `job_lifecycle` suite, which runs jobs over media muxed here rather than
/// keeping a second muxer of its own.
pub(crate) struct VideoTrack {
    pub(crate) width: u16,
    pub(crate) height: u16,
    pub(crate) timescale: u32,
    pub(crate) frames: u32,
    pub(crate) sample_duration: u32,
}

/// An audio track to mux: `frames` frames of `samples_per_frame` samples at `sample_rate`.
pub(crate) struct AudioTrack {
    pub(crate) sample_rate: ::mp4::SampleFreqIndex,
    pub(crate) channels: ::mp4::ChannelConfig,
    pub(crate) frames: u32,
    pub(crate) samples_per_frame: u32,
}

/// An ISO base media file (MP4, QuickTime, 3GP, 3G2 — the `brand` decides which) carrying
/// the tracks given, written with the ISO base media writer.
pub(crate) fn iso_bmff(
    brand: &[u8; 4],
    video: Option<VideoTrack>,
    audio: Option<AudioTrack>,
) -> Vec<u8> {
    let config = ::mp4::Mp4Config {
        major_brand: ::mp4::FourCC::from(*brand),
        minor_version: 512,
        compatible_brands: vec![::mp4::FourCC::from(*brand)],
        timescale: MOVIE_TIMESCALE,
    };
    let mut writer = ::mp4::Mp4Writer::write_start(std::io::Cursor::new(Vec::new()), &config)
        .expect("the ISO base media writer starts");
    let mut next_track_id = 1u32;
    let mut video_track = None;
    let mut audio_track = None;
    if let Some(track) = &video {
        writer
            .add_track(&::mp4::TrackConfig {
                track_type: ::mp4::TrackType::Video,
                timescale: track.timescale,
                language: "und".to_string(),
                media_conf: ::mp4::MediaConfig::AvcConfig(::mp4::AvcConfig {
                    width: track.width,
                    height: track.height,
                    seq_param_set: SEQUENCE_PARAMETER_SET.to_vec(),
                    pic_param_set: PICTURE_PARAMETER_SET.to_vec(),
                }),
            })
            .expect("the writer takes a video track");
        video_track = Some(next_track_id);
        next_track_id += 1;
    }
    if let Some(track) = &audio {
        writer
            .add_track(&::mp4::TrackConfig {
                track_type: ::mp4::TrackType::Audio,
                timescale: track.sample_rate.freq(),
                language: "eng".to_string(),
                media_conf: ::mp4::MediaConfig::AacConfig(::mp4::AacConfig {
                    bitrate: 128_000,
                    profile: ::mp4::AudioObjectType::AacLowComplexity,
                    freq_index: track.sample_rate,
                    chan_conf: track.channels,
                }),
            })
            .expect("the writer takes an audio track");
        audio_track = Some(next_track_id);
    }
    if let (Some(id), Some(track)) = (video_track, &video) {
        write_samples(
            &mut writer,
            id,
            track.frames,
            track.sample_duration,
            VIDEO_SAMPLE,
        );
    }
    if let (Some(id), Some(track)) = (audio_track, &audio) {
        write_samples(
            &mut writer,
            id,
            track.frames,
            track.samples_per_frame,
            AUDIO_SAMPLE,
        );
    }
    writer.write_end().expect("the file is finished");
    writer.into_writer().into_inner()
}

/// `frames` samples of `duration` units each, back to back, onto one track.
fn write_samples(
    writer: &mut ::mp4::Mp4Writer<std::io::Cursor<Vec<u8>>>,
    track_id: u32,
    frames: u32,
    duration: u32,
    payload: &[u8],
) {
    for frame in 0..frames {
        writer
            .write_sample(
                track_id,
                &::mp4::Mp4Sample {
                    start_time: u64::from(frame) * u64::from(duration),
                    duration,
                    rendering_offset: 0,
                    is_sync: frame == 0,
                    bytes: ::bytes::Bytes::from(payload.to_vec()),
                },
            )
            .expect("the writer takes a sample");
    }
}

/// A RIFF/WAVE file of `frames` silent frames in the format given, written with the
/// RIFF/WAVE writer.
fn wave(channels: u16, sample_rate: u32, bits_per_sample: u16, frames: u32) -> Vec<u8> {
    let spec = ::hound::WavSpec {
        channels,
        sample_rate,
        bits_per_sample,
        sample_format: ::hound::SampleFormat::Int,
    };
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = ::hound::WavWriter::new(&mut buffer, spec).expect("a wave writer");
        for _ in 0..frames {
            for _ in 0..channels {
                writer.write_sample(0i32).expect("a wave sample");
            }
        }
        writer.finalize().expect("the wave file is finished");
    }
    buffer.into_inner()
}

/// An emulator with S3 mounted beside MediaConvert on the same engine, holding one bucket.
async fn emulator_with_bucket() -> Emulator {
    let emulator = Emulator::beside(service_com_amazonaws_s3_amazons3::EmulatedService::new);
    create_bucket(&emulator).await;
    emulator
}

/// Create this suite's bucket in the S3 mounted beside MediaConvert, so a file can be
/// written into it. Shared with the `job_lifecycle` suite, which needs the same bucket under
/// an emulator whose clock span is its own.
pub(crate) async fn create_bucket(emulator: &Emulator) {
    // A create sent to a REGIONAL endpoint states its location constraint: the
    // unspecified constraint means us-east-1, and this suite signs for `REGION`.
    let (status, _, body) = emulator
        .rest(
            "s3",
            "PUT",
            &format!("/{BUCKET}"),
            format!(
                "<CreateBucketConfiguration \
                 xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
                 <LocationConstraint>{REGION}</LocationConstraint>\
                 </CreateBucketConfiguration>"
            )
            .into_bytes(),
        )
        .await;
    assert!(
        (200..300).contains(&status),
        "setup: CreateBucket must succeed, got {status} {}",
        String::from_utf8_lossy(&body)
    );
}

/// Write `bytes` to `key` and answer the entity tag S3 reported for the object, so what
/// Probe reports as the file's metadata can be held against what the store holds.
pub(crate) async fn put_object(emulator: &Emulator, key: &str, bytes: &[u8]) -> String {
    let (status, headers, body) = emulator
        .rest("s3", "PUT", &format!("/{BUCKET}/{key}"), bytes.to_vec())
        .await;
    assert!(
        (200..300).contains(&status),
        "setup: PutObject of {key:?} must succeed, got {status} {}",
        String::from_utf8_lossy(&body)
    );
    headers
        .get("etag")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .trim_matches('"')
        .to_string()
}

/// Probe the URLs given, in that order.
async fn probe(emulator: &Emulator, urls: &[&str]) -> Reply {
    let files: Vec<Value> = urls.iter().map(|url| json!({ "fileUrl": url })).collect();
    emulator
        .send(
            "POST",
            "/2017-08-29/probe",
            Some(json!({ "inputFiles": files })),
        )
        .await
}

/// The `s3://` URI of a key in this suite's bucket.
pub(crate) fn s3_uri(key: &str) -> String {
    format!("s3://{BUCKET}/{key}")
}

/// One member a row pins on a track, as a JSON pointer into that track and the value it
/// must carry.
type Pinned = (&'static str, Value);

/// One row: a media file, how it is addressed, and what Probe must report about it.
struct Case {
    case: &'static str,
    key: &'static str,
    bytes: Vec<u8>,
    format: &'static str,
    mime_type: &'static str,
    /// The duration the file was written with, in seconds.
    duration: f64,
    /// One entry per track the file carries, in the order Probe must report them; each
    /// entry pins the members that track states.
    tracks: Vec<Vec<Pinned>>,
    /// The track indexes, by type, the result's one `trackMappings` entry must carry.
    video_indexes: Option<Value>,
    audio_indexes: Option<Value>,
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            case: "an MP4 carrying a video track and an audio track",
            key: "in/movie.mp4",
            bytes: iso_bmff(
                b"isom",
                Some(VideoTrack {
                    width: 1_920,
                    height: 1_080,
                    // 30 frames of 1001 units at 30000: the exact 30000/1001 rate.
                    timescale: 30_000,
                    frames: 30,
                    sample_duration: 1_001,
                }),
                Some(AudioTrack {
                    sample_rate: ::mp4::SampleFreqIndex::Freq48000,
                    channels: ::mp4::ChannelConfig::Stereo,
                    frames: 40,
                    samples_per_frame: 1_200,
                }),
            ),
            format: "mp4",
            mime_type: "video/mp4",
            // The movie runs as long as its longest track: 30 * 1001 / 30000.
            duration: 1.001,
            tracks: vec![
                vec![
                    ("/index", json!(1)),
                    ("/trackType", json!("video")),
                    ("/codec", json!("AVC")),
                    ("/videoProperties/width", json!(1_920)),
                    ("/videoProperties/height", json!(1_080)),
                    ("/videoProperties/frameRate/numerator", json!(30_000)),
                    ("/videoProperties/frameRate/denominator", json!(1_001)),
                ],
                vec![
                    ("/index", json!(2)),
                    ("/trackType", json!("audio")),
                    ("/codec", json!("AAC")),
                    ("/audioProperties/sampleRate", json!(48_000)),
                    ("/audioProperties/channels", json!(2)),
                    ("/audioProperties/channelLayout", json!("stereo")),
                    ("/audioProperties/languageCode", json!("eng")),
                ],
            ],
            video_indexes: Some(json!([1])),
            audio_indexes: Some(json!([2])),
        },
        Case {
            case: "a QuickTime movie carrying only a video track",
            key: "in/movie.mov",
            bytes: iso_bmff(
                b"qt  ",
                Some(VideoTrack {
                    width: 640,
                    height: 480,
                    timescale: 1_000,
                    frames: 25,
                    sample_duration: 40,
                }),
                None,
            ),
            format: "quicktime",
            mime_type: "video/quicktime",
            duration: 1.0,
            tracks: vec![vec![
                ("/index", json!(1)),
                ("/trackType", json!("video")),
                ("/codec", json!("AVC")),
                ("/videoProperties/width", json!(640)),
                ("/videoProperties/height", json!(480)),
                ("/videoProperties/frameRate/numerator", json!(25)),
                ("/videoProperties/frameRate/denominator", json!(1)),
            ]],
            video_indexes: Some(json!([1])),
            audio_indexes: None,
        },
        Case {
            case: "a 3GP file carrying only a mono audio track",
            key: "in/call.3gp",
            bytes: iso_bmff(
                b"3gp5",
                None,
                Some(AudioTrack {
                    sample_rate: ::mp4::SampleFreqIndex::Freq16000,
                    channels: ::mp4::ChannelConfig::Mono,
                    frames: 32,
                    samples_per_frame: 1_024,
                }),
            ),
            format: "three_gp",
            mime_type: "video/3gpp",
            // 32 * 1024 / 16000.
            duration: 2.048,
            tracks: vec![vec![
                ("/index", json!(1)),
                ("/trackType", json!("audio")),
                ("/codec", json!("AAC")),
                ("/audioProperties/sampleRate", json!(16_000)),
                ("/audioProperties/channels", json!(1)),
                ("/audioProperties/channelLayout", json!("mono")),
            ]],
            video_indexes: None,
            audio_indexes: Some(json!([1])),
        },
        Case {
            case: "a 3G2 file, the same family under another brand",
            key: "in/call.3g2",
            bytes: iso_bmff(
                b"3g2a",
                None,
                Some(AudioTrack {
                    sample_rate: ::mp4::SampleFreqIndex::Freq44100,
                    channels: ::mp4::ChannelConfig::Stereo,
                    frames: 10,
                    samples_per_frame: 4_410,
                }),
            ),
            format: "three_g2",
            mime_type: "video/3gpp2",
            // 10 * 4410 / 44100.
            duration: 1.0,
            tracks: vec![vec![
                ("/index", json!(1)),
                ("/trackType", json!("audio")),
                ("/codec", json!("AAC")),
                ("/audioProperties/sampleRate", json!(44_100)),
                ("/audioProperties/channels", json!(2)),
            ]],
            video_indexes: None,
            audio_indexes: Some(json!([1])),
        },
        Case {
            case: "a mono 8-bit Wave file",
            key: "in/voice.wav",
            bytes: wave(1, 8_000, 8, 8_000),
            format: "wave",
            mime_type: "audio/wav",
            duration: 1.0,
            tracks: vec![vec![
                ("/index", json!(1)),
                ("/trackType", json!("audio")),
                ("/codec", json!("PCM")),
                ("/audioProperties/sampleRate", json!(8_000)),
                ("/audioProperties/channels", json!(1)),
                ("/audioProperties/channelLayout", json!("mono")),
                ("/audioProperties/bitDepth", json!(8)),
                // Linear PCM: sample rate * channels * bit depth, exactly.
                ("/audioProperties/bitRate", json!(64_000)),
            ]],
            video_indexes: None,
            audio_indexes: Some(json!([1])),
        },
        Case {
            case: "a stereo 16-bit Wave file at another sample rate",
            key: "in/music.wav",
            bytes: wave(2, 48_000, 16, 24_000),
            format: "wave",
            mime_type: "audio/wav",
            duration: 0.5,
            tracks: vec![vec![
                ("/index", json!(1)),
                ("/trackType", json!("audio")),
                ("/codec", json!("PCM")),
                ("/audioProperties/sampleRate", json!(48_000)),
                ("/audioProperties/channels", json!(2)),
                ("/audioProperties/channelLayout", json!("stereo")),
                ("/audioProperties/bitDepth", json!(16)),
                ("/audioProperties/bitRate", json!(1_536_000)),
            ]],
            video_indexes: None,
            audio_indexes: Some(json!([1])),
        },
        Case {
            case: "a 24-bit Wave file with a surround channel count",
            key: "in/surround.wav",
            bytes: wave(6, 48_000, 24, 4_800),
            format: "wave",
            mime_type: "audio/wav",
            duration: 0.1,
            tracks: vec![vec![
                ("/index", json!(1)),
                ("/trackType", json!("audio")),
                ("/codec", json!("PCM")),
                ("/audioProperties/channels", json!(6)),
                // A layout the member documents beyond the two this suite's other rows
                // carry.
                ("/audioProperties/channelLayout", json!("5.1")),
                ("/audioProperties/bitDepth", json!(24)),
                ("/audioProperties/bitRate", json!(6_912_000)),
            ]],
            video_indexes: None,
            audio_indexes: Some(json!([1])),
        },
    ]
}

/// The one result of a reply that probed one file.
fn only_result(reply: &Reply, case: &str) -> Value {
    let results = reply
        .at("/probeResults")
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected probeResults, got {:?}", reply.body))
        .clone();
    assert_eq!(
        results.len(),
        1,
        "case {case:?}: expected one result per input file, got {:?}",
        reply.body
    );
    results[0].clone()
}

/// A member of `value`, or `Value::Null` when it is absent.
fn member(value: &Value, pointer: &str) -> Value {
    value.pointer(pointer).cloned().unwrap_or(Value::Null)
}

#[tokio::test]
async fn a_probe_reports_the_container_the_bytes_really_are() {
    let emulator = emulator_with_bucket().await;
    let cases = cases();
    for case in &cases {
        let e_tag = put_object(&emulator, case.key, &case.bytes).await;
        let reply = probe(&emulator, &[&s3_uri(case.key)]).await;
        assert_eq!(
            reply.status, 200,
            "case {:?}: Probe must succeed, got {} {:?}",
            case.case, reply.status, reply.body
        );
        let result = only_result(&reply, case.case);

        // The container the bytes ARE, and the media type of that container — neither of
        // which the object was stored with: every object above was written with
        // `application/octet-stream` as its content type.
        assert_eq!(
            member(&result, "/container/format"),
            json!(case.format),
            "case {:?}: expected format {:?}, got {result}",
            case.case,
            case.format
        );
        assert_eq!(
            member(&result, "/metadata/mimeType"),
            json!(case.mime_type),
            "case {:?}: expected mimeType {:?}, got {result}",
            case.case,
            case.mime_type
        );

        // The object's own metadata, as the store holds it.
        let file_size = member(&result, "/metadata/fileSize").as_i64();
        assert_eq!(
            file_size,
            Some(case.bytes.len() as i64),
            "case {:?}: expected the stored file's size, got {file_size:?}",
            case.case
        );
        assert_eq!(
            member(&result, "/metadata/eTag"),
            json!(e_tag),
            "case {:?}: expected the stored object's entity tag {e_tag:?}, got {result}",
            case.case
        );
        let last_modified = member(&result, "/metadata/lastModified").as_f64();
        assert!(
            last_modified.is_some_and(|seconds| seconds > 0.0),
            "case {:?}: expected the object's last modification instant, got \
             {last_modified:?}",
            case.case
        );

        // The duration the file was written with, and the overall rate the member
        // documents as (file size in bytes * 8) / duration in seconds.
        let duration = member(&result, "/container/duration").as_f64().unwrap_or(0.0);
        assert!(
            (duration - case.duration).abs() < DURATION_TOLERANCE,
            "case {:?}: expected a duration of {} s, got {duration}",
            case.case,
            case.duration
        );
        let expected_rate = (case.bytes.len() as f64 * 8.0 / duration).round() as i64;
        let bit_rate = member(&result, "/container/bitRate").as_i64();
        assert_eq!(
            bit_rate,
            Some(expected_rate),
            "case {:?}: expected an overall bit rate of {expected_rate}, got {bit_rate:?}",
            case.case
        );

        // One entry per track the container carries, in order, each stating what that
        // track is.
        let tracks = member(&result, "/container/tracks");
        let tracks = tracks.as_array().cloned().unwrap_or_default();
        assert_eq!(
            tracks.len(),
            case.tracks.len(),
            "case {:?}: expected {} track(s), got {tracks:?}",
            case.case,
            case.tracks.len()
        );
        for (track, pinned) in tracks.iter().zip(&case.tracks) {
            for (pointer, expected) in pinned {
                assert_eq!(
                    member(track, pointer),
                    *expected,
                    "case {:?}: expected {pointer} to be {expected}, got {track}",
                    case.case
                );
            }
            let seconds = member(track, "/duration").as_f64();
            assert!(
                seconds.is_some_and(|seconds| seconds > 0.0),
                "case {:?}: every track states how long it runs, got {seconds:?} in \
                 {track}",
                case.case
            );
        }

        // The mapping is those same tracks indexed by type: a type the file carries none
        // of is left out rather than reported empty.
        let mappings = member(&result, "/trackMappings");
        let mappings = mappings.as_array().cloned().unwrap_or_default();
        assert_eq!(
            mappings.len(),
            1,
            "case {:?}: expected one track mapping, got {mappings:?}",
            case.case
        );
        let mapping = &mappings[0];
        assert_eq!(
            member(mapping, "/videoTrackIndexes"),
            case.video_indexes.clone().unwrap_or(Value::Null),
            "case {:?}: expected the video track indexes, got {mapping}",
            case.case
        );
        assert_eq!(
            member(mapping, "/audioTrackIndexes"),
            case.audio_indexes.clone().unwrap_or(Value::Null),
            "case {:?}: expected the audio track indexes, got {mapping}",
            case.case
        );
        assert_eq!(
            member(mapping, "/dataTrackIndexes"),
            Value::Null,
            "case {:?}: no file here carries a data track, got {mapping}",
            case.case
        );
    }
}

#[tokio::test]
async fn one_object_probes_the_same_however_its_url_addresses_it() {
    let emulator = emulator_with_bucket().await;
    let key = "in/addressed.mp4";
    let bytes = iso_bmff(
        b"isom",
        Some(VideoTrack {
            width: 1_280,
            height: 720,
            timescale: 1_000,
            frames: 50,
            sample_duration: 20,
        }),
        None,
    );
    put_object(&emulator, key, &bytes).await;

    // The forms `FileUrl` documents for an S3 object: the URI, and the virtual-hosted and
    // path-style S3 endpoint URLs. A key percent-encoded in a URL addresses the same
    // object as the decoded one.
    let encoded_key = "in%2Faddressed.mp4";
    let urls = [
        s3_uri(key),
        format!("https://{BUCKET}.s3.{REGION}.amazonaws.com/{key}"),
        format!("https://{BUCKET}.s3.amazonaws.com/{key}"),
        format!("https://s3.{REGION}.amazonaws.com/{BUCKET}/{key}"),
        format!("https://s3.{REGION}.amazonaws.com/{BUCKET}/{encoded_key}"),
    ];
    let mut answers = Vec::new();
    for url in &urls {
        let reply = probe(&emulator, &[url]).await;
        assert_eq!(
            reply.status, 200,
            "case {url:?}: Probe must succeed, got {} {:?}",
            reply.status, reply.body
        );
        answers.push(only_result(&reply, url));
    }
    let first = answers[0].clone();
    assert_eq!(
        member(&first, "/container/format"),
        json!("mp4"),
        "case {:?}: expected the MP4 the bytes are, got {first}",
        urls[0]
    );
    for (url, answer) in urls.iter().zip(&answers) {
        assert_eq!(
            answer, &first,
            "case {url:?}: every form of one object's address must probe the same, \
             expected {first}, got {answer}"
        );
    }
}

#[tokio::test]
async fn a_probe_answers_one_result_per_file_in_request_order() {
    let emulator = emulator_with_bucket().await;
    let wave_bytes = wave(2, 44_100, 16, 4_410);
    let movie_bytes = iso_bmff(
        b"isom",
        Some(VideoTrack {
            width: 320,
            height: 240,
            timescale: 1_000,
            frames: 10,
            sample_duration: 100,
        }),
        None,
    );
    put_object(&emulator, "in/first.wav", &wave_bytes).await;
    put_object(&emulator, "in/second.mp4", &movie_bytes).await;

    // The same two files in both orders: the results follow the request, not the store.
    for (case, keys, formats) in [
        (
            "the Wave file first",
            ["in/first.wav", "in/second.mp4"],
            ["wave", "mp4"],
        ),
        (
            "the MP4 first",
            ["in/second.mp4", "in/first.wav"],
            ["mp4", "wave"],
        ),
    ] {
        let urls: Vec<String> = keys.iter().map(|key| s3_uri(key)).collect();
        let borrowed: Vec<&str> = urls.iter().map(String::as_str).collect();
        let reply = probe(&emulator, &borrowed).await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: Probe must succeed, got {} {:?}",
            reply.status, reply.body
        );
        let results = reply.at("/probeResults").as_array().cloned().unwrap_or_default();
        assert_eq!(
            results.len(),
            keys.len(),
            "case {case:?}: expected one result per input file, got {:?}",
            reply.body
        );
        for ((result, format), key) in results.iter().zip(formats).zip(keys) {
            assert_eq!(
                member(result, "/container/format"),
                json!(format),
                "case {case:?}: expected {key:?} to be probed as {format:?}, got {result}"
            );
        }
    }
}

#[tokio::test]
async fn a_file_that_cannot_be_probed_is_refused_for_why() {
    let emulator = emulator_with_bucket().await;
    put_object(&emulator, "in/not-media.txt", b"this is not a media file at all").await;
    // An MP4 with its signature cut away is bytes no container signature matches.
    let headless: Vec<u8> = iso_bmff(
        b"isom",
        Some(VideoTrack {
            width: 320,
            height: 240,
            timescale: 1_000,
            frames: 5,
            sample_duration: 200,
        }),
        None,
    )
    .split_off(64);
    put_object(&emulator, "in/headless.mp4", &headless).await;

    /// One refusal row: what the request asks for, and the status and error shape the
    /// service answers with.
    struct Refusal {
        case: &'static str,
        files: Value,
        status: u16,
        error_type: &'static str,
        /// The message the row pins, where the text is the requirement for its error
        /// class rather than a sentence this service words for the file it names.
        message: Option<&'static str>,
    }
    /// What a probe of a media file the object store holds nothing at answers: fixed text,
    /// interpolating nothing, because what is absent is an object of the caller's rather
    /// than a resource of this service's.
    const MISSING_FILE: &str = "The file does not exist.";
    let refusals = vec![
        Refusal {
            case: "a key the bucket does not hold",
            files: json!([{ "fileUrl": s3_uri("in/never-written.mp4") }]),
            status: 404,
            error_type: "NotFoundException",
            message: Some(MISSING_FILE),
        },
        Refusal {
            case: "a bucket this account does not hold",
            files: json!([{ "fileUrl": format!("s3://{ABSENT_BUCKET}/in/one.mp4") }]),
            status: 404,
            error_type: "NotFoundException",
            message: Some(MISSING_FILE),
        },
        Refusal {
            case: "a URL that addresses no object this emulator can read",
            files: json!([{ "fileUrl": "https://example.com/in/one.mp4" }]),
            status: 400,
            error_type: "BadRequestException",
            message: None,
        },
        Refusal {
            case: "an s3 URI with no key",
            files: json!([{ "fileUrl": format!("s3://{BUCKET}/") }]),
            status: 400,
            error_type: "BadRequestException",
            message: None,
        },
        Refusal {
            case: "a fileUrl that carries no characters",
            files: json!([{ "fileUrl": "" }]),
            status: 400,
            error_type: "BadRequestException",
            message: None,
        },
        Refusal {
            case: "no input file at all",
            files: json!([]),
            status: 400,
            error_type: "BadRequestException",
            message: None,
        },
        Refusal {
            case: "more files than one call reads",
            files: Value::Array(
                (0..=service_com_amazonaws_mediaconvert_mediaconvert::ops::probe::MAX_PROBED_FILES)
                    .map(|index| json!({ "fileUrl": s3_uri(&format!("in/bulk-{index}.mp4")) }))
                    .collect(),
            ),
            status: 400,
            error_type: "BadRequestException",
            message: None,
        },
        Refusal {
            case: "a file that is not media",
            files: json!([{ "fileUrl": s3_uri("in/not-media.txt") }]),
            status: 422,
            error_type: "UnprocessableEntityException",
            message: None,
        },
        Refusal {
            case: "a media file whose container signature is gone",
            files: json!([{ "fileUrl": s3_uri("in/headless.mp4") }]),
            status: 422,
            error_type: "UnprocessableEntityException",
            message: None,
        },
        Refusal {
            case: "one readable file beside one that is not",
            files: json!([
                { "fileUrl": s3_uri("in/not-media.txt") },
                { "fileUrl": s3_uri("in/never-written.mp4") },
            ]),
            status: 422,
            error_type: "UnprocessableEntityException",
            message: None,
        },
    ];
    for refusal in &refusals {
        let reply = emulator
            .send(
                "POST",
                "/2017-08-29/probe",
                Some(json!({ "inputFiles": refusal.files })),
            )
            .await;
        assert_eq!(
            reply.status, refusal.status,
            "case {:?}: expected HTTP {}, got {} {:?}",
            refusal.case, refusal.status, reply.status, reply.body
        );
        assert_eq!(
            reply.error_type.as_deref(),
            Some(refusal.error_type),
            "case {:?}: expected {}, got {:?}",
            refusal.case,
            refusal.error_type,
            reply.error_type
        );
        let message = reply.string("/message");
        assert!(
            !message.is_empty(),
            "case {:?}: a refusal must say what was wrong, got {:?}",
            refusal.case,
            reply.body
        );
        if let Some(expected) = refusal.message {
            assert_eq!(
                message, expected,
                "case {:?}: expected the message for that error class, got {message:?}",
                refusal.case
            );
        }
        // No result is reported for a request that was refused: a partial list would
        // claim the files it left out were never asked about.
        assert_eq!(
            reply.at("/probeResults"),
            &Value::Null,
            "case {:?}: a refused probe reports no result, got {:?}",
            refusal.case,
            reply.body
        );
    }
}

#[tokio::test]
async fn probing_a_file_changes_nothing_a_later_caller_sees() {
    let emulator = emulator_with_bucket().await;
    let key = "in/read-twice.wav";
    let bytes = wave(1, 16_000, 16, 8_000);
    put_object(&emulator, key, &bytes).await;
    let url = s3_uri(key);

    let first = probe(&emulator, &[&url]).await;
    assert_eq!(
        first.status, 200,
        "the first probe must succeed, got {} {:?}",
        first.status, first.body
    );
    let second = probe(&emulator, &[&url]).await;
    assert_eq!(
        second.body, first.body,
        "a second probe of one file must answer what the first did, expected {:?}, got {:?}",
        first.body, second.body
    );

    // The object itself is still there, byte for byte: the probe read it, it did not
    // consume it.
    let (status, _, served) = emulator
        .rest("s3", "GET", &format!("/{BUCKET}/{key}"), Vec::new())
        .await;
    assert_eq!(
        status, 200,
        "the probed object must still be readable, got {status}"
    );
    assert_eq!(
        served.len(),
        bytes.len(),
        "the probed object must be unchanged, expected {} bytes, got {}",
        bytes.len(),
        served.len()
    );
}
