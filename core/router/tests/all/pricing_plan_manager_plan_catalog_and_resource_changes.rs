// SPDX-License-Identifier: CC0-1.0

//! Pricing Plan Manager's PLAN CATALOGUE gate, its subscription-ARN gate, and the
//! resource and scheduled-change mutations, driven over the assembled
//! `MultiServiceRouter`.
//!
//! Three rules are pinned here, each over a TABLE whose rows differ along what the rule
//! generalises over — the plan family, the tier, the operation, the structural element of
//! an ARN that is wrong, the kind and the count of the resources a request names:
//!
//! 1. a subscription is only ever to a plan this service OFFERS: `CreateSubscription`
//!    resolves `planFamily` against the catalogue and then `planTier` within that family's
//!    plan, before anything is read or written, and refuses a family no plan is offered for
//!    with HTTP 400 `ValidationException` / `Invalid plan name: <planFamily>` and a tier the
//!    resolved plan does not sell with `Invalid plan tier "<planTier>" for the plan
//!    "<plan code>"`, leaving no subscription behind. `UpdateSubscription` weighs the new
//!    tier against the plan the subscription is already on, the same way;
//! 2. a value that is not a well-formed subscription ARN is refused as a malformed input —
//!    HTTP 400 `ValidationException` / `Invalid subscription ARN: <arn>` — identically on
//!    EVERY operation that takes one, and before that operation's other inputs (its
//!    `If-Match`, its resources) are weighed;
//! 3. a clause refusing a collection member for its modelled `@length` quotes the
//!    collection THE REQUEST CARRIED, in AWS's constraint sentence, rather than a summary of
//!    its size that no caller sent.
//!
//! The resource and change mutations are asserted the same way: every accepted one is read
//! back through `GetSubscription`, so what is proven is the stored cover rather than the
//! answer one call happened to render.
//!
//! These rows live in this crate rather than beside the Pricing Plan Manager crate's own
//! `tests/all/` suites because that crate still roots its integration tests at a
//! hand-declared `tests/all.rs`, which a fix may not edit; the router's `tests/all/` picks a
//! new suite up with nothing to declare, and its one HTTP harness
//! (`crate::cloudfront_oac_s3_origin_edge`) is the one this suite drives. Nothing here
//! sleeps or reads the wall clock: the one instant a case depends on is written into the
//! store it is read from.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_pricingplanmanager_awspricingplanmanager::state::{
    now, ComAmazonawsPricingplanmanagerAwspricingplanmanagerState as PricingPlanState,
    MAX_SUBSCRIPTION_RESOURCES, PLAN_TIERS_ASCENDING,
};
use service_com_amazonaws_pricingplanmanager_awspricingplanmanager::EmulatedService;

// The router tests' ONE HTTP harness: the request builder that signs a credential scope for
// a signing name (which is what routes it) and the raw answer it hands back. This suite adds
// no second copy of it — only the reading of a restJson1 answer this service's operations
// return, which the harness's raw bytes do not interpret.
use crate::cloudfront_oac_s3_origin_edge::{call as dispatch, Answer as RawAnswer, REGION};

/// The account every emulated ARN carries.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// This service's SigV4 signing name, which is what routes a request to it, and the host it
/// is addressed at.
const SIGNING_NAME: &str = "pricingplanmanager";

/// The error code every refusal in rules 1 to 3 carries.
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// One HTTP exchange, read as the restJson1 answer this service renders: the status, the
/// parsed body, the `ETag` response header and the error class `x-amzn-errortype` names.
struct Answer {
    status: u16,
    body: Value,
    etag: Option<String>,
    error_code: Option<String>,
}

impl Answer {
    /// The raw answer the shared harness returned, read as this service's JSON answer.
    fn of(raw: &RawAnswer) -> Self {
        let header = |name: &str| {
            raw.headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string)
        };
        Self {
            status: raw.status,
            body: if raw.body.is_empty() {
                Value::Null
            } else {
                serde_json::from_slice(&raw.body).unwrap_or(Value::Null)
            },
            etag: header("etag"),
            // The header may carry the shape's namespace or a URI beside the name.
            error_code: header("x-amzn-errortype").map(|value| {
                value
                    .split('/')
                    .next()
                    .unwrap_or(&value)
                    .split('#')
                    .next_back()
                    .unwrap_or(&value)
                    .to_string()
            }),
        }
    }

    /// The body member `name` as a string, naming the whole body when it is absent.
    fn string(&self, name: &str) -> String {
        self.body
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("member {name} missing from body {:?}", self.body))
            .to_string()
    }

    /// The `message` member of an error body, naming the whole body when it is absent.
    fn message(&self, case: &str) -> String {
        self.body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case}: no message member in {:?}", self.body))
            .to_string()
    }

    /// The `ETag` response header; panics when the operation sent none.
    fn etag(&self, case: &str) -> String {
        self.etag
            .clone()
            .unwrap_or_else(|| panic!("case {case}: no ETag header on {:?}", self.body))
    }

    /// The resources the answered subscription covers.
    fn resources(&self, case: &str) -> Vec<String> {
        self.body["resourceArns"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case}: no resourceArns in {:?}", self.body))
            .iter()
            .map(|arn| {
                arn.as_str()
                    .unwrap_or_else(|| panic!("case {case}: a resource ARN is a string"))
                    .to_string()
            })
            .collect()
    }
}

/// The emulator with this service mounted on one engine, and the engine a case that writes
/// an instant into the store reads it back through.
fn router_on(engine: Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

/// The emulator with this service mounted on a fresh engine.
fn router() -> MultiServiceRouter {
    router_on(Arc::new(engine::InMemoryEngine::new()))
}

/// POST `/v1/<operation>` with `body`, optionally under `If-Match`, through the shared
/// harness.
async fn call(
    router: &MultiServiceRouter,
    operation: &str,
    body: Value,
    if_match: Option<&str>,
) -> Answer {
    let mut headers: Vec<(&str, &str)> = vec![("content-type", "application/json")];
    if let Some(tag) = if_match {
        headers.push(("if-match", tag));
    }
    let raw = dispatch(
        router,
        SIGNING_NAME,
        &format!("{SIGNING_NAME}.{REGION}.amazonaws.com"),
        "POST",
        &format!("/v1/{operation}"),
        &headers,
        Bytes::from(serde_json::to_vec(&body).expect("the request body serializes")),
    )
    .await;
    Answer::of(&raw)
}

/// A `CreateSubscription` body over `resources`, activated immediately.
fn create_body(family: &str, tier: &str, resources: &[String]) -> Value {
    json!({
        "planFamily": family,
        "planTier": tier,
        "resourceArns": resources,
        "approvalMode": "IMMEDIATE",
    })
}

/// Create one subscription, requiring HTTP 200.
async fn create(
    router: &MultiServiceRouter,
    case: &str,
    family: &str,
    tier: &str,
    resources: &[String],
) -> Answer {
    let created = call(
        router,
        "CreateSubscription",
        create_body(family, tier, resources),
        None,
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case {case}: CreateSubscription must succeed, got {} {:?}",
        created.status, created.body
    );
    created
}

/// The ARNs the owning services mint for the resources a subscription may cover.
fn distribution(id: &str) -> String {
    format!("arn:aws:cloudfront::{ACCOUNT}:distribution/{id}")
}
fn web_acl(name: &str) -> String {
    format!("arn:aws:wafv2:{REGION}:{ACCOUNT}:global/webacl/{name}/11111111-2222-3333-4444-555555555555")
}
fn hosted_zone(id: &str) -> String {
    format!("arn:aws:route53::{ACCOUNT}:hostedzone/{id}")
}

/// How many subscriptions the store holds, read through `ListSubscriptions`.
async fn stored_subscriptions(router: &MultiServiceRouter) -> usize {
    let listed = call(router, "ListSubscriptions", json!({}), None).await;
    assert_eq!(
        listed.status, 200,
        "ListSubscriptions must succeed, got {} {:?}",
        listed.status, listed.body
    );
    listed.body["subscriptionSummaries"]
        .as_array()
        .map(Vec::len)
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Rule 1: only a plan the service offers can be subscribed to
// ---------------------------------------------------------------------------

/// A `planFamily` no plan is offered for, and a `planTier` the resolved plan does not sell,
/// are refused with AWS's two sentences — and nothing is stored either way.
///
/// The rows differ along the family (both of the catalogue's, each in the spelling the
/// catalogue publishes and in another case), the plan the tier refusal names, and the shape
/// of the rejected value: a typo, another service's name, a tier of the wrong case, an empty
/// value.
#[tokio::test]
async fn only_an_offered_plan_can_be_subscribed_to() {
    // (case, planFamily, planTier, the refusal's message)
    let rows: [(&str, &str, &str, String); 10] = [
        (
            "a family no plan is offered for",
            "NotARealPlanFamily",
            "PRO",
            "Invalid plan name: NotARealPlanFamily".to_string(),
        ),
        (
            "another AWS service's name",
            "S3",
            "FREE",
            "Invalid plan name: S3".to_string(),
        ),
        (
            "a family with trailing space",
            "CloudFront ",
            "PRO",
            "Invalid plan name: CloudFront ".to_string(),
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
            "Invalid plan tier \"NotARealPlanTier\" for the plan \"CF_FRP\"".to_string(),
        ),
        (
            "a tier of the wrong case on the CloudFront plan",
            "CloudFront",
            "pro",
            "Invalid plan tier \"pro\" for the plan \"CF_FRP\"".to_string(),
        ),
        (
            "an empty tier on the CloudFront plan",
            "CloudFront",
            "",
            "Invalid plan tier \"\" for the plan \"CF_FRP\"".to_string(),
        ),
        (
            "the CloudFront family named in lower case, with an unsold tier",
            "cloudfront",
            "ENTERPRISE",
            "Invalid plan tier \"ENTERPRISE\" for the plan \"CF_FRP\"".to_string(),
        ),
        (
            "a tier the Route 53 plan does not sell",
            "Route53",
            "ENTERPRISE",
            "Invalid plan tier \"ENTERPRISE\" for the plan \"R53_FRP\"".to_string(),
        ),
        (
            "the Route 53 family named in upper case, with an unsold tier",
            "ROUTE53",
            "CUSTOM",
            "Invalid plan tier \"CUSTOM\" for the plan \"R53_FRP\"".to_string(),
        ),
    ];

    let router = router();
    for (case, family, tier, expected) in &rows {
        let refused = call(
            &router,
            "CreateSubscription",
            create_body(family, tier, &[hosted_zone("Z1")]),
            None,
        )
        .await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: an unoffered plan must be refused 400, got {} {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_code.as_deref(),
            Some(VALIDATION_EXCEPTION),
            "case {case:?}: unexpected error code for {:?}",
            refused.body
        );
        assert_eq!(
            &refused.message(case),
            expected,
            "case {case:?}: unexpected message"
        );
        assert_eq!(
            stored_subscriptions(&router).await,
            0,
            "case {case:?}: a refused create must leave no subscription behind"
        );
    }
}

/// Every plan the catalogue offers, at every tier it sells, is accepted and reads back with
/// the family and tier the request named — so the gate above refuses only what is not on
/// offer.
///
/// The rows are the catalogue's own families crossed with the published tier ladder, read
/// from the service's constants rather than repeated here, which covers both published
/// bounds of that ladder.
#[tokio::test]
async fn every_offered_plan_and_tier_is_accepted() {
    for family in ["CloudFront", "Route53"] {
        for tier in PLAN_TIERS_ASCENDING {
            // An account of its own per row: a resource is covered by at most one
            // subscription, so rows naming the same resources are independent accounts.
            let router = router();
            let case = format!("{family}/{tier}");
            let created = create(
                &router,
                &case,
                family,
                tier,
                &[distribution("E1"), web_acl("acl")],
            )
            .await;
            let arn = created.string("arn");
            let fetched = call(&router, "GetSubscription", json!({"arn": arn}), None).await;
            assert_eq!(
                fetched.status, 200,
                "case {case}: the subscription must read back, got {} {:?}",
                fetched.status, fetched.body
            );
            assert_eq!(
                (fetched.string("planFamily"), fetched.string("planTier")),
                (family.to_string(), (*tier).to_string()),
                "case {case}: the stored plan must be the one the request named, got {:?}",
                fetched.body
            );
        }
    }
}

/// `UpdateSubscription` weighs the new tier against the plan the subscription is already
/// on: a tier that plan does not sell is the same refusal, and the stored tier does not
/// move.
///
/// The rows differ along the plan the subscription is on, the tier it starts at and the
/// rejected value.
#[tokio::test]
async fn an_update_to_an_unsold_tier_is_refused() {
    // (case, family, starting tier, requested tier, the plan the refusal names)
    let rows = [
        ("CloudFront, a typo", "CloudFront", "PRO", "PREMIUMM", "CF_FRP"),
        ("CloudFront, wrong case", "CloudFront", "BUSINESS", "free", "CF_FRP"),
        ("Route 53, a tier nothing sells", "Route53", "FREE", "ENTERPRISE", "R53_FRP"),
    ];

    for (case, family, from, to, plan) in rows {
        // An account of its own per row: a resource is covered by at most one subscription,
        // so rows naming the same resources are independent accounts.
        let router = router();
        let created = create(&router, case, family, from, &[hosted_zone("Z1")]).await;
        let arn = created.string("arn");
        let etag = created.etag(case);

        let refused = call(
            &router,
            "UpdateSubscription",
            json!({"arn": arn.clone(), "planTier": to}),
            Some(&etag),
        )
        .await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: an unsold tier must be refused 400, got {} {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_code.as_deref(),
            Some(VALIDATION_EXCEPTION),
            "case {case:?}: unexpected error code for {:?}",
            refused.body
        );
        assert_eq!(
            refused.message(case),
            format!("Invalid plan tier \"{to}\" for the plan \"{plan}\""),
            "case {case:?}: unexpected message"
        );

        let fetched = call(&router, "GetSubscription", json!({"arn": arn}), None).await;
        assert_eq!(
            fetched.string("planTier"),
            from,
            "case {case:?}: a refused update must not move the stored tier, got {:?}",
            fetched.body
        );
        assert_eq!(
            fetched.etag(case),
            etag,
            "case {case:?}: a refused update must not mint a new ETag"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule 2: the subscription-ARN gate, on every operation that takes one
// ---------------------------------------------------------------------------

/// One operation that takes a subscription ARN, with whatever else the model requires of
/// it.
struct ArnOperation {
    name: &'static str,
    /// Body members beyond `arn` the model marks `@required`.
    extra: fn() -> Value,
    /// The `If-Match` header, for the operations whose `ifMatch` is `@required`. Every one
    /// here is a tag this service never issued, so a gate that ran after the `If-Match` was
    /// weighed would answer about the tag instead of about the ARN.
    if_match: Option<&'static str>,
}

/// Every operation of this service whose input carries a `SubscriptionArn`.
fn arn_operations() -> Vec<ArnOperation> {
    vec![
        ArnOperation {
            name: "GetSubscription",
            extra: || json!({}),
            if_match: None,
        },
        ArnOperation {
            name: "CancelSubscription",
            extra: || json!({}),
            if_match: Some("1"),
        },
        ArnOperation {
            name: "CancelSubscriptionChange",
            extra: || json!({}),
            if_match: Some("1"),
        },
        ArnOperation {
            name: "ApprovePaidSubscription",
            extra: || json!({}),
            if_match: Some("1"),
        },
        ArnOperation {
            name: "UpdateSubscription",
            extra: || json!({"planTier": "PRO"}),
            if_match: Some("1"),
        },
        ArnOperation {
            name: "AssociateResourcesToSubscription",
            extra: || json!({"resourceArns": [hosted_zone("Z1")]}),
            if_match: Some("1"),
        },
        ArnOperation {
            name: "DisassociateResourcesFromSubscription",
            extra: || json!({"resourceArns": [hosted_zone("Z1")]}),
            if_match: Some("1"),
        },
    ]
}

/// Rule 2: a value outside the subscription-ARN grammar is a 400 `ValidationException`
/// naming it, on every operation that takes one and ahead of that operation's other inputs.
///
/// The rows differ along the element that is wrong: the service namespace, the field count,
/// whether the Region segment is a label, the account id, the resource type, and the
/// subscription id itself.
#[tokio::test]
async fn a_malformed_subscription_arn_is_refused_by_every_operation_that_takes_one() {
    let malformed = [
        ("no ARN at all", "not-an-arn".to_string()),
        (
            "an ARN of another service",
            format!("arn:aws:cloudfront::{ACCOUNT}:subscription/01234567-89ab-cdef-0123-456789abcdef"),
        ),
        (
            "too few ARN fields",
            format!("arn:aws:pricingplanmanager::{ACCOUNT}"),
        ),
        (
            "a Region segment that is not an ARN label",
            format!("arn:aws:pricingplanmanager:us east 1:{ACCOUNT}:subscription:sub_abc123"),
        ),
        (
            "an account id that is not twelve digits",
            "arn:aws:pricingplanmanager::0001:subscription:sub_abc123".to_string(),
        ),
        (
            "another resource type of this service",
            format!("arn:aws:pricingplanmanager::{ACCOUNT}:plan:sub_abc123"),
        ),
        (
            "an id this service never issues",
            format!("arn:aws:pricingplanmanager::{ACCOUNT}:subscription/does-not-exist"),
        ),
        (
            "an empty id",
            format!("arn:aws:pricingplanmanager::{ACCOUNT}:subscription/"),
        ),
    ];

    let router = router();
    for (label, arn) in &malformed {
        for operation in arn_operations() {
            let case = format!("{}/{label}", operation.name);
            let mut body = (operation.extra)();
            body["arn"] = json!(arn);
            let refused = call(&router, operation.name, body, operation.if_match).await;
            assert_eq!(
                refused.status, 400,
                "case {case}: a malformed ARN must be refused 400, got {} {:?}",
                refused.status, refused.body
            );
            assert_eq!(
                refused.error_code.as_deref(),
                Some(VALIDATION_EXCEPTION),
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

/// A well-formed ARN naming no stored subscription is the other answer: HTTP 404
/// `ResourceNotFoundException`, on the same set of operations — so the 400 above is about
/// the ARN's syntax and not about the subscription being absent.
#[tokio::test]
async fn a_well_formed_absent_subscription_arn_is_not_found_by_every_operation() {
    let absent = [
        (
            "the published region-less form",
            format!("arn:aws:pricingplanmanager::{ACCOUNT}:subscription:sub_absent1"),
        ),
        (
            "a populated Region segment and the minted id form",
            format!(
                "arn:aws:pricingplanmanager:{REGION}:{ACCOUNT}:subscription/01234567-89ab-cdef-0123-456789abcdef"
            ),
        ),
    ];

    let router = router();
    for (label, arn) in &absent {
        for operation in arn_operations() {
            let case = format!("{}/{label}", operation.name);
            let mut body = (operation.extra)();
            body["arn"] = json!(arn);
            let refused = call(&router, operation.name, body, operation.if_match).await;
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
        }
    }
}

// ---------------------------------------------------------------------------
// The resource mutations, read back from the store
// ---------------------------------------------------------------------------

/// What one resource mutation is expected to leave behind.
enum Outcome {
    /// The set the subscription includes afterwards, in order.
    Covers(Vec<String>),
    /// The refusal's status, error code, and message as a function of the subscription's
    /// own ARN — which the refusals that name the subscription they are about interpolate,
    /// and which is minted per run.
    Refused(u16, &'static str, fn(&str) -> String),
}

/// Associating and disassociating resources land on the stored cover: the union in the
/// order the request named it, the removal of exactly what was named, and the modelled and
/// documented bounds on what the cover may be.
///
/// The rows differ along the plan family, the operation, the kind of resource named
/// (distribution, web ACL, hosted zone), whether it is already covered, and which bound the
/// request would break.
#[tokio::test]
async fn resources_are_associated_and_disassociated_in_the_store() {
    struct Case {
        label: &'static str,
        family: &'static str,
        initial: Vec<String>,
        operation: &'static str,
        resources: Vec<String>,
        outcome: Outcome,
    }

    let full: Vec<String> = (0..MAX_SUBSCRIPTION_RESOURCES)
        .map(|index| hosted_zone(&format!("Z{index}")))
        .collect();

    let cases = vec![
        Case {
            label: "one new resource is added at the end",
            family: "Route53",
            initial: vec![hosted_zone("Z1")],
            operation: "AssociateResourcesToSubscription",
            resources: vec![hosted_zone("Z2")],
            outcome: Outcome::Covers(vec![hosted_zone("Z1"), hosted_zone("Z2")]),
        },
        Case {
            label: "several new resources keep the order the request named",
            family: "Route53",
            initial: vec![hosted_zone("Z1")],
            operation: "AssociateResourcesToSubscription",
            resources: vec![hosted_zone("Z3"), hosted_zone("Z2")],
            outcome: Outcome::Covers(vec![
                hosted_zone("Z1"),
                hosted_zone("Z3"),
                hosted_zone("Z2"),
            ]),
        },
        Case {
            label: "a resource the subscription already covers cannot be added again",
            family: "Route53",
            initial: vec![hosted_zone("Z1"), hosted_zone("Z2")],
            operation: "AssociateResourcesToSubscription",
            resources: vec![hosted_zone("Z2")],
            outcome: Outcome::Refused(400, VALIDATION_EXCEPTION, |arn| {
                format!(
                    "Cannot add {} to subscription {arn}: the resource is already \
                     associated with this subscription.",
                    hosted_zone("Z2")
                )
            }),
        },
        Case {
            label: "the CloudFront family takes a resource of no required kind",
            family: "CloudFront",
            initial: vec![distribution("E1"), web_acl("acl")],
            operation: "AssociateResourcesToSubscription",
            resources: vec![hosted_zone("Z1")],
            outcome: Outcome::Covers(vec![
                distribution("E1"),
                web_acl("acl"),
                hosted_zone("Z1"),
            ]),
        },
        // A resource of a kind the CloudFront family requires is admitted here because
        // nothing can be asked about it: this router mounts Pricing Plan Manager alone, so
        // no CloudFront answers about a distribution and nothing is established against it
        // (the rule the crate's own `resource_eligibility_grounding` suite pins).
        Case {
            label: "a second distribution, with no CloudFront to ask about it",
            family: "CloudFront",
            initial: vec![distribution("E1"), web_acl("acl")],
            operation: "AssociateResourcesToSubscription",
            resources: vec![distribution("E2")],
            outcome: Outcome::Covers(vec![
                distribution("E1"),
                web_acl("acl"),
                distribution("E2"),
            ]),
        },
        Case {
            label: "a second web ACL, which no plan family's rule refuses on its own",
            family: "CloudFront",
            initial: vec![distribution("E1"), web_acl("acl")],
            operation: "AssociateResourcesToSubscription",
            resources: vec![web_acl("other")],
            outcome: Outcome::Covers(vec![
                distribution("E1"),
                web_acl("acl"),
                web_acl("other"),
            ]),
        },
        Case {
            label: "at the modelled maximum, a resource already covered is still refused",
            family: "Route53",
            initial: full.clone(),
            operation: "AssociateResourcesToSubscription",
            resources: vec![full[0].clone()],
            outcome: Outcome::Refused(400, VALIDATION_EXCEPTION, |arn| {
                format!(
                    "Cannot add {} to subscription {arn}: the resource is already \
                     associated with this subscription.",
                    hosted_zone("Z0")
                )
            }),
        },
        Case {
            label: "one resource past the modelled maximum is refused",
            family: "Route53",
            initial: full.clone(),
            operation: "AssociateResourcesToSubscription",
            resources: vec![hosted_zone("ZNEW")],
            outcome: Outcome::Refused(400, VALIDATION_EXCEPTION, |arn| {
                format!(
                    "Cannot change the resources of subscription {arn}: it covers \
                     {MAX_SUBSCRIPTION_RESOURCES} and the request adds 1; a subscription \
                     must include at most {MAX_SUBSCRIPTION_RESOURCES} resources."
                )
            }),
        },
        Case {
            label: "a covered resource is removed",
            family: "Route53",
            initial: vec![hosted_zone("Z1"), hosted_zone("Z2"), hosted_zone("Z3")],
            operation: "DisassociateResourcesFromSubscription",
            resources: vec![hosted_zone("Z2")],
            outcome: Outcome::Covers(vec![hosted_zone("Z1"), hosted_zone("Z3")]),
        },
        Case {
            label: "several covered resources are removed at once",
            family: "Route53",
            initial: vec![hosted_zone("Z1"), hosted_zone("Z2"), hosted_zone("Z3")],
            operation: "DisassociateResourcesFromSubscription",
            resources: vec![hosted_zone("Z1"), hosted_zone("Z3")],
            outcome: Outcome::Covers(vec![hosted_zone("Z2")]),
        },
        Case {
            label: "a resource the subscription does not cover cannot be removed",
            family: "Route53",
            initial: vec![hosted_zone("Z1")],
            operation: "DisassociateResourcesFromSubscription",
            resources: vec![hosted_zone("Z9")],
            outcome: Outcome::Refused(400, VALIDATION_EXCEPTION, |arn| {
                format!(
                    "Cannot remove {} from subscription {arn}: the resource is not \
                     associated with this subscription.",
                    hosted_zone("Z9")
                )
            }),
        },
        Case {
            label: "the last resource cannot be removed",
            family: "Route53",
            initial: vec![hosted_zone("Z1")],
            operation: "DisassociateResourcesFromSubscription",
            resources: vec![hosted_zone("Z1")],
            outcome: Outcome::Refused(400, VALIDATION_EXCEPTION, |arn| {
                format!(
                    "Cannot remove every resource from subscription {arn}: a subscription \
                     must always include at least one resource."
                )
            }),
        },
        Case {
            label: "the CloudFront family's distribution cannot be removed",
            family: "CloudFront",
            initial: vec![distribution("E1"), web_acl("acl"), hosted_zone("Z1")],
            operation: "DisassociateResourcesFromSubscription",
            resources: vec![distribution("E1")],
            outcome: Outcome::Refused(400, VALIDATION_EXCEPTION, |_| {
                format!(
                    "For subscriptions in the CloudFront plan family, the associated \
                     resources must always include exactly one Amazon CloudFront \
                     distribution and exactly one WAF web ACL. You cannot remove these \
                     required resources: {}.",
                    distribution("E1")
                )
            }),
        },
        Case {
            label: "the CloudFront family's web ACL cannot be removed",
            family: "CloudFront",
            initial: vec![distribution("E1"), web_acl("acl"), hosted_zone("Z1")],
            operation: "DisassociateResourcesFromSubscription",
            resources: vec![web_acl("acl")],
            outcome: Outcome::Refused(400, VALIDATION_EXCEPTION, |_| {
                format!(
                    "For subscriptions in the CloudFront plan family, the associated \
                     resources must always include exactly one Amazon CloudFront \
                     distribution and exactly one WAF web ACL. You cannot remove these \
                     required resources: {}.",
                    web_acl("acl")
                )
            }),
        },
        Case {
            label: "another family's distribution is removable",
            family: "Route53",
            initial: vec![distribution("E1"), hosted_zone("Z1")],
            operation: "DisassociateResourcesFromSubscription",
            resources: vec![distribution("E1")],
            outcome: Outcome::Covers(vec![hosted_zone("Z1")]),
        },
    ];

    for case in &cases {
        // An account of its own per row: a resource is covered by at most one subscription,
        // so rows naming the same resources are independent accounts.
        let router = router();
        let label = case.label;
        let created = create(&router, label, case.family, "PRO", &case.initial).await;
        let arn = created.string("arn");
        let etag = created.etag(label);

        let answered = call(
            &router,
            case.operation,
            json!({"arn": arn.clone(), "resourceArns": case.resources}),
            Some(&etag),
        )
        .await;

        match &case.outcome {
            Outcome::Covers(expected) => {
                assert_eq!(
                    answered.status, 200,
                    "case {label:?}: {} must succeed, got {} {:?}",
                    case.operation, answered.status, answered.body
                );
                assert_eq!(
                    &answered.resources(label),
                    expected,
                    "case {label:?}: unexpected cover in the answer"
                );
                // The READ-BACK is what proves the cover was stored, not just reported.
                let fetched =
                    call(&router, "GetSubscription", json!({"arn": arn.clone()}), None).await;
                assert_eq!(
                    &fetched.resources(label),
                    expected,
                    "case {label:?}: GetSubscription must report the stored cover"
                );
                assert_eq!(
                    fetched.body, answered.body,
                    "case {label:?}: GetSubscription must agree with {}",
                    case.operation
                );
                assert_ne!(
                    answered.etag(label),
                    etag,
                    "case {label:?}: a mutation must mint a new ETag"
                );
            }
            Outcome::Refused(status, code, message) => {
                assert_eq!(
                    answered.status, *status,
                    "case {label:?}: expected HTTP {status}, got {} {:?}",
                    answered.status, answered.body
                );
                assert_eq!(
                    answered.error_code.as_deref(),
                    Some(*code),
                    "case {label:?}: unexpected error code for {:?}",
                    answered.body
                );
                assert_eq!(
                    answered.message(label),
                    message(&arn),
                    "case {label:?}: unexpected message"
                );
                // A refused mutation is inert: the cover and the tag are unchanged.
                let fetched =
                    call(&router, "GetSubscription", json!({"arn": arn.clone()}), None).await;
                assert_eq!(
                    fetched.resources(label),
                    case.initial,
                    "case {label:?}: a refused mutation must not change the cover"
                );
                assert_eq!(
                    fetched.etag(label),
                    etag,
                    "case {label:?}: a refused mutation must not mint a new ETag"
                );
            }
        }
    }
}

/// `AssociateResourcesToSubscription` needs a subscription whose own state admits a change:
/// one held for approval fails the business rule (HTTP 400), one carrying a scheduled change
/// conflicts with the record's state (HTTP 409), and neither has its set touched.
#[tokio::test]
async fn an_association_needs_an_active_subscription_with_nothing_pending() {
    let router = router();

    // Held for approval: created under MANUAL on a paid tier.
    let held = call(
        &router,
        "CreateSubscription",
        json!({
            "planFamily": "Route53",
            "planTier": "PRO",
            "resourceArns": [hosted_zone("Z1")],
            "approvalMode": "MANUAL",
        }),
        None,
    )
    .await;
    assert_eq!(
        held.status, 200,
        "setup: CreateSubscription must succeed, got {} {:?}",
        held.status, held.body
    );
    let held_arn = held.string("arn");
    let held_etag = held.etag("pending approval");

    // Carrying a scheduled change: an active subscription downgraded. Over a resource of
    // its own, because the subscription above covers the one it was created with.
    let active = create(&router, "pending change", "Route53", "PREMIUM", &[hosted_zone("Z3")]).await;
    let active_arn = active.string("arn");
    let downgraded = call(
        &router,
        "UpdateSubscription",
        json!({"arn": active_arn.clone(), "planTier": "FREE"}),
        Some(&active.etag("pending change")),
    )
    .await;
    assert_eq!(
        downgraded.status, 200,
        "setup: the downgrade must be scheduled, got {} {:?}",
        downgraded.status, downgraded.body
    );
    let pending_etag = downgraded.etag("pending change");

    for (case, arn, etag, status, code, covered) in [
        (
            "pending approval",
            held_arn,
            held_etag,
            400,
            VALIDATION_EXCEPTION,
            hosted_zone("Z1"),
        ),
        (
            "pending change",
            active_arn,
            pending_etag,
            409,
            "ConflictException",
            hosted_zone("Z3"),
        ),
    ] {
        let refused = call(
            &router,
            "AssociateResourcesToSubscription",
            json!({"arn": arn.clone(), "resourceArns": [hosted_zone("Z2")]}),
            Some(&etag),
        )
        .await;
        assert_eq!(
            refused.status, status,
            "case {case:?}: expected HTTP {status}, got {} {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_code.as_deref(),
            Some(code),
            "case {case:?}: unexpected error code for {:?}",
            refused.body
        );
        let fetched = call(&router, "GetSubscription", json!({"arn": arn}), None).await;
        assert_eq!(
            fetched.resources(case),
            vec![covered],
            "case {case:?}: a refused association must not change the set"
        );
    }
}

// ---------------------------------------------------------------------------
// CancelSubscriptionChange
// ---------------------------------------------------------------------------

/// `CancelSubscriptionChange` drops the pending change and the subscription returns to the
/// state it was in before it was scheduled; a subscription that carries no change this call
/// can cancel — none scheduled at all, or one already inside the processing window — is
/// refused as a business-rule validation failure (HTTP 400 `ValidationException`, "the
/// subscription might not be in the required state for this operation") rather than as the
/// conflict, which the model reserves for a request weighed against the record's revision.
///
/// The rows differ along which change is pending (a downgrade, a cancellation) and how far
/// off its effective instant is. The instant a row depends on is WRITTEN into the store the
/// handler reads, so no row depends on when the suite runs.
#[tokio::test]
async fn a_pending_change_is_cancelled_outside_the_processing_window() {
    /// How the pending change is scheduled, and the tier the subscription must still report
    /// once the change is dropped.
    struct Case {
        label: &'static str,
        operation: &'static str,
        body: fn(&str) -> Value,
        tier: &'static str,
    }

    let cases = [
        Case {
            label: "a pending downgrade",
            operation: "UpdateSubscription",
            body: |arn| json!({"arn": arn, "planTier": "FREE"}),
            tier: "PREMIUM",
        },
        Case {
            label: "a pending cancellation",
            operation: "CancelSubscription",
            body: |arn| json!({"arn": arn}),
            tier: "PREMIUM",
        },
    ];

    for case in &cases {
        let label = case.label;
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let router = router_on(Arc::clone(&engine));

        let created = create(&router, label, "Route53", "PREMIUM", &[hosted_zone("Z1")]).await;
        let arn = created.string("arn");

        // Nothing is pending yet, so there is nothing to cancel.
        let nothing = call(
            &router,
            "CancelSubscriptionChange",
            json!({"arn": arn.clone()}),
            Some(&created.etag(label)),
        )
        .await;
        assert_eq!(
            nothing.status, 400,
            "case {label:?}: a subscription with no pending change must answer 400, got {} {:?}",
            nothing.status, nothing.body
        );
        assert_eq!(
            nothing.error_code.as_deref(),
            Some("ValidationException"),
            "case {label:?}: unexpected error code for {:?}",
            nothing.body
        );

        let scheduled = call(&router, case.operation, (case.body)(&arn), Some(&created.etag(label)))
            .await;
        assert_eq!(
            scheduled.status, 200,
            "case {label:?}: {} must schedule the change, got {} {:?}",
            case.operation, scheduled.status, scheduled.body
        );
        assert!(
            scheduled.body.get("scheduledChange").is_some(),
            "case {label:?}: expected a pending change in {:?}",
            scheduled.body
        );

        // Inside the processing window: the change is already being processed. The instant
        // is written into the store rather than waited for, and stays in the future, so the
        // clock owes nothing yet.
        set_effective_date(
            &engine,
            &arn,
            aws_smithy_types::DateTime::from_secs(now().secs() + 60),
        );
        let too_late = call(
            &router,
            "CancelSubscriptionChange",
            json!({"arn": arn.clone()}),
            Some(&scheduled.etag(label)),
        )
        .await;
        assert_eq!(
            too_late.status, 400,
            "case {label:?}: a change inside the processing window must answer 400, got {} {:?}",
            too_late.status, too_late.body
        );
        assert_eq!(
            too_late.error_code.as_deref(),
            Some("ValidationException"),
            "case {label:?}: unexpected error code for {:?}",
            too_late.body
        );
        assert!(
            call(&router, "GetSubscription", json!({"arn": arn.clone()}), None)
                .await
                .body
                .get("scheduledChange")
                .is_some(),
            "case {label:?}: a refused cancellation must leave the change pending"
        );

        // Outside it: the change is dropped and the subscription is as it was.
        set_effective_date(
            &engine,
            &arn,
            aws_smithy_types::DateTime::from_secs(now().secs() + 30 * 24 * 60 * 60),
        );
        let cancelled = call(
            &router,
            "CancelSubscriptionChange",
            json!({"arn": arn.clone()}),
            Some(&scheduled.etag(label)),
        )
        .await;
        assert_eq!(
            cancelled.status, 200,
            "case {label:?}: the pending change must be cancellable, got {} {:?}",
            cancelled.status, cancelled.body
        );
        assert!(
            cancelled.body.get("scheduledChange").is_none(),
            "case {label:?}: the change must be gone, got {:?}",
            cancelled.body
        );

        let fetched = call(&router, "GetSubscription", json!({"arn": arn}), None).await;
        assert_eq!(
            fetched.body, cancelled.body,
            "case {label:?}: GetSubscription must agree with CancelSubscriptionChange"
        );
        assert_eq!(
            (fetched.string("planTier"), fetched.string("status")),
            (case.tier.to_string(), "ACTIVE".to_string()),
            "case {label:?}: the subscription must be as it was before the change, got {:?}",
            fetched.body
        );
        assert_ne!(
            fetched.etag(label),
            scheduled.etag(label),
            "case {label:?}: dropping the change is a write, so the ETag moves"
        );
    }
}

/// Write `effective` onto the pending change of the subscription `arn` names, in the store
/// the handlers read: the date the change takes effect, the confirmation that is what dates
/// it on the wire, and the instant the store applies it at. The one way this suite names an
/// instant: a case that depends on how far off a change is sets that instant rather than
/// waiting for the clock to reach it.
fn set_effective_date(
    engine: &Arc<engine::InMemoryEngine>,
    arn: &str,
    effective: aws_smithy_types::DateTime,
) {
    let state = engine.state::<PricingPlanState>();
    let mut store = state.lock();
    let record = store
        .subscriptions
        .get_mut(arn)
        .expect("the subscription is stored");
    let change = record
        .scheduled_change
        .as_mut()
        .expect("the pending change is stored");
    change.takes_effect_at = effective;
    change.confirmed = true;
    change.due_at = effective;
}

// ---------------------------------------------------------------------------
// Rule 3: a collection-length clause quotes the collection the request carried
// ---------------------------------------------------------------------------

/// Rule 3: a `resourceArns` outside its modelled `@length` is refused with AWS's constraint
/// sentence quoting the LIST the request carried — the elements in order, comma-separated,
/// in brackets — while a list at either published bound is accepted.
///
/// The rows differ along which bound is broken and how far past it the request is, and the
/// operations differ in which member the list sits on.
#[tokio::test]
async fn a_resource_list_outside_its_bound_is_refused_quoting_the_list() {
    let router = router();

    let eleven: Vec<String> = (0..=MAX_SUBSCRIPTION_RESOURCES)
        .map(|index| hosted_zone(&format!("Z{index}")))
        .collect();
    let ten: Vec<String> = eleven[..MAX_SUBSCRIPTION_RESOURCES].to_vec();
    let empty: Vec<String> = Vec::new();

    // (case, the list the request carries, the requirement the clause states)
    let rows = [
        (
            "one element past the modelled maximum",
            eleven.clone(),
            format!("Member must have length less than or equal to {MAX_SUBSCRIPTION_RESOURCES}"),
        ),
        (
            "no element at all, below the modelled minimum",
            empty,
            "Member must have length greater than or equal to 1".to_string(),
        ),
    ];

    for (case, list, requirement) in &rows {
        let refused = call(
            &router,
            "CreateSubscription",
            create_body("Route53", "PRO", list),
            None,
        )
        .await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: a list outside its bound must be refused 400, got {} {:?}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_code.as_deref(),
            Some(VALIDATION_EXCEPTION),
            "case {case:?}: unexpected error code for {:?}",
            refused.body
        );
        assert_eq!(
            refused.message(case),
            format!(
                "1 validation error detected: Value '[{}]' at 'resourceArns' failed to \
                 satisfy constraint: {requirement}",
                list.join(", ")
            ),
            "case {case:?}: unexpected message"
        );
    }

    // At the maximum the same request is served, so the clause above is about the bound.
    let created = create(&router, "at the modelled maximum", "Route53", "PRO", &ten).await;
    let arn = created.string("arn");
    assert_eq!(
        created.resources("at the modelled maximum"),
        ten,
        "case \"at the modelled maximum\": the cover must be the list the request named"
    );

    // The same clause on the other operation whose input carries that list.
    let refused = call(
        &router,
        "AssociateResourcesToSubscription",
        json!({"arn": arn, "resourceArns": eleven}),
        Some(&created.etag("at the modelled maximum")),
    )
    .await;
    assert_eq!(
        refused.status, 400,
        "case \"an association past the modelled maximum\": expected HTTP 400, got {} {:?}",
        refused.status, refused.body
    );
    assert_eq!(
        refused.message("an association past the modelled maximum"),
        format!(
            "1 validation error detected: Value '[{}]' at 'resourceArns' failed to satisfy \
             constraint: Member must have length less than or equal to {MAX_SUBSCRIPTION_RESOURCES}",
            eleven.join(", ")
        ),
        "case \"an association past the modelled maximum\": unexpected message"
    );
}
