// SPDX-License-Identifier: CC0-1.0

//! WHY a patch baseline a patch group is registered with cannot be deleted, read through the
//! wire a client reads it on: `AmazonSSM.DeletePatchBaseline` over `awsJson1_1`, dispatched
//! by the multi-service router through the shared harness in `all/parameter_store.rs`.
//!
//! RULE under test: a delete of a resource another record of this service still references is
//! REFUSED with that operation's modelled in-use error, which names the referencing record,
//! and the resource is left exactly as it was — for a patch baseline, "Error returned if an
//! attempt is made to delete a patch baseline that is registered for a patch group"
//! (`ResourceInUseException`, HTTP 400), whose message is fixed text naming the patch group
//! the registration is held under. Only once the registration is taken away does the delete
//! go through.
//!
//! The rows differ along what the rule generalises over: the form of the patch group's name,
//! the operating system the baseline applies to, and which spelling the delete names the
//! baseline by (its own id or its full ARN). The recorded case — one hyphenated probe group
//! name — is one row.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call_in_region, error_code, error_message, router, ACCOUNT, REGION};
use crate::patch_baseline_deletion::APPROVED_PATCH;

/// The wire error code a delete of a baseline a group still holds answers with
/// (`smithy_query(query="operation", operation="DeletePatchBaseline")`:
/// `ResourceInUseException`, client, HTTP 400).
const IN_USE_CODE: &str = "ResourceInUseException";

/// The fixed text of that refusal, before the patch group the registration is held under.
/// Spelled out here rather than read from the crate, so the wording itself is what the
/// assertions check.
const IN_USE_PREFIX: &str = "PatchBaseline to be deleted is used for Patch Group: ";

/// The full ARN spelling of a baseline id at `REGION`'s endpoint.
fn baseline_arn(baseline_id: &str) -> String {
    format!("arn:aws:ssm:{REGION}:{ACCOUNT}:patchbaseline/{baseline_id}")
}

/// `CreatePatchBaseline` at `REGION`'s endpoint, asserting it succeeded, and answering the
/// `BaselineId` it minted.
async fn create(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    operating_system: &str,
) -> String {
    let (status, response) = call_in_region(
        router,
        REGION,
        "CreatePatchBaseline",
        &json!({
            "Name": name,
            "OperatingSystem": operating_system,
            // "Approved Patches and Approval Rule, at least one must be non empty": one
            // approved patch, so no case has to pick filter criteria for its operating
            // system.
            "ApprovedPatches": [APPROVED_PATCH],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreatePatchBaseline {name} must succeed, got {status} with {response}"
    );
    response["BaselineId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a create reports a BaselineId, got {response}"))
        .to_string()
}

/// `RegisterPatchBaselineForPatchGroup`, asserting it succeeded.
async fn register(router: &MultiServiceRouter, case: &str, baseline_id: &str, group: &str) {
    let (status, response) = call_in_region(
        router,
        REGION,
        "RegisterPatchBaselineForPatchGroup",
        &json!({ "BaselineId": baseline_id, "PatchGroup": group }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the register of {group} must succeed, got {status} with {response}"
    );
}

/// `DeregisterPatchBaselineForPatchGroup`, asserting it succeeded.
async fn deregister(router: &MultiServiceRouter, case: &str, baseline_id: &str, group: &str) {
    let (status, response) = call_in_region(
        router,
        REGION,
        "DeregisterPatchBaselineForPatchGroup",
        &json!({ "BaselineId": baseline_id, "PatchGroup": group }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the deregister of {group} must succeed, got {status} with {response}"
    );
}

/// `DeletePatchBaseline` naming the baseline by `named`.
async fn delete(router: &MultiServiceRouter, named: &str) -> (u16, Value) {
    call_in_region(
        router,
        REGION,
        "DeletePatchBaseline",
        &json!({ "BaselineId": named }),
    )
    .await
}

/// `GetPatchBaseline` for `baseline_id`.
async fn read(router: &MultiServiceRouter, baseline_id: &str) -> (u16, Value) {
    call_in_region(
        router,
        REGION,
        "GetPatchBaseline",
        &json!({ "BaselineId": baseline_id }),
    )
    .await
}

/// One row: the group name the baseline is registered for, the operating system it applies
/// to, and whether the delete names it by its full ARN.
struct Row {
    case: &'static str,
    name: &'static str,
    group: &'static str,
    operating_system: &'static str,
    by_arn: bool,
}

/// A baseline a patch group is registered with is not deleted: the delete is refused with
/// `ResourceInUseException` naming that group, the baseline is still readable, and the delete
/// goes through once the registration is taken away.
#[tokio::test]
async fn a_baseline_a_patch_group_holds_is_not_deleted() {
    let router = router();

    let rows = [
        Row {
            // The form the recorded probe used: a hyphenated name with a hex suffix.
            case: "a hyphenated group name",
            name: "InUse.Hyphenated",
            group: "probe-ssm-patch-group-9b1f38ab",
            operating_system: "WINDOWS",
            by_arn: false,
        },
        Row {
            case: "a group name carrying spaces",
            name: "InUse.Spaced",
            group: "production fleet",
            operating_system: "WINDOWS",
            by_arn: false,
        },
        Row {
            case: "a group name of one character",
            name: "InUse.Short",
            group: "a",
            operating_system: "AMAZON_LINUX_2023",
            by_arn: false,
        },
        Row {
            case: "a group name carrying punctuation a tag value admits",
            name: "InUse.Punctuated",
            group: "fleet/eu.west-1:patching_2026",
            operating_system: "UBUNTU",
            by_arn: false,
        },
        Row {
            case: "a linux baseline named by its full ARN",
            name: "InUse.ByArn",
            group: "linux-fleet",
            operating_system: "AMAZON_LINUX_2",
            by_arn: true,
        },
        Row {
            case: "a macOS baseline named by its own id",
            name: "InUse.MacOs",
            group: "developer-laptops",
            operating_system: "MACOS",
            by_arn: false,
        },
    ];

    for row in rows {
        let case = row.case;
        let baseline_id = create(&router, case, row.name, row.operating_system).await;
        register(&router, case, &baseline_id, row.group).await;

        let named = if row.by_arn {
            baseline_arn(&baseline_id)
        } else {
            baseline_id.clone()
        };
        let (status, refused) = delete(&router, &named).await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (400, IN_USE_CODE),
            "case {case:?}: expected the delete of {named} to be refused, got {status} with \
             {refused}"
        );
        assert_eq!(
            error_message(&refused),
            format!("{IN_USE_PREFIX}{}", row.group),
            "case {case:?}: expected the refusal to name the group holding the baseline, got \
             {refused}"
        );

        // The refusal left the baseline exactly as it was.
        let (status, held) = read(&router, &baseline_id).await;
        assert_eq!(
            (status, held["BaselineId"].as_str()),
            (200, Some(baseline_id.as_str())),
            "case {case:?}: expected the refused delete to leave the baseline, got {status} with \
             {held}"
        );
        assert_eq!(
            held["PatchGroups"].as_array().map(Vec::as_slice),
            Some([json!(row.group)].as_slice()),
            "case {case:?}: expected the baseline to still report its group, got {held}"
        );

        // Once the registration is taken away nothing holds the baseline, and the delete
        // goes through — reporting the baseline by its own id, whichever spelling named it.
        deregister(&router, case, &baseline_id, row.group).await;
        let (status, deleted) = delete(&router, &named).await;
        assert_eq!(
            (status, deleted["BaselineId"].as_str()),
            (200, Some(baseline_id.as_str())),
            "case {case:?}: expected the delete to succeed once the group let go, got {status} \
             with {deleted}"
        );
    }
}

/// Which group the refusal names when several hold the one baseline, and that a baseline no
/// group holds is deleted without a refusal.
#[tokio::test]
async fn the_refusal_names_a_group_the_store_holds_the_registration_under() {
    let router = router();

    // Two groups of DIFFERENT operating systems can hold one baseline only if the baseline
    // applies to each, so the several-groups case registers two groups against one baseline
    // of one operating system.
    let case = "two groups registered with one baseline";
    let baseline_id = create(&router, case, "InUse.TwoGroups", "WINDOWS").await;
    let groups = ["alpha-fleet", "beta-fleet"];
    for group in groups {
        register(&router, case, &baseline_id, group).await;
    }
    let (status, refused) = delete(&router, &baseline_id).await;
    assert_eq!(
        (status, error_code(&refused).as_str()),
        (400, IN_USE_CODE),
        "case {case:?}: expected the delete to be refused, got {status} with {refused}"
    );
    let message = error_message(&refused);
    assert!(
        groups
            .iter()
            .any(|group| message == format!("{IN_USE_PREFIX}{group}")),
        "case {case:?}: expected the refusal to name one of the groups holding the baseline, \
         got {message:?}"
    );

    // Taking ONE registration away is not enough: the other group still holds the baseline.
    deregister(&router, case, &baseline_id, groups[0]).await;
    let (status, still_refused) = delete(&router, &baseline_id).await;
    assert_eq!(
        (status, error_code(&still_refused).as_str(), error_message(&still_refused)),
        (400, IN_USE_CODE, format!("{IN_USE_PREFIX}{}", groups[1])),
        "case {case:?}: expected the remaining group to still hold the baseline, got {status} \
         with {still_refused}"
    );
    deregister(&router, case, &baseline_id, groups[1]).await;
    let (status, deleted) = delete(&router, &baseline_id).await;
    assert_eq!(
        (status, deleted["BaselineId"].as_str()),
        (200, Some(baseline_id.as_str())),
        "case {case:?}: expected the delete to succeed once every group let go, got {status} \
         with {deleted}"
    );

    // A baseline no group ever held is deleted with no refusal at all.
    let case = "a baseline no group holds";
    let unheld = create(&router, case, "InUse.Never", "WINDOWS").await;
    let (status, deleted) = delete(&router, &unheld).await;
    assert_eq!(
        (status, deleted["BaselineId"].as_str()),
        (200, Some(unheld.as_str())),
        "case {case:?}: expected the delete to succeed, got {status} with {deleted}"
    );

    // Another group's registration does not protect a baseline it does not name.
    let case = "a baseline whose group holds another baseline";
    let held = create(&router, case, "InUse.Held", "WINDOWS").await;
    let other = create(&router, case, "InUse.Other", "AMAZON_LINUX_2").await;
    register(&router, case, &held, "shared-fleet").await;
    let (status, deleted) = delete(&router, &other).await;
    assert_eq!(
        (status, deleted["BaselineId"].as_str()),
        (200, Some(other.as_str())),
        "case {case:?}: expected the unregistered baseline to be deleted, got {status} with \
         {deleted}"
    );
}
