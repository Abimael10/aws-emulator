// SPDX-License-Identifier: CC0-1.0

//! `DescribeMaintenanceWindowsForTarget` end to end: the maintenance windows one target is
//! associated with are the windows a `RegisterTargetWithMaintenanceWindow` registered it with.
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` ([`router`], [`call_in_region`], [`error_code`]) dispatches
//! `POST /` with `X-Amz-Target: AmazonSSM.<Operation>` through the multi-service router.
//! Nothing reaches into the state struct, so a response that is not computed from persisted
//! state cannot pass: every report follows a create and a register.
//!
//! The rules under test, each asserted over a TABLE whose rows differ along what the rule
//! generalises over rather than over one recorded request:
//!
//! 1. **A window is reported when a registration of it covers the target.** A registration
//!    covers the asked-about criteria when the two state the same key and share a value under
//!    it, and the `ResourceType` the read states narrows that to the registrations made for
//!    that type; each window reported carries the id and the name its create persisted, and
//!    is reported ONCE however many of its registrations cover the target. The rows differ
//!    along the form of the criteria (a node id, another node id of the same registration, a
//!    tag, a resource group name), the resource type, the number of criteria stated, and the
//!    cases that name nothing: an unregistered node, a key no registration states, and an
//!    empty criteria list.
//! 2. **A window and its registrations live in ONE Region.** A target registered through one
//!    Region's endpoint is associated with no window at another's.
//! 3. **A page holds at most `MaxResults` windows** and the `NextToken` names the window the
//!    next page starts at, so a walk over the pages reports each window once; a token this
//!    read never handed out is refused rather than served a page.
//! 4. **A `ResourceType` outside the modelled enum never reaches the read**: the boundary walk
//!    refuses it as a malformed request.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call_in_region, error_code, router, REGION};

/// A second Region, so the windows a target is associated with are shown to be the ones of the
/// endpoint the registers ran through.
const OTHER_REGION: &str = "eu-central-1";

/// A managed node id a window's targets are registered against.
const A_NODE: &str = "i-0123456789abcdef0";

/// A second node id of the SAME registration, so a criterion is shown to be matched on the
/// value it shares with the registration rather than on the registration's first value.
const ANOTHER_NODE: &str = "i-0fedcba987654321f";

/// A node id no register of these cases names.
const UNREGISTERED_NODE: &str = "i-00000000000000000";

/// A `WindowId` of the form the modelled `MaintenanceWindowId` pattern requires
/// (`^mw-[0-9a-f]{17}$`) that no create has minted.
const UNKNOWN_WINDOW_ID: &str = "mw-00000000000000000";

/// Create a window at `region`'s endpoint under `name` and answer the id it minted.
async fn window(router: &MultiServiceRouter, case: &str, region: &str, name: &str) -> String {
    let (status, response) = call_in_region(
        router,
        region,
        "CreateMaintenanceWindow",
        &json!({
            "Name": name,
            "Schedule": "cron(0 3 ? * SUN *)",
            "Duration": 2,
            "Cutoff": 1,
            "AllowUnassociatedTargets": false,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the window {name:?} must be created, got {status} {response}"
    );
    response["WindowId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a create reports a WindowId, got {response}"))
        .to_string()
}

/// Register `targets` of `resource_type` with `window_id` at `region`'s endpoint, asserting the
/// register succeeded.
async fn register_ok(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    window_id: &str,
    resource_type: &str,
    targets: &Value,
) {
    let body = json!({
        "WindowId": window_id,
        "ResourceType": resource_type,
        "Targets": targets,
    });
    let (status, response) = call_in_region(
        router,
        region,
        "RegisterTargetWithMaintenanceWindow",
        &body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the register {body} must succeed, got {status} {response}"
    );
}

/// `DescribeMaintenanceWindowsForTarget` at `region`'s endpoint with `body`.
async fn for_target(router: &MultiServiceRouter, region: &str, body: &Value) -> (u16, Value) {
    call_in_region(
        router,
        region,
        "DescribeMaintenanceWindowsForTarget",
        body,
    )
    .await
}

/// The windows the read serves at `region`'s endpoint for `body`, asserting it was served, as
/// the reported entries beside the `NextToken` it handed out.
async fn served(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    body: &Value,
) -> (Vec<Value>, Option<String>) {
    let (status, response) = for_target(router, region, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeMaintenanceWindowsForTarget {body} must be served, got {status} \
         {response}"
    );
    let identities = response["WindowIdentities"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let token = response["NextToken"]
        .as_str()
        .filter(|token| !token.is_empty())
        .map(str::to_string);
    (identities, token)
}

/// The `WindowId`s reported, in the order the read states them.
fn reported_ids(identities: &[Value]) -> Vec<String> {
    identities
        .iter()
        .map(|identity| {
            identity["WindowId"]
                .as_str()
                .unwrap_or_else(|| panic!("every reported window carries a WindowId, got {identity}"))
                .to_string()
        })
        .collect()
}

/// A criterion naming one key and the values under it.
fn criterion(key: &str, values: &[&str]) -> Value {
    json!({ "Key": key, "Values": values })
}

/// RULE 1: the windows reported for a target are the windows whose registrations of the stated
/// resource type cover it, each carrying the id and name its create persisted, each once.
#[tokio::test]
async fn the_windows_reported_are_the_ones_a_registration_covers() {
    let router = router();
    let case = "the association table";

    // Three windows, created in this order, so the order the report states them in is the
    // order they were created in.
    let by_node = window(&router, case, REGION, "ForTargetByNode").await;
    let by_tag = window(&router, case, REGION, "ForTargetByTag").await;
    let by_group = window(&router, case, REGION, "ForTargetByGroup").await;

    // The first window is registered against two node ids AND, a second time, against the
    // same tag as the second window — so it is a window the tag names, and still reported
    // once.
    register_ok(
        &router,
        case,
        REGION,
        &by_node,
        "INSTANCE",
        &json!([criterion("InstanceIds", &[A_NODE, ANOTHER_NODE])]),
    )
    .await;
    register_ok(
        &router,
        case,
        REGION,
        &by_node,
        "INSTANCE",
        &json!([criterion("tag:Environment", &["production"])]),
    )
    .await;
    register_ok(
        &router,
        case,
        REGION,
        &by_tag,
        "INSTANCE",
        &json!([criterion("tag:Environment", &["production"])]),
    )
    .await;
    // `MaintenanceWindowResourceType` declares INSTANCE and RESOURCE_GROUP
    // (`smithy_query(query="shape", shape="MaintenanceWindowResourceType")`), so the third
    // window is registered as the other one of the two.
    register_ok(
        &router,
        case,
        REGION,
        &by_group,
        "RESOURCE_GROUP",
        &json!([criterion("resource-groups:Name", &["PatchGroup"])]),
    )
    .await;

    // (case, resource type, the criteria asked about, the windows they are associated with)
    let rows: Vec<(&str, &str, Value, Vec<&String>)> = vec![
        (
            "the node the first registration's criteria name",
            "INSTANCE",
            json!([criterion("InstanceIds", &[A_NODE])]),
            vec![&by_node],
        ),
        (
            "another node of that same registration",
            "INSTANCE",
            json!([criterion("InstanceIds", &[ANOTHER_NODE])]),
            vec![&by_node],
        ),
        (
            "a tag two windows are registered against",
            "INSTANCE",
            json!([criterion("tag:Environment", &["production"])]),
            vec![&by_node, &by_tag],
        ),
        (
            "the resource group, of the type it was registered for",
            "RESOURCE_GROUP",
            json!([criterion("resource-groups:Name", &["PatchGroup"])]),
            vec![&by_group],
        ),
        (
            "the resource group, of the other resource type",
            "INSTANCE",
            json!([criterion("resource-groups:Name", &["PatchGroup"])]),
            vec![],
        ),
        (
            "a node, of the resource type it was not registered for",
            "RESOURCE_GROUP",
            json!([criterion("InstanceIds", &[A_NODE])]),
            vec![],
        ),
        (
            "two criteria, each naming a window of its own",
            "INSTANCE",
            json!([
                criterion("InstanceIds", &[A_NODE]),
                criterion("tag:Environment", &["production"]),
            ]),
            vec![&by_node, &by_tag],
        ),
        (
            "a node no register named",
            "INSTANCE",
            json!([criterion("InstanceIds", &[UNREGISTERED_NODE])]),
            vec![],
        ),
        (
            "the right value under a key no registration states",
            "INSTANCE",
            json!([criterion("InstanceId", &[A_NODE])]),
            vec![],
        ),
        (
            "a tag key held under another value",
            "INSTANCE",
            json!([criterion("tag:Environment", &["staging"])]),
            vec![],
        ),
        (
            "no criterion at all, which names no node",
            "INSTANCE",
            json!([]),
            vec![],
        ),
    ];

    for (row, resource_type, targets, expected) in rows {
        let body = json!({ "ResourceType": resource_type, "Targets": targets });
        let (identities, token) = served(&router, row, REGION, &body).await;
        let expected_ids: Vec<String> = expected.iter().map(|id| (*id).clone()).collect();
        assert_eq!(
            reported_ids(&identities),
            expected_ids,
            "case {row:?}: expected the windows {expected_ids:?}, got {identities:?}"
        );
        assert!(
            token.is_none(),
            "case {row:?}: every window of this result fits one page, so no NextToken is owed, \
             got {token:?}"
        );
    }

    // The name reported for a window is the one its create persisted, so the report projects
    // the stored window rather than the request.
    let (identities, _) = served(
        &router,
        case,
        REGION,
        &json!({
            "ResourceType": "INSTANCE",
            "Targets": [criterion("tag:Environment", &["production"])],
        }),
    )
    .await;
    let named: Vec<(Option<&str>, Option<&str>)> = identities
        .iter()
        .map(|identity| (identity["WindowId"].as_str(), identity["Name"].as_str()))
        .collect();
    assert_eq!(
        named,
        vec![
            (Some(by_node.as_str()), Some("ForTargetByNode")),
            (Some(by_tag.as_str()), Some("ForTargetByTag")),
        ],
        "case {case:?}: each window must be reported under the id and name its create \
         persisted, got {identities:?}"
    );
}

/// RULE 2: a window and the registrations that make it one of a target's live in the Region
/// their endpoint names, so the same question answers differently at another Region's.
#[tokio::test]
async fn a_target_is_associated_with_the_windows_of_its_own_region() {
    let router = router();
    let case = "the Region of the register";
    let here = window(&router, case, REGION, "ForTargetHere").await;
    let there = window(&router, case, OTHER_REGION, "ForTargetThere").await;
    let targets = json!([criterion("InstanceIds", &[A_NODE])]);
    register_ok(&router, case, REGION, &here, "INSTANCE", &targets).await;
    register_ok(&router, case, OTHER_REGION, &there, "INSTANCE", &targets).await;

    for (region, expected) in [(REGION, &here), (OTHER_REGION, &there)] {
        let row = format!("the endpoint of {region}");
        let (identities, _) = served(
            &router,
            &row,
            region,
            &json!({ "ResourceType": "INSTANCE", "Targets": targets }),
        )
        .await;
        assert_eq!(
            reported_ids(&identities),
            vec![expected.clone()],
            "case {row:?}: only the window registered through this endpoint must be reported, \
             got {identities:?}"
        );
    }
}

/// RULE 3: a page holds at most `MaxResults` windows, the `NextToken` resumes at the window the
/// page stopped at, and a token this read never handed out is refused.
#[tokio::test]
async fn a_page_is_bounded_and_its_token_resumes_the_walk() {
    let router = router();
    let case = "the paged walk";
    let targets = json!([criterion("InstanceIds", &[A_NODE])]);
    // Four windows the one node is registered with, in creation order.
    let mut created: Vec<String> = Vec::new();
    for ordinal in 0..4 {
        let id = window(&router, case, REGION, &format!("ForTargetPaged{ordinal}")).await;
        register_ok(&router, case, REGION, &id, "INSTANCE", &targets).await;
        created.push(id);
    }

    // The rows differ along the page size asked for, including the smallest the modelled
    // `MaintenanceWindowSearchMaxResults` admits (`range 1..`), one that divides the result,
    // one that does not, and one past the whole result.
    for page_size in [1usize, 2, 3, 4, 10] {
        let row = format!("MaxResults {page_size}");
        let mut walked: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut body = json!({
                "ResourceType": "INSTANCE",
                "Targets": targets,
                "MaxResults": page_size,
            });
            if let Some(resume) = &token {
                body["NextToken"] = Value::String(resume.clone());
            }
            let (identities, next) = served(&router, &row, REGION, &body).await;
            assert!(
                identities.len() <= page_size,
                "case {row:?}: a page holds at most {page_size} windows, got {identities:?}"
            );
            if let Some(resume) = &next {
                assert_eq!(
                    identities.len(),
                    page_size,
                    "case {row:?}: a page that owes a token is full, got {identities:?}"
                );
                assert!(
                    created.contains(resume),
                    "case {row:?}: the token names the window the next page starts at, got \
                     {resume:?}"
                );
            }
            walked.extend(reported_ids(&identities));
            token = next;
            if token.is_none() {
                break;
            }
        }
        assert_eq!(
            walked, created,
            "case {row:?}: the walk must report each associated window once, in the order the \
             windows were created, got {walked:?}"
        );
    }

    // A token this read never handed out: one that is not shaped like a window id at all, and
    // one that is but names no window of this result.
    for token in ["not-a-token", UNKNOWN_WINDOW_ID] {
        let row = format!("NextToken {token:?}");
        let (status, response) = for_target(
            &router,
            REGION,
            &json!({
                "ResourceType": "INSTANCE",
                "Targets": targets,
                "NextToken": token,
            }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {row:?}: a token this read did not hand out must be refused, got {status} \
             {response}"
        );
        assert_eq!(
            error_code(&response),
            "ValidationException",
            "case {row:?}: the refusal is a malformed-request one, got {response}"
        );
    }

    // `MaxResults` one past the modelled lower bound is refused as well.
    let (status, response) = for_target(
        &router,
        REGION,
        &json!({ "ResourceType": "INSTANCE", "Targets": targets, "MaxResults": 0 }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case MaxResults 0: a page size under the modelled bound must be refused, got {status} \
         {response}"
    );
    assert_eq!(
        error_code(&response),
        "ValidationException",
        "case MaxResults 0: the refusal is a malformed-request one, got {response}"
    );
}

/// RULE 4: a `ResourceType` outside the modelled `MaintenanceWindowResourceType` enum is
/// refused by the boundary walk rather than answered as a type nothing is registered for.
#[tokio::test]
async fn a_resource_type_outside_the_modelled_enum_is_refused() {
    let router = router();
    let case = "the enum guard";
    // A window the node IS registered with exists, so the refusals below are about the type
    // rather than about an empty store.
    let window_id = window(&router, case, REGION, "ForTargetEnumGuard").await;
    let targets = json!([criterion("InstanceIds", &[A_NODE])]);
    register_ok(&router, case, REGION, &window_id, "INSTANCE", &targets).await;

    // Each row is a spelling the enum does not declare: the right value in the wrong case, a
    // resource type of another read of this service, a plural, and the empty string.
    for resource_type in ["instance", "EC2Instance", "INSTANCES", ""] {
        let row = format!("ResourceType {resource_type:?}");
        let (status, response) = for_target(
            &router,
            REGION,
            &json!({ "ResourceType": resource_type, "Targets": targets }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {row:?}: the read must be refused, got {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            "ValidationException",
            "case {row:?}: the type must be refused as a malformed request, got {response}"
        );
    }
}
