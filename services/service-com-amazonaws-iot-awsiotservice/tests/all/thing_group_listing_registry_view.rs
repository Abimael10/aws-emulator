// SPDX-License-Identifier: CC0-1.0

//! `ListThingGroups` as a VIEW OF THE LIVE REGISTRY, over the real wire path
//! (`MultiServiceRouter::dispatch` of a restJson1 request signed for the `iot` signing
//! name), so what is asserted is what an AWS SDK client would see.
//!
//! `tests/all/thing_group_listing.rs` pins the filters and the paging of the listing over
//! a fixed hierarchy. This suite pins the rules that tie the listing to the registry it
//! reads:
//!
//! * the listing is "the thing groups in your account" — every group the registry holds
//!   whatever created it, so a DYNAMIC group (`CreateDynamicThingGroup`, which models no
//!   parent member) is reported beside the static ones, as a group with no parent;
//! * it is derived from the registry at the moment of the call, so a group a later create
//!   adds is reported and a group a delete removes is not, while a REFUSED delete leaves
//!   the listing exactly as it was;
//! * it cannot disagree with `DescribeThingGroup` about a group: same name, same ARN, and
//!   the parent the hierarchy filter reported it under is the parent that reader reports;
//! * it is a pure read — listing, paging and a refused listing change nothing a later
//!   caller sees.

use std::collections::BTreeMap;
use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region this suite signs for.
const REGION: &str = "eu-west-2";

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
            .expect("the thing group request is well formed");
        Reply::of(self.router.dispatch(request).await)
    }

    /// Enable the THING fleet index (`UpdateIndexingConfiguration`, `POST
    /// /indexing/config`), which a dynamic thing group is a view of: with no index there
    /// is nothing for such a group to be a view of, so every case that creates one
    /// enables it first.
    async fn given_thing_indexing(&self) {
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
            enabled.status, 200,
            "case enable thing indexing: expected 200, got {} body {}",
            enabled.status,
            enabled.text()
        );
    }

    /// `CreateThingGroup`: `POST /thing-groups/{thingGroupName}`, optionally under a
    /// parent. Answers the ARN the create minted.
    async fn create_static_group(&self, group_name: &str, parent: Option<&str>) -> String {
        let body = match parent {
            Some(parent) => json!({ "parentGroupName": parent }),
            None => json!({}),
        };
        let reply = self
            .dispatch("POST", &format!("/thing-groups/{group_name}"), Some(&body))
            .await;
        assert_eq!(
            reply.status, 200,
            "case create static {group_name}: expected 200, got {} body {}",
            reply.status,
            reply.text()
        );
        reply.arn("thingGroupArn", &format!("create static {group_name}"))
    }

    /// `CreateDynamicThingGroup`: `POST /dynamic-thing-groups/{thingGroupName}`. Its
    /// input models `queryString` and no parent member, so such a group is a root.
    async fn create_dynamic_group(&self, group_name: &str, query_string: &str) -> String {
        let reply = self
            .dispatch(
                "POST",
                &format!("/dynamic-thing-groups/{group_name}"),
                Some(&json!({ "queryString": query_string })),
            )
            .await;
        assert_eq!(
            reply.status, 200,
            "case create dynamic {group_name}: expected 200, got {} body {}",
            reply.status,
            reply.text()
        );
        reply.arn("thingGroupArn", &format!("create dynamic {group_name}"))
    }

    /// `DeleteThingGroup`: `DELETE /thing-groups/{thingGroupName}`.
    async fn delete_static_group(&self, group_name: &str) -> Reply {
        self.dispatch("DELETE", &format!("/thing-groups/{group_name}"), None)
            .await
    }

    /// `DeleteDynamicThingGroup`: `DELETE /dynamic-thing-groups/{thingGroupName}`.
    async fn delete_dynamic_group(&self, group_name: &str) -> Reply {
        self.dispatch(
            "DELETE",
            &format!("/dynamic-thing-groups/{group_name}"),
            None,
        )
        .await
    }

    /// `DescribeThingGroup`: `GET /thing-groups/{thingGroupName}`.
    async fn describe_group(&self, group_name: &str) -> Reply {
        self.dispatch("GET", &format!("/thing-groups/{group_name}"), None)
            .await
    }

    /// `ListThingGroups` with the query string a case asks for (`""` for none).
    async fn list_groups(&self, query: &str) -> Reply {
        let path = if query.is_empty() {
            "/thing-groups".to_string()
        } else {
            format!("/thing-groups?{query}")
        };
        self.dispatch("GET", &path, None).await
    }

    /// The `(groupName, groupArn)` rows one 200 page of the listing carries.
    async fn listed(&self, case: &str, query: &str) -> Vec<(String, String)> {
        let reply = self.list_groups(query).await;
        assert_eq!(
            reply.status, 200,
            "case {case}: expected the listing to answer 200, got {} body {}",
            reply.status,
            reply.text()
        );
        reply.rows(case)
    }

    /// Just the names of that page.
    async fn listed_names(&self, case: &str, query: &str) -> Vec<String> {
        self.listed(case, query)
            .await
            .into_iter()
            .map(|(group_name, _arn)| group_name)
            .collect()
    }

    /// Every group the listing reports, walked one row at a time so the paging is
    /// exercised beside the single-page read.
    async fn listed_names_by_paging(&self, case: &str, query: &str) -> Vec<String> {
        let mut seen: Vec<String> = Vec::new();
        let mut next: Option<String> = None;
        loop {
            let mut page_query = if query.is_empty() {
                "maxResults=1".to_string()
            } else {
                format!("{query}&maxResults=1")
            };
            if let Some(token) = &next {
                page_query.push_str(&format!("&nextToken={token}"));
            }
            let page = self.list_groups(&page_query).await;
            assert_eq!(
                page.status, 200,
                "case {case}: expected a page to answer 200, got {} body {}",
                page.status,
                page.text()
            );
            let names: Vec<String> = page
                .rows(case)
                .into_iter()
                .map(|(group_name, _arn)| group_name)
                .collect();
            assert!(
                names.len() <= 1,
                "case {case}: a maxResults=1 page holds at most one group, got {names:?}"
            );
            seen.extend(names);
            match page.next_token() {
                Some(token) => next = Some(token),
                None => break,
            }
        }
        seen
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

    fn arn(&self, member: &str, case: &str) -> String {
        self.json()[member]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: no {member} in {}", self.text()))
            .to_string()
    }

    /// The `(groupName, groupArn)` rows of a `ListThingGroups` page. `thingGroups` is
    /// always present, even empty: "nothing matches" is an answer, not a missing one.
    fn rows(&self, case: &str) -> Vec<(String, String)> {
        self.json()["thingGroups"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case}: no thingGroups in {}", self.text()))
            .iter()
            .map(|group| {
                (
                    group["groupName"].as_str().unwrap_or_default().to_string(),
                    group["groupArn"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect()
    }

    fn next_token(&self) -> Option<String> {
        self.json()["nextToken"].as_str().map(str::to_string)
    }
}

fn names(of: &[&str]) -> Vec<String> {
    of.iter().map(|name| (*name).to_string()).collect()
}

/// RULE: the listing reports every thing group the account holds whatever created it, so
/// a dynamic thing group is reported beside the static ones — as a root, since its create
/// models no parent.
#[tokio::test]
async fn the_listing_reports_dynamic_groups_beside_static_ones() {
    let emulator = Emulator::new();
    emulator.given_thing_indexing().await;

    // The rows differ in what created the group and at which depth it sits: a static
    // root, a static child, and two dynamic groups whose queries differ.
    let mut minted: BTreeMap<String, String> = BTreeMap::new();
    minted.insert(
        "fleet".to_string(),
        emulator.create_static_group("fleet", None).await,
    );
    minted.insert(
        "fleet-east".to_string(),
        emulator.create_static_group("fleet-east", Some("fleet")).await,
    );
    minted.insert(
        "derived-north".to_string(),
        emulator
            .create_dynamic_group("derived-north", "attributes.zone:north")
            .await,
    );
    minted.insert(
        "derived-south".to_string(),
        emulator
            .create_dynamic_group("derived-south", "thingName:south*")
            .await,
    );

    let whole_account = emulator.listed("the whole account", "").await;
    let expected: Vec<(String, String)> = minted
        .iter()
        .map(|(group_name, arn)| (group_name.clone(), arn.clone()))
        .collect();
    assert_eq!(
        whole_account, expected,
        "case the whole account: expected every group once in name order with the ARN its \
         create minted, got {whole_account:?}"
    );

    // A dynamic group's create models no parent, so it is a root: the static parent's
    // subtree does not pick it up, and neither does the dynamic group's own.
    let subtree = emulator
        .listed_names("a static group's subtree", "parentGroup=fleet&recursive=true")
        .await;
    assert_eq!(
        subtree,
        names(&["fleet-east"]),
        "case a static group's subtree: expected only the static child, got {subtree:?}"
    );
    let dynamic_children = emulator
        .listed_names(
            "a dynamic group's children",
            "parentGroup=derived-north&recursive=true",
        )
        .await;
    assert!(
        dynamic_children.is_empty(),
        "case a dynamic group's children: expected none, got {dynamic_children:?}"
    );

    // The filters read a dynamic group's own name like any other group's.
    let by_prefix = emulator
        .listed_names("a prefix matching the dynamic groups", "namePrefixFilter=derived")
        .await;
    assert_eq!(
        by_prefix,
        names(&["derived-north", "derived-south"]),
        "case a prefix matching the dynamic groups: expected both, got {by_prefix:?}"
    );
}

/// What a step of [`the_listing_follows_the_registry`] does to the registry.
enum Step {
    /// `CreateThingGroup`, optionally under a parent.
    CreateStatic(&'static str, Option<&'static str>),
    /// `CreateDynamicThingGroup`.
    CreateDynamic(&'static str),
    /// `DeleteThingGroup`, which must succeed.
    DeleteStatic(&'static str),
    /// `DeleteDynamicThingGroup`, which must succeed.
    DeleteDynamic(&'static str),
    /// `DeleteThingGroup` of a group that still has child groups: refused, so the
    /// registry — and the listing over it — is left exactly as it was.
    RefusedDeleteStatic(&'static str),
}

/// RULE: the listing is derived from the registry at the moment of the call, so every
/// create and every delete that lands moves it, and one that is refused does not.
#[tokio::test]
async fn the_listing_follows_the_registry_as_groups_are_created_and_deleted() {
    let emulator = Emulator::new();
    emulator.given_thing_indexing().await;

    // The rows span the mutations a group registry admits: a root created, a child
    // created under it, a dynamic group created, a leaf deleted, a dynamic group deleted,
    // a delete refused, and a deleted name created again.
    let steps: [(Step, &[&str]); 8] = [
        (Step::CreateStatic("fleet", None), &["fleet"]),
        (
            Step::CreateStatic("fleet-east", Some("fleet")),
            &["fleet", "fleet-east"],
        ),
        (
            Step::CreateStatic("fleet-west", Some("fleet")),
            &["fleet", "fleet-east", "fleet-west"],
        ),
        (
            Step::CreateDynamic("derived"),
            &["derived", "fleet", "fleet-east", "fleet-west"],
        ),
        (
            Step::RefusedDeleteStatic("fleet"),
            &["derived", "fleet", "fleet-east", "fleet-west"],
        ),
        (
            Step::DeleteStatic("fleet-west"),
            &["derived", "fleet", "fleet-east"],
        ),
        (Step::DeleteDynamic("derived"), &["fleet", "fleet-east"]),
        (
            Step::CreateStatic("fleet-west", Some("fleet-east")),
            &["fleet", "fleet-east", "fleet-west"],
        ),
    ];

    // An account that holds nothing yet reports an empty listing.
    let before_anything = emulator.listed_names("an account with no group", "").await;
    assert!(
        before_anything.is_empty(),
        "case an account with no group: expected an empty listing, got {before_anything:?}"
    );

    for (step, expected) in &steps {
        let label = match step {
            Step::CreateStatic(group_name, None) => {
                emulator.create_static_group(group_name, None).await;
                format!("after creating the root {group_name}")
            }
            Step::CreateStatic(group_name, Some(parent)) => {
                emulator.create_static_group(group_name, Some(parent)).await;
                format!("after creating {group_name} under {parent}")
            }
            Step::CreateDynamic(group_name) => {
                emulator
                    .create_dynamic_group(group_name, "attributes.zone:north")
                    .await;
                format!("after creating the dynamic group {group_name}")
            }
            Step::DeleteStatic(group_name) => {
                let deleted = emulator.delete_static_group(group_name).await;
                assert_eq!(
                    deleted.status, 200,
                    "case deleting {group_name}: expected 200, got {} body {}",
                    deleted.status,
                    deleted.text()
                );
                format!("after deleting {group_name}")
            }
            Step::DeleteDynamic(group_name) => {
                let deleted = emulator.delete_dynamic_group(group_name).await;
                assert_eq!(
                    deleted.status, 200,
                    "case deleting the dynamic group {group_name}: expected 200, got {} body {}",
                    deleted.status,
                    deleted.text()
                );
                format!("after deleting the dynamic group {group_name}")
            }
            Step::RefusedDeleteStatic(group_name) => {
                let refused = emulator.delete_static_group(group_name).await;
                assert_eq!(
                    refused.status, 400,
                    "case a refused delete of {group_name}: expected 400, got {} body {}",
                    refused.status,
                    refused.text()
                );
                assert_eq!(
                    refused.error_code(),
                    "InvalidRequestException",
                    "case a refused delete of {group_name}: got {}",
                    refused.text()
                );
                format!("after a refused delete of {group_name}")
            }
        };

        let listed = emulator.listed_names(&label, "").await;
        assert_eq!(
            listed,
            names(expected),
            "case {label}: expected {expected:?}, got {listed:?}"
        );
        // The same groups whether the listing is read in one page or walked one row at a
        // time, since both are the same view of the same registry.
        let paged = emulator.listed_names_by_paging(&label, "").await;
        assert_eq!(
            paged,
            names(expected),
            "case {label} walked one row at a time: expected {expected:?}, got {paged:?}"
        );
    }
}

/// RULE: the listing and `DescribeThingGroup` are two readers of one registry record, so
/// they report the same name and ARN for a group, and the parent the hierarchy filter
/// reported it under is the parent that reader reports.
#[tokio::test]
async fn the_listing_agrees_with_describe_thing_group_about_every_group() {
    let emulator = Emulator::new();
    emulator.given_thing_indexing().await;

    // `(group, parent)`: a chain three deep beside a second root and a dynamic group, so
    // the rows differ in depth, in parent and in what created them.
    emulator.create_static_group("fleet", None).await;
    emulator.create_static_group("fleet-east", Some("fleet")).await;
    emulator
        .create_static_group("fleet-east-rack1", Some("fleet-east"))
        .await;
    emulator.create_static_group("warehouse", None).await;
    emulator
        .create_dynamic_group("derived", "attributes.zone:north")
        .await;

    for (group_name, listed_arn) in emulator.listed("agreement", "").await {
        let described = emulator.describe_group(&group_name).await;
        assert_eq!(
            described.status, 200,
            "case {group_name}: expected DescribeThingGroup to answer 200, got {} body {}",
            described.status,
            described.text()
        );
        assert_eq!(
            described.json()["thingGroupName"].as_str(),
            Some(group_name.as_str()),
            "case {group_name}: the reader names another group: {}",
            described.text()
        );
        assert_eq!(
            described.arn("thingGroupArn", &group_name),
            listed_arn,
            "case {group_name}: the listing and the reader disagree about the ARN, the \
             reader says {}",
            described.text()
        );
    }

    // Each group the children filter reports under a parent is a group the reader reports
    // that same parent for; a root is reported by no parent's listing and the reader
    // carries no parent for it.
    let parents: [(&str, Option<&str>); 5] = [
        ("fleet", None),
        ("fleet-east", Some("fleet")),
        ("fleet-east-rack1", Some("fleet-east")),
        ("warehouse", None),
        ("derived", None),
    ];
    for (group_name, parent) in &parents {
        let described = emulator.describe_group(group_name).await;
        let reported_parent = described.json()["thingGroupMetadata"]["parentGroupName"]
            .as_str()
            .map(str::to_string);
        assert_eq!(
            reported_parent.as_deref(),
            *parent,
            "case {group_name}: expected parent {parent:?}, the reader says {}",
            described.text()
        );
        if let Some(parent) = parent {
            let children = emulator
                .listed_names(
                    &format!("the children of {parent}"),
                    &format!("parentGroup={parent}"),
                )
                .await;
            assert!(
                children.contains(&(*group_name).to_string()),
                "case {group_name}: expected the children of {parent} to report it, got \
                 {children:?}"
            );
        }
    }
}

/// RULE: the listing is a pure read, so listing, paging through it and a listing that is
/// refused all leave every later caller the same answers.
#[tokio::test]
async fn the_listing_changes_nothing_a_later_caller_sees() {
    let emulator = Emulator::new();
    emulator.given_thing_indexing().await;

    emulator.create_static_group("fleet", None).await;
    emulator.create_static_group("fleet-east", Some("fleet")).await;
    emulator
        .create_dynamic_group("derived", "attributes.zone:north")
        .await;
    let held = ["derived", "fleet", "fleet-east"];

    /// Everything a reader reports about one group, which a read must not move.
    async fn snapshot(emulator: &Emulator, held: &[&str]) -> Vec<(String, Value)> {
        let mut records = Vec::new();
        for group_name in held {
            let described = emulator.describe_group(group_name).await;
            assert_eq!(
                described.status, 200,
                "case snapshot {group_name}: expected 200, got {} body {}",
                described.status,
                described.text()
            );
            records.push(((*group_name).to_string(), described.json()));
        }
        records
    }

    let before_rows = emulator.listed("before the reads", "").await;
    let before_records = snapshot(&emulator, &held).await;

    // The reads: the whole account, each filter, the paging, and a listing that is
    // refused because it names a group the account does not hold.
    for query in [
        "",
        "recursive=true",
        "parentGroup=fleet",
        "parentGroup=fleet&recursive=true",
        "namePrefixFilter=fleet",
        "maxResults=1",
    ] {
        let read = emulator.list_groups(query).await;
        assert_eq!(
            read.status, 200,
            "case reading {query:?}: expected 200, got {} body {}",
            read.status,
            read.text()
        );
    }
    let walked = emulator.listed_names_by_paging("the paged read", "").await;
    assert_eq!(
        walked,
        names(&held),
        "case the paged read: expected every group once, got {walked:?}"
    );
    let refused = emulator.list_groups("parentGroup=no-such-group").await;
    assert_eq!(
        refused.status, 404,
        "case a listing of a group the account does not hold: expected 404, got {} body {}",
        refused.status,
        refused.text()
    );

    let after_rows = emulator.listed("after the reads", "").await;
    assert_eq!(
        after_rows, before_rows,
        "case after the reads: the listing moved, expected {before_rows:?}, got {after_rows:?}"
    );
    let after_records = snapshot(&emulator, &held).await;
    assert_eq!(
        after_records, before_records,
        "case after the reads: a group's record moved, expected {before_records:?}, got \
         {after_records:?}"
    );
}
