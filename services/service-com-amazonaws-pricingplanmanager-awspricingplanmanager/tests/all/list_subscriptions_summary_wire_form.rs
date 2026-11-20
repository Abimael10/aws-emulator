// SPDX-License-Identifier: CC0-1.0

//! The wire form of EVERY `SubscriptionSummary` a `ListSubscriptions` page carries.
//!
//! `tests/all/subscription_wire_form.rs` pins the two rules on the summary a caller looks
//! up by ARN. This suite pins the same two rules over the WHOLE listing — every row of
//! every page of a store larger than one page — because both rules live in the one shared
//! summary conversion `ListSubscriptions` maps over, so a row's form cannot depend on the
//! row, on its revision, or on the page it lands on:
//!
//! 1. `SubscriptionSummary.createdAt` / `updatedAt` are `@timestampFormat("date-time")`
//!    Timestamp members (`smithy_query(query="shape", shape="SubscriptionSummary")`), so
//!    each travels on this restJson1 service as an RFC 3339 date-time STRING, never as the
//!    protocol-default epoch-seconds JSON number. The instants are per run; the wire type
//!    and format are the requirement, so what is asserted is the form and that the listing
//!    names the instants the single-resource read reports.
//! 2. `SubscriptionSummary.eTag` is the record's monotonic revision rendered in decimal —
//!    the same token the `ETag` response header of a read carries — so it is a small
//!    integer that counts the writes the record has taken, not a digest over its content.
//!
//! The rows differ along plan family, tier, usage level, resource count, approval mode and
//! the number of mutations each subscription has taken (zero through eleven, so the token
//! is asserted at one and at two digits), and there are more of them than
//! [`LIST_PAGE_SIZE`], so the table spans the paging boundary as well.
//!
//! The HTTP harness and the page walk are `subscription_wire_form`'s — `pages` there is the
//! one paging walk both suites use — so this file adds no second copy of either.

use std::collections::BTreeMap;

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_pricingplanmanager_awspricingplanmanager::state::{
    FIRST_REVISION, FREE_PLAN_TIER, LIST_PAGE_SIZE,
};

use super::subscription_wire_form::{
    call, create, create_body, date_time_millis, pages, revision_of, router,
};

/// One subscription to put in the store before the listing is walked.
struct Row {
    label: &'static str,
    family: &'static str,
    tier: &'static str,
    /// How many resource ARNs the subscription covers.
    resources: usize,
    usage_level: Option<&'static str>,
    approval: Option<&'static str>,
    /// How many mutations the subscription takes after the create.
    mutations: u64,
}

/// What the listing must report for one stored subscription.
struct Expected {
    label: &'static str,
    /// The revision the record reached, i.e. the decimal token its summary must carry.
    revision: u64,
}

/// The rows: more than one page's worth, differing along everything the two rules
/// generalise over. `resources` stays within the 1..10 bound the model puts on
/// `resourceArns`.
const ROWS: &[Row] = &[
    Row {
        label: "free tier, untouched",
        family: "CloudFront",
        tier: "FREE",
        resources: 1,
        usage_level: None,
        approval: None,
        mutations: 0,
    },
    Row {
        label: "free tier, one mutation",
        family: "CloudFront",
        tier: "FREE",
        resources: 2,
        usage_level: Some("DEFAULT"),
        approval: Some("IMMEDIATE"),
        mutations: 1,
    },
    Row {
        label: "paid tier held for approval",
        family: "CloudFront",
        tier: "PRO",
        resources: 1,
        usage_level: None,
        approval: Some("MANUAL"),
        mutations: 0,
    },
    Row {
        label: "paid tier held for approval, then mutated twice",
        family: "CloudFront",
        tier: "BUSINESS",
        resources: 3,
        usage_level: Some("LEVEL_2"),
        approval: Some("MANUAL"),
        mutations: 2,
    },
    Row {
        label: "premium, activated immediately",
        family: "CloudFront",
        tier: "PREMIUM",
        resources: 10,
        usage_level: Some("LEVEL_2"),
        approval: Some("IMMEDIATE"),
        mutations: 3,
    },
    Row {
        label: "another plan family",
        family: "Route53",
        tier: "PRO",
        resources: 4,
        usage_level: Some("DEFAULT"),
        approval: None,
        mutations: 1,
    },
    Row {
        label: "another plan family, eleven mutations so the token is two digits",
        family: "Route53",
        tier: "BUSINESS",
        resources: 2,
        usage_level: None,
        approval: Some("IMMEDIATE"),
        mutations: 11,
    },
    Row {
        label: "the bottom of the published tier ladder",
        family: "CloudFront",
        tier: "FREE",
        resources: 1,
        usage_level: None,
        approval: Some("IMMEDIATE"),
        mutations: 2,
    },
    Row {
        label: "free tier under manual approval is still activated",
        family: "CloudFront",
        tier: "FREE",
        resources: 5,
        usage_level: Some("DEFAULT"),
        approval: Some("MANUAL"),
        mutations: 1,
    },
    Row {
        label: "premium, untouched",
        family: "Route53",
        tier: "PREMIUM",
        resources: 1,
        usage_level: None,
        approval: None,
        mutations: 0,
    },
    Row {
        label: "business, four mutations",
        family: "CloudFront",
        tier: "BUSINESS",
        resources: 6,
        usage_level: Some("LEVEL_2"),
        approval: Some("IMMEDIATE"),
        mutations: 4,
    },
    Row {
        label: "pro, nine mutations",
        family: "CloudFront",
        tier: "PRO",
        resources: 2,
        usage_level: None,
        approval: None,
        mutations: 9,
    },
    Row {
        label: "free tier, ten resources",
        family: "Route53",
        tier: "FREE",
        resources: 10,
        usage_level: Some("DEFAULT"),
        approval: Some("IMMEDIATE"),
        mutations: 1,
    },
];

/// Create `row`'s subscription, drive its mutations, and report its ARN with the revision
/// the record reached.
async fn stock(router: &MultiServiceRouter, row: &Row) -> (String, u64) {
    let label = row.label;
    let created = create(
        router,
        create_body(
            row.family,
            row.tier,
            row.resources,
            row.usage_level,
            row.approval,
        ),
    )
    .await;
    let arn = created.string("arn");
    let mut token = created.require_etag(label);

    // Minting is the record's first write; putting it into service is a second one, which
    // a paid tier held for `ApprovePaidSubscription` has not taken yet. The free tier is
    // "always activated immediately regardless of approval mode".
    let activated_by_the_create = row.tier == FREE_PLAN_TIER || row.approval != Some("MANUAL");
    let mut revision = if activated_by_the_create {
        FIRST_REVISION + 1
    } else {
        FIRST_REVISION
    };
    assert_eq!(
        revision_of(label, &token),
        revision,
        "case {label}: the create's own writes put the first token at {revision}, got \
         {token:?}"
    );

    for step in 0..row.mutations {
        // Re-applying the tier the subscription already has is an in-place update: equal
        // rank is not a downgrade, so it schedules nothing and stays repeatable, which is
        // what lets a row take an arbitrary number of writes.
        let answer = call(
            router,
            "UpdateSubscription",
            json!({"arn": arn.clone(), "planTier": row.tier}),
            Some(&token),
        )
        .await;
        assert_eq!(
            answer.status, 200,
            "case {label}/mutation {step}: UpdateSubscription under the current If-Match \
             must succeed, got {} {:?}",
            answer.status, answer.body
        );
        token = answer.require_etag(label);
        revision += 1;
        assert_eq!(
            revision_of(label, &token),
            revision,
            "case {label}/mutation {step}: each write must advance the revision by one, \
             got {token:?}"
        );
    }

    (arn, revision)
}

/// Both rules hold for every summary of every page: the token is the record's decimal
/// revision and equals the `ETag` a read of the same subscription reports, and the two
/// required date-time members are RFC 3339 strings naming the instants that read reports.
#[tokio::test]
async fn every_summary_of_every_page_carries_the_decimal_token_and_date_time_members() {
    let router = router();

    let mut expected: BTreeMap<String, Expected> = BTreeMap::new();
    for row in ROWS {
        let (arn, revision) = stock(&router, row).await;
        let previous = expected.insert(
            arn.clone(),
            Expected {
                label: row.label,
                revision,
            },
        );
        assert!(
            previous.is_none(),
            "case {}: every create must mint its own ARN, got {arn} twice",
            row.label
        );
    }

    let pages = pages(&router).await;
    assert!(
        pages.len() > 1,
        "case \"paging\": {} subscriptions exceed the {LIST_PAGE_SIZE}-summary page, so the \
         listing must span more than one page, got {} page(s)",
        ROWS.len(),
        pages.len()
    );

    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    for (index, page) in pages.iter().enumerate() {
        for summary in page {
            let arn = summary["arn"]
                .as_str()
                .unwrap_or_else(|| panic!("case \"page {index}\": a summary without an arn: {summary:?}"))
                .to_string();
            let Expected { label, revision } = expected
                .get(&arn)
                .unwrap_or_else(|| panic!("case \"page {index}\": {arn} was never created"));
            let case = format!("{label}/page {index}");
            assert!(
                seen.insert(arn.clone(), index).is_none(),
                "case {case}: a paged listing reports each subscription once, got {arn} again"
            );

            // Rule 2 — the token.
            let listed_token = summary["eTag"].as_str().unwrap_or_else(|| {
                panic!("case {case}: eTag is a required String member, got {summary:?}")
            });
            assert_eq!(
                revision_of(&case, listed_token),
                *revision,
                "case {case}: the summary's eTag is the record's revision, got \
                 {listed_token:?}"
            );

            let read = call(&router, "GetSubscription", json!({"arn": arn.clone()}), None).await;
            assert_eq!(
                read.status, 200,
                "case {case}: GetSubscription must succeed, got {} {:?}",
                read.status, read.body
            );
            assert_eq!(
                read.require_etag(&case),
                listed_token,
                "case {case}: the listed token must be the one a read of the same \
                 subscription reports"
            );

            // Rule 1 — the date-time members.
            let mut instants = Vec::new();
            for member in ["createdAt", "updatedAt"] {
                let listed = date_time_millis(&case, member, &summary[member]);
                let from_read = date_time_millis(&case, member, &read.body[member]);
                assert_eq!(
                    listed, from_read,
                    "case {case}: the summary's {member} must name the instant the read \
                     reports, got {listed} ms vs {from_read} ms"
                );
                instants.push(listed);
            }
            assert!(
                instants[1] >= instants[0],
                "case {case}: updatedAt is never before createdAt, got {} ms and {} ms",
                instants[1],
                instants[0]
            );
        }
    }

    assert_eq!(
        seen.len(),
        expected.len(),
        "case \"paging\": every stored subscription must appear in the listing, got {} of {}",
        seen.len(),
        expected.len()
    );
}
