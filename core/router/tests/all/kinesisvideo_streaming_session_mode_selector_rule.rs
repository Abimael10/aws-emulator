// SPDX-License-Identifier: CC0-1.0

//! The playback-mode selector rule and the session-parameter defaults are ONE rule, and
//! both streaming-session operations of Kinesis Video Archived Media answer by it.
//!
//! The rule under test: **a streaming-session request's `PlaybackMode` decides what its
//! fragment selector must carry and how much of the archive the session serves — LIVE
//! carries no `TimestampRange`, every other mode requires one with a `StartTimestamp`,
//! ON_DEMAND requires an `EndTimestamp` too, an end at or before the start is refused, and
//! an unstated `PlaybackMode`, `Expires` or maximum-fragment count means the documented
//! default (LIVE, 300 seconds, 5 fragments for LIVE/LIVE_REPLAY and 1,000 for ON_DEMAND)**.
//! `GetHLSStreamingSessionURL` and `GetDASHStreamingSessionURL` are the two operations that
//! mint such a session, so the rule holds identically for both: the only thing that differs
//! is the input member each carries its selector in, which is the member each refusal names.
//!
//! Three crates have to agree for one of these calls to be answered at all — the Kinesis
//! Video Streams control plane is where a stream EXISTS (`CreateStream`), the archived-media
//! reader resolves the request's identifier in that registry and serves the archive held
//! under the ARN it hands back — so the suite lives here and creates its streams through the
//! control plane, exactly as a client does.
//!
//! The rows differ along what the rule generalises over: the playback mode (named and
//! defaulted), whether a selector is present, which bounds its range carries, how the bounds
//! sit relative to each other and to the archive, and whether the request caps the snapshot
//! or leaves the per-mode default to apply. Each row is driven through BOTH operations.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_kinesisvideoarchivedmedia_awsacuityreader as archived_media;

use archived_media::state::{
    from_epoch_millis, service_state, ArchivedStream, SessionProtocol, StoredFragment,
    ARCHIVE_CONTENT_TYPE,
};

// The archived Matroska document one fragment IS, written by the suite that owns this
// family's container fixture: a session is minted over media whose codec ids and codec
// private data the read plane can classify, so the seeded archive carries the real thing.
use crate::kinesisvideo_clip_muxes_the_ingested_fragments::archived_fragment;

/// The region every call here is signed for.
const REGION: &str = "us-west-2";

/// The address every call is addressed to: this emulator, on loopback.
const AUTHORITY: &str = "127.0.0.1:9297";

/// The stream whose archive lies in the past, which the ON_DEMAND and LIVE_REPLAY rows read.
const ARCHIVE_STREAM: &str = "session-rule-archive";

/// The stream whose fragments were ingested just now, which the LIVE rows read: a LIVE
/// session exists only while the stream head is fresh.
const LIVE_STREAM: &str = "session-rule-live";

/// How long both streams retain data: more than zero, which is what the streaming-session
/// operations require of a stream at all.
const RETENTION_HOURS: i32 = 24;

/// The media type both streams declare.
const MEDIA_TYPE: &str = "video/h264";

/// The base instant of the archived stream's fragments: well in the past, so none of them is
/// inside the LIVE window.
const T0_SECS: i64 = 1_761_000_000;

/// How far apart the archived stream's fragments are stamped, in seconds.
const FRAGMENT_SPACING_SECS: i64 = 10;

/// How many fragments each stream holds. More than the LIVE/LIVE_REPLAY default of five, so
/// a row that leaves the cap unstated shows the default rather than "everything".
const FRAGMENTS: u64 = 6;

/// The archived stream's fragment numbers start here, and the live stream's elsewhere, so a
/// session that served the wrong stream's archive cannot pass.
const ARCHIVE_FRAGMENT_BASE: u64 = 9_134_385_233_318_143_200;
/// The live stream's own fragment-number base.
const LIVE_FRAGMENT_BASE: u64 = 9_244_385_233_318_143_200;

/// The sentence a range whose end is not later than its start is refused with.
const RANGE_ORDER_MESSAGE: &str = "StartTimestamp must be before EndTimestamp.";

/// The `Expires` default, in seconds: "The default is 300 (5 minutes)."
const DEFAULT_EXPIRES_SECS: u32 = 300;

/// The emulator with the two services one of these calls needs, mounted on ONE engine as the
/// router mounts them, with request authentication off so each call carries a credential
/// scope (which names the Region and the service) without a computed signature.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_kinesisvideo_kinesisvideo_20170930::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(archived_media::EmulatedService::new(Arc::clone(&engine)));
    (router, engine)
}

/// What one call answered: its status and its parsed JSON body.
#[derive(Debug)]
struct Answer {
    status: u16,
    body: Value,
}

impl Answer {
    /// The error code the envelope names, or `""` for a body that names none.
    fn error_code(&self) -> String {
        self.body
            .get("__type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .rsplit('#')
            .next()
            .unwrap_or_default()
            .to_string()
    }

    /// The refusal's own sentence.
    fn message(&self) -> String {
        self.body
            .get("message")
            .or_else(|| self.body.get("Message"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

/// POST one restJson1 call to [`AUTHORITY`], signed for `kinesisvideo` in [`REGION`].
async fn call(router: &MultiServiceRouter, path: &str, body: &Value) -> Answer {
    let request = ::http::Request::builder()
        .method("POST")
        .uri(format!("https://{AUTHORITY}{path}"))
        .header("host", AUTHORITY)
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 \
                 Credential=AKIDEXAMPLE/20260101/{REGION}/kinesisvideo/aws4_request, \
                 SignedHeaders=host, Signature=not-verified"
            ),
        )
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    Answer { status, body }
}

/// Create `stream` through the control plane and answer the `StreamARN` it minted.
async fn create_stream(router: &MultiServiceRouter, stream: &str) -> String {
    let created = call(
        router,
        "/createStream",
        &json!({
            "StreamName": stream,
            "MediaType": MEDIA_TYPE,
            "DataRetentionInHours": RETENTION_HOURS,
        }),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case create {stream}: CreateStream expected HTTP 200, got {created:?}"
    );
    created
        .body
        .get("StreamARN")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case create {stream}: no StreamARN in {created:?}"))
        .to_string()
}

/// Wait until the control plane reports `stream` as `ACTIVE`, which is the one status an
/// archived-media read is served for ("CreateStream is an asynchronous operation").
///
/// Waits for the CONDITION, not for a span: it looks again until the status holds, under a
/// deadline far longer than the transition needs.
async fn await_active(router: &MultiServiceRouter, stream: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    let mut last = String::new();
    while std::time::Instant::now() < deadline {
        let answer = call(router, "/describeStream", &json!({ "StreamName": stream })).await;
        last = answer.body["StreamInfo"]["Status"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if last == "ACTIVE" {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("case await {stream}: expected Status ACTIVE before the deadline, got {last:?}");
}

/// The fragment number of the `index`-th fragment of the stream `base` belongs to.
fn fragment_number(base: u64, index: u64) -> String {
    (base + index).to_string()
}

/// Seed one stream's archive under the ARN the control plane registered it with — the
/// identity an ingest writes an archive under — with [`FRAGMENTS`] fragments stamped from
/// `first_millis` at [`FRAGMENT_SPACING_SECS`].
///
/// Each fragment's media is the Matroska document an ingest would have written for it
/// ([`archived_fragment`]), distinct per fragment: the rule under test selects fragments by
/// their timestamps and reports their NUMBERS, but minting a session over the archive reads
/// the codec ids and the codec private data of the fragment it starts from, so the seeded
/// archive is a real one.
fn seed_archive(
    engine: &engine::InMemoryEngine,
    stream: &str,
    arn: &str,
    base: u64,
    first_millis: i64,
) {
    let state = service_state(engine).expect("the archived-media store");
    let mut state = state.lock();
    state.put_stream(ArchivedStream {
        name: stream.to_string(),
        arn: arn.to_string(),
        data_retention_in_hours: u32::try_from(RETENTION_HOURS).unwrap_or_default(),
        media_type: MEDIA_TYPE.to_string(),
        archive_content_type: ARCHIVE_CONTENT_TYPE.to_string(),
        codec_private_data: None,
        fragments: Vec::new(),
    });
    for index in 0..FRAGMENTS {
        let stamped =
            first_millis + i64::try_from(index).unwrap_or_default() * FRAGMENT_SPACING_SECS * 1000;
        assert!(
            state.append_fragment(
                stream,
                StoredFragment::new(
                    fragment_number(base, index),
                    from_epoch_millis(stamped),
                    from_epoch_millis(stamped),
                    u64::try_from(FRAGMENT_SPACING_SECS).unwrap_or_default() * 1000,
                    archived_fragment(u8::try_from(index + 1).unwrap_or(u8::MAX)),
                ),
            ),
            "case seed {stream} fragment {index}: the seeded stream must exist"
        );
    }
}

/// One of the two streaming-session operations: everything about it that the shared rule is
/// parameterised by.
struct Operation {
    /// The operation's name, for the case label.
    name: &'static str,
    /// Its route.
    path: &'static str,
    /// The input member it carries its fragment selector in, which is the member its
    /// refusals name.
    selector_member: &'static str,
    /// Its own maximum-fragment member.
    max_member: &'static str,
    /// The output member carrying the session URL.
    url_member: &'static str,
    /// The protocol recorded on the session it mints.
    protocol: SessionProtocol,
    /// The path the session URL it reports addresses.
    manifest_path: &'static str,
}

/// The two operations the rule is held once for.
fn operations() -> [Operation; 2] {
    [
        Operation {
            name: "GetHLSStreamingSessionURL",
            path: "/getHLSStreamingSessionURL",
            selector_member: "HLSFragmentSelector",
            max_member: "MaxMediaPlaylistFragmentResults",
            url_member: "HLSStreamingSessionURL",
            protocol: SessionProtocol::Hls,
            manifest_path: "/hls/v1/getHLSMasterPlaylist.m3u8",
        },
        Operation {
            name: "GetDASHStreamingSessionURL",
            path: "/getDASHStreamingSessionURL",
            selector_member: "DASHFragmentSelector",
            max_member: "MaxManifestFragmentResults",
            url_member: "DASHStreamingSessionURL",
            protocol: SessionProtocol::Dash,
            manifest_path: "/dash/v1/getMPDManifest.mpd",
        },
    ]
}

/// What a row's request carries where its fragment selector would go.
#[derive(Debug, Clone, Copy)]
enum Selector {
    /// No selector member at all.
    Absent,
    /// The selector, carrying no `TimestampRange`.
    WithoutRange,
    /// The selector and a range, with each bound in seconds from [`T0_SECS`] when present.
    Range {
        start: Option<i64>,
        end: Option<i64>,
    },
}

/// Which stream a row reads.
#[derive(Debug, Clone, Copy)]
enum Which {
    /// The archive in the past.
    Archive,
    /// The freshly ingested one.
    Live,
}

/// What a row expects the operation to answer.
#[derive(Debug, Clone)]
enum Expected {
    /// A minted session: the fragment indices it serves, the `PlaybackMode` recorded on it
    /// and its `Expires`.
    Session {
        fragments: Vec<u64>,
        playback_mode: &'static str,
        expires_in_secs: u32,
    },
    /// A refusal, with `{member}` standing for the operation's own selector member.
    Refused { message: &'static str },
}

/// One row of the table: a request, and what the rule says of it.
struct Row {
    case: &'static str,
    which: Which,
    playback_mode: Option<&'static str>,
    selector: Selector,
    max_fragments: Option<i64>,
    expires: Option<i64>,
    expected: Expected,
}

/// The request body for `row`, in `operation`'s own members.
fn request_body(operation: &Operation, row: &Row, stream: &str) -> Value {
    let mut body = json!({ "StreamName": stream });
    if let Some(mode) = row.playback_mode {
        body["PlaybackMode"] = json!(mode);
    }
    if let Some(max) = row.max_fragments {
        body[operation.max_member] = json!(max);
    }
    if let Some(expires) = row.expires {
        body["Expires"] = json!(expires);
    }
    let selector = match row.selector {
        Selector::Absent => None,
        Selector::WithoutRange => Some(json!({ "FragmentSelectorType": "PRODUCER_TIMESTAMP" })),
        Selector::Range { start, end } => {
            let mut range = json!({});
            if let Some(start) = start {
                range["StartTimestamp"] = json!(T0_SECS + start);
            }
            if let Some(end) = end {
                range["EndTimestamp"] = json!(T0_SECS + end);
            }
            Some(json!({
                "FragmentSelectorType": "PRODUCER_TIMESTAMP",
                "TimestampRange": range,
            }))
        }
    };
    if let Some(selector) = selector {
        body[operation.selector_member] = selector;
    }
    body
}

/// Both streaming-session operations answer one streaming-session request by the same mode
/// and selector rule, and apply the same documented defaults.
#[tokio::test(flavor = "multi_thread")]
async fn both_streaming_session_operations_answer_by_one_mode_and_selector_rule() {
    let (router, engine) = emulator();
    let archive_arn = create_stream(&router, ARCHIVE_STREAM).await;
    let live_arn = create_stream(&router, LIVE_STREAM).await;
    await_active(&router, ARCHIVE_STREAM).await;
    await_active(&router, LIVE_STREAM).await;

    seed_archive(
        &engine,
        ARCHIVE_STREAM,
        &archive_arn,
        ARCHIVE_FRAGMENT_BASE,
        T0_SECS * 1000,
    );
    // The live stream's fragments end at "now" and step BACK, so every one of them is inside
    // the 30-second window a LIVE session exists in.
    let live_first_millis = archived_media::state::epoch_millis(
        SystemTime::now()
            - Duration::from_secs(
                (FRAGMENTS - 1) * u64::try_from(FRAGMENT_SPACING_SECS).unwrap_or_default(),
            ),
    );
    seed_archive(
        &engine,
        LIVE_STREAM,
        &live_arn,
        LIVE_FRAGMENT_BASE,
        live_first_millis,
    );

    let whole_archive = Selector::Range {
        start: Some(0),
        end: Some(i64::try_from(FRAGMENTS).unwrap_or_default() * FRAGMENT_SPACING_SECS),
    };
    let rows: Vec<Row> = vec![
        Row {
            case: "no PlaybackMode at all: the LIVE default, at the LIVE default of five",
            which: Which::Live,
            playback_mode: None,
            selector: Selector::Absent,
            max_fragments: None,
            expires: None,
            expected: Expected::Session {
                fragments: (FRAGMENTS - 5..FRAGMENTS).collect(),
                playback_mode: "LIVE",
                expires_in_secs: DEFAULT_EXPIRES_SECS,
            },
        },
        Row {
            case: "LIVE named, capped at two",
            which: Which::Live,
            playback_mode: Some("LIVE"),
            selector: Selector::WithoutRange,
            max_fragments: Some(2),
            expires: None,
            expected: Expected::Session {
                fragments: (FRAGMENTS - 2..FRAGMENTS).collect(),
                playback_mode: "LIVE",
                expires_in_secs: DEFAULT_EXPIRES_SECS,
            },
        },
        Row {
            case: "ON_DEMAND over the whole archive, uncapped: the ON_DEMAND default of 1,000",
            which: Which::Archive,
            playback_mode: Some("ON_DEMAND"),
            selector: whole_archive,
            max_fragments: None,
            expires: None,
            expected: Expected::Session {
                fragments: (0..FRAGMENTS).collect(),
                playback_mode: "ON_DEMAND",
                expires_in_secs: DEFAULT_EXPIRES_SECS,
            },
        },
        Row {
            case: "ON_DEMAND over a window inside the archive, with an Expires of its own",
            which: Which::Archive,
            playback_mode: Some("ON_DEMAND"),
            selector: Selector::Range {
                start: Some(FRAGMENT_SPACING_SECS * 2 - 5),
                end: Some(FRAGMENT_SPACING_SECS * 3 + 5),
            },
            max_fragments: None,
            expires: Some(1800),
            expected: Expected::Session {
                fragments: vec![2, 3],
                playback_mode: "ON_DEMAND",
                expires_in_secs: 1800,
            },
        },
        Row {
            case: "LIVE_REPLAY with an open-ended range, at the LIVE_REPLAY default of five",
            which: Which::Archive,
            playback_mode: Some("LIVE_REPLAY"),
            selector: Selector::Range {
                start: Some(0),
                end: None,
            },
            max_fragments: None,
            expires: None,
            expected: Expected::Session {
                fragments: (0..5).collect(),
                playback_mode: "LIVE_REPLAY",
                expires_in_secs: DEFAULT_EXPIRES_SECS,
            },
        },
        Row {
            case: "LIVE with a TimestampRange",
            which: Which::Live,
            playback_mode: Some("LIVE"),
            selector: whole_archive,
            max_fragments: None,
            expires: None,
            expected: Expected::Refused {
                message: "{member}.TimestampRange must not be set when PlaybackMode is LIVE",
            },
        },
        Row {
            case: "ON_DEMAND with no selector",
            which: Which::Archive,
            playback_mode: Some("ON_DEMAND"),
            selector: Selector::Absent,
            max_fragments: None,
            expires: None,
            expected: Expected::Refused {
                message: "{member} with a TimestampRange is required when PlaybackMode is \
                          ON_DEMAND",
            },
        },
        Row {
            case: "LIVE_REPLAY with no selector",
            which: Which::Archive,
            playback_mode: Some("LIVE_REPLAY"),
            selector: Selector::Absent,
            max_fragments: None,
            expires: None,
            expected: Expected::Refused {
                message: "{member} with a TimestampRange is required when PlaybackMode is \
                          LIVE_REPLAY",
            },
        },
        Row {
            case: "ON_DEMAND with a selector carrying no range",
            which: Which::Archive,
            playback_mode: Some("ON_DEMAND"),
            selector: Selector::WithoutRange,
            max_fragments: None,
            expires: None,
            expected: Expected::Refused {
                message: "{member} with a TimestampRange is required when PlaybackMode is \
                          ON_DEMAND",
            },
        },
        Row {
            case: "LIVE_REPLAY with a range carrying no StartTimestamp",
            which: Which::Archive,
            playback_mode: Some("LIVE_REPLAY"),
            selector: Selector::Range {
                start: None,
                end: Some(FRAGMENT_SPACING_SECS * 3),
            },
            max_fragments: None,
            expires: None,
            expected: Expected::Refused {
                message: "{member}.TimestampRange.StartTimestamp is required",
            },
        },
        Row {
            case: "ON_DEMAND with no EndTimestamp",
            which: Which::Archive,
            playback_mode: Some("ON_DEMAND"),
            selector: Selector::Range {
                start: Some(0),
                end: None,
            },
            max_fragments: None,
            expires: None,
            expected: Expected::Refused {
                message: "{member}.TimestampRange.EndTimestamp is required when PlaybackMode is \
                          ON_DEMAND",
            },
        },
        Row {
            case: "ON_DEMAND with an end before the start",
            which: Which::Archive,
            playback_mode: Some("ON_DEMAND"),
            selector: Selector::Range {
                start: Some(FRAGMENT_SPACING_SECS * 3),
                end: Some(0),
            },
            max_fragments: None,
            expires: None,
            expected: Expected::Refused {
                message: RANGE_ORDER_MESSAGE,
            },
        },
        Row {
            case: "LIVE_REPLAY with an end equal to the start",
            which: Which::Archive,
            playback_mode: Some("LIVE_REPLAY"),
            selector: Selector::Range {
                start: Some(0),
                end: Some(0),
            },
            max_fragments: None,
            expires: None,
            expected: Expected::Refused {
                message: RANGE_ORDER_MESSAGE,
            },
        },
    ];

    for row in &rows {
        for operation in &operations() {
            let case = format!("{} on {}", row.case, operation.name);
            let (stream, base) = match row.which {
                Which::Archive => (ARCHIVE_STREAM, ARCHIVE_FRAGMENT_BASE),
                Which::Live => (LIVE_STREAM, LIVE_FRAGMENT_BASE),
            };
            let body = request_body(operation, row, stream);
            let answer = call(&router, operation.path, &body).await;

            match &row.expected {
                Expected::Refused { message } => {
                    let expected = message.replace("{member}", operation.selector_member);
                    assert_eq!(
                        answer.status, 400,
                        "case {case}: expected HTTP 400, got {answer:?}"
                    );
                    assert_eq!(
                        answer.error_code(),
                        "InvalidArgumentException",
                        "case {case}: expected InvalidArgumentException, got {answer:?}"
                    );
                    assert_eq!(
                        answer.message(),
                        expected,
                        "case {case}: expected the refusal {expected:?}, got {answer:?}"
                    );
                }
                Expected::Session {
                    fragments,
                    playback_mode,
                    expires_in_secs,
                } => {
                    assert_eq!(
                        answer.status, 200,
                        "case {case}: expected HTTP 200, got {answer:?}"
                    );
                    let url = answer
                        .body
                        .get(operation.url_member)
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let prefix = format!(
                        "https://{AUTHORITY}{}?SessionToken=",
                        operation.manifest_path
                    );
                    assert!(
                        url.starts_with(&prefix),
                        "case {case}: expected a URL starting {prefix:?}, got {url:?}"
                    );
                    let token = url
                        .rsplit("SessionToken=")
                        .next()
                        .unwrap_or_default()
                        .to_string();
                    assert!(
                        !token.is_empty(),
                        "case {case}: the URL must carry a session token, got {url:?}"
                    );

                    let state = service_state(&*engine).expect("the archived-media store");
                    let state = state.lock();
                    let session = state
                        .sessions
                        .get(&token)
                        .unwrap_or_else(|| panic!("case {case}: no session recorded for {token:?}"));
                    assert_eq!(
                        session.protocol, operation.protocol,
                        "case {case}: wrong protocol recorded, got {session:?}"
                    );
                    assert_eq!(
                        session.stream_name, stream,
                        "case {case}: wrong stream recorded, got {session:?}"
                    );
                    assert_eq!(
                        session.playback_mode, *playback_mode,
                        "case {case}: expected PlaybackMode {playback_mode}, got {session:?}"
                    );
                    assert_eq!(
                        session.expires_in_secs, *expires_in_secs,
                        "case {case}: expected Expires {expires_in_secs}, got {session:?}"
                    );
                    let expected_numbers: Vec<String> = fragments
                        .iter()
                        .map(|index| fragment_number(base, *index))
                        .collect();
                    assert_eq!(
                        session.fragment_numbers, expected_numbers,
                        "case {case}: expected the snapshot {expected_numbers:?}, got {session:?}"
                    );
                }
            }
        }
    }
}
