// SPDX-License-Identifier: CC0-1.0

//! End-to-end behaviour of the two operations this service binds, driven over HTTP
//! through the same `MultiServiceRouter::dispatch` the production front door uses.
//!
//! Both operations answer an EMPTY body, so "it worked" is only observable through
//! the shared store: the session the channel's first join creates is the session
//! every later join of either kind lands on. Every test here asserts the wire answer
//! AND the state the call left behind.
//!
//! The channel itself is NOT this service's resource: it is created by the Kinesis
//! Video control plane, which seeds the one signaling registry both joins resolve a
//! `channelArn` against, so each case puts its channel there first
//! ([`register_channel`]) exactly as that create does.
//!
//! One thing a producer join does is observable to a PEER rather than to the caller:
//! the SDP offer it pushes down the channel's signaling plane. Those cases mount the
//! signaling service beside this one, open a real connection to the channel's
//! announced WebSocket as the video producing device does, and read the frames that
//! connection is delivered.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures_util::StreamExt as _;
use serde_json::Value;
use service_com_amazonaws_kinesisvideosignaling_awsacuitysignalingservice as signaling;
use service_com_amazonaws_kinesisvideowebrtcstorage_awsacuityroutingservicelambda as webrtc_storage;
use signaling::state::session::SDP_OFFER;
use smithy_server_runtime::MultiServiceRouter;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use webrtc_storage::state::{
    ComAmazonawsKinesisvideowebrtcstorageAwsacuityroutingservicelambdaState as StorageState,
    StorageSession,
};

/// Signaling channel ARNs spanning what the modelled `@pattern` on `ChannelArn`
/// (`^arn:(aws[a-zA-Z-]*):kinesisvideo:[a-z0-9-]+:[0-9]+:[a-z]+/[a-zA-Z0-9_.-]+/[0-9]+$`,
/// from `smithy_query(query="operation",operation="JoinStorageSession")`) admits of the
/// ARNs a signaling channel of this emulator can CARRY: several regions, account ids,
/// name spellings and creation times.
const CHANNEL_ARNS: &[&str] = &[
    "arn:aws:kinesisvideo:us-east-1:123456789012:channel/demo-channel/1700000000000",
    "arn:aws:kinesisvideo:eu-west-2:000000000000:channel/edge.cam_01-A/1",
    "arn:aws:kinesisvideo:cn-north-1:210987654321:channel/c/9999999999999",
    "arn:aws:kinesisvideo:us-gov-west-1:111122223333:channel/Gov_Cam.2/1762000000000",
];

/// An answer from the emulator: the HTTP status and the wire error code
/// (restJson1 carries it as the body's `__type` and the `x-amzn-errortype` header).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Answer {
    status: u16,
    error_code: Option<String>,
}

impl Answer {
    fn ok(&self) -> bool {
        self.status == 200 && self.error_code.is_none()
    }
}

/// The emulator under test: one service on one shared engine, reached over HTTP.
struct Emulator {
    engine: Arc<engine::InMemoryEngine>,
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        Self::mounted(false)
    }

    /// The emulator with the SIGNALING data plane mounted beside this service, on the
    /// same engine: a channel's live peers are that service's state, so a case that
    /// reads what a peer was delivered needs it.
    fn with_signaling_plane() -> Self {
        Self::mounted(true)
    }

    fn mounted(with_signaling_plane: bool) -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // Signature verification is the core's business, not this service's; the
        // requests below are unsigned, so no authenticator is registered.
        router.set_request_authentication(false);
        router.register_service(webrtc_storage::EmulatedService::new(Arc::clone(&engine)));
        if with_signaling_plane {
            router.register_service(signaling::EmulatedService::new(Arc::clone(&engine)));
        }
        Self { engine, router }
    }

    async fn post(&self, path: &str, body: &serde_json::Value) -> Answer {
        let req = http::Request::builder()
            .method("POST")
            .uri(format!("http://kinesisvideo.us-east-1.amazonaws.com{path}"))
            // The routing key: this service's sigv4 signing name, `kinesisvideo`.
            .header("host", "kinesisvideo.us-east-1.amazonaws.com")
            .header("content-type", "application/json")
            .body(Bytes::from(
                serde_json::to_vec(body).expect("a serializable request body"),
            ))
            .expect("a valid request");
        let resp = self.router.dispatch(req).await;
        let status = resp.status().as_u16();
        let error_code = serde_json::from_slice::<serde_json::Value>(resp.body())
            .ok()
            .and_then(|body| {
                body.get("__type")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            });
        Answer { status, error_code }
    }

    async fn join_as_producer(&self, channel_arn: &str) -> Answer {
        self.post(
            "/joinStorageSession",
            &serde_json::json!({ "channelArn": channel_arn }),
        )
        .await
    }

    async fn join_as_viewer(&self, channel_arn: &str, client_id: &str) -> Answer {
        self.post(
            "/joinStorageSessionAsViewer",
            &serde_json::json!({ "channelArn": channel_arn, "clientId": client_id }),
        )
        .await
    }

    /// Read the channel's session out of the shared store — the only place the
    /// outcome of an empty-bodied join is visible.
    fn with_session<R>(&self, channel_arn: &str, read: impl FnOnce(Option<&StorageSession>) -> R) -> R {
        let state = self.engine.state::<StorageState>();
        let guard = state.lock();
        read(guard.session(channel_arn))
    }

    fn has_no_session(&self, channel_arn: &str) -> bool {
        self.with_session(channel_arn, |session| session.is_none())
    }

    /// Whether the channel is one this emulator HOLDS — read from the registry the
    /// Kinesis Video control plane's `CreateSignalingChannel` writes and its
    /// `DeleteSignalingChannel` empties, which is where both joins resolve existence.
    fn knows_channel(&self, channel_arn: &str) -> bool {
        signaling::state::knows_channel_of(&*self.engine, channel_arn)
    }

    fn viewer_ids(&self, channel_arn: &str) -> BTreeSet<String> {
        self.with_session(channel_arn, |session| {
            session
                .map(|s| s.viewers.keys().cloned().collect())
                .unwrap_or_default()
        })
    }
}

/// The whole happy path, per channel: on a channel the control plane holds, a VIEWER
/// may open the session before any video producing device has joined; the producer
/// join then lands on THAT session; a second clientId is a second viewer and a repeat
/// one is the same viewer; and a second producer join supersedes the first rather
/// than erroring or no-op'ing.
#[tokio::test]
async fn either_participant_opens_the_session_every_later_join_lands_on() {
    let emulator = Emulator::new();

    for channel_arn in CHANNEL_ARNS {
        // Step 1 — the channel exists on the control plane, and the VIEWER joins
        // first: "Both a video producing device and viewers can join a session
        // first and wait for other participants."
        register_channel(&emulator, channel_arn);
        let first = emulator.join_as_viewer(channel_arn, "viewer-1").await;
        assert!(
            first.ok(),
            "case {channel_arn:?}: expected 200 from a viewer join before any producer \
             join, got {first:?}"
        );
        emulator.with_session(channel_arn, |session| {
            let session = session.unwrap_or_else(|| {
                panic!("case {channel_arn:?}: the viewer join recorded no session")
            });
            assert_eq!(
                session.channel_arn, *channel_arn,
                "case {channel_arn:?}: the session is keyed by the ARN as supplied, got {:?}",
                session.channel_arn
            );
            assert_eq!(
                session.join_count, 0,
                "case {channel_arn:?}: a viewer join is not a producer join, so the \
                 session has served none, got {}",
                session.join_count
            );
            assert!(
                session.producer.is_none(),
                "case {channel_arn:?}: no video producing device has joined yet, got {:?}",
                session.producer
            );
            assert_eq!(
                session.viewers.keys().collect::<Vec<_>>(),
                vec!["viewer-1"],
                "case {channel_arn:?}: the viewer that joined must be on the session"
            );
        });

        // Step 2 — the producer join lands on the session the viewer opened.
        let producer = emulator.join_as_producer(channel_arn).await;
        assert!(
            producer.ok(),
            "case {channel_arn:?}: expected 200 with no error from JoinStorageSession, \
             got {producer:?}"
        );
        emulator.with_session(channel_arn, |session| {
            let session = session.unwrap_or_else(|| {
                panic!("case {channel_arn:?}: JoinStorageSession recorded no session")
            });
            assert_eq!(
                session.join_count, 1,
                "case {channel_arn:?}: expected join_count 1 after one producer join, got {}",
                session.join_count
            );
            let producer = session.producer.as_ref().unwrap_or_else(|| {
                panic!("case {channel_arn:?}: no video producing device was recorded")
            });
            assert_eq!(
                producer.generation, 1,
                "case {channel_arn:?}: expected producer generation 1, got {}",
                producer.generation
            );
            assert_eq!(
                session.viewers.len(),
                1,
                "case {channel_arn:?}: a producer join must register no viewer and drop \
                 none, got {:?}",
                session.viewers.keys().collect::<Vec<_>>()
            );
        });

        // Step 4 — a second clientId is a second viewer, not a replacement.
        let second = emulator.join_as_viewer(channel_arn, "viewer-2").await;
        assert!(
            second.ok(),
            "case {channel_arn:?}: expected 200 for a second viewer, got {second:?}"
        );
        assert_eq!(
            emulator.viewer_ids(channel_arn),
            ["viewer-1".to_owned(), "viewer-2".to_owned()]
                .into_iter()
                .collect::<BTreeSet<_>>(),
            "case {channel_arn:?}: two distinct clientIds must be two viewers"
        );

        // Step 5 — a repeat join by viewer-1 refreshes that one viewer.
        let repeat = emulator.join_as_viewer(channel_arn, "viewer-1").await;
        assert!(
            repeat.ok(),
            "case {channel_arn:?}: expected 200 for a repeat viewer join, got {repeat:?}"
        );
        emulator.with_session(channel_arn, |session| {
            let session = session.expect("the session exists");
            assert_eq!(
                session.viewers.len(),
                2,
                "case {channel_arn:?}: a repeat join must not duplicate the viewer, got {:?}",
                session.viewers.keys().collect::<Vec<_>>()
            );
            let viewer = &session.viewers["viewer-1"];
            assert_eq!(
                viewer.join_count, 2,
                "case {channel_arn:?}: expected viewer-1 join_count 2, got {}",
                viewer.join_count
            );
            assert_eq!(
                viewer.client_id, "viewer-1",
                "case {channel_arn:?}: the viewer is keyed by its own clientId, got {:?}",
                viewer.client_id
            );
        });

        // Step 6 — the API is not idempotent and the most recent producer wins.
        let created_at = emulator.with_session(channel_arn, |s| s.expect("session").created_at);
        let again = emulator.join_as_producer(channel_arn).await;
        assert!(
            again.ok(),
            "case {channel_arn:?}: a second producer join must succeed, got {again:?}"
        );
        emulator.with_session(channel_arn, |session| {
            let session = session.expect("the session exists");
            assert_eq!(
                session.join_count, 2,
                "case {channel_arn:?}: expected join_count 2 after two producer joins, got {}",
                session.join_count
            );
            assert_eq!(
                session.producer.as_ref().expect("a producer").generation,
                2,
                "case {channel_arn:?}: the newer producer must supersede the incumbent, got {:?}",
                session.producer
            );
            assert_eq!(
                session.created_at, created_at,
                "case {channel_arn:?}: re-joining must not re-create the session"
            );
            assert_eq!(
                session.viewers.len(),
                2,
                "case {channel_arn:?}: re-joining as producer must keep the viewers, got {:?}",
                session.viewers.keys().collect::<Vec<_>>()
            );
        });
    }

    // Sessions are per channel: every ARN above kept its own.
    for channel_arn in CHANNEL_ARNS {
        assert_eq!(
            emulator.with_session(channel_arn, |s| s.expect("session").join_count),
            2,
            "case {channel_arn:?}: one channel's joins must not be counted on another"
        );
    }
}

/// BOTH joins are refused for any channel ARN the control plane holds no channel
/// for — including ARNs that differ from a held one in a single segment, so the
/// decision is made from the registry and not from the ARN's shape.
#[tokio::test]
async fn a_join_of_a_channel_the_control_plane_does_not_hold_is_not_found() {
    let known = "arn:aws:kinesisvideo:us-east-1:123456789012:channel/demo-channel/1700000000000";
    // Each row changes exactly one segment of `known`: name, region, account,
    // creation time, partition.
    let unknown: &[&str] = &[
        "arn:aws:kinesisvideo:us-east-1:123456789012:channel/other-channel/1700000000000",
        "arn:aws:kinesisvideo:us-west-2:123456789012:channel/demo-channel/1700000000000",
        "arn:aws:kinesisvideo:us-east-1:210987654321:channel/demo-channel/1700000000000",
        "arn:aws:kinesisvideo:us-east-1:123456789012:channel/demo-channel/1700000000001",
        "arn:aws-cn:kinesisvideo:us-east-1:123456789012:channel/demo-channel/1700000000000",
    ];

    let emulator = Emulator::new();
    register_channel(&emulator, known);
    assert!(
        emulator.join_as_producer(known).await.ok(),
        "the channel the control plane holds must accept a producer join"
    );

    for channel_arn in unknown {
        for (role, answer) in [
            (
                "JoinStorageSessionAsViewer",
                emulator.join_as_viewer(channel_arn, "viewer-1").await,
            ),
            (
                "JoinStorageSession",
                emulator.join_as_producer(channel_arn).await,
            ),
        ] {
            assert_eq!(
                answer,
                Answer {
                    status: 404,
                    error_code: Some("ResourceNotFoundException".to_owned()),
                },
                "case {channel_arn:?} on {role}: expected 404 ResourceNotFoundException, \
                 got {answer:?}"
            );
        }
        assert!(
            !emulator.knows_channel(channel_arn),
            "case {channel_arn:?}: a refused join must not bring the channel into being"
        );
        assert!(
            emulator.has_no_session(channel_arn),
            "case {channel_arn:?}: a refused join must create no session"
        );
    }

    // The channel that exists is untouched by every refusal above.
    assert_eq!(
        emulator.viewer_ids(known),
        BTreeSet::new(),
        "refused joins on other ARNs must not land on the held channel"
    );
    assert_eq!(
        emulator.with_session(known, |s| s.expect("session").join_count),
        1,
        "refused joins on other ARNs must not touch the held channel's session"
    );
}

/// The modelled ARN pattern admits ANY lowercase resource type and puts no length
/// on the ARN, so an ARN that names something other than a signaling channel, or
/// that is longer than a `kinesisvideo` resource ARN can be, satisfies the schema
/// and must be refused on semantic grounds — with `InvalidArgumentException`, by
/// BOTH operations, recording nothing.
#[tokio::test]
async fn an_arn_that_cannot_name_a_signaling_channel_is_an_invalid_argument() {
    let emulator = Emulator::new();

    // Resource types a `kinesisvideo` ARN can carry; `[a-z]+` in the modelled
    // pattern keeps them lowercase. `stream` is the service's other resource type.
    let resource_types = ["stream", "mediastream", "account"];
    // Rows past a bound the Kinesis Video control-plane model publishes: a channel
    // name longer than `ChannelName`'s 256 (`smithy_query(query="shape",
    // shape="ChannelName")` on the kinesisvideo model), and an ARN longer than
    // `ResourceARN`'s 1024 — both still matching this service's `@pattern`.
    let over_long_name = format!(
        "arn:aws:kinesisvideo:us-east-1:123456789012:channel/{}/1700000000000",
        "n".repeat(257)
    );
    let over_long_arn = format!(
        "arn:aws:kinesisvideo:us-east-1:{}:channel/demo-channel/1700000000000",
        "1".repeat(1100)
    );
    let mut rows: Vec<(String, String)> = resource_types
        .iter()
        .map(|resource_type| {
            (
                format!("resource type {resource_type:?}"),
                format!(
                    "arn:aws:kinesisvideo:us-east-1:123456789012:{resource_type}/demo/1700000000000"
                ),
            )
        })
        .collect();
    rows.push(("channel name of 257 characters".to_owned(), over_long_name));
    rows.push(("ARN of more than 1024 characters".to_owned(), over_long_arn));

    for (case, channel_arn) in &rows {
        let producer = emulator.join_as_producer(channel_arn).await;
        assert_eq!(
            producer,
            Answer {
                status: 400,
                error_code: Some("InvalidArgumentException".to_owned()),
            },
            "case {case:?}: JoinStorageSession must refuse it, got {producer:?}"
        );
        let viewer = emulator.join_as_viewer(channel_arn, "viewer-1").await;
        assert_eq!(
            viewer,
            Answer {
                status: 400,
                error_code: Some("InvalidArgumentException".to_owned()),
            },
            "case {case:?}: JoinStorageSessionAsViewer must refuse it, got {viewer:?}"
        );
        assert!(
            !emulator.knows_channel(channel_arn),
            "case {case:?}: a refused join must register nothing"
        );
        assert!(
            emulator.has_no_session(channel_arn),
            "case {case:?}: a refused join must create no session"
        );
    }

    // The published bound itself is accepted: a channel name of exactly 256.
    let at_bound = format!(
        "arn:aws:kinesisvideo:us-east-1:123456789012:channel/{}/1700000000000",
        "n".repeat(256)
    );
    register_channel(&emulator, &at_bound);
    let answer = emulator.join_as_producer(&at_bound).await;
    assert!(
        answer.ok(),
        "case channel name of exactly 256 characters: a name on the published bound \
         must be accepted, got {answer:?}"
    );
}

/// Values outside the modelled constraints are refused by this service's FRONT DOOR
/// before any handler runs: 400 `BadRequestException` — the one code this service
/// emits for a body it cannot accept — never a 501 and never a success. The rows walk
/// each constraint the model declares: `channelArn`'s pattern, and `clientId`'s
/// pattern and 1..256 length.
#[tokio::test]
async fn values_outside_the_modelled_constraints_are_refused_with_400() {
    let known = "arn:aws:kinesisvideo:us-east-1:123456789012:channel/demo-channel/1700000000000";
    let emulator = Emulator::new();
    register_channel(&emulator, known);
    assert!(
        emulator.join_as_producer(known).await.ok(),
        "the channel the control plane holds must accept a producer join"
    );

    // (case, channelArn, clientId) — a `None` clientId exercises JoinStorageSession.
    let too_long = "v".repeat(257);
    let rows: &[(&str, &str, Option<&str>)] = &[
        ("channelArn is not an ARN at all", "not-an-arn", None),
        ("channelArn has no creation time", "arn:aws:kinesisvideo:us-east-1:123456789012:channel/demo-channel", None),
        ("channelArn resource type is not lowercase", "arn:aws:kinesisvideo:us-east-1:123456789012:Channel/demo-channel/1700000000000", None),
        ("channelArn account is not numeric", "arn:aws:kinesisvideo:us-east-1:acct:channel/demo-channel/1700000000000", None),
        ("channelArn is not an ARN at all, on the viewer path", "not-an-arn", Some("viewer-1")),
        ("clientId contains a space and a bang", known, Some("bad id!")),
        ("clientId contains a slash", known, Some("viewer/1")),
        ("clientId is empty (length minimum 1)", known, Some("")),
        ("clientId is 257 characters (length maximum 256)", known, Some(too_long.as_str())),
    ];

    for (case, channel_arn, client_id) in rows {
        let answer = match client_id {
            Some(client_id) => emulator.join_as_viewer(channel_arn, client_id).await,
            None => emulator.join_as_producer(channel_arn).await,
        };
        assert_eq!(
            answer,
            Answer {
                status: 400,
                error_code: Some(webrtc_storage::service::BAD_REQUEST_EXCEPTION.to_owned()),
            },
            "case {case:?}: a modelled-constraint violation must be refused by the front \
             door with 400 {}, got {answer:?}",
            webrtc_storage::service::BAD_REQUEST_EXCEPTION
        );
    }

    // The published `clientId` bounds themselves are accepted.
    for length in [1_usize, 256] {
        let client_id = "v".repeat(length);
        let answer = emulator.join_as_viewer(known, &client_id).await;
        assert!(
            answer.ok(),
            "case clientId of length {length}: a value on the published bound must be \
             accepted, got {answer:?}"
        );
    }
    assert_eq!(
        emulator.viewer_ids(known).len(),
        2,
        "only the two accepted viewers may have been registered, got {:?}",
        emulator.viewer_ids(known)
    );
}

/// The counting invariants, over a long deterministic interleaving of joins across
/// several channels and clients: the session's `join_count` is the number of
/// producer joins it has served, its producer's `generation` is the `join_count` of
/// the most recent one, and each viewer's `join_count` is the number of times that
/// `clientId` joined — whatever order the calls arrive in.
#[tokio::test]
async fn join_counts_track_the_calls_whatever_the_interleaving() {
    /// Fixed seed, so this sequence is the same on every run (printed here rather
    /// than generated, so a failure is reproducible from the source alone).
    const SEED: u64 = 0x5DEE_CE66_D15E_A5E5;
    /// Bounded case count: step budget for the interleaving.
    const STEPS: usize = 120;

    // xorshift64*: a deterministic sequence, no generator crate and nothing on disk.
    let mut rng_state = SEED;
    let mut next = move || {
        rng_state ^= rng_state >> 12;
        rng_state ^= rng_state << 25;
        rng_state ^= rng_state >> 27;
        rng_state = rng_state.wrapping_mul(0x2545_F491_4F6C_DD1D);
        rng_state
    };

    // As many distinct clientIds as one session admits ([`MAX_VIEWERS_PER_SESSION`]),
    // so every join in the interleaving is one the quota allows.
    let clients = ["cam.a", "cam_b", "cam-c"];
    assert_eq!(
        clients.len(),
        webrtc_storage::state::MAX_VIEWERS_PER_SESSION,
        "the interleaving drives one client per viewer slot"
    );
    let emulator = Emulator::new();
    for channel_arn in CHANNEL_ARNS {
        register_channel(&emulator, channel_arn);
    }

    // The independent model this test checks the emulator against.
    let mut expected_producer_joins = vec![0_u64; CHANNEL_ARNS.len()];
    let mut expected_viewer_joins = vec![[0_u64; 3]; CHANNEL_ARNS.len()];

    for step in 0..STEPS {
        let roll = next();
        let channel = (roll >> 3) as usize % CHANNEL_ARNS.len();
        let channel_arn = CHANNEL_ARNS[channel];
        let as_producer = expected_producer_joins[channel] == 0 || roll % 3 == 0;

        if as_producer {
            let answer = emulator.join_as_producer(channel_arn).await;
            assert!(
                answer.ok(),
                "case step {step} producer join on {channel_arn:?}: expected 200, got {answer:?}"
            );
            expected_producer_joins[channel] += 1;
        } else {
            let client = (roll >> 11) as usize % clients.len();
            let answer = emulator.join_as_viewer(channel_arn, clients[client]).await;
            assert!(
                answer.ok(),
                "case step {step} viewer join {:?} on {channel_arn:?}: expected 200, got {answer:?}",
                clients[client]
            );
            expected_viewer_joins[channel][client] += 1;
            // A viewer join is not a producer join: it must leave the incumbent
            // producing device, and its joined_at, exactly where they were.
            let producer = emulator.with_session(channel_arn, |session| {
                session.expect("the session exists").producer.clone()
            });
            assert_eq!(
                producer.as_ref().map(|p| p.generation),
                Some(expected_producer_joins[channel]),
                "case step {step} viewer join on {channel_arn:?}: a viewer join must not \
                 change the incumbent producer, got {producer:?}"
            );
        }
    }

    for (channel, channel_arn) in CHANNEL_ARNS.iter().enumerate() {
        let producer_joins = expected_producer_joins[channel];
        emulator.with_session(channel_arn, |session| {
            let session = session.unwrap_or_else(|| {
                panic!("case {channel_arn:?}: expected a session after {producer_joins} joins")
            });
            assert_eq!(
                session.join_count, producer_joins,
                "case {channel_arn:?}: expected join_count {producer_joins}, got {}",
                session.join_count
            );
            assert_eq!(
                session.producer.as_ref().map(|p| p.generation),
                Some(producer_joins),
                "case {channel_arn:?}: the incumbent producer's generation must be the \
                 join_count of the most recent producer join, got {:?}",
                session.producer
            );
            for (client, client_id) in clients.iter().enumerate() {
                let joins = expected_viewer_joins[channel][client];
                match session.viewers.get(*client_id) {
                    Some(viewer) => assert_eq!(
                        viewer.join_count, joins,
                        "case {channel_arn:?}/{client_id:?}: expected {joins} viewer joins, \
                         got {}",
                        viewer.join_count
                    ),
                    None => assert_eq!(
                        joins, 0,
                        "case {channel_arn:?}/{client_id:?}: {joins} joins were accepted but \
                         no viewer was registered"
                    ),
                }
            }
        });
    }
}

/// The viewer quota of a storage session, and the bound that keeps request-keyed
/// state from growing without end, are enforced through the modelled
/// `ClientLimitExceededException`, and only for a join that would ADD an entry: a
/// repeat join of something already recorded still succeeds.
#[tokio::test]
async fn joins_past_a_limit_are_refused_with_client_limit_exceeded() {
    let limit_exceeded = Answer {
        status: 400,
        error_code: Some("ClientLimitExceededException".to_owned()),
    };
    let channel_arn =
        "arn:aws:kinesisvideo:us-east-1:123456789012:channel/demo-channel/1700000000000";
    let emulator = Emulator::new();
    register_channel(&emulator, channel_arn);
    assert!(
        emulator.join_as_producer(channel_arn).await.ok(),
        "the channel the control plane holds must accept a producer join"
    );

    // Every viewer up to the per-channel maximum is admitted.
    for index in 0..webrtc_storage::state::MAX_VIEWERS_PER_SESSION {
        let client_id = format!("viewer-{index}");
        let answer = emulator.join_as_viewer(channel_arn, &client_id).await;
        assert!(
            answer.ok(),
            "case viewer {index} of {}: expected 200, got {answer:?}",
            webrtc_storage::state::MAX_VIEWERS_PER_SESSION
        );
    }
    assert_eq!(
        emulator.viewer_ids(channel_arn).len(),
        webrtc_storage::state::MAX_VIEWERS_PER_SESSION,
        "every viewer up to the limit must be registered, got {:?}",
        emulator.viewer_ids(channel_arn)
    );

    // One more DISTINCT viewer is refused, and records nothing.
    let answer = emulator.join_as_viewer(channel_arn, "viewer-one-too-many").await;
    assert_eq!(
        answer, limit_exceeded,
        "case one viewer past the limit: expected 400 ClientLimitExceededException, \
         got {answer:?}"
    );
    assert!(
        !emulator.viewer_ids(channel_arn).contains("viewer-one-too-many"),
        "case one viewer past the limit: the refused viewer must not be registered"
    );

    // A viewer already on the session still joins again at the limit.
    let answer = emulator.join_as_viewer(channel_arn, "viewer-0").await;
    assert!(
        answer.ok(),
        "case repeat join at the limit: an already-registered viewer must still join, \
         got {answer:?}"
    );
    emulator.with_session(channel_arn, |session| {
        let session = session.expect("the session exists");
        assert_eq!(
            session.viewers.len(),
            webrtc_storage::state::MAX_VIEWERS_PER_SESSION,
            "case repeat join at the limit: the viewer count must not move, got {:?}",
            session.viewers.len()
        );
        assert_eq!(
            session.viewers["viewer-0"].join_count, 2,
            "case repeat join at the limit: expected join_count 2 for viewer-0, got {}",
            session.viewers["viewer-0"].join_count
        );
    });

    // The session bound, the same way: fill the store to its maximum through the
    // state API (the request path would be the same calls), then drive the next
    // join over HTTP for a channel the control plane DOES hold, so the refusal is
    // the bound and not a missing channel.
    let filler = Emulator::new();
    let now = std::time::SystemTime::now();
    {
        let state = filler.engine.state::<StorageState>();
        let mut store = state.lock();
        for index in 0..webrtc_storage::state::MAX_SIGNALING_CHANNELS {
            let arn =
                format!("arn:aws:kinesisvideo:us-east-1:123456789012:channel/c{index}/1700000000000");
            store
                .join_as_producer(&arn, now)
                .unwrap_or_else(|refused| panic!("case channel {index}: {}", refused.message()));
        }
        assert_eq!(
            store.channel_count(),
            webrtc_storage::state::MAX_SIGNALING_CHANNELS,
            "the store must hold exactly the maximum, got {}",
            store.channel_count()
        );
    }
    let one_too_many =
        "arn:aws:kinesisvideo:us-east-1:123456789012:channel/one-too-many/1700000000000";
    register_channel(&filler, one_too_many);
    let answer = filler.join_as_producer(one_too_many).await;
    assert_eq!(
        answer, limit_exceeded,
        "case one session past the bound: expected 400 ClientLimitExceededException, \
         got {answer:?}"
    );
    assert!(
        filler.has_no_session(one_too_many),
        "case one session past the bound: the refused channel must have no session"
    );

    // A channel that already has a session still accepts a producer join at the bound.
    let existing = "arn:aws:kinesisvideo:us-east-1:123456789012:channel/c0/1700000000000";
    register_channel(&filler, existing);
    let answer = filler.join_as_producer(existing).await;
    assert!(
        answer.ok(),
        "case repeat producer join at the session bound: expected 200, got {answer:?}"
    );
    assert_eq!(
        filler.with_session(existing, |s| s.expect("session").join_count),
        2,
        "case repeat producer join at the session bound: the join must have been counted"
    );
}

// The SDP offer half of a producer join: what the channel's video producing device is
// pushed, read off a real connection to the channel's own WebSocket.
//
// The rule these cases assert, one level above the recorded case (one channel, one open
// master connection, one join):
//
// > EVERY `JoinStorageSession` call makes the channel's storage session offer THAT join a
// > session description of its own — the API "is not idempotent" and "an offer is sent once
// > per each call" — pushed as one `SDP_OFFER` signaling message onto the connection the
// > video producing device holds to that channel, carrying a base64 `messagePayload` that
// > decodes to the JSEP envelope `{"type":"offer","sdp":…}` whose `sdp` is an RFC 4566
// > description ("v=0", plus a media line per track WebRTC ingestion requires: H.264 video
// > and Opus audio).
//
// The rows span what the rule generalises over: the channel, how many successive calls one
// session serves, and whether a producing device is attached to receive the offer at all.

/// How long a case waits for one offer before failing: generous for a loaded test machine,
/// and bounded so a case that will never be answered fails instead of hanging the suite.
const OFFER_DEADLINE: Duration = Duration::from_secs(30);

/// The retention a channel's control-plane record states, in seconds: the default
/// `SingleMasterConfiguration.MessageTtlSeconds` the Kinesis Video control plane registers
/// a channel with ("the default configuration for the channel message's time to live is 60
/// seconds").
const MESSAGE_TTL_SECS: i32 = 60;

/// The status a completed RFC 6455 opening handshake carries (§4.1).
const SWITCHING_PROTOCOLS: u16 = 101;

/// The loopback host the core binds a resource listener on by default, and so the only
/// address these cases ever dial.
const LOOPBACK_HOST: &str = "127.0.0.1";

/// The socket the video producing device holds after its handshake.
type ClientSocket = tokio_tungstenite::WebSocketStream<TcpStream>;

/// Put a channel in the signaling registry the way the Kinesis Video control plane's
/// `CreateSignalingChannel` does, so its data plane serves the channel.
fn register_channel(emulator: &Emulator, channel_arn: &str) {
    assert_eq!(
        signaling::state::register_channel_of(&*emulator.engine, channel_arn, MESSAGE_TTL_SECS),
        signaling::state::ChannelRegistration::Registered,
        "case {channel_arn:?}: the signaling registry must accept the created channel"
    );
}

/// The port of the channels' announced WebSocket endpoint, having checked its host is
/// [`LOOPBACK_HOST`] — so the dial below names that literal and reaches nothing else.
fn loopback_port(case: &str, announced: &str) -> u16 {
    let authority = announced
        .strip_prefix(&format!("{}://", signaling::state::WSS_SCHEME))
        .unwrap_or_else(|| panic!("case {case:?}: expected a wss:// URL, got {announced:?}"));
    let (host, port) = authority
        .rsplit_once(':')
        .unwrap_or_else(|| panic!("case {case:?}: expected a host:port, got {authority:?}"));
    assert_eq!(
        host, LOOPBACK_HOST,
        "case {case:?}: a case dials only the emulator's own loopback listener, got \
         {authority:?}"
    );
    port.parse()
        .unwrap_or_else(|_| panic!("case {case:?}: expected a port, got {authority:?}"))
}

/// Percent-encode one query value the way a signing client writes it into a presigned URL.
fn encoded(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

/// Connect to the channel's announced WebSocket as its MASTER, which is the role the video
/// producing device holds: the SDKs name the channel in `X-Amz-ChannelARN` and a viewer's
/// own id in `X-Amz-ClientId`, so a connection naming no client id is the master's.
async fn master_socket(case: &str, announced: &str, channel_arn: &str) -> ClientSocket {
    let port = loopback_port(case, announced);
    // The emulator's own loopback port, named as a literal: this case dials that address
    // and nothing else.
    let stream = TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap_or_else(|error| {
            panic!("case {case:?}: the announced socket must accept a connection, got {error}")
        });
    let url = format!(
        "ws://{LOOPBACK_HOST}:{port}/?X-Amz-ChannelARN={}&X-Amz-Algorithm=AWS4-HMAC-SHA256",
        encoded(channel_arn)
    );
    let (socket, response) = tokio_tungstenite::client_async(&url, stream)
        .await
        .unwrap_or_else(|error| {
            panic!("case {case:?}: the master's handshake must be completed, got {error}")
        });
    assert_eq!(
        response.status().as_u16(),
        SWITCHING_PROTOCOLS,
        "case {case:?}: a completed handshake must answer 101"
    );
    socket
}

/// The next frame `socket` is delivered, parsed, or `None` when none arrives within
/// [`OFFER_DEADLINE`].
async fn next_frame(case: &str, socket: &mut ClientSocket) -> Option<Value> {
    match tokio::time::timeout(OFFER_DEADLINE, socket.next()).await {
        Ok(Some(Ok(Message::Text(text)))) => Some(
            serde_json::from_str(text.as_str()).unwrap_or_else(|error| {
                panic!("case {case:?}: frame is not JSON ({error}): {text}")
            }),
        ),
        Ok(Some(Ok(Message::Binary(bytes)))) => Some(
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|error| panic!("case {case:?}: frame is not JSON ({error})")),
        ),
        Ok(Some(Ok(other))) => panic!("case {case:?}: unexpected frame {other:?}"),
        Ok(Some(Err(error))) => panic!("case {case:?}: the socket failed: {error}"),
        Ok(None) => panic!("case {case:?}: the socket closed instead of delivering a frame"),
        Err(_elapsed) => None,
    }
}

/// The string member `name` of `frame`.
fn member(frame: &Value, name: &str) -> Option<String> {
    frame.get(name).and_then(Value::as_str).map(str::to_string)
}

/// The session description a signaling payload carries, as a WebRTC client reads it: the
/// base64 `messagePayload` decoded to the JSEP envelope, and that envelope's `type` and
/// `sdp`.
fn description(case: &str, payload: &str) -> (String, String) {
    let bytes = aws_smithy_types::base64::decode(payload).unwrap_or_else(|error| {
        panic!("case {case:?}: messagePayload must be base64 ({error}): {payload:?}")
    });
    let envelope: Value = serde_json::from_slice(&bytes).unwrap_or_else(|error| {
        panic!(
            "case {case:?}: the payload must decode to a JSON description ({error}): {}",
            String::from_utf8_lossy(&bytes)
        )
    });
    let description_type = member(&envelope, "type")
        .unwrap_or_else(|| panic!("case {case:?}: the envelope states no type: {envelope}"));
    let sdp = member(&envelope, "sdp")
        .unwrap_or_else(|| panic!("case {case:?}: the envelope carries no sdp: {envelope}"));
    (description_type, sdp)
}

/// Every producer join pushes ONE offer of its own onto the connection the video producing
/// device holds to the channel, and the session records what it offered.
#[tokio::test]
async fn every_producer_join_pushes_its_own_sdp_offer_to_the_channels_master() {
    // (case, channelArn, successive joins). The ARNs span regions, account ids, name
    // spellings and creation times; the signaling registry names the `aws` partition
    // literally, so these rows stay in it.
    let rows: &[(&str, &str, u64)] = &[
        (
            "the recorded case: one join on one open connection",
            "arn:aws:kinesisvideo:us-east-1:123456789012:channel/demo-channel/1700000000000",
            1,
        ),
        (
            "two successive joins, which are not deduplicated",
            "arn:aws:kinesisvideo:eu-west-2:000000000000:channel/edge.cam_01-A/1",
            2,
        ),
        (
            "a one-character channel name, joined four times",
            "arn:aws:kinesisvideo:ap-southeast-2:210987654321:channel/c/9999999999999",
            4,
        ),
    ];

    let emulator = Emulator::with_signaling_plane();
    let announced = signaling::state::ensure_wss_endpoint(&*emulator.engine)
        .await
        .expect("the signaling data plane's socket must open");

    for (case, channel_arn, joins) in rows {
        register_channel(&emulator, channel_arn);
        let mut master = master_socket(case, &announced, channel_arn).await;

        let mut payloads: Vec<String> = Vec::new();
        for join in 1..=*joins {
            let answer = emulator.join_as_producer(channel_arn).await;
            assert!(
                answer.ok(),
                "case {case:?} join {join}: expected 200 from JoinStorageSession, got {answer:?}"
            );

            let frame = next_frame(case, &mut master).await.unwrap_or_else(|| {
                panic!(
                    "case {case:?} join {join}: the storage session pushed no signaling message \
                     within {}s of JoinStorageSession",
                    OFFER_DEADLINE.as_secs()
                )
            });
            assert_eq!(
                member(&frame, "messageType").as_deref(),
                Some(SDP_OFFER),
                "case {case:?} join {join}: expected an SDP_OFFER, got {frame}"
            );
            let payload = member(&frame, "messagePayload").unwrap_or_else(|| {
                panic!("case {case:?} join {join}: the offer carries no messagePayload: {frame}")
            });
            assert!(
                !payload.is_empty(),
                "case {case:?} join {join}: the offer's messagePayload must not be empty"
            );

            let (description_type, sdp) = description(case, &payload);
            assert_eq!(
                description_type, "offer",
                "case {case:?} join {join}: expected a description of type \"offer\", got \
                 {description_type:?}"
            );
            assert!(
                sdp.starts_with("v=0"),
                "case {case:?} join {join}: an RFC 4566 description opens with its version \
                 line, got {sdp:?}"
            );
            // Both tracks WebRTC ingestion requires, per this operation's own documented
            // requirements ("Video track: H.264 Audio track: Opus").
            let media: Vec<&str> = sdp
                .lines()
                .filter(|line| line.starts_with("m="))
                .collect();
            assert_eq!(
                media.len(),
                2,
                "case {case:?} join {join}: expected an audio and a video media line, got \
                 {media:?}"
            );
            for expected in ["m=audio", "m=video"] {
                assert!(
                    media.iter().any(|line| line.starts_with(expected)),
                    "case {case:?} join {join}: expected a {expected:?} line, got {media:?}"
                );
            }
            for codec in ["opus", "H264"] {
                assert!(
                    sdp.contains(codec),
                    "case {case:?} join {join}: the offer must name the {codec:?} codec, got \
                     {sdp:?}"
                );
            }

            assert!(
                !payloads.contains(&payload),
                "case {case:?} join {join}: each call must be offered its own description, got \
                 {payload:?} twice"
            );
            payloads.push(payload);
        }

        // What the device was delivered is what the session recorded for each of its
        // joins — the offers are the session's state, not a value made up on the way out.
        emulator.with_session(channel_arn, |session| {
            let session = session
                .unwrap_or_else(|| panic!("case {case:?}: the joins recorded no session"));
            let recorded: Vec<String> = session
                .offers
                .iter()
                .map(|offer| offer.message_payload.clone())
                .collect();
            assert_eq!(
                recorded, payloads,
                "case {case:?}: expected one recorded offer per call, in order, got {} records",
                recorded.len()
            );
            for join in 1..=*joins {
                let offer = session.offer_for(join).unwrap_or_else(|| {
                    panic!("case {case:?}: no offer was recorded for join {join}")
                });
                assert!(
                    offer.delivered,
                    "case {case:?} join {join}: the attached producing device must have been \
                     handed the offer, got {offer:?}"
                );
            }
        });
    }
}

/// A join with no producing device attached to receive the offer is still a join: the call
/// succeeds and the session records the offer it sent, whether or not the channel's
/// signaling plane holds a connection for it.
#[tokio::test]
async fn a_join_with_no_attached_device_still_records_the_offer_it_sent() {
    // (case, channelArn). Every row is a channel the control plane holds, which is what
    // makes the join a join; what they differ along is where the channel lives and how its
    // name is spelled.
    let rows: &[(&str, &str)] = &[
        (
            "a channel nobody is connected to",
            "arn:aws:kinesisvideo:us-east-1:123456789012:channel/registered-idle/1700000000000",
        ),
        (
            "another channel in the same region",
            "arn:aws:kinesisvideo:us-east-1:123456789012:channel/idle.two_B/1700000000001",
        ),
        (
            "a one-character name in another region",
            "arn:aws:kinesisvideo:cn-north-1:210987654321:channel/c/9999999999999",
        ),
    ];

    let emulator = Emulator::with_signaling_plane();
    for (case, channel_arn) in rows {
        register_channel(&emulator, channel_arn);
        let answer = emulator.join_as_producer(channel_arn).await;
        assert!(
            answer.ok(),
            "case {case:?}: expected 200 from JoinStorageSession, got {answer:?}"
        );
        emulator.with_session(channel_arn, |session| {
            let session =
                session.unwrap_or_else(|| panic!("case {case:?}: the join recorded no session"));
            let offer = session
                .offer_for(1)
                .unwrap_or_else(|| panic!("case {case:?}: the join recorded no offer"));
            assert!(
                !offer.delivered,
                "case {case:?}: no device was attached, so nothing can have been handed the \
                 offer, got {offer:?}"
            );
            let (description_type, sdp) = description(case, &offer.message_payload);
            assert_eq!(
                description_type, "offer",
                "case {case:?}: expected a description of type \"offer\", got {description_type:?}"
            );
            assert!(
                sdp.starts_with("v=0") && sdp.lines().any(|line| line.starts_with("m=")),
                "case {case:?}: the offer must be a well-formed session description, got {sdp:?}"
            );
        });
    }
}
