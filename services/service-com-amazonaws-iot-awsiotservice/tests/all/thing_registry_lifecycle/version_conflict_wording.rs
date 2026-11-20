// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of the SENTENCE this registry states an `expectedVersion` that
//! does not match a thing's record with.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `iot` signing name — so what is asserted is what an AWS SDK
//! client would see, not an internal function's return value.
//!
//! The rule under test: `VersionConflictException` is "An exception thrown when the version
//! of an entity specified with the expectedVersion parameter does not match the latest
//! version in the system", and this family states it as ONE fixed sentence whose only
//! variable part is the thing the request addressed — neither the version the request asked
//! for nor the version the registry holds is part of it. Both operations that take an
//! `expectedVersion` for a thing state that one sentence, and a refused call leaves the
//! record and its version exactly as they were.
//!
//! The table spans the class the rule generalises over rather than one recorded request: the
//! stored version is reached by a different number of writes per row, the supplied version
//! sits below, above and at zero against it, the thing names take the forms and lengths
//! `ThingName` admits, and each row is driven through both operations.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use super::{signed_for_iot, REGION};

/// The `x-amzn-errortype` a modelled version conflict carries.
const VERSION_CONFLICT: &str = "VersionConflictException";

/// One response, reduced to what a client branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    error_code: Option<String>,
    body: Value,
}

impl Reply {
    /// The `message` member a modelled error carries.
    fn message(&self) -> Option<String> {
        self.body
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    /// The `version` a read of the thing reports.
    fn version(&self) -> Option<i64> {
        self.body.get("version").and_then(Value::as_i64)
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
            .header("authorization", signed_for_iot(REGION))
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

    async fn create_thing(&self, thing_name: &str) -> Reply {
        self.call("POST", &format!("/things/{thing_name}"), Some(&json!({})))
            .await
    }

    async fn describe_thing(&self, thing_name: &str) -> Reply {
        self.call("GET", &format!("/things/{thing_name}"), None).await
    }

    /// One write of the thing, which advances its registry version.
    async fn touch_thing(&self, thing_name: &str, marker: &str) -> Reply {
        self.call(
            "PATCH",
            &format!("/things/{thing_name}"),
            Some(&json!({ "attributePayload": { "attributes": { "marker": marker } } })),
        )
        .await
    }

    async fn update_thing_expecting(&self, thing_name: &str, expected_version: i64) -> Reply {
        self.call(
            "PATCH",
            &format!("/things/{thing_name}"),
            Some(&json!({ "expectedVersion": expected_version })),
        )
        .await
    }

    async fn delete_thing_expecting(&self, thing_name: &str, expected_version: i64) -> Reply {
        self.call(
            "DELETE",
            &format!("/things/{thing_name}?expectedVersion={expected_version}"),
            None,
        )
        .await
    }
}

/// The ONE sentence this family states a version conflict with.
fn version_conflict_message(thing_name: &str) -> String {
    format!("The version for thing {thing_name} does not match the expected version.")
}

/// RULE: a stale `expectedVersion` is refused 409 with the family's one fixed sentence —
/// the same from the update and from the delete — and the record survives untouched.
#[tokio::test]
async fn a_stale_expected_version_states_one_fixed_sentence_and_changes_nothing() {
    // `(case, thing name, writes after the create, the expectedVersion supplied)`. The
    // stored version is one plus the number of writes, so the rows put the supplied value
    // below it, above it and at zero.
    let rows: Vec<(&str, String, usize, i64)> = vec![
        (
            "one write behind the record",
            "sensor-one".to_string(),
            1,
            1,
        ),
        (
            "two writes behind the record",
            "Sensor:Two_9-x".to_string(),
            2,
            1,
        ),
        (
            "a version ahead of a freshly created record",
            "sensor-three".to_string(),
            0,
            7,
        ),
        ("a version of zero", "s".to_string(), 0, 0),
        (
            "a name at the modelled maximum length, one write behind",
            "n".repeat(128),
            1,
            1,
        ),
    ];

    for (case, thing_name, writes, expected_version) in rows {
        // Each operation gets its own emulator, so the delete half of a row cannot act on
        // a record the update half already changed.
        for operation in ["UpdateThing", "DeleteThing"] {
            let emulator = Emulator::new();
            let created = emulator.create_thing(&thing_name).await;
            assert_eq!(
                created.status, 200,
                "case {case:?} via {operation}: CreateThing must succeed, got {created:?}"
            );
            for write in 0..writes {
                let touched = emulator
                    .touch_thing(&thing_name, &format!("write-{write}"))
                    .await;
                assert_eq!(
                    touched.status, 200,
                    "case {case:?} via {operation}: UpdateThing must succeed, got {touched:?}"
                );
            }
            let stored_version = i64::try_from(writes).expect("the row's write count fits") + 1;
            let before = emulator.describe_thing(&thing_name).await;
            assert_eq!(
                before.version(),
                Some(stored_version),
                "case {case:?} via {operation}: expected the record at version \
                 {stored_version}, got {before:?}"
            );

            let refused = match operation {
                "UpdateThing" => {
                    emulator
                        .update_thing_expecting(&thing_name, expected_version)
                        .await
                }
                _ => {
                    emulator
                        .delete_thing_expecting(&thing_name, expected_version)
                        .await
                }
            };
            assert_eq!(
                refused.status, 409,
                "case {case:?} via {operation}: a stale expectedVersion must be refused 409, \
                 got {refused:?}"
            );
            assert_eq!(
                refused.error_code.as_deref(),
                Some(VERSION_CONFLICT),
                "case {case:?} via {operation}: expected VersionConflictException, got \
                 {refused:?}"
            );
            assert_eq!(
                refused.message(),
                Some(version_conflict_message(&thing_name)),
                "case {case:?} via {operation}: expected {:?}, got {refused:?}",
                version_conflict_message(&thing_name)
            );

            // A refused call wrote nothing: the thing is still there, at the version it
            // held before.
            let after = emulator.describe_thing(&thing_name).await;
            assert_eq!(
                after.status, 200,
                "case {case:?} via {operation}: a refused call must leave the thing, got \
                 {after:?}"
            );
            assert_eq!(
                after.version(),
                Some(stored_version),
                "case {case:?} via {operation}: a refused call must not advance the version, \
                 got {after:?}"
            );
        }
    }
}

/// RULE: the `expectedVersion` that DOES match the record is accepted, so the refusal above
/// is about the mismatch and not about the member being supplied at all.
#[tokio::test]
async fn the_matching_expected_version_is_accepted_by_both_operations() {
    for (case, writes) in [("a freshly created thing", 0), ("a thing written twice", 2)] {
        for operation in ["UpdateThing", "DeleteThing"] {
            let emulator = Emulator::new();
            let thing_name = "matching-version";
            let created = emulator.create_thing(thing_name).await;
            assert_eq!(
                created.status, 200,
                "case {case:?} via {operation}: CreateThing must succeed, got {created:?}"
            );
            for write in 0..writes {
                emulator
                    .touch_thing(thing_name, &format!("write-{write}"))
                    .await;
            }
            let stored_version = i64::from(writes) + 1;

            let accepted = match operation {
                "UpdateThing" => {
                    emulator
                        .update_thing_expecting(thing_name, stored_version)
                        .await
                }
                _ => {
                    emulator
                        .delete_thing_expecting(thing_name, stored_version)
                        .await
                }
            };
            assert_eq!(
                accepted.status, 200,
                "case {case:?} via {operation}: the matching expectedVersion must be \
                 accepted, got {accepted:?}"
            );

            let after = emulator.describe_thing(thing_name).await;
            let expected_status = if operation == "DeleteThing" { 404 } else { 200 };
            assert_eq!(
                after.status, expected_status,
                "case {case:?} via {operation}: expected the record to read \
                 {expected_status}, got {after:?}"
            );
        }
    }
}
