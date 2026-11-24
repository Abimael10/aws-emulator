// SPDX-License-Identifier: CC0-1.0

//! A deleted stream's archived media is gone with the stream.
//!
//! THE RULE UNDER TEST: **a stream exists for the archived-media read plane only
//! while the Kinesis Video Streams registry holds its record `ACTIVE`, and the
//! archive a read serves is the media an ingest wrote for THAT record — held
//! under the `StreamARN` the registry minted for it.** So the archive of a
//! record the registry no longer holds answers nothing (HTTP 404
//! `ResourceNotFoundException`, "The requested stream is not found or not
//! active."), and a stream created again under the same name — a different
//! stream, with a new ARN, since a stream's creation instant is part of it —
//! reads the empty archive it has rather than the removed incarnation's
//! fragments.
//!
//! Three crates have to agree for that to hold, so the suite lives here: the
//! control plane creates the stream, mints its ARN and removes the record its
//! `DeleteStream` marked, Kinesis Video Media's one ingest write records each
//! produced fragment on the archive read plane under the ARN the registry holds,
//! and Kinesis Video Archived Media is the plane that must stop serving it.
//!
//! The rows differ along what the rule generalises over: how much media the
//! removed stream held, which read is made (the listing, the media fetch and a
//! playback session over the same archive), how the request addresses its stream
//! (`StreamName`, `StreamARN`) and where in the delete lifecycle the read lands
//! (served, marked for deletion, removed, re-created).
//!
//! The ingest goes through the media plane's own one write
//! (`ingest_fragment`, which records the fragment on both read planes) rather
//! than over the `/putMedia` wire: what this suite is about is the LIFETIME of a
//! stream's archive, and the wire ingest — a Matroska container and its
//! acknowledgements — is covered by that crate's own suite and by
//! `kinesisvideo_clip_muxes_the_ingested_fragments`.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_kinesisvideo_kinesisvideo_20170930 as kvs;
use service_com_amazonaws_kinesisvideomedia_awsacuityinletservice as kv_media;

/// The region every call here is signed for.
const REGION: &str = "us-west-2";

/// The address every call arrives on: the loopback form a client on this machine
/// reaches the emulator at.
const AUTHORITY: &str = "127.0.0.1:9297";

/// The retention every stream here is created with, in hours, so no read is
/// refused for the stream retaining nothing.
const RETENTION_HOURS: i64 = 1;

/// The instant the first ingested fragment is stamped at, in epoch seconds: well
/// in the past, so no selector in this suite depends on the wall clock.
const T0_SECS: i64 = 1_761_000_000;

/// How far apart the ingested fragments are stamped, in seconds.
const SPACING_SECS: i64 = 10;

/// How long each ingested fragment plays, in milliseconds.
const FRAGMENT_DURATION_MILLIS: u64 = 2_000;

/// The sentence the archived-media plane refuses an unreadable stream with. It
/// states both halves of the condition and names neither the stream nor the
/// identifier form the request used.
const STREAM_NOT_FOUND: &str = "The requested stream is not found or not active.";

/// How long the suite keeps looking for a condition the clock owes — far longer
/// than any span this emulator uses, so a loaded host still passes.
const DEADLINE: Duration = Duration::from_secs(30);

/// How long it waits between two looks.
const LOOK_AGAIN: Duration = Duration::from_millis(10);

/// The emulator the suite drives: the Kinesis Video Streams control plane, the
/// ingest plane and the archived-media read plane, mounted on ONE engine exactly
/// as the router mounts them, with request authentication off so each call
/// carries the credential scope that names its Region and service without a
/// computed signature.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(kvs::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(kv_media::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(
        service_com_amazonaws_kinesisvideoarchivedmedia_awsacuityreader::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    (router, engine)
}

/// What one call answered.
#[derive(Debug)]
struct Answer {
    status: u16,
    /// The body as served.
    bytes: Vec<u8>,
}

impl Answer {
    /// The body as JSON, or `Null` for a body that is not.
    fn json(&self) -> Value {
        serde_json::from_slice(&self.bytes).unwrap_or(Value::Null)
    }

    /// The error code the envelope names, or `""` for a body that names none.
    fn code(&self) -> String {
        self.json()
            .get("__type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .rsplit('#')
            .next()
            .unwrap_or_default()
            .to_string()
    }

    /// The `Message` the envelope carries, or `""` for a body that carries none.
    fn message(&self) -> String {
        let body = self.json();
        body.get("Message")
            .or_else(|| body.get("message"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    /// Whether this answer is the plane's unreadable-stream refusal, whole: the
    /// status, the modelled error and the fixed sentence.
    fn is_stream_not_found(&self) -> bool {
        self.status == 404
            && self.code() == "ResourceNotFoundException"
            && self.message() == STREAM_NOT_FOUND
    }
}

/// POST one restJson1 call through the router.
async fn call(router: &MultiServiceRouter, path: &str, body: &Value) -> Answer {
    let request = ::http::Request::builder()
        .method("POST")
        .uri(format!("https://{AUTHORITY}{path}"))
        .header("host", AUTHORITY)
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20261123/{REGION}/kinesisvideo/\
                 aws4_request, SignedHeaders=host, Signature=not-verified"
            ),
        )
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    Answer {
        status: response.status().as_u16(),
        bytes: response.into_body().to_vec(),
    }
}

/// Create `stream`, wait until the control plane reports it `ACTIVE` ("CreateStream
/// is an asynchronous operation") and answer the `StreamARN` the registry minted
/// for it.
///
/// Waits for the CONDITION, not for a span: it looks again until the status holds,
/// under a deadline far longer than the transition needs.
async fn given_active_stream(router: &MultiServiceRouter, case: &str, stream: &str) -> String {
    let created = call(
        router,
        "/createStream",
        &json!({
            "StreamName": stream,
            "MediaType": "video/h264",
            "DataRetentionInHours": RETENTION_HOURS,
        }),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: CreateStream expected HTTP 200, got {created:?}"
    );
    let arn = created.json()["StreamARN"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no StreamARN in {created:?}"))
        .to_string();

    let deadline = Instant::now() + DEADLINE;
    let mut last = Value::Null;
    while Instant::now() < deadline {
        let described = call(router, "/describeStream", &json!({ "StreamName": stream })).await;
        if described.json()["StreamInfo"]["Status"].as_str() == Some("ACTIVE") {
            return arn;
        }
        last = described.json();
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    panic!("case {case:?}: {stream} never reached ACTIVE, last read {last:?}");
}

/// Ingest `count` fragments for `stream` through the media plane's one ingest
/// write, which records each of them on the archive read plane as well, and
/// answer the fragment numbers it minted.
fn ingest(
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    stream: &str,
    count: u64,
) -> Vec<String> {
    let state = kv_media::state::service_state(&**engine).expect("the media store");
    let mut store = state.lock();
    (0..count)
        .map(|index| {
            let offset = SPACING_SECS * i64::try_from(index).expect("a small index");
            let at = SystemTime::UNIX_EPOCH
                + Duration::from_secs(
                    u64::try_from(T0_SECS + offset).expect("an instant after the epoch"),
                );
            store
                .ingest_fragment(
                    &**engine,
                    Some(stream),
                    None,
                    kv_media::state::ProducedFragment {
                        chunk: produced_media(index),
                        producer_timestamp: at,
                        server_timestamp: at,
                        duration_millis: FRAGMENT_DURATION_MILLIS,
                    },
                )
                .unwrap_or_else(|| {
                    panic!("case {case:?}: a registered stream must take fragment {index}")
                })
        })
        .collect()
}

/// The media bytes of the `index`-th produced fragment, distinct per fragment so
/// one fragment's media can never be mistaken for another's.
fn produced_media(index: u64) -> Vec<u8> {
    format!("produced-fragment-{index}-media").into_bytes()
}

/// The timestamp range every selector in this suite asks for: wide enough to
/// cover every ingested fragment.
fn window() -> Value {
    json!({
        "StartTimestamp": T0_SECS - SPACING_SECS,
        "EndTimestamp": T0_SECS + SPACING_SECS * 1_000,
    })
}

/// The reads this suite makes of a stream's archive, each with a body that
/// reaches the operation's own verdict once the stream-naming member is added:
/// the listing, the media fetch of the fragments it reports, and a playback
/// session minted over the same archive.
fn reads(fragment_numbers: &[String]) -> Vec<(&'static str, Value)> {
    vec![
        (
            "/listFragments",
            json!({
                "FragmentSelector": {
                    "FragmentSelectorType": "PRODUCER_TIMESTAMP",
                    "TimestampRange": window(),
                },
            }),
        ),
        (
            "/getMediaForFragmentList",
            json!({ "Fragments": fragment_numbers }),
        ),
        (
            "/getHLSStreamingSessionURL",
            json!({
                "PlaybackMode": "ON_DEMAND",
                "HLSFragmentSelector": {
                    "FragmentSelectorType": "PRODUCER_TIMESTAMP",
                    "TimestampRange": window(),
                },
            }),
        ),
    ]
}

/// `base` with every member of `extra` added.
fn with_members(base: &Value, extra: &Value) -> Value {
    let mut object = base.as_object().cloned().unwrap_or_default();
    for (key, value) in extra.as_object().expect("an object of members") {
        object.insert(key.clone(), value.clone());
    }
    Value::Object(object)
}

/// How a request addresses its stream: both modelled forms, each naming the one
/// stream on its own.
fn addressing_forms(stream: &str, arn: &str) -> Vec<(&'static str, Value)> {
    vec![
        ("StreamName", json!({ "StreamName": stream })),
        ("StreamARN", json!({ "StreamARN": arn })),
    ]
}

/// Whether `payload` carries `needle` somewhere in it — how a client checks the
/// media it fetched is the media that was ingested.
fn carries(payload: &[u8], needle: &[u8]) -> bool {
    payload.windows(needle.len()).any(|window| window == needle)
}

/// RULE: once the registry no longer holds a stream's record, every archived-media
/// read of it refuses with the modelled missing-resource error — and a stream
/// created again under the same name reads the empty archive of the new record,
/// not the removed incarnation's fragments.
///
/// The rows differ along how much media the removed stream held.
#[tokio::test(flavor = "multi_thread")]
async fn a_removed_streams_archive_is_read_by_nobody() {
    let (router, engine) = emulator();

    // (the case, how many fragments were ingested before the delete)
    let rows: [(&str, u64); 3] = [
        ("a stream that held one fragment", 1),
        ("a stream that held three fragments", 3),
        ("a stream that held six fragments", 6),
    ];

    for (case, held) in rows {
        let stream = format!("deleted-archive-probe-{held}");
        let arn = given_active_stream(&router, case, &stream).await;
        let numbers = ingest(&engine, case, &stream, held);

        // THE BASELINE: while the registry holds the record, the archive the
        // ingest wrote is served — the listing reports every fragment and the
        // fetch returns the bytes the producer submitted.
        for (form, addressing) in addressing_forms(&stream, &arn) {
            for (path, body) in reads(&numbers) {
                let served = call(&router, path, &with_members(&body, &addressing)).await;
                assert_eq!(
                    served.status, 200,
                    "case {case:?}: {path} by {form} must serve the live stream, got {served:?}"
                );
            }
            let listed = call(
                &router,
                "/listFragments",
                &with_members(&reads(&numbers)[0].1, &addressing),
            )
            .await;
            assert_eq!(
                listed.json()["Fragments"].as_array().map_or(0, Vec::len) as u64,
                held,
                "case {case:?}: the listing by {form} must report every ingested fragment, got \
                 {listed:?}"
            );
            let fetched = call(
                &router,
                "/getMediaForFragmentList",
                &with_members(&reads(&numbers)[1].1, &addressing),
            )
            .await;
            for index in 0..held {
                assert!(
                    carries(&fetched.bytes, &produced_media(index)),
                    "case {case:?}: the media fetched by {form} must carry fragment {index}'s \
                     bytes, got {} bytes",
                    fetched.bytes.len()
                );
            }
        }

        // The delete marks the record; its removal falls due at an instant the
        // control plane fixed.
        let deleted = call(&router, "/deleteStream", &json!({ "StreamARN": arn })).await;
        assert_eq!(
            deleted.status, 200,
            "case {case:?}: DeleteStream expected HTTP 200, got {deleted:?}"
        );

        // FROM THE DELETE ONWARDS every read refuses, through the whole life of
        // the record: while the registry holds it as deleting, and after the
        // removal the delete made due, which the reads themselves settle.
        let deadline = Instant::now() + DEADLINE;
        let mut removed = false;
        while Instant::now() < deadline {
            for (form, addressing) in addressing_forms(&stream, &arn) {
                for (path, body) in reads(&numbers) {
                    let refused = call(&router, path, &with_members(&body, &addressing)).await;
                    assert!(
                        refused.is_stream_not_found(),
                        "case {case:?}: {path} by {form} on a deleted stream must be the modelled \
                         missing resource, got {refused:?}"
                    );
                }
            }
            let described =
                call(&router, "/describeStream", &json!({ "StreamName": stream })).await;
            if described.status == 404 {
                removed = true;
                break;
            }
            tokio::time::sleep(LOOK_AGAIN).await;
        }
        assert!(
            removed,
            "case {case:?}: the record the delete marked must be removed before the deadline"
        );

        // THE SAME NAME, A NEW STREAM: a fresh record with a fresh ARN, whose
        // archive is empty until something is ingested into it.
        let recreated_arn = given_active_stream(&router, case, &stream).await;
        assert_ne!(
            recreated_arn, arn,
            "case {case:?}: a re-created stream must carry a new ARN"
        );
        for (form, addressing) in addressing_forms(&stream, &recreated_arn) {
            let listed = call(
                &router,
                "/listFragments",
                &with_members(&reads(&numbers)[0].1, &addressing),
            )
            .await;
            assert_eq!(
                listed.status, 200,
                "case {case:?}: the listing by {form} of a registered stream with an empty \
                 archive is a success, got {listed:?}"
            );
            let fragments = listed.json()["Fragments"]
                .as_array()
                .cloned()
                .unwrap_or_else(|| panic!("case {case:?}: no Fragments in {listed:?}"));
            assert!(
                fragments.is_empty(),
                "case {case:?}: the re-created stream read by {form} must report none of the \
                 removed incarnation's fragments, got {fragments:?}"
            );

            let fetched = call(
                &router,
                "/getMediaForFragmentList",
                &with_members(&reads(&numbers)[1].1, &addressing),
            )
            .await;
            assert_eq!(
                fetched.status, 400,
                "case {case:?}: the removed incarnation's fragment numbers name no fragment of \
                 the new stream, got {fetched:?}"
            );
            assert_eq!(
                fetched.code(),
                "InvalidArgumentException",
                "case {case:?}: expected InvalidArgumentException by {form}, got {fetched:?}"
            );
            let expected = format!("Fragment numbers are invalid : [{}]", numbers.join(", "));
            assert_eq!(
                fetched.message(),
                expected,
                "case {case:?}: expected {expected:?} by {form}, got {fetched:?}"
            );
            for index in 0..held {
                assert!(
                    !carries(&fetched.bytes, &produced_media(index)),
                    "case {case:?}: no answer to the new stream may carry the removed \
                     incarnation's fragment {index}"
                );
            }
        }
    }
}
