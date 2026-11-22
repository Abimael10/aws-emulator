// SPDX-License-Identifier: CC0-1.0

//! The archived-media read loop, end to end over HTTP.
//!
//! Every test drives the real `MultiServiceRouter` with this one service
//! registered, so a request goes through routing, restJson1 deserialization,
//! constraint validation, the handler, and serialization — the same path a
//! socket-served request takes.
//!
//! The archive is SEEDED through the service's own state API (this service has
//! no create operation: streams and fragments are ingested through the Kinesis
//! Video Streams control/data plane), and every assertion below is about what
//! the handlers compute FROM that archive.

use bytes::Bytes;
use service_com_amazonaws_kinesisvideoarchivedmedia_awsacuityreader as archived_media;
use smithy_server_runtime::MultiServiceRouter;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use archived_media::state::{
    epoch_millis, from_epoch_millis, service_state, ArchivedStream, StoredFragment,
};

/// The region every request in this suite is signed for.
const REGION: &str = "us-east-1";
/// The endpoint every request in this suite addresses: this emulator, on
/// loopback. A session URL must come back here, never to a real AWS host.
pub(crate) const EMULATOR_ENDPOINT: &str = "127.0.0.1:9297";
/// The seeded stream with an archive and 24 hours of retention.
pub(crate) const STREAM: &str = "demo-stream";
/// A seeded stream with `DataRetentionInHours` of 0.
pub(crate) const NO_RETENTION_STREAM: &str = "no-retention-stream";
/// A seeded stream whose fragments were ingested just now.
pub(crate) const LIVE_STREAM: &str = "live-stream";
/// The base of the seeded range: an instant well in the past, so nothing in
/// this stream's archive is within the LIVE 30-second window.
pub(crate) const T0_SECS: i64 = 1_761_000_000;
/// How many fragments `STREAM` holds.
const SEEDED_FRAGMENTS: u64 = 5;
/// How many fragments `LIVE_STREAM` holds.
pub(crate) const LIVE_FRAGMENTS: u64 = 7;
/// A realistic Kinesis Video fragment number: a long, increasing decimal.
const FRAGMENT_BASE: u64 = 9_134_385_233_318_143_200;

/// The fragment number of the `index`-th seeded fragment.
pub(crate) fn fragment_number(index: u64) -> String {
    (FRAGMENT_BASE + index).to_string()
}

/// The media bytes of the `index`-th seeded fragment: one archived Matroska
/// fragment carrying [`FRAMES_PER_FRAGMENT`] of the fixture's coded pictures,
/// distinct per fragment, so a payload can only come out right if the right
/// fragments were picked in the right order.
pub(crate) fn media(index: u64) -> Vec<u8> {
    MkvFragment::of(first_unit(index), FRAMES_PER_FRAGMENT).bytes()
}

/// The access units the `index`-th seeded fragment carries, in order — what a
/// clip of that fragment has to put in its track.
fn media_units(index: u64) -> Vec<Vec<u8>> {
    access_units(first_unit(index), FRAMES_PER_FRAGMENT)
}

/// Where the `index`-th seeded fragment's pictures start in the fixture's coded
/// video.
fn first_unit(index: u64) -> usize {
    usize::try_from(index).expect("small index") * FRAMES_PER_FRAGMENT
}

// ---------------------------------------------------------------------------
// The archived fragment: a Matroska document carrying coded H.264.
// ---------------------------------------------------------------------------
//
// Kinesis Video Streams archives each fragment as a Matroska document of its own
// (the submitted header in front of one Cluster), so the seeded media is one. The
// element ids below are the Matroska specification's and the sizes are EBML
// variable-size integers (RFC 8794 section 4.4), written here in the test rather
// than by the code under test. The frames are really encoded H.264 and the
// track's `CodecPrivate` is really that encoding's decoder configuration record,
// so the clip's samples and its sample description are a codec's own rather than
// bytes shaped to look like one.

/// Matroska `EBML` — the document's header (level 0).
const ID_EBML: &[u8] = &[0x1A, 0x45, 0xDF, 0xA3];
/// Matroska `DocType`.
const ID_DOC_TYPE: &[u8] = &[0x42, 0x82];
/// Matroska `Segment` (level 0), the master element the media sits in.
const ID_SEGMENT: &[u8] = &[0x18, 0x53, 0x80, 0x67];
/// Matroska `Info`.
const ID_INFO: &[u8] = &[0x15, 0x49, 0xA9, 0x66];
/// Matroska `TimestampScale` (nanoseconds per timecode tick).
const ID_TIMESTAMP_SCALE: &[u8] = &[0x2A, 0xD7, 0xB1];
/// Matroska `Tracks`.
const ID_TRACKS: &[u8] = &[0x16, 0x54, 0xAE, 0x6B];
/// Matroska `TrackEntry`.
const ID_TRACK_ENTRY: &[u8] = &[0xAE];
/// Matroska `TrackNumber`.
const ID_TRACK_NUMBER: &[u8] = &[0xD7];
/// Matroska `TrackType`.
const ID_TRACK_TYPE: &[u8] = &[0x83];
/// Matroska `CodecID`.
const ID_CODEC_ID: &[u8] = &[0x86];
/// Matroska `CodecPrivate`.
const ID_CODEC_PRIVATE: &[u8] = &[0x63, 0xA2];
/// Matroska `Video` (the video settings of a `TrackEntry`).
const ID_VIDEO: &[u8] = &[0xE0];
/// Matroska `PixelWidth`.
const ID_PIXEL_WIDTH: &[u8] = &[0xB0];
/// Matroska `PixelHeight`.
const ID_PIXEL_HEIGHT: &[u8] = &[0xBA];
/// Matroska `Cluster`.
const ID_CLUSTER: &[u8] = &[0x1F, 0x43, 0xB6, 0x75];
/// Matroska `Timestamp` (a Cluster's own timecode).
const ID_TIMESTAMP: &[u8] = &[0xE7];
/// Matroska `SimpleBlock` (one frame).
const ID_SIMPLE_BLOCK: &[u8] = &[0xA3];

/// Matroska `TrackType` 1: a video track.
const TRACK_TYPE_VIDEO: u64 = 1;

/// The `SimpleBlock` flag that says the frame is a keyframe.
const KEYFRAME_FLAG: u8 = 0x80;

/// The default Matroska timecode scale: one millisecond per tick, in nanoseconds.
const MILLISECOND_SCALE_NANOS: u64 = 1_000_000;

/// The Matroska codec id of H.264, as the codec registry assigns it and the
/// Kinesis Video Streams producers write it.
pub(crate) const AVC_CODEC_ID: &str = "V_MPEG4/ISO/AVC";

/// How many pictures the fixture's coded video carries. More than one fragment
/// and more than one window of the clip's fragment ceiling need, so two
/// selections of the archive carry different pictures.
const CODED_PICTURES: usize = 16;

/// The width the fixture's pictures are coded at, in pixels.
const CODED_WIDTH: u32 = 64;
/// Their height, in pixels. Even sides, as the codec's chroma sampling needs.
const CODED_HEIGHT: u32 = 48;

/// How many coded pictures one seeded fragment carries.
const FRAMES_PER_FRAGMENT: usize = 2;

/// How far apart the frames of one fragment are shown, in the container's own
/// timecode ticks.
const FRAME_SPACING_TICKS: i16 = 33;

/// The fixture's coded video: the parameter sets of the track that carries it,
/// and one access unit per coded picture in the length-prefixed framing an MP4
/// track carries a sample in — the framing a Kinesis Video Streams producer
/// writes its frames in, so one fragment frame is one of these.
struct CodedVideo {
    seq_param_set: Vec<u8>,
    pic_param_set: Vec<u8>,
    access_units: Vec<Vec<u8>>,
}

/// One fixture picture: a bright bar whose place follows `index`, so two coded
/// pictures differ and a clip that carried the wrong one cannot pass.
fn fixture_picture(index: usize) -> Vec<u8> {
    let mut rgb = vec![16u8; (CODED_WIDTH * CODED_HEIGHT * 3) as usize];
    let column = (index as u32 * 4) % CODED_WIDTH;
    for row in 0..CODED_HEIGHT {
        for offset in 0..4u32 {
            let x = (column + offset) % CODED_WIDTH;
            let at = ((row * CODED_WIDTH + x) * 3) as usize;
            if let Some(pixel) = rgb.get_mut(at..at + 3) {
                pixel.copy_from_slice(&[235, 235, 235]);
            }
        }
    }
    rgb
}

/// The fixture's coded video, encoded ONCE for the whole suite.
fn coded_video() -> &'static CodedVideo {
    static CODED: std::sync::OnceLock<CodedVideo> = std::sync::OnceLock::new();
    CODED.get_or_init(|| {
        let mut encoder = ::openh264::encoder::Encoder::with_api_config(
            ::openh264::OpenH264API::from_source(),
            ::openh264::encoder::EncoderConfig::new(),
        )
        .expect("the fixture's encoder starts");
        let mut stream: Vec<u8> = Vec::new();
        for index in 0..CODED_PICTURES {
            let rgb = fixture_picture(index);
            let yuv = ::openh264::formats::YUVBuffer::from_rgb8_source(
                ::openh264::formats::RgbSliceU8::new(
                    &rgb,
                    (CODED_WIDTH as usize, CODED_HEIGHT as usize),
                ),
            );
            stream.extend_from_slice(
                &encoder
                    .encode(&yuv)
                    .expect("the fixture's picture encodes")
                    .to_vec(),
            );
        }
        let units = ::service_common::h264_mp4::annex_b_units(&stream);
        let access_units: Vec<Vec<u8>> = ::service_common::h264_mp4::avcc_samples(&units)
            .into_iter()
            .map(|sample| sample.bytes)
            .collect();
        assert_eq!(
            access_units.len(),
            CODED_PICTURES,
            "case the fixture's coded video: expected one access unit per picture, got {}",
            access_units.len()
        );
        CodedVideo {
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
            access_units,
        }
    })
}

/// `count` access units of the fixture's coded video from `first`, wrapping round
/// the pictures it carries.
fn access_units(first: usize, count: usize) -> Vec<Vec<u8>> {
    let coded = coded_video();
    (0..count)
        .map(|offset| coded.access_units[(first + offset) % CODED_PICTURES].clone())
        .collect()
}

/// The AVC decoder configuration record (ISO/IEC 14496-15) a track declares as
/// its `CodecPrivate`: the configuration version, the profile, its compatibility
/// and the level the sequence parameter set states, the four-byte length framing
/// of the units inside a frame, then the sequence and the picture parameter set,
/// each preceded by its own 16-bit big-endian length.
pub(crate) fn avc_record(seq_param_set: &[u8], pic_param_set: &[u8]) -> Vec<u8> {
    let mut record = vec![
        1,
        seq_param_set.get(1).copied().unwrap_or_default(),
        seq_param_set.get(2).copied().unwrap_or_default(),
        seq_param_set.get(3).copied().unwrap_or_default(),
        // `lengthSizeMinusOne` of 3: four-byte lengths, with the reserved bits set.
        0xFF,
        // One sequence parameter set, with the reserved bits set.
        0xE1,
    ];
    record.extend_from_slice(&u16::try_from(seq_param_set.len()).unwrap_or(0).to_be_bytes());
    record.extend_from_slice(seq_param_set);
    record.push(1);
    record.extend_from_slice(&u16::try_from(pic_param_set.len()).unwrap_or(0).to_be_bytes());
    record.extend_from_slice(pic_param_set);
    record
}

/// The record of the fixture's own coded video — the one an archived fragment of
/// that video declares.
pub(crate) fn fixture_record() -> Vec<u8> {
    let coded = coded_video();
    avc_record(&coded.seq_param_set, &coded.pic_param_set)
}

/// One archived fragment, as a Matroska document.
#[derive(Debug, Clone)]
pub(crate) struct MkvFragment {
    /// The `TrackNumber` the track is declared with and every frame is written on.
    pub(crate) track: u64,
    /// The track's `CodecID`.
    pub(crate) codec_id: String,
    /// Its `CodecPrivate`, or `None` for a track that declares none.
    pub(crate) codec_private: Option<Vec<u8>>,
    /// The `PixelWidth`/`PixelHeight` the container declares for the track, or
    /// `None` for a container that declares neither.
    pub(crate) pixel_size: Option<(u64, u64)>,
    /// `Info/TimestampScale`: nanoseconds per timecode tick.
    pub(crate) scale_nanos: u64,
    /// The Cluster's own `Timestamp`, in those ticks.
    pub(crate) timecode: u64,
    /// How far apart its frames are shown, in those ticks.
    pub(crate) spacing_ticks: i16,
    /// The frames the Cluster carries, in order.
    pub(crate) frames: Vec<Vec<u8>>,
}

impl MkvFragment {
    /// A fragment of the fixture's coded video: `count` access units from
    /// `first`, on track 1, declaring that video's own record and leaving the
    /// geometry to the coded video itself.
    pub(crate) fn of(first: usize, count: usize) -> Self {
        Self {
            track: 1,
            codec_id: AVC_CODEC_ID.to_string(),
            codec_private: Some(fixture_record()),
            pixel_size: None,
            scale_nanos: MILLISECOND_SCALE_NANOS,
            timecode: 0,
            spacing_ticks: FRAME_SPACING_TICKS,
            frames: access_units(first, count),
        }
    }

    /// The document's bytes.
    pub(crate) fn bytes(&self) -> Vec<u8> {
        let mut document = Vec::new();
        let mut header = Vec::new();
        push_ebml_element(&mut header, ID_DOC_TYPE, b"matroska");
        push_ebml_element(&mut document, ID_EBML, &header);

        let mut segment = Vec::new();
        let mut info = Vec::new();
        push_ebml_element(&mut info, ID_TIMESTAMP_SCALE, &uint_body(self.scale_nanos));
        push_ebml_element(&mut segment, ID_INFO, &info);

        let mut entry = Vec::new();
        push_ebml_element(&mut entry, ID_TRACK_NUMBER, &uint_body(self.track));
        push_ebml_element(&mut entry, ID_TRACK_TYPE, &uint_body(TRACK_TYPE_VIDEO));
        push_ebml_element(&mut entry, ID_CODEC_ID, self.codec_id.as_bytes());
        if let Some(private) = &self.codec_private {
            push_ebml_element(&mut entry, ID_CODEC_PRIVATE, private);
        }
        if let Some((width, height)) = self.pixel_size {
            let mut video = Vec::new();
            push_ebml_element(&mut video, ID_PIXEL_WIDTH, &uint_body(width));
            push_ebml_element(&mut video, ID_PIXEL_HEIGHT, &uint_body(height));
            push_ebml_element(&mut entry, ID_VIDEO, &video);
        }
        let mut tracks = Vec::new();
        push_ebml_element(&mut tracks, ID_TRACK_ENTRY, &entry);
        push_ebml_element(&mut segment, ID_TRACKS, &tracks);

        let mut cluster = Vec::new();
        push_ebml_element(&mut cluster, ID_TIMESTAMP, &uint_body(self.timecode));
        for (position, frame) in self.frames.iter().enumerate() {
            // A `SimpleBlock` body is the track number as a variable-size
            // integer, the frame's signed 16-bit offset from the Cluster
            // timecode, one flags byte, then the frame itself.
            let offset = self
                .spacing_ticks
                .saturating_mul(i16::try_from(position).expect("a small frame count"));
            let mut block = vec![0x80 | u8::try_from(self.track).expect("a small track number")];
            block.extend_from_slice(&offset.to_be_bytes());
            block.push(KEYFRAME_FLAG);
            block.extend_from_slice(frame);
            push_ebml_element(&mut cluster, ID_SIMPLE_BLOCK, &block);
        }
        push_ebml_element(&mut segment, ID_CLUSTER, &cluster);
        push_ebml_element(&mut document, ID_SEGMENT, &segment);
        document
    }
}

/// Append the EBML variable-size integer (RFC 8794 section 4.4) encoding `size`,
/// in the shortest width that holds it.
fn push_ebml_size(out: &mut Vec<u8>, size: u64) {
    for width in 1..=8u32 {
        let bits = 7 * width;
        // The all-ones data bits of a width are the "unknown size" encoding, so a
        // size that would need them goes in the next width up.
        let largest = (1u64 << bits) - 2;
        if size <= largest {
            let value = (1u64 << bits) | size;
            let bytes = value.to_be_bytes();
            out.extend_from_slice(&bytes[8 - width as usize..]);
            return;
        }
    }
    panic!("case an element body of {size} bytes: larger than the largest EBML size");
}

/// Append one EBML element: its id (itself already a variable-size integer), its
/// size, then `body`.
fn push_ebml_element(out: &mut Vec<u8>, id: &[u8], body: &[u8]) {
    out.extend_from_slice(id);
    push_ebml_size(out, body.len() as u64);
    out.extend_from_slice(body);
}

/// `value` as a Matroska unsigned-integer element body: big-endian, shortest form.
fn uint_body(value: u64) -> Vec<u8> {
    let bytes = value.to_be_bytes();
    let first = bytes.iter().position(|byte| *byte != 0).unwrap_or(7);
    bytes[first..].to_vec()
}

// ---------------------------------------------------------------------------
// The clip, read back as a client's demuxer reads it.
// ---------------------------------------------------------------------------

/// What the clip's video track carries, read through a maintained ISO base media
/// reader.
pub(crate) struct ClipTrack {
    /// The visible width the track states, in pixels.
    pub(crate) width: u16,
    /// Its visible height.
    pub(crate) height: u16,
    /// The sequence parameter set of its sample description.
    pub(crate) seq_param_set: Vec<u8>,
    /// Its picture parameter set.
    pub(crate) pic_param_set: Vec<u8>,
    /// Its samples, in decode order.
    pub(crate) samples: Vec<Vec<u8>>,
    /// Which of them a player may start decoding at.
    pub(crate) sync: Vec<bool>,
}

/// The clip's one video track, failing `case` when the bytes are not an MP4 file
/// carrying one.
pub(crate) fn clip_track(case: &str, clip: &[u8]) -> ClipTrack {
    let size = clip.len() as u64;
    let mut reader = ::mp4::Mp4Reader::read_header(std::io::Cursor::new(clip), size)
        .unwrap_or_else(|error| {
            panic!("case {case}: the clip must read as an MP4 file, got {error}")
        });
    let (id, width, height, seq_param_set, pic_param_set, count) = {
        let track = reader
            .tracks()
            .values()
            .find(|track| track.track_type().ok() == Some(::mp4::TrackType::Video))
            .unwrap_or_else(|| panic!("case {case}: the clip must carry a video track"));
        assert_eq!(
            track.media_type().ok(),
            Some(::mp4::MediaType::H264),
            "case {case}: the clip's video track must be H.264"
        );
        (
            track.track_id(),
            track.width(),
            track.height(),
            track
                .sequence_parameter_set()
                .unwrap_or_else(|error| {
                    panic!("case {case}: the track must state a sequence parameter set, got {error}")
                })
                .to_vec(),
            track
                .picture_parameter_set()
                .unwrap_or_else(|error| {
                    panic!("case {case}: the track must state a picture parameter set, got {error}")
                })
                .to_vec(),
            track.sample_count(),
        )
    };
    let mut samples = Vec::new();
    let mut sync = Vec::new();
    for sample_id in 1..=count {
        let sample = reader
            .read_sample(id, sample_id)
            .ok()
            .flatten()
            .unwrap_or_else(|| panic!("case {case}: sample {sample_id} must be readable"));
        samples.push(sample.bytes.to_vec());
        sync.push(sample.is_sync);
    }
    ClipTrack {
        width,
        height,
        seq_param_set,
        pic_param_set,
        samples,
        sync,
    }
}

/// The first bytes of an MP4 file are its File Type box: a four-byte big-endian
/// size, then the box type `ftyp` (ISO/IEC 14496-12). What a client probes before
/// handing a payload to a demuxer.
pub(crate) fn assert_is_mp4(case: &str, clip: &[u8]) {
    assert_eq!(
        clip.get(4..8),
        Some(b"ftyp".as_slice()),
        "case {case}: a clip must be an MP4 container, got {:?}",
        &clip[..clip.len().min(12)]
    );
}

/// The producer timestamp of the `index`-th seeded fragment.
fn producer_secs(index: u64) -> i64 {
    T0_SECS + 10 * i64::try_from(index).expect("small index")
}

/// The server timestamp of the `index`-th seeded fragment: an hour's worth of
/// offset from the producer timestamp, so selecting on one field cannot pass by
/// accident when the other was meant.
fn server_secs(index: u64) -> i64 {
    producer_secs(index) + 60
}

/// A seeded emulator: the router, the shared engine, and the seeded stream's ARN.
pub(crate) struct Harness {
    router: MultiServiceRouter,
    pub(crate) engine: Arc<engine::InMemoryEngine>,
    pub(crate) arn: String,
}

impl Harness {
    /// POST `body` to `path` through the router, signed for `REGION` so the
    /// request routes by its SigV4 credential scope, and addressed at a
    /// loopback endpoint — the emulator's own, never a real AWS host.
    pub(crate) async fn post(
        &self,
        path: &str,
        body: &serde_json::Value,
    ) -> http::Response<Bytes> {
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("https://{EMULATOR_ENDPOINT}{path}"))
            .header("host", EMULATOR_ENDPOINT)
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/kinesisvideo/\
                     aws4_request, SignedHeaders=host, Signature=deadbeef"
                ),
            )
            .body(Bytes::from(body.to_string()))
            .expect("a valid request");
        self.router.dispatch(request).await
    }
}

/// The JSON body of a response.
pub(crate) fn body_json(response: &http::Response<Bytes>) -> serde_json::Value {
    serde_json::from_slice(response.body()).unwrap_or(serde_json::Value::Null)
}

/// The `content-type` header of a response.
fn content_type(response: &http::Response<Bytes>) -> String {
    response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

/// A router serving this service alone over an engine seeded with three
/// streams: an archive in the past, one with no retention, and one ingested
/// just now.
pub(crate) fn seeded() -> Harness {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let arn = {
        let state = service_state(&*engine).expect("the service store");
        let mut state = state.lock();

        let created_at = from_epoch_millis(T0_SECS * 1000);
        state.put_stream(ArchivedStream::new(STREAM, REGION, created_at, 24));
        state.put_stream(ArchivedStream::new(
            NO_RETENTION_STREAM,
            REGION,
            created_at,
            0,
        ));
        state.put_stream(ArchivedStream::new(LIVE_STREAM, REGION, created_at, 24));

        // Appended out of ingestion order on purpose: the archive is kept in
        // ascending fragment-number order by the state API, not by the caller.
        for index in [2, 0, 4, 1, 3] {
            assert!(
                state.append_fragment(
                    STREAM,
                    StoredFragment::new(
                        fragment_number(index),
                        from_epoch_millis(producer_secs(index) * 1000),
                        from_epoch_millis(server_secs(index) * 1000),
                        2000,
                        media(index),
                    ),
                ),
                "case seed fragment {index}: the seeded stream must exist"
            );
        }

        let now = SystemTime::now();
        for index in 0..LIVE_FRAGMENTS {
            let ingested = now - Duration::from_secs(LIVE_FRAGMENTS - index);
            assert!(
                state.append_fragment(
                    LIVE_STREAM,
                    StoredFragment::new(
                        fragment_number(index),
                        ingested,
                        ingested,
                        1000,
                        media(index),
                    ),
                ),
                "case seed live fragment {index}: the live stream must exist"
            );
        }

        state
            .streams
            .get(STREAM)
            .expect("the seeded stream")
            .arn
            .clone()
    };

    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(archived_media::EmulatedService::new(Arc::clone(&engine)));
    Harness {
        router,
        engine,
        arn,
    }
}

/// `base` with every member of `extra` added.
pub(crate) fn with_members(
    base: &serde_json::Value,
    extra: &serde_json::Value,
) -> serde_json::Value {
    let mut object = base.as_object().cloned().unwrap_or_default();
    for (key, value) in extra.as_object().expect("an object of members") {
        object.insert(key.clone(), value.clone());
    }
    serde_json::Value::Object(object)
}

/// Every operation of the read loop, with a request body that succeeds for the
/// seeded stream once the stream-naming members are added.
pub(crate) fn read_loop_operations() -> Vec<(&'static str, serde_json::Value)> {
    vec![
        (
            "/listFragments",
            serde_json::json!({
                "FragmentSelector": {
                    "FragmentSelectorType": "PRODUCER_TIMESTAMP",
                    "TimestampRange": {
                        "StartTimestamp": T0_SECS,
                        "EndTimestamp": T0_SECS + 1000,
                    },
                },
            }),
        ),
        (
            "/getMediaForFragmentList",
            serde_json::json!({ "Fragments": [fragment_number(0)] }),
        ),
        (
            "/getClip",
            serde_json::json!({
                "ClipFragmentSelector": {
                    "FragmentSelectorType": "PRODUCER_TIMESTAMP",
                    "TimestampRange": {
                        "StartTimestamp": T0_SECS,
                        "EndTimestamp": T0_SECS + 1000,
                    },
                },
            }),
        ),
        (
            "/getHLSStreamingSessionURL",
            serde_json::json!({
                "PlaybackMode": "ON_DEMAND",
                "HLSFragmentSelector": {
                    "FragmentSelectorType": "PRODUCER_TIMESTAMP",
                    "TimestampRange": {
                        "StartTimestamp": T0_SECS,
                        "EndTimestamp": T0_SECS + 1000,
                    },
                },
            }),
        ),
        (
            "/getDASHStreamingSessionURL",
            serde_json::json!({
                "PlaybackMode": "ON_DEMAND",
                "DASHFragmentSelector": {
                    "FragmentSelectorType": "PRODUCER_TIMESTAMP",
                    "TimestampRange": {
                        "StartTimestamp": T0_SECS,
                        "EndTimestamp": T0_SECS + 1000,
                    },
                },
            }),
        ),
    ]
}

/// RULE: every archived-media operation resolves the stream the same way —
/// exactly one of `StreamName` and `StreamARN` must be present (neither and
/// both are `InvalidArgumentException`, 400), and a name or ARN that answers to
/// no stream is `ResourceNotFoundException` (404).
#[tokio::test]
async fn every_operation_resolves_the_stream_by_the_same_rule() {
    let harness = seeded();
    let cases: Vec<(&str, serde_json::Value, u16, &str)> = vec![
        (
            "StreamName only",
            serde_json::json!({ "StreamName": STREAM }),
            200,
            "",
        ),
        (
            "StreamARN only",
            serde_json::json!({ "StreamARN": harness.arn.clone() }),
            200,
            "",
        ),
        (
            "both StreamName and StreamARN",
            serde_json::json!({ "StreamName": STREAM, "StreamARN": harness.arn.clone() }),
            400,
            "InvalidArgumentException",
        ),
        (
            "neither StreamName nor StreamARN",
            serde_json::json!({}),
            400,
            "InvalidArgumentException",
        ),
        (
            "unknown StreamName",
            serde_json::json!({ "StreamName": "no-such-stream" }),
            404,
            "ResourceNotFoundException",
        ),
        (
            "unknown StreamARN",
            serde_json::json!({
                "StreamARN": "arn:aws:kinesisvideo:us-east-1:000000000000:stream/absent/1761000000"
            }),
            404,
            "ResourceNotFoundException",
        ),
    ];

    for (path, extra) in read_loop_operations() {
        for (case, stream, status, error) in &cases {
            let body = with_members(stream, &extra);
            let response = harness.post(path, &body).await;
            assert_eq!(
                response.status().as_u16(),
                *status,
                "case {path} / {case}: expected HTTP {status}, got {} with body {:?}",
                response.status(),
                String::from_utf8_lossy(response.body())
            );
            if !error.is_empty() {
                let reported = body_json(&response);
                assert_eq!(
                    reported["__type"], *error,
                    "case {path} / {case}: expected error {error}, got {reported:?}"
                );
            }
        }
    }
}

/// RULE: `ListFragments` returns the archived fragments whose SELECTED
/// timestamp field (`PRODUCER_TIMESTAMP` or `SERVER_TIMESTAMP`) falls inside
/// the inclusive `TimestampRange` — "This value is inclusive. The EndTimestamp
/// is compared to the (starting) timestamp of the fragment."
#[tokio::test]
async fn list_fragments_filters_on_the_selected_timestamp_field() {
    let harness = seeded();
    // (case, selector type, start offset, end offset, the fragment indexes the
    // archive holds in that window)
    let cases: Vec<(&str, &str, i64, i64, Vec<u64>)> = vec![
        ("producer, whole archive", "PRODUCER_TIMESTAMP", 0, 1000, vec![0, 1, 2, 3, 4]),
        ("producer, first three", "PRODUCER_TIMESTAMP", 0, 25, vec![0, 1, 2]),
        ("producer, one instant", "PRODUCER_TIMESTAMP", 10, 10, vec![1]),
        ("producer, before the archive", "PRODUCER_TIMESTAMP", -100, -1, vec![]),
        ("producer, inclusive end", "PRODUCER_TIMESTAMP", 20, 30, vec![2, 3]),
        // The same windows read on the server timestamps, which are offset by
        // a minute: the SAME range selects a different set.
        ("server, producer window", "SERVER_TIMESTAMP", 0, 25, vec![]),
        ("server, first three", "SERVER_TIMESTAMP", 60, 85, vec![0, 1, 2]),
        ("server, last one", "SERVER_TIMESTAMP", 100, 1000, vec![4]),
        ("server, whole archive", "SERVER_TIMESTAMP", 0, 1000, vec![0, 1, 2, 3, 4]),
    ];

    for (case, selector_type, start, end, expected) in cases {
        let body = serde_json::json!({
            "StreamName": STREAM,
            "FragmentSelector": {
                "FragmentSelectorType": selector_type,
                "TimestampRange": {
                    "StartTimestamp": T0_SECS + start,
                    "EndTimestamp": T0_SECS + end,
                },
            },
        });
        let response = harness.post("/listFragments", &body).await;
        assert_eq!(
            response.status().as_u16(),
            200,
            "case {case}: expected HTTP 200, got {} with body {:?}",
            response.status(),
            String::from_utf8_lossy(response.body())
        );
        let reported = body_json(&response);
        let numbers: Vec<String> = reported["Fragments"]
            .as_array()
            .expect("Fragments is a list")
            .iter()
            .map(|fragment| fragment["FragmentNumber"].as_str().unwrap_or("").to_string())
            .collect();
        let wanted: Vec<String> = expected.iter().copied().map(fragment_number).collect();
        assert_eq!(
            numbers, wanted,
            "case {case}: expected fragments {wanted:?}, got {numbers:?}"
        );
        assert!(
            reported["NextToken"].is_null(),
            "case {case}: a page that returned everything must carry no NextToken, got {:?}",
            reported["NextToken"]
        );
    }
}

/// RULE: each returned `Fragment` reports that fragment's own persisted
/// identity, size, timestamps and duration.
#[tokio::test]
async fn list_fragments_reports_each_fragment_from_the_archive() {
    let harness = seeded();
    let body = serde_json::json!({
        "StreamName": STREAM,
        "FragmentSelector": {
            "FragmentSelectorType": "PRODUCER_TIMESTAMP",
            "TimestampRange": { "StartTimestamp": T0_SECS, "EndTimestamp": T0_SECS + 1000 },
        },
    });
    let response = harness.post("/listFragments", &body).await;
    assert_eq!(response.status().as_u16(), 200, "expected HTTP 200");
    let reported = body_json(&response);
    let fragments = reported["Fragments"].as_array().expect("Fragments is a list");

    for index in 0..SEEDED_FRAGMENTS {
        let reported = &fragments[usize::try_from(index).expect("small index")];
        let case = format!("fragment {index}");
        assert_eq!(
            reported["FragmentNumber"].as_str().unwrap_or_default(),
            fragment_number(index),
            "case {case}: wrong FragmentNumber, got {reported:?}"
        );
        assert_eq!(
            reported["FragmentSizeInBytes"].as_i64().unwrap_or_default(),
            i64::try_from(media(index).len()).expect("a small fragment"),
            "case {case}: FragmentSizeInBytes must be the stored media's length, got {reported:?}"
        );
        assert_eq!(
            reported["ProducerTimestamp"].as_f64().unwrap_or_default(),
            producer_secs(index) as f64,
            "case {case}: wrong ProducerTimestamp, got {reported:?}"
        );
        assert_eq!(
            reported["ServerTimestamp"].as_f64().unwrap_or_default(),
            server_secs(index) as f64,
            "case {case}: wrong ServerTimestamp, got {reported:?}"
        );
        assert_eq!(
            reported["FragmentLengthInMilliseconds"]
                .as_i64()
                .unwrap_or_default(),
            2000,
            "case {case}: wrong FragmentLengthInMilliseconds, got {reported:?}"
        );
    }
}

/// RULE: pagination is a partition — whatever the page size, replaying the
/// returned `NextToken` continues where the previous page stopped, no fragment
/// repeats or goes missing, and the final page carries no token. The token also
/// carries the selector, which is why the model makes `FragmentSelector`
/// optional on a continuation.
#[tokio::test]
async fn list_fragments_pages_partition_the_matching_fragments() {
    let harness = seeded();
    let all: Vec<String> = (0..SEEDED_FRAGMENTS).map(fragment_number).collect();

    for page_size in 1..=(SEEDED_FRAGMENTS + 1) {
        let case = format!("MaxResults {page_size}");
        let mut seen: Vec<String> = Vec::new();
        let mut next_token: Option<String> = None;
        let mut pages: u64 = 0;

        loop {
            let body = match &next_token {
                // A continuation carries ONLY the token: no selector.
                Some(token) => serde_json::json!({
                    "StreamName": STREAM,
                    "MaxResults": page_size,
                    "NextToken": token,
                }),
                None => serde_json::json!({
                    "StreamName": STREAM,
                    "MaxResults": page_size,
                    "FragmentSelector": {
                        "FragmentSelectorType": "PRODUCER_TIMESTAMP",
                        "TimestampRange": {
                            "StartTimestamp": T0_SECS,
                            "EndTimestamp": T0_SECS + 1000,
                        },
                    },
                }),
            };
            let response = harness.post("/listFragments", &body).await;
            assert_eq!(
                response.status().as_u16(),
                200,
                "case {case}: expected HTTP 200, got {} with body {:?}",
                response.status(),
                String::from_utf8_lossy(response.body())
            );
            let reported = body_json(&response);
            let page: Vec<String> = reported["Fragments"]
                .as_array()
                .expect("Fragments is a list")
                .iter()
                .map(|fragment| fragment["FragmentNumber"].as_str().unwrap_or("").to_string())
                .collect();
            assert!(
                page.len() <= usize::try_from(page_size).expect("small page size"),
                "case {case}: a page returned {} fragments, more than MaxResults",
                page.len()
            );
            seen.extend(page);
            pages += 1;
            assert!(pages <= SEEDED_FRAGMENTS + 2, "case {case}: pagination did not terminate");

            match reported["NextToken"].as_str() {
                Some(token) => {
                    assert!(
                        token
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'='),
                        "case {case}: NextToken {token} is outside the modelled token pattern"
                    );
                    next_token = Some(token.to_string());
                }
                None => break,
            }
        }

        assert_eq!(
            seen, all,
            "case {case}: paging must yield every matching fragment exactly once, in order"
        );
    }
}

/// RULE: a request that carries neither `NextToken` nor `FragmentSelector`, or
/// a range whose end precedes its start, or a token issued for another stream,
/// is `InvalidArgumentException` (400).
#[tokio::test]
async fn list_fragments_refuses_a_selector_it_cannot_page_on() {
    let harness = seeded();
    let cases: Vec<(&str, serde_json::Value)> = vec![
        (
            "no selector and no token",
            serde_json::json!({ "StreamName": STREAM }),
        ),
        (
            "end before start",
            serde_json::json!({
                "StreamName": STREAM,
                "FragmentSelector": {
                    "FragmentSelectorType": "PRODUCER_TIMESTAMP",
                    "TimestampRange": {
                        "StartTimestamp": T0_SECS + 10,
                        "EndTimestamp": T0_SECS,
                    },
                },
            }),
        ),
        (
            "a token that is not one",
            serde_json::json!({ "StreamName": STREAM, "NextToken": "bm90YXRva2Vu" }),
        ),
    ];

    for (case, body) in cases {
        let response = harness.post("/listFragments", &body).await;
        assert_eq!(
            response.status().as_u16(),
            400,
            "case {case}: expected HTTP 400, got {} with body {:?}",
            response.status(),
            String::from_utf8_lossy(response.body())
        );
        let reported = body_json(&response);
        assert_eq!(
            reported["__type"], "InvalidArgumentException",
            "case {case}: expected InvalidArgumentException, got {reported:?}"
        );
    }
}

/// RULE: `GetMediaForFragmentList`'s payload is the stored media of exactly the
/// requested fragments, concatenated in the order the request lists them.
#[tokio::test]
async fn get_media_for_fragment_list_concatenates_exactly_what_was_asked_for() {
    let harness = seeded();
    // (case, the fragment indexes requested, in request order)
    let cases: Vec<(&str, Vec<u64>)> = vec![
        ("one fragment", vec![0]),
        ("two in archive order", vec![0, 1]),
        ("two in reverse order", vec![1, 0]),
        ("the same fragment twice", vec![2, 2]),
        ("a non-contiguous selection", vec![4, 1, 3]),
        ("the whole archive", vec![0, 1, 2, 3, 4]),
    ];

    for (case, indexes) in cases {
        let body = serde_json::json!({
            "StreamName": STREAM,
            "Fragments": indexes.iter().copied().map(fragment_number).collect::<Vec<_>>(),
        });
        let response = harness.post("/getMediaForFragmentList", &body).await;
        assert_eq!(
            response.status().as_u16(),
            200,
            "case {case}: expected HTTP 200, got {} with body {:?}",
            response.status(),
            String::from_utf8_lossy(response.body())
        );
        assert_eq!(
            content_type(&response),
            "video/webm",
            "case {case}: wrong ContentType header"
        );
        let expected: Vec<u8> = indexes.iter().copied().flat_map(media).collect();
        assert_eq!(
            response.body().as_ref(),
            expected.as_slice(),
            "case {case}: expected the concatenation of {indexes:?}, got {} bytes",
            response.body().len()
        );
    }
}

/// RULE: a fragment number the archive does not hold is
/// `InvalidArgumentException` (400) — never a short payload.
#[tokio::test]
async fn get_media_for_fragment_list_refuses_an_unarchived_fragment() {
    let harness = seeded();
    let cases: Vec<(&str, Vec<String>)> = vec![
        ("only an absent fragment", vec![fragment_number(99)]),
        (
            "an absent fragment beside a present one",
            vec![fragment_number(0), fragment_number(99)],
        ),
        (
            "a fragment of another stream's numbering",
            vec!["1".to_string()],
        ),
    ];

    for (case, fragments) in cases {
        let body = serde_json::json!({ "StreamName": STREAM, "Fragments": fragments });
        let response = harness.post("/getMediaForFragmentList", &body).await;
        assert_eq!(
            response.status().as_u16(),
            400,
            "case {case}: expected HTTP 400, got {} with body {:?}",
            response.status(),
            String::from_utf8_lossy(response.body())
        );
        let reported = body_json(&response);
        assert_eq!(
            reported["__type"], "InvalidArgumentException",
            "case {case}: expected InvalidArgumentException, got {reported:?}"
        );
    }
}

/// RULE: the bytes one response assembles are bounded, not just the number of
/// fragments a request may name — a list that names one large fragment
/// repeatedly is refused with the modelled limit error rather than
/// materialising N copies of it.
#[tokio::test]
async fn get_media_for_fragment_list_bounds_the_bytes_it_assembles() {
    use archived_media::state::MAX_PAYLOAD_BYTES;

    let harness = seeded();
    // One fragment just over half the ceiling, so TWO references to it exceed
    // it while one does not: the count is identical in both cases, only the
    // product differs.
    let large = fragment_number(SEEDED_FRAGMENTS + 1);
    {
        let state = service_state(&*harness.engine).expect("the service store");
        let mut state = state.lock();
        assert!(
            state.append_fragment(
                STREAM,
                StoredFragment::new(
                    large.clone(),
                    from_epoch_millis((T0_SECS + 500) * 1000),
                    from_epoch_millis((T0_SECS + 560) * 1000),
                    2000,
                    vec![b'z'; MAX_PAYLOAD_BYTES / 2 + 1],
                ),
            ),
            "case seed the large fragment: the seeded stream must exist"
        );
    }

    let cases: Vec<(&str, Vec<String>, u16)> = vec![
        ("one copy, under the ceiling", vec![large.clone()], 200),
        (
            "two copies, over the ceiling",
            vec![large.clone(), large.clone()],
            400,
        ),
    ];

    for (case, fragments, status) in cases {
        let body = serde_json::json!({ "StreamName": STREAM, "Fragments": fragments });
        let response = harness.post("/getMediaForFragmentList", &body).await;
        assert_eq!(
            response.status().as_u16(),
            status,
            "case {case}: expected HTTP {status}, got {}",
            response.status()
        );
        if status == 400 {
            let reported = body_json(&response);
            assert_eq!(
                reported["__type"], "ClientLimitExceededException",
                "case {case}: expected ClientLimitExceededException, got {reported:?}"
            );
        }
    }
}

/// RULE: `GetClip` answers `video/mp4` and an MP4 container muxed from exactly
/// the fragments the archive holds in the selected inclusive range — the frames
/// those fragments carry on their H.264 track become the samples of the clip's
/// one video track, in archive order, with that track's codec private data as the
/// sample description.
#[tokio::test]
async fn get_clip_muxes_the_fragments_the_archive_holds_in_range() {
    let harness = seeded();
    // (case, selector type, start offset, end offset, fragment indexes)
    let cases: Vec<(&str, &str, i64, i64, Vec<u64>)> = vec![
        ("producer, whole archive", "PRODUCER_TIMESTAMP", 0, 1000, vec![0, 1, 2, 3, 4]),
        ("producer, first two", "PRODUCER_TIMESTAMP", 0, 15, vec![0, 1]),
        ("producer, one fragment", "PRODUCER_TIMESTAMP", 30, 35, vec![3]),
        ("server, first three", "SERVER_TIMESTAMP", 60, 85, vec![0, 1, 2]),
        ("server, whole archive", "SERVER_TIMESTAMP", 0, 1000, vec![0, 1, 2, 3, 4]),
    ];

    for (case, selector_type, start, end, indexes) in cases {
        let body = serde_json::json!({
            "StreamName": STREAM,
            "ClipFragmentSelector": {
                "FragmentSelectorType": selector_type,
                "TimestampRange": {
                    "StartTimestamp": T0_SECS + start,
                    "EndTimestamp": T0_SECS + end,
                },
            },
        });
        let response = harness.post("/getClip", &body).await;
        assert_eq!(
            response.status().as_u16(),
            200,
            "case {case}: expected HTTP 200, got {} with body {:?}",
            response.status(),
            String::from_utf8_lossy(response.body())
        );
        assert_eq!(
            content_type(&response),
            "video/mp4",
            "case {case}: GetClip downloads an MP4 clip"
        );
        assert_is_mp4(case, response.body());
        let expected: Vec<Vec<u8>> = indexes.iter().copied().flat_map(media_units).collect();
        let track = clip_track(case, response.body());
        assert_eq!(
            track.samples,
            expected,
            "case {case}: expected the access units of the fragments {indexes:?} as the clip's \
             samples, got {} samples",
            track.samples.len()
        );
        assert_eq!(
            (track.width, track.height),
            (
                u16::try_from(CODED_WIDTH).expect("a short side"),
                u16::try_from(CODED_HEIGHT).expect("a short side")
            ),
            "case {case}: the clip's track must state the coded geometry, got {}x{}",
            track.width,
            track.height
        );
        assert_eq!(
            (track.seq_param_set, track.pic_param_set),
            (
                coded_video().seq_param_set.clone(),
                coded_video().pic_param_set.clone()
            ),
            "case {case}: the sample description must carry the track's own parameter sets"
        );
    }
}

/// RULE: a clip carries at most the documented number of fragments — "The
/// output will contain the first 100 MB or the first 200 fragments from the
/// specified start timestamp" — counted from the start of the range, so a range
/// holding more than that is TRUNCATED rather than refused or served whole.
#[tokio::test]
async fn get_clip_truncates_at_the_documented_fragment_ceiling() {
    /// The published ceiling this test pins, from the `GetClip$Payload` doc
    /// sentence quoted above.
    const MAX_CLIP_FRAGMENTS: usize = 200;
    /// A stream holding more fragments in range than one clip may carry.
    const DEEP_STREAM: &str = "deep-archive-stream";

    // One picture per fragment, taken from a different place in the fixture's
    // coded video, so the expected sample sequence pins WHICH fragments were
    // taken and in what order: a handler that returned the LAST 200 fragments of
    // the range, or any other 200, produces a different sequence of the same
    // length.
    let fragment = |index: usize| MkvFragment::of(index, 1);

    let harness = seeded();
    let held = MAX_CLIP_FRAGMENTS + 50;
    {
        let state = service_state(&*harness.engine).expect("the service store");
        let mut state = state.lock();
        state.put_stream(ArchivedStream::new(
            DEEP_STREAM,
            REGION,
            from_epoch_millis(T0_SECS * 1000),
            24,
        ));
        for index in 0..held {
            let at = from_epoch_millis((T0_SECS + index as i64) * 1000);
            assert!(
                state.append_fragment(
                    DEEP_STREAM,
                    StoredFragment::new(
                        fragment_number(index as u64),
                        at,
                        at,
                        1000,
                        fragment(index).bytes(),
                    ),
                ),
                "case seed fragment {index}: the deep stream must exist"
            );
        }
    }

    let body = serde_json::json!({
        "StreamName": DEEP_STREAM,
        "ClipFragmentSelector": {
            "FragmentSelectorType": "PRODUCER_TIMESTAMP",
            "TimestampRange": {
                "StartTimestamp": T0_SECS,
                "EndTimestamp": T0_SECS + held as i64,
            },
        },
    });
    let response = harness.post("/getClip", &body).await;
    assert_eq!(
        response.status().as_u16(),
        200,
        "case truncated clip: expected HTTP 200, got {} with body {:?}",
        response.status(),
        String::from_utf8_lossy(response.body())
    );
    assert_eq!(
        content_type(&response),
        "video/mp4",
        "case truncated clip: a truncated clip is still an MP4 clip"
    );
    assert_is_mp4("truncated clip", response.body());
    let track = clip_track("truncated clip", response.body());
    let expected: Vec<Vec<u8>> = (0..MAX_CLIP_FRAGMENTS)
        .flat_map(|index| fragment(index).frames)
        .collect();
    assert_eq!(
        track.samples.len(),
        expected.len(),
        "case truncated clip: a clip must stop at the first {MAX_CLIP_FRAGMENTS} fragments of the \
         range, got {} samples for an archive of {held} fragments in range",
        track.samples.len()
    );
    assert_eq!(
        track.samples, expected,
        "case truncated clip: a clip must carry the frames of the FIRST {MAX_CLIP_FRAGMENTS} \
         fragments of the range, in order"
    );

    // The same range read through ListFragments still reports every fragment:
    // the ceiling is GetClip's own output limit, not a limit on the archive.
    let listed = harness
        .post(
            "/listFragments",
            &serde_json::json!({
                "StreamName": DEEP_STREAM,
                "MaxResults": 1000,
                "FragmentSelector": {
                    "FragmentSelectorType": "PRODUCER_TIMESTAMP",
                    "TimestampRange": {
                        "StartTimestamp": T0_SECS,
                        "EndTimestamp": T0_SECS + held as i64,
                    },
                },
            }),
        )
        .await;
    assert_eq!(
        listed.status().as_u16(),
        200,
        "case the archive behind the clip: expected HTTP 200, got {}",
        listed.status()
    );
    assert_eq!(
        body_json(&listed)["Fragments"]
            .as_array()
            .map_or(0, Vec::len),
        held,
        "case the archive behind the clip: ListFragments must still report every fragment in range"
    );
}

/// RULE: a stream that retains no data is `NoDataRetentionException` (400) —
/// "Data retention must be greater than 0" — and a range the archive holds no
/// fragment in is `ResourceNotFoundException` (404).
#[tokio::test]
async fn get_clip_refuses_a_stream_or_range_it_cannot_clip() {
    let harness = seeded();
    let cases: Vec<(&str, &str, i64, i64, u16, &str)> = vec![
        (
            "a stream with no retention",
            NO_RETENTION_STREAM,
            0,
            1000,
            400,
            "NoDataRetentionException",
        ),
        (
            "a range before the archive",
            STREAM,
            -1000,
            -1,
            404,
            "ResourceNotFoundException",
        ),
        (
            "a range after the archive",
            STREAM,
            5000,
            6000,
            404,
            "ResourceNotFoundException",
        ),
        (
            "a range between two fragments",
            STREAM,
            1,
            9,
            404,
            "ResourceNotFoundException",
        ),
    ];

    for (case, stream, start, end, status, error) in cases {
        let body = serde_json::json!({
            "StreamName": stream,
            "ClipFragmentSelector": {
                "FragmentSelectorType": "PRODUCER_TIMESTAMP",
                "TimestampRange": {
                    "StartTimestamp": T0_SECS + start,
                    "EndTimestamp": T0_SECS + end,
                },
            },
        });
        let response = harness.post("/getClip", &body).await;
        assert_eq!(
            response.status().as_u16(),
            status,
            "case {case}: expected HTTP {status}, got {} with body {:?}",
            response.status(),
            String::from_utf8_lossy(response.body())
        );
        let reported = body_json(&response);
        assert_eq!(
            reported["__type"], error,
            "case {case}: expected {error}, got {reported:?}"
        );
    }
}

/// RULE: `GetHLSStreamingSessionURL` mints a session per call and RECORDS it —
/// its parameters, its expiry and the playlist snapshot it serves — so a second
/// call answers a different URL and both sessions exist in state.
#[tokio::test]
async fn get_hls_streaming_session_url_records_a_distinct_session_per_call() {
    let harness = seeded();
    let body = serde_json::json!({
        "StreamName": STREAM,
        "PlaybackMode": "ON_DEMAND",
        "Expires": 600,
        "HLSFragmentSelector": {
            "FragmentSelectorType": "PRODUCER_TIMESTAMP",
            "TimestampRange": { "StartTimestamp": T0_SECS, "EndTimestamp": T0_SECS + 1000 },
        },
    });

    let mut urls: Vec<String> = Vec::new();
    for call in 1..=2 {
        let response = harness.post("/getHLSStreamingSessionURL", &body).await;
        assert_eq!(
            response.status().as_u16(),
            200,
            "case call {call}: expected HTTP 200, got {} with body {:?}",
            response.status(),
            String::from_utf8_lossy(response.body())
        );
        let reported = body_json(&response);
        let url = reported["HLSStreamingSessionURL"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            url.starts_with(&format!("https://{EMULATOR_ENDPOINT}/hls/v1/")),
            "case call {call}: the URL must address the endpoint the caller reached this \
             emulator on, got {url:?}"
        );
        urls.push(url);
    }

    assert_ne!(
        urls[0], urls[1],
        "case two calls: each call must mint its own session, got {urls:?}"
    );

    let state = service_state(&*harness.engine).expect("the service store");
    let state = state.lock();
    assert_eq!(
        state.sessions.len(),
        2,
        "case two calls: both sessions must be recorded, got {:?}",
        state.sessions.keys().collect::<Vec<_>>()
    );
    for (token, session) in &state.sessions {
        assert!(
            urls.iter().any(|url| url.ends_with(token)),
            "case session {token}: no returned URL carries this session's token"
        );
        assert_eq!(
            session.stream_name, STREAM,
            "case session {token}: wrong stream, got {session:?}"
        );
        assert_eq!(
            session.expires_in_secs, 600,
            "case session {token}: Expires must be honoured, got {session:?}"
        );
        assert_eq!(
            epoch_millis(session.expires_at()) - epoch_millis(session.created_at),
            600 * 1000,
            "case session {token}: the expiry must be created_at + Expires, got {session:?}"
        );
        assert_eq!(
            session.fragment_numbers,
            (0..SEEDED_FRAGMENTS).map(fragment_number).collect::<Vec<_>>(),
            "case session {token}: the playlist snapshot must be the fragments in range"
        );
    }
}

/// One row of the `GetHLSStreamingSessionURL` defaults table: the case name,
/// the selector type, the requested `MaxMediaPlaylistFragmentResults`, the
/// `DiscontinuityMode` the defaults imply, and the playlist snapshot expected.
type HlsDefaultsRow = (&'static str, &'static str, Option<u64>, &'static str, Vec<u64>);

/// RULE: the session store is bounded at the site that grows it — a session
/// whose `Expires` window the clock has already closed is dropped when the next
/// session is recorded, and live sessions never exceed `MAX_STREAMING_SESSIONS`.
#[tokio::test]
async fn minting_a_session_bounds_the_session_store() {
    use archived_media::state::{
        MAX_STREAMING_SESSIONS, SessionProtocol, StreamingSession,
    };

    let harness = seeded();
    let body = serde_json::json!({
        "StreamName": STREAM,
        "PlaybackMode": "ON_DEMAND",
        "HLSFragmentSelector": {
            "FragmentSelectorType": "PRODUCER_TIMESTAMP",
            "TimestampRange": { "StartTimestamp": T0_SECS, "EndTimestamp": T0_SECS + 1000 },
        },
    });

    // A session recorded long enough ago that its own Expires window has
    // closed, beside one that is still live.
    let now = SystemTime::now();
    let session_at = |token: &str, created_at: SystemTime, expires_in_secs: u32| StreamingSession {
        token: token.to_string(),
        stream_name: STREAM.to_string(),
        protocol: SessionProtocol::Hls,
        playback_mode: "ON_DEMAND".to_string(),
        container_format: "FRAGMENTED_MP4".to_string(),
        discontinuity_mode: Some("NEVER".to_string()),
        display_fragment_timestamp: "NEVER".to_string(),
        display_fragment_number: None,
        fragment_numbers: Vec::new(),
        created_at,
        expires_in_secs,
    };
    {
        let state = service_state(&*harness.engine).expect("the service store");
        let mut state = state.lock();
        state.record_session(session_at("expired", now - Duration::from_secs(3600), 300));
        state.record_session(session_at("live", now, 43200));
    }

    let response = harness.post("/getHLSStreamingSessionURL", &body).await;
    assert_eq!(
        response.status().as_u16(),
        200,
        "expected HTTP 200, got {}",
        response.status()
    );

    {
        let state = service_state(&*harness.engine).expect("the service store");
        let state = state.lock();
        assert!(
            !state.sessions.contains_key("expired"),
            "case expired session: a session past its Expires window must be dropped, held {:?}",
            state.sessions.keys().collect::<Vec<_>>()
        );
        assert!(
            state.sessions.contains_key("live"),
            "case live session: a session inside its Expires window must be kept, held {:?}",
            state.sessions.keys().collect::<Vec<_>>()
        );
    }

    // And live sessions alone cannot grow the store without end: recording
    // more than the ceiling keeps the ceiling and drops the oldest first.
    {
        let state = service_state(&*harness.engine).expect("the service store");
        let mut state = state.lock();
        for ordinal in 0..(MAX_STREAMING_SESSIONS + 16) {
            let created_at = now + Duration::from_millis(ordinal as u64);
            state.record_session(session_at(&format!("live-{ordinal}"), created_at, 43200));
        }
        assert!(
            state.sessions.len() <= MAX_STREAMING_SESSIONS,
            "case over the ceiling: the store holds {} sessions, over the {MAX_STREAMING_SESSIONS} \
             ceiling",
            state.sessions.len()
        );
        assert!(
            !state.sessions.contains_key("live-0"),
            "case over the ceiling: the oldest session must be the one evicted"
        );
        assert!(
            state.sessions.contains_key(&format!("live-{}", MAX_STREAMING_SESSIONS + 15)),
            "case over the ceiling: the newest session must be kept"
        );
    }
}

/// RULE: the documented parameter defaults are what an unspecified member
/// means — `Expires` 300, `ContainerFormat` FRAGMENTED_MP4,
/// `DisplayFragmentTimestamp` NEVER, and `DiscontinuityMode` ALWAYS for
/// SERVER_TIMESTAMP / NEVER for PRODUCER_TIMESTAMP — and
/// `MaxMediaPlaylistFragmentResults` caps the playlist snapshot.
#[tokio::test]
async fn get_hls_streaming_session_url_applies_the_documented_defaults() {
    // (case, selector type, max results, expected discontinuity mode, snapshot)
    let cases: Vec<HlsDefaultsRow> = vec![
        (
            "producer timestamps, no cap",
            "PRODUCER_TIMESTAMP",
            None,
            "NEVER",
            vec![0, 1, 2, 3, 4],
        ),
        (
            "server timestamps, no cap",
            "SERVER_TIMESTAMP",
            None,
            "ALWAYS",
            vec![0, 1, 2, 3, 4],
        ),
        (
            "server timestamps, capped at two",
            "SERVER_TIMESTAMP",
            Some(2),
            "ALWAYS",
            vec![0, 1],
        ),
        (
            "producer timestamps, capped at one",
            "PRODUCER_TIMESTAMP",
            Some(1),
            "NEVER",
            vec![0],
        ),
    ];

    for (case, selector_type, max_results, discontinuity, snapshot) in cases {
        let harness = seeded();
        let mut body = serde_json::json!({
            "StreamName": STREAM,
            "PlaybackMode": "ON_DEMAND",
            "HLSFragmentSelector": {
                "FragmentSelectorType": selector_type,
                "TimestampRange": { "StartTimestamp": T0_SECS, "EndTimestamp": T0_SECS + 1000 },
            },
        });
        if let Some(max) = max_results {
            body["MaxMediaPlaylistFragmentResults"] = serde_json::json!(max);
        }
        let response = harness.post("/getHLSStreamingSessionURL", &body).await;
        assert_eq!(
            response.status().as_u16(),
            200,
            "case {case}: expected HTTP 200, got {} with body {:?}",
            response.status(),
            String::from_utf8_lossy(response.body())
        );

        let state = service_state(&*harness.engine).expect("the service store");
        let state = state.lock();
        let session = state
            .sessions
            .values()
            .next()
            .expect("the minted session is recorded");
        assert_eq!(
            session.expires_in_secs, 300,
            "case {case}: the default Expires is 300 seconds, got {session:?}"
        );
        assert_eq!(
            session.container_format, "FRAGMENTED_MP4",
            "case {case}: the default ContainerFormat is FRAGMENTED_MP4, got {session:?}"
        );
        assert_eq!(
            session.display_fragment_timestamp, "NEVER",
            "case {case}: the default DisplayFragmentTimestamp is NEVER, got {session:?}"
        );
        assert_eq!(
            session.discontinuity_mode.as_deref(),
            Some(discontinuity),
            "case {case}: wrong default DiscontinuityMode, got {session:?}"
        );
        assert_eq!(
            session.fragment_numbers,
            snapshot.iter().copied().map(fragment_number).collect::<Vec<_>>(),
            "case {case}: wrong playlist snapshot, got {session:?}"
        );
    }
}

/// RULE: a session that would serve nothing does not exist —
/// `ResourceNotFoundException` (404) for ON_DEMAND / LIVE_REPLAY with no
/// fragment in the requested range, and for LIVE with no fragment in the last
/// 30 seconds. A LIVE session over a freshly ingested archive is minted, and
/// serves the most recent fragments up to the maximum.
#[tokio::test]
async fn get_hls_streaming_session_url_needs_fragments_to_serve() {
    let harness = seeded();
    let empty_range = serde_json::json!({
        "StartTimestamp": T0_SECS - 1000,
        "EndTimestamp": T0_SECS - 1,
    });
    let cases: Vec<(&str, serde_json::Value, u16)> = vec![
        (
            "ON_DEMAND outside the archive",
            serde_json::json!({
                "StreamName": STREAM,
                "PlaybackMode": "ON_DEMAND",
                "HLSFragmentSelector": {
                    "FragmentSelectorType": "PRODUCER_TIMESTAMP",
                    "TimestampRange": empty_range,
                },
            }),
            404,
        ),
        (
            "LIVE_REPLAY outside the archive",
            serde_json::json!({
                "StreamName": STREAM,
                "PlaybackMode": "LIVE_REPLAY",
                "HLSFragmentSelector": {
                    "FragmentSelectorType": "PRODUCER_TIMESTAMP",
                    "TimestampRange": empty_range,
                },
            }),
            404,
        ),
        (
            "LIVE over an archive older than the live window",
            serde_json::json!({ "StreamName": STREAM, "PlaybackMode": "LIVE" }),
            404,
        ),
        (
            "LIVE over a freshly ingested archive",
            serde_json::json!({ "StreamName": LIVE_STREAM, "PlaybackMode": "LIVE" }),
            200,
        ),
    ];

    for (case, body, status) in cases {
        let response = harness.post("/getHLSStreamingSessionURL", &body).await;
        assert_eq!(
            response.status().as_u16(),
            status,
            "case {case}: expected HTTP {status}, got {} with body {:?}",
            response.status(),
            String::from_utf8_lossy(response.body())
        );
        if status == 404 {
            let reported = body_json(&response);
            assert_eq!(
                reported["__type"], "ResourceNotFoundException",
                "case {case}: expected ResourceNotFoundException, got {reported:?}"
            );
        }
    }

    // The LIVE session that was minted serves the newest fragments, capped by
    // the documented LIVE default of 5.
    let state = service_state(&*harness.engine).expect("the service store");
    let state = state.lock();
    let session = state
        .sessions
        .values()
        .find(|session| session.stream_name == LIVE_STREAM)
        .expect("the LIVE session is recorded");
    let newest: Vec<String> = ((LIVE_FRAGMENTS - 5)..LIVE_FRAGMENTS)
        .map(fragment_number)
        .collect();
    assert_eq!(
        session.fragment_numbers, newest,
        "case LIVE snapshot: a LIVE playlist carries the 5 most recent fragments, got {session:?}"
    );
}

/// RULE: the playback mode decides which selector a session may carry —
/// `HLSFragmentSelector` with a `TimestampRange` is required for ON_DEMAND and
/// LIVE_REPLAY, `EndTimestamp` is required for ON_DEMAND and optional for
/// LIVE_REPLAY, and a LIVE session must carry no `TimestampRange`.
#[tokio::test]
async fn get_hls_streaming_session_url_enforces_the_selector_rules_of_each_mode() {
    let harness = seeded();
    let cases: Vec<(&str, serde_json::Value, u16)> = vec![
        (
            "ON_DEMAND with no selector",
            serde_json::json!({ "StreamName": STREAM, "PlaybackMode": "ON_DEMAND" }),
            400,
        ),
        (
            "LIVE_REPLAY with no selector",
            serde_json::json!({ "StreamName": STREAM, "PlaybackMode": "LIVE_REPLAY" }),
            400,
        ),
        (
            "ON_DEMAND with no EndTimestamp",
            serde_json::json!({
                "StreamName": STREAM,
                "PlaybackMode": "ON_DEMAND",
                "HLSFragmentSelector": {
                    "FragmentSelectorType": "PRODUCER_TIMESTAMP",
                    "TimestampRange": { "StartTimestamp": T0_SECS },
                },
            }),
            400,
        ),
        (
            "LIVE_REPLAY with no EndTimestamp",
            serde_json::json!({
                "StreamName": STREAM,
                "PlaybackMode": "LIVE_REPLAY",
                "HLSFragmentSelector": {
                    "FragmentSelectorType": "PRODUCER_TIMESTAMP",
                    "TimestampRange": { "StartTimestamp": T0_SECS },
                },
            }),
            200,
        ),
        (
            "LIVE with a TimestampRange",
            serde_json::json!({
                "StreamName": STREAM,
                "PlaybackMode": "LIVE",
                "HLSFragmentSelector": {
                    "FragmentSelectorType": "SERVER_TIMESTAMP",
                    "TimestampRange": {
                        "StartTimestamp": T0_SECS,
                        "EndTimestamp": T0_SECS + 1000,
                    },
                },
            }),
            400,
        ),
        (
            "ON_DEMAND with an end that precedes the start",
            serde_json::json!({
                "StreamName": STREAM,
                "PlaybackMode": "ON_DEMAND",
                "HLSFragmentSelector": {
                    "FragmentSelectorType": "PRODUCER_TIMESTAMP",
                    "TimestampRange": {
                        "StartTimestamp": T0_SECS + 10,
                        "EndTimestamp": T0_SECS,
                    },
                },
            }),
            400,
        ),
    ];

    for (case, body, status) in cases {
        let response = harness.post("/getHLSStreamingSessionURL", &body).await;
        assert_eq!(
            response.status().as_u16(),
            status,
            "case {case}: expected HTTP {status}, got {} with body {:?}",
            response.status(),
            String::from_utf8_lossy(response.body())
        );
        if status == 400 {
            let reported = body_json(&response);
            assert_eq!(
                reported["__type"], "InvalidArgumentException",
                "case {case}: expected InvalidArgumentException, got {reported:?}"
            );
        }
    }
}

/// RULE: a stream that retains no data cannot have a streaming session —
/// `NoDataRetentionException` (400).
#[tokio::test]
async fn get_hls_streaming_session_url_refuses_a_stream_with_no_retention() {
    let harness = seeded();
    let body = serde_json::json!({
        "StreamName": NO_RETENTION_STREAM,
        "PlaybackMode": "ON_DEMAND",
        "HLSFragmentSelector": {
            "FragmentSelectorType": "PRODUCER_TIMESTAMP",
            "TimestampRange": { "StartTimestamp": T0_SECS, "EndTimestamp": T0_SECS + 1000 },
        },
    });
    let response = harness.post("/getHLSStreamingSessionURL", &body).await;
    assert_eq!(
        response.status().as_u16(),
        400,
        "expected HTTP 400, got {} with body {:?}",
        response.status(),
        String::from_utf8_lossy(response.body())
    );
    let reported = body_json(&response);
    assert_eq!(
        reported["__type"], "NoDataRetentionException",
        "expected NoDataRetentionException, got {reported:?}"
    );
}

/// Seed `stream` with one fragment per entry of `fragments`, ten seconds apart
/// from [`T0_SECS`], and answer the request body that clips the whole of it.
fn seeded_clip_request(
    harness: &Harness,
    stream: &str,
    fragments: &[Vec<u8>],
) -> serde_json::Value {
    {
        let state = service_state(&*harness.engine).expect("the service store");
        let mut state = state.lock();
        state.put_stream(ArchivedStream::new(
            stream,
            REGION,
            from_epoch_millis(T0_SECS * 1000),
            24,
        ));
        for (index, media) in fragments.iter().enumerate() {
            let at = from_epoch_millis((T0_SECS + 10 * index as i64) * 1000);
            assert!(
                state.append_fragment(
                    stream,
                    StoredFragment::new(
                        fragment_number(index as u64),
                        at,
                        at,
                        2000,
                        media.clone(),
                    ),
                ),
                "case seed {stream:?} fragment {index}: the stream must exist"
            );
        }
    }
    serde_json::json!({
        "StreamName": stream,
        "ClipFragmentSelector": {
            "FragmentSelectorType": "PRODUCER_TIMESTAMP",
            "TimestampRange": {
                "StartTimestamp": T0_SECS,
                "EndTimestamp": T0_SECS + 1000,
            },
        },
    })
}

/// One row of the mux table: the fragments an archive holds, and the track the
/// clip of them has to carry.
struct MuxRow {
    case: &'static str,
    fragments: Vec<MkvFragment>,
    /// The samples the clip's track must carry, in order.
    expected_samples: Vec<Vec<u8>>,
    /// The geometry the clip's track must state.
    geometry: (u16, u16),
    /// The parameter sets its sample description must carry.
    parameter_sets: (Vec<u8>, Vec<u8>),
}

/// RULE: whatever shape the archived Matroska fragments have, the clip is an MP4
/// file whose one video track carries their H.264 track's frames as its samples,
/// with that track's codec private data as the sample description and the coded
/// geometry it states.
///
/// The rows differ along what the rule generalises over: how many fragments the
/// range holds and how many frames each carries, which spelling of the H.264
/// codec id the track declares, which track number it is, where the geometry is
/// declared, the container's own timecode scale, and the framing the producer
/// wrote its units in.
#[tokio::test]
async fn a_clip_is_an_mp4_of_the_frames_the_archived_track_carries() {
    let coded = coded_video();
    let fixture_sets = || (coded.seq_param_set.clone(), coded.pic_param_set.clone());
    let coded_geometry = (
        u16::try_from(CODED_WIDTH).expect("a short side"),
        u16::try_from(CODED_HEIGHT).expect("a short side"),
    );

    // A track whose record carries parameter sets that are not readable H.264,
    // for the row where the CONTAINER is what declares the geometry.
    let opaque_seq: Vec<u8> = vec![0x67, 0x64, 0x00, 0x1F, 0x00, 0x00];
    let opaque_pic: Vec<u8> = vec![0x68, 0xEE, 0x3C, 0x80];

    // A producer that wrote its units DELIMITED rather than length-prefixed: the
    // same access units, as the Annex B byte stream (ISO/IEC 14496-10 Annex B).
    let delimited = |unit: &[u8]| {
        let mut stream = Vec::new();
        assert!(
            ::service_common::h264_mp4::annex_b_from_avcc(unit, 4, &mut stream),
            "case the delimited fixture: the access unit must re-frame"
        );
        stream
    };

    let mut rows: Vec<MuxRow> = vec![
        MuxRow {
            case: "one fragment of one frame",
            fragments: vec![MkvFragment::of(0, 1)],
            expected_samples: access_units(0, 1),
            geometry: coded_geometry,
            parameter_sets: fixture_sets(),
        },
        MuxRow {
            case: "one fragment of three frames",
            fragments: vec![MkvFragment::of(2, 3)],
            expected_samples: access_units(2, 3),
            geometry: coded_geometry,
            parameter_sets: fixture_sets(),
        },
        MuxRow {
            case: "three fragments of two frames each",
            fragments: vec![
                MkvFragment::of(0, 2),
                MkvFragment::of(2, 2),
                MkvFragment::of(4, 2),
            ],
            expected_samples: access_units(0, 6),
            geometry: coded_geometry,
            parameter_sets: fixture_sets(),
        },
        MuxRow {
            case: "the codec id spelling this operation's documentation prints",
            fragments: vec![MkvFragment {
                codec_id: "V_MPEG/ISO/AVC".to_string(),
                ..MkvFragment::of(5, 2)
            }],
            expected_samples: access_units(5, 2),
            geometry: coded_geometry,
            parameter_sets: fixture_sets(),
        },
        MuxRow {
            case: "a track number other than one",
            fragments: vec![MkvFragment {
                track: 3,
                ..MkvFragment::of(7, 2)
            }],
            expected_samples: access_units(7, 2),
            geometry: coded_geometry,
            parameter_sets: fixture_sets(),
        },
        MuxRow {
            case: "a container that declares the geometry as well",
            fragments: vec![MkvFragment {
                pixel_size: Some((u64::from(CODED_WIDTH), u64::from(CODED_HEIGHT))),
                ..MkvFragment::of(1, 2)
            }],
            expected_samples: access_units(1, 2),
            geometry: coded_geometry,
            parameter_sets: fixture_sets(),
        },
        MuxRow {
            case: "a record whose parameter sets are opaque, with the container's geometry",
            fragments: vec![MkvFragment {
                codec_private: Some(avc_record(&opaque_seq, &opaque_pic)),
                pixel_size: Some((320, 240)),
                ..MkvFragment::of(3, 2)
            }],
            expected_samples: access_units(3, 2),
            geometry: (320, 240),
            parameter_sets: (opaque_seq.clone(), opaque_pic.clone()),
        },
        MuxRow {
            case: "a hundred-microsecond timecode scale",
            fragments: vec![MkvFragment {
                scale_nanos: 100_000,
                timecode: 4_000,
                spacing_ticks: 330,
                ..MkvFragment::of(6, 3)
            }],
            expected_samples: access_units(6, 3),
            geometry: coded_geometry,
            parameter_sets: fixture_sets(),
        },
        MuxRow {
            case: "frames the producer wrote as a delimited byte stream",
            fragments: vec![MkvFragment {
                frames: access_units(8, 2)
                    .iter()
                    .map(|unit| delimited(unit))
                    .collect(),
                ..MkvFragment::of(8, 2)
            }],
            expected_samples: access_units(8, 2),
            geometry: coded_geometry,
            parameter_sets: fixture_sets(),
        },
    ];

    let harness = seeded();
    for (index, row) in rows.drain(..).enumerate() {
        let stream = format!("mux-probe-{index}");
        let media: Vec<Vec<u8>> = row
            .fragments
            .iter()
            .map(MkvFragment::bytes)
            .collect();
        let body = seeded_clip_request(&harness, &stream, &media);
        let response = harness.post("/getClip", &body).await;
        let case = row.case;
        assert_eq!(
            response.status().as_u16(),
            200,
            "case {case}: expected HTTP 200, got {} with body {:?}",
            response.status(),
            String::from_utf8_lossy(response.body())
        );
        assert_eq!(
            content_type(&response),
            "video/mp4",
            "case {case}: GetClip downloads an MP4 clip"
        );
        assert_is_mp4(case, response.body());
        let track = clip_track(case, response.body());
        assert_eq!(
            track.samples,
            row.expected_samples,
            "case {case}: expected the archived frames as the clip's samples, got {} samples",
            track.samples.len()
        );
        assert_eq!(
            (track.width, track.height),
            row.geometry,
            "case {case}: expected the track to state {:?}, got {}x{}",
            row.geometry,
            track.width,
            track.height
        );
        assert_eq!(
            (track.seq_param_set, track.pic_param_set),
            row.parameter_sets,
            "case {case}: the sample description must carry the track's own parameter sets"
        );
        assert_eq!(
            track.sync.first(),
            Some(&true),
            "case {case}: a frame the producer marked a keyframe must be a sync sample, got {:?}",
            track.sync
        );
    }
}

/// RULE: the mux has preconditions, and each is the modelled refusal for it — a
/// track that declares no codec private data, codec private data that is not a
/// decoder configuration record, media whose codec ids name no H.264 video track,
/// and fragments whose frames cannot be read at all.
///
/// The rows differ along which precondition the archived media breaks, and along
/// which track and which fragment of the range breaks it.
#[tokio::test]
async fn get_clip_refuses_archived_media_it_cannot_mux() {
    // (case, the media the archive holds, the code, the message)
    let rows: Vec<(&str, Vec<Vec<u8>>, &str, String)> = vec![
        (
            "a track that declares no codec private data",
            vec![MkvFragment {
                codec_private: None,
                ..MkvFragment::of(0, 2)
            }
            .bytes()],
            "MissingCodecPrivateDataException",
            "Missing codec private data in fragment for track 1.".to_string(),
        ),
        (
            "another track that declares none",
            vec![MkvFragment {
                track: 2,
                codec_private: None,
                ..MkvFragment::of(0, 2)
            }
            .bytes()],
            "MissingCodecPrivateDataException",
            "Missing codec private data in fragment for track 2.".to_string(),
        ),
        (
            "codec private data of zero length",
            vec![MkvFragment {
                codec_private: Some(Vec::new()),
                ..MkvFragment::of(0, 2)
            }
            .bytes()],
            "MissingCodecPrivateDataException",
            "Missing codec private data in fragment for track 1.".to_string(),
        ),
        (
            "a later fragment of the range that declares none",
            vec![
                MkvFragment::of(0, 2).bytes(),
                MkvFragment {
                    codec_private: None,
                    ..MkvFragment::of(2, 2)
                }
                .bytes(),
            ],
            "MissingCodecPrivateDataException",
            "Missing codec private data in fragment for track 1.".to_string(),
        ),
        (
            "codec private data that is not a record at all",
            vec![MkvFragment {
                codec_private: Some(b"not-a-decoder-configuration-record".to_vec()),
                ..MkvFragment::of(0, 2)
            }
            .bytes()],
            "InvalidCodecPrivateDataException",
            "Invalid codec private data in fragment for track 1.".to_string(),
        ),
        (
            "a record of another configuration version",
            vec![MkvFragment {
                codec_private: Some({
                    let mut record = fixture_record();
                    record[0] = 2;
                    record
                }),
                ..MkvFragment::of(0, 2)
            }
            .bytes()],
            "InvalidCodecPrivateDataException",
            "Invalid codec private data in fragment for track 1.".to_string(),
        ),
        (
            "a record whose parameter set runs past its own bytes",
            vec![MkvFragment {
                codec_private: Some(fixture_record()[..8].to_vec()),
                ..MkvFragment::of(0, 2)
            }
            .bytes()],
            "InvalidCodecPrivateDataException",
            "Invalid codec private data in fragment for track 1.".to_string(),
        ),
        (
            "a record whose sequence parameter set states no profile",
            vec![MkvFragment {
                codec_private: Some(avc_record(&[0x67, 0x64], &[0x68, 0xEE, 0x3C, 0x80])),
                pixel_size: Some((320, 240)),
                ..MkvFragment::of(0, 2)
            }
            .bytes()],
            "InvalidCodecPrivateDataException",
            "Invalid codec private data in fragment for track 1.".to_string(),
        ),
        (
            "a record stating neither a readable parameter set nor a geometry",
            vec![MkvFragment {
                codec_private: Some(avc_record(
                    &[0x67, 0x64, 0x00, 0x1F, 0x00, 0x00],
                    &[0x68, 0xEE, 0x3C, 0x80],
                )),
                ..MkvFragment::of(0, 2)
            }
            .bytes()],
            "InvalidCodecPrivateDataException",
            "Invalid codec private data in fragment for track 1.".to_string(),
        ),
        (
            "an audio codec id",
            vec![MkvFragment {
                codec_id: "A_AAC".to_string(),
                ..MkvFragment::of(0, 2)
            }
            .bytes()],
            "UnsupportedStreamMediaTypeException",
            UNSUPPORTED_MEDIA_TYPE.to_string(),
        ),
        (
            "a video codec id that is not H.264",
            vec![MkvFragment {
                codec_id: "V_VP9".to_string(),
                ..MkvFragment::of(0, 2)
            }
            .bytes()],
            "UnsupportedStreamMediaTypeException",
            UNSUPPORTED_MEDIA_TYPE.to_string(),
        ),
        (
            "media that is not a Matroska document",
            vec![b"this is not a matroska document".to_vec()],
            "InvalidMediaFrameException",
            INVALID_MEDIA_FRAME.to_string(),
        ),
        (
            "a fragment that carries no frame",
            vec![MkvFragment {
                frames: Vec::new(),
                ..MkvFragment::of(0, 2)
            }
            .bytes()],
            "InvalidMediaFrameException",
            INVALID_MEDIA_FRAME.to_string(),
        ),
    ];

    let harness = seeded();
    for (index, (case, media, code, message)) in rows.into_iter().enumerate() {
        let stream = format!("refusal-probe-{index}");
        let body = seeded_clip_request(&harness, &stream, &media);
        let response = harness.post("/getClip", &body).await;
        assert_eq!(
            response.status().as_u16(),
            400,
            "case {case}: expected HTTP 400, got {} with body {:?}",
            response.status(),
            String::from_utf8_lossy(response.body())
        );
        let reported = body_json(&response);
        assert_eq!(
            reported["__type"], code,
            "case {case}: expected {code}, got {reported:?}"
        );
        assert_eq!(
            reported["Message"].as_str().unwrap_or_default(),
            message,
            "case {case}: expected {message:?}, got {reported:?}"
        );
    }
}

/// The sentence a clip of media whose codec ids name no H.264 video track is
/// refused with: the modelled description of `UnsupportedStreamMediaTypeException`.
const UNSUPPORTED_MEDIA_TYPE: &str =
    "The type of the media (for example, h.264 or h.265 video or ACC or G.711 audio) could not \
     be determined from the codec IDs of the tracks in the first fragment for a playback session.";

/// The sentence a clip of media whose frames cannot be read is refused with: the
/// modelled description of `InvalidMediaFrameException`.
const INVALID_MEDIA_FRAME: &str =
    "One or more frames in the requested clip could not be parsed based on the specified codec.";

/// Every operation of this service is implemented, so none of them answers
/// 501: `GetDASHStreamingSessionURL`, the last scaffold, is exercised in depth
/// by the `get_dash_streaming_session_url` suite.
#[tokio::test]
async fn no_operation_of_this_service_answers_not_implemented() {
    let harness = seeded();
    let named = serde_json::json!({ "StreamName": STREAM });

    for (path, body) in read_loop_operations() {
        let response = harness.post(path, &with_members(&body, &named)).await;
        assert_eq!(
            response.status().as_u16(),
            200,
            "case {path}: expected HTTP 200, got {} with body {:?}",
            response.status(),
            String::from_utf8_lossy(response.body())
        );
    }
}
