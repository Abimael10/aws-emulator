// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of thing group MEMBERSHIP and of DYNAMIC thing groups:
//! `AddThingToThingGroup`, the `ListThingsInThingGroup` reader that proves what it
//! persisted, `CreateDynamicThingGroup` with the `DescribeThingGroup` reader, and
//! `DeleteDynamicThingGroup`.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a
//! restJson1 request signed for the `iot` signing name — so what is asserted is what
//! an AWS SDK client would see, not an internal function's return value.
//!
//! The rules under test:
//!
//! * a membership is a relation between records that EXIST, written by naming each
//!   end either by name or by ARN: a missing end is `ResourceNotFoundException` (404),
//!   a group whose membership is its query's is `InvalidRequestException` (400), and a
//!   repeat of the same add leaves the same one membership;
//! * a thing belongs to at most the published number of thing groups: the add past that
//!   bound is `InvalidRequestException` (400) and writes nothing, whatever
//!   `overrideDynamicGroups` asks for, and the bound counts the memberships the thing
//!   holds at that moment;
//! * a group's members are what the group's own reader reports, and a RECURSIVE
//!   listing is the union of the group's members and those of every group below it;
//! * a job targeting a thing group runs on the group's MEMBERS: each gets one
//!   execution, and a group the registry does not hold is a 404;
//! * a dynamic thing group IS its query: an unparsable `queryString` is
//!   `InvalidQueryException` (400) with nothing created, and a created one reports the
//!   query that decides its membership;
//! * a member the model documents a default for — the index a dynamic group is over
//!   and the version its query is read under — is recorded at that default when the
//!   request omits it, and an `indexName` outside the recognised set is refused;
//! * a BULK update of the groups one thing belongs to
//!   (`UpdateThingGroupsForThing`) leaves the thing in exactly the groups it belonged
//!   to, minus the ones it left, plus the ones it joined, under the same rules one add
//!   or one removal follows — and a refused update applies none of itself.
//!
//! Each table spans what the rule generalises over — which end is named by name and
//! which by ARN, how many members a group has, how deep the hierarchy goes, and the
//! form of a query string — rather than one recorded request.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::state::{
    DEFAULT_DYNAMIC_GROUP_QUERY_VERSION, MAX_THING_GROUPS_PER_THING, UNPARSABLE_QUERY_MESSAGE,
};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The account id every emulated ARN carries (`emulator_server::EMULATOR_ACCOUNT_ID`).
const ACCOUNT_ID: &str = "000000000000";
/// The region this suite signs for.
const REGION: &str = "us-east-2";

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
            .expect("the thing-group request is well formed");
        Reply::of(self.router.dispatch(request).await)
    }

    async fn create_thing(&self, thing_name: &str) -> Reply {
        self.dispatch("POST", &format!("/things/{thing_name}"), Some(&json!({})))
            .await
    }

    async fn create_group(&self, group_name: &str, body: &Value) -> Reply {
        self.dispatch("POST", &format!("/thing-groups/{group_name}"), Some(body))
            .await
    }

    async fn create_dynamic_group(&self, group_name: &str, body: &Value) -> Reply {
        self.dispatch(
            "POST",
            &format!("/dynamic-thing-groups/{group_name}"),
            Some(body),
        )
        .await
    }

    async fn delete_dynamic_group(&self, group_name: &str) -> Reply {
        self.dispatch("DELETE", &format!("/dynamic-thing-groups/{group_name}"), None)
            .await
    }

    async fn describe_group(&self, group_name: &str) -> Reply {
        self.dispatch("GET", &format!("/thing-groups/{group_name}"), None)
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

    /// The groups a thing reports belonging to, in the order the listing reported them
    /// (`ListThingGroupsForThing`, `GET /things/{thingName}/thing-groups`).
    async fn groups_of(&self, case: &str, thing_name: &str) -> Vec<String> {
        let reply = self
            .dispatch("GET", &format!("/things/{thing_name}/thing-groups"), None)
            .await;
        assert_eq!(
            reply.status,
            200,
            "case {case}: expected the listing to answer 200, got {} body {}",
            reply.status,
            reply.text()
        );
        reply.json()["thingGroups"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case}: no thingGroups in {}", reply.text()))
            .iter()
            .map(|group| {
                group["groupName"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect()
    }

    async fn list_things_in_group(&self, group_name: &str, query: &str) -> Reply {
        self.dispatch(
            "GET",
            &format!("/thing-groups/{group_name}/things{query}"),
            None,
        )
        .await
    }

    async fn create_job(&self, job_id: &str, body: &Value) -> Reply {
        self.dispatch("PUT", &format!("/jobs/{job_id}"), Some(body))
            .await
    }

    async fn list_job_executions_for_thing(&self, thing_name: &str) -> Reply {
        self.dispatch("GET", &format!("/things/{thing_name}/jobs"), None)
            .await
    }

    /// The things a group reports, in the order the listing reported them.
    async fn things_in(&self, case: &str, group_name: &str, query: &str) -> Vec<String> {
        let reply = self.list_things_in_group(group_name, query).await;
        assert_eq!(
            reply.status,
            200,
            "case {case}: expected the listing to answer 200, got {} body {}",
            reply.status,
            reply.text()
        );
        reply.json()["things"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case}: no things in {}", reply.text()))
            .iter()
            .map(|thing| thing.as_str().unwrap_or_default().to_string())
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

fn thing_arn(thing_name: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:thing/{thing_name}")
}

fn thing_group_arn(group_name: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:thinggroup/{group_name}")
}

/// What AWS IoT publishes as "Maximum number of thing groups a thing can belong to":
/// the figure the service's bound has to be, asserted in
/// [`a_thing_belongs_to_at_most_the_published_number_of_thing_groups`] rather than taken
/// on trust from the store.
const PUBLISHED_THING_GROUPS_PER_THING: usize = 10;

/// RULE: an add writes the membership, whichever way each end is named, and the
/// group's reader reports it.
#[tokio::test]
async fn an_added_thing_is_reported_by_the_groups_own_reader() {
    let emulator = Emulator::new();
    for thing_name in ["sensor-a", "sensor-b", "sensor-c"] {
        assert_eq!(
            emulator.create_thing(thing_name).await.status,
            200,
            "the thing a case adds must be registrable"
        );
    }
    assert_eq!(
        emulator.create_group("fleet", &json!({})).await.status,
        200,
        "the group a case adds to must be creatable"
    );

    // The rows span which end is named by NAME and which by ARN: either identifies the
    // resource, so all four combinations write the same one relation.
    let rows: Vec<(&str, Value)> = vec![
        (
            "both ends by name",
            json!({ "thingGroupName": "fleet", "thingName": "sensor-a" }),
        ),
        (
            "the group by ARN, the thing by name",
            json!({ "thingGroupArn": thing_group_arn("fleet"), "thingName": "sensor-b" }),
        ),
        (
            "the group by name, the thing by ARN",
            json!({ "thingGroupName": "fleet", "thingArn": thing_arn("sensor-c") }),
        ),
        (
            "a repeat of the first add",
            json!({ "thingGroupArn": thing_group_arn("fleet"), "thingArn": thing_arn("sensor-a") }),
        ),
    ];
    for (case, body) in &rows {
        let reply = emulator.add_thing_to_group(body).await;
        assert_eq!(
            reply.status,
            200,
            "case {case}: expected 200, got {} body {}",
            reply.status,
            reply.text()
        );
    }

    let members = emulator.things_in("every add", "fleet", "").await;
    assert_eq!(
        members,
        vec![
            "sensor-a".to_string(),
            "sensor-b".to_string(),
            "sensor-c".to_string()
        ],
        "case every add: expected each thing once, got {members:?}"
    );

    // The listing is paginated over that one order, so a page plus its continuation is
    // the whole membership and no row appears twice.
    let first_page = emulator.list_things_in_group("fleet", "?maxResults=2").await;
    let first = first_page.json();
    assert_eq!(
        first["things"],
        json!(["sensor-a", "sensor-b"]),
        "case a page of two: expected the first two members, got {first}"
    );
    let token = first["nextToken"]
        .as_str()
        .unwrap_or_else(|| panic!("case a page of two: expected a continuation, got {first}"));
    let second = emulator
        .things_in(
            "the continuation",
            "fleet",
            &format!("?maxResults=2&nextToken={token}"),
        )
        .await;
    assert_eq!(
        second,
        vec!["sensor-c".to_string()],
        "case the continuation: expected the rest of the membership, got {second:?}"
    );
}

/// RULE: a membership is only written between records that exist, and only for a group
/// whose membership is a caller's to write.
#[tokio::test]
async fn a_membership_between_records_that_do_not_exist_is_refused() {
    let emulator = Emulator::new();
    assert_eq!(emulator.create_thing("sensor-a").await.status, 200);
    assert_eq!(
        emulator.create_group("fleet", &json!({})).await.status,
        200
    );
    assert_eq!(
        emulator
            .create_dynamic_group(
                "derived",
                &json!({ "queryString": "attributes.zone:north" })
            )
            .await
            .status,
        200,
        "the dynamic group a case names must be creatable"
    );

    let rows: Vec<(&str, Value, u16, &str, String)> = vec![
        (
            "a thing group the registry does not hold",
            json!({ "thingGroupName": "no-such-fleet", "thingName": "sensor-a" }),
            404,
            "ResourceNotFoundException",
            format!("ThingGroup {ACCOUNT_ID}:no-such-fleet not found."),
        ),
        (
            "a thing group ARN the registry does not hold",
            json!({ "thingGroupArn": thing_group_arn("no-such-fleet"), "thingName": "sensor-a" }),
            404,
            "ResourceNotFoundException",
            format!("ThingGroup {ACCOUNT_ID}:no-such-fleet not found."),
        ),
        (
            "a thing the registry does not hold",
            json!({ "thingGroupName": "fleet", "thingName": "no-such-sensor" }),
            404,
            "ResourceNotFoundException",
            format!("Thing {ACCOUNT_ID}:no-such-sensor not found."),
        ),
        (
            "a group whose membership is its query's",
            json!({ "thingGroupName": "derived", "thingName": "sensor-a" }),
            400,
            "InvalidRequestException",
            "Cannot add a thing to the dynamic thing group derived: a dynamic thing group's \
             membership is defined by its query"
                .to_string(),
        ),
        (
            "neither thingGroupName nor thingGroupArn",
            json!({ "thingName": "sensor-a" }),
            400,
            "InvalidRequestException",
            "either thingGroupName or thingGroupArn must name the thing group to add the thing to"
                .to_string(),
        ),
        (
            "neither thingName nor thingArn",
            json!({ "thingGroupName": "fleet" }),
            400,
            "InvalidRequestException",
            "either thingName or thingArn must name the thing to add to the thing group"
                .to_string(),
        ),
    ];

    for (case, body, status, code, message) in &rows {
        let reply = emulator.add_thing_to_group(body).await;
        assert_eq!(
            reply.status, *status,
            "case {case}: expected {status}, got {} body {}",
            reply.status,
            reply.text()
        );
        assert_eq!(
            &reply.error_code(),
            code,
            "case {case}: got {}",
            reply.text()
        );
        assert_eq!(
            &reply.error_message(),
            message,
            "case {case}: got {}",
            reply.text()
        );
    }

    // Nothing above was written: the group the adds named still holds no thing.
    let members = emulator.things_in("after the refusals", "fleet", "").await;
    assert!(
        members.is_empty(),
        "case after the refusals: expected no membership written, got {members:?}"
    );

    // A listing of a group that does not exist is the same 404, never an empty page.
    let missing = emulator.list_things_in_group("no-such-fleet", "").await;
    assert_eq!(
        missing.status,
        404,
        "case a listing of a group that does not exist: expected 404, got {} body {}",
        missing.status,
        missing.text()
    );
}

/// RULE: a recursive listing is the union of the group's members and those of every
/// group below it in the hierarchy.
#[tokio::test]
async fn a_recursive_listing_covers_the_groups_below_it() {
    let emulator = Emulator::new();
    // A three-level hierarchy, each level holding one thing of its own: root -> middle
    // -> leaf.
    let levels = [("root", None), ("middle", Some("root")), ("leaf", Some("middle"))];
    for (group_name, parent) in levels {
        let body = match parent {
            Some(parent) => json!({ "parentGroupName": parent }),
            None => json!({}),
        };
        assert_eq!(
            emulator.create_group(group_name, &body).await.status,
            200,
            "case {group_name}: the group must be creatable"
        );
        let thing_name = format!("thing-in-{group_name}");
        assert_eq!(emulator.create_thing(&thing_name).await.status, 200);
        assert_eq!(
            emulator
                .add_thing_to_group(
                    &json!({ "thingGroupName": group_name, "thingName": thing_name })
                )
                .await
                .status,
            200,
            "case {group_name}: the membership must be writable"
        );
    }

    // Each row: the group listed, and the things a recursive listing of it reports.
    let rows: Vec<(&str, Vec<&str>)> = vec![
        (
            "root",
            vec!["thing-in-leaf", "thing-in-middle", "thing-in-root"],
        ),
        ("middle", vec!["thing-in-leaf", "thing-in-middle"]),
        ("leaf", vec!["thing-in-leaf"]),
    ];
    for (group_name, expected) in rows {
        let recursive = emulator
            .things_in(group_name, group_name, "?recursive=true")
            .await;
        let expected: Vec<String> = expected.iter().map(|name| (*name).to_string()).collect();
        assert_eq!(
            recursive, expected,
            "case {group_name} recursively: expected {expected:?}, got {recursive:?}"
        );
        // A non-recursive listing of the same group reports only its OWN members.
        let direct = emulator.things_in(group_name, group_name, "").await;
        assert_eq!(
            direct,
            vec![format!("thing-in-{group_name}")],
            "case {group_name} directly: expected only its own member, got {direct:?}"
        );
    }
}

/// RULE: a job that targets a thing group runs on the group's MEMBERS — one execution
/// each — and a group the registry does not hold is a 404.
#[tokio::test]
async fn a_group_targeted_job_runs_on_the_groups_members() {
    let emulator = Emulator::new();
    assert_eq!(
        emulator.create_group("fleet", &json!({})).await.status,
        200
    );
    for thing_name in ["sensor-a", "sensor-b"] {
        assert_eq!(emulator.create_thing(thing_name).await.status, 200);
        assert_eq!(
            emulator
                .add_thing_to_group(
                    &json!({ "thingGroupName": "fleet", "thingName": thing_name })
                )
                .await
                .status,
            200
        );
    }
    // A thing that is NOT in the group, so the fan-out is the membership and not the
    // registry.
    assert_eq!(emulator.create_thing("outsider").await.status, 200);

    let created = emulator
        .create_job(
            "group-job",
            &json!({
                "targets": [thing_group_arn("fleet")],
                "document": "{\"operation\":\"reboot\"}",
            }),
        )
        .await;
    assert_eq!(
        created.status,
        200,
        "case a group target: expected 200, got {} body {}",
        created.status,
        created.text()
    );

    for thing_name in ["sensor-a", "sensor-b"] {
        let listed = emulator.list_job_executions_for_thing(thing_name).await;
        assert_eq!(
            listed.status,
            200,
            "case {thing_name}: expected the listing to answer 200, got {} body {}",
            listed.status,
            listed.text()
        );
        let summaries = listed.json();
        let jobs: Vec<String> = summaries["executionSummaries"]
            .as_array()
            .unwrap_or_else(|| panic!("case {thing_name}: no executionSummaries in {summaries}"))
            .iter()
            .map(|entry| entry["jobId"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            jobs,
            vec!["group-job".to_string()],
            "case {thing_name}: a member of the targeted group gets one execution, got {jobs:?}"
        );
    }
    let outsider = emulator.list_job_executions_for_thing("outsider").await.json();
    assert_eq!(
        outsider["executionSummaries"],
        json!([]),
        "case a thing outside the group: expected no execution, got {outsider}"
    );

    let unknown_group = emulator
        .create_job(
            "unknown-group-job",
            &json!({
                "targets": [thing_group_arn("no-such-fleet")],
                "document": "{\"operation\":\"reboot\"}",
            }),
        )
        .await;
    assert_eq!(
        unknown_group.status,
        404,
        "case a group the registry does not hold: expected 404, got {} body {}",
        unknown_group.status,
        unknown_group.text()
    );

    // A DYNAMIC group's members are the things its query selects, which this emulator
    // does not evaluate — so the devices it names are not known and the job is refused,
    // never answered with a 200 that dispatched to nobody.
    assert_eq!(
        emulator
            .create_dynamic_group("derived", &json!({ "queryString": "attributes.zone:north" }))
            .await
            .status,
        200
    );
    let dynamic_target = emulator
        .create_job(
            "dynamic-group-job",
            &json!({
                "targets": [thing_group_arn("derived")],
                "document": "{\"operation\":\"reboot\"}",
            }),
        )
        .await;
    assert_eq!(
        dynamic_target.status,
        400,
        "case a dynamic group target: expected 400, got {} body {}",
        dynamic_target.status,
        dynamic_target.text()
    );
    assert_eq!(
        dynamic_target.error_code(),
        "InvalidRequestException",
        "case a dynamic group target: got {}",
        dynamic_target.text()
    );
    // The same fact refuses a listing of that group's things, rather than an empty page
    // standing in for a membership nobody computed.
    let dynamic_listing = emulator.list_things_in_group("derived", "").await;
    assert_eq!(
        dynamic_listing.status,
        400,
        "case listing a dynamic group's things: expected 400, got {} body {}",
        dynamic_listing.status,
        dynamic_listing.text()
    );
    assert_eq!(
        dynamic_listing.error_code(),
        "InvalidRequestException",
        "case listing a dynamic group's things: got {}",
        dynamic_listing.text()
    );
}

/// RULE: a member the model documents a default for is recorded at that default when
/// the request omits it, so the group reads back under the index and the query version
/// its query is read under whether or not the writer stated them.
#[tokio::test]
async fn an_omitted_dynamic_group_default_is_recorded_and_reported() {
    let emulator = Emulator::new();

    // Each row: what the create sent beside `queryString`, and the `indexName` and
    // `queryVersion` the group is then recorded under. The defaults are the model's own
    // — `smithy_query(query="doc",shape="CreateDynamicThingGroupRequest",member="indexName")`
    // ("Currently one index is supported: AWS_Things") and `member="queryVersion"`
    // ("Currently one query version is supported: "2017-09-30". If not specified, the
    // query version defaults to this value").
    let rows: Vec<(&str, Value, &str, &str)> = vec![
        (
            "neither member stated",
            json!({ "queryString": "attributes.zone:north" }),
            "AWS_Things",
            DEFAULT_DYNAMIC_GROUP_QUERY_VERSION,
        ),
        (
            "the query version stated as the supported one",
            json!({
                "queryString": "thingName:sensor-*",
                "queryVersion": DEFAULT_DYNAMIC_GROUP_QUERY_VERSION,
            }),
            "AWS_Things",
            DEFAULT_DYNAMIC_GROUP_QUERY_VERSION,
        ),
        (
            "the index stated, the query version omitted",
            json!({ "queryString": "registry.version:[1 TO 10]", "indexName": "AWS_Things" }),
            "AWS_Things",
            DEFAULT_DYNAMIC_GROUP_QUERY_VERSION,
        ),
        (
            "both members stated",
            json!({
                "queryString": "attributes.zone:\"north west\"",
                "indexName": "AWS_Things",
                "queryVersion": DEFAULT_DYNAMIC_GROUP_QUERY_VERSION,
            }),
            "AWS_Things",
            DEFAULT_DYNAMIC_GROUP_QUERY_VERSION,
        ),
    ];

    for (index, (case, body, expected_index, expected_version)) in rows.iter().enumerate() {
        let group_name = format!("defaulted-{index}");
        let created = emulator.create_dynamic_group(&group_name, body).await;
        assert_eq!(
            created.status,
            200,
            "case {case}: expected 200, got {} body {}",
            created.status,
            created.text()
        );
        let reported = created.json();
        assert_eq!(
            reported["indexName"],
            json!(expected_index),
            "case {case}: expected indexName {expected_index}, got {reported}"
        );
        assert_eq!(
            reported["queryVersion"],
            json!(expected_version),
            "case {case}: expected queryVersion {expected_version}, got {reported}"
        );

        // What the create reported is what the reader reads: the default was PERSISTED,
        // not filled into one response.
        let read = emulator.describe_group(&group_name).await;
        assert_eq!(
            read.status,
            200,
            "case {case}: expected the read to answer 200, got {} body {}",
            read.status,
            read.text()
        );
        let read = read.json();
        assert_eq!(
            read["indexName"],
            json!(expected_index),
            "case {case}: the read must report the recorded index, got {read}"
        );
        assert_eq!(
            read["queryVersion"],
            json!(expected_version),
            "case {case}: the read must report the recorded query version, got {read}"
        );
    }

    // An `indexName` outside the set this service recognises is not a default to fill
    // in but a request it cannot act on, and nothing is written for it.
    for index_name in ["AWS_ThingGroups", "my-index"] {
        let refused = emulator
            .create_dynamic_group(
                "unrecognized",
                &json!({ "queryString": "thingName:*", "indexName": index_name }),
            )
            .await;
        assert_eq!(
            refused.status,
            400,
            "case the index {index_name}: expected 400, got {} body {}",
            refused.status,
            refused.text()
        );
        assert_eq!(
            refused.error_code(),
            "InvalidRequestException",
            "case the index {index_name}: got {}",
            refused.text()
        );
        assert_eq!(
            refused.error_message(),
            format!("Unrecognized indexName {index_name}"),
            "case the index {index_name}: got {}",
            refused.text()
        );
        assert_eq!(
            emulator.describe_group("unrecognized").await.status,
            404,
            "case the index {index_name}: a refused create must leave no group behind"
        );
    }
}

/// RULE: a dynamic thing group IS its query — an unparsable one is refused and nothing
/// is created, and a created one reports the query that decides its membership.
#[tokio::test]
async fn a_dynamic_thing_group_is_its_query() {
    let emulator = Emulator::new();

    // The rows span the forms a fleet-indexing query takes: a field-qualified term, a
    // wildcard, a range, quoted text, the boolean operators and grouping.
    let accepted: Vec<(&str, &str)> = vec![
        ("a field-qualified term", "attributes.zone:north"),
        ("a wildcard value", "thingName:sensor-*"),
        ("a range", "registry.version:[1 TO 10]"),
        ("a quoted phrase", "attributes.zone:\"north west\""),
        (
            "boolean operators and grouping",
            "(attributes.zone:north OR attributes.zone:south) AND NOT thingName:spare",
        ),
    ];
    for (index, (case, query_string)) in accepted.iter().enumerate() {
        let group_name = format!("derived-{index}");
        let created = emulator
            .create_dynamic_group(&group_name, &json!({ "queryString": query_string }))
            .await;
        assert_eq!(
            created.status,
            200,
            "case {case}: expected 200, got {} body {}",
            created.status,
            created.text()
        );
        let body = created.json();
        assert_eq!(
            body["queryString"],
            json!(query_string),
            "case {case}: the create must report the stored query, got {body}"
        );
        assert_eq!(
            body["indexName"],
            json!("AWS_Things"),
            "case {case}: a dynamic group is over the thing index, got {body}"
        );
        assert_eq!(
            body["thingGroupArn"],
            json!(thing_group_arn(&group_name)),
            "case {case}: expected the minted ARN, got {body}"
        );

        let described = emulator.describe_group(&group_name).await;
        assert_eq!(
            described.status,
            200,
            "case {case}: expected the read to answer 200, got {} body {}",
            described.status,
            described.text()
        );
        let read = described.json();
        assert_eq!(
            read["queryString"],
            json!(query_string),
            "case {case}: the read must report the stored query, got {read}"
        );
        assert_eq!(
            read["status"],
            json!("ACTIVE"),
            "case {case}: a group whose query is recorded is active, got {read}"
        );

        // The delete takes the group with it, and a repeat of it changes nothing.
        for repeat in ["the delete", "a repeat of the delete"] {
            let deleted = emulator.delete_dynamic_group(&group_name).await;
            assert_eq!(
                deleted.status,
                200,
                "case {case}, {repeat}: expected 200, got {} body {}",
                deleted.status,
                deleted.text()
            );
        }
        assert_eq!(
            emulator.describe_group(&group_name).await.status,
            404,
            "case {case}: the deleted group must be gone"
        );
    }

    // A string that is not a query at all: each row breaks the syntax a different way.
    let unparsable = [
        "thingName:",
        ":north",
        "(attributes.zone:north",
        "attributes.zone:north)",
        "registry.version:[1 TO 10",
        "attributes.zone:\"north",
        "AND thingName:sensor",
        "thingName:sensor AND",
        "thingName:a AND OR thingName:b",
        "   ",
    ];
    for query_string in unparsable {
        let refused = emulator
            .create_dynamic_group("refused", &json!({ "queryString": query_string }))
            .await;
        assert_eq!(
            refused.status,
            400,
            "case {query_string:?}: expected 400, got {} body {}",
            refused.status,
            refused.text()
        );
        assert_eq!(
            refused.error_code(),
            "InvalidQueryException",
            "case {query_string:?}: got {}",
            refused.text()
        );
        assert_eq!(
            refused.error_message(),
            UNPARSABLE_QUERY_MESSAGE,
            "case {query_string:?}: got {}",
            refused.text()
        );
        assert_eq!(
            emulator.describe_group("refused").await.status,
            404,
            "case {query_string:?}: a refused create must leave no group behind"
        );
    }

    // The two kinds of group share one namespace, so a name a static group holds is a
    // conflict for the dynamic create too.
    assert_eq!(
        emulator.create_group("fleet", &json!({})).await.status,
        200
    );
    let conflict = emulator
        .create_dynamic_group("fleet", &json!({ "queryString": "thingName:*" }))
        .await;
    assert_eq!(
        conflict.status,
        409,
        "case a name a static group holds: expected 409, got {} body {}",
        conflict.status,
        conflict.text()
    );
    // The static group is left as it was: no query was written onto it.
    let still_static = emulator.describe_group("fleet").await.json();
    assert!(
        still_static.get("queryString").is_none_or(Value::is_null),
        "case a name a static group holds: the static group must be untouched, got {still_static}"
    );
}

/// RULE: a bulk update of one thing's group memberships leaves the thing in exactly the
/// groups it belonged to, minus the ones the request names to leave, plus the ones it
/// names to join.
#[tokio::test]
async fn a_bulk_update_leaves_the_thing_in_the_groups_the_update_names() {
    let emulator = Emulator::new();
    for group_name in ["alpha", "beta", "gamma", "delta"] {
        assert_eq!(
            emulator.create_group(group_name, &json!({})).await.status,
            200,
            "the group {group_name} a case names must be creatable"
        );
    }
    assert_eq!(emulator.create_thing("sensor-a").await.status, 200);
    assert_eq!(
        emulator
            .add_thing_to_group(&json!({ "thingGroupName": "alpha", "thingName": "sensor-a" }))
            .await
            .status,
        200,
        "the membership the first case starts from must be writable"
    );

    // The rows span what the rule generalises over: how many groups are joined and
    // left in one call, whether a list is absent, whether the same update is repeated,
    // a group named by both lists, a group the thing is not in being left, and the
    // `overrideDynamicGroups` flag being set. Each row is applied to the membership the
    // row before it left, and names the memberships that must hold afterwards.
    let rows: Vec<(&str, Value, Vec<&str>)> = vec![
        (
            "two groups joined at once",
            json!({ "thingName": "sensor-a", "thingGroupsToAdd": ["beta", "gamma"] }),
            vec!["alpha", "beta", "gamma"],
        ),
        (
            "one group joined and one left in the same call",
            json!({
                "thingName": "sensor-a",
                "thingGroupsToAdd": ["delta"],
                "thingGroupsToRemove": ["alpha"],
            }),
            vec!["beta", "delta", "gamma"],
        ),
        (
            "a repeat of that same update",
            json!({
                "thingName": "sensor-a",
                "thingGroupsToAdd": ["delta"],
                "thingGroupsToRemove": ["alpha"],
            }),
            vec!["beta", "delta", "gamma"],
        ),
        (
            "removals only",
            json!({ "thingName": "sensor-a", "thingGroupsToRemove": ["beta", "gamma"] }),
            vec!["delta"],
        ),
        (
            "a group named by both lists",
            json!({
                "thingName": "sensor-a",
                "thingGroupsToAdd": ["delta"],
                "thingGroupsToRemove": ["delta"],
            }),
            vec!["delta"],
        ),
        (
            "neither list",
            json!({ "thingName": "sensor-a" }),
            vec!["delta"],
        ),
        (
            "a group the thing is not in, named to be left",
            json!({ "thingName": "sensor-a", "thingGroupsToRemove": ["alpha"] }),
            vec!["delta"],
        ),
        (
            "every group left",
            json!({ "thingName": "sensor-a", "thingGroupsToRemove": ["delta"] }),
            vec![],
        ),
        (
            "a static group joined with overrideDynamicGroups set",
            json!({
                "thingName": "sensor-a",
                "thingGroupsToAdd": ["alpha"],
                "overrideDynamicGroups": true,
            }),
            vec!["alpha"],
        ),
    ];

    for (case, body, expected) in &rows {
        let reply = emulator.update_thing_groups_for_thing(body).await;
        assert_eq!(
            reply.status,
            200,
            "case {case}: expected 200, got {} body {}",
            reply.status,
            reply.text()
        );
        let groups = emulator.groups_of(case, "sensor-a").await;
        let expected: Vec<String> = expected.iter().map(|name| (*name).to_string()).collect();
        assert_eq!(
            groups, expected,
            "case {case}: expected the thing in {expected:?}, got {groups:?}"
        );
        // The relation has ONE record, so the group's own reader agrees with the
        // thing's: a group the thing is in reports it, and one it is not in does not.
        for group_name in ["alpha", "beta", "gamma", "delta"] {
            let members = emulator.things_in(case, group_name, "").await;
            let holds = expected.iter().any(|name| name == group_name);
            assert_eq!(
                members.contains(&"sensor-a".to_string()),
                holds,
                "case {case}: group {group_name} must {} the thing, got {members:?}",
                if holds { "hold" } else { "not hold" }
            );
        }
    }
}

/// RULE: a bulk update is ONE update — it is refused whole, under the rules a single
/// add or removal follows, and a refused update applies none of itself.
#[tokio::test]
async fn a_refused_bulk_update_changes_no_membership() {
    let emulator = Emulator::new();
    for group_name in ["alpha", "beta"] {
        assert_eq!(
            emulator.create_group(group_name, &json!({})).await.status,
            200
        );
    }
    assert_eq!(
        emulator
            .create_dynamic_group("derived", &json!({ "queryString": "attributes.zone:north" }))
            .await
            .status,
        200,
        "the dynamic group a case names must be creatable"
    );
    assert_eq!(emulator.create_thing("sensor-a").await.status, 200);
    assert_eq!(
        emulator
            .add_thing_to_group(&json!({ "thingGroupName": "alpha", "thingName": "sensor-a" }))
            .await
            .status,
        200
    );

    let rows: Vec<(&str, Value, u16, &str, String)> = vec![
        (
            "a thing the registry does not hold",
            json!({ "thingName": "no-such-sensor", "thingGroupsToAdd": ["beta"] }),
            404,
            "ResourceNotFoundException",
            format!("Thing {ACCOUNT_ID}:no-such-sensor not found."),
        ),
        (
            "a group to join the registry does not hold, beside one it does",
            json!({ "thingName": "sensor-a", "thingGroupsToAdd": ["beta", "no-such-fleet"] }),
            404,
            "ResourceNotFoundException",
            format!("ThingGroup {ACCOUNT_ID}:no-such-fleet not found."),
        ),
        (
            "a group to leave the registry does not hold",
            json!({
                "thingName": "sensor-a",
                "thingGroupsToAdd": ["beta"],
                "thingGroupsToRemove": ["no-such-fleet"],
            }),
            404,
            "ResourceNotFoundException",
            format!("ThingGroup {ACCOUNT_ID}:no-such-fleet not found."),
        ),
        (
            "a group to join whose membership is its query's",
            json!({ "thingName": "sensor-a", "thingGroupsToAdd": ["beta", "derived"] }),
            400,
            "InvalidRequestException",
            "Cannot add a thing to the dynamic thing group derived: a dynamic thing group's \
             membership is defined by its query"
                .to_string(),
        ),
        (
            "a group to leave whose membership is its query's",
            json!({
                "thingName": "sensor-a",
                "thingGroupsToAdd": ["beta"],
                "thingGroupsToRemove": ["derived"],
            }),
            400,
            "InvalidRequestException",
            "Cannot remove a thing from the dynamic thing group derived: a dynamic thing group's \
             membership is defined by its query"
                .to_string(),
        ),
        (
            "no thing named at all",
            json!({ "thingGroupsToAdd": ["beta"] }),
            400,
            "InvalidRequestException",
            "thingName must name the thing whose group memberships will be updated".to_string(),
        ),
    ];

    for (case, body, status, code, message) in &rows {
        let reply = emulator.update_thing_groups_for_thing(body).await;
        assert_eq!(
            reply.status, *status,
            "case {case}: expected {status}, got {} body {}",
            reply.status,
            reply.text()
        );
        assert_eq!(
            &reply.error_code(),
            code,
            "case {case}: got {}",
            reply.text()
        );
        assert_eq!(
            &reply.error_message(),
            message,
            "case {case}: got {}",
            reply.text()
        );
        // None of the update was applied: the group every refused row also named is
        // still not one the thing belongs to, and the one it started in still is.
        let groups = emulator.groups_of(case, "sensor-a").await;
        assert_eq!(
            groups,
            vec!["alpha".to_string()],
            "case {case}: a refused update must leave the membership it found, got {groups:?}"
        );
    }
}

/// RULE: a thing may belong to at most the published number of thing groups (AWS IoT's
/// "Maximum number of thing groups a thing can belong to"), and the limit is on the
/// membership the update LEAVES — so an update that leaves as many groups as it joins
/// is within it.
#[tokio::test]
async fn a_bulk_update_is_bounded_by_the_group_limit_for_a_thing() {
    let emulator = Emulator::new();
    // One group more than the limit, so the table covers the bound exactly and one
    // past it.
    let group_names: Vec<String> = (0..=MAX_THING_GROUPS_PER_THING)
        .map(|index| format!("limit-{index:02}"))
        .collect();
    for group_name in &group_names {
        assert_eq!(
            emulator.create_group(group_name, &json!({})).await.status,
            200,
            "the group {group_name} must be creatable"
        );
    }
    assert_eq!(emulator.create_thing("sensor-a").await.status, 200);

    let over_the_limit =
        format!("Thing sensor-a cannot belong to more than {MAX_THING_GROUPS_PER_THING} thing groups");

    // One past the bound, in one call: refused, and nothing joined.
    let refused = emulator
        .update_thing_groups_for_thing(
            &json!({ "thingName": "sensor-a", "thingGroupsToAdd": group_names }),
        )
        .await;
    assert_eq!(
        refused.status,
        400,
        "case one group past the limit at once: expected 400, got {} body {}",
        refused.status,
        refused.text()
    );
    assert_eq!(
        refused.error_message(),
        over_the_limit,
        "case one group past the limit at once: got {}",
        refused.text()
    );
    let groups = emulator
        .groups_of("one group past the limit at once", "sensor-a")
        .await;
    assert!(
        groups.is_empty(),
        "case one group past the limit at once: expected no membership written, got {groups:?}"
    );

    // Exactly the bound: accepted.
    let at_the_bound: Vec<String> = group_names[..MAX_THING_GROUPS_PER_THING].to_vec();
    assert_eq!(
        emulator
            .update_thing_groups_for_thing(
                &json!({ "thingName": "sensor-a", "thingGroupsToAdd": at_the_bound }),
            )
            .await
            .status,
        200,
        "case exactly the limit: the update must be accepted"
    );
    let groups = emulator.groups_of("exactly the limit", "sensor-a").await;
    assert_eq!(
        groups, at_the_bound,
        "case exactly the limit: expected {at_the_bound:?}, got {groups:?}"
    );

    // One more group on top of the bound: refused, membership untouched.
    let last = group_names[MAX_THING_GROUPS_PER_THING].clone();
    let refused = emulator
        .update_thing_groups_for_thing(
            &json!({ "thingName": "sensor-a", "thingGroupsToAdd": [last.clone()] }),
        )
        .await;
    assert_eq!(
        refused.status,
        400,
        "case one group past a full membership: expected 400, got {} body {}",
        refused.status,
        refused.text()
    );
    assert_eq!(
        refused.error_message(),
        over_the_limit,
        "case one group past a full membership: got {}",
        refused.text()
    );
    let groups = emulator
        .groups_of("one group past a full membership", "sensor-a")
        .await;
    assert_eq!(
        groups, at_the_bound,
        "case one group past a full membership: expected {at_the_bound:?}, got {groups:?}"
    );

    // Leaving one group and joining another in the same call stays within the bound.
    let swapped = emulator
        .update_thing_groups_for_thing(&json!({
            "thingName": "sensor-a",
            "thingGroupsToAdd": [last.clone()],
            "thingGroupsToRemove": [group_names[0].clone()],
        }))
        .await;
    assert_eq!(
        swapped.status,
        200,
        "case one group swapped for another: expected 200, got {} body {}",
        swapped.status,
        swapped.text()
    );
    let expected: Vec<String> = group_names[1..].to_vec();
    let groups = emulator
        .groups_of("one group swapped for another", "sensor-a")
        .await;
    assert_eq!(
        groups, expected,
        "case one group swapped for another: expected {expected:?}, got {groups:?}"
    );
}

/// RULE: a thing belongs to at most [`MAX_THING_GROUPS_PER_THING`] thing groups — the
/// add AT the bound is written, the add PAST it is refused and writes nothing, and the
/// bound counts the memberships the thing holds at that moment rather than the adds ever
/// made.
#[tokio::test]
async fn a_thing_belongs_to_at_most_the_published_number_of_thing_groups() {
    let emulator = Emulator::new();
    // The bound the service enforces is the published one. `AddThingToThingGroup`
    // declares no limit error of its own
    // (`smithy_query(query="operation",operation="AddThingToThingGroup")` lists
    // InternalFailure, InvalidRequest, ResourceNotFound and Throttling), so the add past
    // it is the modelled `InvalidRequestException`.
    assert_eq!(
        MAX_THING_GROUPS_PER_THING, PUBLISHED_THING_GROUPS_PER_THING,
        "case the published bound: a thing may belong to {PUBLISHED_THING_GROUPS_PER_THING} \
         thing groups, the service bounds it at {MAX_THING_GROUPS_PER_THING}"
    );
    assert_eq!(
        emulator.create_thing("sensor-a").await.status,
        200,
        "the thing every case adds must be registrable"
    );
    // One group MORE than the bound admits, so the last of them is the add past it.
    // Zero-padded, so the name order the registry and both listings use is the order
    // these were created in.
    let group_names: Vec<String> = (0..=MAX_THING_GROUPS_PER_THING)
        .map(|index| format!("fleet-{index:02}"))
        .collect();
    for group_name in &group_names {
        assert_eq!(
            emulator.create_group(group_name, &json!({})).await.status,
            200,
            "case {group_name}: the group must be creatable"
        );
    }
    let (past_the_bound, within_the_bound) = group_names
        .split_last()
        .expect("the case list holds one group past the bound");

    // Every add up to and including the bound is written.
    for (index, group_name) in within_the_bound.iter().enumerate() {
        let reply = emulator
            .add_thing_to_group(&json!({ "thingGroupName": group_name, "thingName": "sensor-a" }))
            .await;
        assert_eq!(
            reply.status,
            200,
            "case membership {} of {MAX_THING_GROUPS_PER_THING}: expected 200, got {} body {}",
            index + 1,
            reply.status,
            reply.text()
        );
    }
    let at_the_bound = emulator.groups_of("at the bound", "sensor-a").await;
    assert_eq!(
        at_the_bound,
        within_the_bound.to_vec(),
        "case at the bound: expected every written membership once, got {at_the_bound:?}"
    );

    // The add past the bound is refused, however the request is phrased. The rows span
    // what `overrideDynamicGroups` can ask for and how each end is named, because
    // neither changes the bound: what that member overrides is a DYNAMIC membership
    // being dropped to make room, and a dynamic group's membership is its query's, so
    // there is no such membership here to drop.
    let refused: Vec<(&str, Value)> = vec![
        (
            "overrideDynamicGroups left to its modelled default",
            json!({ "thingGroupName": past_the_bound, "thingName": "sensor-a" }),
        ),
        (
            "overrideDynamicGroups false",
            json!({
                "thingGroupName": past_the_bound,
                "thingName": "sensor-a",
                "overrideDynamicGroups": false,
            }),
        ),
        (
            "overrideDynamicGroups true",
            json!({
                "thingGroupName": past_the_bound,
                "thingName": "sensor-a",
                "overrideDynamicGroups": true,
            }),
        ),
        (
            "both ends named by ARN",
            json!({
                "thingGroupArn": thing_group_arn(past_the_bound),
                "thingArn": thing_arn("sensor-a"),
                "overrideDynamicGroups": true,
            }),
        ),
    ];
    // The limit refusal names the thing from the request and the bound itself; a single
    // add and a bulk update state the same rule the same way.
    let over_the_limit = format!(
        "Thing sensor-a cannot belong to more than {MAX_THING_GROUPS_PER_THING} thing groups"
    );
    for (case, body) in &refused {
        let reply = emulator.add_thing_to_group(body).await;
        assert_eq!(
            reply.status,
            400,
            "case {case}: expected 400, got {} body {}",
            reply.status,
            reply.text()
        );
        assert_eq!(
            reply.error_code(),
            "InvalidRequestException",
            "case {case}: got {}",
            reply.text()
        );
        assert_eq!(
            reply.error_message(),
            over_the_limit,
            "case {case}: got {}",
            reply.text()
        );
        // Nothing was written: the group past the bound holds no thing, and the thing's
        // own listing still reports only the memberships it had.
        let members = emulator.things_in(case, past_the_bound, "").await;
        assert!(
            members.is_empty(),
            "case {case}: a refused add must write no membership, got {members:?}"
        );
        let groups = emulator.groups_of(case, "sensor-a").await;
        assert_eq!(
            groups,
            within_the_bound.to_vec(),
            "case {case}: a refused add must leave the thing's groups as they were, got \
             {groups:?}"
        );
    }

    // A thing at the bound re-added to a group it is ALREADY in names no new membership,
    // so that add is still a 200 and leaves the same memberships.
    let first_group = within_the_bound
        .first()
        .expect("the case list holds a group within the bound");
    let repeat = emulator
        .add_thing_to_group(&json!({ "thingGroupName": first_group, "thingName": "sensor-a" }))
        .await;
    assert_eq!(
        repeat.status,
        200,
        "case a repeat of a membership the thing holds: expected 200, got {} body {}",
        repeat.status,
        repeat.text()
    );

    // The bound counts the memberships the thing holds NOW: giving one up makes room for
    // exactly one more.
    let removed = emulator
        .remove_thing_from_group(&json!({ "thingGroupName": first_group, "thingName": "sensor-a" }))
        .await;
    assert_eq!(
        removed.status,
        200,
        "case a membership given up: expected 200, got {} body {}",
        removed.status,
        removed.text()
    );
    let readmitted = emulator
        .add_thing_to_group(&json!({ "thingGroupName": past_the_bound, "thingName": "sensor-a" }))
        .await;
    assert_eq!(
        readmitted.status,
        200,
        "case the add the removal made room for: expected 200, got {} body {}",
        readmitted.status,
        readmitted.text()
    );
    let mut expected_groups: Vec<String> = within_the_bound[1..].to_vec();
    expected_groups.push(past_the_bound.clone());
    let after = emulator.groups_of("after the removal", "sensor-a").await;
    assert_eq!(
        after, expected_groups,
        "case after the removal: expected the group given up gone and the new one in its \
         place, got {after:?}"
    );
    let members = emulator
        .things_in("after the removal", past_the_bound, "")
        .await;
    assert_eq!(
        members,
        vec!["sensor-a".to_string()],
        "case after the removal: the group's own reader must report the written membership, \
         got {members:?}"
    );
}
