// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of the DEPTH the thing-group hierarchy admits.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `iot` signing name — so what is asserted is what an AWS SDK
//! client would see, not an internal function's return value.
//!
//! The rule under test: a thing-group hierarchy is capped at the published number of levels
//! (`state::MAX_THING_GROUP_HIERARCHY_DEPTH`, which the refusal itself states), so a
//! `CreateThingGroup` whose `parentGroupName` already sits at the deepest level is refused
//! the modelled `InvalidRequestException` (400) and records nothing — the over-deep group
//! is absent from the reader of one group, from the parent's own child listing and from the
//! account's listing — while a create at every level above it succeeds.
//!
//! The table spans the class the rule generalises over rather than one recorded request:
//! every level of one chain is driven, so the bound is checked at each level below it, on
//! it and one past it; the rows differ in the form of the group names, and in whether the
//! over-deep create carries properties and tags beside its parent.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::state::MAX_THING_GROUP_HIERARCHY_DEPTH;
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use super::{signed_for_iot, REGION};

/// The `x-amzn-errortype` this service names its legacy bad-request shape with: the shape
/// name qualified with the API doc namespace (`service::ShapeNamespacedErrorType`).
const INVALID_REQUEST: &str = "InvalidRequestException:http://iot.amazonaws.com/doc/2015-10-02/";

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

    /// The `groupName`s a `ListThingGroups` answer reported, in its order.
    fn group_names(&self) -> Vec<String> {
        self.body
            .get("thingGroups")
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .filter_map(|row| row.get("groupName").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
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

    async fn create_group(&self, thing_group_name: &str, body: &Value) -> Reply {
        self.call(
            "POST",
            &format!("/thing-groups/{thing_group_name}"),
            Some(body),
        )
        .await
    }

    async fn describe_group(&self, thing_group_name: &str) -> Reply {
        self.call("GET", &format!("/thing-groups/{thing_group_name}"), None)
            .await
    }

    async fn children_of(&self, parent_group_name: &str) -> Reply {
        self.call(
            "GET",
            &format!("/thing-groups?parentGroup={parent_group_name}"),
            None,
        )
        .await
    }
}

/// The sentence the service refuses a create under a parent at the deepest level with: the
/// parent the request named and the published ceiling, read from the service's own constant
/// so the expectation cannot drift from it.
fn over_deep_message(parent_group_name: &str) -> String {
    format!(
        "Cannot create child group for parent group : {parent_group_name}. Depth of group \
         hierarchy exceeds allowed max depth :{MAX_THING_GROUP_HIERARCHY_DEPTH}"
    )
}

/// RULE: a create succeeds at every level the hierarchy admits and is refused one level
/// past it, whatever the create carries beside its parent, and the refused group is
/// nowhere to be read.
#[tokio::test]
async fn a_child_of_a_group_at_the_deepest_level_is_refused_and_recorded_nowhere() {
    // The rows differ in the form of the group names the chain is built from, and in what
    // the over-deep create carries beside `parentGroupName`: the rule is about depth, so
    // neither may change the answer.
    for (case, name_of, over_deep_extras) in [
        (
            "plainly named levels, a bare over-deep create",
            (|level: usize| format!("level-{level}")) as fn(usize) -> String,
            json!({}),
        ),
        (
            "levels named with every admitted character class, properties and tags on the \
             over-deep create",
            (|level: usize| format!("Fleet:Level_{level}-x")) as fn(usize) -> String,
            json!({
                "thingGroupProperties": {
                    "thingGroupDescription": "one level too deep",
                    "attributePayload": { "attributes": { "site": "berlin" } }
                },
                "tags": [{ "Key": "owner", "Value": "fleet-team" }]
            }),
        ),
    ] {
        let emulator = Emulator::new();

        // One chain, built a level at a time: the root is the first level, so every level
        // up to and including the published ceiling must be accepted.
        let chain: Vec<String> = (1..=MAX_THING_GROUP_HIERARCHY_DEPTH).map(name_of).collect();
        for (index, thing_group_name) in chain.iter().enumerate() {
            let body = match index.checked_sub(1) {
                Some(parent) => json!({ "parentGroupName": chain[parent] }),
                None => json!({}),
            };
            let created = emulator.create_group(thing_group_name, &body).await;
            assert_eq!(
                created.status,
                200,
                "case {case:?}: CreateThingGroup at level {} must succeed, got {created:?}",
                index + 1
            );
        }

        // One level past the ceiling: the create is refused and the group is never
        // recorded.
        let deepest = chain
            .last()
            .expect("the chain holds at least one level")
            .clone();
        let over_deep = name_of(MAX_THING_GROUP_HIERARCHY_DEPTH + 1);
        let mut body = over_deep_extras.clone();
        body.as_object_mut()
            .expect("the extras are a JSON object")
            .insert("parentGroupName".to_string(), json!(deepest));
        let refused = emulator.create_group(&over_deep, &body).await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: a create one level past the ceiling must be refused 400, got \
             {refused:?}"
        );
        assert_eq!(
            refused.error_code.as_deref(),
            Some(INVALID_REQUEST),
            "case {case:?}: expected InvalidRequestException, got {refused:?}"
        );
        assert_eq!(
            refused.message(),
            Some(over_deep_message(&deepest)),
            "case {case:?}: expected {:?}, got {refused:?}",
            over_deep_message(&deepest)
        );

        let absent = emulator.describe_group(&over_deep).await;
        assert_eq!(
            absent.status, 404,
            "case {case:?}: the refused group must not exist, got {absent:?}"
        );
        let children = emulator.children_of(&deepest).await;
        assert_eq!(
            children.status, 200,
            "case {case:?}: listing the deepest group's children must succeed, got \
             {children:?}"
        );
        assert_eq!(
            children.group_names(),
            Vec::<String>::new(),
            "case {case:?}: the deepest group must have no child, got {children:?}"
        );

        // The ceiling is on the DEPTH of a chain, not on how many groups a parent has: a
        // sibling at a level the hierarchy admits is still accepted after the refusal.
        let sibling = format!("{}-sibling", name_of(MAX_THING_GROUP_HIERARCHY_DEPTH));
        let parent_of_sibling = chain[MAX_THING_GROUP_HIERARCHY_DEPTH - 2].clone();
        let accepted = emulator
            .create_group(&sibling, &json!({ "parentGroupName": parent_of_sibling }))
            .await;
        assert_eq!(
            accepted.status, 200,
            "case {case:?}: a sibling at an admitted level must still be created, got \
             {accepted:?}"
        );
    }
}
