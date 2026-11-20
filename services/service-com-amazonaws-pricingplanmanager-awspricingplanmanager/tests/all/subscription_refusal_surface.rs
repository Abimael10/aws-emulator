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
//!
//! The HTTP harness is `subscription_wire_form`'s, so this file adds no second copy of it.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_pricingplanmanager_awspricingplanmanager::state::{
    encode_page_token, CLOUDFRONT_PLAN_CODE, LIST_PAGE_SIZE, PLAN_CATALOG, PLAN_TIERS_ASCENDING,
    ROUTE_53_PLAN_CODE,
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

    let router = router();
    for operation in ARN_OPERATIONS.iter().filter(|op| op.if_match.is_some()) {
        for stale in &stale_tags {
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
