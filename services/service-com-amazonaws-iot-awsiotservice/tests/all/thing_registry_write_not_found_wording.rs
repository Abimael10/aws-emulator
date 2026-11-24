// SPDX-License-Identifier: CC0-1.0

//! How the THING family words a `thingName` the registry does not hold, on each of its
//! two halves, end to end over HTTP.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `iot` signing name — so what is asserted is the status, the
//! `x-amzn-errortype` a client resolves the error shape from and the `message` an SDK
//! surfaces, not an internal function's return value.
//!
//! The rule under test: the thing family states a miss in TWO wordings, split along
//! whether the operation reads the thing record or writes it. A READ of a thing qualifies
//! the name with the account the call was made for — `Thing <accountId>:<thingName> not
//! found.` — while the operation that WRITES the record addressed by name (`UpdateThing`,
//! `PATCH /things/{thingName}`) states the thing name ALONE: `Thing <thingName> not
//! found.`, whose only variable part is the name the request carried, so nothing of the
//! serving account appears in it. Both halves are 404 `ResourceNotFoundException`, the
//! error the model declares for either operation.
//!
//! The table spans what the rule generalises over: the form and length of the name (the
//! `ThingName` constraint `length 1..128, pattern ^[a-zA-Z0-9:_-]+$`, from
//! `smithy_query(query="shape",shape="ThingName")`), which instruction the update request
//! carries, and whether the name never existed or named a thing that was registered,
//! updated and then deleted.
//!
//! The split is pinned on BOTH sides of the family, over the operations it holds for: every
//! operation that READS a thing addressed by its `thingName` label states the qualified
//! sentence, and the write states the unqualified one AHEAD of every other refusal the same
//! request would earn — a `thingTypeName` naming no type, a contradictory thing-type
//! instruction, an `expectedVersion` that can match nothing — because the thing the request
//! addresses is what it is missing. The unqualified wording belongs to that MISS and not to
//! `UpdateThing`'s 404 in general: the same request against a REGISTERED thing is refused
//! with the thing-TYPE family's own account-qualified sentence.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region every request here is signed for. Neither refusal is regional, so no
/// assertion depends on it.
const REGION: &str = "ap-south-1";

/// The wire error shape of a thing this service does not hold, on both halves.
const NOT_FOUND: &str = "ResourceNotFoundException";

/// A SigV4 `Authorization` header whose credential scope names the `iot` signing name and
/// region: the scope is how the router picks the claiming service. The signature itself is
/// never checked here — the harness turns request authentication off, exactly as an
/// unauthenticated emulator runs.
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
    /// The error SHAPE a client resolves the refusal from: the `x-amzn-errortype` value up
    /// to the `:` that some of this service's error shapes qualify it with, which is the
    /// part an SDK matches the modelled error on.
    fn error_shape(&self) -> Option<&str> {
        self.error_code
            .as_deref()
            .map(|value| value.split(':').next().unwrap_or(value))
    }

    /// The error message the refusal carries, or the empty string when it carries none.
    fn message(&self) -> String {
        self.body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
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

    async fn create_thing(&self, thing_name: &str, request: &Value) -> Reply {
        self.call("POST", &format!("/things/{thing_name}"), Some(request))
            .await
    }

    async fn update_thing(&self, thing_name: &str, request: &Value) -> Reply {
        self.call("PATCH", &format!("/things/{thing_name}"), Some(request))
            .await
    }

    async fn describe_thing(&self, thing_name: &str) -> Reply {
        self.call("GET", &format!("/things/{thing_name}"), None).await
    }

    async fn delete_thing(&self, thing_name: &str) -> Reply {
        self.call("DELETE", &format!("/things/{thing_name}"), None)
            .await
    }
}

/// The names the table runs over: the shortest and the longest a `ThingName` admits
/// (`length 1..128`), and two of the forms its `pattern ^[a-zA-Z0-9:_-]+$` allows,
/// including the `:` and `_` punctuation — so no row depends on one generated name.
fn absent_names() -> Vec<String> {
    vec![
        "a".to_string(),
        "missing-thing".to_string(),
        "Missing:Scoped_Name-9".to_string(),
        "n".repeat(128),
    ]
}

/// The update instructions the table runs over: a request that changes nothing, one
/// carrying an attribute list, one removing the thing-type association, and one carrying
/// an `expectedVersion`. The name addresses no record at all, so every form must be
/// refused the same way and worded the same way.
fn update_requests() -> Vec<(&'static str, Value)> {
    vec![
        ("no instruction at all", json!({})),
        (
            "an attribute payload",
            json!({ "attributePayload": { "attributes": { "color": "blue" }, "merge": true } }),
        ),
        ("removeThingType", json!({ "removeThingType": true })),
        ("an expectedVersion", json!({ "expectedVersion": 1 })),
    ]
}

/// What a READ of a missing thing states: the family noun, the account the record would
/// belong to, `:`, the name the request asked about, and ` not found.`
fn read_wording(thing_name: &str) -> String {
    format!(
        "Thing {}:{thing_name} not found.",
        engine::emulator_account_id()
    )
}

/// What the WRITE of a missing thing record states: the family noun, the name the request
/// asked about, and ` not found.` — with no account id and no `:` separator.
fn write_wording(thing_name: &str) -> String {
    format!("Thing {thing_name} not found.")
}

/// What the thing-TYPE family states for a `thingTypeName` it does not hold: its own noun
/// as TWO words, the account the record would belong to, the name the request carried and
/// ` not found.` — the sentence an `UpdateThing` of an EXISTING thing earns, which is how
/// the unqualified wording above is shown to belong to the missing THING and not to this
/// operation's 404 in general.
fn missing_thing_type_wording(thing_type_name: &str) -> String {
    format!(
        "Thing Type {}:{thing_type_name} not found.",
        engine::emulator_account_id()
    )
}

/// The operations that READ the thing a `thingName` label addresses, each as the method
/// and path its model binds — `DescribeThing` `GET /things/{thingName}` and
/// `ListThingGroupsForThing` `GET /things/{thingName}/thing-groups`, from
/// `smithy_query(query="operation",operation="DescribeThing,ListThingGroupsForThing")`.
/// Both report on one thing record, so both state the family's READ sentence.
fn read_operations(thing_name: &str) -> Vec<(&'static str, String)> {
    vec![
        ("DescribeThing", format!("/things/{thing_name}")),
        (
            "ListThingGroupsForThing",
            format!("/things/{thing_name}/thing-groups"),
        ),
    ]
}

/// The `thingTypeName` the precedence table names: a type no case registers, so the
/// request carries an instruction the registry cannot carry out either.
const ABSENT_THING_TYPE: &str = "no-such-thing-type";

/// One row of the precedence table: an update instruction that earns a refusal OF ITS OWN
/// against a thing the registry holds, and what that refusal is — so the row shows that
/// a missing thing is answered with the write half's sentence ahead of it, and that the
/// unqualified wording is the missing THING's and not this operation's in general.
struct Precedence {
    /// What the request instructs, for the case label.
    instruction: &'static str,
    /// The `UpdateThing` request body.
    request: Value,
    /// The status a thing the registry HOLDS earns for this request.
    held_status: u16,
    /// The wire error shape it earns.
    held_code: &'static str,
    /// The message it carries, in that family's own wording.
    held_message: String,
}

/// The precedence table for `thing_name`, whose registered record sits at version 1: a
/// `thingTypeName` naming no thing type (that family's own 404), a request that both sets
/// and removes the association (`InvalidRequestException`, 400) and an `expectedVersion`
/// the record is not at (`VersionConflictException`, 409) — the three refusals
/// `UpdateThing` models besides the missing thing.
fn precedence_table(thing_name: &str) -> Vec<Precedence> {
    vec![
        Precedence {
            instruction: "a thingTypeName naming no type",
            request: json!({ "thingTypeName": ABSENT_THING_TYPE }),
            held_status: 404,
            held_code: NOT_FOUND,
            held_message: missing_thing_type_wording(ABSENT_THING_TYPE),
        },
        Precedence {
            instruction: "a contradictory thing-type instruction",
            request: json!({ "thingTypeName": ABSENT_THING_TYPE, "removeThingType": true }),
            held_status: 400,
            held_code: "InvalidRequestException",
            held_message: "Can not remove thing type and set thing type in the same request"
                .to_string(),
        },
        Precedence {
            instruction: "an expectedVersion the record is not at",
            request: json!({ "expectedVersion": 97 }),
            held_status: 409,
            held_code: "VersionConflictException",
            held_message: format!(
                "The version for thing {thing_name} does not match the expected version."
            ),
        },
    ]
}

#[tokio::test]
async fn an_update_of_an_unheld_thing_states_the_name_alone_where_a_read_qualifies_it() {
    for thing_name in absent_names() {
        for (instruction, request) in update_requests() {
            // A fresh emulator per row: a refusal must not depend on what another row
            // left behind, and nothing here registers the thing it asks about.
            let emulator = Emulator::new();
            let case = format!("UpdateThing({thing_name:?}) with {instruction}");

            let refused = emulator.update_thing(&thing_name, &request).await;
            assert_eq!(
                refused.status, 404,
                "case {case:?}: expected status 404, got {refused:?}"
            );
            assert_eq!(
                refused.error_code.as_deref(),
                Some(NOT_FOUND),
                "case {case:?}: expected x-amzn-errortype {NOT_FOUND:?}, got {refused:?}"
            );
            let expected = write_wording(&thing_name);
            assert_eq!(
                refused.message(),
                expected,
                "case {case:?}: expected message {expected:?}, got {:?}",
                refused.message()
            );

            // The other half of the same family, on the same name in the same store: a
            // READ qualifies it with the account, so the two wordings genuinely differ.
            let described = emulator.describe_thing(&thing_name).await;
            let expected_read = read_wording(&thing_name);
            assert_eq!(
                described.status, 404,
                "case {case:?}: the read of the same name must be 404, got {described:?}"
            );
            assert_eq!(
                described.message(),
                expected_read,
                "case {case:?}: expected the read's message {expected_read:?}, got {:?}",
                described.message()
            );
        }
    }
}

#[tokio::test]
async fn a_thing_registered_updated_and_deleted_is_worded_the_same_as_one_never_registered() {
    for thing_name in absent_names() {
        let emulator = Emulator::new();
        let case = format!("the deleted thing {thing_name:?}");

        let created = emulator
            .create_thing(
                &thing_name,
                &json!({ "attributePayload": { "attributes": { "env": "v1" } } }),
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: the fixture thing must register, got {created:?}"
        );

        // One successful update first, so the record this case deletes is one a caller
        // has written: the wording of the miss afterwards must not depend on that.
        let updated = emulator
            .update_thing(
                &thing_name,
                &json!({
                    "expectedVersion": 1,
                    "attributePayload": { "attributes": { "env": "v2" } },
                }),
            )
            .await;
        assert_eq!(
            updated.status, 200,
            "case {case:?}: the update of the registered thing must succeed, got {updated:?}"
        );

        let deleted = emulator.delete_thing(&thing_name).await;
        assert_eq!(
            deleted.status, 200,
            "case {case:?}: the delete must succeed, got {deleted:?}"
        );

        let described = emulator.describe_thing(&thing_name).await;
        let expected_read = read_wording(&thing_name);
        assert_eq!(
            described.status, 404,
            "case {case:?}: the deleted thing must be gone, got {described:?}"
        );
        assert_eq!(
            described.message(),
            expected_read,
            "case {case:?}: expected the read's message {expected_read:?}, got {:?}",
            described.message()
        );

        let refused = emulator
            .update_thing(
                &thing_name,
                &json!({ "attributePayload": { "attributes": { "env": "v3" } } }),
            )
            .await;
        let expected = write_wording(&thing_name);
        assert_eq!(
            refused.status, 404,
            "case {case:?}: updating the deleted thing must be 404, got {refused:?}"
        );
        assert_eq!(
            refused.error_code.as_deref(),
            Some(NOT_FOUND),
            "case {case:?}: expected x-amzn-errortype {NOT_FOUND:?}, got {refused:?}"
        );
        assert_eq!(
            refused.message(),
            expected,
            "case {case:?}: expected message {expected:?}, got {:?}",
            refused.message()
        );
    }
}

#[tokio::test]
async fn every_read_of_an_unheld_thing_qualifies_the_name_that_its_write_states_alone() {
    for thing_name in absent_names() {
        let emulator = Emulator::new();

        // The READ half, over each operation that reports on the thing a `thingName`
        // label addresses: one wording for the whole half, not one per operation.
        for (operation, path) in read_operations(&thing_name) {
            let case = format!("{operation}({thing_name:?})");
            let refused = emulator.call("GET", &path, None).await;
            assert_eq!(
                refused.status, 404,
                "case {case:?}: expected status 404, got {refused:?}"
            );
            assert_eq!(
                refused.error_shape(),
                Some(NOT_FOUND),
                "case {case:?}: expected error shape {NOT_FOUND:?}, got {refused:?}"
            );
            let expected = read_wording(&thing_name);
            assert_eq!(
                refused.message(),
                expected,
                "case {case:?}: expected message {expected:?}, got {:?}",
                refused.message()
            );
        }

        // The WRITE half on the same name in the same store states it unqualified, so the
        // two halves are genuinely different sentences for one missing record.
        let case = format!("UpdateThing({thing_name:?})");
        let refused = emulator.update_thing(&thing_name, &json!({})).await;
        let expected = write_wording(&thing_name);
        assert_eq!(
            refused.message(),
            expected,
            "case {case:?}: expected message {expected:?}, got {:?}",
            refused.message()
        );
    }
}

#[tokio::test]
async fn the_unheld_thing_is_worded_ahead_of_every_other_refusal_the_request_earns() {
    for thing_name in absent_names() {
        for row in precedence_table(&thing_name) {
            let emulator = Emulator::new();
            let case = format!("UpdateThing({thing_name:?}) with {}", row.instruction);

            // Nothing is registered, so whatever else the request instructs, the record it
            // addresses is what is missing: the thing family's write sentence.
            let refused = emulator.update_thing(&thing_name, &row.request).await;
            let expected = write_wording(&thing_name);
            assert_eq!(
                refused.status, 404,
                "case {case:?}: expected status 404, got {refused:?}"
            );
            assert_eq!(
                refused.error_shape(),
                Some(NOT_FOUND),
                "case {case:?}: expected error shape {NOT_FOUND:?}, got {refused:?}"
            );
            assert_eq!(
                refused.message(),
                expected,
                "case {case:?}: expected message {expected:?}, got {:?}",
                refused.message()
            );

            // The SAME request against a thing the registry does hold earns that row's own
            // refusal, in its own family's wording: the unqualified sentence above belongs
            // to the missing thing, not to this operation's refusals in general.
            let created = emulator.create_thing(&thing_name, &json!({})).await;
            assert_eq!(
                created.status, 200,
                "case {case:?}: the fixture thing must register, got {created:?}"
            );
            let held = emulator.update_thing(&thing_name, &row.request).await;
            assert_eq!(
                held.status, row.held_status,
                "case {case:?}: the registered thing must answer {}, got {held:?}",
                row.held_status
            );
            assert_eq!(
                held.error_shape(),
                Some(row.held_code),
                "case {case:?}: expected error shape {:?}, got {held:?}",
                row.held_code
            );
            assert_eq!(
                held.message(),
                row.held_message,
                "case {case:?}: expected message {:?}, got {:?}",
                row.held_message,
                held.message()
            );
        }
    }
}
