// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of a WRITE of an existing AWS IoT billing group:
//! `UpdateBillingGroup`, the `DescribeBillingGroup` reader that proves what it left behind,
//! and the optimistic concurrency the two share.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `iot` signing name — so what is asserted is what an AWS SDK
//! client would see, not an internal function's return value.
//!
//! The rule under test: an update of a versioned registry record REPLACES the properties
//! its `@required` properties member carries and advances the record's version by one per
//! accepted write, answering that new version; the reader of the same record then reports
//! the new properties at the new version. The version is the optimistic-concurrency
//! generation the writes are serialized by: an `expectedVersion` equal to the stored version
//! is accepted and moves it on, and one that is not is refused `VersionConflictException`
//! (409) with this family's sentence while the record — and its version — stay exactly as
//! they were. A name the account holds no record under is refused
//! `ResourceNotFoundException` (404) with the registry families' account-qualified sentence
//! and writes nothing, so the name is still free.
//!
//! The table spans what the rule generalises over rather than one recorded request: the form
//! and length of the group name within its modelled `length 1..128, pattern
//! ^[a-zA-Z0-9:_-]+$`, what the stored description is before the write and what the write
//! asks it to become (absent, empty, ordinary text and at the modelled `length 0..2028`
//! ceiling, in both directions), and how many writes the record has taken.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::state::FIRST_BILLING_GROUP_VERSION;
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The account id this emulator serves, which the registry families qualify a missing name
/// with (`engine::emulator_account_id`).
const ACCOUNT_ID: &str = "000000000000";

/// The region every request in this suite is signed for. Nothing an update changes is
/// regional, so no assertion depends on it.
const REGION: &str = "ap-south-1";

/// The wire error shape of a request naming a resource this account does not hold.
const NOT_FOUND: &str = "ResourceNotFoundException";

/// The wire error shape of an `expectedVersion` that does not match the record.
const VERSION_CONFLICT: &str = "VersionConflictException";

/// The longest `billingGroupDescription` the model admits: `BillingGroupDescription` is
/// `length 0..2028, pattern ^[\p{Graph}\x20]*$`
/// (`smithy_query(query="shape",shape="BillingGroupProperties")`).
const MAX_DESCRIPTION_LEN: usize = 2028;

/// The longest `billingGroupName` the model admits: `BillingGroupName` is
/// `length 1..128, pattern ^[a-zA-Z0-9:_-]+$`
/// (`smithy_query(query="operation",operation="UpdateBillingGroup")`).
const MAX_NAME_LEN: usize = 128;

/// A SigV4 `Authorization` header whose credential scope names the `iot` signing name: the
/// scope is how the router picks the claiming service. The signature itself is never checked
/// here — the harness turns request authentication off, exactly as an unauthenticated
/// emulator runs.
fn signed_for_iot() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTIOT/20260101/{REGION}/iot/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=0000"
    )
}

/// One response, reduced to what a client branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    error_code: Option<String>,
    body: Value,
}

impl Reply {
    /// The `message` an SDK surfaces for a refusal.
    fn message(&self) -> Option<String> {
        self.body
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    /// The `version` an update answers with, or a read of the group reports.
    fn version(&self) -> Option<i64> {
        self.body.get("version").and_then(Value::as_i64)
    }

    /// `billingGroupProperties.billingGroupDescription`, or `None` when either the
    /// properties or the description is absent.
    fn description(&self) -> Option<String> {
        self.body
            .get("billingGroupProperties")?
            .get("billingGroupDescription")?
            .as_str()
            .map(str::to_string)
    }

    /// A string member of the body, or `None` when it is absent or not a string.
    fn text(&self, name: &str) -> Option<String> {
        self.body
            .get(name)
            .and_then(Value::as_str)
            .map(str::to_string)
    }
}

/// The emulator under test: ONE shared engine with the IoT service mounted on it, reached
/// the way the production router reaches it.
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

    async fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Reply {
        let payload = body.map_or_else(Vec::new, |value| {
            serde_json::to_vec(value).expect("a request body serializes")
        });
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://iot.{REGION}.amazonaws.com{path}"))
            .header("authorization", signed_for_iot())
            .header("content-type", "application/json")
            .body(bytes::Bytes::from(payload))
            .expect("a well-formed request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_code = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Reply {
            status,
            error_code,
            body,
        }
    }

    /// `CreateBillingGroup`, bound to `POST /billing-groups/{billingGroupName}`.
    async fn create(&self, name: &str, properties: &Value) -> Reply {
        let body = json!({ "billingGroupProperties": properties });
        self.call("POST", &format!("/billing-groups/{name}"), Some(&body))
            .await
    }

    /// `UpdateBillingGroup`, bound to `PATCH /billing-groups/{billingGroupName}`.
    async fn update(&self, name: &str, body: &Value) -> Reply {
        self.call("PATCH", &format!("/billing-groups/{name}"), Some(body))
            .await
    }

    /// `DescribeBillingGroup`, bound to `GET /billing-groups/{billingGroupName}`.
    async fn describe(&self, name: &str) -> Reply {
        self.call("GET", &format!("/billing-groups/{name}"), None)
            .await
    }
}

/// The sentence this family states a missing billing group with: the registry families'
/// account-qualified form.
fn not_found_message(name: &str) -> String {
    format!("BillingGroup {ACCOUNT_ID}:{name} not found.")
}

/// The sentence this family states a version conflict with.
fn version_conflict_message(name: &str) -> String {
    format!("The version for billing group {name} does not match the expected version.")
}

/// `billingGroupProperties` as a request carries them for a description, and `None` for
/// properties that name no description at all.
fn properties(description: Option<&str>) -> Value {
    match description {
        Some(text) => json!({ "billingGroupDescription": text }),
        None => json!({}),
    }
}

/// One row: a group name, the description its create stores, and the description its update
/// asks for — which is what the record then has, because the properties the update carries
/// ARE the group's properties afterwards.
struct Case {
    label: &'static str,
    name: String,
    created: Option<String>,
    updated: Option<String>,
}

/// The table every case below is built from. The rows differ along what the rule generalises
/// over: the form and length of the name, and the description before and after the write —
/// absent, empty, ordinary text and at the modelled ceiling, in both directions.
fn cases() -> Vec<Case> {
    vec![
        Case {
            label: "a description replacing another description",
            name: "metered-fleet".to_string(),
            created: Some("the fleet as created".to_string()),
            updated: Some("the fleet as updated".to_string()),
        },
        Case {
            label: "a description replacing none",
            name: "b".to_string(),
            created: None,
            updated: Some("first description".to_string()),
        },
        Case {
            label: "properties naming no description, over a stored one",
            name: "EU:billing_group-9".to_string(),
            created: Some("to be dropped".to_string()),
            updated: None,
        },
        Case {
            label: "an EMPTY description, which the modelled length admits",
            name: "Metered:Fleet_01-x".to_string(),
            created: Some("to be emptied".to_string()),
            updated: Some(String::new()),
        },
        Case {
            label: "a description at the modelled length ceiling",
            name: "ceiling-description".to_string(),
            created: Some(String::new()),
            updated: Some("d".repeat(MAX_DESCRIPTION_LEN)),
        },
        Case {
            label: "a name at the modelled length ceiling",
            name: "n".repeat(MAX_NAME_LEN),
            created: Some("Line-haul fleet #7 (eu)".to_string()),
            updated: Some("Line-haul fleet #8 (eu)".to_string()),
        },
    ]
}

/// RULE: an accepted `UpdateBillingGroup` replaces the stored `billingGroupProperties` and
/// advances the record's version by one, answering that new version; the reader then reports
/// the new properties at the new version, and the identity the create minted is untouched.
#[tokio::test]
async fn an_update_replaces_the_stored_properties_and_advances_the_version() {
    for case in cases() {
        let Case {
            label,
            name,
            created,
            updated,
        } = case;
        // A fresh emulator per row, so what one row stored cannot decide another.
        let emulator = Emulator::new();

        let create = emulator
            .create(&name, &properties(created.as_deref()))
            .await;
        assert_eq!(
            create.status, 200,
            "case {label:?}: CreateBillingGroup must succeed, got {create:?}"
        );
        let before = emulator.describe(&name).await;
        assert_eq!(
            before.description(),
            created,
            "case {label:?}: expected the created description {created:?}, got {before:?}"
        );
        assert_eq!(
            before.version(),
            Some(FIRST_BILLING_GROUP_VERSION),
            "case {label:?}: expected the version of a record written once, got {before:?}"
        );

        let write = emulator
            .update(
                &name,
                &json!({ "billingGroupProperties": properties(updated.as_deref()) }),
            )
            .await;
        assert_eq!(
            write.status, 200,
            "case {label:?}: UpdateBillingGroup must succeed, got {write:?}"
        );
        assert_eq!(
            write.version(),
            Some(FIRST_BILLING_GROUP_VERSION + 1),
            "case {label:?}: expected the update to answer the advanced version, got {write:?}"
        );

        let after = emulator.describe(&name).await;
        assert_eq!(
            after.status, 200,
            "case {label:?}: the updated group must still read, got {after:?}"
        );
        assert_eq!(
            after.description(),
            updated,
            "case {label:?}: expected the read to report the updated description {updated:?}, \
             got {after:?}"
        );
        assert_eq!(
            after.version(),
            write.version(),
            "case {label:?}: expected the read to report the version the update answered, got \
             {after:?}"
        );
        // Nothing of the group's identity is part of an update: the name, the id and the ARN
        // the create minted survive the write.
        for member in ["billingGroupName", "billingGroupId", "billingGroupArn"] {
            assert_eq!(
                after.text(member),
                before.text(member),
                "case {label:?}: expected {member} to survive the update, got {after:?}"
            );
        }
        // A read writes nothing, so a second one reports the same record.
        let again = emulator.describe(&name).await;
        assert_eq!(
            again.body, after.body,
            "case {label:?}: expected a second read to report the same record, got {again:?}"
        );
    }
}

/// RULE: each accepted write advances the version by exactly one, so the generation counter
/// counts the writes the record has taken rather than resting where the create left it.
#[tokio::test]
async fn every_accepted_write_advances_the_version_by_one() {
    let emulator = Emulator::new();
    let name = "counted-writes";
    assert_eq!(
        emulator.create(name, &properties(None)).await.status,
        200,
        "the group the writes are counted on must exist"
    );

    // Eight writes: enough that a version frozen at the create, or advanced once and then
    // stuck, is a different number from the writes taken.
    for write in 1..=8_i64 {
        let description = format!("description after write {write}");
        let reply = emulator
            .update(
                name,
                &json!({
                    "billingGroupProperties": { "billingGroupDescription": description },
                }),
            )
            .await;
        let expected = FIRST_BILLING_GROUP_VERSION + write;
        assert_eq!(
            reply.status, 200,
            "write {write}: UpdateBillingGroup must succeed, got {reply:?}"
        );
        assert_eq!(
            reply.version(),
            Some(expected),
            "write {write}: expected version {expected}, got {reply:?}"
        );
        let read = emulator.describe(name).await;
        assert_eq!(
            read.version(),
            Some(expected),
            "write {write}: expected the read to report version {expected}, got {read:?}"
        );
        assert_eq!(
            read.description(),
            Some(description.clone()),
            "write {write}: expected the read to report {description:?}, got {read:?}"
        );
    }
}

/// RULE: the version a reader reports is the optimistic-concurrency generation a write is
/// matched against — an `expectedVersion` equal to it is accepted and moves it on, and one
/// that is not is refused 409 with this family's sentence while the record and its version
/// stay as they were.
#[tokio::test]
async fn an_expected_version_is_matched_against_the_stored_generation() {
    // `(case, writes taken after the create, the expectedVersion supplied)`. The stored
    // version is one plus the writes, so the rows put the supplied value below it, above it,
    // and at zero.
    let stale: Vec<(&str, i64, i64)> = vec![
        ("one write behind the record", 1, 1),
        ("two writes behind the record", 2, 1),
        ("a version ahead of a freshly created record", 0, 7),
        ("a version of zero", 0, 0),
        ("a version far past the record", 3, 4_294_967_296),
    ];

    for (label, writes, expected_version) in stale {
        let emulator = Emulator::new();
        let name = "concurrent-fleet";
        assert_eq!(
            emulator.create(name, &properties(None)).await.status,
            200,
            "case {label:?}: the group the conflict is measured on must exist"
        );
        for write in 0..writes {
            let accepted = emulator
                .update(
                    name,
                    &json!({
                        "billingGroupProperties": {
                            "billingGroupDescription": format!("write {write}"),
                        },
                    }),
                )
                .await;
            assert_eq!(
                accepted.status, 200,
                "case {label:?}: the setup write must succeed, got {accepted:?}"
            );
        }
        let stored_version = FIRST_BILLING_GROUP_VERSION + writes;
        let before = emulator.describe(name).await;
        assert_eq!(
            before.version(),
            Some(stored_version),
            "case {label:?}: expected the record at version {stored_version}, got {before:?}"
        );

        let refused = emulator
            .update(
                name,
                &json!({
                    "billingGroupProperties": { "billingGroupDescription": "clobbered" },
                    "expectedVersion": expected_version,
                }),
            )
            .await;
        assert_eq!(
            refused.status, 409,
            "case {label:?}: a stale expectedVersion must be refused 409, got {refused:?}"
        );
        assert_eq!(
            refused.error_code.as_deref(),
            Some(VERSION_CONFLICT),
            "case {label:?}: expected {VERSION_CONFLICT}, got {refused:?}"
        );
        assert_eq!(
            refused.message(),
            Some(version_conflict_message(name)),
            "case {label:?}: expected {:?}, got {refused:?}",
            version_conflict_message(name)
        );

        let after = emulator.describe(name).await;
        assert_eq!(
            after.body, before.body,
            "case {label:?}: expected the refused write to change nothing, got {after:?}"
        );

        // The version the reader reports IS the one the write is matched against: supplied
        // as `expectedVersion`, it is accepted and advances the record.
        let accepted = emulator
            .update(
                name,
                &json!({
                    "billingGroupProperties": { "billingGroupDescription": "threaded" },
                    "expectedVersion": stored_version,
                }),
            )
            .await;
        assert_eq!(
            accepted.status, 200,
            "case {label:?}: the matching expectedVersion must be accepted, got {accepted:?}"
        );
        assert_eq!(
            accepted.version(),
            Some(stored_version + 1),
            "case {label:?}: expected the accepted write to advance the version, got \
             {accepted:?}"
        );
        // And the version it just left is no longer the one a writer may claim, so the
        // generation the caller held cannot be reused.
        let reused = emulator
            .update(
                name,
                &json!({
                    "billingGroupProperties": { "billingGroupDescription": "second writer" },
                    "expectedVersion": stored_version,
                }),
            )
            .await;
        assert_eq!(
            reused.status, 409,
            "case {label:?}: the version already written past must conflict, got {reused:?}"
        );
        let settled = emulator.describe(name).await;
        assert_eq!(
            settled.description(),
            Some("threaded".to_string()),
            "case {label:?}: expected the accepted write's description to stand, got \
             {settled:?}"
        );
        assert_eq!(
            settled.version(),
            Some(stored_version + 1),
            "case {label:?}: expected the record at the accepted version, got {settled:?}"
        );
    }
}

/// RULE: a write of a name the account holds no billing group under is refused 404 with the
/// registry families' account-qualified sentence and records nothing — never a 200 updating a
/// group that does not exist, and never a 501.
#[tokio::test]
async fn a_name_the_store_does_not_hold_is_refused_by_the_writer() {
    let emulator = Emulator::new();
    let created = emulator.create("metered-fleet", &properties(None)).await;
    assert_eq!(
        created.status, 200,
        "the group the misses are measured against must exist"
    );

    // The rows span the ways a name can miss: never created, a prefix of a stored name, a
    // stored name with a suffix, the same letters in another case, and a name at the
    // modelled length ceiling.
    let misses = [
        ("a name nothing created", "absent-group".to_string()),
        ("a prefix of a stored name", "metered".to_string()),
        ("a stored name with a suffix", "metered-fleet-2".to_string()),
        ("a stored name in another case", "Metered-Fleet".to_string()),
        ("a name at the modelled ceiling", "m".repeat(MAX_NAME_LEN)),
    ];
    for (label, name) in misses {
        for (which, body) in [
            (
                "with no expectedVersion",
                json!({ "billingGroupProperties": { "billingGroupDescription": "x" } }),
            ),
            (
                "with an expectedVersion",
                json!({
                    "billingGroupProperties": { "billingGroupDescription": "x" },
                    "expectedVersion": 1,
                }),
            ),
        ] {
            let refused = emulator.update(&name, &body).await;
            assert_eq!(
                refused.status, 404,
                "case {label:?} {which}: expected a 404 write, got {refused:?}"
            );
            assert_eq!(
                refused.error_code.as_deref(),
                Some(NOT_FOUND),
                "case {label:?} {which}: expected {NOT_FOUND}, got {refused:?}"
            );
            assert_eq!(
                refused.message(),
                Some(not_found_message(&name)),
                "case {label:?} {which}: expected {:?}, got {refused:?}",
                not_found_message(&name)
            );
            let read = emulator.describe(&name).await;
            assert_eq!(
                read.status, 404,
                "case {label:?} {which}: the refused write must have recorded nothing, got \
                 {read:?}"
            );
        }
    }

    // The group that DOES exist is untouched by every refused write above.
    let stored = emulator.describe("metered-fleet").await;
    assert_eq!(
        stored.version(),
        Some(FIRST_BILLING_GROUP_VERSION),
        "expected the stored group to be left at its created version, got {stored:?}"
    );
}
