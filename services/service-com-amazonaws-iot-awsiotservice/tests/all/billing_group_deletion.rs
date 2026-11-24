// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of the REMOVAL of an AWS IoT billing group:
//! `DeleteBillingGroup`, the `DescribeBillingGroup` reader that proves the record is gone,
//! and the optimistic concurrency the delete runs under.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `iot` signing name — so what is asserted is what an AWS SDK client
//! would see, not an internal function's return value.
//!
//! The rule under test: a delete of a versioned registry record checks its `expectedVersion`
//! before it removes anything and then removes the record outright, so a later read of the
//! name is the family's 404 and the name is free again; an `expectedVersion` the record is not
//! at is refused `VersionConflictException` (409) with this family's sentence and leaves the
//! record — and its version — exactly as they were; and because the operation models no
//! not-found error, a name the account holds no record under is a success that removes
//! nothing, which makes the delete idempotent. A membership that only exists while the record
//! does (a thing's `billingGroupName`) is gone with it.
//!
//! The table spans what the rule generalises over rather than one recorded request: the form
//! and length of the group name within its modelled `length 1..128, pattern
//! ^[a-zA-Z0-9:_-]+$`, how many writes the record has taken before the delete (so the stored
//! version differs row to row), whether the request supplies an `expectedVersion` at all and
//! how a supplied one misses (below, above, zero, far past), and whether the group has things
//! in it.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::state::FIRST_BILLING_GROUP_VERSION;
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The account id this emulator serves, which the registry families qualify a missing name
/// with (`engine::emulator_account_id`).
const ACCOUNT_ID: &str = "000000000000";

/// The region every request in this suite is signed for. Nothing a delete changes is
/// regional, so no assertion depends on it.
const REGION: &str = "eu-west-3";

/// The wire error shape of a request naming a resource this account does not hold.
const NOT_FOUND: &str = "ResourceNotFoundException";

/// The wire error shape of an `expectedVersion` that does not match the record.
const VERSION_CONFLICT: &str = "VersionConflictException";

/// The longest `billingGroupName` the model admits: `BillingGroupName` is
/// `length 1..128, pattern ^[a-zA-Z0-9:_-]+$`
/// (`smithy_query(query="operation",operation="DeleteBillingGroup")`).
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

    /// The `version` a read of the group reports.
    fn version(&self) -> Option<i64> {
        self.body.get("version").and_then(Value::as_i64)
    }

    /// A string member of the body, or `None` when it is absent or not a string.
    fn text(&self, name: &str) -> Option<String> {
        self.body
            .get(name)
            .and_then(Value::as_str)
            .map(str::to_string)
    }
}

/// The emulator under test: ONE shared engine with the IoT service mounted on it, reached the
/// way the production router reaches it.
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
    async fn create(&self, name: &str) -> Reply {
        let body = json!({ "billingGroupProperties": { "billingGroupDescription": "metered" } });
        self.call("POST", &format!("/billing-groups/{name}"), Some(&body))
            .await
    }

    /// `UpdateBillingGroup`, bound to `PATCH /billing-groups/{billingGroupName}` — used to
    /// move a record's version on before a delete reads it.
    async fn update(&self, name: &str, description: &str) -> Reply {
        let body = json!({
            "billingGroupProperties": { "billingGroupDescription": description },
        });
        self.call("PATCH", &format!("/billing-groups/{name}"), Some(&body))
            .await
    }

    /// `DescribeBillingGroup`, bound to `GET /billing-groups/{billingGroupName}`.
    async fn describe(&self, name: &str) -> Reply {
        self.call("GET", &format!("/billing-groups/{name}"), None)
            .await
    }

    /// `DeleteBillingGroup`, bound to `DELETE /billing-groups/{billingGroupName}` with
    /// `expectedVersion` carried as the modelled `httpQuery` parameter.
    async fn delete(&self, name: &str, expected_version: Option<i64>) -> Reply {
        let path = match expected_version {
            Some(version) => format!("/billing-groups/{name}?expectedVersion={version}"),
            None => format!("/billing-groups/{name}"),
        };
        self.call("DELETE", &path, None).await
    }

    /// `CreateThing`, bound to `POST /things/{thingName}`, optionally in a billing group.
    async fn create_thing(&self, thing: &str, billing_group: Option<&str>) -> Reply {
        let body = match billing_group {
            Some(group) => json!({ "billingGroupName": group }),
            None => json!({}),
        };
        self.call("POST", &format!("/things/{thing}"), Some(&body))
            .await
    }

    /// `DescribeThing`, bound to `GET /things/{thingName}`.
    async fn describe_thing(&self, thing: &str) -> Reply {
        self.call("GET", &format!("/things/{thing}"), None).await
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

/// One row: a group name, how many writes the record takes before the delete, and whether the
/// delete supplies the version it is then at as `expectedVersion`.
struct Case {
    label: &'static str,
    name: String,
    writes: i64,
    supply_expected_version: bool,
}

/// The table every accepted delete below is built from. The rows differ along what the rule
/// generalises over: the form and length of the name, the version the record is at when the
/// delete reads it, and whether the request carries an `expectedVersion` at all.
fn cases() -> Vec<Case> {
    vec![
        Case {
            label: "a freshly created group, no expectedVersion",
            name: "metered-fleet".to_string(),
            writes: 0,
            supply_expected_version: false,
        },
        Case {
            label: "a freshly created group, at its created version",
            name: "b".to_string(),
            writes: 0,
            supply_expected_version: true,
        },
        Case {
            label: "a group written once, no expectedVersion",
            name: "EU:billing_group-9".to_string(),
            writes: 1,
            supply_expected_version: false,
        },
        Case {
            label: "a group written twice, at the version it reached",
            name: "Metered:Fleet_01-x".to_string(),
            writes: 2,
            supply_expected_version: true,
        },
        Case {
            label: "a group written five times, at the version it reached",
            name: "n".repeat(MAX_NAME_LEN),
            writes: 5,
            supply_expected_version: true,
        },
    ]
}

/// RULE: an accepted `DeleteBillingGroup` removes the record outright and answers an empty
/// 200, so the reader of that name is the family's 404 and the name is free for a new create —
/// which mints a new identity at the first version rather than resurrecting the old record.
#[tokio::test]
async fn an_accepted_delete_removes_the_record_and_frees_the_name() {
    for case in cases() {
        let Case {
            label,
            name,
            writes,
            supply_expected_version,
        } = case;
        // A fresh emulator per row, so what one row stored cannot decide another.
        let emulator = Emulator::new();

        let created = emulator.create(&name).await;
        assert_eq!(
            created.status, 200,
            "case {label:?}: CreateBillingGroup must succeed, got {created:?}"
        );
        for write in 0..writes {
            let accepted = emulator.update(&name, &format!("write {write}")).await;
            assert_eq!(
                accepted.status, 200,
                "case {label:?}: the setup write must succeed, got {accepted:?}"
            );
        }
        let stored_version = FIRST_BILLING_GROUP_VERSION + writes;
        let before = emulator.describe(&name).await;
        assert_eq!(
            before.version(),
            Some(stored_version),
            "case {label:?}: expected the record at version {stored_version}, got {before:?}"
        );
        let first_id = before.text("billingGroupId");

        let expected_version = supply_expected_version.then_some(stored_version);
        let deleted = emulator.delete(&name, expected_version).await;
        assert_eq!(
            deleted.status, 200,
            "case {label:?}: DeleteBillingGroup must succeed, got {deleted:?}"
        );
        assert_eq!(
            deleted.error_code, None,
            "case {label:?}: a successful delete carries no error type, got {deleted:?}"
        );

        let gone = emulator.describe(&name).await;
        assert_eq!(
            gone.status, 404,
            "case {label:?}: the deleted group must no longer read, got {gone:?}"
        );
        assert_eq!(
            gone.error_code.as_deref(),
            Some(NOT_FOUND),
            "case {label:?}: expected {NOT_FOUND}, got {gone:?}"
        );
        assert_eq!(
            gone.message(),
            Some(not_found_message(&name)),
            "case {label:?}: expected {:?}, got {gone:?}",
            not_found_message(&name)
        );

        // The name is free again, and what takes it is a NEW record: a fresh id, at the first
        // version, rather than the version the deleted record had reached.
        let recreated = emulator.create(&name).await;
        assert_eq!(
            recreated.status, 200,
            "case {label:?}: the freed name must take a new create, got {recreated:?}"
        );
        let after = emulator.describe(&name).await;
        assert_eq!(
            after.version(),
            Some(FIRST_BILLING_GROUP_VERSION),
            "case {label:?}: expected the new record at the first version, got {after:?}"
        );
        assert_ne!(
            after.text("billingGroupId"),
            first_id,
            "case {label:?}: expected the new record to carry a new id, got {after:?}"
        );
    }
}

/// RULE: the `expectedVersion` of a delete is matched against the stored generation — a value
/// the record is not at is refused 409 with this family's sentence and removes NOTHING, while
/// the value the reader reports is accepted and removes the record.
#[tokio::test]
async fn a_stale_expected_version_refuses_the_delete_and_removes_nothing() {
    // `(case, writes taken after the create, the expectedVersion supplied)`. The stored
    // version is one plus the writes, so the rows put the supplied value below it, above it,
    // at zero, and far past it.
    let stale: Vec<(&str, i64, i64)> = vec![
        ("one version behind the record", 1, 1),
        ("two versions behind the record", 2, 1),
        ("a version ahead of a freshly created record", 0, 7),
        ("a version of zero", 0, 0),
        ("a version far past the record", 3, 4_294_967_296),
    ];

    for (label, writes, expected_version) in stale {
        let emulator = Emulator::new();
        let name = "concurrent-fleet";
        assert_eq!(
            emulator.create(name).await.status,
            200,
            "case {label:?}: the group the conflict is measured on must exist"
        );
        for write in 0..writes {
            let accepted = emulator.update(name, &format!("write {write}")).await;
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

        let refused = emulator.delete(name, Some(expected_version)).await;
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

        let survived = emulator.describe(name).await;
        assert_eq!(
            survived.body, before.body,
            "case {label:?}: expected the refused delete to change nothing, got {survived:?}"
        );

        // The version the reader reports IS the one the delete is matched against.
        let accepted = emulator.delete(name, Some(stored_version)).await;
        assert_eq!(
            accepted.status, 200,
            "case {label:?}: the matching expectedVersion must be accepted, got {accepted:?}"
        );
        let gone = emulator.describe(name).await;
        assert_eq!(
            gone.status, 404,
            "case {label:?}: the accepted delete must have removed the record, got {gone:?}"
        );
    }
}

/// RULE: the delete models no not-found error at all, so a name the account holds no record
/// under removes nothing and still succeeds — with or without an `expectedVersion` — which is
/// what makes a repeated delete idempotent.
#[tokio::test]
async fn a_name_the_store_does_not_hold_is_deleted_idempotently() {
    let emulator = Emulator::new();
    assert_eq!(
        emulator.create("metered-fleet").await.status,
        200,
        "the group the misses are measured against must exist"
    );

    // The rows span the ways a name can miss: never created, a prefix of a stored name, a
    // stored name with a suffix, the same letters in another case, and a name at the modelled
    // length ceiling.
    let misses = [
        ("a name nothing created", "absent-group".to_string()),
        ("a prefix of a stored name", "metered".to_string()),
        ("a stored name with a suffix", "metered-fleet-2".to_string()),
        ("a stored name in another case", "Metered-Fleet".to_string()),
        ("a name at the modelled ceiling", "m".repeat(MAX_NAME_LEN)),
    ];
    for (label, name) in misses {
        for expected_version in [None, Some(1_i64), Some(99_i64)] {
            let reply = emulator.delete(&name, expected_version).await;
            assert_eq!(
                reply.status, 200,
                "case {label:?} with expectedVersion {expected_version:?}: a name the store \
                 does not hold must still succeed, got {reply:?}"
            );
            let read = emulator.describe(&name).await;
            assert_eq!(
                read.status, 404,
                "case {label:?} with expectedVersion {expected_version:?}: the name must still \
                 hold no record, got {read:?}"
            );
        }
    }

    // The group that DOES exist is untouched by every delete of a name above, and deleting it
    // twice is the same answer twice: the second call has nothing left to remove.
    let stored = emulator.describe("metered-fleet").await;
    assert_eq!(
        stored.version(),
        Some(FIRST_BILLING_GROUP_VERSION),
        "expected the stored group to be left exactly as created, got {stored:?}"
    );
    for attempt in 1..=3 {
        let reply = emulator.delete("metered-fleet", None).await;
        assert_eq!(
            reply.status, 200,
            "delete attempt {attempt}: expected an idempotent success, got {reply:?}"
        );
        let read = emulator.describe("metered-fleet").await;
        assert_eq!(
            read.status, 404,
            "delete attempt {attempt}: expected the group to stay removed, got {read:?}"
        );
    }
}

/// RULE: a membership that exists only while the record does goes with it — a thing's
/// `billingGroupName` names a group the account holds, so once the group is deleted no reader
/// of the thing reports it and a create may not name it either, while the things themselves
/// stay registered.
#[tokio::test]
async fn deleting_a_group_ends_the_memberships_that_only_exist_with_it() {
    // The rows span how many things are in the group when it is deleted: none, one, and
    // several, beside a thing in ANOTHER group that must be left alone.
    for (label, members) in [
        ("no things in the group", 0_usize),
        ("one thing in the group", 1),
        ("several things in the group", 4),
    ] {
        let emulator = Emulator::new();
        let deleted_group = "metered-fleet";
        let kept_group = "kept-fleet";
        for group in [deleted_group, kept_group] {
            assert_eq!(
                emulator.create(group).await.status,
                200,
                "case {label:?}: the group {group} must exist"
            );
        }

        let member_names: Vec<String> = (0..members).map(|n| format!("sensor-{n}")).collect();
        for thing in &member_names {
            let created = emulator.create_thing(thing, Some(deleted_group)).await;
            assert_eq!(
                created.status, 200,
                "case {label:?}: CreateThing in {deleted_group} must succeed, got {created:?}"
            );
            let read = emulator.describe_thing(thing).await;
            assert_eq!(
                read.text("billingGroupName").as_deref(),
                Some(deleted_group),
                "case {label:?}: expected the thing to report its group, got {read:?}"
            );
        }
        let outsider = "gateway-0";
        assert_eq!(
            emulator.create_thing(outsider, Some(kept_group)).await.status,
            200,
            "case {label:?}: the thing in the kept group must be created"
        );

        let deleted = emulator.delete(deleted_group, None).await;
        assert_eq!(
            deleted.status, 200,
            "case {label:?}: DeleteBillingGroup must succeed, got {deleted:?}"
        );

        for thing in &member_names {
            let read = emulator.describe_thing(thing).await;
            assert_eq!(
                read.status, 200,
                "case {label:?}: the thing itself must stay registered, got {read:?}"
            );
            assert_eq!(
                read.text("billingGroupName"),
                None,
                "case {label:?}: expected no membership of a deleted group, got {read:?}"
            );
        }
        // A create may no longer name the deleted group: the registry resolves
        // `billingGroupName` against the records the account holds.
        let refused = emulator.create_thing("late-sensor", Some(deleted_group)).await;
        assert_eq!(
            refused.status, 404,
            "case {label:?}: a thing may not join a deleted group, got {refused:?}"
        );
        assert_eq!(
            refused.error_code.as_deref(),
            Some(NOT_FOUND),
            "case {label:?}: expected {NOT_FOUND}, got {refused:?}"
        );
        // The group that was NOT deleted, and the thing in it, are untouched.
        let kept = emulator.describe(kept_group).await;
        assert_eq!(
            kept.status, 200,
            "case {label:?}: the other group must survive, got {kept:?}"
        );
        let kept_member = emulator.describe_thing(outsider).await;
        assert_eq!(
            kept_member.text("billingGroupName").as_deref(),
            Some(kept_group),
            "case {label:?}: expected the other group's membership to survive, got \
             {kept_member:?}"
        );
    }
}
