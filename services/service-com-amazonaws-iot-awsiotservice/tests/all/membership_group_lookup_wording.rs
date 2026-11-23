// SPDX-License-Identifier: CC0-1.0

//! How the operations that WRITE a thing's thing-group membership refuse a thing group
//! this account does not hold, end to end over HTTP.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a restJson1
//! request signed for the `iot` signing name — so what is asserted is the status, the
//! error shape and the `message` an SDK surfaces, not an internal function's return
//! value.
//!
//! The rule under test: the operations that write which groups a THING belongs to state a
//! missing thing group as the LOOKUP they make — fixed text saying the requested group
//! could not be found, followed by the two things that identify a group, the account the
//! call was made for and the name the request asked about — while the operations that
//! address the group RECORD itself keep the registry families' account-qualified
//! sentence. The refusal is the modelled `ResourceNotFoundException` (404) for all of
//! them.
//!
//! The table spans what the rule generalises over: the three operations that write the
//! relation (`AddThingToThingGroup`, `RemoveThingFromThingGroup` and both directions of
//! `UpdateThingGroupsForThing`), whether the group is named by name or by ARN, and the
//! form and length of the name — rather than one recorded request.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The account id every record this emulator holds belongs to
/// (`emulator_server::EMULATOR_ACCOUNT_ID`), which both sentences qualify a name with.
const ACCOUNT_ID: &str = "000000000000";
/// The region this suite signs for. Neither sentence is regional, so no assertion on a
/// message depends on it.
const REGION: &str = "ap-south-1";
/// The wire error shape of a resource this service does not hold.
const NOT_FOUND: &str = "ResourceNotFoundException";

/// One emulator: the shared engine plus a router with this service mounted.
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

    async fn dispatch(&self, method: &str, path: &str, body: Option<&Value>) -> Reply {
        let host = format!("iot.{REGION}.amazonaws.com");
        let payload = body.map_or_else(Bytes::new, |value| Bytes::from(value.to_string()));
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{path}"))
            .header("host", host)
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/iot/aws4_request, \
                     SignedHeaders=host, Signature=0123456789abcdef"
                ),
            )
            .body(payload)
            .expect("the membership request is well formed");
        Reply::of(self.router.dispatch(request).await)
    }

    async fn create_thing(&self, thing_name: &str) -> Reply {
        self.dispatch("POST", &format!("/things/{thing_name}"), Some(&json!({})))
            .await
    }

    async fn create_group(&self, thing_group_name: &str) -> Reply {
        self.dispatch(
            "POST",
            &format!("/thing-groups/{thing_group_name}"),
            Some(&json!({})),
        )
        .await
    }

    async fn describe_group(&self, thing_group_name: &str) -> Reply {
        self.dispatch("GET", &format!("/thing-groups/{thing_group_name}"), None)
            .await
    }

    async fn add_thing_to_group(&self, body: &Value) -> Reply {
        self.dispatch("PUT", "/thing-groups/addThingToThingGroup", Some(body))
            .await
    }

    async fn remove_thing_from_group(&self, body: &Value) -> Reply {
        self.dispatch("PUT", "/thing-groups/removeThingFromThingGroup", Some(body))
            .await
    }

    async fn update_thing_groups_for_thing(&self, body: &Value) -> Reply {
        self.dispatch("PUT", "/thing-groups/updateThingGroupsForThing", Some(body))
            .await
    }

    /// The groups the thing reports belonging to
    /// (`ListThingGroupsForThing`, `GET /things/{thingName}/thing-groups`).
    async fn groups_of(&self, case: &str, thing_name: &str) -> Vec<String> {
        let reply = self
            .dispatch("GET", &format!("/things/{thing_name}/thing-groups"), None)
            .await;
        assert_eq!(
            reply.status, 200,
            "case {case}: expected the listing to answer 200, got {} body {}",
            reply.status,
            reply.text()
        );
        reply.json()["thingGroups"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case}: no thingGroups in {}", reply.text()))
            .iter()
            .map(|group| group["groupName"].as_str().unwrap_or_default().to_string())
            .collect()
    }
}

/// A dispatched response, reduced to what the assertions read.
struct Reply {
    status: u16,
    body: Bytes,
}

impl Reply {
    fn of(response: http::Response<Bytes>) -> Self {
        Self {
            status: response.status().as_u16(),
            body: response.into_body(),
        }
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("response body is not JSON ({e}): {}", self.text()))
    }

    fn error_code(&self) -> String {
        self.json()["__type"]
            .as_str()
            .unwrap_or_else(|| panic!("no __type in {}", self.text()))
            .to_string()
    }

    fn error_message(&self) -> String {
        self.json()["message"]
            .as_str()
            .unwrap_or_else(|| panic!("no message in {}", self.text()))
            .to_string()
    }
}

fn thing_group_arn(thing_group_name: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:thinggroup/{thing_group_name}")
}

/// The sentence the MEMBERSHIP plane states a missing thing group with.
fn membership_missing_group_message(thing_group_name: &str) -> String {
    format!(
        "Cannot find requested thing group for accountId: {ACCOUNT_ID} and thing group name: \
         {thing_group_name}"
    )
}

/// The sentence the operations addressing the GROUP state the same miss with: the registry
/// families' account-qualified form, which is the other half of this family.
fn registry_missing_group_message(thing_group_name: &str) -> String {
    format!("ThingGroup {ACCOUNT_ID}:{thing_group_name} not found.")
}

/// `thingGroupName`s spanning the modelled class
/// (`smithy_query(query="operation",operation="AddThingToThingGroup")`: `ThingGroupName`,
/// length 1..128, pattern `^[a-zA-Z0-9:_-]+$`): the shortest name, an ordinary one, one of
/// each punctuation the pattern admits, and the longest.
fn missing_group_names() -> Vec<String> {
    vec![
        "a".to_string(),
        "fleet-alpha".to_string(),
        "Fleet_Group:2".to_string(),
        "g".repeat(128),
    ]
}

/// RULE: every operation that WRITES a thing's group membership states a group this
/// account does not hold in the membership plane's own sentence, whichever way the request
/// named the group.
#[tokio::test]
async fn the_membership_plane_states_a_missing_thing_group_in_its_own_words() {
    let emulator = Emulator::new();
    let thing_name = "sensor-a";
    assert_eq!(
        emulator.create_thing(thing_name).await.status,
        200,
        "the thing every case names must be creatable"
    );

    for thing_group_name in &missing_group_names() {
        let expected = membership_missing_group_message(thing_group_name);
        let rows: Vec<(&str, Reply)> = vec![
            (
                "AddThingToThingGroup naming the group by name",
                emulator
                    .add_thing_to_group(&json!({
                        "thingGroupName": thing_group_name,
                        "thingName": thing_name,
                    }))
                    .await,
            ),
            (
                "AddThingToThingGroup naming the group by ARN",
                emulator
                    .add_thing_to_group(&json!({
                        "thingGroupArn": thing_group_arn(thing_group_name),
                        "thingName": thing_name,
                    }))
                    .await,
            ),
            (
                "RemoveThingFromThingGroup naming the group by name",
                emulator
                    .remove_thing_from_group(&json!({
                        "thingGroupName": thing_group_name,
                        "thingName": thing_name,
                    }))
                    .await,
            ),
            (
                "RemoveThingFromThingGroup naming the group by ARN",
                emulator
                    .remove_thing_from_group(&json!({
                        "thingGroupArn": thing_group_arn(thing_group_name),
                        "thingName": thing_name,
                    }))
                    .await,
            ),
            (
                "UpdateThingGroupsForThing with a group to join",
                emulator
                    .update_thing_groups_for_thing(&json!({
                        "thingName": thing_name,
                        "thingGroupsToAdd": [thing_group_name],
                    }))
                    .await,
            ),
            (
                "UpdateThingGroupsForThing with a group to leave",
                emulator
                    .update_thing_groups_for_thing(&json!({
                        "thingName": thing_name,
                        "thingGroupsToRemove": [thing_group_name],
                    }))
                    .await,
            ),
        ];

        for (case, reply) in &rows {
            assert_eq!(
                (reply.status, reply.error_code(), reply.error_message()),
                (404, NOT_FOUND.to_string(), expected.clone()),
                "case {case} on the missing group {thing_group_name:?}: expected 404 \
                 {NOT_FOUND} {expected:?}, got {} {}",
                reply.status,
                reply.text()
            );
        }

        // The other half of this family: an operation addressing the GROUP ITSELF states
        // the same miss in the registry families' account-qualified sentence, so the two
        // halves are pinned apart here rather than collapsed into one wording.
        let described = emulator.describe_group(thing_group_name).await;
        let registry_expected = registry_missing_group_message(thing_group_name);
        assert_eq!(
            (
                described.status,
                described.error_code(),
                described.error_message()
            ),
            (404, NOT_FOUND.to_string(), registry_expected.clone()),
            "case DescribeThingGroup on the missing group {thing_group_name:?}: expected 404 \
             {NOT_FOUND} {registry_expected:?}, got {} {}",
            described.status,
            described.text()
        );
    }
}

/// RULE: the sentence above is the LOOKUP's and not the plane's — a group this account
/// does hold takes every one of those writes, and the membership it leaves is the one the
/// thing reports.
#[tokio::test]
async fn a_group_the_account_holds_takes_every_membership_write() {
    let emulator = Emulator::new();
    let thing_name = "sensor-b";
    assert_eq!(emulator.create_thing(thing_name).await.status, 200);

    // The same name classes the refusals above span, as groups that EXIST.
    for thing_group_name in &missing_group_names() {
        let created = emulator.create_group(thing_group_name).await;
        assert_eq!(
            created.status,
            200,
            "case creating {thing_group_name:?}: expected 200, got {} body {}",
            created.status,
            created.text()
        );

        for (case, reply) in [
            (
                "AddThingToThingGroup",
                emulator
                    .add_thing_to_group(&json!({
                        "thingGroupName": thing_group_name,
                        "thingName": thing_name,
                    }))
                    .await,
            ),
            (
                "UpdateThingGroupsForThing with a group to join",
                emulator
                    .update_thing_groups_for_thing(&json!({
                        "thingName": thing_name,
                        "thingGroupsToAdd": [thing_group_name],
                    }))
                    .await,
            ),
        ] {
            assert_eq!(
                reply.status,
                200,
                "case {case} on the held group {thing_group_name:?}: expected 200, got {} body {}",
                reply.status,
                reply.text()
            );
        }

        let groups = emulator.groups_of("the write is visible", thing_name).await;
        assert!(
            groups.contains(thing_group_name),
            "case the write is visible: expected {thing_group_name:?} among the thing's groups, \
             got {groups:?}"
        );

        let removed = emulator
            .remove_thing_from_group(&json!({
                "thingGroupArn": thing_group_arn(thing_group_name),
                "thingName": thing_name,
            }))
            .await;
        assert_eq!(
            removed.status,
            200,
            "case RemoveThingFromThingGroup on the held group {thing_group_name:?}: expected 200, \
             got {} body {}",
            removed.status,
            removed.text()
        );
        let groups = emulator
            .groups_of("the removal is visible", thing_name)
            .await;
        assert!(
            !groups.contains(thing_group_name),
            "case the removal is visible: expected {thing_group_name:?} gone from the thing's \
             groups, got {groups:?}"
        );
    }
}
