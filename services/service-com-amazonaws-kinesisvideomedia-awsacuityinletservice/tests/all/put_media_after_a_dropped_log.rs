// SPDX-License-Identifier: CC0-1.0

//! What a producer is acknowledged after this store's log ceiling dropped a live stream's
//! media log.
//!
//! The rule under test:
//!
//! > A fragment number is STREAM-scoped and increases with write order over the stream's
//! > whole lifetime, so a number a stream has already used is never minted for that stream
//! > again: what a store ceiling drops is MEDIA, never the point the stream's numbering had
//! > reached. The next fragment sent to a stream whose media log has gone is therefore
//! > minted the number AFTER the last one that stream used, and the archive read plane —
//! > which still holds the earlier fragments, the stream being the control plane's record
//! > that no log drop removes — reports every fragment of the stream under a number of its
//! > own.
//!
//! The rows differ in how much media the stream carried before its log was dropped, how
//! much the producer sent afterwards, and which identifier form addresses the stream; the
//! case the issue recorded (one chunk before, one after) is one row.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use bytes::Bytes;
use service_com_amazonaws_kinesisvideo_kinesisvideo_20170930 as control_plane;
use service_com_amazonaws_kinesisvideoarchivedmedia_awsacuityreader as archived_media;
use service_com_amazonaws_kinesisvideomedia_awsacuityinletservice as media;
use smithy_server_runtime::MultiServiceRouter;

use crate::get_media::split_payload;
use crate::put_media_ingest::{cluster, ingest_headers, Container};

/// The region every request in this suite is signed for.
const REGION: &str = "us-east-1";

/// The host a `GetDataEndpoint` answer points a data-plane call at: this emulator.
const ENDPOINT_HOST: &str = "kinesisvideo.us-east-1.amazonaws.com";

/// The epoch instant, in milliseconds, the first submitted Cluster of a session before the
/// drop is captured at. The sessions are ABSOLUTE, so a Cluster timecode IS this instant —
/// in the past, as a producer's capture instants are.
const BASE_MILLIS: u64 = 1_700_000_000_000;

/// Milliseconds between two Clusters of one session.
const CLUSTER_SPACING_MILLIS: u64 = 1_000;

/// How far after the pre-drop media the post-drop session's Clusters were captured.
const SESSION_GAP_MILLIS: u64 = 60_000;

/// The media bytes one filler stream is produced, to grow the number of logs the store
/// holds until the ceiling drops the stream under test.
const FILLER_CHUNK: &[u8] = b"filler-chunk";

/// One router over one engine carrying the three services this round trip needs: the
/// control plane that creates the stream, this ingest/media service, and the archive read
/// plane that reports the ingested fragments.
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
        router.register_service(archived_media::EmulatedService::new(Arc::clone(&engine)));
        Self { router, engine }
    }

    /// The SigV4 credential scope every request here carries: its service field is the
    /// routing key the Kinesis Video services share.
    fn authorization() -> String {
        format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20261125/{REGION}/kinesisvideo/\
             aws4_request, SignedHeaders=host, Signature=0000"
        )
    }

    /// POST one restJson1 call at the data endpoint host.
    async fn call(&self, path: &str, body: &serde_json::Value) -> http::Response<Bytes> {
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("https://{ENDPOINT_HOST}{path}"))
            .header("host", ENDPOINT_HOST)
            .header("content-type", "application/json")
            .header("authorization", Self::authorization())
            .body(Bytes::from(
                serde_json::to_vec(body).expect("the request body serializes"),
            ))
            .expect("a well-formed request");
        self.router.dispatch(request).await
    }

    /// POST one ingest session: the headers a producer sends and the container as its body.
    async fn put_media(&self, headers: &[(&str, String)], body: Vec<u8>) -> http::Response<Bytes> {
        let mut request = http::Request::builder()
            .method("POST")
            .uri(format!("https://{ENDPOINT_HOST}/putMedia"))
            .header("host", ENDPOINT_HOST)
            .header("authorization", Self::authorization());
        for (name, value) in headers {
            request = request.header(*name, value.clone());
        }
        self.router
            .dispatch(request.body(Bytes::from(body)).expect("a well-formed request"))
            .await
    }

    /// Create `name` through the control plane and answer the `StreamARN` it minted.
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
        json(&response)
            .get("StreamARN")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_else(|| panic!("case \"create {name}\": no StreamARN in the answer"))
            .to_string()
    }

    /// Wait until the control plane reports `name` as `ACTIVE`, as a client does before
    /// reading a stream it just created: the archive read plane serves a stream only while
    /// the registry holds it active.
    ///
    /// Waits for the CONDITION, not for a span: it looks again until the status holds, under
    /// a deadline far longer than the transition needs.
    async fn await_active(&self, name: &str) {
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut last = String::new();
        while Instant::now() < deadline {
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

    /// Produce one chunk to the stream `name` through this service's media-log writer: what
    /// grows the number of logs the store holds, so the ceiling drops the oldest.
    fn produce_filler(&self, name: &str) {
        let state = media::state::service_state(&*self.engine).expect("the media store");
        let mut store = state.lock();
        let at = SystemTime::UNIX_EPOCH + Duration::from_millis(BASE_MILLIS);
        store
            .put_fragment(
                &*self.engine,
                Some(name),
                None,
                FILLER_CHUNK.to_vec(),
                at,
                at,
            )
            .unwrap_or_else(|| {
                panic!("case \"fill {name}\": a registered stream must accept media")
            });
    }

    /// Whether this store still holds a media log for the stream `arn` names.
    fn holds_log(&self, arn: &str) -> bool {
        let state = media::state::service_state(&*self.engine).expect("the media store");
        let store = state.lock();
        store.log_by_arn(arn).is_some()
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

    /// The fragment numbers `ListFragments` reports for the stream `addressing` names, over
    /// a window wide enough to hold every fragment this suite ingests.
    async fn archived_fragment_numbers(&self, addressing: &serde_json::Value) -> Vec<String> {
        let mut body = addressing.as_object().cloned().unwrap_or_default();
        body.insert(
            "FragmentSelector".to_string(),
            serde_json::json!({
                "FragmentSelectorType": "PRODUCER_TIMESTAMP",
                "TimestampRange": { "StartTimestamp": 0, "EndTimestamp": 4_000_000_000i64 },
            }),
        );
        let response = self
            .call("/listFragments", &serde_json::Value::Object(body))
            .await;
        assert_eq!(
            status(&response),
            200,
            "case \"listFragments\": expected HTTP 200, got {} {}",
            status(&response),
            text(&response)
        );
        json(&response)
            .get("Fragments")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|fragment| {
                fragment
                    .get("FragmentNumber")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            })
            .collect()
    }

    /// `GetMedia` from the earliest chunk of the stream `addressing` names.
    async fn get_media_earliest(&self, addressing: &serde_json::Value) -> http::Response<Bytes> {
        let mut body = addressing.as_object().cloned().unwrap_or_default();
        body.insert(
            "StartSelector".to_string(),
            serde_json::json!({ "StartSelectorType": "EARLIEST" }),
        );
        self.call("/getMedia", &serde_json::Value::Object(body))
            .await
    }
}

fn status(response: &http::Response<Bytes>) -> u16 {
    response.status().as_u16()
}

fn text(response: &http::Response<Bytes>) -> String {
    String::from_utf8_lossy(response.body()).to_string()
}

fn json(response: &http::Response<Bytes>) -> serde_json::Value {
    serde_json::from_slice(response.body()).unwrap_or(serde_json::Value::Null)
}

/// The fragment numbers the `PERSISTED` acknowledgements of one ingest answer carry.
fn persisted_fragment_numbers(case: &str, response: &http::Response<Bytes>) -> Vec<String> {
    text(response)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str::<serde_json::Value>(line).unwrap_or_else(|error| {
                panic!("case {case:?}: an acknowledgement is not JSON ({error}): {line:?}")
            })
        })
        .filter(|acknowledgement| {
            acknowledgement
                .get("EventType")
                .and_then(serde_json::Value::as_str)
                == Some("PERSISTED")
        })
        .map(|acknowledgement| {
            acknowledgement
                .get("FragmentNumber")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// One ABSOLUTE session of `count` Clusters, the first captured at `first_millis`.
fn session(first_millis: u64, count: usize, marker: u8) -> Container {
    Container::of(
        (0..count)
            .map(|at| {
                cluster(
                    first_millis + (at as u64) * CLUSTER_SPACING_MILLIS,
                    2,
                    25,
                    marker.wrapping_add(at as u8),
                )
            })
            .collect(),
    )
}

/// The decimal fragment numbers `count` fragments minted from `first` carry.
fn numbers(first: u64, count: usize) -> Vec<String> {
    (0..count)
        .map(|at| (first + at as u64).to_string())
        .collect()
}

/// One row: how much media the stream carried before its log was dropped, how much the
/// producer sent afterwards, and which identifier form addresses it.
struct Row {
    case: &'static str,
    before: usize,
    after: usize,
    /// `true` to address the stream by its `StreamARN`, `false` by its `StreamName`.
    by_arn: bool,
}

/// RULE: the fragment numbers of one stream increase over the stream's whole lifetime, so
/// media produced after this store's log ceiling dropped the stream's log is minted the
/// numbers AFTER the ones that stream already used — on both of its read planes.
#[tokio::test]
async fn media_produced_after_a_dropped_log_continues_the_streams_fragment_numbering() {
    let rows = [
        Row {
            // The session the issue recorded: one chunk each side of the drop.
            case: "one chunk before the drop, one after, addressed by name",
            before: 1,
            after: 1,
            by_arn: false,
        },
        Row {
            case: "one chunk before the drop, three after, addressed by ARN",
            before: 1,
            after: 3,
            by_arn: true,
        },
        Row {
            case: "three chunks before the drop, two after, addressed by name",
            before: 3,
            after: 2,
            by_arn: false,
        },
    ];

    for row in &rows {
        let case = row.case;
        let harness = Harness::new();
        let name = format!("dropped-log-probe-{}-{}", row.before, row.after);
        let arn = harness.create_stream(&name).await;
        harness.await_active(&name).await;
        let addressing = if row.by_arn {
            serde_json::json!({ "StreamARN": arn })
        } else {
            serde_json::json!({ "StreamName": name })
        };
        let header = if row.by_arn {
            ("x-amzn-stream-arn", arn.clone())
        } else {
            ("x-amzn-stream-name", name.clone())
        };
        // Only a RELATIVE session reads it; these sessions are ABSOLUTE, so every Cluster
        // timecode names its own capture instant.
        let producer_start = SystemTime::UNIX_EPOCH + Duration::from_millis(BASE_MILLIS);

        // The media the stream carried before its log went: minted from 1, as the first
        // session on a stream is.
        let first = harness
            .put_media(
                &ingest_headers(header.clone(), "ABSOLUTE", producer_start),
                session(BASE_MILLIS, row.before, 0xA1).bytes(),
            )
            .await;
        assert_eq!(
            status(&first),
            200,
            "case {case:?}: expected HTTP 200 from the ingest plane, got {} {}",
            status(&first),
            text(&first)
        );
        let before_numbers = persisted_fragment_numbers(case, &first);
        assert_eq!(
            before_numbers,
            numbers(1, row.before),
            "case {case:?}: expected the first session's fragments {:?}, got {before_numbers:?}",
            numbers(1, row.before)
        );

        // Produce to further streams until this store's ceiling drops this stream's log.
        // The log written longest ago is the one that goes, and this stream's is the
        // oldest; the loop looks for the CONDITION, with a bound well past the ceiling.
        let mut filled = 0usize;
        while harness.holds_log(&arn) {
            assert!(
                filled < media::state::MAX_STORED_STREAM_LOGS * 2,
                "case {case:?}: the ceiling must drop the oldest log within {} further \
                 streams, it still holds this one after {filled}",
                media::state::MAX_STORED_STREAM_LOGS * 2
            );
            let filler = format!("{name}-filler-{filled:03}");
            harness.create_stream(&filler).await;
            harness.produce_filler(&filler);
            filled += 1;
        }

        // The drop took the MEDIA: the stream reads as one nothing has been produced to.
        let emptied = harness.get_media_earliest(&addressing).await;
        assert_eq!(
            status(&emptied),
            200,
            "case {case:?}: a registered stream whose log went must still read, got {} {}",
            status(&emptied),
            text(&emptied)
        );
        assert!(
            emptied.body().is_empty(),
            "case {case:?}: its log was dropped, so it must read empty, got {} byte(s)",
            emptied.body().len()
        );

        // And the next session on that same stream picks the numbering up where it left
        // off, rather than minting a number that already names media.
        let second = harness
            .put_media(
                &ingest_headers(header, "ABSOLUTE", producer_start),
                session(BASE_MILLIS + SESSION_GAP_MILLIS, row.after, 0xC3).bytes(),
            )
            .await;
        assert_eq!(
            status(&second),
            200,
            "case {case:?}: expected HTTP 200 from the ingest plane, got {} {}",
            status(&second),
            text(&second)
        );
        let after_numbers = persisted_fragment_numbers(case, &second);
        let expected_after = numbers(row.before as u64 + 1, row.after);
        assert_eq!(
            after_numbers, expected_after,
            "case {case:?}: expected the fragments after the drop to carry \
             {expected_after:?}, got {after_numbers:?}"
        );

        // The media read plane serves the new chunks under those same numbers, so no
        // number names two chunks of this stream.
        let chunks = harness.stored_chunks(&arn);
        assert_eq!(
            chunks.len(),
            row.after,
            "case {case:?}: expected the re-created log to hold the {} new chunk(s), got {}",
            row.after,
            chunks.len()
        );
        let read = harness.get_media_earliest(&addressing).await;
        assert_eq!(
            status(&read),
            200,
            "case {case:?}: expected HTTP 200 from GetMedia, got {} {}",
            status(&read),
            text(&read)
        );
        let borrowed: Vec<&[u8]> = chunks.iter().map(Vec::as_slice).collect();
        let delivered: Vec<String> = split_payload(case, read.body(), &borrowed)
            .iter()
            .map(|chunk| chunk.tag("AWS_KINESISVIDEO_FRAGMENT_NUMBER"))
            .collect();
        assert_eq!(
            delivered, after_numbers,
            "case {case:?}: GetMedia must tag the new chunks with {after_numbers:?}, \
             got {delivered:?}"
        );

        // The archive read plane still holds the fragments from before the drop — the
        // stream is the control plane's record, which no log drop removed — and the new
        // ones beside them, each under a number of its own.
        let archived = harness.archived_fragment_numbers(&addressing).await;
        let expected_archive: Vec<String> = before_numbers
            .iter()
            .chain(after_numbers.iter())
            .cloned()
            .collect();
        assert_eq!(
            archived, expected_archive,
            "case {case:?}: the archive must report every fragment of the stream \
             {expected_archive:?}, got {archived:?}"
        );
        let mut distinct = archived.clone();
        distinct.sort();
        distinct.dedup();
        assert_eq!(
            distinct.len(),
            archived.len(),
            "case {case:?}: no fragment number may name two fragments of one stream, \
             got {archived:?}"
        );
    }
}

/// RULE: the number an acknowledgement reports for a fragment as RECEIVED is the one the
/// next fragment of that stream is minted under, so a stream whose log this store dropped
/// reports the numbering that log had reached and not the numbering of a fresh stream.
#[tokio::test]
async fn the_received_fragment_number_survives_a_dropped_log() {
    let harness = Harness::new();
    let name = "dropped-log-received-probe";
    let arn = harness.create_stream(name).await;
    harness.await_active(name).await;
    let producer_start = SystemTime::UNIX_EPOCH + Duration::from_millis(BASE_MILLIS);

    // Two chunks, so the stream's numbering has reached 3.
    let response = harness
        .put_media(
            &ingest_headers(
                ("x-amzn-stream-name", name.to_string()),
                "ABSOLUTE",
                producer_start,
            ),
            session(BASE_MILLIS, 2, 0x11).bytes(),
        )
        .await;
    assert_eq!(
        status(&response),
        200,
        "case \"first session\": expected HTTP 200, got {} {}",
        status(&response),
        text(&response)
    );

    let mut filled = 0usize;
    while harness.holds_log(&arn) {
        assert!(
            filled < media::state::MAX_STORED_STREAM_LOGS * 2,
            "case \"fill the store\": the ceiling must drop the oldest log within {} \
             further streams, it still holds this one after {filled}",
            media::state::MAX_STORED_STREAM_LOGS * 2
        );
        let filler = format!("{name}-filler-{filled:03}");
        harness.create_stream(&filler).await;
        harness.produce_filler(&filler);
        filled += 1;
    }

    // Read through the store's own reporting seam, which the acknowledgement stream reads:
    // it mints nothing, and it must report where the stream's numbering stands.
    let reported = {
        let state = media::state::service_state(&*harness.engine).expect("the media store");
        let store = state.lock();
        store
            .received_fragment_number(&*harness.engine, Some(name), None)
            .expect("a registered stream reports the number its next fragment takes")
    };
    assert_eq!(
        reported, "3",
        "case \"after the drop\": expected the stream's numbering to stand at 3, \
         got {reported:?}"
    );
    assert!(
        !harness.holds_log(&arn),
        "case \"after the drop\": reading the next fragment number must not re-create the \
         stream's log"
    );
}
