// SPDX-License-Identifier: CC0-1.0

//! `GetMedia`'s `PRODUCER_TIMESTAMP` and `SERVER_TIMESTAMP` start selectors over a log
//! whose fragments can SHARE an instant.
//!
//! The rule under test: a timestamp start selector starts the session with the FIRST
//! chunk carrying the newest timestamp that is not after the requested instant — the
//! earliest chunk that contains a fragment with that timestamp — so every chunk stamped
//! with the requested instant is delivered, and an instant older than every fragment
//! starts at the earliest chunk.
//!
//! The table spans the shapes a log's timestamps take — one instant shared by every
//! fragment, runs of equal instants followed by a singleton, all-distinct instants, and
//! a shared newest instant — crossed with where the requested instant falls (on a
//! fragment, inside its span, before every fragment, after every fragment) and with both
//! timestamp selectors. The two end-to-end cases drive the same rule through the real
//! ingest plane, where one `/putMedia` session stamps every fragment it takes with ONE
//! server instant and two Clusters may carry one timecode.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use service_com_amazonaws_kinesisvideo_kinesisvideo_20170930 as control_plane;
use service_com_amazonaws_kinesisvideomedia_awsacuityinletservice as media;
use smithy_server_runtime::MultiServiceRouter;

use crate::get_media::split_payload;
use crate::put_media_ingest::{cluster, ingest_headers, Container};

/// The signing region every request in this suite uses.
const REGION: &str = "us-east-1";

/// The address every request in this suite arrives on.
const HOST: &str = "kinesisvideo.us-east-1.amazonaws.com";

/// The epoch instant the seeded tables are laid out from, on a whole second.
const BASE_MILLIS: u64 = 1_700_000_000_000;

/// Where the stamps of the field the selector does NOT read start: far enough after
/// every requested instant that a handler reading the wrong field would resolve the
/// cursor to the earliest chunk instead.
const DECOY_OFFSET_MILLIS: u64 = 1_000_000;

/// An epoch-millisecond instant as the epoch-seconds number restJson1 binds a timestamp
/// from. Every instant this suite sends is a multiple of a quarter second, which an `f64`
/// carries exactly.
fn as_epoch_seconds(millis: u64) -> serde_json::Value {
    serde_json::json!(millis as f64 / 1000.0)
}

/// One router over one engine carrying the control plane that creates a stream and this
/// ingest/media service that writes and reads its media.
struct Harness {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Harness {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.register_service(control_plane::EmulatedService::new(Arc::clone(&engine)));
        router.register_service(media::EmulatedService::new(Arc::clone(&engine)));
        Self { router, engine }
    }

    /// The SigV4 credential scope every request carries: its service field is the routing
    /// key the Kinesis Video services share.
    fn authorization() -> String {
        format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20261123/{REGION}/kinesisvideo/\
             aws4_request, SignedHeaders=host, Signature=0000"
        )
    }

    async fn call(&self, path: &str, body: &serde_json::Value) -> http::Response<Bytes> {
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("https://{HOST}{path}"))
            .header("host", HOST)
            .header("content-type", "application/json")
            .header("authorization", Self::authorization())
            .body(Bytes::from(
                serde_json::to_vec(body).expect("the request body serializes"),
            ))
            .expect("a well-formed request");
        self.router.dispatch(request).await
    }

    /// POST one ingest session: the headers a producer sends and the container as its
    /// body.
    async fn put_media(
        &self,
        headers: &[(&'static str, String)],
        body: Vec<u8>,
    ) -> http::Response<Bytes> {
        let mut request = http::Request::builder()
            .method("POST")
            .uri(format!("https://{HOST}/putMedia"))
            .header("host", HOST)
            .header("authorization", Self::authorization());
        for (name, value) in headers {
            request = request.header(*name, value.clone());
        }
        self.router
            .dispatch(
                request
                    .body(Bytes::from(body))
                    .expect("a well-formed request"),
            )
            .await
    }

    /// Create `name` through the control plane and answer the `StreamARN` it minted.
    async fn create_stream(&self, name: &str) -> String {
        let response = self
            .call("/createStream", &serde_json::json!({ "StreamName": name }))
            .await;
        assert_eq!(
            response.status().as_u16(),
            200,
            "case \"create {name}\": expected 200, got {} {}",
            response.status().as_u16(),
            text(&response)
        );
        json(&response)
            .get("StreamARN")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_else(|| panic!("case \"create {name}\": no StreamARN in the answer"))
            .to_string()
    }

    /// Seed one fragment per `(producer, server)` millisecond pair into the stream's media
    /// log through the producer entry point, and answer the chunk bytes each was seeded
    /// with.
    fn seed(&self, stream_name: &str, stamps: &[(u64, u64)]) -> Vec<Vec<u8>> {
        let state = media::state::service_state(&*self.engine).expect("the media store");
        let mut store = state.lock();
        stamps
            .iter()
            .enumerate()
            .map(|(index, (producer, server))| {
                let chunk = format!("chunk-{index}-of-{stream_name}").into_bytes();
                store
                    .put_fragment(
                        &*self.engine,
                        Some(stream_name),
                        None,
                        chunk.clone(),
                        at(*producer),
                        at(*server),
                    )
                    .expect("the created stream accepts produced media");
                chunk
            })
            .collect()
    }

    /// The chunks this service's media log holds for the stream `arn` names, in order.
    fn stored_chunks(&self, arn: &str) -> Vec<Vec<u8>> {
        let state = media::state::service_state(&*self.engine).expect("the media store");
        let store = state.lock();
        store
            .log_by_arn(arn)
            .map(|log| {
                log.fragments
                    .iter()
                    .map(|fragment| fragment.chunk.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// `GetMedia` with `selector`, naming the stream by name.
    async fn get_media(
        &self,
        stream_name: &str,
        selector: serde_json::Value,
    ) -> http::Response<Bytes> {
        self.call(
            "/getMedia",
            &serde_json::json!({ "StreamName": stream_name, "StartSelector": selector }),
        )
        .await
    }
}

fn at(millis: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_millis(millis)
}

fn json(response: &http::Response<Bytes>) -> serde_json::Value {
    serde_json::from_slice(response.body()).unwrap_or(serde_json::Value::Null)
}

fn text(response: &http::Response<Bytes>) -> String {
    String::from_utf8_lossy(response.body()).to_string()
}

fn error_code(response: &http::Response<Bytes>) -> String {
    response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

/// The `(fragment number, server timestamp, producer timestamp)` of every chunk a payload
/// delivered, read out of the chunks' own MKV tags, given the media bytes the expected
/// chunks hold.
fn delivered(case: &str, payload: &[u8], expected_media: &[Vec<u8>]) -> Vec<(String, u64, u64)> {
    let borrowed: Vec<&[u8]> = expected_media.iter().map(Vec::as_slice).collect();
    split_payload(case, payload, &borrowed)
        .iter()
        .map(|chunk| {
            let millis = |name: &str| chunk.tag(name).parse::<u64>().unwrap_or_default();
            (
                chunk.tag("AWS_KINESISVIDEO_FRAGMENT_NUMBER"),
                millis("AWS_KINESISVIDEO_SERVER_TIMESTAMP"),
                millis("AWS_KINESISVIDEO_PRODUCER_TIMESTAMP"),
            )
        })
        .collect()
}

/// One seeded case: the stamps the selector's own field carries, the instant requested,
/// and the index of the first fragment the session must deliver.
struct Row {
    case: &'static str,
    /// Milliseconds after [`BASE_MILLIS`], one per fragment, in log order.
    stamps: &'static [u64],
    /// The requested instant, in milliseconds after [`BASE_MILLIS`].
    wanted: u64,
    expected_start: usize,
}

/// RULE: a timestamp start selector starts with the FIRST chunk carrying the newest
/// timestamp that is not after the requested instant, so a run of fragments sharing one
/// instant is delivered whole; an instant older than every fragment starts at the
/// earliest chunk.
#[tokio::test]
async fn a_timestamp_selector_starts_at_the_first_chunk_carrying_the_requested_instant() {
    // One instant shared by every fragment — the shape this crate's own ingest plane
    // writes, stamping a whole session with one server instant.
    const ALL_SHARED: &[u64] = &[10_000, 10_000, 10_000];
    // Two runs of equal instants, then a singleton.
    const RUNS: &[u64] = &[10_000, 10_000, 14_000, 14_000, 18_000];
    // All-distinct instants: the already-correct shape, which must not move.
    const DISTINCT: &[u64] = &[10_000, 12_000, 14_000];
    // Only the NEWEST instant is shared.
    const NEWEST_SHARED: &[u64] = &[10_000, 12_000, 12_000];

    let rows = [
        Row {
            case: "one shared instant, requested exactly",
            stamps: ALL_SHARED,
            wanted: 10_000,
            expected_start: 0,
        },
        Row {
            case: "one shared instant, requested after it",
            stamps: ALL_SHARED,
            wanted: 12_500,
            expected_start: 0,
        },
        Row {
            case: "one shared instant, requested before it",
            stamps: ALL_SHARED,
            wanted: 5_000,
            expected_start: 0,
        },
        Row {
            case: "runs: the first run's instant, requested exactly",
            stamps: RUNS,
            wanted: 10_000,
            expected_start: 0,
        },
        Row {
            case: "runs: inside the first run's span",
            stamps: RUNS,
            wanted: 12_000,
            expected_start: 0,
        },
        Row {
            case: "runs: the second run's instant, requested exactly",
            stamps: RUNS,
            wanted: 14_000,
            expected_start: 2,
        },
        Row {
            case: "runs: inside the second run's span",
            stamps: RUNS,
            wanted: 15_250,
            expected_start: 2,
        },
        Row {
            case: "runs: the singleton's instant, requested exactly",
            stamps: RUNS,
            wanted: 18_000,
            expected_start: 4,
        },
        Row {
            case: "runs: after every fragment",
            stamps: RUNS,
            wanted: 30_000,
            expected_start: 4,
        },
        Row {
            case: "distinct: the second fragment's instant, requested exactly",
            stamps: DISTINCT,
            wanted: 12_000,
            expected_start: 1,
        },
        Row {
            case: "distinct: inside the second fragment's span",
            stamps: DISTINCT,
            wanted: 12_500,
            expected_start: 1,
        },
        Row {
            case: "distinct: after every fragment",
            stamps: DISTINCT,
            wanted: 30_000,
            expected_start: 2,
        },
        Row {
            case: "distinct: before every fragment",
            stamps: DISTINCT,
            wanted: 5_000,
            expected_start: 0,
        },
        Row {
            case: "a shared newest instant, requested exactly",
            stamps: NEWEST_SHARED,
            wanted: 12_000,
            expected_start: 1,
        },
        Row {
            case: "a shared newest instant, requested after every fragment",
            stamps: NEWEST_SHARED,
            wanted: 30_000,
            expected_start: 1,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        for selector_type in ["PRODUCER_TIMESTAMP", "SERVER_TIMESTAMP"] {
            let case = format!("{} [{selector_type}]", row.case);
            let harness = Harness::new();
            let stream_name = format!("ts-cursor-{index}-{selector_type}");
            harness.create_stream(&stream_name).await;

            // The field the selector does NOT read carries distinct stamps far after
            // every requested instant, so a read of the wrong field cannot pass.
            let stamps: Vec<(u64, u64)> = row
                .stamps
                .iter()
                .enumerate()
                .map(|(fragment, offset)| {
                    let own = BASE_MILLIS + offset;
                    let decoy =
                        BASE_MILLIS + DECOY_OFFSET_MILLIS + (fragment as u64) * 1_000;
                    if selector_type == "PRODUCER_TIMESTAMP" {
                        (own, decoy)
                    } else {
                        (decoy, own)
                    }
                })
                .collect();
            let chunks = harness.seed(&stream_name, &stamps);

            let response = harness
                .get_media(
                    &stream_name,
                    serde_json::json!({
                        "StartSelectorType": selector_type,
                        "StartTimestamp": as_epoch_seconds(BASE_MILLIS + row.wanted),
                    }),
                )
                .await;
            assert_eq!(
                response.status().as_u16(),
                200,
                "case {case:?}: expected 200, got {} ({})",
                response.status().as_u16(),
                error_code(&response)
            );

            let expected_media: Vec<Vec<u8>> = chunks[row.expected_start..].to_vec();
            let got = delivered(&case, response.body(), &expected_media);
            let want: Vec<String> = (row.expected_start..row.stamps.len())
                .map(|fragment| (fragment + 1).to_string())
                .collect();
            let numbers: Vec<String> = got.iter().map(|(number, _, _)| number.clone()).collect();
            assert_eq!(
                numbers, want,
                "case {case:?}: expected fragments {want:?}, got {numbers:?}"
            );

            // And the chunk the session starts with really is stamped with the instant
            // the cursor was resolved from: the newest stamp not after the request.
            let expected_stamp = BASE_MILLIS
                + row
                    .stamps
                    .iter()
                    .copied()
                    .filter(|offset| *offset <= row.wanted)
                    .max()
                    .unwrap_or(row.stamps[0]);
            let first = got
                .first()
                .unwrap_or_else(|| panic!("case {case:?}: the session delivered no chunk"));
            let actual = if selector_type == "PRODUCER_TIMESTAMP" {
                first.2
            } else {
                first.1
            };
            assert_eq!(
                actual, expected_stamp,
                "case {case:?}: expected the first delivered chunk stamped {expected_stamp}, \
                 got {actual}"
            );
        }
    }
}

/// RULE, through the real ingest plane: one `/putMedia` session stamps every fragment it
/// takes with ONE server instant, so a `SERVER_TIMESTAMP` read at that instant delivers
/// the whole session rather than its newest fragment alone.
#[tokio::test]
async fn one_ingest_sessions_fragments_are_read_from_the_first_of_their_shared_server_instant()
{
    let harness = Harness::new();
    let stream_name = "shared-server-instant";
    let arn = harness.create_stream(stream_name).await;

    // The producer start timestamp: a whole second in the past, as a producer's first
    // fragment is stamped.
    let producer_start = SystemTime::UNIX_EPOCH + Duration::from_millis(1_761_000_000_000);
    let container = Container::of(vec![
        cluster(0, 2, 33, 0xA1),
        cluster(1000, 2, 33, 0xB2),
        cluster(2000, 2, 33, 0xC3),
    ]);
    let response = harness
        .put_media(
            &ingest_headers(
                ("x-amzn-stream-name", stream_name.to_string()),
                "RELATIVE",
                producer_start,
            ),
            container.bytes(),
        )
        .await;
    assert_eq!(
        response.status().as_u16(),
        200,
        "case \"ingest three clusters\": expected 200, got {} ({}) {}",
        response.status().as_u16(),
        error_code(&response),
        text(&response)
    );

    let stored = harness.stored_chunks(&arn);
    assert_eq!(
        stored.len(),
        3,
        "case \"ingest three clusters\": expected three persisted fragments, got {}",
        stored.len()
    );

    // What the session itself reports: every fragment of it carries the SAME server
    // instant, which is the premise the selector has to serve.
    let response = harness
        .get_media(stream_name, serde_json::json!({"StartSelectorType": "EARLIEST"}))
        .await;
    assert_eq!(
        response.status().as_u16(),
        200,
        "case \"read the whole session\": expected 200, got {} ({})",
        response.status().as_u16(),
        error_code(&response)
    );
    let chunks = delivered("read the whole session", response.body(), &stored);
    let instants: Vec<u64> = chunks.iter().map(|(_, server, _)| *server).collect();
    assert!(
        instants.iter().all(|instant| *instant == instants[0]),
        "case \"read the whole session\": expected one server instant for the session, \
         got {instants:?}"
    );

    // The requested instant, named in whole seconds — the exact form an epoch-seconds
    // timestamp takes — one second past the instant the session reported. Every fragment
    // of the session is stamped before it, so the newest stamp not after it is the
    // session's own, and the read starts at the FIRST fragment carrying it.
    let wanted = (instants[0] / 1000 + 1) * 1000;
    let response = harness
        .get_media(
            stream_name,
            serde_json::json!({
                "StartSelectorType": "SERVER_TIMESTAMP",
                "StartTimestamp": as_epoch_seconds(wanted),
            }),
        )
        .await;
    assert_eq!(
        response.status().as_u16(),
        200,
        "case \"SERVER_TIMESTAMP on the session's instant\": expected 200, got {} ({})",
        response.status().as_u16(),
        error_code(&response)
    );
    let got = delivered(
        "SERVER_TIMESTAMP on the session's instant",
        response.body(),
        &stored,
    );
    let numbers: Vec<String> = got.iter().map(|(number, _, _)| number.clone()).collect();
    assert_eq!(
        numbers,
        vec!["1".to_string(), "2".to_string(), "3".to_string()],
        "case \"SERVER_TIMESTAMP on the session's instant\": expected the whole session, \
         got {numbers:?}"
    );
}

/// RULE, through the real ingest plane: two Clusters carrying ONE timecode become two
/// fragments with one producer instant, and a `PRODUCER_TIMESTAMP` read at that instant
/// starts with the first of them.
#[tokio::test]
async fn two_clusters_on_one_timecode_are_both_read_from_their_producer_instant() {
    let harness = Harness::new();
    let stream_name = "shared-producer-instant";
    let arn = harness.create_stream(stream_name).await;

    let start_millis = 1_761_000_000_000u64;
    let producer_start = SystemTime::UNIX_EPOCH + Duration::from_millis(start_millis);
    // The third Cluster repeats the second's timecode, which the ingest plane accepts:
    // only a timecode moving BACKWARDS is a fault.
    let container = Container::of(vec![
        cluster(0, 2, 20, 0x11),
        cluster(1000, 2, 20, 0x22),
        cluster(1000, 2, 20, 0x33),
    ]);
    let response = harness
        .put_media(
            &ingest_headers(
                ("x-amzn-stream-name", stream_name.to_string()),
                "RELATIVE",
                producer_start,
            ),
            container.bytes(),
        )
        .await;
    assert_eq!(
        response.status().as_u16(),
        200,
        "case \"ingest two clusters on one timecode\": expected 200, got {} ({}) {}",
        response.status().as_u16(),
        error_code(&response),
        text(&response)
    );

    let stored = harness.stored_chunks(&arn);
    assert_eq!(
        stored.len(),
        3,
        "case \"ingest two clusters on one timecode\": expected three persisted fragments, \
         got {}",
        stored.len()
    );

    // The shared producer instant is the second Cluster's: the read must start at the
    // first fragment carrying it, not at the last.
    let rows: [(&str, u64, usize); 2] = [
        ("PRODUCER_TIMESTAMP on the shared instant", start_millis + 1000, 1),
        ("PRODUCER_TIMESTAMP on the first fragment", start_millis, 0),
    ];
    for (case, wanted, expected_start) in rows {
        let response = harness
            .get_media(
                stream_name,
                serde_json::json!({
                    "StartSelectorType": "PRODUCER_TIMESTAMP",
                    "StartTimestamp": as_epoch_seconds(wanted),
                }),
            )
            .await;
        assert_eq!(
            response.status().as_u16(),
            200,
            "case {case:?}: expected 200, got {} ({})",
            response.status().as_u16(),
            error_code(&response)
        );
        let expected_media: Vec<Vec<u8>> = stored[expected_start..].to_vec();
        let got = delivered(case, response.body(), &expected_media);
        let numbers: Vec<String> = got.iter().map(|(number, _, _)| number.clone()).collect();
        let want: Vec<String> = (expected_start..stored.len())
            .map(|fragment| (fragment + 1).to_string())
            .collect();
        assert_eq!(
            numbers, want,
            "case {case:?}: expected fragments {want:?}, got {numbers:?}"
        );
        // Nothing older than the requested instant is delivered: the cursor moved to it,
        // it did not fall back to the earliest chunk.
        for (number, _, producer) in &got {
            assert!(
                *producer >= wanted,
                "case {case:?}: fragment {number} carries producer instant {producer}, \
                 which is before the requested {wanted}"
            );
        }
    }
}
