// SPDX-License-Identifier: CC0-1.0

//! What `DeletePatchBaseline` persists, and what every later read of the deleted baseline
//! reports, end to end: a create mints a baseline, a delete removes it, and the reads that
//! resolve a baseline id report it absent from then on.
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` ([`router`], [`call_in_region`], [`error_code`],
//! [`error_message`]) dispatches `POST /` with `X-Amz-Target: AmazonSSM.<Operation>`
//! through the multi-service router. Nothing reaches into the state struct, so a response
//! that is not computed from persisted state cannot pass: every read follows a write.
//!
//! The rule under test is that a delete of a resource this service holds REMOVES the
//! record, so every later operation that resolves that resource's id finds nothing under it
//! and reports it absent, while everything the delete did not name is left exactly as it
//! was. The suites are tables whose rows differ along what the rule generalises over: the
//! operating system the baseline applies to, which optional members its create supplied,
//! the Region the endpoint is, which spelling the delete names the baseline by (its own id
//! or its full ARN), which read is made after the delete, how many baselines the endpoint
//! holds, and how many times the same baseline is deleted.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::parameter_store::{call_in_region, error_code, error_message, router, ACCOUNT, REGION};

/// A second Region, so a case can prove a delete reaches the records of the endpoint it was
/// signed for and no other's.
pub(crate) const OTHER_REGION: &str = "eu-central-1";

/// The sentence `DoesNotExistException` carries for a `BaselineId` that names no patch
/// baseline reachable through the endpoint: fixed text for this error class, naming the
/// resource class and interpolating nothing from the request. Spelled out here rather than
/// read from the crate, so the wording itself is what the assertions check.
const NOT_FOUND_MESSAGE: &str = "Patch Baseline to be retrieved does not exist.";

/// The wire error code a read of a baseline this endpoint does not hold answers with
/// (`smithy_query(query="operation", operation="GetPatchBaseline")`: `DoesNotExistException`,
/// client, HTTP 400).
const NOT_FOUND_CODE: &str = "DoesNotExistException";

/// `CreatePatchBaseline` at `region`'s endpoint, asserting it succeeded, and returning the
/// `BaselineId` it minted.
pub(crate) async fn create_ok(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    body: &Value,
) -> String {
    let (status, response) = call_in_region(router, region, "CreatePatchBaseline", body).await;
    assert_eq!(
        status, 200,
        "case {case}: CreatePatchBaseline {body} must succeed, got {status} {response}"
    );
    response["BaselineId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: a create reports a BaselineId, got {response}"))
        .to_string()
}

/// `GetPatchBaseline` at `region`'s endpoint for `baseline_id`.
pub(crate) async fn get(
    router: &MultiServiceRouter,
    region: &str,
    baseline_id: &str,
) -> (u16, Value) {
    call_in_region(
        router,
        region,
        "GetPatchBaseline",
        &json!({ "BaselineId": baseline_id }),
    )
    .await
}

/// `DeletePatchBaseline` at `region`'s endpoint for `baseline_id`.
async fn delete(router: &MultiServiceRouter, region: &str, baseline_id: &str) -> (u16, Value) {
    call_in_region(
        router,
        region,
        "DeletePatchBaseline",
        &json!({ "BaselineId": baseline_id }),
    )
    .await
}

/// The same delete, asserting it succeeded, and returning the `BaselineId` it reports.
pub(crate) async fn delete_ok(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    baseline_id: &str,
) -> String {
    let (status, response) = delete(router, region, baseline_id).await;
    assert_eq!(
        status, 200,
        "case {case}: DeletePatchBaseline {baseline_id:?} must succeed, got {status} {response}"
    );
    response["BaselineId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: a delete reports a BaselineId, got {response}"))
        .to_string()
}

/// Assert that `baseline_id` names no baseline at `region`'s endpoint: the modelled refusal,
/// its status, its wire code and the sentence it carries.
pub(crate) async fn assert_absent(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    baseline_id: &str,
) {
    let (status, response) = get(router, region, baseline_id).await;
    assert_eq!(
        status, 400,
        "case {case}: GetPatchBaseline {baseline_id:?} must be refused, got {status} {response}"
    );
    assert_eq!(
        error_code(&response),
        NOT_FOUND_CODE,
        "case {case}: expected {NOT_FOUND_CODE}, got {response}"
    );
    assert_eq!(
        error_message(&response),
        NOT_FOUND_MESSAGE,
        "case {case}: expected {NOT_FOUND_MESSAGE:?}, got {response}"
    );
}

/// The ARN of the patch baseline `baseline_id` names in `region`, the form the account's own
/// ARNs carry.
pub(crate) fn baseline_arn(region: &str, baseline_id: &str) -> String {
    format!("arn:aws:ssm:{region}:{ACCOUNT}:patchbaseline/{baseline_id}")
}

/// The bare id a baseline ARN names: the resource after the last `/` of the ARN, which is
/// what the `patchbaseline/` resource-type prefix is followed by.
fn id_of_arn(arn: &str) -> String {
    arn.rsplit_once('/')
        .map(|(_, baseline_id)| baseline_id.to_string())
        .unwrap_or_else(|| panic!("a baseline ARN names a resource, got {arn:?}"))
}

/// A patch id a create names among its approved patches, so the baseline satisfies "Approved
/// Patches and Approval Rule, at least one must be non empty" without a case having to pick
/// filter criteria its operating system accepts.
pub(crate) const APPROVED_PATCH: &str = "KB2032276";

/// One approval rule selecting patches of `classification`, approving them after a day — the
/// shape `PatchRule` requires: a filter group, plus "a value for either `ApproveAfterDays`
/// or `ApproveUntilDate`".
pub(crate) fn patch_rule(classification: &str) -> Value {
    json!({
        "PatchFilterGroup": {
            "PatchFilters": [{ "Key": "CLASSIFICATION", "Values": [classification] }],
        },
        "ApproveAfterDays": 1,
    })
}

/// A delete removes the baseline it names, whichever Region holds it, whatever operating
/// system it applies to, whichever members its create supplied, and whichever spelling the
/// delete names it by — so a read of that id afterwards reports no baseline.
#[tokio::test]
async fn a_deleted_baseline_is_no_longer_readable() {
    let router = router();

    // (case, region, the create body, whether the delete names the baseline by its ARN).
    let cases: &[(&str, &str, Value, bool)] = &[
        (
            "a windows baseline named by its own id",
            REGION,
            json!({
                "Name": "Deletion.Windows",
                "OperatingSystem": "WINDOWS",
                "ApprovedPatches": [APPROVED_PATCH],
            }),
            false,
        ),
        (
            "a windows baseline named by its full ARN",
            REGION,
            json!({
                "Name": "Deletion.WindowsByArn",
                "OperatingSystem": "WINDOWS",
                "ApprovalRules": { "PatchRules": [patch_rule("SecurityUpdates")] },
            }),
            true,
        ),
        (
            "a baseline whose create named no operating system",
            REGION,
            json!({ "Name": "Deletion.DefaultedOs", "ApprovedPatches": [APPROVED_PATCH] }),
            false,
        ),
        (
            "a linux baseline carrying every optional member",
            REGION,
            json!({
                "Name": "Deletion.AmazonLinux2",
                "OperatingSystem": "AMAZON_LINUX_2",
                "Description": "the baseline a delete removes",
                "ApprovedPatches": ["kernel-5.10.184-175.731.amzn2"],
                "ApprovedPatchesComplianceLevel": "HIGH",
                "ApprovedPatchesEnableNonSecurity": true,
                "RejectedPatches": ["kernel-5.10.179-166.674.amzn2"],
                "RejectedPatchesAction": "BLOCK",
                "GlobalFilters": {
                    "PatchFilters": [{ "Key": "PRODUCT", "Values": ["AmazonLinux2"] }],
                },
                "ApprovalRules": { "PatchRules": [patch_rule("Security")] },
                "Sources": [{
                    "Name": "main",
                    "Products": ["AmazonLinux2"],
                    "Configuration": "[main]\nname=main\nbaseurl=https://example.invalid/repo",
                }],
                "Tags": [{ "Key": "Owner", "Value": "patching" }],
            }),
            false,
        ),
        (
            "a baseline of another Region, named by its ARN there",
            OTHER_REGION,
            json!({
                "Name": "Deletion.Ubuntu",
                "OperatingSystem": "UBUNTU",
                "ApprovedPatches": ["linux-image-generic"],
            }),
            true,
        ),
        (
            "a baseline of another Region, named by its own id",
            OTHER_REGION,
            json!({
                "Name": "Deletion.Macos",
                "OperatingSystem": "MACOS",
                "ApprovedPatches": ["Safari"],
            }),
            false,
        ),
    ];

    for (case, region, body, by_arn) in cases {
        let baseline_id = create_ok(&router, case, region, body).await;
        let (status, before) = get(&router, region, &baseline_id).await;
        assert_eq!(
            status, 200,
            "case {case}: the created baseline must be readable, got {status} {before}"
        );

        let named = if *by_arn {
            baseline_arn(region, &baseline_id)
        } else {
            baseline_id.clone()
        };
        let reported = delete_ok(&router, case, region, &named).await;
        assert_eq!(
            reported, baseline_id,
            "case {case}: a delete reports the id of the baseline it removed, got {reported:?}"
        );

        assert_absent(&router, case, region, &baseline_id).await;
        // Either spelling of the removed id reaches the same absent baseline.
        assert_absent(&router, case, region, &baseline_arn(region, &baseline_id)).await;
    }
}

/// Every operation that resolves a baseline id reports a deleted baseline absent: the read of
/// the baseline itself, the read of the patches it makes effective, a write of it, a register
/// of it as a default, and the listing of what the endpoint holds. The default of its
/// operating system falls back to the baseline Systems Manager predefines, because nothing
/// holds the registered one any more.
#[tokio::test]
async fn every_read_that_resolves_a_baseline_id_reports_a_deleted_one_absent() {
    let router = router();
    let case = "a deleted baseline that was the registered default";

    let predefined = {
        let (status, response) = call_in_region(
            &router,
            REGION,
            "GetDefaultPatchBaseline",
            &json!({ "OperatingSystem": "WINDOWS" }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: GetDefaultPatchBaseline must succeed, got {status} {response}"
        );
        response["BaselineId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: a read reports a BaselineId, got {response}"))
            .to_string()
    };

    let baseline_id = create_ok(
        &router,
        case,
        REGION,
        &json!({
            "Name": "Deletion.EveryReader",
            "OperatingSystem": "WINDOWS",
            "ApprovalRules": { "PatchRules": [patch_rule("SecurityUpdates")] },
        }),
    )
    .await;
    let (status, registered) = call_in_region(
        &router,
        REGION,
        "RegisterDefaultPatchBaseline",
        &json!({ "BaselineId": baseline_id }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case}: the register must succeed, got {status} {registered}"
    );

    delete_ok(&router, case, REGION, &baseline_id).await;

    // Every read that resolves a `BaselineId` and models the absent-baseline refusal:
    // (operation, its request body).
    let readers: &[(&str, Value)] = &[
        ("GetPatchBaseline", json!({ "BaselineId": baseline_id })),
        (
            "UpdatePatchBaseline",
            json!({ "BaselineId": baseline_id, "Description": "a write of a removed baseline" }),
        ),
        (
            "DescribeEffectivePatchesForPatchBaseline",
            json!({ "BaselineId": baseline_id }),
        ),
        (
            "RegisterDefaultPatchBaseline",
            json!({ "BaselineId": baseline_id }),
        ),
    ];
    for (operation, body) in readers {
        let (status, response) = call_in_region(&router, REGION, operation, body).await;
        assert_eq!(
            status, 400,
            "case {case}: {operation} of the removed baseline must be refused, \
             got {status} {response}"
        );
        assert_eq!(
            error_code(&response),
            NOT_FOUND_CODE,
            "case {case}: {operation} expected {NOT_FOUND_CODE}, got {response}"
        );
        assert_eq!(
            error_message(&response),
            NOT_FOUND_MESSAGE,
            "case {case}: {operation} expected {NOT_FOUND_MESSAGE:?}, got {response}"
        );
    }

    let (status, listing) = call_in_region(&router, REGION, "DescribePatchBaselines", &json!({})).await;
    assert_eq!(
        status, 200,
        "case {case}: DescribePatchBaselines must succeed, got {status} {listing}"
    );
    let listed: Vec<String> = listing["BaselineIdentities"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case}: a listing reports BaselineIdentities, got {listing}"))
        .iter()
        .map(|identity| identity["BaselineId"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(
        !listed.contains(&baseline_id),
        "case {case}: the listing must not report the removed baseline {baseline_id:?}, \
         got {listed:?}"
    );

    let (status, default) = call_in_region(
        &router,
        REGION,
        "GetDefaultPatchBaseline",
        &json!({ "OperatingSystem": "WINDOWS" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case}: GetDefaultPatchBaseline must succeed, got {status} {default}"
    );
    assert_eq!(
        default["BaselineId"].as_str().unwrap_or_default(),
        predefined,
        "case {case}: the default must fall back to {predefined:?}, got {default}"
    );
}

/// A delete removes the baseline it names and nothing else: every other baseline the
/// endpoint holds stays readable, and a baseline of another Region is out of a delete's
/// reach even when the delete names its id.
#[tokio::test]
async fn a_delete_leaves_every_other_baseline_where_it_was() {
    let router = router();
    let case = "one of several baselines is deleted";

    // Several baselines at one endpoint, so a delete has neighbours to leave alone.
    let mut held: Vec<(String, String)> = Vec::new();
    for (name, operating_system) in [
        ("Deletion.Keep.Windows", "WINDOWS"),
        ("Deletion.Keep.Suse", "SUSE"),
        ("Deletion.Keep.Debian", "DEBIAN"),
        ("Deletion.Goes.Centos", "CENTOS"),
    ] {
        let baseline_id = create_ok(
            &router,
            case,
            REGION,
            &json!({
                "Name": name,
                "OperatingSystem": operating_system,
                "ApprovedPatches": [APPROVED_PATCH],
            }),
        )
        .await;
        held.push((name.to_string(), baseline_id));
    }
    let elsewhere = create_ok(
        &router,
        case,
        OTHER_REGION,
        &json!({
            "Name": "Deletion.OtherRegion",
            "OperatingSystem": "WINDOWS",
            "ApprovedPatches": [APPROVED_PATCH],
        }),
    )
    .await;

    let (_, removed) = held
        .pop()
        .unwrap_or_else(|| panic!("case {case}: the table created a baseline to remove"));
    delete_ok(&router, case, REGION, &removed).await;
    assert_absent(&router, case, REGION, &removed).await;

    for (name, baseline_id) in &held {
        let (status, response) = get(&router, REGION, baseline_id).await;
        assert_eq!(
            status, 200,
            "case {case}: {name} must stay readable, got {status} {response}"
        );
        assert_eq!(
            response["Name"].as_str().unwrap_or_default(),
            name,
            "case {case}: expected the baseline named {name}, got {response}"
        );
    }

    // A baseline is reached through the endpoint of one Region, so a delete signed for
    // another Region reaches no record of it.
    delete_ok(&router, case, REGION, &elsewhere).await;
    let (status, response) = get(&router, OTHER_REGION, &elsewhere).await;
    assert_eq!(
        status, 200,
        "case {case}: a delete at another endpoint must leave {elsewhere:?} readable, \
         got {status} {response}"
    );
}

/// Deleting the same baseline again reports the same id the first delete did, and the
/// baseline stays absent: this operation's modelled error set carries no refusal for a
/// `BaselineId` that reaches no record — unlike the reads, which model
/// `DoesNotExistException` — so a repeated delete, or a delete of an id nothing was ever
/// held under, reports "the ID of the deleted patch baseline" it was given.
#[tokio::test]
async fn deleting_a_baseline_again_reports_the_id_it_was_given() {
    let router = router();
    let case = "the same baseline is deleted twice";

    let baseline_id = create_ok(
        &router,
        case,
        REGION,
        &json!({
            "Name": "Deletion.Repeated",
            "OperatingSystem": "WINDOWS",
            "ApprovedPatches": [APPROVED_PATCH],
        }),
    )
    .await;
    let first = delete_ok(&router, case, REGION, &baseline_id).await;
    assert_eq!(
        first, baseline_id,
        "case {case}: the first delete reports the removed id, got {first:?}"
    );

    // (case, the id a later delete names).
    let cases: &[(&str, String)] = &[
        ("the id just removed", baseline_id.clone()),
        ("the ARN of the id just removed", baseline_arn(REGION, &baseline_id)),
        // A well formed id nothing was ever held under: `BaselineId` is "len 20..128,
        // pattern ^[a-zA-Z0-9_\-:/]{20,128}$" (`smithy_query(query="operation",
        // operation="DeletePatchBaseline")`), which this spelling satisfies.
        ("an id nothing was ever held under", "pb-00000000000000000".to_string()),
    ];
    for (named, value) in cases {
        let reported = delete_ok(&router, named, REGION, value).await;
        assert_eq!(
            &reported, value,
            "case {named}: a delete that removed nothing reports the id it was given, \
             got {reported:?}"
        );
        assert_absent(&router, named, REGION, value).await;
    }
}

/// A baseline Systems Manager predefines is Patch Manager's own rather than the account's,
/// so this endpoint holds no record of one for a delete to remove and a read still reports
/// it: it never stopped existing.
#[tokio::test]
async fn a_predefined_baseline_is_not_the_accounts_to_remove() {
    let router = router();

    // (case, the operating system whose predefined baseline the delete names). Each is a
    // value of the modelled `OperatingSystem` enum (`smithy_query(query="shape",
    // shape="OperatingSystem")`) that Patch Manager predefines a baseline for.
    let cases: &[(&str, &str)] = &[
        ("the predefined windows baseline", "WINDOWS"),
        ("the predefined ubuntu baseline", "UBUNTU"),
        ("the predefined amazon linux 2023 baseline", "AMAZON_LINUX_2023"),
        ("the predefined macos baseline", "MACOS"),
        ("the predefined debian baseline", "DEBIAN"),
    ];

    for (case, operating_system) in cases {
        let (status, default) = call_in_region(
            &router,
            REGION,
            "GetDefaultPatchBaseline",
            &json!({ "OperatingSystem": operating_system }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: GetDefaultPatchBaseline must succeed, got {status} {default}"
        );
        let predefined = default["BaselineId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: a read reports a BaselineId, got {default}"))
            .to_string();

        let (status, before) = get(&router, REGION, &predefined).await;
        assert_eq!(
            status, 200,
            "case {case}: the predefined baseline must be readable, got {status} {before}"
        );

        // Either spelling of that baseline: the full ARN a read names it by, and the bare id
        // that ARN carries. Both reach the same baseline, and the delete reports it in the
        // spelling this service NAMES a baseline of Patch Manager's by — its full ARN.
        for named in [predefined.clone(), id_of_arn(&predefined)] {
            let reported = delete_ok(&router, case, REGION, &named).await;
            assert_eq!(
                reported, predefined,
                "case {case}: a delete naming {named:?} reports the ARN the service names that \
                 baseline by, got {reported:?}"
            );

            let (status, after) = get(&router, REGION, &predefined).await;
            assert_eq!(
                status, 200,
                "case {case}: the predefined baseline must stay readable, got {status} {after}"
            );
            assert_eq!(
                after, before,
                "case {case}: a delete naming {named:?} changes nothing about that baseline, \
                 got {after}"
            );
        }
    }
}
