// SPDX-License-Identifier: CC0-1.0

//! WHOSE media an announced live-media endpoint reads, and which refusal a read that used
//! the wrong one earns.
//!
//! The rule under test, one level above any single call:
//!
//! > An announced data endpoint belongs to the ONE stream it was discovered for —
//! > `GetDataEndpoint` reports "an endpoint for a specified stream", with the stream named
//! > in the discovery call's own input — so a `GetMedia` there naming another stream of
//! > the account used the wrong endpoint: refused HTTP 400 `InvalidEndpointException`,
//! > which is the class Kinesis Video Media declares for a caller that "used wrong
//! > endpoint" and whose remedy is to re-run `GetDataEndpoint` and read at the endpoint it
//! > reports. An identifier the stream registry does not hold is the OTHER class, HTTP 404
//! > `ResourceNotFoundException` ("The stream with the given name does not exist"),
//! > whichever address asked for it: what the registry answers is settled before the
//! > address is read against it. Neither refusal carries a value from the request, and
//! > neither serves a byte of anyone's media.
//!
//! The first table's rows span what the rule generalises over: which endpoint the
//! identical read was sent to (each of two streams' own announced live-media endpoints),
//! which stream it named (the endpoint's own, the other one, and identifiers the registry
//! holds nothing for) and in which identifier form (`StreamName` and `StreamARN`). Both
//! directions of the pair are rows, so the refusal is about the (endpoint, stream) pair
//! and not a label on one of the two streams.
//!
//! The second table pins the PRECEDENCE: the endpoint is read against the stream before
//! the starting cursor the request named is interpreted at all, so a wrong-endpoint read
//! earns that class whatever `StartSelector` it carries — including selectors that would
//! otherwise be refused `InvalidArgumentException` for a cursor this stream has no chunk
//! for.

use bytes::Bytes;

use crate::control_plane_stream_reads::error_code;
use crate::ingest_plane_addressing::{status, text, Harness};
use crate::media_endpoint_grouping::carries;
use crate::put_media_ingest::{cluster, ingest_headers, Cluster, Container};
use crate::refusal_message_fidelity::message;

/// The address the control-plane calls in this suite are made at: a client on the machine
/// this emulator runs on reaches its front door at a loopback address.
const CONTROL: &str = "127.0.0.1:9297";

/// The one sentence this family states for a stream that is not there to serve, written
/// out as the wire carries it rather than read off the code under test.
const STREAM_NOT_FOUND: &str = "The requested stream is not found or not active.";

/// The sentence Kinesis Video Media states for a caller that used the wrong endpoint,
/// written out as the wire carries it rather than read off the code under test: the
/// documented text of `InvalidEndpointException`
/// (`smithy_query(query="doc", shape="InvalidEndpointException",
/// model_path="services/service-com-amazonaws-kinesisvideomedia-awsacuityinletservice/smithy-model/com-amazonaws-kinesisvideomedia-awsacuityinletservice.json")`),
/// whose remedy clause is what a client re-discovers its endpoint on.
const WRONG_ENDPOINT: &str = "Caller used wrong endpoint to write data to a stream. On \
                              receiving such an exception, the user must call \
                              GetDataEndpoint with AccessMode set to \"READ\" and use the \
                              endpoint Kinesis Video returns in the next GetMedia call.";

/// The producer clock every session in this suite is stamped from: a whole second well
/// behind this machine's clock, so no fragment is refused for an instant that has not
/// happened.
fn producer_start() -> std::time::SystemTime {
    std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_millis(1_761_000_000_000)
}

/// Which announced endpoint a row sent its read to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum At {
    /// The live-media endpoint announced for the first stream.
    FirstStreamsEndpoint,
    /// The live-media endpoint announced for the second stream.
    SecondStreamsEndpoint,
}

/// Which stream a row's read named, and in which identifier form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Named {
    FirstByName,
    FirstByArn,
    SecondByName,
    SecondByArn,
    /// A `StreamName` no stream of this account answers to.
    AbsentName,
    /// A well-formed `StreamARN` no stream of this account answers to.
    AbsentArn,
}

/// What this endpoint answers a row's read with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answer {
    /// The read is served, and carries the addressed stream's own media.
    Served,
    /// The stream resolved, but at an address announced for another one.
    WrongEndpoint,
    /// The registry holds no such stream at all.
    NotFound,
}

impl Answer {
    /// The status, error code and sentence this answer is stated with, or `None` for the
    /// answer that is not a refusal.
    fn refusal(self) -> Option<(u16, &'static str, &'static str)> {
        match self {
            Self::Served => None,
            Self::WrongEndpoint => Some((400, "InvalidEndpointException", WRONG_ENDPOINT)),
            Self::NotFound => Some((404, "ResourceNotFoundException", STREAM_NOT_FOUND)),
        }
    }
}

/// One row: the endpoint the identical read was sent to, the stream it named, and the
/// answer the rule gives it.
#[derive(Debug, Clone, Copy)]
struct Row {
    case: &'static str,
    at: At,
    named: Named,
    answer: Answer,
}

/// The rows, differing along the endpoint addressed, the stream named and the identifier
/// form it was named in.
const ROWS: &[Row] = &[
    Row {
        case: "its own stream, by name",
        at: At::FirstStreamsEndpoint,
        named: Named::FirstByName,
        answer: Answer::Served,
    },
    Row {
        case: "its own stream, by ARN",
        at: At::FirstStreamsEndpoint,
        named: Named::FirstByArn,
        answer: Answer::Served,
    },
    Row {
        case: "another active stream of the account, by name",
        at: At::FirstStreamsEndpoint,
        named: Named::SecondByName,
        answer: Answer::WrongEndpoint,
    },
    Row {
        case: "another active stream of the account, by ARN",
        at: At::FirstStreamsEndpoint,
        named: Named::SecondByArn,
        answer: Answer::WrongEndpoint,
    },
    Row {
        case: "a stream name that exists nowhere",
        at: At::FirstStreamsEndpoint,
        named: Named::AbsentName,
        answer: Answer::NotFound,
    },
    Row {
        case: "a well-formed stream ARN that exists nowhere",
        at: At::FirstStreamsEndpoint,
        named: Named::AbsentArn,
        answer: Answer::NotFound,
    },
    // The other direction of the same pair: the second stream's endpoint serves the second
    // stream and refuses the first, so the refusal is about the pair and not about which
    // stream this suite created first.
    Row {
        case: "the second endpoint's own stream, by name",
        at: At::SecondStreamsEndpoint,
        named: Named::SecondByName,
        answer: Answer::Served,
    },
    Row {
        case: "the second endpoint's own stream, by ARN",
        at: At::SecondStreamsEndpoint,
        named: Named::SecondByArn,
        answer: Answer::Served,
    },
    Row {
        case: "the first stream on the second endpoint, by name",
        at: At::SecondStreamsEndpoint,
        named: Named::FirstByName,
        answer: Answer::WrongEndpoint,
    },
    Row {
        case: "the first stream on the second endpoint, by ARN",
        at: At::SecondStreamsEndpoint,
        named: Named::FirstByArn,
        answer: Answer::WrongEndpoint,
    },
    Row {
        case: "a stream name that exists nowhere, on the second endpoint",
        at: At::SecondStreamsEndpoint,
        named: Named::AbsentName,
        answer: Answer::NotFound,
    },
];

/// The two streams of this suite, each with its announced live-media endpoint and the
/// media only it holds.
struct Streams {
    harness: Harness,
    first: String,
    first_arn: String,
    first_endpoint: String,
    first_media: Cluster,
    second: String,
    second_arn: String,
    second_endpoint: String,
    second_media: Cluster,
    absent_name: String,
    absent_arn: String,
}

impl Streams {
    /// Create both streams, discover each one's live-media endpoint, and produce one
    /// Cluster of its own to each through that same endpoint's ingest API.
    async fn seed(prefix: &str) -> Self {
        let harness = Harness::new();
        let first = format!("{prefix}-first");
        let second = format!("{prefix}-second");
        let first_arn = harness.create_stream(CONTROL, &first).await;
        let second_arn = harness.create_stream(CONTROL, &second).await;
        let first_endpoint = harness
            .announced_authority(CONTROL, &first, "GET_MEDIA")
            .await;
        let second_endpoint = harness
            .announced_authority(CONTROL, &second, "GET_MEDIA")
            .await;
        assert_ne!(
            first_endpoint, second_endpoint,
            "setup: two streams must each be announced their own live-media endpoint, got \
             {first_endpoint:?} for both"
        );

        // The live read and the ingest are two APIs of ONE media endpoint, so the stream's
        // media is produced at the very address its reads are served at.
        let first_media = cluster(0, 2, 20, 0x41);
        let second_media = cluster(0, 2, 20, 0x5B);
        let seeded = Self {
            absent_name: format!("{prefix}-never-created"),
            absent_arn: first_arn.replace(&first, &format!("{prefix}-never-created")),
            harness,
            first,
            first_arn,
            first_endpoint,
            first_media,
            second,
            second_arn,
            second_endpoint,
            second_media,
        };
        seeded
            .produce(At::FirstStreamsEndpoint, Named::FirstByName)
            .await;
        seeded
            .produce(At::SecondStreamsEndpoint, Named::SecondByName)
            .await;
        seeded
    }

    /// Ingest the stream's own Cluster at its own endpoint.
    async fn produce(&self, at: At, named: Named) {
        let container = Container::of(vec![self.media(at).clone()]).bytes();
        let headers = ingest_headers(
            ("x-amzn-stream-name", self.addressed(named).1),
            "RELATIVE",
            producer_start(),
        );
        let response = self
            .harness
            .put_media_at(self.endpoint(at), &headers, container)
            .await;
        assert_eq!(
            status(&response),
            200,
            "setup {at:?}: the endpoint must take its own stream's media, got {} {}",
            status(&response),
            text(&response)
        );
    }

    /// The address of one row's endpoint.
    fn endpoint(&self, at: At) -> &str {
        match at {
            At::FirstStreamsEndpoint => &self.first_endpoint,
            At::SecondStreamsEndpoint => &self.second_endpoint,
        }
    }

    /// The media the stream that endpoint belongs to holds.
    fn media(&self, at: At) -> &Cluster {
        match at {
            At::FirstStreamsEndpoint => &self.first_media,
            At::SecondStreamsEndpoint => &self.second_media,
        }
    }

    /// The `(member, value)` pair a row addresses its stream with.
    fn addressed(&self, named: Named) -> (&'static str, String) {
        match named {
            Named::FirstByName => ("StreamName", self.first.clone()),
            Named::FirstByArn => ("StreamARN", self.first_arn.clone()),
            Named::SecondByName => ("StreamName", self.second.clone()),
            Named::SecondByArn => ("StreamARN", self.second_arn.clone()),
            Named::AbsentName => ("StreamName", self.absent_name.clone()),
            Named::AbsentArn => ("StreamARN", self.absent_arn.clone()),
        }
    }

    /// One `GetMedia` at `at`, naming the stream `named` and starting from `selector`.
    async fn read(
        &self,
        at: At,
        named: Named,
        selector: serde_json::Value,
    ) -> http::Response<Bytes> {
        let (member, value) = self.addressed(named);
        let mut body = ::serde_json::Map::new();
        body.insert(member.to_string(), serde_json::Value::String(value));
        body.insert("StartSelector".to_string(), selector);
        self.harness
            .call_at(
                self.endpoint(at),
                "/getMedia",
                &serde_json::Value::Object(body),
            )
            .await
    }
}

/// The selector a read starts from when the case is not about the cursor.
fn earliest() -> serde_json::Value {
    serde_json::json!({ "StartSelectorType": "EARLIEST" })
}
/// RULE: an announced live-media endpoint serves the ONE stream it was discovered for — a
/// read there naming another stream of the account is refused 400
/// `InvalidEndpointException` with that class's own sentence, a read naming an identifier
/// the registry does not hold is refused 404 `ResourceNotFoundException` with the sentence
/// this family states for a stream that is not there to serve, neither refusal states a
/// value from the request or serves any media, and the endpoint keeps serving its own
/// stream in either identifier form.
#[tokio::test]
async fn a_media_endpoint_serves_only_the_stream_it_was_discovered_for() {
    let streams = Streams::seed("scoped-read").await;

    for row in ROWS {
        let Row {
            case,
            at,
            named,
            answer,
        } = *row;
        // The identical read, differing only in the endpoint it was sent to and the stream
        // it named.
        let response = streams.read(at, named, earliest()).await;
        let (_, identifier) = streams.addressed(named);
        let own_frames = streams.media(at).frames();
        let other_frames = streams
            .media(match at {
                At::FirstStreamsEndpoint => At::SecondStreamsEndpoint,
                At::SecondStreamsEndpoint => At::FirstStreamsEndpoint,
            })
            .frames();

        match answer.refusal() {
            None => {
                assert_eq!(
                    status(&response),
                    200,
                    "case {case:?}: the endpoint must serve its own stream, got {} {}",
                    status(&response),
                    text(&response)
                );
                for frame in &own_frames {
                    assert!(
                        carries(response.body(), frame),
                        "case {case:?}: the payload must carry this stream's own frame \
                         {frame:?}, got {} byte(s)",
                        response.body().len()
                    );
                }
                for frame in &other_frames {
                    assert!(
                        !carries(response.body(), frame),
                        "case {case:?}: the payload must carry no frame of the other \
                         stream, but it carries {frame:?}"
                    );
                }
            }
            Some((expected_status, expected_code, expected_message)) => {
                assert_eq!(
                    status(&response),
                    expected_status,
                    "case {case:?}: expected HTTP {expected_status}, got {} {}",
                    status(&response),
                    text(&response)
                );
                assert_eq!(
                    error_code(&response),
                    expected_code,
                    "case {case:?}: expected {expected_code}, got {:?} {}",
                    error_code(&response),
                    text(&response)
                );
                assert_eq!(
                    message(&response),
                    expected_message,
                    "case {case:?}: expected that class's own sentence, got {:?}",
                    message(&response)
                );
                assert!(
                    !text(&response).contains(identifier.as_str()),
                    "case {case:?}: the refusal must state no identifier from the request, \
                     but {:?} carries {identifier:?}",
                    text(&response)
                );
                for frame in own_frames.iter().chain(&other_frames) {
                    assert!(
                        !carries(response.body(), frame),
                        "case {case:?}: a refused read serves no media, but the answer \
                         carries {frame:?}"
                    );
                }
            }
        }
    }
}

/// RULE: the endpoint is read against the stream BEFORE the starting cursor the request
/// named is interpreted, so a read that used the wrong endpoint earns the wrong-endpoint
/// class whatever `StartSelector` it carries — while the identical selector at the
/// stream's own endpoint is answered on its own terms.
#[tokio::test]
async fn the_wrong_endpoint_is_settled_before_the_start_selector_is_read() {
    let streams = Streams::seed("selector-read").await;

    // (case, the selector) — spanning the selector types a request can carry, including
    // two whose cursor this stream has no chunk for and which are otherwise refused
    // `InvalidArgumentException`.
    let selectors: Vec<(&str, serde_json::Value)> = vec![
        ("EARLIEST", earliest()),
        ("NOW", serde_json::json!({ "StartSelectorType": "NOW" })),
        (
            "PRODUCER_TIMESTAMP",
            serde_json::json!({
                "StartSelectorType": "PRODUCER_TIMESTAMP",
                "StartTimestamp": 1_761_000_000.0,
            }),
        ),
        (
            "SERVER_TIMESTAMP",
            serde_json::json!({
                "StartSelectorType": "SERVER_TIMESTAMP",
                "StartTimestamp": 1_761_000_000.0,
            }),
        ),
        (
            "FRAGMENT_NUMBER naming no fragment of the stream",
            serde_json::json!({
                "StartSelectorType": "FRAGMENT_NUMBER",
                "AfterFragmentNumber": "91343852333181432387507324523",
            }),
        ),
        (
            "CONTINUATION_TOKEN this service never issued",
            serde_json::json!({
                "StartSelectorType": "CONTINUATION_TOKEN",
                "ContinuationToken": "never-issued-token",
            }),
        ),
    ];

    for (case, selector) in &selectors {
        let refused = streams
            .read(
                At::FirstStreamsEndpoint,
                Named::SecondByName,
                selector.clone(),
            )
            .await;
        assert_eq!(
            status(&refused),
            400,
            "case {case:?}: a read that used the wrong endpoint is refused 400, got {} {}",
            status(&refused),
            text(&refused)
        );
        assert_eq!(
            error_code(&refused),
            "InvalidEndpointException",
            "case {case:?}: the endpoint decides before the cursor, so the class is the \
             wrong-endpoint one, got {:?} {}",
            error_code(&refused),
            text(&refused)
        );
        assert_eq!(
            message(&refused),
            WRONG_ENDPOINT,
            "case {case:?}: expected the wrong-endpoint sentence, got {:?}",
            message(&refused)
        );

        // The same selector at the stream's OWN endpoint is answered on its own terms:
        // served, or refused for the cursor rather than for the endpoint.
        let own = streams
            .read(
                At::SecondStreamsEndpoint,
                Named::SecondByName,
                selector.clone(),
            )
            .await;
        assert_ne!(
            error_code(&own),
            "InvalidEndpointException",
            "case {case:?}: the stream's own endpoint must not answer the wrong-endpoint \
             class, got {} {}",
            status(&own),
            text(&own)
        );
    }
}
