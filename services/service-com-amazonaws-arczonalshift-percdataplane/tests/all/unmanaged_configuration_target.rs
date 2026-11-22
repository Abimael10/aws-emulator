// SPDX-License-Identifier: CC0-1.0

//! What ARC STATES when a call on a resource's practice run configuration names a resource
//! it does not manage for the caller's account.
//!
//! RULE: the operations whose subject is a resource's practice run configuration — `POST
//! /configuration`, `PATCH /configuration/{resourceIdentifier}` and `DELETE
//! /configuration/{resourceIdentifier}` — have the caller's authorization on the named
//! target decided before the target is resolved, so a `resourceIdentifier` ARC holds no
//! managed resource of this account for is denied 403 `AccessDeniedException`, and that
//! denial NAMES ITS SUBJECT: `Resource <the resourceIdentifier the request carried> is not
//! a managed resource for account ID <the account this emulator answers for>.` Both
//! variable parts are read rather than written — the identifier off the request, the
//! account id off the one place this emulator states its account — so the sentence is a
//! template and no part of it is one call's value.
//!
//! What a denial of this service states is PER OPERATION, and the boundary is asserted
//! here too: a traffic-shift mutation on the very same unheld identifier keeps the
//! non-disclosing sentence it is recorded answering (the table over those targets lives in
//! `access_denied_precedence.rs`), and a denial taken on a `zonalShiftId`, which resolved
//! no target to speak about, states no reason at all (`refusal_disclosure.rs`). The
//! document the text is carried in — `Message`, with no class discriminator — is
//! `access_denial_envelope.rs`'s subject.
//!
//! The rows differ along what the rule generalises over: the FORM and SIZE of the
//! identifier (ARN-shaped and not, this account and another, another Region, each end of
//! `ResourceIdentifier`'s modelled `@length` 8..1024, a near miss on an identifier the
//! store holds), the OPERATION the denial was reached through, and the configuration the
//! call carried. The last case is the boundary: a target the store DOES hold is served, so
//! the denial is computed from the store rather than answered unconditionally.

use serde_json::{json, Value};

use super::practice_run_configuration::{assert_holds, get_managed_resource, hold, Row, ROWS};
use super::zonal_shift_lifecycle::{call, emulator, encoded, refused, seeded, start_body};
use service_com_amazonaws_arczonalshift_percdataplane::state::TARGET_UNAVAILABLE_MESSAGE;

/// The sentence ARC states for this refusal, with its two variable parts filled.
///
/// Written out here rather than taken from the handler's own formatter, so this suite pins
/// AWS's wording instead of re-asserting the implementation's string. The account id is the
/// one this emulator answers for, read from the single place that states it.
pub(crate) fn not_a_managed_resource_message(resource_identifier: &str) -> String {
    format!(
        "Resource {resource_identifier} is not a managed resource for account ID {}.",
        engine::emulator_account_id()
    )
}

/// An ARN-shaped `resourceIdentifier` of exactly `length` characters that names nothing ARC
/// manages, padded in its resource part — so each end of `ResourceIdentifier`'s modelled
/// `@length` 8..1024 (`smithy_query(query="shape", shape="ResourceIdentifier")`) is a row.
fn padded_arn(length: usize) -> String {
    const PREFIX: &str = "arn:aws:eks:us-east-1:000000000000:cluster/";
    format!("{PREFIX}{}", "p".repeat(length - PREFIX.len()))
}

/// `resourceIdentifier`s ARC holds no managed resource for, as `(identifier, class)`.
///
/// The forms come from the model: `ResourceIdentifier` is length 8..1024 with no pattern,
/// and its documentation says "The identifier is the Amazon Resource Name (ARN) for the
/// resource" — so an ARN naming an unregistered resource, an ARN of another account or
/// Region, a value that is not an ARN at all and each end of the length range are all
/// identifiers a caller can reach this branch with.
fn unheld_targets() -> Vec<(String, &'static str)> {
    vec![
        (
            "arn:aws:elasticloadbalancing:us-east-1:000000000000:loadbalancer/app/never-registered/1a2b3c4d5e6f7890"
                .to_string(),
            "a load balancer ARN of this account that ARC does not manage",
        ),
        (
            "arn:aws:elasticloadbalancing:us-east-1:111111111111:loadbalancer/app/absent/73e2d6bc24d8a067"
                .to_string(),
            "a load balancer ARN of another account",
        ),
        (
            "arn:aws:eks:eu-west-1:000000000000:cluster/absent".to_string(),
            "an EKS cluster ARN of another Region",
        ),
        (
            "arn:aws:autoscaling:us-east-1:000000000000:autoScalingGroupName/absent".to_string(),
            "an Auto Scaling group ARN ARC has no record of",
        ),
        (
            "arn:this-is-not-a-well-formed-arn".to_string(),
            "an ARN-prefixed value with no further structure",
        ),
        (
            "abcdefgh".to_string(),
            "the shortest identifier the model accepts",
        ),
        (
            "zonal-shift-alb".to_string(),
            "a resource name rather than its ARN",
        ),
        (
            padded_arn(1024),
            "the longest identifier the model accepts",
        ),
    ]
}

/// Every operation whose subject is a resource's practice run configuration, as
/// `(operation, method, uri, body)` for a call on `target`. The routes are the model's
/// (`smithy_query(query="digest")`, reproduced in the crate's `OPERATIONS.md`); `row` is the
/// configuration the call carries, varied across the table so the branch is shown to be
/// taken whatever else the request asks for.
fn configuration_calls(
    target: &str,
    row: &Row,
) -> Vec<(&'static str, &'static str, String, String)> {
    let labelled = format!("/configuration/{}", encoded(target));
    vec![
        (
            "CreatePracticeRunConfiguration",
            "POST",
            "/configuration".to_string(),
            row.create_body(target),
        ),
        (
            "UpdatePracticeRunConfiguration",
            "PATCH",
            labelled.clone(),
            Value::Object(row.configuration()).to_string(),
        ),
        (
            "DeletePracticeRunConfiguration",
            "DELETE",
            labelled,
            String::new(),
        ),
    ]
}

/// The member names a JSON document carries, in order.
fn members(document: &Value) -> Vec<&str> {
    document
        .as_object()
        .map(|object| object.keys().map(String::as_str).collect())
        .unwrap_or_default()
}

#[tokio::test]
async fn a_configuration_call_on_an_unmanaged_target_names_the_resource_and_the_account() {
    let (router, engine) = emulator();
    let mut targets = unheld_targets();
    // A near miss on a target the store DOES hold: the same ARN with one more character is
    // still an identifier ARC manages no resource for.
    let (held, _zones) = seeded(&engine)[0].clone();
    targets.push((
        format!("{held}0"),
        "a near miss on an identifier the store holds",
    ));

    for (index, (target, class)) in targets.iter().enumerate() {
        let row = &ROWS[index % ROWS.len()];
        for (operation, method, uri, body) in configuration_calls(target, row) {
            let case = format!("{operation}/{class}/{}", row.case);
            let (status, answered, document) = refused(&router, method, &uri, &body).await;
            assert_eq!(
                status, 403,
                "case {case:?}: a target ARC manages no resource for is denied 403, \
                 got {status} {document}"
            );
            assert_eq!(
                answered, "AccessDeniedException",
                "case {case:?}: expected AccessDeniedException, got {answered:?} \
                 with {document}"
            );
            let expected = not_a_managed_resource_message(target);
            assert_eq!(
                document["Message"].as_str(),
                Some(expected.as_str()),
                "case {case:?}: expected {expected:?}, got {document}"
            );
            assert_eq!(
                members(&document),
                vec!["Message"],
                "case {case:?}: the denial carries the text and nothing else, got {document}"
            );
        }
    }
}

#[tokio::test]
async fn the_denial_is_a_template_filled_from_the_request_and_the_account() {
    // The two variable parts are READ, not written: each denial names the identifier its own
    // call carried and the account this emulator answers for, so no two targets of the table
    // are refused the same sentence.
    let (router, _engine) = emulator();
    let mut stated: Vec<(String, String)> = Vec::new();
    for (target, class) in unheld_targets() {
        let (_status, _answered, document) = refused(
            &router,
            "POST",
            "/configuration",
            &ROWS[0].create_body(&target),
        )
        .await;
        let message = document["Message"].as_str().unwrap_or_default().to_string();
        assert!(
            message.contains(&target),
            "case {class:?}: the denial names the identifier the request carried, \
             got {message:?}"
        );
        assert!(
            message.contains(engine::emulator_account_id()),
            "case {class:?}: the denial names the account the call was made in, got {message:?}"
        );
        if let Some((earlier, earlier_message)) =
            stated.iter().find(|(_, earlier)| earlier == &message)
        {
            panic!(
                "case {class:?}: a denial for {target:?} must differ from the one for \
                 {earlier:?}, got {earlier_message:?} twice"
            );
        }
        stated.push((class.to_string(), message));
    }
}

#[tokio::test]
async fn a_traffic_shift_mutation_on_the_same_identifier_keeps_its_own_wording() {
    // The boundary of the rule: what a denial of this service states is per operation. The
    // very same unheld identifiers, on the mutation that starts a traffic shift for a
    // resource, are refused with the non-disclosing sentence instead.
    let (router, _engine) = emulator();
    for (target, class) in unheld_targets() {
        let (status, answered, document) = refused(
            &router,
            "POST",
            "/zonalshifts",
            &start_body(&target, "use1-az1", "1h", "a denied shift"),
        )
        .await;
        assert_eq!(
            status, 403,
            "case {class:?}: an unheld target is denied 403, got {status} {document}"
        );
        assert_eq!(
            answered, "AccessDeniedException",
            "case {class:?}: expected AccessDeniedException, got {answered:?} with {document}"
        );
        assert_eq!(
            document["Message"].as_str(),
            Some(TARGET_UNAVAILABLE_MESSAGE),
            "case {class:?}: expected the non-disclosing sentence, got {document}"
        );
    }
}

#[tokio::test]
async fn a_target_the_store_holds_is_served_rather_than_denied() {
    // The denial is computed from the store: every one of the three operations on a target
    // the store DOES hold is served, and the configuration the create wrote reads back
    // through `GetManagedResource`.
    let (router, engine) = emulator();
    let (held, _zones) = seeded(&engine)[0].clone();
    let configuration = hold(&router, &ROWS[1], &held).await;

    let (status, view) = get_managed_resource(&router, &held).await;
    assert_eq!(
        status, 200,
        "GetManagedResource answers 200 for a managed resource, got {status} {view}"
    );
    assert_holds(
        "the configuration written for a managed resource",
        &view["practiceRunConfiguration"],
        &configuration,
    );

    let labelled = format!("/configuration/{}", encoded(&held));
    let (status, updated) = call(
        &router,
        "PATCH",
        &labelled,
        &json!({ "blockedDates": ["2026-01-01"] }).to_string(),
    )
    .await;
    assert_eq!(
        status, 200,
        "the change to a managed resource's configuration is served, got {status} {updated}"
    );
    assert!(
        updated["Message"].is_null(),
        "a served answer carries no denial text, got {updated}"
    );

    let (status, deleted) = call(&router, "DELETE", &labelled, "").await;
    assert_eq!(
        status, 200,
        "the delete of a managed resource's configuration is served, got {status} {deleted}"
    );
    assert!(
        deleted["Message"].is_null(),
        "a served answer carries no denial text, got {deleted}"
    );
}
