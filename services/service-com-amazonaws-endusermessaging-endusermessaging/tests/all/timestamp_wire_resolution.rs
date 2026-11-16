// SPDX-License-Identifier: CC0-1.0

//! The resolution every reported instant of this service is carried at.
//!
//! The rule under test, over a SET of instants and over every operation that
//! reports one rather than one recorded response: every `Timestamp` member this
//! service reports is `smithy.api#Timestamp` with no `@timestampFormat` override, so
//! restJson1 carries it in the body as epoch seconds, and this API — which documents
//! each one as a time "in Unix epoch time" — reports the whole second the instant
//! falls in. So, whatever sub-second offset the host clock happened to be at when a
//! record was written:
//!
//! * the serialized number carries no fractional digits at all, for
//!   `CreateBrandProfile`, `GetBrandProfile`, `UpdateBrandProfile`,
//!   `ListBrandProfiles`, `ListBrandProfileAttributes`, `GetBrandProfileAttribute`,
//!   `CreateNotifyCodeConfiguration` and `GetNotifyCodeConfiguration` — every
//!   implemented operation of this service that projects a stored instant;
//! * it is epoch SECONDS, not milliseconds, so it falls in the range a second count
//!   of this era occupies;
//! * it is stable: reading the same record again reports the identical instant;
//! * the shared projection truncates an instant to the second it falls in, so a
//!   reported instant never runs ahead of the clock that recorded it.
//!
//! The fractional digits are the thing under test, so they are read from the
//! response bytes as the server wrote them rather than from a parsed float, whose
//! own rounding would hide them.

use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use aws_smithy_types::date_time::Format;
use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_endusermessaging_endusermessaging::state::timestamp;
use service_com_amazonaws_endusermessaging_endusermessaging::EmulatedService;

/// This service's sigv4 signing name, from the model (`OPERATIONS.md`, "Wire protocol").
const SIGNING_NAME: &str = "end-user-messaging";

/// The epoch-second window a reported instant of a record created by this test run
/// falls in: generous on both sides, so this is a check that the number is a second
/// count of this era rather than a millisecond count (which would be a thousand
/// times larger) and not a check on how fast the host clock runs.
const EARLIEST_PLAUSIBLE_SECS: f64 = 1_600_000_000.0;
const LATEST_PLAUSIBLE_SECS: f64 = 4_102_444_800.0;

/// One second's worth of nanoseconds.
const NANOS_PER_SECOND: u32 = 1_000_000_000;

/// One response, with the body kept both parsed and exactly as the server wrote it.
#[derive(Debug)]
struct Reply {
    status: u16,
    body: Value,
    raw: String,
}

/// One emulator over one shared engine: every call sees the same store.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    async fn call(&self, method: &str, path_and_query: &str, body: Option<Value>) -> Reply {
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential=AKIDTESTONLY/20260101/us-east-1/{SIGNING_NAME}/aws4_request, \
             SignedHeaders=host, Signature=00"
        );
        let payload = body
            .map(|value| Bytes::from(serde_json::to_vec(&value).expect("a serializable body")))
            .unwrap_or_default();
        let request = http::Request::builder()
            .method(method)
            .uri(format!(
                "https://{SIGNING_NAME}.us-east-1.amazonaws.com{path_and_query}"
            ))
            .header("authorization", authorization)
            .header("content-type", "application/json")
            .body(payload)
            .expect("a well-formed test request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let bytes = response.into_body();
        let raw = String::from_utf8_lossy(&bytes).to_string();
        let body: Value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Reply { status, body, raw }
    }

    async fn create_profile(&self, name: &str) -> Reply {
        self.call(
            "POST",
            "/v1/brand-profiles",
            Some(json!({ "brandProfileName": name })),
        )
        .await
    }

    async fn get_profile(&self, id: &str) -> Reply {
        self.call("GET", &format!("/v1/brand-profiles/{id}"), None)
            .await
    }

    async fn update_profile(&self, id: &str, body: Value) -> Reply {
        self.call("PUT", &format!("/v1/brand-profiles/{id}"), Some(body))
            .await
    }

    async fn list_profiles(&self) -> Reply {
        self.call("GET", "/v1/brand-profiles", None).await
    }

    async fn list_attributes(&self, id: &str) -> Reply {
        self.call("GET", &format!("/v1/brand-profiles/{id}/attributes"), None)
            .await
    }

    async fn create_attribute(&self, id: &str, name: &str) -> Reply {
        self.call(
            "POST",
            &format!("/v1/brand-profiles/{id}/attributes"),
            Some(json!({
                "attributes": [{
                    "attributeName": name,
                    "attributeType": "TEXT",
                    "attributeValue": "a value",
                }]
            })),
        )
        .await
    }

    async fn get_attribute(&self, id: &str, name: &str) -> Reply {
        self.call(
            "GET",
            &format!("/v1/brand-profiles/{id}/attributes/{name}"),
            None,
        )
        .await
    }

    async fn create_configuration(&self, name: &str) -> Reply {
        self.call(
            "POST",
            "/v1/notify-code-configurations",
            Some(json!({ "notifyCodeConfigurationName": name })),
        )
        .await
    }

    async fn get_configuration(&self, id: &str) -> Reply {
        self.call(
            "GET",
            &format!("/v1/notify-code-configurations/{id}"),
            None,
        )
        .await
    }
}

/// The text of every number the body carries under `member`, exactly as the response
/// serialized it, wherever in the body it appears (a top-level member, a member of a
/// nested resource, or one entry per item of a list).
fn wire_numbers(raw: &str, member: &str) -> Vec<String> {
    let needle = format!("\"{member}\":");
    raw.match_indices(needle.as_str())
        .map(|(at, _)| {
            raw[at + needle.len()..]
                .trim_start()
                .chars()
                .take_while(|character| {
                    character.is_ascii_digit() || matches!(character, '.' | '-' | '+' | 'e' | 'E')
                })
                .collect::<String>()
        })
        .collect()
}

/// The digits after the decimal point of a serialized number.
fn fractional_digits(text: &str) -> usize {
    text.split_once('.')
        .map(|(_, fraction)| fraction.len())
        .unwrap_or(0)
}

/// Assert `reply` reports every one of `members` as a whole count of epoch seconds,
/// at least once each.
fn assert_whole_epoch_seconds(case: &str, reply: &Reply, members: &[&str]) {
    assert_eq!(
        reply.status, 200,
        "case {case}: the call under test must succeed, got {} / {:?}",
        reply.status, reply.body
    );
    for member in members {
        let reported = wire_numbers(&reply.raw, member);
        assert!(
            !reported.is_empty(),
            "case {case}: {member} must be reported, got {:?}",
            reply.raw
        );
        for text in reported {
            assert!(
                !text.contains('e') && !text.contains('E'),
                "case {case}: {member} must be a plain decimal count of epoch seconds, \
                 got {text:?}"
            );
            assert_eq!(
                fractional_digits(&text),
                0,
                "case {case}: {member} must carry no fractional digits, got {text:?}"
            );
            let seconds: f64 = text
                .parse()
                .unwrap_or_else(|_| panic!("case {case}: {member} is not a number, got {text:?}"));
            assert!(
                (EARLIEST_PLAUSIBLE_SECS..=LATEST_PLAUSIBLE_SECS).contains(&seconds),
                "case {case}: {member} must be a count of epoch SECONDS, got {seconds}"
            );
        }
    }
}

#[tokio::test]
async fn every_operation_reports_its_instants_as_whole_epoch_seconds() {
    let em = Emulator::new();

    // One row per record, created back to back so each lands at a different
    // sub-second offset of the host clock: whatever offset a record was written at,
    // every projection of it reports the second it fell in.
    for round in 0..16 {
        let case = format!("round {round}");
        let created = em.create_profile(&format!("brand-{round}")).await;
        assert_whole_epoch_seconds(
            &format!("{case}: CreateBrandProfile"),
            &created,
            &["createdAt", "updatedAt"],
        );
        let id = created.body["brandProfileId"]
            .as_str()
            .unwrap_or_else(|| panic!("{case}: a created profile reports its id"))
            .to_string();

        assert_whole_epoch_seconds(
            &format!("{case}: GetBrandProfile"),
            &em.get_profile(&id).await,
            &["createdAt", "updatedAt"],
        );
        assert_whole_epoch_seconds(
            &format!("{case}: UpdateBrandProfile"),
            &em.update_profile(&id, json!({ "brandProfileName": "renamed" }))
                .await,
            &["createdAt", "updatedAt"],
        );
        // Every seeded default attribute of the profile is projected here, so this
        // row covers the attribute instants as well as the profile's own.
        assert_whole_epoch_seconds(
            &format!("{case}: ListBrandProfileAttributes"),
            &em.list_attributes(&id).await,
            &["createdAt", "updatedAt"],
        );
        // And an attribute written by this round's own request, read back through
        // the single-attribute reader.
        let attribute = format!("Slogan-{round}");
        let written = em.create_attribute(&id, &attribute).await;
        assert_eq!(
            written.status, 200,
            "{case}: writing an attribute must succeed, got {} / {:?}",
            written.status, written.body
        );
        assert_whole_epoch_seconds(
            &format!("{case}: GetBrandProfileAttribute"),
            &em.get_attribute(&id, &attribute).await,
            &["createdAt", "updatedAt"],
        );

        let configuration = em.create_configuration(&format!("code-{round}")).await;
        assert_whole_epoch_seconds(
            &format!("{case}: CreateNotifyCodeConfiguration"),
            &configuration,
            &["createdAt", "updatedAt"],
        );
        let configuration_id = configuration.body["notifyCodeConfiguration"]
            ["notifyCodeConfigurationId"]
            .as_str()
            .unwrap_or_else(|| panic!("{case}: a created configuration reports its id"))
            .to_string();
        assert_whole_epoch_seconds(
            &format!("{case}: GetNotifyCodeConfiguration"),
            &em.get_configuration(&configuration_id).await,
            &["createdAt", "updatedAt"],
        );
    }

    // And the list projection, which reports every profile written above in one body.
    assert_whole_epoch_seconds(
        "ListBrandProfiles",
        &em.list_profiles().await,
        &["createdAt", "updatedAt"],
    );
}

#[tokio::test]
async fn a_reported_instant_is_the_same_on_every_read() {
    let em = Emulator::new();
    let created = em.create_profile("stable").await;
    let id = created.body["brandProfileId"]
        .as_str()
        .expect("a created profile reports its id")
        .to_string();

    let read_back = em.get_profile(&id).await;
    assert_eq!(
        read_back.status, 200,
        "GetBrandProfile must find the profile just created, got {} / {:?}",
        read_back.status, read_back.body
    );
    let first = wire_numbers(&read_back.raw, "createdAt");
    assert_eq!(
        first.len(),
        1,
        "a read reports exactly one createdAt, got {first:?}"
    );
    for read in 0..4 {
        let next = em.get_profile(&id).await;
        assert_eq!(
            next.status, 200,
            "case read {read}: GetBrandProfile must keep finding the profile, got {} / {:?}",
            next.status, next.body
        );
        let again = wire_numbers(&next.raw, "createdAt");
        assert_eq!(
            again, first,
            "case read {read}: a reported instant must not change between reads, \
             got {again:?} then {first:?}"
        );
    }
    assert_eq!(
        wire_numbers(&created.raw, "createdAt"),
        first,
        "the create and the read must report one instant, got {:?} vs {first:?}",
        created.raw
    );
}

#[test]
fn the_shared_projection_truncates_an_instant_to_its_second() {
    // Each row is a sub-second offset the host clock can be at: the whole second,
    // the smallest offset above it, a microsecond, a millisecond, a
    // nanosecond-resolution offset, the half second (so truncating is told apart
    // from rounding), and the last nanosecond of the second.
    let offsets: [(&str, u32); 7] = [
        ("the whole second", 0),
        ("one nanosecond", 1),
        ("one microsecond", 1_000),
        ("one millisecond", 1_000_000),
        ("a nanosecond-resolution instant", 870_512_500),
        ("the half second", NANOS_PER_SECOND / 2),
        ("the last nanosecond of the second", NANOS_PER_SECOND - 1),
    ];

    for (case, nanos) in offsets {
        let at = UNIX_EPOCH + Duration::new(1_700_000_000, nanos);
        let reported = timestamp(at);
        assert_eq!(
            reported.subsec_nanos(),
            0,
            "case {case}: an instant is reported as the whole second it falls in, got {}",
            reported.subsec_nanos()
        );
        assert_eq!(
            reported.secs(),
            1_700_000_000,
            "case {case}: truncating must not move the second, got {}",
            reported.secs()
        );
        let text = reported
            .fmt(Format::EpochSeconds)
            .unwrap_or_else(|error| panic!("case {case}: epoch seconds must format: {error}"));
        assert_eq!(
            fractional_digits(&text),
            0,
            "case {case}: no fractional digit reaches the wire, got {text:?}"
        );
    }

    // The floor of the second, never its ceiling: a reported instant never runs
    // ahead of the instant it was taken from, and is less than a second behind it.
    for nanos in [0_u32, 1, 499_999_999, 500_000_000, NANOS_PER_SECOND - 1] {
        let at = UNIX_EPOCH + Duration::new(1_700_000_000, nanos);
        let projected = timestamp(at);
        let reported = Duration::new(
            u64::try_from(projected.secs()).expect("an instant after the epoch"),
            projected.subsec_nanos(),
        );
        let exact = at
            .duration_since(UNIX_EPOCH)
            .expect("an instant after the epoch");
        assert!(
            reported <= exact && exact - reported < Duration::from_secs(1),
            "case {nanos} ns: the reported instant must be the second containing the stored \
             one, got {reported:?} for {exact:?}"
        );
    }

    // An instant from before the epoch, which a stored instant never is, reports the
    // epoch itself rather than a negative count, at the same resolution.
    let before_epoch = UNIX_EPOCH - Duration::from_nanos(1);
    let fallback = timestamp(before_epoch);
    assert_eq!(
        (fallback.secs(), fallback.subsec_nanos()),
        (0, 0),
        "case before the epoch: the fallback instant is the epoch, got {fallback:?}"
    );
}
