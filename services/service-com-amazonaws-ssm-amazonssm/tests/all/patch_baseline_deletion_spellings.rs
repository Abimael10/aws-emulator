// SPDX-License-Identifier: CC0-1.0

//! The `BaselineId` values `DeletePatchBaseline` reaches no baseline of this endpoint with,
//! and what a delete removes when the record it names was last written by an update — the
//! dimensions the sibling suite `all/patch_baseline_deletion.rs` does not vary. Its harness
//! is reused rather than copied: the create, the delete, the read and the absent-baseline
//! assertion all come from that module, and the default-baseline read from
//! `all/patch_baseline_listing.rs`, so there is one seam for every suite.
//!
//! Every case goes through the front door a client uses — the shared harness in
//! `all/parameter_store.rs` dispatches `POST /` with `X-Amz-Target: AmazonSSM.<Operation>`
//! through the multi-service router. Nothing reaches into the state struct, so a response
//! that is not computed from persisted state cannot pass: every read follows a write.
//!
//! The rule under test is the one a delete follows for the id it is given: the value resolves
//! the way every "id or ARN" member of this service resolves one — a bare id, or a full ARN
//! of THIS endpoint's Region and account — so a value whose ARN names another account,
//! another Region or another resource type reaches no record here, removes nothing and is
//! reported back as it was given, this operation modelling no refusal for an id it holds
//! nothing under; and the record a delete does reach is removed whatever that record holds
//! and whatever wrote it last, together with the default registration that named it. The
//! tables' rows differ along which part of an ARN puts a baseline out of this endpoint's
//! reach, and along the Region and operating system a rewritten baseline's recorded default
//! is held under.

use serde_json::json;

use crate::parameter_store::{call_in_region, router, ACCOUNT, REGION};
use crate::patch_baseline_deletion::{
    assert_absent, baseline_arn, create_ok, delete_ok, get, patch_rule, APPROVED_PATCH,
    OTHER_REGION,
};
use crate::patch_baseline_listing::default_baseline_id;

/// A value whose ARN names another account, another Region or another resource type reaches
/// no baseline of the endpoint it is sent to: nothing is removed, the value is reported back
/// as it was given, and the baselines this endpoint and the other Region hold stay readable.
///
/// The rows differ along which part of the ARN puts the baseline out of this endpoint's
/// reach.
#[tokio::test]
async fn an_arn_of_another_account_region_or_resource_removes_nothing() {
    let router = router();
    let setup = "the baselines a delete of a foreign ARN must leave alone";

    let held = create_ok(
        &router,
        setup,
        REGION,
        &json!({
            "Name": "Spelling.Held",
            "OperatingSystem": "WINDOWS",
            "ApprovedPatches": [APPROVED_PATCH],
        }),
    )
    .await;
    let elsewhere = create_ok(
        &router,
        setup,
        OTHER_REGION,
        &json!({
            "Name": "Spelling.Elsewhere",
            "OperatingSystem": "SUSE",
            "ApprovedPatches": ["bash"],
        }),
    )
    .await;

    // (case, the value the delete names at this endpoint).
    let cases: Vec<(&str, String)> = vec![
        (
            "an ARN naming this Region's baseline in another account",
            format!("arn:aws:ssm:{REGION}:111122223333:patchbaseline/{held}"),
        ),
        (
            "an ARN naming the other Region's baseline",
            baseline_arn(OTHER_REGION, &elsewhere),
        ),
        (
            "an ARN whose resource is not a patch baseline",
            format!("arn:aws:ssm:{REGION}:{ACCOUNT}:document/Spelling-Document"),
        ),
    ];

    for (case, named) in cases {
        let reported = delete_ok(&router, case, REGION, &named).await;
        assert_eq!(
            reported, named,
            "case {case}: a delete that removed nothing reports the value it was given, got \
             {reported:?}"
        );
        let (status, response) = get(&router, REGION, &held).await;
        assert_eq!(
            status, 200,
            "case {case}: this endpoint's baseline must stay readable, got {status} {response}"
        );
        assert_eq!(
            response["Name"].as_str().unwrap_or_default(),
            "Spelling.Held",
            "case {case}: expected the baseline this endpoint holds, got {response}"
        );
        let (status, response) = get(&router, OTHER_REGION, &elsewhere).await;
        assert_eq!(
            status, 200,
            "case {case}: the other Region's baseline must stay readable, got {status} {response}"
        );
        assert_eq!(
            response["Name"].as_str().unwrap_or_default(),
            "Spelling.Elsewhere",
            "case {case}: expected the baseline the other Region holds, got {response}"
        );
    }
}

/// The delete removes whatever the record holds when it runs: a baseline an update rewrote
/// goes the way one nothing has written since its create does, it is reported by its own id
/// from the ARN spelling, and the default recorded for its operating system falls back to the
/// baseline Patch Manager predefines — at whichever Region's endpoint, and for whichever
/// operating system, the register recorded it.
///
/// The rows differ along the Region the endpoint is and the operating system the baseline
/// applies to.
#[tokio::test]
async fn a_rewritten_baseline_and_the_default_that_named_it_both_go() {
    let router = router();

    // (case, the Region the endpoint is, the operating system the baseline applies to).
    let cases: &[(&str, &str, &str)] = &[
        ("a non-windows default at this endpoint", REGION, "UBUNTU"),
        (
            "a default at another Region's endpoint",
            OTHER_REGION,
            "REDHAT_ENTERPRISE_LINUX",
        ),
    ];

    for (case, region, operating_system) in cases {
        let predefined = default_baseline_id(&router, case, region, operating_system).await;
        let own = create_ok(
            &router,
            case,
            region,
            &json!({
                "Name": format!("Spelling.Rewritten.{operating_system}"),
                "OperatingSystem": operating_system,
                "ApprovedPatches": [APPROVED_PATCH],
            }),
        )
        .await;

        // An update is a write of the baseline, so the record the delete removes is the one
        // the update left behind rather than the one the create wrote.
        let (status, updated) = call_in_region(
            &router,
            region,
            "UpdatePatchBaseline",
            &json!({
                "BaselineId": own,
                "Description": "rewritten before the delete",
                "ApprovalRules": { "PatchRules": [patch_rule("Security")] },
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: UpdatePatchBaseline must succeed, got {status} {updated}"
        );

        let (status, registered) = call_in_region(
            &router,
            region,
            "RegisterDefaultPatchBaseline",
            &json!({ "BaselineId": baseline_arn(region, &own) }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: RegisterDefaultPatchBaseline must succeed, got {status} {registered}"
        );
        assert_eq!(
            default_baseline_id(&router, case, region, operating_system).await,
            own,
            "case {case}: the register must record the account's own baseline as the default"
        );

        let reported = delete_ok(&router, case, region, &baseline_arn(region, &own)).await;
        assert_eq!(
            reported, own,
            "case {case}: the delete must report the own id of the baseline it removed, got \
             {reported:?}"
        );
        assert_absent(&router, case, region, &own).await;
        assert_eq!(
            default_baseline_id(&router, case, region, operating_system).await,
            predefined,
            "case {case}: the default must fall back to {predefined:?} once nothing holds the \
             registered baseline"
        );
    }
}
