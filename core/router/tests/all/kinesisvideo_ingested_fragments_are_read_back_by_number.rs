// SPDX-License-Identifier: CC0-1.0

//! A fragment the ingest plane ACKNOWLEDGED is the stream's media on every read plane of
//! that stream, addressed by the fragment number the ingest minted for it.
//!
//! The rule under test: **`POST /putMedia` persists one fragment per submitted Matroska
//! Cluster, mints that fragment's number and reports it in the session's acknowledgement
//! stream, and the read planes of the same stream then report exactly those fragments —
//! `ListFragments` the minted numbers (none before the first session), `GetMedia` under
//! `FRAGMENT_NUMBER`/`AfterFragmentNumber` the fragments AFTER the one named ("Start with
//! the chunk after a specific fragment number"), and `GetMediaForFragmentList` the named
//! fragments and no others — with each fragment reported at the instant the session's
//! timecode type names** (`RELATIVE`: the producer start timestamp plus the Cluster
//! timecode; `ABSOLUTE`: the Cluster timecode as epoch milliseconds), and a later session
//! on the same stream extending it with fragment numbers past every number already minted.
//!
//! Three crates have to agree for that to hold — the Kinesis Video Streams control plane
//! creates the stream and announces the ingest and archive planes, Kinesis Video Media
//! takes the container on the announced ingest origin and serves `/getMedia` on the
//! announced read origin, and Kinesis Video Archived Media serves `/listFragments` and
//! `/getMediaForFragmentList` on the announced archive origin — so the suite lives here.
//! The container is written in the test, from the element ids the Matroska specification
//! fixes and the EBML variable-size integers of RFC 8794 section 4.4, so the bytes the
//! read planes hand back are not bytes the code under test produced.
//!
//! The rows differ along what the rule generalises over: the session's timecode type, how
//! many Clusters it carries and how far apart their timecodes are, and how many frames
//! each Cluster holds. The three-Cluster `RELATIVE` session is one row.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The region every call here is signed for.
const REGION: &str = "us-west-2";

/// The control-plane address the calls that create the stream and discover its planes
/// arrive on — a loopback listener, the form a client on this machine reaches the
/// emulator at.
const CONTROL_AUTHORITY: &str = "127.0.0.1:9297";

/// How long the suite keeps looking for a condition the clock owes — far longer than any
/// span this emulator uses, so a loaded host still passes.
const DEADLINE: Duration = Duration::from_secs(30);

/// How long it waits between looks.
const LOOK_AGAIN: Duration = Duration::from_millis(10);

/// The producer's clock at the start of every session here, in epoch milliseconds: a
/// fixed instant well behind this machine's clock, so no fragment is captured at a moment
/// the service has not reached, and the instant a `RELATIVE` timecode is an offset from is
/// the same in the test as on the wire.
const PRODUCER_START_MILLIS: i64 = 1_761_000_000_000;

/// The epoch milliseconds an `ABSOLUTE` session's Cluster timecodes name. Deliberately
/// NOT [`PRODUCER_START_MILLIS`]: an `ABSOLUTE` timecode names the instant itself, so a
/// plane that added the session's producer start to it would report the wrong instant and
/// fail the row.
const ABSOLUTE_BASE_MILLIS: u64 = 1_761_000_060_000;

/// How far a second session's timecodes are moved past the first session's, so the order
/// the fragments are archived in is the order they were submitted in whichever timecode
/// type the row uses.
const SESSION_GAP_MILLIS: u64 = 10_000;

/// The widest timestamp range `ListFragments` is asked for here, in epoch seconds: every
/// fragment any row archives falls in it.
const WHOLE_ARCHIVE_END_SECONDS: i64 = 4_000_000_000;

// ---------------------------------------------------------------------------
// The Matroska container a producer submits.
// ---------------------------------------------------------------------------

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

/// The track number the submitted container declares and writes its frames on.
const TRACK_NUMBER: u64 = 1;

/// The Matroska codec id of H.264, the one the Kinesis Video Streams producers write.
const AVC_CODEC_ID: &str = "V_MPEG4/ISO/AVC";

/// How far apart, in the container's timecode scale, the frames of one Cluster sit.
const FRAME_SPACING_MILLIS: i16 = 33;

/// Append the EBML variable-size integer (RFC 8794 section 4.4) encoding `size`, in the
/// shortest width that holds it.
fn push_ebml_size(out: &mut Vec<u8>, size: u64) {
    for width in 1..=8u32 {
        let bits = 7 * width;
        // The all-ones data bits of a width are the "unknown size" encoding, so a size
        // that would need them goes in the next width up.
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

/// Append one EBML element: its id (itself already a variable-size integer), its size,
/// then `body`.
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

/// The bytes of one submitted frame: a fixed sentinel so a window of it cannot be
/// mistaken for framing the emulator wrote, then `tag`, which is unique to this frame
/// across every session of the suite.
fn frame(tag: u8) -> Vec<u8> {
    vec![
        0xA5,
        0x5A,
        0xC3,
        0x3C,
        tag,
        tag ^ 0xFF,
        tag.wrapping_add(0x11),
        0x7E,
    ]
}

/// The container one session submits: one Cluster per entry of `timecodes`, each carrying
/// `frames_each` frames a frame apart, tagged from `first_tag` upwards.
fn container(timecodes: &[u64], frames_each: usize, first_tag: u8) -> Vec<u8> {
    let mut document = Vec::new();
    let mut header = Vec::new();
    push_ebml_element(&mut header, ID_DOC_TYPE, b"matroska");
    push_ebml_element(&mut document, ID_EBML, &header);

    let mut segment = Vec::new();
    let mut info = Vec::new();
    push_ebml_element(
        &mut info,
        ID_TIMESTAMP_SCALE,
        &uint_body(MILLISECOND_SCALE_NANOS),
    );
    push_ebml_element(&mut segment, ID_INFO, &info);

    let mut entry = Vec::new();
    push_ebml_element(&mut entry, ID_TRACK_NUMBER, &uint_body(TRACK_NUMBER));
    push_ebml_element(&mut entry, ID_TRACK_TYPE, &uint_body(TRACK_TYPE_VIDEO));
    push_ebml_element(&mut entry, ID_CODEC_ID, AVC_CODEC_ID.as_bytes());
    let mut tracks = Vec::new();
    push_ebml_element(&mut tracks, ID_TRACK_ENTRY, &entry);
    push_ebml_element(&mut segment, ID_TRACKS, &tracks);

    for (index, timecode) in timecodes.iter().enumerate() {
        let mut body = Vec::new();
        push_ebml_element(&mut body, ID_TIMESTAMP, &uint_body(*timecode));
        for position in 0..frames_each {
            // A `SimpleBlock` body is the track number as a variable-size integer, the
            // frame's signed 16-bit offset from the Cluster timecode, one flags byte,
            // then the frame itself.
            let mut block = vec![0x80 | TRACK_NUMBER as u8];
            block.extend_from_slice(&(position as i16 * FRAME_SPACING_MILLIS).to_be_bytes());
            block.push(KEYFRAME_FLAG);
            block.extend_from_slice(&frame(frame_tag(first_tag, index, frames_each, position)));
            push_ebml_element(&mut body, ID_SIMPLE_BLOCK, &block);
        }
        push_ebml_element(&mut segment, ID_CLUSTER, &body);
    }
    push_ebml_element(&mut document, ID_SEGMENT, &segment);
    document
}

/// The tag the `position`-th frame of the `cluster`-th Cluster of a session starting at
/// `first_tag` carries.
fn frame_tag(first_tag: u8, cluster: usize, frames_each: usize, position: usize) -> u8 {
    let offset = cluster * frames_each + position;
    first_tag
        .checked_add(u8::try_from(offset).expect("a small frame count"))
        .expect("a tag per frame of the suite")
}

/// The frames the `cluster`-th Cluster of a session starting at `first_tag` carries.
fn submitted_frames(first_tag: u8, cluster: usize, frames_each: usize) -> Vec<Vec<u8>> {
    (0..frames_each)
        .map(|position| frame(frame_tag(first_tag, cluster, frames_each, position)))
        .collect()
}

// ---------------------------------------------------------------------------
// The harness.
// ---------------------------------------------------------------------------

/// The emulator with the three services this suite exercises, mounted on ONE engine
/// exactly as the router mounts them, with request authentication off so each call
/// carries a credential scope (which names the Region and the service) without a computed
/// signature.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_kinesisvideo_kinesisvideo_20170930::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_kinesisvideomedia_awsacuityinletservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_kinesisvideoarchivedmedia_awsacuityreader::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// The SigV4 credential scope every call carries: its service field is the routing key the
/// Kinesis Video services share.
fn credential_scope() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/kinesisvideo/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// What one call answered.
#[derive(Debug)]
struct Answer {
    status: u16,
    /// The response body, as served.
    bytes: Vec<u8>,
}

impl Answer {
    /// The body as JSON, or `Null` for a body that is not one document.
    fn json(&self) -> Value {
        serde_json::from_slice(&self.bytes).unwrap_or(Value::Null)
    }

    /// The body as text, for a message a failing assertion states.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes).to_string()
    }

    /// The acknowledgement stream the body carries: one JSON object per line, in the
    /// order the session produced them, which is the form a producer reads off this wire.
    fn acknowledgements(&self) -> Vec<Value> {
        self.text()
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                serde_json::from_str::<Value>(line)
                    .unwrap_or_else(|_| panic!("an acknowledgement per line, got {line:?}"))
            })
            .collect()
    }

    /// Whether the served bytes carry `needle` — how a consumer checks that the media it
    /// is handed is the media that was ingested.
    fn carries(&self, needle: &[u8]) -> bool {
        self.bytes.windows(needle.len()).any(|window| window == needle)
    }
}

/// POST `body` to `authority` with `headers`, through the router.
async fn post(
    router: &MultiServiceRouter,
    authority: &str,
    path: &str,
    headers: &[(&str, String)],
    body: Vec<u8>,
) -> Answer {
    let mut request = ::http::Request::builder()
        .method("POST")
        .uri(format!("https://{authority}{path}"))
        .header("host", authority)
        .header("authorization", credential_scope());
    for (name, value) in headers {
        request = request.header(*name, value.clone());
    }
    let response = router
        .dispatch(request.body(Bytes::from(body)).expect("a well-formed request"))
        .await;
    Answer {
        status: response.status().as_u16(),
        bytes: response.into_body().to_vec(),
    }
}

/// POST one restJson1 call to `authority`.
async fn call(router: &MultiServiceRouter, authority: &str, path: &str, body: &Value) -> Answer {
    post(
        router,
        authority,
        path,
        &[("content-type", "application/json".to_string())],
        serde_json::to_vec(body).expect("the request body serializes"),
    )
    .await
}

/// The authority the control plane announces for `api_name` on `stream`, which is the only
/// documented way to find a stream's media planes.
async fn data_authority(router: &MultiServiceRouter, stream: &str, api_name: &str) -> String {
    let answer = call(
        router,
        CONTROL_AUTHORITY,
        "/getDataEndpoint",
        &json!({ "StreamName": stream, "APIName": api_name }),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case {stream:?}/{api_name:?}: GetDataEndpoint expected HTTP 200, got {answer:?}"
    );
    let endpoint = answer
        .json()
        .get("DataEndpoint")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {stream:?}/{api_name:?}: no DataEndpoint in {answer:?}"))
        .to_string();
    let authority = endpoint
        .split_once("://")
        .map(|(_, authority)| authority.to_string())
        .unwrap_or_else(|| {
            panic!("case {stream:?}/{api_name:?}: expected a bare origin, got {endpoint:?}")
        });
    assert_ne!(
        authority, CONTROL_AUTHORITY,
        "case {stream:?}/{api_name:?}: a data plane is announced at its own address, not at \
         the control-plane one, got {authority:?}"
    );
    authority
}

/// Create `stream` with a retention of one hour and wait until the control plane reports
/// it `ACTIVE`, which is what a producer does before ingesting into a stream it just
/// created ("CreateStream is an asynchronous operation").
///
/// Waits for the CONDITION, not for a span: it looks again until the status holds, under a
/// deadline far longer than the transition needs.
async fn given_active_stream(router: &MultiServiceRouter, stream: &str) {
    let created = call(
        router,
        CONTROL_AUTHORITY,
        "/createStream",
        &json!({ "StreamName": stream, "MediaType": "video/h264", "DataRetentionInHours": 1 }),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case create {stream:?}: expected HTTP 200, got {created:?}"
    );
    let deadline = Instant::now() + DEADLINE;
    let mut last = String::new();
    while Instant::now() < deadline {
        let described = call(
            router,
            CONTROL_AUTHORITY,
            "/describeStream",
            &json!({ "StreamName": stream }),
        )
        .await;
        last = described.json()["StreamInfo"]["Status"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if last == "ACTIVE" {
            return;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    panic!("case create {stream:?}: expected Status ACTIVE before the deadline, got {last:?}");
}

/// One ingest session, as a producer sends it: the container submitted to the announced
/// `PUT_MEDIA` origin for `stream`, under `timecode_type` and the suite's producer start.
async fn put_media(
    router: &MultiServiceRouter,
    stream: &str,
    timecode_type: &str,
    body: Vec<u8>,
) -> Answer {
    let authority = data_authority(router, stream, "PUT_MEDIA").await;
    let seconds = PRODUCER_START_MILLIS / 1_000;
    let millis = PRODUCER_START_MILLIS % 1_000;
    post(
        router,
        &authority,
        "/putMedia",
        &[
            ("x-amzn-stream-name", stream.to_string()),
            ("x-amzn-fragment-timecode-type", timecode_type.to_string()),
            (
                "x-amzn-producer-start-timestamp",
                format!("{seconds}.{millis:03}"),
            ),
            // The ingest front end does not buffer the stream it is handed, so the
            // payload hash the signature covers is declared rather than computed.
            ("x-amz-content-sha256", "UNSIGNED-PAYLOAD".to_string()),
        ],
        body,
    )
    .await
}

/// The fragments `ListFragments` reports for `stream` once it reports at least `count` of
/// them, asked for over the whole archive on the announced `LIST_FRAGMENTS` origin.
///
/// Waits for the CONDITION, not for a span.
async fn archived_fragments(
    router: &MultiServiceRouter,
    stream: &str,
    count: usize,
) -> Vec<Value> {
    let authority = data_authority(router, stream, "LIST_FRAGMENTS").await;
    let body = json!({
        "StreamName": stream,
        "FragmentSelector": {
            "FragmentSelectorType": "SERVER_TIMESTAMP",
            "TimestampRange": { "StartTimestamp": 0, "EndTimestamp": WHOLE_ARCHIVE_END_SECONDS },
        },
    });
    let deadline = Instant::now() + DEADLINE;
    let mut last = Answer {
        status: 0,
        bytes: Vec::new(),
    };
    while Instant::now() < deadline {
        last = call(router, &authority, "/listFragments", &body).await;
        assert_eq!(
            last.status, 200,
            "case {stream:?}: ListFragments expected HTTP 200, got {last:?}"
        );
        let listed = last.json()["Fragments"].as_array().cloned().unwrap_or_default();
        if listed.len() >= count {
            return listed;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    panic!("case {stream:?}: expected {count} archived fragment(s), got {last:?}");
}

/// `GetMedia` on the announced `GET_MEDIA` origin for `stream`, under `selector`.
async fn get_media(router: &MultiServiceRouter, stream: &str, selector: Value) -> Answer {
    let authority = data_authority(router, stream, "GET_MEDIA").await;
    call(
        router,
        &authority,
        "/getMedia",
        &json!({ "StreamName": stream, "StartSelector": selector }),
    )
    .await
}

/// `GetMediaForFragmentList` on the announced origin for `stream`, for `fragments`.
async fn get_media_for_fragment_list(
    router: &MultiServiceRouter,
    stream: &str,
    fragments: &[String],
) -> Answer {
    let authority = data_authority(router, stream, "GET_MEDIA_FOR_FRAGMENT_LIST").await;
    call(
        router,
        &authority,
        "/getMediaForFragmentList",
        &json!({ "StreamName": stream, "Fragments": fragments }),
    )
    .await
}

/// A fragment number as the number it is: the value is a numeric string, so the ordering
/// a producer reads monotonicity off is the numeric one.
fn as_number(case: &str, fragment_number: &str) -> u128 {
    fragment_number
        .parse::<u128>()
        .unwrap_or_else(|_| panic!("case {case:?}: expected a numeric fragment number, got {fragment_number:?}"))
}

/// The epoch milliseconds a JSON timestamp member names: the wire carries epoch seconds,
/// and a fragment's instants are stated to the millisecond.
fn millis_of(case: &str, member: &str, value: &Value) -> i64 {
    let seconds = value
        .as_f64()
        .unwrap_or_else(|| panic!("case {case:?}: expected {member} as epoch seconds, got {value:?}"));
    (seconds * 1_000.0).round() as i64
}

// ---------------------------------------------------------------------------
// The rule.
// ---------------------------------------------------------------------------

/// One row: the session a producer sends.
#[derive(Debug, Clone, Copy)]
struct Row {
    case: &'static str,
    stream: &'static str,
    /// How the Clusters' timecodes are to be read.
    timecode_type: &'static str,
    /// The Cluster timecodes the first session sends, at the container's millisecond
    /// scale.
    timecodes: &'static [u64],
    /// How many frames each of those Clusters carries.
    frames_each: usize,
}

/// The rows, differing along the timecode type, the Cluster count and spacing, and the
/// frames per Cluster.
const ROWS: &[Row] = &[
    Row {
        case: "a RELATIVE session of three Clusters a second apart, two frames each",
        stream: "ingest-readback-relative",
        timecode_type: "RELATIVE",
        timecodes: &[0, 1_000, 2_000],
        frames_each: 2,
    },
    Row {
        case: "an ABSOLUTE session of two Clusters half a second apart, one frame each",
        stream: "ingest-readback-absolute",
        timecode_type: "ABSOLUTE",
        timecodes: &[ABSOLUTE_BASE_MILLIS, ABSOLUTE_BASE_MILLIS + 500],
        frames_each: 1,
    },
    Row {
        case: "a RELATIVE session of one Cluster of three frames",
        stream: "ingest-readback-single",
        timecode_type: "RELATIVE",
        timecodes: &[0],
        frames_each: 3,
    },
];

/// What one submitted Cluster became: the fragment number the ingest minted for it, the
/// timecode it was sent with, the instant the session's timecode type names for it, and
/// the frames it carried.
#[derive(Debug, Clone)]
struct Persisted {
    number: String,
    timecode: u64,
    producer_millis: i64,
    frames: Vec<Vec<u8>>,
}

/// Submit one session of `row` and check its acknowledgement stream: a `BUFFERING`, a
/// `RECEIVED` and a `PERSISTED` per accepted Cluster, each naming the minted fragment
/// number and that Cluster's own timecode, and no `ERROR` for a container with no fault
/// in it.
async fn ingest_session(
    router: &MultiServiceRouter,
    row: &Row,
    timecodes: &[u64],
    first_tag: u8,
) -> Vec<Persisted> {
    let Row {
        case,
        stream,
        timecode_type,
        frames_each,
        ..
    } = *row;
    let answer = put_media(
        router,
        stream,
        timecode_type,
        container(timecodes, frames_each, first_tag),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: PutMedia answers the session with its acknowledgement stream, got \
         {answer:?}"
    );
    let acknowledgements = answer.acknowledgements();
    let event_types: Vec<String> = acknowledgements
        .iter()
        .map(|acknowledgement| {
            acknowledgement["EventType"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    assert!(
        !event_types.iter().any(|event| event == "ERROR"),
        "case {case:?}: a container with no fault in it earns no ERROR acknowledgement, got \
         {event_types:?}"
    );
    for event in ["BUFFERING", "RECEIVED", "PERSISTED"] {
        let counted = event_types.iter().filter(|seen| *seen == event).count();
        assert_eq!(
            counted,
            timecodes.len(),
            "case {case:?}: expected one {event} acknowledgement per submitted Cluster \
             ({}), got {counted} in {event_types:?}",
            timecodes.len()
        );
    }

    let persisted: Vec<&Value> = acknowledgements
        .iter()
        .filter(|acknowledgement| acknowledgement["EventType"].as_str() == Some("PERSISTED"))
        .collect();
    timecodes
        .iter()
        .enumerate()
        .map(|(index, timecode)| {
            let acknowledgement = persisted[index];
            assert_eq!(
                acknowledgement["FragmentTimecode"].as_u64(),
                Some(*timecode),
                "case {case:?}: the acknowledgement of Cluster {index} states the timecode it \
                 was sent with, got {acknowledgement:?}"
            );
            let number = acknowledgement["FragmentNumber"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: a stored fragment is acknowledged with the number \
                            the service minted, got {acknowledgement:?}")
                })
                .to_string();
            let producer_millis = match timecode_type {
                // A RELATIVE timecode is an offset from the producer's own start.
                "RELATIVE" => PRODUCER_START_MILLIS + i64::try_from(*timecode).expect("a timecode"),
                // An ABSOLUTE one already names the instant, in epoch milliseconds.
                _ => i64::try_from(*timecode).expect("a timecode"),
            };
            Persisted {
                number,
                timecode: *timecode,
                producer_millis,
                frames: submitted_frames(first_tag, index, frames_each),
            }
        })
        .collect()
}

/// THE RULE: every fragment the ingest plane acknowledged is read back from the stream by
/// the number the ingest minted — `ListFragments` reports exactly those numbers at the
/// instants the session's timecode type names, `GetMedia` under
/// `FRAGMENT_NUMBER`/`AfterFragmentNumber` resumes with the fragments after the one named,
/// and `GetMediaForFragmentList` serves the named fragment and no other — and a second
/// session on the same stream extends it with numbers past every number already minted.
#[tokio::test(flavor = "multi_thread")]
async fn every_acknowledged_fragment_is_read_back_by_its_minted_number() {
    let router = emulator();

    for row in ROWS {
        let Row {
            case,
            stream,
            frames_each,
            timecodes,
            ..
        } = *row;
        given_active_stream(&router, stream).await;

        // Before anything is produced, the archive of an ACTIVE stream reports no
        // fragment at all: what the reads below report is what the sessions wrote.
        let empty = archived_fragments(&router, stream, 0).await;
        assert!(
            empty.is_empty(),
            "case {case:?}: a stream nothing was produced to holds no fragment, got {empty:?}"
        );

        // The first session, then a second one whose timecodes name later instants: one
        // producer reconnecting, which has to extend the same stream.
        let first = ingest_session(&router, row, timecodes, 0x11).await;
        let later: Vec<u64> = timecodes
            .iter()
            .map(|timecode| timecode + SESSION_GAP_MILLIS)
            .collect();
        let second = ingest_session(&router, row, &later, 0x61).await;
        let fragments: Vec<Persisted> = first.iter().chain(second.iter()).cloned().collect();

        // Monotonic: every number the later session minted is past every number minted
        // before it, so a consumer walking the archive forwards never goes back.
        let mut previous = 0u128;
        for fragment in &fragments {
            let number = as_number(case, &fragment.number);
            assert!(
                number > previous,
                "case {case:?}: fragment numbers extend the stream monotonically, got \
                 {number} after {previous}"
            );
            previous = number;
        }

        // ListFragments: exactly the ingested fragments, in the order they were produced,
        // each at the instant its Cluster's timecode names under the session's timecode
        // type.
        let listed = archived_fragments(&router, stream, fragments.len()).await;
        assert_eq!(
            listed.len(),
            fragments.len(),
            "case {case:?}: expected exactly the {} ingested fragment(s), got {listed:?}",
            fragments.len()
        );
        for (index, fragment) in fragments.iter().enumerate() {
            let reported = &listed[index];
            assert_eq!(
                reported["FragmentNumber"].as_str(),
                Some(fragment.number.as_str()),
                "case {case:?}: fragment {index} of the archive is the one acknowledged with \
                 that number, got {reported:?}"
            );
            assert_eq!(
                millis_of(case, "ProducerTimestamp", &reported["ProducerTimestamp"]),
                fragment.producer_millis,
                "case {case:?}: the fragment of Cluster timecode {} is reported at the instant \
                 its {} timecode names, got {reported:?}",
                fragment.timecode,
                row.timecode_type
            );
        }

        // GetMedia from the start of the archive: the frames of every ingested fragment,
        // byte for byte as they were submitted.
        let whole = get_media(&router, stream, json!({ "StartSelectorType": "EARLIEST" })).await;
        assert_eq!(
            whole.status, 200,
            "case {case:?}: GetMedia expected HTTP 200, got {whole:?}"
        );
        for (index, fragment) in fragments.iter().enumerate() {
            for (position, submitted) in fragment.frames.iter().enumerate() {
                assert!(
                    whole.carries(submitted),
                    "case {case:?}: the media of the stream carries frame {position} of \
                     fragment {index} as submitted, got {} byte(s) carrying none of it",
                    whole.bytes.len()
                );
            }
        }

        // One row per fragment the archive holds, read back by its number through each
        // plane that addresses a fragment that way.
        for (index, fragment) in fragments.iter().enumerate() {
            let resumed = get_media(
                &router,
                stream,
                json!({
                    "StartSelectorType": "FRAGMENT_NUMBER",
                    "AfterFragmentNumber": fragment.number,
                }),
            )
            .await;
            assert_eq!(
                resumed.status, 200,
                "case {case:?}: GetMedia after fragment {index} expected HTTP 200, got {resumed:?}"
            );
            for (other, held) in fragments.iter().enumerate() {
                let expected = other > index;
                for (position, submitted) in held.frames.iter().enumerate() {
                    assert_eq!(
                        resumed.carries(submitted),
                        expected,
                        "case {case:?}: GetMedia after fragment {index} starts with the chunk \
                         AFTER it, so frame {position} of fragment {other} is {}, got the \
                         other",
                        if expected { "served" } else { "not served" }
                    );
                }
            }

            let named = get_media_for_fragment_list(
                &router,
                stream,
                std::slice::from_ref(&fragment.number),
            )
            .await;
            assert_eq!(
                named.status, 200,
                "case {case:?}: GetMediaForFragmentList for fragment {index} expected HTTP 200, \
                 got {named:?}"
            );
            for (other, held) in fragments.iter().enumerate() {
                let expected = other == index;
                for (position, submitted) in held.frames.iter().enumerate() {
                    assert_eq!(
                        named.carries(submitted),
                        expected,
                        "case {case:?}: GetMediaForFragmentList serves the named fragment and \
                         no other, so frame {position} of fragment {other} is {}, got the other",
                        if expected { "served" } else { "not served" }
                    );
                }
            }
        }
        assert_eq!(
            frames_each * timecodes.len() * 2,
            fragments
                .iter()
                .map(|fragment| fragment.frames.len())
                .sum::<usize>(),
            "case {case:?}: every submitted frame of both sessions is accounted for"
        );
    }
}
