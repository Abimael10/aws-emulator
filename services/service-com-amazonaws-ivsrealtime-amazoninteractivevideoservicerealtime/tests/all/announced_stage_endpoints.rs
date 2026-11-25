// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the four ADDRESSES every stage this service reports announces,
//! and for the LIFETIME every participant token it mints states, driven over the wire
//! through `MultiServiceRouter::dispatch`.
//!
//! The rules under test, each stated once and then exercised over a SET of inputs:
//!
//! 1. Every address this emulator hands a caller names the address that caller reached
//!    this emulator at — never a host belonging to someone else. So each member of
//!    `Stage$endpoints` is an absolute URL on `<the address the request was answered on>`,
//!    keeping its own protocol: the `wss` events channel and the `https` WHIP endpoint are
//!    dialed at that address itself, and the `rtmp`/`rtmps` contribution endpoints at the
//!    application path both carry.
//! 2. Every projection of one stage agrees: the create, the read and the update all
//!    announce the same set for the same caller, and a read announces its own caller's
//!    address without changing what a later caller is told.
//! 3. The addresses are the DEPLOYMENT's, not a stage's: every stage reports the same four,
//!    carrying no part of a stage's own identity — which stage a connection joins is
//!    carried by the participant token the client presents on them.
//! 4. A minted `ParticipantToken` expires the EFFECTIVE lifetime it was issued for out —
//!    the `duration` the request sized it with, or the default the model documents on the
//!    reported member itself — on both operations that issue a token;
//!    `CreateParticipantToken` reports `duration` only where the request sized it, while
//!    `CreateStage` restates the effective lifetime of every token it attached.

use std::collections::BTreeSet;
use std::sync::Arc;

use aws_smithy_types::date_time::{DateTime, Format};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_ivsrealtime_amazoninteractivevideoservicerealtime::EmulatedService;

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
/// production traffic.
struct RealTime {
    router: MultiServiceRouter,
}

impl RealTime {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.register_service(EmulatedService::new(engine));
        Self { router }
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
/// application path under it.
fn assert_announced_endpoints(case: &str, stage: &Value, authority: &str) {
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
    for (member, scheme, path) in [
        ("events", "wss", ""),
        ("whip", "https", ""),
        ("rtmp", "rtmp", CONTRIBUTE_APPLICATION_PATH),
        ("rtmps", "rtmps", CONTRIBUTE_APPLICATION_PATH),
    ] {
        assert_announced(case, member, endpoints, authority, scheme, path);
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
            assert_announced_endpoints(&case, &stage, authority);
            let arn = stage["arn"].as_str().unwrap_or_default().to_string();

            // Rule 2: the read and the update of the same stage agree, for the same caller.
            let read = realtime
                .ok_as(&case, "GetStage", &json!({ "arn": arn }), host)
                .await;
            assert_announced_endpoints(&case, &read["stage"], authority);
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
            assert_announced_endpoints(&case, &updated["stage"], authority);
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
    for round in 1..=2 {
        for host in addresses {
            let case = format!("round {round} at {host}");
            let read = realtime
                .ok_as(&case, "GetStage", &json!({ "arn": arn }), Some(host))
                .await;
            assert_announced_endpoints(&case, &read["stage"], host);
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
