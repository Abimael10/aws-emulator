// SPDX-License-Identifier: CC0-1.0

//! `GetCaptureData`'s OUTPUT FORMAT, end to end over the same `MultiServiceRouter` the
//! emulator boots with.
//!
//! The rule these suites pin: `formatSettings` carries the output format configuration for
//! video processing, so the media a read answers with is PROCESSED to the rate and the
//! geometry it asks for — the pictures the asked rate samples out of the stored span, each
//! drawn the asked number of pixels across — over the same span the answered segment
//! covers, with every target the settings leave out taken from the media itself. Settings
//! that ask for no processing (naming no target, or naming the format the media is already
//! in) are answered with the stored media, media no picture can be taken out of is too, and
//! an output past what one read renders is refused rather than silently cut down.
//!
//! The table below spans the three members, each alone and together, at a lower and a
//! higher rate, at a smaller and a larger geometry, at the smallest geometry the model
//! admits and at an odd one, plus the identity cases and the refusals. What the answers are
//! checked with is the maintained container reader and the maintained H.264 decoder, so
//! what is asserted is what a player would find in the file: how many pictures it carries,
//! how often it shows them, what geometry it states and what geometry its pictures really
//! decode at.
//!
//! The fixture clip is ENCODED here, with the same maintained encoder and MP4 writer the
//! emulator renders with, because a producer's `mp4` file is real coded video and nothing
//! else can stand in for one; it is the suite's INPUT, never its oracle.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_iotsitewise_awsiotsitewise::ops::types::TimeInNanos;
use service_com_amazonaws_iotsitewise_awsiotsitewise::state::{
    service_state, CaptureSegment, MAX_RENDERED_FRAMES, MAX_RENDERED_FRAME_PIXELS,
    MAX_RENDERED_PIXELS, MAX_RENDERED_SIDE,
};

use crate::asset_lifecycle::{call, error_code, router_with_engine};

/// The workspace these suites read from.
const WORKSPACE: &str = "capture-format";

/// The seeded source's `TimeSeriesId` — a 36-character value, inside the modelled bound of
/// 36..73 (`smithy_query(query="shape", shape="TimeSeriesId")`).
const SOURCE_ID: &str = "41d0a7c6-5b33-4f8e-9a21-7c6d05be3f14";

/// The `VideoDataType` the seeded media is in. Source: `smithy_query(query="shape",
/// shape="VideoDataType")` — the enum holds exactly this one value.
const DATA_TYPE: &str = "VIDEO-MP4";

/// The geometry the fixture clip is coded at.
const SOURCE_WIDTH: u32 = 64;
/// ... and its height.
const SOURCE_HEIGHT: u32 = 48;
/// How many pictures the fixture clip carries.
const SOURCE_FRAMES: u32 = 8;
/// How many of them it shows per second, so the clip runs for two seconds.
const SOURCE_RATE: u32 = 4;

/// When the seeded segment's video begins.
const START_SECONDS: i64 = 1_700_000_000;
/// ... and when it ends: the two seconds the fixture clip runs for.
const END_SECONDS: i64 = 1_700_000_002;

/// `POST /workspaces/{workspaceName}/get-capture-data`, as `OPERATIONS.md` reports the
/// route.
fn capture_uri(workspace: &str) -> String {
    format!("/workspaces/{workspace}/get-capture-data")
}

/// A modelled `TimeInNanos` on the wire.
fn instant(seconds: i64, nanos: i32) -> Value {
    json!({ "timeInSeconds": seconds, "offsetInNanos": nanos })
}

fn time_in_nanos(seconds: i64, nanos: i32) -> TimeInNanos {
    TimeInNanos {
        time_in_seconds: seconds,
        offset_in_nanos: Some(nanos),
    }
}

/// Standard base64 with padding — the encoding restJson1 binds a blob member to, from the
/// runtime's own encoder.
fn base64(bytes: &[u8]) -> String {
    ::aws_smithy_types::base64::encode(bytes)
}

/// The bytes of an answer's `data` member.
fn media_of(body: &Value) -> Vec<u8> {
    let encoded = body
        .get("data")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("an answer must carry its data, got {body}"));
    ::aws_smithy_types::base64::decode(encoded).expect("the data member is base64")
}

// ---------------------------------------------------------------------------
// The fixture clip
// ---------------------------------------------------------------------------

/// One fixture picture: a bright bar whose place follows `index`, so two pictures of the
/// clip differ and a rendered answer that showed the wrong one cannot pass.
fn fixture_picture(width: u32, height: u32, index: u32) -> Vec<u8> {
    let mut rgb = vec![16u8; (width * height * 3) as usize];
    let column = (index * 6) % width;
    for row in 0..height {
        for offset in 0..6u32 {
            let x = (column + offset) % width;
            let at = ((row * width + x) * 3) as usize;
            if let Some(pixel) = rgb.get_mut(at..at + 3) {
                pixel.copy_from_slice(&[235, 235, 235]);
            }
        }
    }
    rgb
}

/// The fixture clip: `frames` coded pictures of `width` by `height`, shown `rate` times a
/// second, in an MP4 file — the kind of file a bulk import of an `mp4` object brings in.
fn fixture_clip(width: u32, height: u32, frames: u32, rate: u32) -> Vec<u8> {
    let mut encoder = ::openh264::encoder::Encoder::with_api_config(
        ::openh264::OpenH264API::from_source(),
        ::openh264::encoder::EncoderConfig::new()
            .rate_control_mode(::openh264::encoder::RateControlMode::Quality)
            .max_frame_rate(::openh264::encoder::FrameRate::from_hz(rate as f32)),
    )
    .expect("the fixture's encoder starts");
    let mut stream = Vec::new();
    for index in 0..frames {
        let rgb = fixture_picture(width, height, index);
        let yuv = ::openh264::formats::YUVBuffer::from_rgb8_source(
            ::openh264::formats::RgbSliceU8::new(&rgb, (width as usize, height as usize)),
        );
        stream.extend_from_slice(
            &encoder
                .encode(&yuv)
                .expect("the fixture's picture encodes")
                .to_vec(),
        );
    }
    let units = ::service_common::h264_mp4::annex_b_units(&stream);
    let track = ::service_common::h264_mp4::VideoTrack {
        width: u16::try_from(width).expect("a short side"),
        height: u16::try_from(height).expect("a short side"),
        timescale: rate,
        seq_param_set: ::service_common::h264_mp4::parameter_set(
            &units,
            ::service_common::h264_mp4::SEQUENCE_PARAMETER_SET,
        )
        .expect("the fixture carries a sequence parameter set"),
        pic_param_set: ::service_common::h264_mp4::parameter_set(
            &units,
            ::service_common::h264_mp4::PICTURE_PARAMETER_SET,
        )
        .expect("the fixture carries a picture parameter set"),
    };
    let timed: Vec<::service_common::h264_mp4::TimedSample> =
        ::service_common::h264_mp4::avcc_samples(&units)
            .into_iter()
            .map(|sample| ::service_common::h264_mp4::TimedSample {
                sample,
                duration: 1,
            })
            .collect();
    ::service_common::h264_mp4::mux_avc(&track, &timed).expect("the fixture muxes")
}

/// A container with a movie header and no track at all: video a duration can be read off
/// and no picture taken out of, hand-written from the box layout ISO/IEC 14496-12 defines.
fn header_only_clip(timescale: u32, duration: u32) -> Vec<u8> {
    fn media_box(name: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(8 + body.len());
        let size = u32::try_from(8 + body.len()).expect("a short box");
        bytes.extend_from_slice(&size.to_be_bytes());
        bytes.extend_from_slice(name);
        bytes.extend_from_slice(body);
        bytes
    }
    let mut header = vec![0u8; 4]; // version 0 and no flags
    header.extend_from_slice(&0u32.to_be_bytes()); // creation time
    header.extend_from_slice(&0u32.to_be_bytes()); // modification time
    header.extend_from_slice(&timescale.to_be_bytes());
    header.extend_from_slice(&duration.to_be_bytes());
    header.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // rate 1.0
    header.extend_from_slice(&0x0100u16.to_be_bytes()); // volume 1.0
    header.extend_from_slice(&0u16.to_be_bytes()); // reserved
    header.extend_from_slice(&[0u8; 8]); // reserved
    for value in [
        0x0001_0000u32, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000,
    ] {
        header.extend_from_slice(&value.to_be_bytes()); // the unity matrix
    }
    header.extend_from_slice(&[0u8; 24]); // pre-defined
    header.extend_from_slice(&1u32.to_be_bytes()); // next track id
    let mut brands = Vec::new();
    brands.extend_from_slice(b"isom");
    brands.extend_from_slice(&0u32.to_be_bytes());
    brands.extend_from_slice(b"isom");
    brands.extend_from_slice(b"mp41");
    let mut file = media_box(b"ftyp", &brands);
    file.extend_from_slice(&media_box(b"moov", &media_box(b"mvhd", &header)));
    file.extend_from_slice(&media_box(b"mdat", b"no coded picture here"));
    file
}

// ---------------------------------------------------------------------------
// What an answer's media states about itself
// ---------------------------------------------------------------------------

/// What a file states about its video: the geometry its track declares, how many pictures
/// it carries and how often it shows them — read with the maintained container reader.
#[derive(Debug, PartialEq)]
struct Carried {
    width: u16,
    height: u16,
    frames: u32,
    frames_per_second: u32,
}

fn carried(file: &[u8]) -> Carried {
    let size = u64::try_from(file.len()).expect("a short file");
    let reader = ::mp4::Mp4Reader::read_header(std::io::Cursor::new(file), size)
        .expect("an answer must be a readable container");
    let track = reader
        .tracks()
        .values()
        .find(|track| track.track_type().ok() == Some(::mp4::TrackType::Video))
        .expect("an answer must carry a video track");
    Carried {
        width: track.width(),
        height: track.height(),
        frames: track.sample_count(),
        frames_per_second: track.frame_rate().round() as u32,
    }
}

/// The geometry the pictures of a file really DECODE at, with the maintained H.264 decoder:
/// what says the pixels were resampled rather than the container relabelled.
fn decoded_geometry(file: &[u8]) -> (usize, usize) {
    use ::openh264::formats::YUVSource;

    let size = u64::try_from(file.len()).expect("a short file");
    let mut reader = ::mp4::Mp4Reader::read_header(std::io::Cursor::new(file), size)
        .expect("an answer must be a readable container");
    let (track_id, sets, samples) = {
        let track = reader
            .tracks()
            .values()
            .find(|track| track.track_type().ok() == Some(::mp4::TrackType::Video))
            .expect("an answer must carry a video track");
        let mut sets = Vec::new();
        for set in [
            track.sequence_parameter_set().expect("a sequence parameter set"),
            track.picture_parameter_set().expect("a picture parameter set"),
        ] {
            sets.extend_from_slice(&[0, 0, 0, 1]);
            sets.extend_from_slice(set);
        }
        (track.track_id(), sets, track.sample_count())
    };
    let mut decoder = ::openh264::decoder::Decoder::with_api_config(
        ::openh264::OpenH264API::from_source(),
        ::openh264::decoder::DecoderConfig::new(),
    )
    .expect("the reading decoder starts");
    for sample_id in 1..=samples {
        let Ok(Some(sample)) = reader.read_sample(track_id, sample_id) else {
            continue;
        };
        let mut packet = sets.clone();
        let mut at = 0usize;
        while at + 4 <= sample.bytes.len() {
            let length = u32::from_be_bytes([
                sample.bytes[at],
                sample.bytes[at + 1],
                sample.bytes[at + 2],
                sample.bytes[at + 3],
            ]) as usize;
            at += 4;
            let end = (at + length).min(sample.bytes.len());
            packet.extend_from_slice(&[0, 0, 0, 1]);
            packet.extend_from_slice(&sample.bytes[at..end]);
            at = end;
        }
        if let Ok(Some(picture)) = decoder.decode(&packet) {
            return picture.dimensions();
        }
    }
    panic!("an answer must carry a decodable picture");
}

// ---------------------------------------------------------------------------
// The harness
// ---------------------------------------------------------------------------

/// Create the workspace the segments are seeded in, over the API.
async fn create_workspace(router: &MultiServiceRouter, name: &str) {
    let (status, body) = call(
        router,
        "POST",
        "/workspaces",
        Some(json!({
            "workspaceName": name,
            "encryptionConfiguration": { "encryptionType": "SITEWISE_DEFAULT_ENCRYPTION" },
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup {name:?}: expected the workspace create to be accepted, got {status} {body}"
    );
}

/// Seed one segment on the `(workspace, timeSeriesId)` capture source, through the producer
/// entry point on this service's state — the one writer of a workspace's video.
fn seed(
    engine: &Arc<engine::InMemoryEngine>,
    workspace: &str,
    span: (i64, i64),
    media: Vec<u8>,
) {
    let state = service_state(&**engine).expect("the IoT SiteWise store");
    let mut store = state.lock();
    let stored = store.put_capture_segment(
        workspace,
        SOURCE_ID,
        None,
        DATA_TYPE,
        CaptureSegment {
            start_time: time_in_nanos(span.0, 0),
            end_time: time_in_nanos(span.1, 0),
            data: media,
        },
        std::time::SystemTime::now(),
    );
    assert_eq!(
        stored,
        Ok(()),
        "seeding {workspace}: the segment at {} was refused by the producer entry point",
        span.0
    );
}

/// A router holding the workspace and one seeded segment carrying `media`.
async fn seeded(media: Vec<u8>) -> MultiServiceRouter {
    let (router, engine) = router_with_engine();
    create_workspace(&router, WORKSPACE).await;
    seed(&engine, WORKSPACE, (START_SECONDS, END_SECONDS), media);
    router
}

/// One `GetCaptureData` over the whole seeded span, with `format` as its `formatSettings`.
async fn read_capture(router: &MultiServiceRouter, format: Option<Value>) -> (u16, Value) {
    let mut body = json!({
        "timeSeriesId": SOURCE_ID,
        "startTime": instant(START_SECONDS, 0),
        "endTime": instant(END_SECONDS, 0),
    });
    if let Some(format) = format {
        body.as_object_mut()
            .expect("an object body")
            .insert("formatSettings".to_string(), format);
    }
    call(router, "POST", &capture_uri(WORKSPACE), Some(body)).await
}

// ---------------------------------------------------------------------------
// The rule
// ---------------------------------------------------------------------------

/// One row of the format table: what the read asked for, and what the answer's media then
/// carries.
struct FormatRow {
    case: &'static str,
    format: Value,
    expected: Carried,
}

/// The answer's media carries the asked rate and the asked geometry, over the span the
/// answered segment covers, with every target the settings leave out taken from the media.
#[tokio::test]
async fn an_asked_output_format_is_what_the_answer_carries() {
    let stored = fixture_clip(SOURCE_WIDTH, SOURCE_HEIGHT, SOURCE_FRAMES, SOURCE_RATE);
    let router = seeded(stored.clone()).await;
    let rows = [
        FormatRow {
            case: "half the rate the media shows",
            format: json!({ "framesPerSecond": 2 }),
            expected: Carried {
                width: 64,
                height: 48,
                frames: 4,
                frames_per_second: 2,
            },
        },
        FormatRow {
            case: "twice that rate",
            format: json!({ "framesPerSecond": 8 }),
            expected: Carried {
                width: 64,
                height: 48,
                frames: 16,
                frames_per_second: 8,
            },
        },
        FormatRow {
            case: "a smaller geometry, at the media's own rate",
            format: json!({ "widthInPixels": 32, "heightInPixels": 24 }),
            expected: Carried {
                width: 32,
                height: 24,
                frames: 8,
                frames_per_second: 4,
            },
        },
        FormatRow {
            case: "a larger geometry and a lower rate together",
            format: json!({ "framesPerSecond": 1, "widthInPixels": 96, "heightInPixels": 72 }),
            expected: Carried {
                width: 96,
                height: 72,
                frames: 2,
                frames_per_second: 1,
            },
        },
        FormatRow {
            case: "a width alone, which keeps the height the media has",
            format: json!({ "widthInPixels": 16 }),
            expected: Carried {
                width: 16,
                height: 48,
                frames: 8,
                frames_per_second: 4,
            },
        },
        FormatRow {
            case: "an odd geometry",
            format: json!({ "widthInPixels": 33, "heightInPixels": 25 }),
            expected: Carried {
                width: 33,
                height: 25,
                frames: 8,
                frames_per_second: 4,
            },
        },
        FormatRow {
            // The smallest the model admits: all three members target
            // `PositiveInteger`, range 1.. (`smithy_query(query="shape",
            // shape="FormatSettings")`).
            case: "the smallest rate and geometry the model admits",
            format: json!({ "framesPerSecond": 1, "widthInPixels": 1, "heightInPixels": 1 }),
            expected: Carried {
                width: 1,
                height: 1,
                frames: 2,
                frames_per_second: 1,
            },
        },
    ];
    for row in rows {
        let (status, body) = read_capture(&router, Some(row.format.clone())).await;
        assert_eq!(
            status, 200,
            "case {}: expected the read to be served, got {status} {body}",
            row.case
        );
        let media = media_of(&body);
        assert_ne!(
            base64(&media),
            base64(&stored),
            "case {}: a processed answer must not be the stored bytes",
            row.case
        );
        let actual = carried(&media);
        assert_eq!(
            actual, row.expected,
            "case {}: expected the answer to carry {:?}, got {actual:?}",
            row.case, row.expected
        );
        // The span is the SEGMENT's: processing changes the media, not when it was
        // recorded.
        assert_eq!(
            (body.get("startTime"), body.get("endTime")),
            (
                Some(&instant(START_SECONDS, 0)),
                Some(&instant(END_SECONDS, 0))
            ),
            "case {}: expected the segment's own span, got {body}",
            row.case
        );
        assert_eq!(
            body.get("dataType").and_then(Value::as_str),
            Some(DATA_TYPE),
            "case {}: expected the stored data type, got {body}",
            row.case
        );
        assert!(
            body.get("nextToken").is_none(),
            "case {}: one segment is one answer, so no token resumes it, got {body}",
            row.case
        );
    }
}

/// The pixels are really resampled: the answer's pictures DECODE at the asked geometry.
#[tokio::test]
async fn a_processed_answer_decodes_at_the_asked_geometry() {
    let router = seeded(fixture_clip(
        SOURCE_WIDTH,
        SOURCE_HEIGHT,
        SOURCE_FRAMES,
        SOURCE_RATE,
    ))
    .await;
    for (case, width, height) in [
        ("half the geometry", 32u32, 24u32),
        ("twice the geometry", 128, 96),
    ] {
        let (status, body) = read_capture(
            &router,
            Some(json!({ "widthInPixels": width, "heightInPixels": height })),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: expected the read to be served, got {status} {body}"
        );
        let decoded = decoded_geometry(&media_of(&body));
        assert_eq!(
            decoded,
            (width as usize, height as usize),
            "case {case}: expected pictures decoding at {width}x{height}, got {decoded:?}"
        );
    }
}

/// A read that asks for no processing is answered with the stored media itself, and so is a
/// read of media no picture can be taken out of.
#[tokio::test]
async fn a_read_that_asks_for_no_processing_is_answered_as_stored() {
    let stored = fixture_clip(SOURCE_WIDTH, SOURCE_HEIGHT, SOURCE_FRAMES, SOURCE_RATE);
    let router = seeded(stored.clone()).await;
    let rows: [(&str, Option<Value>); 5] = [
        ("no format settings at all", None),
        ("format settings naming no target", Some(json!({}))),
        (
            "the rate the media already shows",
            Some(json!({ "framesPerSecond": SOURCE_RATE })),
        ),
        (
            "the geometry the media already has",
            Some(json!({ "widthInPixels": SOURCE_WIDTH, "heightInPixels": SOURCE_HEIGHT })),
        ),
        (
            "the whole format the media is already in",
            Some(json!({
                "framesPerSecond": SOURCE_RATE,
                "widthInPixels": SOURCE_WIDTH,
                "heightInPixels": SOURCE_HEIGHT,
            })),
        ),
    ];
    for (case, format) in rows {
        let (status, body) = read_capture(&router, format).await;
        assert_eq!(
            status, 200,
            "case {case}: expected the read to be served, got {status} {body}"
        );
        assert_eq!(
            base64(&media_of(&body)),
            base64(&stored),
            "case {case}: expected the stored media itself"
        );
    }

    // Media that states a duration and carries no coded picture: there is nothing to
    // process, and no picture is invented for it.
    let opaque = header_only_clip(1_000, 2_000);
    let router = seeded(opaque.clone()).await;
    let (status, body) = read_capture(
        &router,
        Some(json!({ "framesPerSecond": 1, "widthInPixels": 16, "heightInPixels": 16 })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case media with no coded picture: expected the read to be served, got {status} {body}"
    );
    assert_eq!(
        base64(&media_of(&body)),
        base64(&opaque),
        "case media with no coded picture: expected the stored media itself"
    );
}

/// An output past what one read renders is REFUSED — the honest answer for work this
/// emulator will not do — rather than served cut down to something else.
#[tokio::test]
async fn an_output_past_the_rendering_budget_is_refused() {
    let router = seeded(fixture_clip(
        SOURCE_WIDTH,
        SOURCE_HEIGHT,
        SOURCE_FRAMES,
        SOURCE_RATE,
    ))
    .await;
    let rows = [
        (
            "more frames than one read renders",
            json!({ "framesPerSecond": MAX_RENDERED_FRAMES }),
            format!("{MAX_RENDERED_FRAMES} frames"),
        ),
        (
            "a frame larger than one read renders",
            json!({ "widthInPixels": 4_000, "heightInPixels": 4_000 }),
            format!("{MAX_RENDERED_FRAME_PIXELS} pixels in each frame"),
        ),
        (
            "a side the answer could not state",
            json!({ "widthInPixels": MAX_RENDERED_SIDE + 1, "heightInPixels": 8 }),
            format!("{MAX_RENDERED_SIDE} pixels in each direction"),
        ),
        (
            "more pixels in total than one read renders",
            json!({ "framesPerSecond": 100, "widthInPixels": 1_920, "heightInPixels": 1_080 }),
            format!("{MAX_RENDERED_PIXELS} pixels for each request"),
        ),
    ];
    for (case, format, expected) in rows {
        let (status, body) = read_capture(&router, Some(format)).await;
        assert_eq!(
            status, 400,
            "case {case}: expected the read to be refused, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidRequestException",
            "case {case}: expected the modelled bad-request error, got {body}"
        );
        let message = body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert!(
            message.contains(&expected),
            "case {case}: expected a refusal naming {expected:?}, got {message:?}"
        );
    }
}

/// Stored media that carries MORE pictures than one read decodes is refused the same way,
/// naming the ceiling — not served unprocessed, which would report the asked format as
/// applied where nothing applied it. A read of that same media asking for no processing is
/// still served the stored bytes.
#[tokio::test]
async fn media_past_what_one_read_decodes_is_refused_only_when_processing_is_asked() {
    // One picture more than the ceiling allows, each of them tiny, so it is the COUNT that
    // is past it and not the pixels.
    let stored = fixture_clip(16, 16, MAX_RENDERED_FRAMES + 1, SOURCE_RATE);
    let router = seeded(stored.clone()).await;

    let (status, body) = read_capture(&router, Some(json!({ "framesPerSecond": 2 }))).await;
    assert_eq!(
        status, 400,
        "a read asking for processing of media past the decode ceiling must be refused, got \
         {status} {body}"
    );
    assert_eq!(
        error_code(&body),
        "InvalidRequestException",
        "expected the modelled bad-request error, got {body}"
    );
    let message = body
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let expected = format!("{MAX_RENDERED_FRAMES} frames");
    assert!(
        message.contains(&expected),
        "expected a refusal naming {expected:?}, got {message:?}"
    );

    let (status, body) = read_capture(&router, None).await;
    assert_eq!(
        status, 200,
        "a read asking for no processing must still be served, got {status} {body}"
    );
    assert_eq!(
        base64(&media_of(&body)),
        base64(&stored),
        "a read asking for no processing must answer with the stored media itself"
    );
}

/// A count below one is no count at all: the model bounds every member of the settings at
/// one, so such a request is refused rather than served as if it had asked for nothing.
#[tokio::test]
async fn a_target_below_the_modelled_floor_is_refused() {
    let router = seeded(fixture_clip(
        SOURCE_WIDTH,
        SOURCE_HEIGHT,
        SOURCE_FRAMES,
        SOURCE_RATE,
    ))
    .await;
    for (case, format) in [
        ("a rate of none", json!({ "framesPerSecond": 0 })),
        ("a width of none", json!({ "widthInPixels": 0 })),
        ("a negative height", json!({ "heightInPixels": -48 })),
    ] {
        let (status, body) = read_capture(&router, Some(format)).await;
        assert_eq!(
            status, 400,
            "case {case}: expected the read to be refused, got {status} {body}"
        );
    }
}

/// What the ASK alone is refused for, before any media is read: a rate or a side past what
/// this emulator renders, and the pixels of one frame at the geometry a request names both
/// sides of.
#[tokio::test]
async fn an_ask_no_media_could_render_is_refused_on_the_request_alone() {
    let router = seeded(fixture_clip(
        SOURCE_WIDTH,
        SOURCE_HEIGHT,
        SOURCE_FRAMES,
        SOURCE_RATE,
    ))
    .await;
    let rows = [
        (
            "a rate past every frame one read renders",
            json!({ "framesPerSecond": MAX_RENDERED_FRAMES + 1 }),
            format!("{MAX_RENDERED_FRAMES} frames"),
        ),
        (
            "a rate no second could carry",
            json!({ "framesPerSecond": i32::MAX }),
            format!("{MAX_RENDERED_FRAMES} frames"),
        ),
        (
            "a height past what the answer could state",
            json!({ "heightInPixels": MAX_RENDERED_SIDE + 1 }),
            format!("{MAX_RENDERED_SIDE} pixels in each direction"),
        ),
        (
            "a geometry past the pixels one frame holds",
            json!({ "widthInPixels": 4_096, "heightInPixels": 4_096 }),
            format!("{MAX_RENDERED_FRAME_PIXELS} pixels in each frame"),
        ),
    ];
    for (case, format, expected) in rows {
        let (status, body) = read_capture(&router, Some(format)).await;
        assert_eq!(
            status, 400,
            "case {case}: expected the read to be refused, got {status} {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidRequestException",
            "case {case}: expected the modelled bad-request error, got {body}"
        );
        let message = body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert!(
            message.contains(&expected),
            "case {case}: expected a refusal naming {expected:?}, got {message:?}"
        );
    }
}

/// Processing is per ANSWER, so a window carrying several segments renders each page at the
/// asked format and still resumes at the next segment.
#[tokio::test]
async fn every_page_of_a_read_carries_the_asked_format() {
    let (router, engine) = router_with_engine();
    create_workspace(&router, WORKSPACE).await;
    // Two back-to-back segments, the second a longer clip, so the pages differ in what
    // they carry as well as in which segment they are.
    seed(
        &engine,
        WORKSPACE,
        (START_SECONDS, END_SECONDS),
        fixture_clip(SOURCE_WIDTH, SOURCE_HEIGHT, SOURCE_FRAMES, SOURCE_RATE),
    );
    seed(
        &engine,
        WORKSPACE,
        (END_SECONDS, END_SECONDS + 4),
        fixture_clip(32, 32, 8, 2),
    );
    let format = json!({ "framesPerSecond": 2, "widthInPixels": 32, "heightInPixels": 24 });
    let mut body = json!({
        "timeSeriesId": SOURCE_ID,
        "startTime": instant(START_SECONDS, 0),
        "endTime": instant(END_SECONDS + 4, 0),
        "formatSettings": format,
    });
    let expected = [
        Carried {
            width: 32,
            height: 24,
            frames: 4,
            frames_per_second: 2,
        },
        Carried {
            width: 32,
            height: 24,
            frames: 8,
            frames_per_second: 2,
        },
    ];
    for (page, expected) in expected.into_iter().enumerate() {
        let (status, answer) = call(&router, "POST", &capture_uri(WORKSPACE), Some(body.clone())).await;
        assert_eq!(
            status, 200,
            "page {page}: expected the read to be served, got {status} {answer}"
        );
        let actual = carried(&media_of(&answer));
        assert_eq!(
            actual, expected,
            "page {page}: expected the answer to carry {expected:?}, got {actual:?}"
        );
        match answer.get("nextToken").and_then(Value::as_str) {
            Some(token) => {
                body.as_object_mut()
                    .expect("an object body")
                    .insert("nextToken".to_string(), json!(token));
            }
            None => {
                assert_eq!(page, 1, "the read must page both segments, stopped at {page}");
            }
        }
    }
}
