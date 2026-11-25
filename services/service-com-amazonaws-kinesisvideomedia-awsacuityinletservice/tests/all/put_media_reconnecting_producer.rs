// SPDX-License-Identifier: CC0-1.0

//! What a producer that RECONNECTS ingests: every real producer opens a new `PutMedia`
//! session on a reconnect, and a RELATIVE session's Cluster timecodes are offsets from
//! THAT session's `x-amzn-producer-start-timestamp`, so a fresh connection legitimately
//! restarts them near zero.
//!
//! The rule under test: a Cluster's timecode is read against the session that submitted
//! it, so the in-band `FRAGMENT_TIMECODE_LESSER_THAN_PREVIOUS` (4004) fault is about that
//! session's OWN run of timecodes — a Cluster behind the one its own session submitted
//! ahead of it — and never about a new connection restarting its run. A reconnecting
//! session is therefore taken whole: `200` with one `PERSISTED` acknowledgement per
//! Cluster, each carrying a fragment number strictly greater than every number the stream
//! has already minted, no `ERROR` acknowledgement, and the stream's log holding every
//! fragment of every session.
//!
//! A container fault of the session's own is still reported in band, which is what the
//! last rows pin: a block on a track the container's `Tracks` never declared is
//! `TRACK_NUMBER_MISMATCH` (4010) with no `PERSISTED` acknowledgement for that fragment,
//! on a reconnecting session exactly as on a first one, and the log is left as it was.
//!
//! The rows differ along what the rule generalises over: how many times the producer
//! reconnects, how many Clusters each session carries, the timecodes it restarts at, and
//! the timecode type the sessions declare (`RELATIVE` sessions restarting their offsets,
//! and an `ABSOLUTE` session whose instants the log already covers).
//!
//! Every assertion goes over the wire through the router the production emulator mounts,
//! reusing the container builders in [`crate::put_media_ingest`], so an acknowledgement
//! that is not computed from the persisted log cannot pass.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use service_com_amazonaws_kinesisvideo_kinesisvideo_20170930 as control_plane;
use service_com_amazonaws_kinesisvideomedia_awsacuityinletservice as media;
use smithy_server_runtime::MultiServiceRouter;

use crate::put_media_ingest::{cluster, cluster_on_track, ingest_headers, Container};

/// The signing region every request in this suite uses.
const REGION: &str = "us-east-1";

/// The address every request in this suite arrives on.
const HOST: &str = "kinesisvideo.us-east-1.amazonaws.com";

/// The epoch instant the sessions are laid out from: a whole second in the past, as a
/// producer's own capture instants are.
const BASE_MILLIS: u64 = 1_761_000_000_000;

/// The frames every Cluster in this suite carries.
const FRAMES_PER_CLUSTER: usize = 2;

/// The milliseconds between the frames of one Cluster.
const FRAME_SPACING_MILLIS: i16 = 30;

/// The track number the container declares in its `Tracks` element, which
/// [`cluster`] writes its blocks on.
const DECLARED_TRACK: u64 = 1;

/// A track number the container's `Tracks` element does NOT declare.
const UNDECLARED_TRACK: u64 = 2;

fn at(millis: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_millis(millis)
}

/// One router over one engine carrying the control plane that creates the stream and the
/// ingest/media service that takes and reports its media.
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
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20261124/{REGION}/kinesisvideo/\
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

    /// Create `name` through the control plane and wait until it is reported `ACTIVE`, as
    /// a producer does before writing to a stream it just created.
    async fn create_stream(&self, name: &str) -> String {
        let response = self
            .call(
                "/createStream",
                &serde_json::json!({ "StreamName": name, "DataRetentionInHours": 1 }),
            )
            .await;
        assert_eq!(
            status(&response),
            200,
            "case \"create {name}\": expected HTTP 200, got {} {}",
            status(&response),
            text(&response)
        );
        let arn = json(&response)
            .get("StreamARN")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_else(|| panic!("case \"create {name}\": no StreamARN in the answer"))
            .to_string();
        // Waits for the CONDITION, not for a span: it looks again until the status holds,
        // under a deadline far longer than the transition needs.
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        let mut last = String::new();
        while std::time::Instant::now() < deadline {
            let described = self
                .call("/describeStream", &serde_json::json!({ "StreamName": name }))
                .await;
            last = json(&described)["StreamInfo"]["Status"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            if last == "ACTIVE" {
                return arn;
            }
            tokio::task::yield_now().await;
        }
        panic!("case \"create {name}\": expected Status ACTIVE before the deadline, got {last:?}");
    }

    /// The producer instants the media log holds for the stream `arn` names, in log order,
    /// in epoch milliseconds.
    fn stored_instants(&self, arn: &str) -> Vec<u64> {
        let state = media::state::service_state(&*self.engine).expect("the media store");
        let store = state.lock();
        store
            .log_by_arn(arn)
            .map(|log| {
                log.fragments
                    .iter()
                    .map(|fragment| {
                        fragment
                            .producer_timestamp
                            .duration_since(SystemTime::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

fn status(response: &http::Response<Bytes>) -> u16 {
    response.status().as_u16()
}

fn json(response: &http::Response<Bytes>) -> serde_json::Value {
    serde_json::from_slice(response.body()).unwrap_or(serde_json::Value::Null)
}

fn text(response: &http::Response<Bytes>) -> String {
    String::from_utf8_lossy(response.body()).to_string()
}

/// The acknowledgements an ingest answer carries, in order: one JSON object per line,
/// which is what a producer reads off this wire.
fn acknowledgements(case: &str, response: &http::Response<Bytes>) -> Vec<serde_json::Value> {
    text(response)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str(line).unwrap_or_else(|error| {
                panic!("case {case:?}: an acknowledgement is not JSON ({error}): {line:?}")
            })
        })
        .collect()
}

/// The `EventType` of one acknowledgement.
fn event_type(acknowledgement: &serde_json::Value) -> String {
    acknowledgement
        .get("EventType")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The fragment numbers the `PERSISTED` acknowledgements carry, in order.
fn persisted_numbers(acknowledgements: &[serde_json::Value]) -> Vec<String> {
    acknowledgements
        .iter()
        .filter(|acknowledgement| event_type(acknowledgement) == "PERSISTED")
        .map(|acknowledgement| {
            acknowledgement
                .get("FragmentNumber")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// Every `ERROR` acknowledgement, as `(ErrorCode, ErrorId)`.
fn faults(acknowledgements: &[serde_json::Value]) -> Vec<(String, u64)> {
    acknowledgements
        .iter()
        .filter(|acknowledgement| event_type(acknowledgement) == "ERROR")
        .map(|acknowledgement| {
            (
                acknowledgement
                    .get("ErrorCode")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                acknowledgement
                    .get("ErrorId")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or_default(),
            )
        })
        .collect()
}

/// A container of one Cluster per timecode, each block on `track` and marked so one
/// Cluster's frames cannot be mistaken for another's.
fn container_on_track(timecodes: &[u64], track: u64, marker: u8) -> Container {
    Container::of(
        timecodes
            .iter()
            .enumerate()
            .map(|(index, timecode)| {
                let index = index as u8;
                if track == DECLARED_TRACK {
                    cluster(*timecode, FRAMES_PER_CLUSTER, FRAME_SPACING_MILLIS, marker ^ index)
                } else {
                    cluster_on_track(
                        *timecode,
                        track,
                        FRAMES_PER_CLUSTER,
                        FRAME_SPACING_MILLIS,
                        marker ^ index,
                    )
                }
            })
            .collect(),
    )
}

/// Submit one session on its own connection and answer its response.
async fn submit(
    harness: &Harness,
    stream: &str,
    timecode_type: &'static str,
    producer_start_millis: u64,
    timecodes: &[u64],
    track: u64,
    marker: u8,
) -> http::Response<Bytes> {
    harness
        .put_media(
            &ingest_headers(
                ("x-amzn-stream-name", stream.to_string()),
                timecode_type,
                at(producer_start_millis),
            ),
            container_on_track(timecodes, track, marker).bytes(),
        )
        .await
}

/// One reconnecting producer: the sessions it opens and the instants its stream ends up
/// holding.
struct Reconnect {
    name: &'static str,
    stream: &'static str,
    timecode_type: &'static str,
    /// One `(producer start in epoch milliseconds, Cluster timecodes)` pair per session,
    /// in the order the producer opens them.
    sessions: Vec<(u64, Vec<u64>)>,
    /// The producer instants the log holds once every session is over, in log order.
    expected_log_millis: Vec<u64>,
}

fn reconnects() -> Vec<Reconnect> {
    vec![
        Reconnect {
            // The recorded case: two RELATIVE connections, the second restarting its
            // offsets at 0 with a producer start of its own, a second after the first.
            name: "two RELATIVE sessions, the second restarting at 0",
            stream: "reconnect-two-relative",
            timecode_type: "RELATIVE",
            sessions: vec![
                (BASE_MILLIS, vec![0, 1_000]),
                (BASE_MILLIS + 1_000, vec![0, 1_000]),
            ],
            expected_log_millis: vec![
                BASE_MILLIS,
                BASE_MILLIS + 1_000,
                BASE_MILLIS + 1_000,
                BASE_MILLIS + 2_000,
            ],
        },
        Reconnect {
            // The reconnect every producer SDK makes: a new session whose producer start
            // is the same instant the previous one declared, so its restarted offsets name
            // instants the log already covers.
            name: "a RELATIVE session restarting on the same producer start",
            stream: "reconnect-same-start",
            timecode_type: "RELATIVE",
            sessions: vec![
                (BASE_MILLIS, vec![0, 1_000, 2_000]),
                (BASE_MILLIS, vec![0, 500]),
            ],
            expected_log_millis: vec![
                BASE_MILLIS,
                BASE_MILLIS,
                BASE_MILLIS + 500,
                BASE_MILLIS + 1_000,
                BASE_MILLIS + 2_000,
            ],
        },
        Reconnect {
            // Three sessions, one Cluster each, all restarting at 0: a producer that keeps
            // dropping its connection still archives every fragment.
            name: "three one-Cluster RELATIVE sessions",
            stream: "reconnect-three-sessions",
            timecode_type: "RELATIVE",
            sessions: vec![
                (BASE_MILLIS + 2_000, vec![0]),
                (BASE_MILLIS + 1_000, vec![0]),
                (BASE_MILLIS + 3_000, vec![0]),
            ],
            expected_log_millis: vec![
                BASE_MILLIS + 1_000,
                BASE_MILLIS + 2_000,
                BASE_MILLIS + 3_000,
            ],
        },
        Reconnect {
            // An ABSOLUTE producer names the instants itself, and a reconnecting session
            // may name instants inside the span the log already holds.
            name: "an ABSOLUTE session inside the span already held",
            stream: "reconnect-absolute-inside",
            timecode_type: "ABSOLUTE",
            sessions: vec![
                (BASE_MILLIS, vec![BASE_MILLIS, BASE_MILLIS + 10_000]),
                (BASE_MILLIS, vec![BASE_MILLIS + 5_000]),
            ],
            expected_log_millis: vec![
                BASE_MILLIS,
                BASE_MILLIS + 5_000,
                BASE_MILLIS + 10_000,
            ],
        },
    ]
}

#[tokio::test]
async fn a_reconnecting_producers_session_appends_to_the_same_stream() {
    for case in reconnects() {
        let name = case.name;
        let harness = Harness::new();
        let arn = harness.create_stream(case.stream).await;

        let mut minted: Vec<String> = Vec::new();
        for (index, (producer_start, timecodes)) in case.sessions.iter().enumerate() {
            let session = format!("{name} / session {}", index + 1);
            let response = submit(
                &harness,
                case.stream,
                case.timecode_type,
                *producer_start,
                timecodes,
                DECLARED_TRACK,
                0x10 + index as u8,
            )
            .await;
            assert_eq!(
                status(&response),
                200,
                "case {session:?}: expected HTTP 200 from the ingest plane, got {} {}",
                status(&response),
                text(&response)
            );
            let acks = acknowledgements(&session, &response);
            let reported = faults(&acks);
            assert_eq!(
                reported,
                Vec::<(String, u64)>::new(),
                "case {session:?}: a session restarting its own run of timecodes carries no \
                 ERROR acknowledgement, got {}",
                text(&response)
            );
            let numbers = persisted_numbers(&acks);
            assert_eq!(
                numbers.len(),
                timecodes.len(),
                "case {session:?}: expected one PERSISTED acknowledgement per Cluster, got {}",
                text(&response)
            );
            // Every number this session minted is greater than every number the stream had
            // already minted: the fragments of a later session extend the stream.
            let highest = minted
                .iter()
                .map(|number| number.parse::<u64>().unwrap_or_default())
                .max();
            for number in &numbers {
                let value = number.parse::<u64>().unwrap_or_else(|_| {
                    panic!("case {session:?}: a fragment number must be numeric, got {number:?}")
                });
                assert!(
                    highest.is_none_or(|held| value > held),
                    "case {session:?}: expected a fragment number above every number already \
                     minted ({highest:?}), got {value}"
                );
            }
            minted.extend(numbers);
        }

        let held = harness.stored_instants(&arn);
        assert_eq!(
            held, case.expected_log_millis,
            "case {name:?}: expected the log to hold the producer instants {:?}, got {held:?}",
            case.expected_log_millis
        );
        assert!(
            held.windows(2).all(|pair| pair[0] <= pair[1]),
            "case {name:?}: the log must stay in capture order, got {held:?}"
        );
        assert_eq!(
            minted.len(),
            case.expected_log_millis.len(),
            "case {name:?}: every acknowledged fragment must be in the log, got {minted:?}"
        );
    }
}

#[tokio::test]
async fn a_block_on_an_undeclared_track_is_refused_in_band_on_any_session() {
    // Rows: the undeclared-track container as the FIRST session a stream ever takes, and
    // as a reconnecting session restarting its RELATIVE offsets at 0 — the fault is the
    // container's, so which session carries it cannot change the answer.
    let rows: [(&str, &str, bool); 2] = [
        ("the first session of the stream", "undeclared-first", false),
        (
            "a reconnecting session restarting at 0",
            "undeclared-reconnect",
            true,
        ),
    ];

    for (name, stream, after_a_session) in rows {
        let harness = Harness::new();
        let arn = harness.create_stream(stream).await;

        let mut expected_log: Vec<u64> = Vec::new();
        if after_a_session {
            let response = submit(
                &harness,
                stream,
                "RELATIVE",
                BASE_MILLIS,
                &[0, 1_000],
                DECLARED_TRACK,
                0x21,
            )
            .await;
            assert_eq!(
                status(&response),
                200,
                "case {name:?}: the preceding session must be taken, got {} {}",
                status(&response),
                text(&response)
            );
            expected_log = vec![BASE_MILLIS, BASE_MILLIS + 1_000];
        }

        let response = submit(
            &harness,
            stream,
            "RELATIVE",
            BASE_MILLIS + 1_000,
            &[0, 1_000],
            UNDECLARED_TRACK,
            0x31,
        )
        .await;
        // A container-level fault is reported IN BAND: the request itself was well formed.
        assert_eq!(
            status(&response),
            200,
            "case {name:?}: expected HTTP 200, got {} {}",
            status(&response),
            text(&response)
        );
        let acks = acknowledgements(name, &response);
        assert_eq!(
            faults(&acks),
            vec![("TRACK_NUMBER_MISMATCH".to_string(), 4010)],
            "case {name:?}: expected the undeclared-track fault, got {}",
            text(&response)
        );
        assert_eq!(
            persisted_numbers(&acks),
            Vec::<String>::new(),
            "case {name:?}: a session a fault ended reports no PERSISTED acknowledgement, got {}",
            text(&response)
        );

        let held = harness.stored_instants(&arn);
        assert_eq!(
            held, expected_log,
            "case {name:?}: expected the log to hold {expected_log:?} after the refused \
             session, got {held:?}"
        );
    }
}
