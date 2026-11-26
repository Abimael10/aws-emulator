// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the four ADDRESSES every stage this service reports announces,
//! and for the LIFETIME every participant token it mints states, driven over the wire
//! through `MultiServiceRouter::dispatch`.
//!
//! The rules under test, each stated once and then exercised over a SET of inputs:
//!
//! 1. Every address this emulator hands a caller is an address of THIS emulator's that
//!    answers the member's own protocol — never a host belonging to someone else, and never
//!    one nothing answers that protocol on. So the `wss` events channel and the `https` WHIP
//!    endpoint are absolute URLs on the address their caller reached this emulator at, while
//!    the `rtmp`/`rtmps` contribution endpoints name the socket this deployment answers RTMP
//!    and RTMPS on, at the application path both carry.
//! 2. Every projection of one stage agrees: the create, the read and the update all
//!    announce the same set for the same caller, and a read announces its own caller's
//!    address without changing what a later caller is told.
//! 3. The addresses are the DEPLOYMENT's, not a stage's: every stage reports the same four,
//!    carrying no part of a stage's own identity — which stage a connection joins is carried
//!    by the participant token the client presents on them, and by the ingest key an encoder
//!    authenticates its publish with.
//! 4. A minted `ParticipantToken` expires the EFFECTIVE lifetime it was issued for out —
//!    the `duration` the request sized it with, or the default the model documents on the
//!    reported member itself — on both operations that issue a token;
//!    `CreateParticipantToken` reports `duration` only where the request sized it, while
//!    `CreateStage` restates the effective lifetime of every token it attached.
//! 5. The two contribution endpoints are DIALABLE: a real encoder that completes an RTMP(S)
//!    handshake against the announced value and publishes with an ingest configuration's
//!    `streamKey` is ingested, which is what produces every fact a live stage contribution
//!    is read by (the configuration reporting `ACTIVE`, the stage's `activeSessionId` and
//!    its session, the publishing participant and its occurrences, and the refusal of a
//!    delete without `force`) — and the unwind of all of them when the publisher stops.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aws_smithy_types::date_time::{DateTime, Format};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use service_com_amazonaws_ivsrealtime_amazoninteractivevideoservicerealtime::{
    state, EmulatedService,
};

/// The lifetime a participant token gets when the request sizes none, documented on the
/// REPORTED member: `smithy_query(query="shape",shape="ParticipantToken")` — `duration`
/// "Duration (in minutes), after which the participant token expires. Default: 720 (12
/// hours)."
const DEFAULT_DURATION_MINUTES: i64 = 720;

/// The published bounds of that member, from the same call: `ParticipantTokenDurationMinutes`
/// `@range(min 1, max 20160)`.
const MIN_DURATION_MINUTES: i64 = 1;
const MAX_DURATION_MINUTES: i64 = 20160;

/// Hosts no address this emulator announces may name: a value on one of these leaves the
/// sandbox, because nothing here answers on an Amazon production host.
const FOREIGN_DOMAINS: [&str; 2] = ["live-video.net", "amazonaws.com"];

/// The application an RTMP or RTMPS contribution dials under the host — the path both of
/// those endpoints carry, which is the fixed part of their value.
const CONTRIBUTE_APPLICATION_PATH: &str = "/app/";

/// A SigV4 `Authorization` header in the credential scope `ivs` uses, which is how a
/// request names the service and the Region it is addressed to.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-east-1/ivs/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=00";

/// One emulator holding only this service, driven through the router that serves
/// production traffic, plus the engine handle the contribution plane's own record is read
/// through.
struct RealTime {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl RealTime {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { router, engine }
    }

    /// The same emulator, SERVED at `base` — the base URL `OperationContext::endpoint`
    /// reports, which is the data port's own URL for a router served on a port and
    /// `UNSERVED_URL_BASE` for one that is not (what [`Self::new`] leaves it as).
    ///
    /// A suite about the addresses a response announces needs both, because an address
    /// this emulator publishes is read off that base whenever the request itself addressed
    /// one of AWS's own hosts.
    fn served_at(base: &str) -> Self {
        let mut realtime = Self::new();
        realtime.router.set_url_base(
            smithy_server_runtime::EndpointUrl::parse(base)
                .expect("the test endpoint base is a bare https URL"),
        );
        realtime
    }

    /// Call `operation` as an SDK would, from a client that ADDRESSED `host`.
    ///
    /// `host` is `None` for a client that names no host of its own: the request URI then
    /// carries this service's published endpoint, which is what an SDK sends.
    async fn call_as(&self, operation: &str, body: &Value, host: Option<&str>) -> (u16, Value) {
        let mut builder = http::Request::builder()
            .method("POST")
            .uri(format!("https://ivs.us-east-1.amazonaws.com/{operation}"))
            .header("content-type", "application/json")
            .header("authorization", AUTHORIZATION);
        if let Some(host) = host {
            builder = builder.header("host", host);
        }
        let request = builder
            .body(bytes::Bytes::from(body.to_string()))
            .expect("the test request is well formed");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed: Value = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        (status, parsed)
    }

    /// [`Self::call_as`] from a client naming no host, asserting the call succeeded.
    async fn ok(&self, case: &str, operation: &str, body: &Value) -> Value {
        self.ok_as(case, operation, body, None).await
    }

    /// [`Self::call_as`], asserting the call succeeded.
    async fn ok_as(&self, case: &str, operation: &str, body: &Value, host: Option<&str>) -> Value {
        let (status, response) = self.call_as(operation, body, host).await;
        assert_eq!(
            status, 200,
            "case {case:?}: {operation} must answer HTTP 200, got {status} {response}"
        );
        response
    }

    /// The `host:port` of the SOCKET this deployment's contribution plane answers RTMP and
    /// RTMPS on, as the plane itself records it.
    ///
    /// It is also the value the control plane ANNOUNCES as every stage's `rtmp`/`rtmps`
    /// endpoints (rule 1 below), so a case that drives a real encoder over a real socket can
    /// check the announced value against where the plane really listens. `None` until a
    /// call that announces those endpoints has opened it.
    fn contribute_socket(&self) -> Option<String> {
        let store = state::service_state(&*self.engine)
            .expect("the IVS RealTime store is reachable");
        let store = store.lock();
        state::contribute_authority(&store).map(str::to_string)
    }

    /// That same address, asserting a call has opened the plane.
    fn contribute_authority(&self) -> String {
        self.contribute_socket()
            .expect("a created stage has opened this deployment's contribution plane")
    }
}

/// One addressing a client reaches this emulator through: the base the emulator is served
/// at, the `Host` the client sent, and the address a URL announced to it must name.
struct Addressing {
    case: &'static str,
    /// The base `OperationContext::endpoint` reports, or `None` for an emulator served on
    /// no port at all (the harness default).
    served_at: Option<&'static str>,
    /// The host the client addressed, or `None` for a client that names only this
    /// service's published endpoint on the request URI.
    host: Option<&'static str>,
    /// The `host[:port]` every URL announced to that client must be on.
    authority: &'static str,
}

/// The addressings this suite spans: rows differ along what rule 1 generalises over — how
/// the client addressed this emulator (an address with a port, a name with a port, an IPv6
/// literal, one of AWS's own hostnames, none at all) and whether this emulator is served
/// on a port.
fn addressings() -> Vec<Addressing> {
    vec![
        Addressing {
            case: "an SDK client naming this service's endpoint, on an unserved emulator",
            served_at: None,
            host: None,
            // `smithy_server_runtime::UNSERVED_URL_BASE`: the one base an emulator that is
            // served on no port reports, and still this emulator's own.
            authority: "emulator.invalid",
        },
        Addressing {
            case: "a client on the loopback address",
            served_at: None,
            host: Some("127.0.0.1:9297"),
            authority: "127.0.0.1:9297",
        },
        Addressing {
            case: "a client naming the loopback by name, on another port",
            served_at: None,
            host: Some("localhost:40305"),
            authority: "localhost:40305",
        },
        Addressing {
            case: "a client on an IPv6 literal",
            served_at: None,
            host: Some("[::1]:9297"),
            authority: "[::1]:9297",
        },
        Addressing {
            case: "a client that resolves AWS's own hosts, on an emulator served on a port",
            served_at: Some("https://127.0.0.1:40305"),
            host: Some("ivs.us-east-1.amazonaws.com"),
            authority: "127.0.0.1:40305",
        },
        Addressing {
            case: "an SDK client on an emulator served at a name",
            served_at: Some("https://emulator.test:8443"),
            host: None,
            authority: "emulator.test:8443",
        },
        Addressing {
            case: "a client on the loopback address of an emulator served elsewhere",
            served_at: Some("https://127.0.0.1:40305"),
            host: Some("localhost:9297"),
            authority: "localhost:9297",
        },
    ]
}

/// The creates this suite spans, so the rules are asserted over a set of stages rather
/// than one: rows differ along the name, the tags, the token configurations and the
/// recording configuration — none of which the announced addresses depend on.
fn create_requests() -> Vec<(&'static str, Value)> {
    vec![
        ("every member defaulted", json!({})),
        ("a name", json!({ "name": "announced" })),
        ("an empty name", json!({ "name": "" })),
        (
            "a name and tags",
            json!({ "name": "tagged", "tags": { "env": "prod" } }),
        ),
        (
            "participant tokens as well",
            json!({ "participantTokenConfigurations": [{ "userId": "alice" }] }),
        ),
        (
            "a recording configuration with no storage",
            json!({ "autoParticipantRecordingConfiguration": {
                "storageConfigurationArn": "", "mediaTypes": ["NONE"] } }),
        ),
    ]
}

/// The stage id a `StageArn` names (`arn:aws:ivs:<region>:<account>:stage/<id>`, the form
/// the modelled `StageArn` pattern pins).
fn stage_id_of(arn: &str) -> String {
    let segments: Vec<&str> = arn.splitn(6, ':').collect();
    assert_eq!(
        segments.len(),
        6,
        "a stage ARN has six colon-separated segments, got {arn:?}"
    );
    segments[5]
        .strip_prefix("stage/")
        .unwrap_or_else(|| panic!("a stage ARN names a stage resource, got {arn:?}"))
        .to_string()
}

/// The authority (`host[:port]`) of an absolute URL, and the path under it.
fn split_url(case: &str, member: &str, url: &str) -> (String, String) {
    let after_scheme = url
        .split_once("://")
        .unwrap_or_else(|| {
            panic!("case {case:?}: {member} must be an absolute URL, got {url:?}")
        })
        .1;
    match after_scheme.find('/') {
        Some(end) => (
            after_scheme[..end].to_string(),
            after_scheme[end..].to_string(),
        ),
        None => (after_scheme.to_string(), String::new()),
    }
}

/// Rule 1 on one announced endpoint: it carries its protocol's own scheme, its host is the
/// address the caller was answered on, it is no foreign host's, and the path under that
/// address is the one its protocol dials.
fn assert_announced(
    case: &str,
    member: &str,
    endpoints: &Value,
    authority: &str,
    scheme: &str,
    expected_path: &str,
) {
    let url = endpoints
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a {member} endpoint, got {endpoints}"));
    assert!(
        url.starts_with(&format!("{scheme}://")),
        "case {case:?}: {member} must keep its {scheme} scheme, got {url:?}"
    );
    let (host, path) = split_url(case, member, url);
    assert_eq!(
        host, authority,
        "case {case:?}: {member} must name the address this caller reached the emulator \
         at ({authority:?}), got host {host:?} in {url:?}"
    );
    for domain in FOREIGN_DOMAINS {
        assert!(
            !host.ends_with(domain),
            "case {case:?}: {member} must not send a client to a {domain} host, got {url:?}"
        );
    }
    assert_eq!(
        path, expected_path,
        "case {case:?}: {member} must dial {expected_path:?} under that address, got {url:?}"
    );
}

/// Rule 1 over every member of one `Stage$endpoints`: the events channel and the WHIP
/// endpoint at the caller's own address, and the two contribution endpoints at the
/// application path under the socket this deployment answers RTMP and RTMPS on.
fn assert_announced_endpoints(case: &str, stage: &Value, authority: &str, contribute: &str) {
    let arn = stage["arn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a stage arn, got {stage}"));
    // The ARN is read for its own sake: a stage that announces addresses is a stage with
    // an id, and rule 3 below asserts no address carries it.
    assert!(
        !stage_id_of(arn).is_empty(),
        "case {case:?}: expected the stage arn to carry a resource id, got {arn:?}"
    );
    let endpoints = &stage["endpoints"];
    for (member, scheme, path, host) in [
        ("events", "wss", "", authority),
        ("whip", "https", "", authority),
        ("rtmp", "rtmp", CONTRIBUTE_APPLICATION_PATH, contribute),
        ("rtmps", "rtmps", CONTRIBUTE_APPLICATION_PATH, contribute),
    ] {
        assert_announced(case, member, endpoints, host, scheme, path);
    }
}

/// Rules 1 and 2 over the addressing table: every operation that projects a stage
/// announces all four endpoints on the address its own caller reached this emulator at.
#[tokio::test]
async fn every_stage_projection_announces_its_endpoints_on_the_callers_own_address() {
    for addressing in addressings() {
        let Addressing {
            case,
            served_at,
            host,
            authority,
        } = addressing;
        let realtime = match served_at {
            Some(base) => RealTime::served_at(base),
            None => RealTime::new(),
        };

        for (row, request) in create_requests() {
            let case = format!("{case}: {row}");
            // Rule 1 on the response the model tells a client to cache at creation time.
            let created = realtime
                .ok_as(&case, "CreateStage", &request, host)
                .await;
            let stage = created["stage"].clone();
            let contribute = realtime.contribute_authority();
            assert_announced_endpoints(&case, &stage, authority, &contribute);
            let arn = stage["arn"].as_str().unwrap_or_default().to_string();

            // Rule 2: the read and the update of the same stage agree, for the same caller.
            let read = realtime
                .ok_as(&case, "GetStage", &json!({ "arn": arn }), host)
                .await;
            assert_announced_endpoints(&case, &read["stage"], authority, &contribute);
            assert_eq!(
                read["stage"]["endpoints"], stage["endpoints"],
                "case {case:?}: GetStage must announce what the create announced, got {read}"
            );
            let updated = realtime
                .ok_as(
                    &case,
                    "UpdateStage",
                    &json!({ "arn": arn, "name": "renamed" }),
                    host,
                )
                .await;
            assert_announced_endpoints(&case, &updated["stage"], authority, &contribute);
            assert_eq!(
                updated["stage"]["endpoints"], stage["endpoints"],
                "case {case:?}: UpdateStage must announce what the create announced, got \
                 {updated}"
            );
        }
    }
}

/// Rule 2's other half: a read announces its OWN caller's address and changes nothing, so
/// a second caller reached at another address is answered its own, and the first caller
/// still gets what it got before.
#[tokio::test]
async fn a_read_through_one_address_does_not_change_what_another_caller_is_told() {
    let realtime = RealTime::new();
    let created = realtime
        .ok_as(
            "shared stage",
            "CreateStage",
            &json!({ "name": "shared" }),
            Some("127.0.0.1:9297"),
        )
        .await;
    let arn = created["stage"]["arn"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    // Rows: the addresses three clients of one emulator reach it at, each read twice so
    // the second round proves a read through one address changed nothing for the others.
    let addresses = ["127.0.0.1:9297", "localhost:40305", "[::1]:8443"];
    let contribute = realtime.contribute_authority();
    for round in 1..=2 {
        for host in addresses {
            let case = format!("round {round} at {host}");
            let read = realtime
                .ok_as(&case, "GetStage", &json!({ "arn": arn }), Some(host))
                .await;
            assert_announced_endpoints(&case, &read["stage"], host, &contribute);
        }
    }
}

/// Rule 3: the four addresses are the DEPLOYMENT's — one set for every stage, carrying no
/// part of a stage's own identity, so a client that cached them can join the next stage
/// with the token it is issued for it.
#[tokio::test]
async fn the_announced_endpoints_are_the_deployments_and_not_a_stages() {
    let realtime = RealTime::new();
    let mut announced: BTreeSet<String> = BTreeSet::new();

    for (case, request) in create_requests() {
        let created = realtime.ok(case, "CreateStage", &request).await;
        let stage = &created["stage"];
        let arn = stage["arn"].as_str().unwrap_or_default();
        let stage_id = stage_id_of(arn);
        let endpoints = &stage["endpoints"];

        for member in ["events", "whip", "rtmp", "rtmps"] {
            let url = endpoints[member].as_str().unwrap_or_default();
            assert!(
                !url.contains(&stage_id) && !url.contains(arn),
                "case {case:?}: {member} must carry no part of the stage's own identity, \
                 got {url:?} for stage {stage_id:?}"
            );
        }
        announced.insert(endpoints.to_string());
    }

    assert_eq!(
        announced.len(),
        1,
        "every stage of this deployment must announce ONE set of addresses, got \
         {announced:?}"
    );
}

/// Rule 4: a minted token expires its effective lifetime out on both operations that issue
/// one, `CreateParticipantToken` reports `duration` only where the request sized it, and
/// `CreateStage` restates the effective lifetime.
///
/// The rows differ along what the rule generalises over: a request that sizes nothing
/// (where the documented default is the answer), both published bounds of the member, the
/// default stated explicitly, and a middling value.
#[tokio::test]
async fn every_minted_token_expires_its_effective_lifetime_out() {
    let realtime = RealTime::new();
    let stage_arn = realtime.ok("the token's stage", "CreateStage", &json!({})).await
        ["stage"]["arn"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    // (case, the duration the request sizes the token with, the duration it must report)
    let rows: Vec<(&str, Option<i64>, i64)> = vec![
        ("no duration at all", None, DEFAULT_DURATION_MINUTES),
        (
            "the modelled minimum",
            Some(MIN_DURATION_MINUTES),
            MIN_DURATION_MINUTES,
        ),
        (
            "the modelled maximum",
            Some(MAX_DURATION_MINUTES),
            MAX_DURATION_MINUTES,
        ),
        (
            "the documented default, asked for explicitly",
            Some(DEFAULT_DURATION_MINUTES),
            DEFAULT_DURATION_MINUTES,
        ),
        ("a middling duration", Some(90), 90),
    ];

    for (case, requested, expected) in &rows {
        let mut request = json!({ "stageArn": stage_arn });
        if let Some(duration) = requested {
            request["duration"] = json!(duration);
        }
        let issued_at = now_secs();
        let response = realtime
            .ok(case, "CreateParticipantToken", &request)
            .await;
        assert_effective_duration(
            case,
            "CreateParticipantToken",
            &response["participantToken"],
            // Echoed as asked: absent for a token the request did not size.
            *requested,
            *expected,
            issued_at,
        );
    }

    // The same rule through `CreateStage`, which issues one token per configuration: the
    // whole table in ONE request, so each token is matched to its own configuration.
    let configurations: Vec<Value> = rows
        .iter()
        .map(|(case, requested, _)| {
            let mut configuration = json!({ "userId": *case });
            if let Some(duration) = requested {
                configuration["duration"] = json!(duration);
            }
            configuration
        })
        .collect();
    let issued_at = now_secs();
    let created = realtime
        .ok(
            "one configuration per row",
            "CreateStage",
            &json!({ "participantTokenConfigurations": configurations }),
        )
        .await;
    let tokens = created["participantTokens"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        tokens.len(),
        rows.len(),
        "expected one token per configuration, got {created}"
    );
    for (index, (case, _, expected)) in rows.iter().enumerate() {
        assert_effective_duration(
            case,
            "CreateStage",
            &tokens[index],
            Some(*expected),
            *expected,
            issued_at,
        );
    }
}

/// Rule 4 on one reported token: `duration` is reported as this operation reports it, the
/// effective lifetime is inside the published range, and `expirationTime` is that many
/// minutes past the issue instant.
fn assert_effective_duration(
    case: &str,
    operation: &str,
    token: &Value,
    reported_duration: Option<i64>,
    expected: i64,
    issued_at: i64,
) {
    let reported = token.get("duration").and_then(Value::as_i64);
    assert_eq!(
        reported, reported_duration,
        "case {case:?} ({operation}): expected duration {reported_duration:?}, got {token}"
    );
    assert!(
        (MIN_DURATION_MINUTES..=MAX_DURATION_MINUTES).contains(&expected),
        "case {case:?} ({operation}): expected a duration inside the published range, \
         got {reported:?}"
    );
    let expiry = timestamp_secs(case, &token["expirationTime"]);
    let due = issued_at + expected * 60;
    assert!(
        (expiry - due).abs() <= 120,
        "case {case:?} ({operation}): expected an expirationTime {expected} minutes out \
         ({due}), got {expiry} in {token}"
    );
}

/// Seconds since the epoch, now.
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() as i64)
}

/// The epoch-second instant a modelled `date-time` member denotes. Only a STRING is
/// accepted: `ParticipantTokenExpirationTime` carries `@timestampFormat("date-time")`.
fn timestamp_secs(case: &str, value: &Value) -> i64 {
    let rendered = value.as_str().unwrap_or_else(|| {
        panic!("case {case:?}: expected an ISO-8601 date-time string, got {value}")
    });
    DateTime::from_str(rendered, Format::DateTime)
        .unwrap_or_else(|e| panic!("case {case:?}: {rendered:?} is not an ISO-8601 instant: {e}"))
        .secs()
}

// =============================================================================
// Rule 5 — the CONTRIBUTION plane: a real publish on the address a stage announces
// =============================================================================
//
// The announced `rtmp`/`rtmps` values are addresses THIS emulator answers RTMP and RTMPS
// on, and a publish authenticated by an ingest configuration's `streamKey` is ingested:
// the participant that key was minted for joins the configuration's stage as a publisher.
// That is what produces every fact a live IVS RealTime contribution is read by —
// `GetIngestConfiguration` reporting `ACTIVE` (and `ListIngestConfigurations`
// `filterByState` agreeing), the stage reporting an `activeSessionId` that
// `ListStageSessions` lists, `GetParticipant` reporting a `CONNECTED`, published
// participant with the protocol it publishes over, `ListParticipantEvents` carrying its
// `JOINED`/`PUBLISH_STARTED` occurrences, and `DeleteIngestConfiguration` without `force`
// refusing while the publisher is live — and the unwind of all of them once it stops. A
// publish presenting a key no ingest configuration minted is refused on the wire and makes
// nothing active.
//
// Each case drives a REAL publisher over a REAL socket, dialing exactly the value the
// create announced, with the same handshake, commands and media messages an encoder sends.

/// The two values the modelled `IngestConfigurationState` carries, from
/// `smithy_query(query="shape",shape="IngestConfigurationState")`.
const PUBLISHING_STATE: &str = "ACTIVE";
const NO_PUBLISHER_STATE: &str = "INACTIVE";

/// The two values the modelled `ParticipantState` carries, from
/// `smithy_query(query="shape",shape="ParticipantState")`.
const PARTICIPANT_CONNECTED: &str = "CONNECTED";
const PARTICIPANT_DISCONNECTED: &str = "DISCONNECTED";

/// The occurrence names a joining publisher and a departing one record, four of the twelve
/// the modelled `EventName` carries (`smithy_query(query="shape",shape="EventName")`).
const EVENT_JOINED: &str = "JOINED";
const EVENT_PUBLISH_STARTED: &str = "PUBLISH_STARTED";
const EVENT_PUBLISH_STOPPED: &str = "PUBLISH_STOPPED";
const EVENT_LEFT: &str = "LEFT";

/// The application an encoder dials under the contribution host, which is the path both
/// announced contribution endpoints carry.
const CONTRIBUTE_APPLICATION: &str = "app";

/// How long a wait on a CONDITION may take before a case gives up.
///
/// Generous on purpose: it bounds the emulator having served a step, not the time any step
/// is expected to take, so a loaded host cannot fail a case that a fast one passes.
const CONDITION_DEADLINE: Duration = Duration::from_secs(30);

/// How often a waiting case looks again.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// How long one read of the publisher's socket may block before the case gives up on it,
/// for the same reason.
const READ_DEADLINE: Duration = Duration::from_secs(30);

/// The media an encoder sends, as the FLV packaging an RTMP publisher carries it in: the
/// frame type and codec in the first byte, the packet type in the second.
const VIDEO_CONFIGURATION: &[u8] = &[0x17, 0x00, 0x00, 0x00, 0x00, 0x01, 0x64, 0x00, 0x2a];
/// A video keyframe.
const VIDEO_KEYFRAME: &[u8] = &[0x17, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05, 0x65];
/// The audio decoder configuration an AAC publisher sends.
const AUDIO_CONFIGURATION: &[u8] = &[0xaf, 0x00, 0x12, 0x10];
/// One audio frame.
const AUDIO_FRAME: &[u8] = &[0xaf, 0x01, 0x21, 0x10, 0x04, 0x60];

/// What a dialled contribution connection is: a plaintext socket, or the TLS stream over
/// one — the two wires this plane serves on the one address it announces.
trait Dialled: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> Dialled for T {}

/// The client side of the TLS an `rtmps://` publisher opens: the certificate is the one the
/// core generated for its own listeners — self-signed, and this client is the emulator's own
/// test dialling a port this very process opened — so there is no authority to check it
/// against. An encoder dialling `rtmps://` out of this process is in the same position.
#[derive(Debug)]
struct AcceptTheEmulatorsCertificate(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for AcceptTheEmulatorsCertificate {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// The port one `host:port` authority names, read through the workspace's one reading of an
/// authority.
fn authority_port(authority: &str) -> u16 {
    ::service_common::data_plane_addressing::addressed_port(Some(authority))
        .and_then(|port| u16::try_from(port).ok())
        .unwrap_or_else(|| panic!("a dialable authority carries a port, got {authority:?}"))
}

/// Open a connection to the announced contribution `authority`, encrypted when the
/// publisher dialled `rtmps://` and in the clear when it dialled `rtmp://`.
///
/// The port is the one the announced value itself carries — all a publisher outside this
/// process holds — and the host it names is this machine's own loopback interface, which is
/// where the core binds the plane's socket.
async fn dial_contribute(authority: &str, encrypted: bool) -> Result<Box<dyn Dialled>, String> {
    let port = authority_port(authority);
    let socket = tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
        .await
        .map_err(|error| {
            format!("the contribution socket {authority} must accept a connection: {error}")
        })?;
    if !encrypted {
        return Ok(Box::new(socket));
    }
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_safe_default_protocol_versions()
        .map_err(|error| format!("the provider supports the default protocol versions: {error}"))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptTheEmulatorsCertificate(provider)))
        .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    let server_name =
        rustls::pki_types::ServerName::try_from("localhost").expect("localhost is a server name");
    let encrypted = connector
        .connect(server_name, socket)
        .await
        .map_err(|error| {
            format!("an rtmps publisher's TLS handshake to {authority} must complete: {error}")
        })?;
    Ok(Box::new(encrypted))
}

/// One encoder publishing to the address a stage announced, over a real socket.
struct Publisher {
    /// The connection to the announced contribution address, encrypted or in the clear.
    conn: Box<dyn Dialled>,
    /// The client half of the RTMP session, which is what writes the publisher's bytes.
    session: rml_rtmp::sessions::ClientSession,
    /// Every byte the ingest server has sent on this connection. A refused publish is
    /// answered with an RTMP status the client session carries no event for, so the status
    /// is read off the wire itself ([`stated_refusal`]).
    seen: Vec<u8>,
}

impl Publisher {
    /// Dial the announced `authority` and publish `stream_key`, encrypted or in the clear,
    /// the way an encoder configured with a stage's contribution endpoint and an ingest
    /// configuration's key does — or `Err` carrying what the ingest server said when it
    /// refused the publish (or when the connection ended without one).
    async fn publishing(
        authority: &str,
        stream_key: &str,
        encrypted: bool,
    ) -> Result<Self, String> {
        use rml_rtmp::sessions::{ClientSession, ClientSessionConfig, PublishRequestType};

        let mut conn = dial_contribute(authority, encrypted).await?;
        let leftover = client_handshake(&mut conn).await?;
        let (session, initial) = ClientSession::new(ClientSessionConfig::new())
            .map_err(|error| format!("the client session must start: {error:?}"))?;
        let mut publisher = Self {
            conn,
            session,
            seen: Vec::new(),
        };
        publisher.carry_out(initial).await?;
        if !leftover.is_empty() {
            let results = publisher.session.handle_input(&leftover).map_err(|error| {
                format!("the session must read what followed the handshake: {error:?}")
            })?;
            publisher.carry_out(results).await?;
        }
        let packet = publisher
            .session
            .request_connection(CONTRIBUTE_APPLICATION.to_string())
            .map_err(|error| format!("the connect command must be built: {error:?}"))?;
        publisher.send(packet).await?;
        publisher.wait_for_connection().await?;
        let packet = publisher
            .session
            .request_publishing(stream_key.to_string(), PublishRequestType::Live)
            .map_err(|error| format!("the publish command must be built: {error:?}"))?;
        publisher.send(packet).await?;
        publisher.wait_for_publish().await?;
        Ok(publisher)
    }

    /// Send the decoder configuration of each wire and a second of media, which is what an
    /// encoder does once its publish is accepted.
    async fn send_media(&mut self) -> Result<(), String> {
        self.send_video(VIDEO_CONFIGURATION, 0).await?;
        self.send_audio(AUDIO_CONFIGURATION, 0).await?;
        self.send_video(VIDEO_KEYFRAME, 1_000).await?;
        self.send_audio(AUDIO_FRAME, 1_250).await
    }

    /// One video message at `timestamp` milliseconds.
    async fn send_video(&mut self, payload: &[u8], timestamp: u32) -> Result<(), String> {
        let packet = self
            .session
            .publish_video_data(
                bytes::Bytes::from(payload.to_vec()),
                rml_rtmp::time::RtmpTimestamp::new(timestamp),
                false,
            )
            .map_err(|error| format!("the video message must be built: {error:?}"))?;
        self.send(packet).await
    }

    /// One audio message at `timestamp` milliseconds.
    async fn send_audio(&mut self, payload: &[u8], timestamp: u32) -> Result<(), String> {
        let packet = self
            .session
            .publish_audio_data(
                bytes::Bytes::from(payload.to_vec()),
                rml_rtmp::time::RtmpTimestamp::new(timestamp),
                false,
            )
            .map_err(|error| format!("the audio message must be built: {error:?}"))?;
        self.send(packet).await
    }

    /// Write one packet the session produced.
    async fn send(&mut self, packet: rml_rtmp::sessions::ClientSessionResult) -> Result<(), String> {
        self.carry_out(vec![packet]).await
    }

    /// Write every response the session produced.
    async fn carry_out(
        &mut self,
        results: Vec<rml_rtmp::sessions::ClientSessionResult>,
    ) -> Result<(), String> {
        for result in results {
            if let rml_rtmp::sessions::ClientSessionResult::OutboundResponse(packet) = result {
                let bytes = packet.bytes.clone();
                write_all(&mut self.conn, &bytes).await?;
            }
        }
        Ok(())
    }

    /// Read until the ingest server has accepted the connection, or carried back its own
    /// refusal of it.
    async fn wait_for_connection(&mut self) -> Result<(), String> {
        self.wait_for(|event| match event {
            rml_rtmp::sessions::ClientSessionEvent::ConnectionRequestAccepted => Some(Ok(())),
            rml_rtmp::sessions::ClientSessionEvent::ConnectionRequestRejected { description } => {
                Some(Err(format!(
                    "the ingest server refused the connection: {description}"
                )))
            }
            _ => None,
        })
        .await
    }

    /// Read until the ingest server has accepted the publish, or TOLD this publisher why it
    /// will not take it — which is what a refusal is on this wire.
    async fn wait_for_publish(&mut self) -> Result<(), String> {
        self.wait_for(|event| match event {
            rml_rtmp::sessions::ClientSessionEvent::PublishRequestAccepted => Some(Ok(())),
            _ => None,
        })
        .await
    }

    /// Read the connection until an event `read` answers for arrives, answering every
    /// response the session produces on the way; `Err` when that event is a refusal, when
    /// the server stated a refusal and closed, or when nothing arrived within the deadline.
    async fn wait_for(
        &mut self,
        read: impl Fn(&rml_rtmp::sessions::ClientSessionEvent) -> Option<Result<(), String>>,
    ) -> Result<(), String> {
        let deadline = Instant::now() + CONDITION_DEADLINE;
        while Instant::now() < deadline {
            let arriving = match read_some(&mut self.conn).await {
                Ok(arriving) => arriving,
                // The connection ended. When the server STATED a refusal before closing,
                // that status is the answer this publisher got.
                Err(ended) => {
                    return Err(stated_refusal(&self.seen).unwrap_or(ended));
                }
            };
            self.seen.extend_from_slice(&arriving);
            let results = self
                .session
                .handle_input(&arriving)
                .map_err(|error| format!("the ingest server's answer must be read: {error:?}"))?;
            let mut answer = None;
            for result in results {
                match result {
                    rml_rtmp::sessions::ClientSessionResult::OutboundResponse(packet) => {
                        let bytes = packet.bytes.clone();
                        write_all(&mut self.conn, &bytes).await?;
                    }
                    rml_rtmp::sessions::ClientSessionResult::RaisedEvent(event) => {
                        if let Some(read) = read(&event) {
                            answer = Some(read);
                        }
                    }
                    rml_rtmp::sessions::ClientSessionResult::UnhandleableMessageReceived(_) => {}
                }
            }
            if let Some(answer) = answer {
                return answer;
            }
            // A refusal the session raises no event for, stated while the connection is
            // still open.
            if let Some(stated) = stated_refusal(&self.seen) {
                return Err(stated);
            }
        }
        Err("the ingest server did not answer within the deadline".to_string())
    }

    /// Whether the ingest server has CLOSED this publisher's connection — a read that
    /// answers end of file, which is what a disconnected ingest session is on the wire.
    async fn severed(&mut self) -> bool {
        for _read in 0..8 {
            let mut buffer = [0_u8; 4096];
            match tokio::time::timeout(READ_DEADLINE, self.conn.read(&mut buffer)).await {
                Ok(Ok(0)) | Ok(Err(_)) => return true,
                Ok(Ok(_)) => continue,
                Err(_deadline) => return false,
            }
        }
        false
    }
}

/// How a refusal the ingest server STATED is reported back by the publisher above, ahead of
/// the status that server sent: the status is the refusal, so a case can tell it apart from
/// a connection that merely ended.
const REFUSED_BY_THE_SERVER: &str = "the ingest server refused the publish: ";

/// The RTMP status an ingest server refuses a publish with when the stream name presented
/// is not one it may publish, which is what an encoder reports to its operator.
const PUBLISH_REFUSED_STATUS: &str = "NetStream.Publish.BadName";

/// The refusal the ingest server stated in `seen`, when it stated one: the status string it
/// sent, read out of the bytes themselves because the client session raises no event for a
/// rejected publish. `None` when no refusal was sent.
fn stated_refusal(seen: &[u8]) -> Option<String> {
    carries(seen, PUBLISH_REFUSED_STATUS.as_bytes())
        .then(|| format!("{REFUSED_BY_THE_SERVER}{PUBLISH_REFUSED_STATUS}"))
}

/// Whether `haystack` carries `needle`.
fn carries(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
}

/// Read some bytes from `conn`, bounded by a generous deadline, or `Err` when the server
/// closed the connection — which is how a refused publish ends.
async fn read_some(conn: &mut Box<dyn Dialled>) -> Result<Vec<u8>, String> {
    let mut buffer = [0_u8; 4096];
    let read = tokio::time::timeout(READ_DEADLINE, conn.read(&mut buffer))
        .await
        .map_err(|_| "the ingest server did not answer within the deadline".to_string())?
        .map_err(|error| format!("the ingest connection must be readable: {error}"))?;
    match read {
        0 => Err("the ingest server closed the connection".to_string()),
        read => Ok(buffer[..read].to_vec()),
    }
}

/// Write `bytes` to `conn`.
async fn write_all(conn: &mut Box<dyn Dialled>, bytes: &[u8]) -> Result<(), String> {
    if bytes.is_empty() {
        return Ok(());
    }
    conn.write_all(bytes)
        .await
        .map_err(|error| format!("the ingest connection must be writable: {error}"))
}

/// Run the CLIENT half of the RTMP handshake on `conn`, answering the bytes that arrived
/// after it — which belong to the session.
async fn client_handshake(conn: &mut Box<dyn Dialled>) -> Result<Vec<u8>, String> {
    use rml_rtmp::handshake::{Handshake, HandshakeProcessResult, PeerType};

    let mut handshake = Handshake::new(PeerType::Client);
    let opening = handshake
        .generate_outbound_p0_and_p1()
        .map_err(|error| format!("the handshake must open: {error:?}"))?;
    write_all(conn, &opening).await?;
    loop {
        let arriving = read_some(conn).await?;
        match handshake.process_bytes(&arriving) {
            Ok(HandshakeProcessResult::InProgress { response_bytes }) => {
                write_all(conn, &response_bytes).await?;
            }
            Ok(HandshakeProcessResult::Completed {
                response_bytes,
                remaining_bytes,
            }) => {
                write_all(conn, &response_bytes).await?;
                return Ok(remaining_bytes);
            }
            Err(error) => return Err(format!("the handshake must complete: {error:?}")),
        }
    }
}

/// Look again until `condition` holds, or fail the case once a generous deadline has
/// passed: what a caller watching a contribution waits on is the CONDITION, never a
/// duration.
async fn poll_until<C, F>(case: &str, waiting_for: &str, mut condition: C)
where
    C: FnMut() -> F,
    F: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + CONDITION_DEADLINE;
    loop {
        if condition().await {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {waiting_for} before the deadline"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// A string member of a JSON object, asserted to be there.
fn member(case: &str, value: &Value, name: &str) -> String {
    value
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a {name} member, got {value}"))
        .to_string()
}

/// One contribution a case drives: which ingest configuration it publishes with, which of
/// that configuration's ingest keys it presents, which wire it dials, and how the encoder
/// spells the key it was configured with.
struct Contribution {
    case: &'static str,
    /// The `CreateIngestConfiguration` request, so the rows span the configurations a
    /// publisher can hold.
    create: Value,
    /// Whether the encoder dials the ENCRYPTED half of the announced address, which is what
    /// an `rtmps://` publisher does.
    encrypted: bool,
    /// Whether the key presented is one of the configuration's REDUNDANT ingest
    /// credentials rather than its own.
    redundant_credential: bool,
    /// Whether the encoder presents the whole path after the host (`app/<key>`) rather than
    /// the key alone: both are what real encoders send.
    key_as_path: bool,
    /// The `ParticipantProtocol` the participant must then report — the wire this
    /// contribution was published over.
    protocol: &'static str,
}

/// The contributions this suite spans: rows differ along what rule 5 generalises over — the
/// wire the encoder dials, the ingest protocol and `insecureIngest` pairing of the
/// configuration, whether the key is the configuration's own or a redundant one, whether the
/// configuration names a user id and attributes, and how the encoder spells the key.
fn contributions() -> Vec<Contribution> {
    vec![
        Contribution {
            case: "an RTMPS configuration published over the encrypted wire",
            create: json!({ "ingestProtocol": "RTMPS" }),
            encrypted: true,
            redundant_credential: false,
            key_as_path: false,
            protocol: "RTMPS",
        },
        Contribution {
            case: "a cleartext RTMP configuration published in the clear",
            create: json!({ "ingestProtocol": "RTMP", "insecureIngest": true, "name": "legacy" }),
            encrypted: false,
            redundant_credential: false,
            key_as_path: false,
            protocol: "RTMP",
        },
        Contribution {
            case: "a redundant ingest credential, presented as the whole path",
            create: json!({ "ingestProtocol": "RTMPS", "redundantIngest": true }),
            encrypted: false,
            redundant_credential: true,
            key_as_path: true,
            protocol: "RTMP",
        },
        Contribution {
            case: "a named publisher carrying attributes, over the encrypted wire",
            create: json!({
                "ingestProtocol": "RTMPS",
                "userId": "studio-7",
                "attributes": { "role": "host" },
            }),
            encrypted: true,
            redundant_credential: false,
            key_as_path: true,
            protocol: "RTMPS",
        },
    ]
}

impl RealTime {
    /// Create a stage, answering the announced contribution address and the stage's ARN —
    /// both read off the create's own response, which is everything a broadcaster holds.
    async fn stage_to_contribute_to(&self, case: &str) -> (String, String) {
        let created = self.ok(case, "CreateStage", &json!({ "name": "contributed" })).await;
        let stage = &created["stage"];
        let announced = member(case, &stage["endpoints"], "rtmp");
        let arn = member(case, stage, "arn");
        // The announced value IS the socket the plane answers on: `host:port` plus the
        // application path an encoder appends its key to.
        let authority = self.contribute_authority();
        assert_eq!(
            announced,
            format!("rtmp://{authority}{CONTRIBUTE_APPLICATION_PATH}"),
            "case {case:?}: the announced rtmp endpoint must be the address this \
             deployment's contribution plane answers on, got {announced:?}"
        );
        (arn, authority)
    }

    /// Create the ingest configuration of `contribution` on `stage_arn`, answering the
    /// reported configuration.
    async fn configuration_for(
        &self,
        contribution: &Contribution,
        stage_arn: &str,
    ) -> Value {
        let mut request = contribution.create.clone();
        request["stageArn"] = json!(stage_arn);
        self.ok(contribution.case, "CreateIngestConfiguration", &request)
            .await["ingestConfiguration"]
            .clone()
    }

    /// The state `GetIngestConfiguration` reports for `arn` right now.
    async fn configuration_state(&self, case: &str, arn: &str) -> String {
        let read = self
            .ok(case, "GetIngestConfiguration", &json!({ "arn": arn }))
            .await;
        member(case, &read["ingestConfiguration"], "state")
    }

    /// Whether `ListIngestConfigurations(filterByState = state)` lists `arn`.
    async fn listed_with_state(&self, case: &str, arn: &str, state: &str) -> bool {
        let listed = self
            .ok(
                case,
                "ListIngestConfigurations",
                &json!({ "filterByState": state }),
            )
            .await;
        listed["ingestConfigurations"]
            .as_array()
            .map(|summaries| {
                summaries
                    .iter()
                    .any(|summary| summary["arn"].as_str() == Some(arn))
            })
            .unwrap_or(false)
    }

    /// The stage's `activeSessionId`, read back over the wire.
    async fn active_session_id(&self, case: &str, stage_arn: &str) -> String {
        let read = self.ok(case, "GetStage", &json!({ "arn": stage_arn })).await;
        member(case, &read["stage"], "activeSessionId")
    }

    /// The occurrence names `ListParticipantEvents` reports for one participant, in order.
    async fn participant_events(
        &self,
        case: &str,
        stage_arn: &str,
        session_id: &str,
        participant_id: &str,
    ) -> Vec<String> {
        let listed = self
            .ok(
                case,
                "ListParticipantEvents",
                &json!({
                    "stageArn": stage_arn,
                    "sessionId": session_id,
                    "participantId": participant_id,
                }),
            )
            .await;
        listed["events"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|event| event["name"].as_str().map(str::to_string))
            .collect()
    }
}

/// The ingest key `contribution` presents, as the encoder was configured with it: the
/// configuration's own key or one of its redundant credentials', spelled as the key alone or
/// as the whole path after the host.
fn presented_key(contribution: &Contribution, configuration: &Value) -> (String, String) {
    let case = contribution.case;
    let (key, participant_id) = if contribution.redundant_credential {
        let credentials = configuration["redundantIngestCredentials"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let credential = credentials.first().unwrap_or_else(|| {
            panic!("case {case:?}: a redundant configuration reports its credentials, got {configuration}")
        });
        (
            member(case, credential, "streamKey"),
            member(case, credential, "participantId"),
        )
    } else {
        (
            member(case, configuration, "streamKey"),
            member(case, configuration, "participantId"),
        )
    };
    let presented = if contribution.key_as_path {
        format!("{CONTRIBUTE_APPLICATION}/{key}")
    } else {
        key
    };
    (presented, participant_id)
}

/// Rule 5, first leg: a publish on the announced contribution address, authenticated by an
/// ingest configuration's ingest key, is ingested — and every read of a live contribution
/// reports it.
#[tokio::test]
async fn a_publish_on_the_announced_address_is_ingested_and_read_back() {
    for contribution in contributions() {
        let case = contribution.case;
        let realtime = RealTime::new();
        let (stage_arn, authority) = realtime.stage_to_contribute_to(case).await;
        let configuration = realtime.configuration_for(&contribution, &stage_arn).await;
        let configuration_arn = member(case, &configuration, "arn");
        let (key, participant_id) = presented_key(&contribution, &configuration);

        // Nothing is publishing yet, so the configuration is INACTIVE and the stage has no
        // session at all.
        assert_eq!(
            realtime.configuration_state(case, &configuration_arn).await,
            NO_PUBLISHER_STATE,
            "case {case:?}: a configuration nobody publishes with must be {NO_PUBLISHER_STATE}"
        );
        assert_eq!(
            realtime.active_session_id(case, &stage_arn).await,
            "",
            "case {case:?}: a stage nobody has joined must report no active session"
        );

        // The publisher is kept alive for the whole case: a configuration is ACTIVE exactly
        // while something is publishing with it.
        let mut publisher = Publisher::publishing(&authority, &key, contribution.encrypted)
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: {error}"));
        publisher
            .send_media()
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: {error}"));

        poll_until(case, "the ingest configuration to report ACTIVE", || {
            let realtime = &realtime;
            let arn = configuration_arn.clone();
            async move { realtime.configuration_state(case, &arn).await == PUBLISHING_STATE }
        })
        .await;

        // The listing agrees with the read, in both directions of the filter.
        assert!(
            realtime
                .listed_with_state(case, &configuration_arn, PUBLISHING_STATE)
                .await,
            "case {case:?}: ListIngestConfigurations(filterByState=ACTIVE) must list the \
             live configuration"
        );
        assert!(
            !realtime
                .listed_with_state(case, &configuration_arn, NO_PUBLISHER_STATE)
                .await,
            "case {case:?}: ListIngestConfigurations(filterByState=INACTIVE) must not list \
             the live configuration"
        );

        // The stage now has a session, in the form the modelled `StageSessionId` pins
        // (`@length 16..16`, `^st-[a-zA-Z0-9]+$`), and `ListStageSessions` lists it.
        let session_id = realtime.active_session_id(case, &stage_arn).await;
        assert_eq!(
            session_id.len(),
            16,
            "case {case:?}: expected a 16-character session id, got {session_id:?}"
        );
        assert!(
            session_id.starts_with("st-")
                && session_id
                    .trim_start_matches("st-")
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric()),
            "case {case:?}: expected a session id of the modelled form, got {session_id:?}"
        );
        let sessions = realtime
            .ok(case, "ListStageSessions", &json!({ "stageArn": stage_arn }))
            .await;
        assert!(
            sessions["stageSessions"]
                .as_array()
                .is_some_and(|listed| listed
                    .iter()
                    .any(|session| session["sessionId"].as_str() == Some(session_id.as_str())
                        && session.get("endTime").is_none())),
            "case {case:?}: ListStageSessions must list the running session {session_id:?}, \
             got {sessions}"
        );

        // The publisher is a participant of that session: connected, publishing, over the
        // wire it dialled, with the configuration it broadcasts with.
        let read = realtime
            .ok(
                case,
                "GetParticipant",
                &json!({
                    "stageArn": stage_arn,
                    "sessionId": session_id,
                    "participantId": participant_id,
                }),
            )
            .await;
        let participant = &read["participant"];
        assert_eq!(
            member(case, participant, "state"),
            PARTICIPANT_CONNECTED,
            "case {case:?}: expected a CONNECTED participant, got {participant}"
        );
        assert_eq!(
            participant["published"].as_bool(),
            Some(true),
            "case {case:?}: expected a published participant, got {participant}"
        );
        assert_eq!(
            member(case, participant, "protocol"),
            contribution.protocol,
            "case {case:?}: expected the protocol the publisher dialled, got {participant}"
        );
        assert_eq!(
            member(case, participant, "ingestConfigurationArn"),
            configuration_arn,
            "case {case:?}: expected the participant's ingest configuration, got {participant}"
        );

        // And its occurrences are the join and the start of its media.
        let events = realtime
            .participant_events(case, &stage_arn, &session_id, &participant_id)
            .await;
        assert_eq!(
            events,
            vec![EVENT_JOINED.to_string(), EVENT_PUBLISH_STARTED.to_string()],
            "case {case:?}: expected the join and the publish occurrence, got {events:?}"
        );
    }
}

/// Rule 5, second leg: the contribution UNWINDS when the publisher stops — the participant
/// is disconnected with its occurrences recorded, the session ends, the stage reports no
/// active session, and the configuration is INACTIVE again.
#[tokio::test]
async fn the_contribution_unwinds_when_the_publisher_stops() {
    for contribution in contributions() {
        let case = contribution.case;
        let realtime = RealTime::new();
        let (stage_arn, authority) = realtime.stage_to_contribute_to(case).await;
        let configuration = realtime.configuration_for(&contribution, &stage_arn).await;
        let configuration_arn = member(case, &configuration, "arn");
        let (key, participant_id) = presented_key(&contribution, &configuration);

        let publisher = Publisher::publishing(&authority, &key, contribution.encrypted)
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: {error}"));
        poll_until(case, "the ingest configuration to report ACTIVE", || {
            let realtime = &realtime;
            let arn = configuration_arn.clone();
            async move { realtime.configuration_state(case, &arn).await == PUBLISHING_STATE }
        })
        .await;
        let session_id = realtime.active_session_id(case, &stage_arn).await;

        // The encoder goes away, which is all a publisher that stops does.
        drop(publisher);

        poll_until(case, "the ingest configuration to report INACTIVE again", || {
            let realtime = &realtime;
            let arn = configuration_arn.clone();
            async move { realtime.configuration_state(case, &arn).await == NO_PUBLISHER_STATE }
        })
        .await;
        assert_eq!(
            realtime.active_session_id(case, &stage_arn).await,
            "",
            "case {case:?}: a stage whose last participant left reports no active session"
        );
        let read = realtime
            .ok(
                case,
                "GetParticipant",
                &json!({
                    "stageArn": stage_arn,
                    "sessionId": session_id,
                    "participantId": participant_id,
                }),
            )
            .await;
        let participant = &read["participant"];
        assert_eq!(
            member(case, participant, "state"),
            PARTICIPANT_DISCONNECTED,
            "case {case:?}: expected a DISCONNECTED participant, got {participant}"
        );
        assert_eq!(
            participant["published"].as_bool(),
            Some(true),
            "case {case:?}: a participant that ever published keeps saying so, got {participant}"
        );
        let events = realtime
            .participant_events(case, &stage_arn, &session_id, &participant_id)
            .await;
        assert_eq!(
            events,
            vec![
                EVENT_JOINED.to_string(),
                EVENT_PUBLISH_STARTED.to_string(),
                EVENT_PUBLISH_STOPPED.to_string(),
                EVENT_LEFT.to_string(),
            ],
            "case {case:?}: expected the whole occurrence run of one contribution, got \
             {events:?}"
        );
        let sessions = realtime
            .ok(case, "ListStageSessions", &json!({ "stageArn": stage_arn }))
            .await;
        assert!(
            sessions["stageSessions"]
                .as_array()
                .is_some_and(|listed| listed.iter().any(|session| {
                    session["sessionId"].as_str() == Some(session_id.as_str())
                        && session.get("endTime").is_some()
                })),
            "case {case:?}: the ended session must report its endTime, got {sessions}"
        );
    }
}

/// Rule 5, third leg: while a publisher is live, a delete without `force` is refused and
/// changes nothing; with `force` the publisher is disconnected and the configuration goes.
#[tokio::test]
async fn a_live_publisher_refuses_a_delete_without_force_and_is_disconnected_by_one_with_it() {
    for contribution in contributions() {
        let case = contribution.case;
        let realtime = RealTime::new();
        let (stage_arn, authority) = realtime.stage_to_contribute_to(case).await;
        let configuration = realtime.configuration_for(&contribution, &stage_arn).await;
        let configuration_arn = member(case, &configuration, "arn");
        let (key, participant_id) = presented_key(&contribution, &configuration);

        let mut publisher = Publisher::publishing(&authority, &key, contribution.encrypted)
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: {error}"));
        publisher
            .send_media()
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: {error}"));
        poll_until(case, "the ingest configuration to report ACTIVE", || {
            let realtime = &realtime;
            let arn = configuration_arn.clone();
            async move { realtime.configuration_state(case, &arn).await == PUBLISHING_STATE }
        })
        .await;
        let session_id = realtime.active_session_id(case, &stage_arn).await;

        // "An IngestConfiguration cannot be deleted if the publisher is actively streaming
        // to a stage, unless force is set to true."
        let (status, response) = realtime
            .call_as(
                "DeleteIngestConfiguration",
                &json!({ "arn": configuration_arn }),
                None,
            )
            .await;
        assert_eq!(
            status, 409,
            "case {case:?}: expected HTTP 409 for a delete without force, got {status} \
             {response}"
        );
        assert_eq!(
            response["__type"].as_str().unwrap_or_default(),
            "ConflictException",
            "case {case:?}: expected the modelled ConflictException, got {response}"
        );
        assert_eq!(
            response["message"].as_str().unwrap_or_default(),
            format!(
                "Unable to mutate resource {configuration_arn} while participant is actively \
                 publishing"
            ),
            "case {case:?}: unexpected refusal wording, got {response}"
        );
        assert_eq!(
            realtime.configuration_state(case, &configuration_arn).await,
            PUBLISHING_STATE,
            "case {case:?}: a refused delete must leave the configuration ACTIVE"
        );

        // With `force`, "the participant is disconnected from the stage, followed by
        // deletion of the IngestConfiguration".
        let (status, response) = realtime
            .call_as(
                "DeleteIngestConfiguration",
                &json!({ "arn": configuration_arn, "force": true }),
                None,
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected a forced delete to be accepted, got {status} {response}"
        );
        let (status, response) = realtime
            .call_as(
                "GetIngestConfiguration",
                &json!({ "arn": configuration_arn }),
                None,
            )
            .await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected the deleted configuration to be not found, got \
             {status} {response}"
        );
        assert_eq!(
            response["__type"].as_str().unwrap_or_default(),
            "ResourceNotFoundException",
            "case {case:?}: expected the modelled ResourceNotFoundException, got {response}"
        );
        assert_eq!(
            response["message"].as_str().unwrap_or_default(),
            format!("Resource: {configuration_arn} not found"),
            "case {case:?}: unexpected not-found wording, got {response}"
        );
        let read = realtime
            .ok(
                case,
                "GetParticipant",
                &json!({
                    "stageArn": stage_arn,
                    "sessionId": session_id,
                    "participantId": participant_id,
                }),
            )
            .await;
        assert_eq!(
            member(case, &read["participant"], "state"),
            PARTICIPANT_DISCONNECTED,
            "case {case:?}: a forced delete disconnects the publisher, got {read}"
        );
        // And the publisher itself is disconnected on the wire: it is sending into a stage
        // it is no longer in, so the ingest server severs the connection.
        publisher
            .send_media()
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: {error}"));
        assert!(
            publisher.severed().await,
            "case {case:?}: expected the ingest server to sever a disconnected publisher"
        );
    }
}

/// Rule 5, fourth leg: a publish presenting a key no ingest configuration of this store
/// mints for a stage is REFUSED on the wire and makes nothing active.
#[tokio::test]
async fn a_publish_presenting_an_unknown_ingest_key_is_refused() {
    let realtime = RealTime::new();
    let case = "the stage a live configuration belongs to";
    let (stage_arn, authority) = realtime.stage_to_contribute_to(case).await;
    // A live configuration, so a refusal is never merely "this store holds no keys at all".
    let live = realtime
        .configuration_for(&contributions()[0], &stage_arn)
        .await;
    let live_arn = member(case, &live, "arn");
    // One associated with NO stage: its key is real, and there is nothing for it to publish
    // to.
    let stageless = realtime
        .ok(
            case,
            "CreateIngestConfiguration",
            &json!({ "ingestProtocol": "RTMPS" }),
        )
        .await["ingestConfiguration"]
        .clone();
    let stageless_arn = member(case, &stageless, "arn");

    // Rows differ along what the rule generalises over: a key nothing minted, the key of a
    // configuration associated with no stage, a key that is a prefix of a live one, and no
    // key at all.
    let live_key = member(case, &live, "streamKey");
    let rows: Vec<(&str, String)> = vec![
        (
            "a key nothing minted",
            "rt_000000000000_us-east-1_absent_not-a-minted-secret".to_string(),
        ),
        (
            "the key of a configuration associated with no stage",
            member(case, &stageless, "streamKey"),
        ),
        (
            "a prefix of a live key",
            live_key[..live_key.len() - 1].to_string(),
        ),
        ("no key at all", String::new()),
    ];
    for (row, key) in rows {
        let refused = Publisher::publishing(&authority, &key, false).await;
        assert!(
            refused.is_err(),
            "case {row:?}: expected the ingest server to refuse the publish"
        );
        let error = refused.err().unwrap_or_default();
        assert!(
            error.contains(PUBLISH_REFUSED_STATUS) || error.contains("closed the connection"),
            "case {row:?}: expected the stated RTMP refusal, got {error:?}"
        );
        assert_eq!(
            realtime.configuration_state(row, &live_arn).await,
            NO_PUBLISHER_STATE,
            "case {row:?}: a refused publish must leave every configuration INACTIVE"
        );
        assert_eq!(
            realtime.configuration_state(row, &stageless_arn).await,
            NO_PUBLISHER_STATE,
            "case {row:?}: a refused publish must leave every configuration INACTIVE"
        );
        assert_eq!(
            realtime.active_session_id(row, &stage_arn).await,
            "",
            "case {row:?}: a refused publish must open no stage session"
        );
    }
}
