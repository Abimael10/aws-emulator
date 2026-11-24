// SPDX-License-Identifier: CC0-1.0

//! The time ordering of one stream's fragment log, across ingest SESSIONS.
//!
//! The rule under test: a stream's fragment log is ordered by capture time, so a
//! submitted Cluster captured BEFORE the newest fragment the stream already holds is
//! refused in band — one `ERROR` acknowledgement of
//! `FRAGMENT_TIMECODE_LESSER_THAN_PREVIOUS` (4004) naming that Cluster, inside the same
//! `200`, ending the session there — and is never appended, whether the fragment it goes
//! back before was taken by the same session or by an earlier one, while a Cluster
//! captured at or after that instant is taken. The instant compared is the one the
//! producer captured at, which a session's `x-amzn-fragment-timecode-type` and
//! `x-amzn-producer-start-timestamp` decide, so two sessions declaring different ones are
//! still ordered against each other.
//!
//! The first table spans what the rule generalises over: the follow-on session's timecode
//! type and producer start, whether it goes back on its first Cluster or on a later one,
//! and where its instant falls against the log's newest stamp — behind it, one
//! millisecond behind it, exactly on it, and after it.
//!
//! The second half pins what the ordering is FOR: `GetMedia`'s timestamp start selector
//! "starts with the chunk containing the fragment that has the specified timestamp"
//! (`smithy_query(query="shape", shape="StartSelector")`, member `StartTimestamp`), so
//! every chunk stamped at or after the requested instant is delivered.

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

/// The epoch instant every table is laid out from, on a whole second and in the past, as
/// a producer's own capture instants are.
const BASE_MILLIS: u64 = 1_761_000_000_000;

/// How far after [`BASE_MILLIS`] the seeded session's newest fragment is captured.
const SEEDED_SPAN_MILLIS: u64 = 10_000;

/// The frames every Cluster in this suite carries.
const FRAMES_PER_CLUSTER: usize = 2;

/// The milliseconds between the frames of one Cluster.
const FRAME_SPACING_MILLIS: i16 = 30;

/// An epoch-millisecond instant as the epoch-seconds number restJson1 binds a timestamp
/// from. Every instant this suite sends is a whole second, which an `f64` carries exactly.
fn as_epoch_seconds(millis: u64) -> serde_json::Value {
    serde_json::json!(millis as f64 / 1000.0)
}

fn at(millis: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_millis(millis)
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

    /// Create `name` through the control plane, wait until it is reported active, and
    /// answer the `StreamARN` it minted.
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
        self.await_active(name).await;
        arn
    }

    /// Wait until the control plane reports `name` as `ACTIVE`, as a producer does before
    /// writing to a stream it just created ("CreateStream is an asynchronous operation").
    ///
    /// Waits for the CONDITION, not for a span: it looks again until the status holds,
    /// under a deadline far longer than the transition needs.
    async fn await_active(&self, name: &str) {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        let mut last = String::new();
        while std::time::Instant::now() < deadline {
            let response = self
                .call("/describeStream", &serde_json::json!({ "StreamName": name }))
                .await;
            last = json(&response)["StreamInfo"]["Status"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            if last == "ACTIVE" {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("case \"create {name}\": expected Status ACTIVE before the deadline, got {last:?}");
    }

    /// The fragments this service's media log holds for the stream `arn` names, in log
    /// order, as `(fragment number, producer timestamp in epoch milliseconds, chunk)`.
    fn stored(&self, arn: &str) -> Vec<(String, u64, Vec<u8>)> {
        let state = media::state::service_state(&*self.engine).expect("the media store");
        let store = state.lock();
        store
            .log_by_arn(arn)
            .map(|log| {
                log.fragments
                    .iter()
                    .map(|fragment| {
                        (
                            fragment.fragment_number.clone(),
                            fragment
                                .producer_timestamp
                                .duration_since(SystemTime::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis() as u64,
                            fragment.chunk.clone(),
                        )
                    })
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

fn status(response: &http::Response<Bytes>) -> u16 {
    response.status().as_u16()
}

fn error_code(response: &http::Response<Bytes>) -> String {
    response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string()
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

/// How many acknowledgements of `event` the stream carries.
fn count_of(acknowledgements: &[serde_json::Value], event: &str) -> usize {
    acknowledgements
        .iter()
        .filter(|acknowledgement| event_type(acknowledgement) == event)
        .count()
}

/// Every `ERROR` acknowledgement, as `(ErrorCode, ErrorId, FragmentTimecode)`.
fn faults(acknowledgements: &[serde_json::Value]) -> Vec<(String, u64, u64)> {
    acknowledgements
        .iter()
        .filter(|acknowledgement| event_type(acknowledgement) == "ERROR")
        .map(|acknowledgement| {
            let number = |member: &str| {
                acknowledgement
                    .get(member)
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or_default()
            };
            (
                acknowledgement
                    .get("ErrorCode")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                number("ErrorId"),
                number("FragmentTimecode"),
            )
        })
        .collect()
}

/// A container of one Cluster per timecode, each marked so one Cluster's frames cannot be
/// mistaken for another's.
fn container_of(timecodes: &[u64], marker: u8) -> Container {
    Container::of(
        timecodes
            .iter()
            .enumerate()
            .map(|(index, timecode)| {
                cluster(
                    *timecode,
                    FRAMES_PER_CLUSTER,
                    FRAME_SPACING_MILLIS,
                    marker ^ (index as u8),
                )
            })
            .collect(),
    )
}

/// Submit one session and answer its response.
async fn submit(
    harness: &Harness,
    stream: &'static str,
    timecode_type: &'static str,
    producer_start_millis: u64,
    timecodes: &[u64],
    marker: u8,
) -> http::Response<Bytes> {
    harness
        .put_media(
            &ingest_headers(
                ("x-amzn-stream-name", stream.to_string()),
                timecode_type,
                at(producer_start_millis),
            ),
            container_of(timecodes, marker).bytes(),
        )
        .await
}

/// Seed the stream with one ABSOLUTE session of two Clusters, the newest captured
/// [`SEEDED_SPAN_MILLIS`] after [`BASE_MILLIS`], and answer their producer instants.
async fn seed(harness: &Harness, stream: &'static str) -> Vec<u64> {
    let timecodes = [BASE_MILLIS, BASE_MILLIS + SEEDED_SPAN_MILLIS];
    let response = submit(harness, stream, "ABSOLUTE", BASE_MILLIS, &timecodes, 0x11).await;
    assert_eq!(
        status(&response),
        200,
        "case \"seed {stream}\": expected HTTP 200 from the ingest plane, got {} ({}) {}",
        status(&response),
        error_code(&response),
        text(&response)
    );
    let acks = acknowledgements("seed", &response);
    assert_eq!(
        count_of(&acks, "PERSISTED"),
        timecodes.len(),
        "case \"seed {stream}\": expected one PERSISTED acknowledgement per Cluster, got {}",
        text(&response)
    );
    timecodes.to_vec()
}

/// One follow-on session and what the stream does with it.
struct Case {
    name: &'static str,
    /// The stream this case ingests into.
    stream: &'static str,
    /// `x-amzn-fragment-timecode-type` of the follow-on session.
    timecode_type: &'static str,
    /// `x-amzn-producer-start-timestamp` of the follow-on session, in epoch milliseconds
    /// — read only by a RELATIVE session.
    producer_start_millis: u64,
    /// The Cluster timecodes the follow-on session submits.
    timecodes: Vec<u64>,
    /// How many of those Clusters the stream takes.
    taken: usize,
    /// The Cluster timecode the refusal names, or `None` when the session is taken whole.
    refused_at: Option<u64>,
    /// The producer instants the log holds once the session is over, in epoch
    /// milliseconds.
    expected_log_millis: Vec<u64>,
}

/// RULE: a fragment log is ordered by capture time, so a Cluster captured before the
/// newest fragment the stream already holds is refused with
/// `FRAGMENT_TIMECODE_LESSER_THAN_PREVIOUS` and not appended — across sessions as within
/// one — while a Cluster captured at or after that instant is taken.
#[tokio::test]
async fn a_cluster_behind_the_log_is_refused_whichever_session_submits_it() {
    let newest = BASE_MILLIS + SEEDED_SPAN_MILLIS;
    let seeded = vec![BASE_MILLIS, newest];

    let cases: Vec<Case> = vec![
        Case {
            // The session the issue recorded: a second ABSOLUTE session whose only
            // Cluster names an instant ten seconds before the fragment already held.
            name: "an ABSOLUTE session behind the log",
            stream: "log-order-absolute-behind",
            timecode_type: "ABSOLUTE",
            producer_start_millis: BASE_MILLIS,
            timecodes: vec![newest - 5_000],
            taken: 0,
            refused_at: Some(newest - 5_000),
            expected_log_millis: seeded.clone(),
        },
        Case {
            // One millisecond behind the log's newest stamp is still behind it: the
            // bound is the newest instant held, not a window around it.
            name: "an ABSOLUTE session one millisecond behind the log",
            stream: "log-order-absolute-one-milli",
            timecode_type: "ABSOLUTE",
            producer_start_millis: BASE_MILLIS,
            timecodes: vec![newest - 1],
            taken: 0,
            refused_at: Some(newest - 1),
            expected_log_millis: seeded.clone(),
        },
        Case {
            // The log's OLDEST stamp is an instant the log holds, and it is still behind
            // the newest one: the comparison is against the end of the log.
            name: "an ABSOLUTE session at the log's oldest instant",
            stream: "log-order-absolute-oldest",
            timecode_type: "ABSOLUTE",
            producer_start_millis: BASE_MILLIS,
            timecodes: vec![BASE_MILLIS],
            taken: 0,
            refused_at: Some(BASE_MILLIS),
            expected_log_millis: seeded.clone(),
        },
        Case {
            // A RELATIVE session is ordered at the instants its producer start puts its
            // timecodes at, so a producer that restarts with an earlier start timestamp
            // goes back even though its first timecode is near zero.
            name: "a RELATIVE session whose producer start is behind the log",
            stream: "log-order-relative-behind",
            timecode_type: "RELATIVE",
            producer_start_millis: BASE_MILLIS,
            timecodes: vec![1_000],
            taken: 0,
            refused_at: Some(1_000),
            expected_log_millis: seeded.clone(),
        },
        Case {
            // Equal is not lesser: a session resuming exactly at the newest instant held
            // is taken.
            name: "a session resuming at the log's newest instant",
            stream: "log-order-resume-on-the-newest",
            timecode_type: "ABSOLUTE",
            producer_start_millis: BASE_MILLIS,
            timecodes: vec![newest, newest + 2_000],
            taken: 2,
            refused_at: None,
            expected_log_millis: vec![BASE_MILLIS, newest, newest, newest + 2_000],
        },
        Case {
            name: "an ABSOLUTE session extending the log",
            stream: "log-order-absolute-extends",
            timecode_type: "ABSOLUTE",
            producer_start_millis: BASE_MILLIS,
            timecodes: vec![newest + 1_000, newest + 3_000],
            taken: 2,
            refused_at: None,
            expected_log_millis: vec![BASE_MILLIS, newest, newest + 1_000, newest + 3_000],
        },
        Case {
            name: "a RELATIVE session extending the log",
            stream: "log-order-relative-extends",
            timecode_type: "RELATIVE",
            producer_start_millis: newest,
            timecodes: vec![5_000, 6_000],
            taken: 2,
            refused_at: None,
            expected_log_millis: vec![BASE_MILLIS, newest, newest + 5_000, newest + 6_000],
        },
        Case {
            // The Clusters taken before the fault stay ingested, and the one that goes
            // back is behind the log the session itself just extended.
            name: "a session that goes back after extending the log",
            stream: "log-order-back-after-extending",
            timecode_type: "ABSOLUTE",
            producer_start_millis: BASE_MILLIS,
            timecodes: vec![newest + 10_000, newest + 5_000],
            taken: 1,
            refused_at: Some(newest + 5_000),
            expected_log_millis: vec![BASE_MILLIS, newest, newest + 10_000],
        },
    ];

    for case in &cases {
        let name = case.name;
        let harness = Harness::new();
        let arn = harness.create_stream(case.stream).await;
        let seeded_millis = seed(&harness, case.stream).await;
        assert_eq!(
            seeded_millis, seeded,
            "case {name:?}: the seeded session must land at {seeded:?}, got {seeded_millis:?}"
        );

        let response = submit(
            &harness,
            case.stream,
            case.timecode_type,
            case.producer_start_millis,
            &case.timecodes,
            0x77,
        )
        .await;
        assert_eq!(
            status(&response),
            200,
            "case {name:?}: a fault in the submitted media is reported in band, so the \
             status stays 200, got {} ({}) {}",
            status(&response),
            error_code(&response),
            text(&response)
        );

        let acks = acknowledgements(name, &response);
        let reported = faults(&acks);
        let expected: Vec<(String, u64, u64)> = case
            .refused_at
            .map(|timecode| {
                vec![(
                    "FRAGMENT_TIMECODE_LESSER_THAN_PREVIOUS".to_string(),
                    4004,
                    timecode,
                )]
            })
            .unwrap_or_default();
        assert_eq!(
            reported, expected,
            "case {name:?}: expected the ERROR acknowledgements {expected:?}, got {} \
             ({reported:?})",
            text(&response)
        );
        assert_eq!(
            count_of(&acks, "RECEIVED"),
            case.taken,
            "case {name:?}: expected {} Cluster(s) acknowledged as received, got {}",
            case.taken,
            text(&response)
        );
        // A session a fault ended reports no PERSISTED acknowledgement at all, whatever
        // it had already taken.
        let expected_persisted = if case.refused_at.is_some() {
            0
        } else {
            case.taken
        };
        assert_eq!(
            count_of(&acks, "PERSISTED"),
            expected_persisted,
            "case {name:?}: expected {expected_persisted} PERSISTED acknowledgement(s), \
             got {}",
            text(&response)
        );

        let held: Vec<u64> = harness
            .stored(&arn)
            .into_iter()
            .map(|(_, millis, _)| millis)
            .collect();
        assert_eq!(
            held, case.expected_log_millis,
            "case {name:?}: expected the log to hold the producer instants {:?}, got {held:?}",
            case.expected_log_millis
        );
        assert!(
            held.windows(2).all(|pair| pair[0] <= pair[1]),
            "case {name:?}: the log must stay in capture order, got {held:?}"
        );
    }
}

/// One read of a log a refused session could not disorder: the instant requested and the
/// index of the first chunk the session must deliver.
struct Read {
    case: &'static str,
    /// The requested instant, in epoch milliseconds.
    wanted: u64,
    expected_start: usize,
}

/// RULE: `GetMedia`'s timestamp start selector starts with the chunk CONTAINING the
/// requested instant and delivers every chunk after it, so no fragment stamped at or
/// after that instant is left undelivered — which holds because the log a refused
/// out-of-order session could not disorder is in capture order.
#[tokio::test]
async fn a_timestamp_selector_delivers_every_chunk_at_or_after_the_requested_instant() {
    const STREAM: &str = "log-order-read-back";
    let newest = BASE_MILLIS + SEEDED_SPAN_MILLIS;

    let harness = Harness::new();
    let arn = harness.create_stream(STREAM).await;
    seed(&harness, STREAM).await;

    // The session of the report: one Cluster captured before the fragment already held,
    // which the log does not take.
    let behind = submit(
        &harness,
        STREAM,
        "ABSOLUTE",
        BASE_MILLIS,
        &[newest - 5_000],
        0x33,
    )
    .await;
    assert_eq!(
        status(&behind),
        200,
        "case \"a session behind the log\": expected HTTP 200, got {} ({}) {}",
        status(&behind),
        error_code(&behind),
        text(&behind)
    );

    let held = harness.stored(&arn);
    let stamps: Vec<u64> = held.iter().map(|(_, millis, _)| *millis).collect();
    assert_eq!(
        stamps,
        vec![BASE_MILLIS, newest],
        "case \"a session behind the log\": the log holds only the fragments it took, \
         got {stamps:?}"
    );

    let reads = [
        Read {
            case: "before every fragment",
            wanted: BASE_MILLIS - 5_000,
            expected_start: 0,
        },
        Read {
            // The read of the report: an instant inside the first chunk's span, after
            // which the later chunk must still arrive.
            case: "inside the earliest chunk's span",
            wanted: newest - 5_000,
            expected_start: 0,
        },
        Read {
            case: "exactly the newest instant",
            wanted: newest,
            expected_start: 1,
        },
        Read {
            case: "after every fragment",
            wanted: newest + 10_000,
            expected_start: 1,
        },
    ];

    for read in &reads {
        let case = read.case;
        let response = harness
            .get_media(
                STREAM,
                serde_json::json!({
                    "StartSelectorType": "PRODUCER_TIMESTAMP",
                    "StartTimestamp": as_epoch_seconds(read.wanted),
                }),
            )
            .await;
        assert_eq!(
            status(&response),
            200,
            "case {case:?}: expected HTTP 200 from GetMedia, got {} ({}) {}",
            status(&response),
            error_code(&response),
            text(&response)
        );

        let expected_media: Vec<&[u8]> = held[read.expected_start..]
            .iter()
            .map(|(_, _, chunk)| chunk.as_slice())
            .collect();
        let chunks = split_payload(case, response.body(), &expected_media);
        let delivered: Vec<(String, u64)> = chunks
            .iter()
            .map(|chunk| {
                (
                    chunk.tag("AWS_KINESISVIDEO_FRAGMENT_NUMBER"),
                    chunk
                        .tag("AWS_KINESISVIDEO_PRODUCER_TIMESTAMP")
                        .parse::<u64>()
                        .unwrap_or_default(),
                )
            })
            .collect();
        let expected: Vec<(String, u64)> = held[read.expected_start..]
            .iter()
            .map(|(number, millis, _)| (number.clone(), *millis))
            .collect();
        assert_eq!(
            delivered, expected,
            "case {case:?}: expected the chunks {expected:?} from the instant \
             {wanted}, got {delivered:?}",
            wanted = read.wanted
        );
        for stamp in stamps.iter().filter(|stamp| **stamp >= read.wanted) {
            assert!(
                delivered.iter().any(|(_, at)| at == stamp),
                "case {case:?}: the chunk stamped {stamp} is at or after the requested \
                 instant {wanted} and must be delivered, got {delivered:?}",
                wanted = read.wanted
            );
        }
    }
}
