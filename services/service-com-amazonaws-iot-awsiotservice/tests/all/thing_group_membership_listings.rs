// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of THING GROUP MEMBERSHIP as its READERS see it:
//! `AddThingToThingGroup` and `RemoveThingFromThingGroup` write the relation, and
//! `ListThingsInThingGroup` and `ListThingGroupsForThing` are the two views of it.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a
//! restJson1 request signed for the `iot` signing name — so what is asserted is what an
//! AWS SDK client would see, not an internal function's return value.
//!
//! The rules under test:
//!
//! * a membership that was WRITTEN is observable from both ends: the thing is in the
//!   group's listing and the group is in the thing's listing, and after the removal it
//!   is in neither — one relation, two views, which cannot disagree;
//! * either end may be named by NAME or by ARN, since the name inside the ARN is the
//!   resource's identity;
//! * a listing of a thing the registry does not hold is `ResourceNotFoundException`
//!   (404) rather than an empty page, a removal naming an end the account does not hold
//!   is the same 404, a DYNAMIC group's membership is its query's so a removal from one
//!   is `InvalidRequestException` (400), and removing a thing that is not in the group
//!   removes nothing and succeeds;
//! * the listing is `@paginated`, so `maxResults` pages it and its own `nextToken`
//!   resumes it.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region this suite signs for; the ARN assertions read it back.
const REGION: &str = "us-east-2";

/// One emulator: a router with this service mounted on its own engine.
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

    /// Register one thing, answering its ARN.
    async fn create_thing(&self, case: &str, thing_name: &str) -> String {
        let reply = self
            .dispatch("POST", &format!("/things/{thing_name}"), Some(&json!({})))
            .await;
        assert_eq!(
            reply.status,
            200,
            "case {case}: expected CreateThing to answer 200, got {} body {}",
            reply.status,
            reply.text()
        );
        reply.json()["thingArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: no thingArn in {}", reply.text()))
            .to_string()
    }

    /// Create one static thing group, answering its ARN.
    async fn create_group(&self, case: &str, group_name: &str) -> String {
        let reply = self
            .dispatch(
                "POST",
                &format!("/thing-groups/{group_name}"),
                Some(&json!({})),
            )
            .await;
        assert_eq!(
            reply.status,
            200,
            "case {case}: expected CreateThingGroup to answer 200, got {} body {}",
            reply.status,
            reply.text()
        );
        reply.json()["thingGroupArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: no thingGroupArn in {}", reply.text()))
            .to_string()
    }

    /// Create one dynamic thing group, whose membership is its query's.
    ///
    /// A dynamic group is a view of the THING fleet index, and AWS refuses the create
    /// while that index is not enabled, so the index is enabled here first
    /// (`UpdateIndexingConfiguration`, `POST /indexing/config`) — once per group, which is
    /// idempotent because the configuration is one record for the account.
    async fn create_dynamic_group(&self, case: &str, group_name: &str) {
        let enabled = self
            .dispatch(
                "POST",
                "/indexing/config",
                Some(&json!({
                    "thingIndexingConfiguration": { "thingIndexingMode": "REGISTRY" },
                })),
            )
            .await;
        assert_eq!(
            enabled.status,
            200,
            "case {case}: expected UpdateIndexingConfiguration to answer 200, got {} body {}",
            enabled.status,
            enabled.text()
        );
        let reply = self
            .dispatch(
                "POST",
                &format!("/dynamic-thing-groups/{group_name}"),
                Some(&json!({ "queryString": "thingName:*" })),
            )
            .await;
        assert_eq!(
            reply.status,
            200,
            "case {case}: expected CreateDynamicThingGroup to answer 200, got {} body {}",
            reply.status,
            reply.text()
        );
    }

    async fn add_membership(&self, body: &Value) -> Reply {
        self.dispatch("PUT", "/thing-groups/addThingToThingGroup", Some(body))
            .await
    }

    async fn remove_membership(&self, body: &Value) -> Reply {
        self.dispatch("PUT", "/thing-groups/removeThingFromThingGroup", Some(body))
            .await
    }

    async fn things_in_group(&self, group_name: &str) -> Reply {
        self.dispatch("GET", &format!("/thing-groups/{group_name}/things"), None)
            .await
    }

    async fn groups_for_thing(&self, thing_name: &str) -> Reply {
        self.dispatch("GET", &format!("/things/{thing_name}/thing-groups"), None)
            .await
    }

    async fn groups_for_thing_page(&self, thing_name: &str, query: &str) -> Reply {
        self.dispatch(
            "GET",
            &format!("/things/{thing_name}/thing-groups?{query}"),
            None,
        )
        .await
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

    /// The thing names a `ListThingsInThingGroup` page carries.
    fn things(&self) -> Vec<String> {
        self.json()["things"]
            .as_array()
            .unwrap_or_else(|| panic!("no things in {}", self.text()))
            .iter()
            .map(|thing| thing.as_str().unwrap_or_default().to_string())
            .collect()
    }

    /// The group names a `ListThingGroupsForThing` page carries.
    fn group_names(&self) -> Vec<String> {
        self.json()["thingGroups"]
            .as_array()
            .unwrap_or_else(|| panic!("no thingGroups in {}", self.text()))
            .iter()
            .map(|group| group["groupName"].as_str().unwrap_or_default().to_string())
            .collect()
    }

    /// The group ARNs it carries, beside those names.
    fn group_arns(&self) -> Vec<String> {
        self.json()["thingGroups"]
            .as_array()
            .unwrap_or_else(|| panic!("no thingGroups in {}", self.text()))
            .iter()
            .map(|group| group["groupArn"].as_str().unwrap_or_default().to_string())
            .collect()
    }
}

/// RULE: a written membership is observable from both ends, and the removal takes it
/// out of both.
#[tokio::test]
async fn a_written_membership_is_read_back_from_both_ends() {
    // The rows span what the rule generalises over: how each end is NAMED (by name or
    // by ARN), how many groups one thing is in, and how many things one group holds.
    struct Case {
        label: &'static str,
        group: &'static str,
        thing: &'static str,
        by_arn: bool,
    }
    let cases = [
        Case {
            label: "both ends by name",
            group: "line-a",
            thing: "sensor-1",
            by_arn: false,
        },
        Case {
            label: "both ends by ARN",
            group: "line-a",
            thing: "sensor-2",
            by_arn: true,
        },
        Case {
            label: "a second group for the same thing, by name",
            group: "line-b",
            thing: "sensor-1",
            by_arn: false,
        },
        Case {
            label: "a third group for the same thing, by ARN",
            group: "line-c",
            thing: "sensor-1",
            by_arn: true,
        },
    ];

    let emulator = Emulator::new();
    let mut arns: Vec<(String, String)> = Vec::new();
    for case in &cases {
        if !arns.iter().any(|(name, _arn)| name == case.thing) {
            let arn = emulator.create_thing(case.label, case.thing).await;
            arns.push((case.thing.to_string(), arn));
        }
        if !arns.iter().any(|(name, _arn)| name == case.group) {
            let arn = emulator.create_group(case.label, case.group).await;
            arns.push((case.group.to_string(), arn));
        }
    }
    let arn_of = |name: &str| -> String {
        arns.iter()
            .find(|(held, _arn)| held == name)
            .map(|(_held, arn)| arn.clone())
            .unwrap_or_else(|| panic!("no ARN recorded for {name}"))
    };

    for case in &cases {
        let body = if case.by_arn {
            json!({ "thingGroupArn": arn_of(case.group), "thingArn": arn_of(case.thing) })
        } else {
            json!({ "thingGroupName": case.group, "thingName": case.thing })
        };
        let added = emulator.add_membership(&body).await;
        assert_eq!(
            added.status,
            200,
            "case {}: expected AddThingToThingGroup to answer 200, got {} body {}",
            case.label,
            added.status,
            added.text()
        );

        // The group's own listing carries the thing...
        let things = emulator.things_in_group(case.group).await;
        assert!(
            things.things().contains(&case.thing.to_string()),
            "case {}: expected {} among the group's things, got {:?}",
            case.label,
            case.thing,
            things.things()
        );
        // ...and the thing's own listing carries the group, with the ARN the create
        // minted for it.
        let groups = emulator.groups_for_thing(case.thing).await;
        assert!(
            groups.group_names().contains(&case.group.to_string()),
            "case {}: expected {} among the thing's groups, got {:?}",
            case.label,
            case.group,
            groups.group_names()
        );
        assert!(
            groups.group_arns().contains(&arn_of(case.group)),
            "case {}: expected the group's minted ARN in the listing, got {:?}",
            case.label,
            groups.group_arns()
        );
    }

    // One group holding several things reports each of them.
    let line_a = emulator.things_in_group("line-a").await.things();
    assert_eq!(
        line_a,
        vec!["sensor-1".to_string(), "sensor-2".to_string()],
        "case a group holding two things: expected both, got {line_a:?}"
    );

    // The removal takes the membership out of both views, whichever way its ends are
    // named, and leaves every other membership of the same thing standing.
    let removed = emulator
        .remove_membership(&json!({
            "thingGroupArn": arn_of("line-b"),
            "thingName": "sensor-1",
        }))
        .await;
    assert_eq!(
        removed.status,
        200,
        "case the removal: expected 200, got {} body {}",
        removed.status,
        removed.text()
    );
    let groups = emulator.groups_for_thing("sensor-1").await.group_names();
    assert_eq!(
        groups,
        vec!["line-a".to_string(), "line-c".to_string()],
        "case the removal: expected the removed group gone and the others kept, got {groups:?}"
    );
    let things = emulator.things_in_group("line-b").await.things();
    assert!(
        things.is_empty(),
        "case the removal: expected the group to hold nothing, got {things:?}"
    );
}

/// RULE: the membership operations answer about the records the account holds — a
/// missing end is a 404, a dynamic group's membership is not a caller's to remove, and
/// removing what is not there removes nothing and succeeds.
#[tokio::test]
async fn the_membership_refusals_name_the_rule_each_request_broke() {
    let emulator = Emulator::new();
    emulator.create_thing("setup", "sensor-1").await;
    emulator.create_group("setup", "line-a").await;
    emulator.create_dynamic_group("setup", "derived").await;

    // The rows span the ways a request can name something the relation cannot be
    // written or read over: an absent thing, an absent group, a dynamic group, and a
    // membership that was never written.
    struct Refusal {
        label: &'static str,
        reply: Reply,
        status: u16,
        code: &'static str,
    }
    let refusals = vec![
        Refusal {
            label: "a listing of a thing the registry does not hold",
            reply: emulator.groups_for_thing("absent-thing").await,
            status: 404,
            code: "ResourceNotFoundException",
        },
        Refusal {
            label: "a removal naming a thing the registry does not hold",
            reply: emulator
                .remove_membership(
                    &json!({ "thingGroupName": "line-a", "thingName": "absent-thing" }),
                )
                .await,
            status: 404,
            code: "ResourceNotFoundException",
        },
        Refusal {
            label: "a removal naming a group the account does not hold",
            reply: emulator
                .remove_membership(
                    &json!({ "thingGroupName": "absent-group", "thingName": "sensor-1" }),
                )
                .await,
            status: 404,
            code: "ResourceNotFoundException",
        },
        Refusal {
            label: "a removal from a dynamic group",
            reply: emulator
                .remove_membership(&json!({ "thingGroupName": "derived", "thingName": "sensor-1" }))
                .await,
            status: 400,
            code: "InvalidRequestException",
        },
        Refusal {
            label: "a removal naming neither end",
            reply: emulator.remove_membership(&json!({})).await,
            status: 400,
            code: "InvalidRequestException",
        },
    ];
    for refusal in &refusals {
        assert_eq!(
            refusal.reply.status, refusal.status,
            "case {}: expected {}, got {} body {}",
            refusal.label,
            refusal.status,
            refusal.reply.status,
            refusal.reply.text()
        );
        assert_eq!(
            refusal.reply.error_code(),
            refusal.code,
            "case {}: got {}",
            refusal.label,
            refusal.reply.text()
        );
    }

    // A removal of a membership that was never written removes nothing and succeeds,
    // and the listing still reports the thing in no group.
    let repeated = emulator
        .remove_membership(&json!({ "thingGroupName": "line-a", "thingName": "sensor-1" }))
        .await;
    assert_eq!(
        repeated.status,
        200,
        "case a removal of a membership that was never written: expected 200, got {} body {}",
        repeated.status,
        repeated.text()
    );
    let groups = emulator.groups_for_thing("sensor-1").await;
    assert!(
        groups.group_names().is_empty(),
        "case a thing in no group: expected an empty listing, got {:?}",
        groups.group_names()
    );
}

/// RULE: the listing of a thing's groups is paginated, so `maxResults` bounds a page
/// and the token it answers with resumes exactly where that page stopped.
#[tokio::test]
async fn the_thing_group_listing_pages_over_the_memberships() {
    let emulator = Emulator::new();
    emulator.create_thing("paging", "sensor-1").await;
    let group_names = ["line-a", "line-b", "line-c"];
    for group_name in group_names {
        emulator.create_group("paging", group_name).await;
        let added = emulator
            .add_membership(&json!({ "thingGroupName": group_name, "thingName": "sensor-1" }))
            .await;
        assert_eq!(
            added.status,
            200,
            "case paging: the membership must be writable, got {} body {}",
            added.status,
            added.text()
        );
    }

    // The rows span the page sizes the rule generalises over: one group at a time, two,
    // and more than the thing belongs to.
    for page_size in [1usize, 2, 10] {
        let mut seen: Vec<String> = Vec::new();
        let mut query = format!("maxResults={page_size}");
        loop {
            let page = emulator.groups_for_thing_page("sensor-1", &query).await;
            assert_eq!(
                page.status,
                200,
                "case maxResults={page_size}: expected 200, got {} body {}",
                page.status,
                page.text()
            );
            let names = page.group_names();
            assert!(
                names.len() <= page_size,
                "case maxResults={page_size}: a page must hold at most {page_size}, got {names:?}"
            );
            seen.extend(names);
            match page.json()["nextToken"].as_str() {
                Some(token) => query = format!("maxResults={page_size}&nextToken={token}"),
                None => break,
            }
        }
        assert_eq!(
            seen,
            group_names
                .iter()
                .map(|name| (*name).to_string())
                .collect::<Vec<String>>(),
            "case maxResults={page_size}: expected every group once, in order, got {seen:?}"
        );
    }

    // A token this listing did not issue is refused rather than silently restarted.
    let refused = emulator
        .groups_for_thing_page("sensor-1", "nextToken=not-a-token")
        .await;
    assert_eq!(
        refused.status,
        400,
        "case a token the listing did not issue: expected 400, got {} body {}",
        refused.status,
        refused.text()
    );
}
