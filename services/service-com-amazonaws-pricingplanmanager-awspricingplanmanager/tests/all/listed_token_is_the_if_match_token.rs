// SPDX-License-Identifier: CC0-1.0

//! The concurrency token a `ListSubscriptions` summary publishes is the token the mutation
//! paths weigh `If-Match` against.
//!
//! ONE rule is pinned, and it is the rule the model's own member doc states:
//! `SubscriptionSummary.eTag` is "The entity tag for concurrency control. Pass this value in
//! the If-Match header when making changes to this subscription."
//! (`smithy_query(query="shape", shape="SubscriptionSummary")`). So the entity tag is a
//! single per-record value — the record's monotonic revision rendered in decimal — and a
//! caller that reads it out of the listing can USE it: the listed token is byte for byte the
//! token `GetSubscription` reports, every operation whose `ifMatch` is `@required` accepts
//! it, the write it authorised hands back the next revision, and the token the listing
//! published BEFORE that write is then refused as outdated. A token of some other form — a
//! content digest over the stored record, say — would still be echoed back by a listing, but
//! it could not be the value the `If-Match` comparison is made against and could not count
//! the writes a record has taken, which is what this suite would catch.
//!
//! What the sibling suites pin is the FORM of the published token and of the published
//! instants: `subscription_wire_form` on every publisher of a `Subscription` payload,
//! `list_subscriptions_summary_wire_form` on every row of every page, and
//! `list_summary_wire_form_after_each_writer` whoever wrote the record last. What none of
//! them reads is the ROUND TRIP — the listed token going back out in `If-Match` — so that is
//! what the rows below do, on each of the six operations the model marks `ifMatch`
//! `@required`, across plan families, tiers, approval modes, resource sets, and whether the
//! record carries a pending change when it is listed.
//!
//! No row waits for a change to fall due: the one row that needs a pending change its caller
//! may still drop has the change's instants WRITTEN past the processing window, so nothing
//! here depends on how fast the host runs or on when in the month the suite is run.
//!
//! The HTTP harness, the create body, the paging walk, the settle poll, the pair of form
//! assertions and the instant writer are the sibling suites' own, so this file adds no
//! second copy of any of them: where a row needs the published form, it CALLS
//! `check_published_subscription`/`check_published_summary`, and where it needs the wording
//! of the outdated-tag refusal it states the one sentence `subscription_refusal_surface`
//! pins, with the tag this suite's listing published. What is written here is only the
//! composition those suites do not make.

use std::sync::Arc;

use aws_smithy_types::DateTime;
use serde_json::{json, Value};

use service_com_amazonaws_pricingplanmanager_awspricingplanmanager::state::{
    FIRST_REVISION, FREE_PLAN_TIER,
};

use super::subscription_lifecycle::{
    create_body_naming, distribution, hosted_zone, key_value_store, move_change_instants,
    poll_until_active, web_acl,
};
use super::subscription_wire_form::{
    call, check_published_subscription, check_published_summary, create, revision_of, router_on,
    summary_for,
};

/// An instant far enough ahead that a confirmed change placed at it is outside the window
/// `CancelSubscriptionChange` refuses inside, and that the clock never reaches it, so the one
/// row holding a pending change holds it whenever the suite runs.
const PAST_THE_PROCESSING_WINDOW_SECONDS: i64 = 4_000_000_000;

/// The mutation a row echoes the listed token on. Together the variants are every operation
/// of this service whose `ifMatch` member the model marks `@required`.
enum Mutation {
    /// `UpdateSubscription` re-applying the tier the subscription already has: equal rank is
    /// no downgrade, so the write is applied in place.
    Update,
    /// `CancelSubscription` on an active subscription, which schedules the cancellation and
    /// leaves the record carrying it.
    Cancel,
    /// `ApprovePaidSubscription`, for a paid row created under manual approval.
    Approve,
    /// `AssociateResourcesToSubscription`, adding the named resource.
    Associate(&'static str),
    /// `DisassociateResourcesFromSubscription`, dropping the named resource.
    Disassociate(&'static str),
    /// `CancelSubscriptionChange`, dropping the change the row's preparation scheduled by
    /// asking for the named lower tier.
    CancelChange(&'static str),
}

impl Mutation {
    /// Whether a further `UpdateSubscription` is a write the record admits once this mutation
    /// has been applied. A cancelled subscription carries a pending change, and "You cannot
    /// update a subscription while a scheduled change is pending", so that row stops at the
    /// refusal of the token it replaced.
    fn leaves_the_record_updatable(&self) -> bool {
        !matches!(self, Self::Cancel)
    }
}

/// One subscription to list and then mutate under the token the listing published.
struct Row {
    label: &'static str,
    family: &'static str,
    tier: &'static str,
    approval: Option<&'static str>,
    /// The resources the subscription is created over, by the names [`resource`] gives the
    /// ARNs the owning services mint.
    held: &'static [&'static str],
    mutation: Mutation,
}

/// The ARN a row's resource name stands for. Every row gets a store of its own, so the rows
/// may share these names.
fn resource(name: &str) -> String {
    match name {
        "distribution" => distribution("E1IFMATCH"),
        "web-acl" => web_acl("acl-if-match"),
        "zone" => hosted_zone("Z1IFMATCH"),
        "kvs" => key_value_store("kvs-if-match"),
        other => panic!("the rows name no resource {other:?}"),
    }
}

/// The ARNs `names` names.
fn resources(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| resource(name)).collect()
}

/// The operation a row's mutation is, and the body that asks for it.
fn request(mutation: &Mutation, arn: &str, tier: &str) -> (&'static str, Value) {
    match mutation {
        Mutation::Update => (
            "UpdateSubscription",
            json!({"arn": arn, "planTier": tier}),
        ),
        Mutation::Cancel => ("CancelSubscription", json!({"arn": arn})),
        Mutation::Approve => ("ApprovePaidSubscription", json!({"arn": arn})),
        Mutation::Associate(name) => (
            "AssociateResourcesToSubscription",
            json!({"arn": arn, "resourceArns": [resource(name)]}),
        ),
        Mutation::Disassociate(name) => (
            "DisassociateResourcesFromSubscription",
            json!({"arn": arn, "resourceArns": [resource(name)]}),
        ),
        Mutation::CancelChange(_) => ("CancelSubscriptionChange", json!({"arn": arn})),
    }
}

/// The rows: every `ifMatch`-bearing operation, over subscriptions differing along plan
/// family, tier, approval mode, and the kind and number of resources held (within the
/// modelled 1..10 bound on `resourceArns`).
const ROWS: &[Row] = &[
    Row {
        label: "an in-place update",
        family: "CloudFront",
        tier: "FREE",
        approval: Some("IMMEDIATE"),
        held: &["distribution", "web-acl"],
        mutation: Mutation::Update,
    },
    Row {
        label: "a cancellation of an active subscription",
        family: "CloudFront",
        tier: "PRO",
        approval: Some("IMMEDIATE"),
        held: &["distribution", "web-acl", "zone"],
        mutation: Mutation::Cancel,
    },
    Row {
        label: "an approval of a paid subscription held for one",
        family: "CloudFront",
        tier: "BUSINESS",
        approval: Some("MANUAL"),
        held: &["distribution", "web-acl"],
        mutation: Mutation::Approve,
    },
    Row {
        label: "a resource added",
        family: "CloudFront",
        tier: "PREMIUM",
        approval: Some("IMMEDIATE"),
        held: &["distribution", "web-acl"],
        mutation: Mutation::Associate("zone"),
    },
    Row {
        label: "a resource removed",
        family: "CloudFront",
        tier: "PRO",
        approval: Some("IMMEDIATE"),
        held: &["distribution", "web-acl", "zone", "kvs"],
        mutation: Mutation::Disassociate("kvs"),
    },
    Row {
        label: "a scheduled change dropped",
        family: "CloudFront",
        tier: "PREMIUM",
        approval: Some("IMMEDIATE"),
        held: &["distribution", "web-acl"],
        mutation: Mutation::CancelChange("FREE"),
    },
    Row {
        label: "another plan family, under no approval mode of its own",
        family: "Route53",
        tier: "BUSINESS",
        approval: None,
        held: &["zone"],
        mutation: Mutation::Update,
    },
    Row {
        label: "another plan family, a resource removed",
        family: "Route53",
        tier: "PRO",
        approval: Some("IMMEDIATE"),
        held: &["zone", "kvs"],
        mutation: Mutation::Disassociate("kvs"),
    },
];

/// The listed token is the record's one concurrency token: the read reports it, the mutation
/// paths accept it in `If-Match`, the write it authorised hands back the next revision, and
/// the token the listing published before that write is refused afterwards.
#[tokio::test]
async fn the_token_the_listing_published_is_the_one_if_match_is_weighed_against() {
    for row in ROWS {
        let label = row.label;
        // A store of its own per row, so rows naming the same resources are independent
        // accounts rather than one.
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let router = router_on(Arc::clone(&engine));

        let created = create(
            &router,
            create_body_naming(row.family, row.tier, &resources(row.held), row.approval),
        )
        .await;
        let arn = created.string("arn");
        let created_token = created.require_etag(label);

        // Minting the record is its first write and putting it into service a second, which
        // a paid subscription held for `ApprovePaidSubscription` has not taken yet; the free
        // tier is "always activated immediately regardless of approval mode".
        let activated_by_the_create = row.tier == FREE_PLAN_TIER || row.approval != Some("MANUAL");
        let mut revision = if activated_by_the_create {
            FIRST_REVISION + 1
        } else {
            FIRST_REVISION
        };
        assert_eq!(
            revision_of(label, &created_token),
            revision,
            "case {label}: the create's own writes put the first token at {revision}, got \
             {created_token:?}"
        );

        if let Mutation::CancelChange(target) = &row.mutation {
            let scheduled = call(
                &router,
                "UpdateSubscription",
                json!({"arn": arn.clone(), "planTier": target}),
                Some(&created_token),
            )
            .await;
            assert_eq!(
                scheduled.status, 200,
                "case {label}: scheduling the change to drop must succeed, got {} {:?}",
                scheduled.status, scheduled.body
            );
            assert!(
                scheduled.body.get("scheduledChange").is_some(),
                "case {label}: a move down the tier ladder is scheduled rather than applied, \
                 got {:?}",
                scheduled.body
            );
            revision += 1;
            // The change is placed at a confirmed instant far past the window a change can no
            // longer be cancelled inside, so it is a change this row's caller may still drop
            // whenever the suite runs, and the clock never applies it mid-row. Writing the
            // instants is not a revision of the subscription, so the token does not move.
            move_change_instants(
                &engine,
                &arn,
                DateTime::from_secs(PAST_THE_PROCESSING_WINDOW_SECONDS),
                label,
            );
        }

        // The two reporting paths publish ONE token for the record, in the one form, naming
        // the same instants.
        let read = call(&router, "GetSubscription", json!({"arn": arn.clone()}), None).await;
        assert_eq!(
            read.status, 200,
            "case {label}: GetSubscription must succeed, got {} {:?}",
            read.status, read.body
        );
        let reported =
            check_published_subscription(&format!("{label}/GetSubscription"), &read, revision);
        let summary = summary_for(&router, &arn).await;
        let listed =
            check_published_summary(&format!("{label}/ListSubscriptions"), &summary, revision);
        assert_eq!(
            listed, reported,
            "case {label}: the listing must name the instants the read reports, got {listed:?} \
             vs {reported:?}"
        );
        let listed_token = summary["eTag"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {label}: eTag is a required String member, got {summary:?}")
            })
            .to_string();
        assert_eq!(
            listed_token,
            read.require_etag(label),
            "case {label}: the token the listing publishes is the one the read reports"
        );

        // The rule: that token is what the mutation path weighs `If-Match` against, and the
        // write it authorises is one write.
        let (operation, body) = request(&row.mutation, &arn, row.tier);
        let answer = call(&router, operation, body, Some(&listed_token)).await;
        assert_eq!(
            answer.status, 200,
            "case {label}: {operation} under the token the listing published must be accepted, \
             got {} {:?}",
            answer.status, answer.body
        );
        let after = answer.require_etag(label);
        assert_eq!(
            revision_of(label, &after),
            revision + 1,
            "case {label}: the accepted mutation is one write, so it hands back the next \
             revision {}, got {after:?}",
            revision + 1
        );

        // And the token the listing published BEFORE that write is no longer the record's, so
        // echoing it again is refused rather than applied a second time.
        let stale = call(
            &router,
            "UpdateSubscription",
            json!({"arn": arn.clone(), "planTier": row.tier}),
            Some(&listed_token),
        )
        .await;
        assert_eq!(
            stale.status, 400,
            "case {label}: the token the write consumed must be refused 400, got {} {:?}",
            stale.status, stale.body
        );
        assert_eq!(
            stale.error_code.as_deref(),
            Some("ValidationException"),
            "case {label}: an outdated entity tag is a validation failure, got {:?}",
            stale.error_code
        );
        // The whole sentence, not a substring: this service's tokens are small decimals, so
        // a message merely CONTAINING the token proves nothing. The wording is the one
        // `subscription_refusal_surface` pins for this refusal class; only the rejected tag
        // varies, and here it is the one the listing published.
        assert_eq!(
            stale.message(label),
            format!("Invalid or outdated ETag '{listed_token}': retrieve current ETag and retry."),
            "case {label}: the refusal must quote the tag it was given {listed_token:?}, got \
             {:?}",
            stale.body
        );

        // A token the listing publishes AFTER that write is accepted in its turn, whoever made
        // the last write — the mutation above, or the clock completing the change it began.
        if row.mutation.leaves_the_record_updatable() {
            poll_until_active(&router, &arn, label).await;
            let relisted = summary_for(&router, &arn).await;
            let relisted_token = relisted["eTag"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {label}: eTag is a required String member, got {relisted:?}")
                })
                .to_string();
            let relisted_revision = revision_of(label, &relisted_token);
            assert!(
                relisted_revision > revision,
                "case {label}: the listing publishes the token the mutation left, so it is at \
                 least {}, got {relisted_token:?}",
                revision + 1
            );
            let accepted = call(
                &router,
                "UpdateSubscription",
                json!({"arn": arn.clone(), "planTier": row.tier}),
                Some(&relisted_token),
            )
            .await;
            assert_eq!(
                accepted.status, 200,
                "case {label}: UpdateSubscription under the re-listed token must be accepted, \
                 got {} {:?}",
                accepted.status, accepted.body
            );
            let token = accepted.require_etag(label);
            assert_eq!(
                revision_of(label, &token),
                relisted_revision + 1,
                "case {label}: that write advances the re-listed revision by one, got {token:?}"
            );
        }
    }
}
