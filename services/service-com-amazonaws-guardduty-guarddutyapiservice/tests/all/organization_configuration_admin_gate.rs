// SPDX-License-Identifier: CC0-1.0

//! The delegated-administrator precondition of the organization-configuration
//! REPORT, end to end over the wire.
//!
//! `DescribeOrganizationConfiguration` "returns information about the account
//! selected as the delegated administrator for GuardDuty"
//! (`smithy_query(query="doc",shape="DescribeOrganizationConfiguration")`), so an
//! organization that has selected no such account has no such information to report:
//! the read is refused HTTP 400 under this service's client-error class, with the
//! service's fixed sentence for that missing precondition, and only reads the stored
//! configuration once the precondition holds.
//!
//! Every request goes through [`smithy_server_runtime::MultiServiceRouter::dispatch`]
//! (the harness lives in the `detector_lifecycle` suite), so routing, restJson1
//! serialization and the handler are exercised together.
//!
//! The rules under test, each asserted over a SET of inputs rather than one call:
//!
//! 1. The precondition is judged BEFORE anything else the request carries, so the
//!    refusal says nothing about what the account holds: the same status, class,
//!    envelope and sentence whatever detector id the request names (one the account
//!    holds, one well formed that was never minted, one not of the minted form) and
//!    whatever page window it asks for — a token this service never issued included.
//! 2. The standing is held Region by Region: the read is served through the endpoint
//!    of the Region whose organization selected this account, and refused with that
//!    same sentence through every other Region's endpoint, which holds no selection.
//! 3. The precondition is a gate, not a wall: a run of refusals writes nothing, and
//!    the same read answers the configuration an earlier update stored once the
//!    account is selected.

use serde_json::{json, Value};

use service_com_amazonaws_guardduty_guarddutyapiservice::scaffolded::DELEGATED_ADMINISTRATOR_NOT_ENABLED_MESSAGE;

use crate::detector_lifecycle::{
    as_delegated_admin, call_in, error_code, error_shape, new_router, Router, ABSENT_DETECTOR_ID,
    DEFAULT_SIGNING_REGION,
};

/// The error CLASS the body's discriminator carries on every client rejection this
/// service answers.
const CLASS: &str = "InvalidInputException";

/// The modelled error SHAPE a 400 is enveloped in, which `x-amzn-errortype` names —
/// the only client error `DescribeOrganizationConfiguration` declares
/// (`smithy_query(operation="DescribeOrganizationConfiguration")`).
const ENVELOPE: &str = "BadRequestException";

/// Regions whose endpoints each serve their own organization: a designation is held
/// by the Region the designating request was signed for and by no other, so the rows
/// of [`the_standing_is_held_region_by_region`] take each of these as the one Region
/// that selected this account.
const REGIONS: [&str; 3] = [DEFAULT_SIGNING_REGION, "eu-west-1", "ap-southeast-2"];

/// Create an enabled detector through `region`'s endpoint and return its id.
async fn detector_in(router: &Router, region: &str) -> String {
    let (status, body) = call_in(
        router,
        region,
        "POST",
        "/detector",
        Some(json!({"enable": true})),
    )
    .await;
    assert_eq!(
        status, 200,
        "create-detector in {region}: expected 200, got {status} {body:?}"
    );
    body["detectorId"]
        .as_str()
        .unwrap_or_else(|| panic!("create-detector in {region}: expected a detectorId, got {body:?}"))
        .to_string()
}

/// `DescribeOrganizationConfiguration` through `region`'s endpoint, with `query`
/// appended to the path verbatim (empty for no page window).
async fn describe_in(
    router: &Router,
    region: &str,
    detector_id: &str,
    query: &str,
) -> (u16, Value) {
    call_in(
        router,
        region,
        "GET",
        &format!("/detector/{detector_id}/admin{query}"),
        None,
    )
    .await
}

/// `UpdateOrganizationConfiguration` through `region`'s endpoint, asserting it was
/// accepted. What it stored is asserted by the read each test then makes.
async fn update_ok(router: &Router, region: &str, detector_id: &str, body: Value) {
    let (status, got) = call_in(
        router,
        region,
        "POST",
        &format!("/detector/{detector_id}/admin"),
        Some(body.clone()),
    )
    .await;
    assert_eq!(
        status, 200,
        "update {body}: expected 200, got {status} {got:?}"
    );
}

/// Assert a response is the refusal of a read whose organization selected no
/// delegated administrator: a 400 under this service's client-error class, in the
/// modelled envelope, carrying the service's sentence for the missing precondition
/// and nothing taken from the request.
fn assert_no_delegated_administrator(status: u16, got: &Value, case: &str) {
    assert_eq!(status, 400, "case {case:?}: expected 400, got {status} {got:?}");
    assert_eq!(
        error_code(got),
        CLASS,
        "case {case:?}: expected the {CLASS} class, got {got:?}"
    );
    assert_eq!(
        error_shape(got),
        ENVELOPE,
        "case {case:?}: expected the {ENVELOPE} envelope, got {got:?}"
    );
    assert_eq!(
        got["message"].as_str().unwrap_or_default(),
        DELEGATED_ADMINISTRATOR_NOT_ENABLED_MESSAGE,
        "case {case:?}: expected GuardDuty's sentence for a missing delegated \
         administrator, got {got:?}"
    );
}

// ---------------------------------------------------------------------------
// Rule 1.
// ---------------------------------------------------------------------------

/// With no account selected as the organization's delegated administrator, the read
/// is refused the same way whatever else the request carries.
///
/// The rows span the detector id (held, well formed but never minted, not of the
/// minted form at all) crossed with the page window (none, each published bound of
/// `maxResults`, and a `nextToken` this service never issued) — every one of which
/// would otherwise be judged by the handler, so a refusal that is the same sentence
/// for all of them is the precondition being judged first.
#[tokio::test]
async fn the_report_is_refused_whatever_else_the_request_carries() {
    let router = new_router();
    let held = detector_in(&router, DEFAULT_SIGNING_REGION).await;

    // (the detector id, why it is interesting)
    let ids: [(&str, &str); 3] = [
        (held.as_str(), "a detector this account holds"),
        (ABSENT_DETECTOR_ID, "well formed, never minted"),
        ("not-a-detector-id", "not of the minted form at all"),
    ];
    // `MaxResults` is `range 1..50`
    // (`smithy_query(operation="DescribeOrganizationConfiguration")`), so both
    // published bounds are in range; the token row is one no page of this service
    // ever handed out.
    let windows: [&str; 4] = ["", "?maxResults=1", "?maxResults=50", "?nextToken=not-a-token"];

    for (id, why) in ids {
        for window in windows {
            let case = format!("detector id {id:?} ({why}), window {window:?}");
            let (status, got) = describe_in(&router, DEFAULT_SIGNING_REGION, id, window).await;
            assert_no_delegated_administrator(status, &got, &case);
        }
    }
}

// ---------------------------------------------------------------------------
// Rule 2.
// ---------------------------------------------------------------------------

/// The selection is held by one Region's organization: the read is served there and
/// refused, with the same sentence, through every other Region's endpoint.
///
/// Each row takes a different Region as the one that selected this account, so no row
/// can pass by accident of which Region the harness signs for by default. Every
/// Region in the row holds a detector of its own, and every one of those detectors is
/// given a configuration that is NOT a fresh detector's default, so the served read
/// has to retrieve what was stored for the detector it was asked about.
#[tokio::test]
async fn the_standing_is_held_region_by_region() {
    for selected in REGIONS {
        let router = new_router();
        let mut detectors = Vec::new();
        for region in REGIONS {
            let detector_id = detector_in(&router, region).await;
            update_ok(
                &router,
                region,
                &detector_id,
                json!({"autoEnableOrganizationMembers": "ALL"}),
            )
            .await;
            detectors.push((region, detector_id));
        }
        as_delegated_admin(&router, selected).await;

        for (region, detector_id) in &detectors {
            let case = format!("selected in {selected}, read through {region}");
            let (status, got) = describe_in(&router, region, detector_id, "").await;
            if *region == selected {
                assert_eq!(
                    status, 200,
                    "case {case:?}: the Region that selected this account must be \
                     served, got {status} {got:?}"
                );
                assert_eq!(
                    got["autoEnableOrganizationMembers"]
                        .as_str()
                        .unwrap_or_default(),
                    "ALL",
                    "case {case:?}: the served read must report the stored \
                     configuration, got {got:?}"
                );
            } else {
                assert_no_delegated_administrator(status, &got, &case);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Rule 3.
// ---------------------------------------------------------------------------

/// The refusals write nothing, and the same read answers the stored configuration
/// once the account is selected.
///
/// The rows differ along what an update stored before the refusals, so the served
/// read is checked against a configuration the test chose rather than against a fresh
/// detector's defaults.
#[tokio::test]
async fn the_precondition_is_a_gate_and_not_a_wall() {
    // (what an update stores first, the auto-enablement it then reports, the status
    // the named feature then holds)
    let rows: [(Value, &str, &str, &str); 3] = [
        (
            json!({"autoEnableOrganizationMembers": "ALL",
                   "features": [{"name": "S3_DATA_EVENTS", "autoEnable": "NEW"}]}),
            "ALL",
            "S3_DATA_EVENTS",
            "NEW",
        ),
        (
            json!({"autoEnableOrganizationMembers": "NEW",
                   "features": [{"name": "EKS_AUDIT_LOGS", "autoEnable": "ALL"}]}),
            "NEW",
            "EKS_AUDIT_LOGS",
            "ALL",
        ),
        (
            json!({"autoEnableOrganizationMembers": "NONE",
                   "features": [{"name": "RUNTIME_MONITORING", "autoEnable": "NEW"}]}),
            "NONE",
            "RUNTIME_MONITORING",
            "NEW",
        ),
    ];

    for (stored, reported, feature, feature_status) in rows {
        let case = format!("stored {stored}");
        let router = new_router();
        let detector_id = detector_in(&router, DEFAULT_SIGNING_REGION).await;
        update_ok(&router, DEFAULT_SIGNING_REGION, &detector_id, stored.clone()).await;

        // Refused while nobody is selected, however often it is asked.
        for attempt in 1..=3 {
            let (status, got) =
                describe_in(&router, DEFAULT_SIGNING_REGION, &detector_id, "").await;
            assert_no_delegated_administrator(status, &got, &format!("{case}, attempt {attempt}"));
        }

        as_delegated_admin(&router, DEFAULT_SIGNING_REGION).await;
        let (status, served) =
            describe_in(&router, DEFAULT_SIGNING_REGION, &detector_id, "").await;
        assert_eq!(
            status, 200,
            "{case}: with the account selected the read must be served, got {status} {served:?}"
        );
        assert_eq!(
            served["autoEnableOrganizationMembers"]
                .as_str()
                .unwrap_or_default(),
            reported,
            "{case}: the served read must report what the update stored, got {served:?}"
        );
        let entry = served["features"]
            .as_array()
            .and_then(|features| {
                features
                    .iter()
                    .find(|entry| entry["name"].as_str() == Some(feature))
            })
            .unwrap_or_else(|| panic!("{case}: feature {feature} is not reported, got {served:?}"));
        assert_eq!(
            entry["autoEnable"].as_str().unwrap_or_default(),
            feature_status,
            "{case}: feature {feature} must report what the update stored, got {served:?}"
        );

        // And the served read is a report: reading again changes nothing.
        let (status, again) =
            describe_in(&router, DEFAULT_SIGNING_REGION, &detector_id, "").await;
        assert_eq!(
            status, 200,
            "{case}: the second read must be served too, got {status} {again:?}"
        );
        assert_eq!(
            again, served,
            "{case}: a report must not change what a later caller sees"
        );
    }
}
