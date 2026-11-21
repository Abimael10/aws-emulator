// SPDX-License-Identifier: CC0-1.0

//! The refusal surface every operation that resolves a subscription ARN shares, plus
//! `ListSubscriptions`' cursor refusal.
//!
//! Four rules are pinned here, each over a TABLE whose rows differ along what the rule
//! generalises over — the operation, the structural element of the ARN that is wrong, the
//! shape of a well-formed ARN, the entity tag that was sent, the token that was sent:
//!
//! 1. A value that is not a well-formed subscription ARN is refused as a malformed input:
//!    HTTP 400 `ValidationException` with `Invalid subscription ARN: <arn>`, decided from
//!    the ARN's syntax alone and so identical on every operation that takes one, and taken
//!    before the store is consulted and before the operation's other inputs are weighed.
//! 2. A well-formed ARN that names no stored subscription is HTTP 404
//!    `ResourceNotFoundException` with `Subscription not found: <arn>. Verify the ARN is
//!    correct and the subscription exists in your account.` and `resourceId` set to that
//!    ARN (`resourceId` is `@required` on the shape:
//!    `smithy_query(query="shape", shape="ResourceNotFoundException")`) — the same sentence
//!    on the reporting path and on the mutating path.
//! 3. An `If-Match` that is not the subscription's current entity tag is an input the
//!    caller must correct, not a conflict with the record's state: HTTP 400
//!    `ValidationException` with `Invalid or outdated ETag '<tag>': retrieve current ETag
//!    and retry.`, echoing the rejected tag — and the refusal is inert, so the record
//!    keeps its status, its pending change and its tag.
//! 4. A `nextToken` this service did not issue is HTTP 400 `ValidationException` with
//!    `The nextToken provided is invalid.` — a sentence naming no value from the request,
//!    because the token is opaque — while a token a page did issue is still accepted.
//! 5. A subscription is only ever to a plan this service OFFERS: a `planFamily` no plan is
//!    offered for is HTTP 400 `ValidationException` with `Invalid plan name: <planFamily>`,
//!    and a `planTier` the resolved plan does not sell is
//!    `Invalid plan tier "<planTier>" for the plan "<plan code>"` — both decided before
//!    anything is stored, so a refused create leaves no subscription behind.
//! 6. A `clientToken` is a safe-retry handle on ONE request: replayed with the same
//!    parameters it answers with the subscription it minted, and replayed with different
//!    parameters it is HTTP 409 `ConflictException` with `The clientToken matches a previous
//!    request but the request parameters differ. Reuse a clientToken only for an identical
//!    request.`, naming that subscription in `resourceId` — and neither path writes.
//! 7. A resource is covered by at most one subscription: a create naming a resource another
//!    subscription already covers is HTTP 400 `ValidationException` with `Cannot create
//!    subscription with <arn>: resource must belong to your account and not be used in
//!    another subscription.`, stores nothing, and is admitted once nothing covers the
//!    resource any more — and the operation that would ADD a resource to a subscription is
//!    held to the same rule, stated in its own terms.
//!
//! The HTTP harness is `subscription_wire_form`'s, so this file adds no second copy of it.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_pricingplanmanager_awspricingplanmanager::state::{
    encode_page_token, CLOUDFRONT_PLAN_CODE, LIST_PAGE_SIZE, PLAN_CATALOG, PLAN_TIERS_ASCENDING,
    ROUTE_53_PLAN_CODE,
};

use super::subscription_lifecycle::{
    create_body_naming, distribution, hosted_zone, key_value_store, web_acl,
};
use super::subscription_wire_form::{call, create, create_body, router, Answer};

/// The canonical account id every emulated ARN carries.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// One operation that takes a subscription ARN, with whatever else it requires.
struct ArnOperation {
    name: &'static str,
    /// Body members beyond `arn` the model marks `@required`.
    extra: fn() -> Value,
    /// The `If-Match` header, for the operations whose `ifMatch` is `@required`.
    if_match: Option<&'static str>,
    /// The `approvalMode` a subject must be created with for this operation to apply.
    approval: Option<&'static str>,
}

/// Every implemented operation whose input carries a `SubscriptionArn` and whose success
/// path a freshly created subscription satisfies, so one table can carry all four rules.
/// The two remaining ARN-taking operations need a subject of their own — a pending change
/// to cancel, a removable resource to drop — so rules 1 and 2 reach them through
/// [`the_remaining_arn_taking_operations_share_the_gate`] here, and
/// `DisassociateResourcesFromSubscription` carries the rest beside its own rows in the
/// `subscription_lifecycle` suite.
const ARN_OPERATIONS: &[ArnOperation] = &[
    ArnOperation {
        name: "GetSubscription",
        extra: || json!({}),
        if_match: None,
        approval: Some("IMMEDIATE"),
    },
    ArnOperation {
        name: "CancelSubscription",
        extra: || json!({}),
        if_match: Some("1"),
        approval: Some("IMMEDIATE"),
    },
    ArnOperation {
        name: "UpdateSubscription",
        extra: || json!({"planTier": "PRO"}),
        if_match: Some("1"),
        approval: Some("IMMEDIATE"),
    },
    ArnOperation {
        name: "ApprovePaidSubscription",
        extra: || json!({}),
        if_match: Some("1"),
        approval: Some("MANUAL"),
    },
    ArnOperation {
        name: "AssociateResourcesToSubscription",
        extra: || json!({"resourceArns": [HOSTED_ZONE]}),
        if_match: Some("1"),
        approval: Some("IMMEDIATE"),
    },
];

/// A resource of no plan family's required kind, so associating it with a subject of any
/// family is a change that family allows.
const HOSTED_ZONE: &str = "arn:aws:route53:::hostedzone/Z1EXAMPLE1EXAMPLE";

/// The ARN-taking operations outside [`ARN_OPERATIONS`]: both mutate something a fresh
/// subscription does not have, so only the two gates that are decided before the store is
/// consulted are asserted for them.
const SUBJECT_BOUND_ARN_OPERATIONS: &[ArnOperation] = &[
    ArnOperation {
        name: "CancelSubscriptionChange",
        extra: || json!({}),
        if_match: Some("1"),
        approval: Some("IMMEDIATE"),
    },
    ArnOperation {
        name: "DisassociateResourcesFromSubscription",
        extra: || json!({"resourceArns": [HOSTED_ZONE]}),
        if_match: Some("1"),
        approval: Some("IMMEDIATE"),
    },
];

/// Call `operation` with `arn` and whatever else the operation requires.
async fn call_with_arn(
    router: &MultiServiceRouter,
    operation: &ArnOperation,
    arn: &str,
) -> Answer {
    let mut body = (operation.extra)();
    body["arn"] = json!(arn);
    call(router, operation.name, body, operation.if_match).await
}

/// Rule 1: a value outside the subscription-ARN grammar is a 400 `ValidationException`
/// naming it, on every operation that takes one.
///
/// The rows differ along the element that is wrong: the `arn:` prefix, the field count,
/// the partition, the service namespace, the account id, the resource type, the resource
/// separator, and the id itself.
#[tokio::test]
async fn a_malformed_subscription_arn_is_a_validation_failure() {
    let malformed = vec![
        ("no ARN at all", "not-an-arn".to_string()),
        (
            "an ARN of another service",
            format!("arn:aws:cloudfront::{ACCOUNT}:subscription:sub_abc123"),
        ),
        (
            "too few ARN fields",
            format!("arn:aws:pricingplanmanager::{ACCOUNT}"),
        ),
        (
            "an empty partition",
            format!("arn::pricingplanmanager::{ACCOUNT}:subscription:sub_abc123"),
        ),
        (
            "an account id that is not numeric",
            "arn:aws:pricingplanmanager::not-an-account:subscription:sub_abc123".to_string(),
        ),
        (
            "an account id that is not twelve digits",
            "arn:aws:pricingplanmanager::0001:subscription:sub_abc123".to_string(),
        ),
        (
            "a region segment that is not an ARN label",
            format!("arn:aws:pricingplanmanager:us west 2:{ACCOUNT}:subscription:sub_abc123"),
        ),
        (
            "another resource type",
            format!("arn:aws:pricingplanmanager::{ACCOUNT}:plan:sub_abc123"),
        ),
        (
            "the '/' separator with an id the service never issues",
            format!("arn:aws:pricingplanmanager::{ACCOUNT}:subscription/probe-ppm-missing-151210ab"),
        ),
        (
            "an empty id after the '/' separator",
            format!("arn:aws:pricingplanmanager::{ACCOUNT}:subscription/"),
        ),
        (
            "an empty id after the ':' separator",
            format!("arn:aws:pricingplanmanager::{ACCOUNT}:subscription:"),
        ),
        (
            "an id without the published prefix",
            format!("arn:aws:pricingplanmanager::{ACCOUNT}:subscription:abc123"),
        ),
        (
            "an id carrying a further ARN field",
            format!("arn:aws:pricingplanmanager::{ACCOUNT}:subscription:sub_abc123:extra"),
        ),
        (
            "an upper-cased ARN prefix",
            format!("ARN:aws:pricingplanmanager::{ACCOUNT}:subscription:sub_abc123"),
        ),
    ];

    let router = router();
    for (label, arn) in &malformed {
        for operation in ARN_OPERATIONS {
            let case = format!("{}/{label}", operation.name);
            let refused = call_with_arn(&router, operation, arn).await;
            assert_eq!(
                refused.status, 400,
                "case {case}: a malformed ARN must be refused 400, got {} {:?}",
                refused.status, refused.body
            );
            assert_eq!(
                refused.error_code.as_deref(),
                Some("ValidationException"),
                "case {case}: unexpected error code for {:?}",
                refused.body
            );
            assert_eq!(
                refused.message(&case),
                format!("Invalid subscription ARN: {arn}"),
                "case {case}: unexpected message for {:?}",
                refused.body
            );
        }
    }
}

/// Rule 2: a well-formed ARN that names nothing is a 404 `ResourceNotFoundException`
/// stating AWS's sentence and naming the ARN in `resourceId`, on every operation.
///
/// The rows differ along the parts a subscription ARN may vary in: the partition, whether
/// the region segment is populated, whether the account id is, which of the two resource
/// forms is used, and the length and charset of the id.
#[tokio::test]
async fn a_well_formed_but_absent_subscription_arn_is_not_found() {
    let absent = vec![
        (
            "the published region-less form",
            format!("arn:aws:pricingplanmanager::{ACCOUNT}:subscription:sub_absent0000000001"),
        ),
        (
            "a populated region segment",
            format!("arn:aws:pricingplanmanager:us-west-2:{ACCOUNT}:subscription:sub_absent2"),
        ),
        (
            "another partition",
            format!("arn:aws-us-gov:pricingplanmanager::{ACCOUNT}:subscription:sub_absent3"),
        ),
        (
            "an empty account segment",
            "arn:aws:pricingplanmanager:::subscription:sub_absent4".to_string(),
        ),
        (
            "an id carrying hyphens and underscores",
            format!("arn:aws:pricingplanmanager::{ACCOUNT}:subscription:sub_absent-5_e"),
        ),
        (
            "a long id",
            format!(
                "arn:aws:pricingplanmanager::{ACCOUNT}:subscription:sub_{}",
                "a".repeat(64)
            ),
        ),
        (
            "the form this service mints today",
            format!(
                "arn:aws:pricingplanmanager:us-west-2:{ACCOUNT}:subscription/01234567-89ab-cdef-0123-456789abcdef"
            ),
        ),
    ];

    let router = router();
    for (label, arn) in &absent {
        for operation in ARN_OPERATIONS {
            let case = format!("{}/{label}", operation.name);
            let refused = call_with_arn(&router, operation, arn).await;
            assert_eq!(
                refused.status, 404,
                "case {case}: an absent subscription must be refused 404, got {} {:?}",
                refused.status, refused.body
            );
            assert_eq!(
                refused.error_code.as_deref(),
                Some("ResourceNotFoundException"),
                "case {case}: unexpected error code for {:?}",
                refused.body
            );
            assert_eq!(
                refused.message(&case),
                format!(
                    "Subscription not found: {arn}. Verify the ARN is correct and the \
                     subscription exists in your account."
                ),
                "case {case}: unexpected message for {:?}",
                refused.body
            );
            assert_eq!(
                refused.body.get("resourceId").and_then(Value::as_str),
                Some(arn.as_str()),
                "case {case}: resourceId must name the ARN, got {:?}",
                refused.body
            );
        }
    }
}

/// The ARNs this service mints are accepted by the same gate: a subscription created over
/// the wire is readable and mutable by the ARN the create handed back.
#[tokio::test]
async fn the_arns_the_service_mints_pass_the_syntax_gate() {
    let router = router();
    // The published tier ladder, read from the service's own catalogue rather than
    // repeated here.
    for tier in PLAN_TIERS_ASCENDING {
        let created = create(
            &router,
            create_body("CloudFront", tier, 1, None, Some("IMMEDIATE")),
        )
        .await;
        let arn = created.string("arn");
        let etag = created.require_etag(tier);

        let fetched = call(&router, "GetSubscription", json!({"arn": arn.clone()}), None).await;
        assert_eq!(
            fetched.status, 200,
            "case {tier}: a minted ARN must be readable, got {} {:?}",
            fetched.status, fetched.body
        );

        let updated = call(
            &router,
            "UpdateSubscription",
            json!({"arn": arn, "planTier": "PREMIUM"}),
            Some(&etag),
        )
        .await;
        assert_eq!(
            updated.status, 200,
            "case {tier}: a minted ARN must be mutable, got {} {:?}",
            updated.status, updated.body
        );
    }
}

/// Rule 3: an `If-Match` that is not the record's current tag is a 400
/// `ValidationException` echoing the rejected tag, and changes nothing.
///
/// The rows differ along the operation and along the rejected value: empty, a smaller
/// revision, a larger one, a non-numeric token and a long one.
#[tokio::test]
async fn a_stale_if_match_is_a_validation_failure_that_echoes_the_tag() {
    // Every one of these is a value this service never issued for a fresh record, whose
    // tag is a small decimal revision.
    let long_tag = "e".repeat(64);
    let stale_tags = ["", "0", "4096", "stale-etag", long_tag.as_str()];

    for operation in ARN_OPERATIONS.iter().filter(|op| op.if_match.is_some()) {
        for stale in &stale_tags {
            // An account of its own per probe: a resource is covered by at most one
            // subscription, and the association probe adds the same resource every time.
            let router = router();
            let case = format!("{}/{stale:?}", operation.name);
            let created = create(
                &router,
                create_body("CloudFront", "BUSINESS", 1, None, operation.approval),
            )
            .await;
            let arn = created.string("arn");
            let current = created.require_etag(&case);
            assert_ne!(
                current, *stale,
                "case {case}: the rejected tag must differ from the current one"
            );

            let mut body = (operation.extra)();
            body["arn"] = json!(arn.clone());
            let refused = call(&router, operation.name, body.clone(), Some(stale)).await;
            assert_eq!(
                refused.status, 400,
                "case {case}: a stale If-Match must be refused 400, got {} {:?}",
                refused.status, refused.body
            );
            assert_eq!(
                refused.error_code.as_deref(),
                Some("ValidationException"),
                "case {case}: unexpected error code for {:?}",
                refused.body
            );
            assert_eq!(
                refused.message(&case),
                format!("Invalid or outdated ETag '{stale}': retrieve current ETag and retry."),
                "case {case}: unexpected message for {:?}",
                refused.body
            );

            // Inert: the record keeps its status, carries no scheduled change and reports
            // the tag it had before the refusal.
            let unchanged = call(&router, "GetSubscription", json!({"arn": arn}), None).await;
            assert_eq!(
                unchanged.body, created.body,
                "case {case}: a refused mutation must not change the subscription"
            );
            assert_eq!(
                unchanged.require_etag(&case),
                current,
                "case {case}: a refused mutation must not mint a new tag"
            );

            let accepted = call(&router, operation.name, body, Some(&current)).await;
            assert_eq!(
                accepted.status, 200,
                "case {case}: the current tag must be accepted, got {} {:?}",
                accepted.status, accepted.body
            );
        }
    }
}

/// Rule 4: a `nextToken` this service did not issue is refused with the service's own
/// fixed sentence, which names nothing from the request.
///
/// The rows differ along how the token fails: not hexadecimal at all, hexadecimal of
/// something that is not an ARN, hexadecimal of another service's ARN, hexadecimal of this
/// service's namespace with another resource type, and a token past the decoder's bound.
#[tokio::test]
async fn an_unissued_next_token_is_refused_with_the_fixed_sentence() {
    let tokens = vec![
        ("not hexadecimal", "not-a-token".to_string()),
        ("an odd number of hex digits", "abc".to_string()),
        // The encoder is the service's own, so each row below is a token that decodes
        // cleanly and still carries something no page could have issued.
        ("a token carrying no ARN", encode_page_token("zzz")),
        (
            "a token carrying another service's ARN",
            encode_page_token("arn:aws:s3:::bucket/key"),
        ),
        (
            "a token carrying another resource type of this service",
            encode_page_token(&format!("arn:aws:pricingplanmanager::{ACCOUNT}:other/1")),
        ),
        (
            "a token carrying a malformed subscription ARN",
            encode_page_token(&format!(
                "arn:aws:pricingplanmanager::{ACCOUNT}:subscription/not-an-id"
            )),
        ),
        // Past the bound the decoder allows for the modelled 2048-character ARN.
        (
            "a token past the decoder's bound",
            encode_page_token(&"a".repeat(2049)),
        ),
    ];

    let router = router();
    for (label, token) in &tokens {
        let refused = call(&router, "ListSubscriptions", json!({"nextToken": token}), None).await;
        assert_eq!(
            refused.status, 400,
            "case {label}: an unissued nextToken must be refused 400, got {} {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_code.as_deref(),
            Some("ValidationException"),
            "case {label}: unexpected error code for {:?}",
            refused.body
        );
        assert_eq!(
            refused.message(label),
            "The nextToken provided is invalid.",
            "case {label}: unexpected message for {:?}",
            refused.body
        );
    }
}

/// The same gate still accepts a cursor a page handed out: the refusal above is about
/// tokens this service did not issue, not about pagination.
#[tokio::test]
async fn a_token_a_page_issued_is_still_accepted() {
    let router = router();
    // One subscription more than a page holds, so the first page carries a cursor.
    for index in 0..=LIST_PAGE_SIZE {
        let mut body = create_body("CloudFront", "PRO", 1, None, Some("IMMEDIATE"));
        body["clientToken"] = json!(format!("refusal-surface-{index}"));
        create(&router, body).await;
    }

    let first = call(&router, "ListSubscriptions", json!({}), None).await;
    let token = first.body["nextToken"]
        .as_str()
        .unwrap_or_else(|| panic!("a store of {} needs a cursor: {:?}", LIST_PAGE_SIZE + 1, first.body))
        .to_string();

    let second = call(&router, "ListSubscriptions", json!({"nextToken": token}), None).await;
    assert_eq!(
        second.status, 200,
        "case \"issued cursor\": a token a page issued must be accepted, got {} {:?}",
        second.status, second.body
    );
}

/// Rules 1 and 2 reach the two ARN-taking operations whose success path needs a subject of
/// its own: both are decided from the ARN alone, before the store is consulted, so they
/// hold for every operation that takes one and not only for the ones a fresh subscription
/// satisfies.
#[tokio::test]
async fn the_remaining_arn_taking_operations_share_the_gate() {
    // (case, the ARN, the status and code it must be refused with)
    let subjects = vec![
        (
            "a value that is no ARN",
            "not-an-arn".to_string(),
            400,
            "ValidationException",
        ),
        (
            "an id this service never issues",
            format!("arn:aws:pricingplanmanager::{ACCOUNT}:subscription/does-not-exist"),
            400,
            "ValidationException",
        ),
        (
            "another resource type of this service",
            format!("arn:aws:pricingplanmanager::{ACCOUNT}:plan:sub_abc123"),
            400,
            "ValidationException",
        ),
        (
            "a well-formed ARN naming nothing",
            format!("arn:aws:pricingplanmanager::{ACCOUNT}:subscription:sub_absent0000000001"),
            404,
            "ResourceNotFoundException",
        ),
        (
            "a well-formed ARN of the minted form naming nothing",
            format!(
                "arn:aws:pricingplanmanager:us-west-2:{ACCOUNT}:subscription/01234567-89ab-cdef-0123-456789abcdef"
            ),
            404,
            "ResourceNotFoundException",
        ),
    ];

    let router = router();
    for (label, arn, status, code) in &subjects {
        for operation in SUBJECT_BOUND_ARN_OPERATIONS {
            let case = format!("{}/{label}", operation.name);
            let refused = call_with_arn(&router, operation, arn).await;
            assert_eq!(
                refused.status, *status,
                "case {case}: expected HTTP {status}, got {} {:?}",
                refused.status, refused.body
            );
            assert_eq!(
                refused.error_code.as_deref(),
                Some(*code),
                "case {case}: unexpected error code for {:?}",
                refused.body
            );
            if *status == 400 {
                assert_eq!(
                    refused.message(&case),
                    format!("Invalid subscription ARN: {arn}"),
                    "case {case}: unexpected message for {:?}",
                    refused.body
                );
            }
        }
    }
}

/// Rule 5: a `CreateSubscription` naming a plan this service does not offer is refused with
/// AWS's sentence for the half that failed, and stores nothing.
///
/// The rows differ along which half is wrong, the family (both the catalogue's, in the
/// published spelling and in another case), the plan the tier refusal names, and the shape
/// of the rejected value.
#[tokio::test]
async fn a_plan_the_service_does_not_offer_is_refused() {
    // (case, planFamily, planTier, the message the refusal states)
    let rows = [
        (
            "a family no plan is offered for",
            "NotARealPlanFamily",
            "PRO",
            "Invalid plan name: NotARealPlanFamily".to_string(),
        ),
        (
            "another AWS service's name",
            "DynamoDB",
            "FREE",
            "Invalid plan name: DynamoDB".to_string(),
        ),
        (
            "an empty family",
            "",
            "PRO",
            "Invalid plan name: ".to_string(),
        ),
        (
            "a tier the CloudFront plan does not sell",
            "CloudFront",
            "NotARealPlanTier",
            format!("Invalid plan tier \"NotARealPlanTier\" for the plan \"{CLOUDFRONT_PLAN_CODE}\""),
        ),
        (
            "a tier of the wrong case",
            "CloudFront",
            "premium",
            format!("Invalid plan tier \"premium\" for the plan \"{CLOUDFRONT_PLAN_CODE}\""),
        ),
        (
            "the family in lower case, with a tier nothing sells",
            "cloudfront",
            "ENTERPRISE",
            format!("Invalid plan tier \"ENTERPRISE\" for the plan \"{CLOUDFRONT_PLAN_CODE}\""),
        ),
        (
            "a tier the Route 53 plan does not sell",
            "Route53",
            "CUSTOM",
            format!("Invalid plan tier \"CUSTOM\" for the plan \"{ROUTE_53_PLAN_CODE}\""),
        ),
        (
            "an empty tier on the Route 53 plan",
            "ROUTE53",
            "",
            format!("Invalid plan tier \"\" for the plan \"{ROUTE_53_PLAN_CODE}\""),
        ),
    ];

    let router = router();
    for (case, family, tier, expected) in &rows {
        let refused = call(
            &router,
            "CreateSubscription",
            create_body(family, tier, 1, None, Some("IMMEDIATE")),
            None,
        )
        .await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: a plan not on offer must be refused 400, got {} {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_code.as_deref(),
            Some("ValidationException"),
            "case {case:?}: unexpected error code for {:?}",
            refused.body
        );
        assert_eq!(
            &refused.message(case),
            expected,
            "case {case:?}: unexpected message"
        );

        let listed = call(&router, "ListSubscriptions", json!({}), None).await;
        assert_eq!(
            listed.body["subscriptionSummaries"]
                .as_array()
                .map(Vec::len)
                .unwrap_or_default(),
            0,
            "case {case:?}: a refused create must leave no subscription behind, got {:?}",
            listed.body
        );
    }

    // Every plan the catalogue DOES offer, at every tier it sells, is still served: the
    // rows above refuse what is not on offer and nothing else.
    for plan in PLAN_CATALOG {
        for tier in plan.tiers {
            let case = format!("{}/{tier}", plan.family);
            let created = call(
                &router,
                "CreateSubscription",
                create_body(plan.family, tier, 1, None, Some("IMMEDIATE")),
                None,
            )
            .await;
            assert_eq!(
                created.status, 200,
                "case {case}: an offered plan must be served, got {} {:?}",
                created.status, created.body
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Rule 6 — a clientToken is a handle on ONE request
// ---------------------------------------------------------------------------

/// The modelled business-rule refusal and the modelled conflict.
const VALIDATION_EXCEPTION: &str = "ValidationException";
const CONFLICT_EXCEPTION: &str = "ConflictException";

/// The whole sentence a `clientToken` replayed with different parameters is refused with:
/// fixed text, carrying no value from the request and none from the store.
const TOKEN_PARAMETERS_DIFFER: &str =
    "The clientToken matches a previous request but the request parameters differ. Reuse a \
     clientToken only for an identical request.";

/// What a replay must earn.
enum Replay {
    /// The same request: the subscription the token minted, unchanged.
    Original,
    /// A different request under a token already used: the modelled conflict.
    Conflict,
}

/// A `clientToken` names ONE request: a replay carrying the same parameters is answered
/// with the subscription that token minted, and a replay carrying different parameters is
/// the modelled `ConflictException` (409) naming that subscription — with nothing written
/// either way.
///
/// The rows differ along which identifying member the replay changes — the plan family, the
/// tier, the usage level, the approval mode, and the resource list by count, by membership,
/// by order and by repetition — because the rule is about every member that says which
/// subscription a request asks for, not about the one the recorded call changed.
#[tokio::test]
async fn a_client_token_replayed_with_other_parameters_is_the_modelled_conflict() {
    // (case, how the replay differs from the first request, what it must earn)
    type Row = (&'static str, fn(Value) -> Value, Replay);
    let rows: &[Row] = &[
        ("the identical request", |body| body, Replay::Original),
        (
            "the same resources in the other order",
            |mut body| {
                let mut resources = resource_arns(&body);
                resources.reverse();
                body["resourceArns"] = json!(resources);
                body
            },
            Replay::Original,
        ),
        (
            "the same resources, one of them named twice",
            |mut body| {
                let mut resources = resource_arns(&body);
                resources.push(resources[0].clone());
                body["resourceArns"] = json!(resources);
                body
            },
            Replay::Original,
        ),
        (
            "another plan tier",
            |mut body| {
                body["planTier"] = json!("BUSINESS");
                body
            },
            Replay::Conflict,
        ),
        (
            "another plan family",
            |mut body| {
                body["planFamily"] = json!("Route53");
                body
            },
            Replay::Conflict,
        ),
        (
            "a usage level the first request did not state",
            |mut body| {
                body["usageLevel"] = json!("DEFAULT");
                body
            },
            Replay::Conflict,
        ),
        (
            "another approval mode",
            |mut body| {
                body["approvalMode"] = json!("MANUAL");
                body
            },
            Replay::Conflict,
        ),
        (
            "one resource more",
            |mut body| {
                let mut resources = resource_arns(&body);
                resources.push(hosted_zone("Z9TOKEN"));
                body["resourceArns"] = json!(resources);
                body
            },
            Replay::Conflict,
        ),
        (
            "one resource fewer",
            |mut body| {
                let mut resources = resource_arns(&body);
                resources.pop();
                body["resourceArns"] = json!(resources);
                body
            },
            Replay::Conflict,
        ),
        (
            "another resource in place of one",
            |mut body| {
                let mut resources = resource_arns(&body);
                let last = resources.len() - 1;
                resources[last] = key_value_store("kvs-token");
                body["resourceArns"] = json!(resources);
                body
            },
            Replay::Conflict,
        ),
    ];

    for (case, differ, expected) in rows {
        // An account of its own per row: a resource is covered by at most one subscription.
        let router = router();
        let mut first = create_body("CloudFront", "PRO", 2, None, Some("IMMEDIATE"));
        first["clientToken"] = json!("retry-token");
        let created = create(&router, first.clone()).await;
        let arn = created.string("arn");
        let etag = created.require_etag(case);

        let replayed = call(&router, "CreateSubscription", differ(first.clone()), None).await;
        match expected {
            Replay::Original => {
                assert_eq!(
                    replayed.status, 200,
                    "case {case:?}: the same request must be served, got {} {:?}",
                    replayed.status, replayed.body
                );
                assert_eq!(
                    replayed.body, created.body,
                    "case {case:?}: a replay must answer the subscription the token minted, \
                     got {:?}",
                    replayed.body
                );
                assert_eq!(
                    replayed.require_etag(case),
                    etag,
                    "case {case:?}: a replay creates nothing, so the token does not move"
                );
            }
            Replay::Conflict => {
                assert_eq!(
                    replayed.status, 409,
                    "case {case:?}: expected the call to be refused 409, got {} {:?}",
                    replayed.status, replayed.body
                );
                assert_eq!(
                    replayed.error_code.as_deref(),
                    Some(CONFLICT_EXCEPTION),
                    "case {case:?}: unexpected error code for {:?}",
                    replayed.body
                );
                assert_eq!(
                    replayed.message(case),
                    TOKEN_PARAMETERS_DIFFER,
                    "case {case:?}: unexpected message for {:?}",
                    replayed.body
                );
                assert_eq!(
                    replayed.body.get("resourceId").and_then(Value::as_str),
                    Some(arn.as_str()),
                    "case {case:?}: the conflict must name the subscription the token \
                     stands for, got {:?}",
                    replayed.body
                );
            }
        }

        // Inert either way: the account still holds exactly the one subscription, with the
        // parameters it was created with and the token it was created under.
        let listed = call(&router, "ListSubscriptions", json!({}), None).await;
        assert_eq!(
            listed.body["subscriptionSummaries"]
                .as_array()
                .map(Vec::len)
                .unwrap_or_default(),
            1,
            "case {case:?}: no replay may mint a second subscription, got {:?}",
            listed.body
        );
        let read = call(&router, "GetSubscription", json!({"arn": arn.clone()}), None).await;
        assert_eq!(
            read.body, created.body,
            "case {case:?}: no replay may change the stored subscription, got {:?}",
            read.body
        );
        assert_eq!(
            read.require_etag(case),
            etag,
            "case {case:?}: no replay may move the subscription's entity tag"
        );
    }
}

/// The `resourceArns` a request body names.
fn resource_arns(body: &Value) -> Vec<String> {
    body["resourceArns"]
        .as_array()
        .unwrap_or_else(|| panic!("the body names resourceArns: {body:?}"))
        .iter()
        .map(|arn| arn.as_str().unwrap_or_default().to_string())
        .collect()
}

// ---------------------------------------------------------------------------
// Rule 7 — one subscription per resource
// ---------------------------------------------------------------------------

/// A create naming a resource another subscription already covers is refused with the
/// modelled `ValidationException` (400) naming that resource, and stores nothing.
///
/// The rows differ along what the rule generalises over: which resource of the holder's set
/// is named again, the vendor and resource type of that resource, where in the new
/// request's list it sits, how many of the named resources are taken, the plan families of
/// the holder and of the new request, and the holder's own status.
#[tokio::test]
async fn a_resource_another_subscription_covers_cannot_be_subscribed_again() {
    let dist = distribution("E1HELD");
    let acl = web_acl("acl-held");
    let zone = hosted_zone("Z1HELD");
    let kvs = key_value_store("kvs-held");
    let free_dist = distribution("E2FREE");
    let free_acl = web_acl("acl-free");

    // (case, the holder's family/tier/approval and set, the family and set the new request
    //  names, the resource the refusal must name — `None` for a request that is admitted)
    struct Row {
        case: &'static str,
        holder: Vec<String>,
        holder_approval: Option<&'static str>,
        family: &'static str,
        named: Vec<String>,
        offending: Option<String>,
    }

    let rows = vec![
        Row {
            case: "the distribution the holder covers, named first",
            holder: vec![dist.clone(), acl.clone(), zone.clone(), kvs.clone()],
            holder_approval: Some("IMMEDIATE"),
            family: "CloudFront",
            named: vec![dist.clone(), free_acl.clone()],
            offending: Some(dist.clone()),
        },
        Row {
            case: "the web ACL the holder covers, named last",
            holder: vec![dist.clone(), acl.clone(), zone.clone(), kvs.clone()],
            holder_approval: Some("IMMEDIATE"),
            family: "CloudFront",
            named: vec![free_dist.clone(), acl.clone()],
            offending: Some(acl.clone()),
        },
        Row {
            case: "a hosted zone the holder covers, of another plan family",
            holder: vec![dist.clone(), acl.clone(), zone.clone(), kvs.clone()],
            holder_approval: Some("IMMEDIATE"),
            family: "Route53",
            named: vec![hosted_zone("Z2FREE"), zone.clone()],
            offending: Some(zone.clone()),
        },
        Row {
            case: "a KeyValueStore the holder covers",
            holder: vec![dist.clone(), acl.clone(), zone.clone(), kvs.clone()],
            holder_approval: Some("IMMEDIATE"),
            family: "Route53",
            named: vec![kvs.clone()],
            offending: Some(kvs.clone()),
        },
        Row {
            case: "two taken resources: the first the request named is refused",
            holder: vec![dist.clone(), acl.clone(), zone.clone(), kvs.clone()],
            holder_approval: Some("IMMEDIATE"),
            family: "CloudFront",
            named: vec![acl.clone(), dist.clone()],
            offending: Some(acl.clone()),
        },
        Row {
            case: "a holder that is still waiting for approval",
            holder: vec![dist.clone(), acl.clone()],
            holder_approval: Some("MANUAL"),
            family: "CloudFront",
            named: vec![dist.clone(), free_acl.clone()],
            offending: Some(dist.clone()),
        },
        Row {
            case: "resources no subscription covers",
            holder: vec![dist.clone(), acl.clone(), zone.clone(), kvs.clone()],
            holder_approval: Some("IMMEDIATE"),
            family: "CloudFront",
            named: vec![free_dist.clone(), free_acl.clone()],
            offending: None,
        },
    ];

    for row in &rows {
        // An account of its own per row, so each row's holder is the only subscription in
        // it when the second create is made.
        let router = router();
        let case = row.case;
        let held = create(
            &router,
            create_body_naming("CloudFront", "PREMIUM", &row.holder, row.holder_approval),
        )
        .await;
        let holder_arn = held.string("arn");

        let answered = call(
            &router,
            "CreateSubscription",
            create_body_naming(row.family, "FREE", &row.named, Some("IMMEDIATE")),
            None,
        )
        .await;
        match row.offending.as_deref() {
            Some(offending) => {
                assert_eq!(
                    answered.status, 400,
                    "case {case:?}: expected the call to be refused 400, got {} {:?}",
                    answered.status, answered.body
                );
                assert_eq!(
                    answered.error_code.as_deref(),
                    Some(VALIDATION_EXCEPTION),
                    "case {case:?}: unexpected error code for {:?}",
                    answered.body
                );
                assert_eq!(
                    answered.message(case),
                    format!(
                        "Cannot create subscription with {offending}: resource must belong \
                         to your account and not be used in another subscription."
                    ),
                    "case {case:?}: unexpected message for {:?}",
                    answered.body
                );
                assert_eq!(
                    answered.body.get("resourceId").and_then(Value::as_str),
                    Some(offending),
                    "case {case:?}: the refusal must name the offending resource, got {:?}",
                    answered.body
                );
                // The refusal stores nothing: the holder is still the only subscription in
                // the account, and still covers exactly what it covered.
                let listed = call(&router, "ListSubscriptions", json!({}), None).await;
                let summaries = listed.body["subscriptionSummaries"]
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                assert_eq!(
                    summaries.len(),
                    1,
                    "case {case:?}: a refused create must mint no subscription, got {:?}",
                    listed.body
                );
                assert_eq!(
                    summaries[0]["resourceArns"],
                    json!(row.holder),
                    "case {case:?}: the holder must keep its resources, got {:?}",
                    summaries[0]
                );
            }
            None => {
                assert_eq!(
                    answered.status, 200,
                    "case {case:?}: a create over free resources must succeed, got {} {:?}",
                    answered.status, answered.body
                );
                assert_eq!(
                    answered.body["resourceArns"],
                    json!(row.named),
                    "case {case:?}: the created subscription must cover what it named, got {:?}",
                    answered.body
                );
            }
        }
        assert!(
            !holder_arn.is_empty(),
            "case {case:?}: the holder must have been created"
        );
    }
}

/// A resource another subscription covers cannot be ADDED to a second subscription either:
/// the same rule, stated by the operation that would add it, with both subscriptions left
/// exactly as they were.
///
/// The rows differ along the kind of the taken resource, where in the request's list it
/// sits, and whether every named resource is taken or only one of them.
#[tokio::test]
async fn a_resource_another_subscription_covers_cannot_be_added_to_a_second_one() {
    let taken_zone = hosted_zone("Z1TAKEN");
    let taken_store = key_value_store("kvs-taken");
    let free_zone = hosted_zone("Z2FREE");

    // (case, the resources the request adds, the one the refusal must name — `None` for a
    //  request that is admitted)
    let rows: [(&str, Vec<String>, Option<String>); 4] = [
        (
            "a hosted zone the other subscription covers",
            vec![taken_zone.clone()],
            Some(taken_zone.clone()),
        ),
        (
            "a KeyValueStore the other subscription covers",
            vec![taken_store.clone()],
            Some(taken_store.clone()),
        ),
        (
            "a taken resource named behind a free one",
            vec![free_zone.clone(), taken_zone.clone()],
            Some(taken_zone.clone()),
        ),
        ("a resource nothing covers", vec![free_zone.clone()], None),
    ];

    for (case, named, offending) in rows {
        // An account of its own per row: each row decides what its two subscriptions cover.
        let router = router();
        let holder_resources = vec![taken_zone.clone(), taken_store.clone()];
        let holder = create(
            &router,
            create_body_naming("Route53", "PRO", &holder_resources, Some("IMMEDIATE")),
        )
        .await;
        let holder_arn = holder.string("arn");

        let subject_resources = vec![distribution("E1ADDS"), web_acl("acl-adds")];
        let subject = create(
            &router,
            create_body_naming("CloudFront", "PRO", &subject_resources, Some("IMMEDIATE")),
        )
        .await;
        let subject_arn = subject.string("arn");
        let subject_etag = subject.require_etag(case);

        let answered = call(
            &router,
            "AssociateResourcesToSubscription",
            json!({"arn": subject_arn.clone(), "resourceArns": named}),
            Some(&subject_etag),
        )
        .await;
        match offending.as_deref() {
            Some(offending) => {
                assert_eq!(
                    answered.status, 400,
                    "case {case:?}: expected the call to be refused 400, got {} {:?}",
                    answered.status, answered.body
                );
                assert_eq!(
                    answered.error_code.as_deref(),
                    Some(VALIDATION_EXCEPTION),
                    "case {case:?}: unexpected error code for {:?}",
                    answered.body
                );
                assert_eq!(
                    answered.message(case),
                    format!(
                        "Cannot add {offending} to subscription {subject_arn}: resource \
                         must belong to your account and not be used in another \
                         subscription."
                    ),
                    "case {case:?}: unexpected message for {:?}",
                    answered.body
                );
                assert_eq!(
                    answered.body.get("resourceId").and_then(Value::as_str),
                    Some(offending),
                    "case {case:?}: the refusal must name the offending resource, got {:?}",
                    answered.body
                );
                // Both subscriptions are left exactly as they were.
                let subject_read = call(
                    &router,
                    "GetSubscription",
                    json!({"arn": subject_arn.clone()}),
                    None,
                )
                .await;
                assert_eq!(
                    subject_read.body, subject.body,
                    "case {case:?}: a refused association must change nothing, got {:?}",
                    subject_read.body
                );
                assert_eq!(
                    subject_read.require_etag(case),
                    subject_etag,
                    "case {case:?}: a refused association must not mint a new ETag"
                );
                let holder_read =
                    call(&router, "GetSubscription", json!({"arn": holder_arn}), None).await;
                assert_eq!(
                    holder_read.body["resourceArns"],
                    json!(holder_resources),
                    "case {case:?}: the subscription covering the resource must keep it, \
                     got {:?}",
                    holder_read.body
                );
            }
            None => {
                assert_eq!(
                    answered.status, 200,
                    "case {case:?}: adding a resource nothing covers must succeed, got {} \
                     {:?}",
                    answered.status, answered.body
                );
                let expected: Vec<String> = subject_resources
                    .iter()
                    .chain(named.iter())
                    .cloned()
                    .collect();
                assert_eq!(
                    answered.body["resourceArns"],
                    json!(expected),
                    "case {case:?}: expected the subscription to cover {expected:?}, got {:?}",
                    answered.body["resourceArns"]
                );
            }
        }
    }
}

/// The gate is STATE-DEPENDENT: the very create that was refused is admitted once nothing
/// covers the resources any more — the holder's cancellation takes effect on the clock, and
/// the resources it covered are free again.
#[tokio::test]
async fn the_same_create_is_admitted_once_the_covering_subscription_is_gone() {
    let router = router();
    let case = "cancel the holder, then subscribe over its resources";
    let resources = vec![distribution("E1FREED"), web_acl("acl-freed")];

    let held = create(
        &router,
        create_body_naming("CloudFront", "PRO", &resources, Some("IMMEDIATE")),
    )
    .await;
    let holder_arn = held.string("arn");

    let request = create_body_naming("CloudFront", "FREE", &resources, Some("IMMEDIATE"));
    let refused = call(&router, "CreateSubscription", request.clone(), None).await;
    assert_eq!(
        (refused.status, refused.error_code.as_deref()),
        (400, Some(VALIDATION_EXCEPTION)),
        "case {case:?}: while the holder covers them the create must be refused, got {} {:?}",
        refused.status,
        refused.body
    );

    // Cancelling is scheduled, so the holder is still the one covering the resources until
    // the change the clock owes has taken effect.
    let cancelled = call(
        &router,
        "CancelSubscription",
        json!({"arn": holder_arn.clone()}),
        Some(&held.require_etag(case)),
    )
    .await;
    assert_eq!(
        cancelled.status, 200,
        "case {case:?}: the cancellation must be scheduled, got {} {:?}",
        cancelled.status, cancelled.body
    );

    // Wait for the CONDITION — the cancellation having taken effect, which the reader
    // reports by no longer finding the subscription — rather than for a time.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    loop {
        let read = call(
            &router,
            "GetSubscription",
            json!({"arn": holder_arn.clone()}),
            None,
        )
        .await;
        if read.status == 404 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the scheduled cancellation must take effect on the clock; the \
             subscription still reads {:?}",
            read.body
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    let admitted = call(&router, "CreateSubscription", request, None).await;
    assert_eq!(
        admitted.status, 200,
        "case {case:?}: once nothing covers the resources the same create must succeed, \
         got {} {:?}",
        admitted.status, admitted.body
    );
    assert_eq!(
        admitted.body["resourceArns"],
        json!(resources),
        "case {case:?}: the new subscription must cover the resources it named, got {:?}",
        admitted.body
    );
}
