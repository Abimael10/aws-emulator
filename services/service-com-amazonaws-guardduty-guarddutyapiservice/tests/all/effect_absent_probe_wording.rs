// SPDX-License-Identifier: CC0-1.0

//! What the EFFECT-ABSENT PROBE is told: the words a read answers with when the create it
//! follows was refused, or the delete it follows was accepted, over the restJson1 wire.
//!
//! Every client that writes checks whether the write took effect, so the read that follows
//! a refused create and the read that follows an accepted delete are the two calls whose
//! wording a lifecycle is steered by. The rules under test:
//!
//! 1. A read whose resource is absent answers the ONE fixed sentence its family words that
//!    absence with — nothing of the request interpolated, and no claim that the identifier
//!    belongs to another account, which is a distinction the service does not draw outside
//!    the detector family. It holds however the resource came to be absent: a create the
//!    service refused, and a delete it accepted.
//! 2. The sentence is a LOOKUP and not a constant: the same read over an identifier the
//!    service does hold reports the resource back.
//! 3. An accepted delete lifts what the resource held, so the protection it carried can be
//!    taken again for the same target and the conflict the first create raised is raised
//!    again by the second — the absence the probe reported is real in both directions.
//!
//! The rows differ along what the rules generalise over: the resource FAMILY (a filter
//! under a detector, a Malware Protection plan addressed on its own), HOW the resource came
//! to be absent (a refused create, an accepted delete), the FORM of the request that was
//! refused (each published bound of the refused member's range and values past it), and the
//! FORM of the target the plan protects (a plain bucket name, a dotted one, one with object
//! prefixes).
//!
//! The harness is the `detector_lifecycle` suite's, so routing, restJson1
//! (de)serialization, modelled-constraint validation and the handlers are exercised
//! together.

use serde_json::{json, Value};

use crate::detector_lifecycle::{call, enabled_detector, error_code, error_shape, new_router};

/// The modelled error SHAPE restJson1 resolves every 400 of this service to.
const ENVELOPE: &str = "BadRequestException";

/// The error CLASS a 400 carries in the body's `__type`.
const INVALID_INPUT: &str = "InvalidInputException";

/// The modelled not-found error the Malware Protection plan operations declare
/// (`smithy_query(operation="GetMalwareProtectionPlan")`: `ResourceNotFoundException`,
/// `httpError` 404).
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// What a value outside a constraint the model declares is refused with.
const INVALID_INPUT_PARAMETERS: &str =
    "The request is rejected because the one or more input parameters have invalid values.";

/// What a filter READ answers for a name the detector does not hold.
const NO_SUCH_RESOURCE: &str = "The request is rejected since no such resource found.";

/// What the Malware Protection plan operations answer for a plan id this account and
/// Region do not hold.
const PLAN_NOT_FOUND: &str =
    "The request was rejected because the specified malware protection plan was not found.";

/// What a second plan for an already-protected bucket is refused with.
const ALREADY_PROTECTED: &str =
    "The request was rejected because the provided S3 bucket is already protected.";

/// The claim no family but the detector's may make about an absent identifier.
const OWNERSHIP_CLAIM: &str = "is not owned by the current account";

/// The collection URI of the Malware Protection plans
/// (`smithy_query(operation="CreateMalwareProtectionPlan")`).
const PLANS_PATH: &str = "/malware-protection-plan";

/// An IAM role ARN of the form `CreateMalwareProtectionPlan.Role` takes.
const PLAN_ROLE: &str = "arn:aws:iam::000000000000:role/GuardDutyMalwareProtectionRole";

/// The published bounds of `CreateFilter.Rank` (`@range` 1..100 —
/// `smithy_query(operation="CreateFilter")`) and, for each, one value past it: the rows
/// the refused creates below are written over, each with whether the create is inside the
/// range.
const RANKS: [(i64, bool, &str); 6] = [
    (1, true, "the published minimum"),
    (100, true, "the published maximum"),
    (0, false, "one below the published minimum"),
    (-1, false, "below the minimum and negative"),
    (101, false, "one above the published maximum"),
    (150, false, "well above the published maximum"),
];

/// A `CreateFilter` body naming `name` and `rank`.
fn create_filter_body(name: &str, rank: i64) -> Value {
    json!({
        "name": name,
        "action": "NOOP",
        "rank": rank,
        "findingCriteria": {"criterion": {"severity": {"greaterThanOrEqual": 1}}},
    })
}

/// Assert one refusal: its status, its error class, the fixed sentence word for word, that
/// the identifier the request sent is not quoted back, and that no ownership claim is made.
fn assert_refusal(case: &str, status: u16, body: &Value, expected: &str, class: &str, code: u16) {
    assert_eq!(
        status, code,
        "case {case:?}: expected HTTP {code}, got {status} {body:?}"
    );
    assert_eq!(
        error_code(body),
        class,
        "case {case:?}: expected the {class} class, got {body:?}"
    );
    let message = body["message"].as_str().unwrap_or_default();
    assert_eq!(
        message, expected,
        "case {case:?}: expected message {expected:?}, got {message:?}"
    );
    assert!(
        !message.contains(OWNERSHIP_CLAIM),
        "case {case:?}: no family but the detector's claims an ownership problem, got \
         {message:?}"
    );
}

/// The names a detector's filter listing reports.
async fn listed_filters(router: &crate::detector_lifecycle::Router, detector: &str) -> Vec<String> {
    let (status, body) = call(
        router,
        "GET",
        &format!("/detector/{detector}/filter?maxResults=50"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"the detector's filters\": expected 200, got {status} {body:?}"
    );
    body["filterNames"]
        .as_array()
        .map(|names| {
            names
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// RULES 1 and 2 for the filter family: the probe that follows a REFUSED create is told the
/// filter read's own absence sentence, with the name it asked about neither quoted back nor
/// claimed to belong to another account; the probe that follows an ACCEPTED create reports
/// the filter.
#[tokio::test]
async fn the_probe_after_a_refused_create_is_told_the_filters_own_absence() {
    let router = new_router();
    let detector = enabled_detector(&router).await;

    for (rank, inside, why) in RANKS {
        let name = format!("probe-rank-{}", rank.rem_euclid(1_000));
        let case = format!("rank {rank} ({why})");
        let (status, answered) = call(
            &router,
            "POST",
            &format!("/detector/{detector}/filter"),
            Some(create_filter_body(&name, rank)),
        )
        .await;

        if inside {
            // Rule 2: a create the service ACCEPTED is what the same read reports back, so
            // the sentence below is a lookup over the store.
            assert_eq!(
                status, 200,
                "case {case:?}: expected the create's own 200, got {status} {answered:?}"
            );
            let (status, read) = call(
                &router,
                "GET",
                &format!("/detector/{detector}/filter/{name}"),
                None,
            )
            .await;
            assert_eq!(
                (status, read["rank"].as_i64()),
                (200, Some(rank)),
                "case {case:?}: expected the read to report the stored rank, got {status} \
                 {read:?}"
            );
            assert!(
                listed_filters(&router, &detector).await.contains(&name),
                "case {case:?}: expected the accepted filter in the listing"
            );
            continue;
        }

        // The create is refused for the value outside the modelled range. A refusal the
        // REQUEST BOUNDARY made carries the modelled envelope shape as its class, where a
        // refusal a handler made carries this service's own 400 class ([`INVALID_INPUT`]).
        assert_refusal(&case, status, &answered, INVALID_INPUT_PARAMETERS, ENVELOPE, 400);

        // Rule 1: the probe that follows is told the filter family's absence sentence.
        let (status, probed) = call(
            &router,
            "GET",
            &format!("/detector/{detector}/filter/{name}"),
            None,
        )
        .await;
        let probe_case = format!("the probe after {case}");
        assert_refusal(
            &probe_case,
            status,
            &probed,
            NO_SUCH_RESOURCE,
            INVALID_INPUT,
            400,
        );
        assert_eq!(
            error_shape(&probed),
            ENVELOPE,
            "case {probe_case:?}: expected the {ENVELOPE} envelope shape, got {probed:?}"
        );
        assert!(
            !probed.to_string().contains(&name),
            "case {probe_case:?}: the refusal must not quote the name back, got {probed:?}"
        );
        assert!(
            !listed_filters(&router, &detector).await.contains(&name),
            "case {probe_case:?}: a refused create leaves no filter in the listing"
        );
    }
}

/// RULES 1, 2 and 3 for the Malware Protection plan family: the probe that follows an
/// ACCEPTED delete is told the plan family's fixed not-found sentence, and the protection
/// the delete lifted can be taken again for the same bucket — which the conflict then
/// tracks against the new plan.
#[tokio::test]
async fn the_probe_after_an_accepted_delete_is_told_the_plans_own_absence() {
    let router = new_router();

    // The rows differ in the FORM of the protected target, which is what a plan is keyed
    // by: a plain bucket name, a dotted one, and one carrying object prefixes.
    for (bucket, prefixes, why) in [
        ("guardduty-probe-plain", None, "a plain bucket name"),
        ("probe.with.dots", None, "a dotted bucket name"),
        (
            "guardduty-probe-prefixed",
            Some(json!(["incoming/", "quarantine/"])),
            "a bucket with object prefixes",
        ),
    ] {
        let mut protected = json!({"bucketName": bucket});
        if let Some(prefixes) = prefixes.clone() {
            protected["objectPrefixes"] = prefixes;
        }
        let body = json!({"role": PLAN_ROLE, "protectedResource": {"s3Bucket": protected}});

        let (status, created) = call(&router, "POST", PLANS_PATH, Some(body.clone())).await;
        assert_eq!(
            status, 200,
            "case {why:?}: expected the create's own 200, got {status} {created:?}"
        );
        let plan = created["malwareProtectionPlanId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {why:?}: expected a plan id, got {created:?}"))
            .to_string();

        // Rule 2: the read reports the plan the create minted.
        let (status, read) = call(&router, "GET", &format!("{PLANS_PATH}/{plan}"), None).await;
        assert_eq!(
            (
                status,
                read["protectedResource"]["s3Bucket"]["bucketName"].as_str()
            ),
            (200, Some(bucket)),
            "case {why:?}: expected the read to report the protected bucket, got {status} \
             {read:?}"
        );

        // The bucket is protected, so a second create for it conflicts.
        let (status, conflict) = call(&router, "POST", PLANS_PATH, Some(body.clone())).await;
        assert_refusal(
            &format!("a second plan for {why}"),
            status,
            &conflict,
            ALREADY_PROTECTED,
            "ConflictException",
            409,
        );

        let (status, deleted) = call(&router, "DELETE", &format!("{PLANS_PATH}/{plan}"), None).await;
        assert_eq!(
            status, 200,
            "case {why:?}: expected the delete's own 200, got {status} {deleted:?}"
        );

        // Rule 1: the probe that follows the delete is told the plan family's own
        // not-found sentence, under the 404 its model declares, with the id neither quoted
        // back nor said to belong to another account.
        let probe_case = format!("the probe after deleting the plan for {why}");
        let (status, probed) = call(&router, "GET", &format!("{PLANS_PATH}/{plan}"), None).await;
        assert_refusal(
            &probe_case,
            status,
            &probed,
            PLAN_NOT_FOUND,
            RESOURCE_NOT_FOUND,
            404,
        );
        assert!(
            !probed.to_string().contains(&plan),
            "case {probe_case:?}: the refusal must not quote the plan id back, got {probed:?}"
        );

        // Rule 3: the delete lifted the protection, so the same bucket can be protected
        // again — by a plan of its own, which the conflict then tracks.
        let (status, again) = call(&router, "POST", PLANS_PATH, Some(body.clone())).await;
        assert_eq!(
            status, 200,
            "case {why:?}: expected the bucket to be protectable again, got {status} {again:?}"
        );
        let second = again["malwareProtectionPlanId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {why:?}: expected a plan id, got {again:?}"))
            .to_string();
        assert_ne!(
            second, plan,
            "case {why:?}: expected a plan of its own rather than the deleted one"
        );
        let (status, read) = call(&router, "GET", &format!("{PLANS_PATH}/{second}"), None).await;
        assert_eq!(
            (
                status,
                read["protectedResource"]["s3Bucket"]["bucketName"].as_str()
            ),
            (200, Some(bucket)),
            "case {why:?}: expected the re-created plan to read back, got {status} {read:?}"
        );
        let (status, conflict) = call(&router, "POST", PLANS_PATH, Some(body)).await;
        assert_refusal(
            &format!("a second plan for {why} after the restore"),
            status,
            &conflict,
            ALREADY_PROTECTED,
            "ConflictException",
            409,
        );

        // And the id the delete removed is still absent: the restore minted a new plan
        // rather than reviving the old one.
        let (status, probed) = call(&router, "GET", &format!("{PLANS_PATH}/{plan}"), None).await;
        assert_refusal(
            &format!("{probe_case}, after the restore"),
            status,
            &probed,
            PLAN_NOT_FOUND,
            RESOURCE_NOT_FOUND,
            404,
        );
    }
}
