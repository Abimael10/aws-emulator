// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of `DeleteThingType` — the steps of the delete procedure the
//! operation documents — and of the sentence a `CreateThing` naming a DEPRECATED type is
//! refused with.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `iot` signing name — so what is asserted is what an AWS SDK
//! client would see, not an internal function's return value.
//!
//! The rules under test:
//!
//! * `DeleteThingType` removes the type only once every step of its documented procedure
//!   has been taken — "You cannot delete a thing type if it has things associated with it.
//!   To delete a thing type, first mark it as deprecated by calling `DeprecateThingType`,
//!   then remove any associated things … and finally use `DeleteThingType`" — and a
//!   deprecation has to be at least the service's published wait old. Each step that has
//!   not been taken is the modelled `InvalidRequestException` (400) stating that step, and
//!   the type survives the refusal exactly as it was; a name the account holds no type
//!   under is the modelled `ResourceNotFoundException` (404). A delete that goes through is
//!   observable: the reader of the type answers 404 and a `CreateThing` naming it no longer
//!   resolves.
//! * `ThingTypeMetadata$deprecated` is "If true, no new things could be associated with
//!   this type", and the refusal of such a `CreateThing` is one fixed sentence whose only
//!   variable part is the `thingTypeName` the request carried.
//!
//! Each table spans the class the rules generalise over — the form and length of a thing
//! type name, how many things are associated with the type, and where the wait boundary
//! falls (one instant short of it, exactly on it, and well past it) — rather than one
//! recorded request. The wait is driven by MOVING the stored deprecation instant back, so
//! no case waits on the clock.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::state::{service_state, DEPRECATION_DELETION_WAIT};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use super::{signed_for_iot, ACCOUNT_ID, REGION};

/// The `x-amzn-errortype` this service names its legacy bad-request shape with: the shape
/// name qualified with the API doc namespace (`service::ShapeNamespacedErrorType`).
const INVALID_REQUEST: &str = "InvalidRequestException:http://iot.amazonaws.com/doc/2015-10-02/";

/// The `x-amzn-errortype` a modelled not-found carries.
const NOT_FOUND: &str = "ResourceNotFoundException";

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

    /// `thingTypeMetadata.deprecated`. Absent is `false`: the member is modelled with
    /// `default false`, so a type nobody deprecated may report either form.
    fn deprecated(&self) -> bool {
        self.body
            .get("thingTypeMetadata")
            .and_then(|metadata| metadata.get("deprecated"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }
}

/// The emulator under test: ONE shared engine with the IoT service mounted on it, reached
/// the way the production router reaches it. The engine is kept beside the router so a case
/// can age a stored deprecation instead of waiting out the service's wait.
struct Emulator {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { router, engine }
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

    async fn create_type(&self, thing_type_name: &str) -> Reply {
        self.call(
            "POST",
            &format!("/thing-types/{thing_type_name}"),
            Some(&json!({})),
        )
        .await
    }

    async fn describe_type(&self, thing_type_name: &str) -> Reply {
        self.call("GET", &format!("/thing-types/{thing_type_name}"), None)
            .await
    }

    async fn deprecate_type(&self, thing_type_name: &str) -> Reply {
        self.call(
            "POST",
            &format!("/thing-types/{thing_type_name}/deprecate"),
            Some(&json!({})),
        )
        .await
    }

    async fn delete_type(&self, thing_type_name: &str) -> Reply {
        self.call("DELETE", &format!("/thing-types/{thing_type_name}"), None)
            .await
    }

    async fn create_thing(&self, thing_name: &str, thing_type_name: Option<&str>) -> Reply {
        let body = match thing_type_name {
            Some(thing_type_name) => json!({ "thingTypeName": thing_type_name }),
            None => json!({}),
        };
        self.call("POST", &format!("/things/{thing_name}"), Some(&body))
            .await
    }

    async fn remove_thing_type(&self, thing_name: &str) -> Reply {
        self.call(
            "PATCH",
            &format!("/things/{thing_name}"),
            Some(&json!({ "removeThingType": true })),
        )
        .await
    }

    /// Move the instant a stored thing type was DEPRECATED back by `age`, so a case can
    /// observe both sides of the post-deprecation wait without waiting for it. Nothing a
    /// request can reach does this; it stands in for minutes of clock.
    fn age_deprecation(&self, thing_type_name: &str, age: Duration) {
        let state = service_state(&*self.engine).expect("the IoT service store");
        let mut store = state.lock();
        let thing_type = store
            .thing_types
            .get_mut(thing_type_name)
            .unwrap_or_else(|| panic!("the store holds the thing type {thing_type_name:?}"));
        let deprecated_at = thing_type
            .deprecation_date
            .unwrap_or_else(|| panic!("the thing type {thing_type_name:?} carries a deprecation"));
        thing_type.deprecation_date = Some(
            deprecated_at
                .checked_sub(age)
                .expect("an instant that far back is representable"),
        );
    }
}

/// The sentence the service refuses the delete of a type nobody deprecated with.
fn not_deprecated_message(thing_type_name: &str) -> String {
    format!("Cannot delete non deprecated thing type {thing_type_name}")
}

/// The sentence the service refuses the delete of a type deprecated too recently with.
fn within_wait_message(thing_type_name: &str) -> String {
    format!(
        "Cannot delete thing type : {thing_type_name}. Please wait for 5 minutes after \
         deprecation and then retry"
    )
}

/// The sentence the service refuses the delete of a type things are still associated with.
fn associated_things_message(thing_type_name: &str) -> String {
    format!(
        "Cannot delete thing type : {thing_type_name}. There are things associated with this \
         thing type"
    )
}

/// The thing type names every table runs over: the forms and lengths `ThingTypeName`
/// admits (`len 1..128, pattern ^[a-zA-Z0-9:_-]+$`), so no case stands on one spelling.
/// `(case label, thing type name)`.
fn thing_type_names() -> Vec<(&'static str, String)> {
    vec![
        ("a single-character name", "t".to_string()),
        ("a hyphenated name", "sensor-type-1".to_string()),
        (
            "a name carrying every admitted character class",
            "Sensor:Type_9-x".to_string(),
        ),
        ("a name at the modelled maximum length", "n".repeat(128)),
    ]
}

/// RULE: a thing type is deleted only after it has been deprecated and the service's
/// post-deprecation wait has passed; each step that has not been taken is refused 400
/// stating that step, with the type left exactly as it was.
#[tokio::test]
async fn a_thing_type_is_deleted_only_after_a_deprecation_the_wait_has_passed() {
    for (case, thing_type_name) in thing_type_names() {
        // A fresh emulator per case, so what one case left behind cannot decide another.
        let emulator = Emulator::new();
        let created = emulator.create_type(&thing_type_name).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: CreateThingType must succeed, got {created:?}"
        );

        // Step one has not been taken: the type was never deprecated.
        let refused = emulator.delete_type(&thing_type_name).await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: deleting a type nobody deprecated must be refused 400, got \
             {refused:?}"
        );
        assert_eq!(
            refused.error_code.as_deref(),
            Some(INVALID_REQUEST),
            "case {case:?}: expected InvalidRequestException, got {refused:?}"
        );
        assert_eq!(
            refused.message(),
            Some(not_deprecated_message(&thing_type_name)),
            "case {case:?}: expected {:?}, got {refused:?}",
            not_deprecated_message(&thing_type_name)
        );
        let survived = emulator.describe_type(&thing_type_name).await;
        assert_eq!(
            survived.status, 200,
            "case {case:?}: a refused delete must leave the type, got {survived:?}"
        );
        assert!(
            !survived.deprecated(),
            "case {case:?}: a refused delete must not deprecate the type, got {survived:?}"
        );

        let deprecated = emulator.deprecate_type(&thing_type_name).await;
        assert_eq!(
            deprecated.status, 200,
            "case {case:?}: DeprecateThingType must succeed, got {deprecated:?}"
        );

        // The wait has not passed: a delete straight after the deprecation, and one an
        // instant short of the published wait, are both refused — the published bound is
        // read from the service's own constant rather than written out here.
        for short_of in [
            Duration::ZERO,
            DEPRECATION_DELETION_WAIT - Duration::from_secs(1),
        ] {
            emulator.age_deprecation(&thing_type_name, short_of);
            let too_soon = emulator.delete_type(&thing_type_name).await;
            assert_eq!(
                too_soon.status, 400,
                "case {case:?} aged {short_of:?}: a delete inside the wait must be refused \
                 400, got {too_soon:?}"
            );
            assert_eq!(
                too_soon.error_code.as_deref(),
                Some(INVALID_REQUEST),
                "case {case:?} aged {short_of:?}: expected InvalidRequestException, got \
                 {too_soon:?}"
            );
            assert_eq!(
                too_soon.message(),
                Some(within_wait_message(&thing_type_name)),
                "case {case:?} aged {short_of:?}: expected {:?}, got {too_soon:?}",
                within_wait_message(&thing_type_name)
            );
            let still_there = emulator.describe_type(&thing_type_name).await;
            assert_eq!(
                still_there.status, 200,
                "case {case:?} aged {short_of:?}: a refused delete must leave the type, got \
                 {still_there:?}"
            );
            assert!(
                still_there.deprecated(),
                "case {case:?} aged {short_of:?}: the type must still report deprecated, got \
                 {still_there:?}"
            );
        }

        // Exactly the published wait is enough, so the delete goes through and the type
        // stops existing: the reader answers 404 and a new thing can no longer name it.
        emulator.age_deprecation(&thing_type_name, DEPRECATION_DELETION_WAIT);
        let deleted = emulator.delete_type(&thing_type_name).await;
        assert_eq!(
            deleted.status, 200,
            "case {case:?}: a delete past the wait must succeed, got {deleted:?}"
        );
        let gone = emulator.describe_type(&thing_type_name).await;
        assert_eq!(
            gone.status, 404,
            "case {case:?}: a deleted type must read 404, got {gone:?}"
        );
        assert_eq!(
            gone.error_code.as_deref(),
            Some(NOT_FOUND),
            "case {case:?}: expected ResourceNotFoundException, got {gone:?}"
        );
        let orphan = emulator
            .create_thing("thing-after-delete", Some(&thing_type_name))
            .await;
        assert_eq!(
            orphan.status, 404,
            "case {case:?}: CreateThing naming a deleted type must read 404, got {orphan:?}"
        );
    }
}

/// RULE: a thing type things are still associated with is not deleted; once every thing
/// has stopped naming it, the same delete goes through.
#[tokio::test]
async fn a_thing_type_things_are_associated_with_is_not_deleted() {
    // The rows differ in HOW MANY things are associated and in how the association is
    // ended — by removing the type from the thing, and by deleting the thing itself.
    for (case, thing_names, detach_by_delete) in [
        ("one associated thing, detached", vec!["sensor-a"], false),
        (
            "three associated things, detached",
            vec!["sensor-b", "sensor-c", "sensor-d"],
            false,
        ),
        ("two associated things, deleted", vec!["sensor-e", "sensor-f"], true),
    ] {
        let emulator = Emulator::new();
        let thing_type_name = "fleet-sensor";
        let created = emulator.create_type(thing_type_name).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: CreateThingType must succeed, got {created:?}"
        );
        for thing_name in &thing_names {
            let thing = emulator.create_thing(thing_name, Some(thing_type_name)).await;
            assert_eq!(
                thing.status, 200,
                "case {case:?}: CreateThing {thing_name} must succeed, got {thing:?}"
            );
        }
        let deprecated = emulator.deprecate_type(thing_type_name).await;
        assert_eq!(
            deprecated.status, 200,
            "case {case:?}: DeprecateThingType must succeed, got {deprecated:?}"
        );
        emulator.age_deprecation(thing_type_name, DEPRECATION_DELETION_WAIT);

        let refused = emulator.delete_type(thing_type_name).await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: deleting a type with things on it must be refused 400, got \
             {refused:?}"
        );
        assert_eq!(
            refused.error_code.as_deref(),
            Some(INVALID_REQUEST),
            "case {case:?}: expected InvalidRequestException, got {refused:?}"
        );
        assert_eq!(
            refused.message(),
            Some(associated_things_message(thing_type_name)),
            "case {case:?}: expected {:?}, got {refused:?}",
            associated_things_message(thing_type_name)
        );
        let survived = emulator.describe_type(thing_type_name).await;
        assert_eq!(
            survived.status, 200,
            "case {case:?}: a refused delete must leave the type, got {survived:?}"
        );

        // Every thing stops naming the type — one half of the table by dropping the
        // association, the other by deleting the thing — and the delete then goes through.
        for thing_name in &thing_names {
            let detached = if detach_by_delete {
                emulator
                    .call("DELETE", &format!("/things/{thing_name}"), None)
                    .await
            } else {
                emulator.remove_thing_type(thing_name).await
            };
            assert_eq!(
                detached.status, 200,
                "case {case:?}: detaching {thing_name} must succeed, got {detached:?}"
            );
        }

        let deleted = emulator.delete_type(thing_type_name).await;
        assert_eq!(
            deleted.status, 200,
            "case {case:?}: a delete with no associated thing left must succeed, got \
             {deleted:?}"
        );
        let gone = emulator.describe_type(thing_type_name).await;
        assert_eq!(
            gone.status, 404,
            "case {case:?}: a deleted type must read 404, got {gone:?}"
        );
    }
}

/// RULE: a name the account holds no thing type under is the registry family's not-found,
/// which states the account the lookup was scoped to.
#[tokio::test]
async fn deleting_a_thing_type_the_account_does_not_hold_is_a_not_found() {
    let emulator = Emulator::new();
    // One type exists, so a row that differs from it only by case or by a suffix is still
    // a miss: the lookup is by the exact name.
    let existing = emulator.create_type("sensor-type").await;
    assert_eq!(
        existing.status, 200,
        "the fixture thing type must be created, got {existing:?}"
    );

    for (case, thing_type_name) in [
        ("a name nobody created", "no-such-type".to_string()),
        ("a name differing only by case", "Sensor-Type".to_string()),
        ("a name differing by a suffix", "sensor-type-2".to_string()),
        ("a single-character name", "z".to_string()),
        ("a name at the modelled maximum length", "m".repeat(128)),
    ] {
        let refused = emulator.delete_type(&thing_type_name).await;
        assert_eq!(
            refused.status, 404,
            "case {case:?}: deleting a type the account does not hold must be refused 404, \
             got {refused:?}"
        );
        assert_eq!(
            refused.error_code.as_deref(),
            Some(NOT_FOUND),
            "case {case:?}: expected ResourceNotFoundException, got {refused:?}"
        );
        assert_eq!(
            refused.message(),
            Some(format!("Thing Type {ACCOUNT_ID}:{thing_type_name} not found.")),
            "case {case:?}: expected the registry family's not-found sentence, got {refused:?}"
        );
    }
}

/// RULE: `CreateThing` naming a DEPRECATED thing type is refused 400 with one fixed
/// sentence whose only variable part is the `thingTypeName` the request carried.
#[tokio::test]
async fn creating_a_thing_with_a_deprecated_thing_type_states_one_fixed_sentence() {
    let emulator = Emulator::new();

    for (index, (case, thing_type_name)) in thing_type_names().into_iter().enumerate() {
        let created = emulator.create_type(&thing_type_name).await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: CreateThingType must succeed, got {created:?}"
        );
        let deprecated = emulator.deprecate_type(&thing_type_name).await;
        assert_eq!(
            deprecated.status, 200,
            "case {case:?}: DeprecateThingType must succeed, got {deprecated:?}"
        );

        let refused = emulator
            .create_thing(&format!("thing-{index}"), Some(&thing_type_name))
            .await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: CreateThing naming a deprecated type must be refused 400, got \
             {refused:?}"
        );
        assert_eq!(
            refused.error_code.as_deref(),
            Some(INVALID_REQUEST),
            "case {case:?}: expected InvalidRequestException, got {refused:?}"
        );
        let expected = format!(
            "Can not create new thing with depreated thing type:{thing_type_name}"
        );
        assert_eq!(
            refused.message(),
            Some(expected.clone()),
            "case {case:?}: expected {expected:?}, got {refused:?}"
        );

        // The refused create recorded nothing: the thing it named does not exist.
        let absent = emulator
            .call("GET", &format!("/things/thing-{index}"), None)
            .await;
        assert_eq!(
            absent.status, 404,
            "case {case:?}: a refused CreateThing must register no thing, got {absent:?}"
        );
    }
}
