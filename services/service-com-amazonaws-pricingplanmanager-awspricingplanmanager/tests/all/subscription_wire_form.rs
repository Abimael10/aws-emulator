// SPDX-License-Identifier: CC0-1.0

//! The wire form of a subscription's `@timestampFormat("date-time")` members and of its
//! concurrency token, on every path that publishes them.
//!
//! Two rules are pinned here.
//!
//! 1. A structure member the model marks `@timestampFormat("date-time")` travels on this
//!    service's restJson1 wire as an RFC 3339 / ISO-8601 date-time STRING — never as the
//!    epoch-seconds number the protocol default would use. The members are
//!    `createdAt`/`updatedAt` on `Subscription` and on `SubscriptionSummary`, and
//!    `effectiveDate` on `ScheduledChange`
//!    (`smithy_query(query="shape", shape="Subscription,SubscriptionSummary,ScheduledChange")`).
//!    The INSTANT is per run; the wire TYPE and format are the requirement, so the
//!    assertions below are about the form and about the three read paths agreeing.
//!
//! 2. The entity tag is a monotonic per-subscription REVISION rendered in decimal: minting
//!    the record is its first write and activating it a second, every mutation hands out
//!    the next integer, a read never moves it, and the same token appears in the `ETag`
//!    response header and in the `eTag` body member of a list summary.
//!
//! Both rules are asserted over TABLES whose rows differ along what the rules generalise
//! over — tier, usage level, resource count, approval mode, whether a change is pending,
//! and the sequence of mutations a subscription has taken — plus one test pinning the
//! protocol seam the first rule's rendering comes from, and two carrying both rules onto
//! the publishers those tables do not read, so neither form can depend on which operation
//! published it.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::{MultiServiceRouter, Service};

use service_com_amazonaws_pricingplanmanager_awspricingplanmanager::ops::types::{
    ScheduledChange, Subscription, SubscriptionSummary,
};
use service_com_amazonaws_pricingplanmanager_awspricingplanmanager::state::{
    parse_date_time, FIRST_REVISION, LIST_PAGE_SIZE,
};
use service_com_amazonaws_pricingplanmanager_awspricingplanmanager::EmulatedService;

// The resource-ARN forms the owning services mint, the `CreateSubscription` body that
// names them exactly, the settle poll and the writer that places a pending change's
// instants are `subscription_lifecycle`'s, so this suite adds no second copy of any of
// them.
use super::subscription_lifecycle::{
    create_body_naming, distribution, hosted_zone, key_value_store, move_change_instants,
    poll_until_active, web_acl,
};
// Making a pending change's confirmation due belongs to the suite that owns that rule.
use super::scheduled_change_confirmation::make_confirmation_due;

/// A SigV4 `Authorization` header scoped to this service, as the router routes on.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-west-2/pricingplanmanager/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0000000000000000000000000000000000000000000000000000000000000000";

/// The region in [`AUTHORIZATION`].
const REGION: &str = "us-west-2";

/// The canonical account id every emulated ARN carries.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// One HTTP exchange: status, parsed body, the `ETag` response header and the error
/// class the `x-amzn-errortype` header names.
///
/// Visible to the sibling suites of this test crate: this is the ONE HTTP harness of the
/// crate's integration tests — the lifecycle suite, the whole-listing form checks in
/// `list_subscriptions_summary_wire_form` and the refusal surface in
/// `subscription_refusal_surface` all drive it.
pub(crate) struct Answer {
    pub(crate) status: u16,
    pub(crate) body: Value,
    pub(crate) etag: Option<String>,
    /// The shape name in `x-amzn-errortype`, without the namespace or URI a restJson1
    /// server may wrap it in; `None` on a success.
    pub(crate) error_code: Option<String>,
}

impl Answer {
    /// The body member `name` as a string, naming the whole body when it is absent.
    pub(crate) fn string(&self, name: &str) -> String {
        self.body
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("member {name} missing from body {:?}", self.body))
            .to_string()
    }

    /// The body member `name`, when present.
    pub(crate) fn opt_string(&self, name: &str) -> Option<String> {
        self.body
            .get(name)
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    /// The `ETag` response header; panics when the operation did not send one.
    pub(crate) fn require_etag(&self, case: &str) -> String {
        self.etag
            .clone()
            .unwrap_or_else(|| panic!("case {case}: no ETag response header on {:?}", self.body))
    }

    /// The `message` member of an error body, naming the whole body when it is absent.
    pub(crate) fn message(&self, case: &str) -> String {
        self.body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case}: no message member in {:?}", self.body))
            .to_string()
    }
}

/// A router with this one service mounted on a shared engine, request authentication off.
pub(crate) fn router() -> MultiServiceRouter {
    router_on(Arc::new(engine::InMemoryEngine::new()))
}

/// The same router, over an engine the caller keeps a handle to — so a test can read or
/// backdate the stored state the handlers see.
pub(crate) fn router_on(engine: Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

/// POST `/v1/<operation>` with `body`, optionally under `If-Match`.
pub(crate) async fn call(
    router: &MultiServiceRouter,
    operation: &str,
    body: Value,
    if_match: Option<&str>,
) -> Answer {
    let mut builder = http::Request::builder()
        .method("POST")
        .uri(format!(
            "https://pricingplanmanager.{REGION}.amazonaws.com/v1/{operation}"
        ))
        .header("host", format!("pricingplanmanager.{REGION}.amazonaws.com"))
        .header("authorization", AUTHORIZATION)
        .header("content-type", "application/json");
    if let Some(tag) = if_match {
        builder = builder.header("if-match", tag);
    }
    let request = builder
        .body(Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let etag = response
        .headers()
        .get("etag")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let error_code = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split('/')
                .next()
                .unwrap_or(value)
                .split('#')
                .next_back()
                .unwrap_or(value)
                .to_string()
        });
    let body = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    Answer {
        status,
        body,
        etag,
        error_code,
    }
}

/// A minimal valid `CreateSubscription` request.
///
/// The resources it names are DISTINCT on every call: a resource is covered by at most one
/// subscription, so two subscriptions can only ever exist side by side over resources of
/// their own. A caller that wants the SAME request twice — an idempotent replay — sends the
/// body this returned a second time rather than building another one.
pub(crate) fn create_body(
    family: &str,
    tier: &str,
    resources: usize,
    usage_level: Option<&str>,
    approval: Option<&str>,
) -> Value {
    let subscription = NEXT_SUBSCRIPTION.fetch_add(1, Ordering::Relaxed);
    let resource_arns: Vec<String> = (0..resources)
        .map(|index| {
            format!("arn:aws:cloudfront::{ACCOUNT}:distribution/E{subscription}X{index}")
        })
        .collect();
    let mut body = json!({
        "planFamily": family,
        "planTier": tier,
        "resourceArns": resource_arns,
    });
    if let Some(level) = usage_level {
        body["usageLevel"] = json!(level);
    }
    if let Some(mode) = approval {
        body["approvalMode"] = json!(mode);
    }
    body
}

/// How many `CreateSubscription` bodies this harness has built, which is what makes each
/// one's resources its own.
static NEXT_SUBSCRIPTION: AtomicU64 = AtomicU64::new(0);

/// Create one subscription, asserting only that it succeeded.
pub(crate) async fn create(router: &MultiServiceRouter, body: Value) -> Answer {
    let answer = call(router, "CreateSubscription", body.clone(), None).await;
    assert_eq!(
        answer.status, 200,
        "CreateSubscription {body:?} must succeed, got {} {:?}",
        answer.status, answer.body
    );
    answer
}

/// Every page of a `ListSubscriptions` walk, in order, following `nextToken` to the end.
///
/// The ONE paging walk of this harness: [`summary_for`] below and the whole-listing suite
/// in `list_subscriptions_summary_wire_form` are both expressed in terms of it, so the
/// page bookkeeping is written once.
pub(crate) async fn pages(router: &MultiServiceRouter) -> Vec<Vec<Value>> {
    let mut pages: Vec<Vec<Value>> = Vec::new();
    let mut next: Option<String> = None;
    loop {
        let body = match next {
            None => json!({}),
            Some(ref token) => json!({"nextToken": token}),
        };
        let listed = call(router, "ListSubscriptions", body, None).await;
        let index = pages.len();
        assert_eq!(
            listed.status, 200,
            "case \"page {index}\": ListSubscriptions must succeed, got {} {:?}",
            listed.status, listed.body
        );
        let page = listed.body["subscriptionSummaries"]
            .as_array()
            .unwrap_or_else(|| {
                panic!(
                    "case \"page {index}\": subscriptionSummaries missing from {:?}",
                    listed.body
                )
            })
            .clone();
        assert!(
            page.len() <= LIST_PAGE_SIZE,
            "case \"page {index}\": a page carries at most {LIST_PAGE_SIZE} summaries, got {}",
            page.len()
        );
        pages.push(page);
        match listed.body["nextToken"].as_str() {
            Some(token) => next = Some(token.to_string()),
            None => return pages,
        }
    }
}

/// The summary `ListSubscriptions` reports for `arn`, searched across every page so a
/// store larger than one page is still covered.
pub(crate) async fn summary_for(router: &MultiServiceRouter, arn: &str) -> Value {
    for page in pages(router).await {
        if let Some(found) = page
            .into_iter()
            .find(|summary| summary["arn"].as_str() == Some(arn))
        {
            return found;
        }
    }
    panic!("no summary for {arn} in the listing")
}

/// Assert `value` is the RFC 3339 date-time STRING a `@timestampFormat("date-time")`
/// member travels as, and return the instant it names in epoch milliseconds.
///
/// A JSON number is exactly the protocol-default encoding this rule forbids, so it is
/// rejected by name rather than parsed.
pub(crate) fn date_time_millis(case: &str, member: &str, value: &Value) -> i64 {
    let text = value.as_str().unwrap_or_else(|| {
        panic!(
            "case {case}: {member} is modelled @timestampFormat(\"date-time\"), so it must be \
             an RFC 3339 string, got {value}"
        )
    });
    // Smithy's `date-time` is RFC 3339 at the `Z` offset: the date/time separator and the
    // zone designator are part of the form, not of the instant.
    assert!(
        text.contains('T') && text.ends_with('Z'),
        "case {case}: {member} must be an RFC 3339 date-time at the Z offset, got {text:?}"
    );
    let parsed = parse_date_time(text)
        .unwrap_or_else(|| panic!("case {case}: {member} = {text:?} must parse as a date-time"));
    parsed.to_millis().unwrap_or_else(|error| {
        panic!("case {case}: {member} = {text:?} must name a representable instant: {error}")
    })
}

// ---------------------------------------------------------------------------
// Rule 1 — the date-time members
// ---------------------------------------------------------------------------

/// Every `date-time` member of a subscription is a date-time string on all three read
/// paths (`CreateSubscription`, `GetSubscription`, `ListSubscriptions`), and the three
/// paths name the same instant.
#[tokio::test]
async fn date_time_members_travel_as_iso_8601_strings_on_every_read_path() {
    struct Case {
        label: &'static str,
        family: &'static str,
        tier: &'static str,
        resources: usize,
        usage_level: Option<&'static str>,
        approval: Option<&'static str>,
        /// A tier to move to after creating, or `None` to leave the record as created.
        then_update_to: Option<&'static str>,
        /// Whether that move is a DOWNGRADE, which leaves a `scheduledChange` behind —
        /// its `effectiveDate` is the third `date-time` member in the model.
        expects_pending_change: bool,
    }

    // The rows differ along plan family, tier, usage level, resource count across the
    // modelled 1..10 bound, approval mode, and whether the record has taken a mutation
    // and carries a pending change — i.e. along everything the rule generalises over.
    let cases = [
        Case {
            label: "free tier, as created",
            family: "CloudFront",
            tier: "FREE",
            resources: 1,
            usage_level: None,
            approval: None,
            then_update_to: None,
            expects_pending_change: false,
        },
        Case {
            label: "paid tier held for approval",
            family: "CloudFront",
            tier: "PREMIUM",
            resources: 10,
            usage_level: Some("LEVEL_2"),
            approval: Some("MANUAL"),
            then_update_to: None,
            expects_pending_change: false,
        },
        Case {
            label: "upgraded immediately",
            family: "Route53",
            tier: "PRO",
            resources: 3,
            usage_level: Some("DEFAULT"),
            approval: Some("IMMEDIATE"),
            then_update_to: Some("BUSINESS"),
            expects_pending_change: false,
        },
        Case {
            label: "downgrade pending, so effectiveDate is populated",
            family: "CloudFront",
            tier: "PREMIUM",
            resources: 2,
            usage_level: None,
            approval: Some("IMMEDIATE"),
            then_update_to: Some("FREE"),
            expects_pending_change: true,
        },
    ];

    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(Arc::clone(&engine));
    for case in &cases {
        let label = case.label;
        let created = create(
            &router,
            create_body(
                case.family,
                case.tier,
                case.resources,
                case.usage_level,
                case.approval,
            ),
        )
        .await;
        let arn = created.string("arn");
        let mut current = created;

        if let Some(target) = case.then_update_to {
            let tag = current.require_etag(label);
            let updated = call(
                &router,
                "UpdateSubscription",
                json!({"arn": arn.clone(), "planTier": target}),
                Some(&tag),
            )
            .await;
            assert_eq!(
                updated.status, 200,
                "case {label}: UpdateSubscription must succeed, got {} {:?}",
                updated.status, updated.body
            );
            current = updated;
            // A downgrade leaves a pending change behind, and that change carries its
            // `effectiveDate` only once it has been confirmed — "This value is populated
            // after the change is confirmed by the billing system" — so the row brings the
            // confirmation forward rather than waiting the span out. The rule itself is
            // `scheduled_change_confirmation`'s; what is asserted here is the FORM the date
            // travels in once it is there.
            if case.expects_pending_change {
                make_confirmation_due(&engine, &arn, label);
            }
        }

        let fetched = call(&router, "GetSubscription", json!({"arn": arn.clone()}), None).await;
        assert_eq!(
            fetched.status, 200,
            "case {label}: GetSubscription must succeed, got {} {:?}",
            fetched.status, fetched.body
        );
        let summary = summary_for(&router, &arn).await;

        // The two required members, on the three paths that publish them.
        for member in ["createdAt", "updatedAt"] {
            let mutated = date_time_millis(
                &format!("{label}/mutating response"),
                member,
                &current.body[member],
            );
            let read = date_time_millis(&format!("{label}/GetSubscription"), member, &fetched.body[member]);
            let listed =
                date_time_millis(&format!("{label}/ListSubscriptions"), member, &summary[member]);
            assert_eq!(
                read, listed,
                "case {label}: the summary's {member} must name the instant the read reports"
            );
            assert_eq!(
                read, mutated,
                "case {label}: {member} must not change between the write and the read"
            );
            assert!(
                read > 1_600_000_000_000,
                "case {label}: {member} must be a real instant, got {read} ms"
            );
        }

        // `effectiveDate` is the one optional `date-time` member; it is present exactly
        // when a change is pending, and then it too is a string on both paths.
        let pending_change = case.expects_pending_change;
        assert_eq!(
            fetched.body.get("scheduledChange").is_some(),
            pending_change,
            "case {label}: a pending change is reported exactly when a downgrade was \
             scheduled, got {:?}",
            fetched.body
        );
        if pending_change {
            let read = date_time_millis(
                &format!("{label}/GetSubscription"),
                "scheduledChange.effectiveDate",
                &fetched.body["scheduledChange"]["effectiveDate"],
            );
            let listed = date_time_millis(
                &format!("{label}/ListSubscriptions"),
                "scheduledChange.effectiveDate",
                &summary["scheduledChange"]["effectiveDate"],
            );
            assert_eq!(
                read, listed,
                "case {label}: the summary's effectiveDate must name the instant the read \
                 reports"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Rule 2 — the concurrency token
// ---------------------------------------------------------------------------

/// The token form the rule requires: a decimal integer, with no sign, no padding and no
/// other character. Returns the revision it names.
pub(crate) fn revision_of(case: &str, token: &str) -> u64 {
    assert!(
        !token.is_empty() && token.bytes().all(|byte| byte.is_ascii_digit()),
        "case {case}: the entity tag is a decimal revision, got {token:?}"
    );
    assert!(
        token == "0" || !token.starts_with('0'),
        "case {case}: a decimal revision carries no leading zero, got {token:?}"
    );
    token
        .parse()
        .unwrap_or_else(|error| panic!("case {case}: {token:?} must parse as a revision: {error}"))
}

/// The entity tag is the record's revision in decimal: the mint after the create, one
/// more when the create also activated the subscription, the next integer after each
/// mutation, unchanged by any read, and identical on the `ETag` response header and the
/// `eTag` body member of the list summary.
#[tokio::test]
async fn the_entity_tag_is_the_decimal_revision_and_advances_once_per_mutation() {
    /// One mutation in a row's sequence: the operation and the body to send it, built
    /// from the subscription's ARN.
    struct Mutation {
        operation: &'static str,
        body: fn(&str) -> Value,
    }

    struct Case {
        label: &'static str,
        tier: &'static str,
        approval: Option<&'static str>,
        /// Whether the create also activates the subscription — a write of its own, so
        /// such a record is one revision past the mint when the create answers, while a
        /// paid tier held for `ApprovePaidSubscription` is still at the mint.
        activated_by_the_create: bool,
        mutations: Vec<Mutation>,
    }

    // The rows differ in the tier they start at, the approval mode, and the number and
    // kind of mutations they take — so what is asserted is the counter, not one path.
    let cases = vec![
        Case {
            label: "no mutation at all",
            tier: "FREE",
            approval: None,
            activated_by_the_create: true,
            mutations: Vec::new(),
        },
        Case {
            label: "approval then two upgrades",
            tier: "PRO",
            approval: Some("MANUAL"),
            activated_by_the_create: false,
            mutations: vec![
                Mutation {
                    operation: "ApprovePaidSubscription",
                    body: |arn| json!({"arn": arn}),
                },
                Mutation {
                    operation: "UpdateSubscription",
                    body: |arn| json!({"arn": arn, "planTier": "BUSINESS"}),
                },
                Mutation {
                    operation: "UpdateSubscription",
                    body: |arn| json!({"arn": arn, "planTier": "PREMIUM"}),
                },
            ],
        },
        Case {
            label: "one upgrade",
            tier: "FREE",
            approval: Some("IMMEDIATE"),
            activated_by_the_create: true,
            mutations: vec![Mutation {
                operation: "UpdateSubscription",
                body: |arn| json!({"arn": arn, "planTier": "BUSINESS"}),
            }],
        },
        Case {
            label: "a scheduled downgrade, then its cancellation",
            tier: "PREMIUM",
            approval: Some("IMMEDIATE"),
            activated_by_the_create: true,
            mutations: vec![
                Mutation {
                    operation: "UpdateSubscription",
                    body: |arn| json!({"arn": arn, "planTier": "PRO"}),
                },
                Mutation {
                    operation: "CancelSubscription",
                    body: |arn| json!({"arn": arn}),
                },
            ],
        },
    ];

    let router = router();
    for case in &cases {
        let label = case.label;
        let created = create(
            &router,
            create_body("CloudFront", case.tier, 1, None, case.approval),
        )
        .await;
        let arn = created.string("arn");
        let mut token = created.require_etag(label);
        // The mint is the record's first write; activating it as part of the create is a
        // second one, which a tier held for approval has not taken yet.
        let first = if case.activated_by_the_create {
            FIRST_REVISION + 1
        } else {
            FIRST_REVISION
        };
        assert_eq!(
            revision_of(label, &token),
            first,
            "case {label}: the create's own writes put the first token at {first}, got \
             {token:?}"
        );

        for (step, mutation) in case.mutations.iter().enumerate() {
            let before = revision_of(label, &token);

            // A read reports the current token and does not move it.
            let read = call(&router, "GetSubscription", json!({"arn": arn.clone()}), None).await;
            assert_eq!(
                read.require_etag(label),
                token,
                "case {label}/step {step}: a read must report the current tag"
            );
            let summary = summary_for(&router, &arn).await;
            assert_eq!(
                summary["eTag"].as_str(),
                Some(token.as_str()),
                "case {label}/step {step}: the summary's eTag must be the current tag, got \
                 {summary:?}"
            );

            let answer = call(
                &router,
                mutation.operation,
                (mutation.body)(&arn),
                Some(&token),
            )
            .await;
            assert_eq!(
                answer.status, 200,
                "case {label}/step {step}: {} under the current If-Match must succeed, got {} {:?}",
                mutation.operation, answer.status, answer.body
            );
            token = answer.require_etag(label);
            assert_eq!(
                revision_of(label, &token),
                before + 1,
                "case {label}/step {step}: {} must advance the revision by one, got {token:?}",
                mutation.operation
            );
        }

        // The record survives every row's sequence here (a cancellation of an ACTIVE
        // subscription schedules the termination rather than deleting the record), so the
        // final token is still readable on both paths.
        let read = call(&router, "GetSubscription", json!({"arn": arn.clone()}), None).await;
        assert_eq!(
            read.status, 200,
            "case {label}: the subscription must still be readable, got {} {:?}",
            read.status, read.body
        );
        assert_eq!(
            read.require_etag(label),
            token,
            "case {label}: the last tag a mutation minted is the one a read reports"
        );
        let summary = summary_for(&router, &arn).await;
        assert_eq!(
            summary["eTag"].as_str(),
            Some(token.as_str()),
            "case {label}: the summary's eTag must equal the record's current tag, got \
             {summary:?}"
        );
        assert_eq!(
            revision_of(label, &token),
            first + case.mutations.len() as u64,
            "case {label}: the revision counts the create's writes plus every mutation"
        );
    }
}

// ---------------------------------------------------------------------------
// Where the date-time form comes from
// ---------------------------------------------------------------------------

/// The `date-time` form reaches the wire through the protocol seam — the shared
/// `restJson1` codec this service returns from `Service::override_protocol` — because the
/// member schemas built from the model carry no timestamp format of their own. This pins
/// both halves of that: the seam is taken, and these are the members that depend on it.
///
/// The member lists are from
/// `smithy_query(query="shape", shape="Subscription,SubscriptionSummary,ScheduledChange")`;
/// they are the model's only timestamp members, which is what makes one protocol-wide
/// correction the right place for the rule.
#[test]
fn the_service_takes_the_shared_date_time_protocol() {
    let service = EmulatedService::new(Arc::new(engine::InMemoryEngine::new()));
    assert!(
        Service::override_protocol(&service).is_some(),
        "case \"protocol seam\": every timestamp member of this model is date-time, so the \
         service must answer override_protocol with the shared date-time codec"
    );

    struct Shape {
        name: &'static str,
        schema: &'static aws_smithy_schema::Schema,
        /// The members the model marks `@timestampFormat("date-time")`.
        date_time_members: &'static [&'static str],
    }

    let shapes = [
        Shape {
            name: "Subscription",
            schema: &Subscription::SCHEMA,
            date_time_members: &["createdAt", "updatedAt"],
        },
        Shape {
            name: "SubscriptionSummary",
            schema: &SubscriptionSummary::SCHEMA,
            date_time_members: &["createdAt", "updatedAt"],
        },
        Shape {
            name: "ScheduledChange",
            schema: &ScheduledChange::SCHEMA,
            date_time_members: &["effectiveDate"],
        },
    ];

    for shape in &shapes {
        let case = shape.name;
        for wanted in shape.date_time_members {
            let member = shape
                .schema
                .members()
                .iter()
                .find(|member| member.member_name() == Some(*wanted))
                .unwrap_or_else(|| {
                    panic!("case {case}: the model declares a {wanted} member, so the schema \
                            must carry one")
                });
            assert_eq!(
                member.shape_type(),
                aws_smithy_schema::ShapeType::Timestamp,
                "case {case}/{wanted}: the model targets this member at Timestamp, got {:?}",
                member.shape_type()
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Both rules, on the remaining operations that publish a subscription
// ---------------------------------------------------------------------------

/// The instants a published subscription or summary names — `createdAt` then `updatedAt`,
/// in epoch milliseconds.
pub(crate) type Instants = (i64, i64);

/// Every `date-time` member the published structure carries, asserted in the form the
/// first rule requires: the two required ones, plus a pending change's `effectiveDate`
/// whenever a change is reported.
fn published_instants(case: &str, published: &Value) -> Instants {
    let created = date_time_millis(case, "createdAt", &published["createdAt"]);
    let updated = date_time_millis(case, "updatedAt", &published["updatedAt"]);
    assert!(
        updated >= created,
        "case {case}: updatedAt is never before createdAt, got {updated} ms and {created} ms"
    );
    let effective = published
        .get("scheduledChange")
        .map(|change| &change["effectiveDate"])
        .filter(|value| !value.is_null());
    if let Some(value) = effective {
        let effective = date_time_millis(case, "scheduledChange.effectiveDate", value);
        assert!(
            effective >= updated,
            "case {case}: a change pending at the end of the billing period takes effect \
             after the write that scheduled it, got {effective} ms and {updated} ms"
        );
    }
    (created, updated)
}

/// Both rules on one `Subscription` payload and the `ETag` header it came with: every
/// `date-time` member is a date-time string, and the tag is `revision` in decimal.
///
/// Visible to the sibling suites of this test crate: this and [`check_published_summary`]
/// are the ONE pair of form assertions the crate's integration tests make, so a suite
/// about a new subject states no second copy of either rule.
pub(crate) fn check_published_subscription(case: &str, answer: &Answer, revision: u64) -> Instants {
    let instants = published_instants(case, &answer.body);
    let token = answer.require_etag(case);
    assert_eq!(
        revision_of(case, &token),
        revision,
        "case {case}: the published entity tag is the record's revision, got {token:?}"
    );
    instants
}

/// The same two rules on a `SubscriptionSummary`, where the model puts the tag in the
/// `eTag` BODY member instead of a response header.
pub(crate) fn check_published_summary(case: &str, summary: &Value, revision: u64) -> Instants {
    let instants = published_instants(case, summary);
    let token = summary["eTag"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: eTag is a required String member, got {summary:?}"));
    assert_eq!(
        revision_of(case, token),
        revision,
        "case {case}: the summary's eTag is the record's revision, got {token:?}"
    );
    instants
}

/// The two reporting paths republish what the last write left: in the same form, naming
/// the same instants, under the same token.
async fn assert_reporting_paths_agree(
    router: &MultiServiceRouter,
    label: &str,
    arn: &str,
    revision: u64,
    written: Instants,
) {
    let read = call(router, "GetSubscription", json!({"arn": arn}), None).await;
    assert_eq!(
        read.status, 200,
        "case {label}: GetSubscription must succeed, got {} {:?}",
        read.status, read.body
    );
    let reported = check_published_subscription(&format!("{label}/GetSubscription"), &read, revision);
    assert_eq!(
        reported, written,
        "case {label}: a read republishes the instants the write published, got {reported:?} \
         vs {written:?}"
    );

    let summary = summary_for(router, arn).await;
    let listed = check_published_summary(&format!("{label}/ListSubscriptions"), &summary, revision);
    assert_eq!(
        listed, written,
        "case {label}: the listing republishes the instants the write published, got \
         {listed:?} vs {written:?}"
    );
}

/// The two publishers of a `Subscription` payload that the tables above do not read:
/// `ApprovePaidSubscription` and `CancelSubscription`.
///
/// `date_time_members_travel_as_iso_8601_strings_on_every_read_path` reads the create, the
/// update and the two reporting paths, and
/// `the_entity_tag_is_the_decimal_revision_and_advances_once_per_mutation` reads the tag
/// every mutation hands out. What no row of either asserts is the FORM of the date-time
/// members on the remaining two publishers. Both rules are asserted here on each of those
/// answers and on the two reporting paths after it, because every publisher renders the
/// same stored record through the one representation/summary conversion and the one
/// protocol seam: neither form can depend on which operation published it.
///
/// A cancelled ACTIVE subscription is also where the model's third date-time member shows
/// up: the pending `CANCELLATION` carries `effectiveDate` once the change has been
/// confirmed, which is a rule of its own (`scheduled_change_confirmation`) and is brought
/// forward here rather than waited out.
///
/// The three remaining publishers — `AssociateResourcesToSubscription`,
/// `DisassociateResourcesFromSubscription` and `CancelSubscriptionChange` — are carried by
/// [`the_resource_change_publishers_use_the_same_wire_form`] below.
#[tokio::test]
async fn the_approve_and_cancel_publishers_use_the_same_wire_form() {
    // (plan family, tier, resource count, usage level). Every row is created under MANUAL
    // approval on a paid tier, which is what holds it for `ApprovePaidSubscription`; the
    // rows differ along everything else the rules generalise over, within the modelled
    // 1..10 bound on `resourceArns`.
    let rows = [
        ("CloudFront", "PRO", 3, Some("DEFAULT")),
        ("Route53", "PREMIUM", 10, Some("LEVEL_2")),
        ("CloudFront", "BUSINESS", 1, None),
    ];

    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(Arc::clone(&engine));
    for (family, tier, resources, usage_level) in rows {
        let label = format!("{family}/{tier}");
        let created = create(
            &router,
            create_body(family, tier, resources, usage_level, Some("MANUAL")),
        )
        .await;
        let arn = created.string("arn");
        let mut token = created.require_etag(&label);
        // A paid tier held for approval has taken the mint and nothing else.
        let mut revision = FIRST_REVISION;
        let published =
            check_published_subscription(&format!("{label}/CreateSubscription"), &created, revision);
        let minted_at = published.0;
        assert_reporting_paths_agree(&router, &label, &arn, revision, published).await;

        // Approving puts the subscription into service; cancelling an ACTIVE subscription
        // schedules the termination, so the record survives that write and the answer
        // carries the pending change whose `effectiveDate` is the model's third date-time
        // member.
        for operation in ["ApprovePaidSubscription", "CancelSubscription"] {
            let case = format!("{label}/{operation}");
            let answer = call(&router, operation, json!({"arn": arn.clone()}), Some(&token)).await;
            assert_eq!(
                answer.status, 200,
                "case {case}: the operation must succeed under the current If-Match, got {} {:?}",
                answer.status, answer.body
            );
            revision += 1;
            token = answer.require_etag(&label);
            let published = check_published_subscription(&case, &answer, revision);
            assert_eq!(
                published.0, minted_at,
                "case {case}: createdAt keeps naming the instant the record was minted, got {} \
                 ms instead of {minted_at} ms",
                published.0
            );
            assert_reporting_paths_agree(&router, &label, &arn, revision, published).await;
        }

        // The `effectiveDate` assertions above are conditional on a change being reported,
        // so pin that the cancellation did leave one behind, and that it is dated once the
        // confirmation it waits for has arrived.
        make_confirmation_due(&engine, &arn, &label);
        let read = call(&router, "GetSubscription", json!({"arn": arn.clone()}), None).await;
        assert!(
            read.body
                .get("scheduledChange")
                .is_some_and(|change| !change["effectiveDate"].is_null()),
            "case {label}: a confirmed cancellation is dated, which is what carries the third \
             date-time member, got {:?}",
            read.body
        );
        check_published_subscription(&format!("{label}/confirmed cancellation"), &read, revision);
    }
}

// ---------------------------------------------------------------------------
// Both rules, on the publishers that change the resource set or drop a change
// ---------------------------------------------------------------------------

/// What a row drives after the create: which of the three remaining publishers of a
/// `Subscription` payload it is about, and what that publisher is handed.
enum Change {
    /// `AssociateResourcesToSubscription`, naming the resources that join the set.
    Associate(Vec<String>),
    /// `DisassociateResourcesFromSubscription`, naming the resources that leave it.
    Disassociate(Vec<String>),
    /// A tier to move DOWN to — a move the service schedules instead of applying, so the
    /// record is left carrying a pending change — and then `CancelSubscriptionChange`,
    /// which drops it.
    CancelPending(&'static str),
}

impl Change {
    /// The publisher this row asserts both rules on.
    fn publisher(&self) -> &'static str {
        match self {
            Change::Associate(_) => "AssociateResourcesToSubscription",
            Change::Disassociate(_) => "DisassociateResourcesFromSubscription",
            Change::CancelPending(_) => "CancelSubscriptionChange",
        }
    }
}

/// One subject: the subscription to create, and the change to drive on it.
struct Publisher {
    label: &'static str,
    family: &'static str,
    tier: &'static str,
    /// The resources the subscription is created over.
    held: Vec<String>,
    change: Change,
}

/// The remaining three publishers of a `Subscription` payload obey the SAME two rules as
/// the create, the update, the approval, the cancellation and the two reporting paths.
///
/// Every one of them renders the stored record through the one representation conversion
/// and the one protocol seam, so neither the date-time form of a timestamp member nor the
/// form of the concurrency token can depend on which operation published it: the token is
/// the record's revision in decimal and advances by one per write — including the write the
/// CLOCK owes a resource change once it has taken effect — and `createdAt`, `updatedAt` and
/// a pending change's `effectiveDate` are RFC 3339 date-time strings.
///
/// The rows differ along what the rules generalise over: the publisher, the plan family,
/// the tier, the kind and number of resources the subscription holds, how many join or
/// leave and in which order, and whether the record carries a pending change when it is
/// published.
#[tokio::test]
async fn the_resource_change_publishers_use_the_same_wire_form() {
    let dist = distribution("E1PUBLISH");
    let acl = web_acl("acl-publish");
    let zone = hosted_zone("Z1PUBLISH");
    let other_zone = hosted_zone("Z2PUBLISH");
    let kvs = key_value_store("kvs-publish");

    let rows = vec![
        Publisher {
            label: "a hosted zone joins a free CloudFront subscription",
            family: "CloudFront",
            tier: "FREE",
            held: vec![dist.clone(), acl.clone()],
            change: Change::Associate(vec![zone.clone()]),
        },
        Publisher {
            label: "a KeyValueStore and a hosted zone join a paid subscription",
            family: "CloudFront",
            tier: "BUSINESS",
            held: vec![dist.clone(), acl.clone()],
            change: Change::Associate(vec![kvs.clone(), zone.clone()]),
        },
        Publisher {
            label: "an optional resource leaves a CloudFront subscription",
            family: "CloudFront",
            tier: "PRO",
            held: vec![dist.clone(), acl.clone(), zone.clone()],
            change: Change::Disassociate(vec![zone.clone()]),
        },
        Publisher {
            label: "two resources leave a family with no required composition",
            family: "Route53",
            tier: "PREMIUM",
            held: vec![zone.clone(), other_zone.clone(), kvs.clone()],
            change: Change::Disassociate(vec![kvs.clone(), other_zone.clone()]),
        },
        Publisher {
            label: "a pending downgrade to the bottom of the tier ladder is dropped",
            family: "CloudFront",
            tier: "PREMIUM",
            held: vec![dist.clone(), acl.clone()],
            change: Change::CancelPending("FREE"),
        },
        Publisher {
            label: "a pending downgrade of another family is dropped",
            family: "Route53",
            tier: "BUSINESS",
            held: vec![zone.clone()],
            change: Change::CancelPending("PRO"),
        },
    ];

    for row in &rows {
        // A router of its own per row: a resource is covered by at most one subscription,
        // so rows naming the same resources are independent accounts rather than one.
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let router = router_on(Arc::clone(&engine));
        let label = row.label;
        let created = create(
            &router,
            create_body_naming(row.family, row.tier, &row.held, Some("IMMEDIATE")),
        )
        .await;
        let arn = created.string("arn");
        let mut token = created.require_etag(label);
        // Minting the record and putting it into service are both writes, so an active
        // subscription's first published revision is one past the mint.
        let mut revision = FIRST_REVISION + 1;
        let minted = check_published_subscription(
            &format!("{label}/CreateSubscription"),
            &created,
            revision,
        );
        assert_reporting_paths_agree(&router, label, &arn, revision, minted).await;

        let case = format!("{label}/{}", row.change.publisher());
        // A change to the resource set is applied asynchronously, so the record is left
        // reporting it as being applied and the clock owes it the move back; dropping a
        // pending change is complete when the call answers.
        let (answer, owed_a_settling_write) = match &row.change {
            Change::Associate(named) => (
                call(
                    &router,
                    "AssociateResourcesToSubscription",
                    json!({"arn": arn.clone(), "resourceArns": named}),
                    Some(&token),
                )
                .await,
                true,
            ),
            Change::Disassociate(named) => (
                call(
                    &router,
                    "DisassociateResourcesFromSubscription",
                    json!({"arn": arn.clone(), "resourceArns": named}),
                    Some(&token),
                )
                .await,
                true,
            ),
            Change::CancelPending(tier) => {
                // A move down the published tier ladder is scheduled rather than applied,
                // so it leaves behind the pending change this publisher drops.
                let scheduled = call(
                    &router,
                    "UpdateSubscription",
                    json!({"arn": arn.clone(), "planTier": tier}),
                    Some(&token),
                )
                .await;
                assert_eq!(
                    scheduled.status, 200,
                    "case {label}: the downgrade must be scheduled, got {} {:?}",
                    scheduled.status, scheduled.body
                );
                revision += 1;
                token = scheduled.require_etag(label);
                let published = check_published_subscription(
                    &format!("{label}/UpdateSubscription"),
                    &scheduled,
                    revision,
                );
                assert_reporting_paths_agree(&router, label, &arn, revision, published).await;

                // The pending change carries its `effectiveDate` — the model's third
                // date-time member — only once it has been confirmed, so the row brings
                // that confirmation forward instead of waiting the span out; the rule is
                // `scheduled_change_confirmation`'s, and what is asserted here is the FORM
                // the date travels in on this publisher's path. Dating the change is no
                // caller's write, so the token does not move with it.
                make_confirmation_due(&engine, &arn, label);
                let dated = call(&router, "GetSubscription", json!({"arn": arn.clone()}), None).await;
                assert!(
                    dated
                        .body
                        .get("scheduledChange")
                        .is_some_and(|change| !change["effectiveDate"].is_null()),
                    "case {label}: a confirmed downgrade is dated, which is what carries \
                     the third date-time member, got {:?}",
                    dated.body
                );
                check_published_subscription(
                    &format!("{label}/confirmed downgrade"),
                    &dated,
                    revision,
                );

                // Well clear of the processing window, so the change is still the caller's
                // to cancel. The instant is WRITTEN rather than waited for, so the row
                // costs no wall-clock time and does not depend on when the suite runs.
                move_change_instants(
                    &engine,
                    &arn,
                    parse_date_time("2099-01-01T00:00:00Z")
                        .expect("the instant is a representable date-time"),
                    label,
                );

                (
                    call(
                        &router,
                        "CancelSubscriptionChange",
                        json!({"arn": arn.clone()}),
                        Some(&token),
                    )
                    .await,
                    false,
                )
            }
        };

        assert_eq!(
            answer.status, 200,
            "case {case}: the publisher must succeed under the current If-Match, got {} {:?}",
            answer.status, answer.body
        );
        revision += 1;
        let published = check_published_subscription(&case, &answer, revision);
        assert_eq!(
            published.0, minted.0,
            "case {case}: createdAt keeps naming the instant the record was minted, got {} \
             ms instead of {} ms",
            published.0, minted.0
        );

        if owed_a_settling_write {
            // The resource change the call started is owed by the clock: the subscription
            // moves back to ACTIVE on its own, and that move is a write like any other, so
            // the token advances once more. Both reporting paths are read AFTER it, so what
            // they report cannot race the change settling between them.
            let settled = poll_until_active(&router, &arn, label).await;
            revision += 1;
            let settled_case = format!("{case}/settled");
            let reported = check_published_subscription(&settled_case, &settled, revision);
            assert_eq!(
                reported.0, minted.0,
                "case {settled_case}: createdAt keeps naming the instant the record was \
                 minted, got {} ms instead of {} ms",
                reported.0, minted.0
            );
            let summary = summary_for(&router, &arn).await;
            let listed = check_published_summary(
                &format!("{settled_case}/ListSubscriptions"),
                &summary,
                revision,
            );
            assert_eq!(
                listed, reported,
                "case {settled_case}: the listing republishes the instants the read reports, \
                 got {listed:?} vs {reported:?}"
            );
        } else {
            assert_reporting_paths_agree(&router, label, &arn, revision, published).await;
        }
    }
}
