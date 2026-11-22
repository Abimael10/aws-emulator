// SPDX-License-Identifier: CC0-1.0

//! The wire form of a `ListSubscriptions` summary does not depend on WHO made the
//! record's last write.
//!
//! Two rules are pinned, the same two the summary's siblings pin:
//!
//! 1. `SubscriptionSummary.createdAt` / `updatedAt` are `@timestampFormat("date-time")`
//!    Timestamp members (`smithy_query(query="shape", shape="SubscriptionSummary")`), so
//!    each travels on this restJson1 service as an RFC 3339 date-time STRING, never as the
//!    protocol-default epoch-seconds JSON number. The instants are per run; the wire type
//!    and format are the requirement.
//! 2. `SubscriptionSummary.eTag` is the record's monotonic revision in decimal — the same
//!    token the `ETag` response header of a read of the same subscription carries — so it
//!    counts the writes the record has taken rather than digesting its content.
//!
//! What this suite adds to `subscription_wire_form` (which carries both rules onto every
//! publisher of a `Subscription` payload, including a resource change the clock has
//! completed) and to `list_subscriptions_summary_wire_form` (every row of every page, over
//! records a caller wrote) is the WRITER no other suite reads the listing after: the CLOCK
//! APPLYING A SCHEDULED DOWNGRADE once the instant it was due at has passed. Both rules
//! are a property of the one summary conversion the listing maps over, so the form a row
//! carries cannot depend on whether its last write was made by the create, by a caller, or
//! by the clock materialising a change it already owed. The rows below differ along exactly
//! that, and along the plan family, the tier, the tier moved to and the kind and number of
//! resources held.
//!
//! No row waits at all: a scheduled change's instants are WRITTEN into the past rather
//! than waited out, so nothing here depends on how fast the host runs and the suite costs
//! no wall-clock time.
//!
//! The HTTP harness, the paging walk, the two form assertions and the instant writer are
//! the sibling suites' own, so this file adds no second copy of any of them.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_pricingplanmanager_awspricingplanmanager::state::FIRST_REVISION;

use super::subscription_lifecycle::{
    create_body_naming, distribution, hosted_zone, key_value_store, move_change_instants,
    web_acl,
};
use super::subscription_wire_form::{
    call, check_published_subscription, check_published_summary, create, router_on, summary_for,
    Answer,
};

/// An instant well in the past, for the row that moves a scheduled change's stored instants
/// so the clock already owes it. All that matters is that it is before now.
const ALREADY_PASSED_SECONDS: i64 = 1_600_000_000;

/// Who makes the record's LAST write before the listing is read.
enum LastWriter {
    /// Nobody after the create: putting a subscription into service is the create's own
    /// second write, so the record is published as the create left it.
    TheCreate,
    /// A caller, through an `UpdateSubscription` that re-applies the tier the subscription
    /// already has — equal rank is no downgrade, so it is applied in place.
    ACaller,
    /// The clock, applying a scheduled downgrade to the named tier once the instant it was
    /// due at has passed.
    TheClockApplyingADowngrade(&'static str),
}

/// One subscription to publish: what to create, and who writes to it last.
struct Row {
    label: &'static str,
    family: &'static str,
    tier: &'static str,
    /// The resources the subscription is created over, by the names [`resource`] gives the
    /// ARNs the owning services mint.
    held: &'static [&'static str],
    last_writer: LastWriter,
}

/// The ARN a row's resource name stands for. A resource is covered by at most one
/// subscription and every row gets a store of its own, so the rows may share these names.
fn resource(name: &str) -> String {
    match name {
        "distribution" => distribution("E1LASTWRITER"),
        "web-acl" => web_acl("acl-last-writer"),
        "zone" => hosted_zone("Z1LASTWRITER"),
        "kvs" => key_value_store("kvs-last-writer"),
        other => panic!("the rows name no resource {other:?}"),
    }
}

/// The ARNs `names` names.
fn resources(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| resource(name)).collect()
}

/// The rows: every writer a subscription record has, across plan families, tiers, and the
/// kind and number of resources held (within the modelled 1..10 bound on `resourceArns`).
const ROWS: &[Row] = &[
    Row {
        label: "the create's own activation",
        family: "CloudFront",
        tier: "FREE",
        held: &["distribution", "web-acl"],
        last_writer: LastWriter::TheCreate,
    },
    Row {
        label: "a caller's in-place update",
        family: "CloudFront",
        tier: "PRO",
        held: &["distribution", "web-acl", "zone"],
        last_writer: LastWriter::ACaller,
    },
    Row {
        label: "the clock applying a scheduled downgrade",
        family: "CloudFront",
        tier: "PREMIUM",
        held: &["distribution", "web-acl"],
        last_writer: LastWriter::TheClockApplyingADowngrade("FREE"),
    },
    Row {
        label: "the clock applying a scheduled downgrade one step down",
        family: "CloudFront",
        tier: "PREMIUM",
        held: &["distribution", "web-acl", "zone", "kvs"],
        last_writer: LastWriter::TheClockApplyingADowngrade("BUSINESS"),
    },
    Row {
        label: "the clock applying a scheduled downgrade of another family",
        family: "Route53",
        tier: "BUSINESS",
        held: &["zone"],
        last_writer: LastWriter::TheClockApplyingADowngrade("PRO"),
    },
];

/// Send `operation` for `arn` under the record's current token, asserting it was accepted.
async fn mutate(
    router: &MultiServiceRouter,
    label: &str,
    operation: &str,
    body: Value,
    token: &str,
) -> Answer {
    let answer = call(router, operation, body, Some(token)).await;
    assert_eq!(
        answer.status, 200,
        "case {label}: {operation} under the current If-Match must succeed, got {} {:?}",
        answer.status, answer.body
    );
    answer
}

/// Drive `row` up to its last write and report the revision the record has reached.
///
/// The writes are COUNTED here, so what the listing must publish is known from the calls
/// that were made rather than read back out of the answer being checked.
async fn drive(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    row: &Row,
    arn: &str,
    created_token: &str,
) -> u64 {
    let label = row.label;
    // Minting the record and putting it into service are both writes, so an active
    // subscription's first published revision is one past the mint.
    let minted = FIRST_REVISION + 1;

    match row.last_writer {
        LastWriter::TheCreate => minted,
        LastWriter::ACaller => {
            mutate(
                router,
                label,
                "UpdateSubscription",
                json!({"arn": arn, "planTier": row.tier}),
                created_token,
            )
            .await;
            minted + 1
        }
        LastWriter::TheClockApplyingADowngrade(target) => {
            let answer = mutate(
                router,
                label,
                "UpdateSubscription",
                json!({"arn": arn, "planTier": target}),
                created_token,
            )
            .await;
            assert!(
                answer.body.get("scheduledChange").is_some(),
                "case {label}: a move down the tier ladder is scheduled rather than \
                 applied, got {:?}",
                answer.body
            );
            // The instant the change is applied at is WRITTEN into the past rather than
            // waited for, so the clock owes the change by the time the next call runs —
            // and applying it is a write like any other.
            move_change_instants(
                engine,
                arn,
                aws_smithy_types::DateTime::from_secs(ALREADY_PASSED_SECONDS),
                label,
            );
            minted + 2
        }
    }
}

/// Both rules hold on the listing whoever wrote the record last: the summary's `eTag` is
/// the record's revision in decimal and is the token a read of the same subscription
/// reports, and `createdAt`/`updatedAt` are RFC 3339 date-time strings naming the instants
/// that read reports.
#[tokio::test]
async fn the_summary_keeps_its_wire_form_whoever_wrote_the_record_last() {
    for row in ROWS {
        let label = row.label;
        // A store of its own per row, so rows naming the same resources are independent
        // accounts rather than one.
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let router = router_on(Arc::clone(&engine));

        let created = create(
            &router,
            create_body_naming(row.family, row.tier, &resources(row.held), Some("IMMEDIATE")),
        )
        .await;
        let arn = created.string("arn");
        let created_token = created.require_etag(label);
        let revision = drive(&router, &engine, row, &arn, &created_token).await;

        // The read is taken FIRST, so a change the clock owes is materialised before
        // either reporting path is compared with the other and neither can race it.
        let read = call(&router, "GetSubscription", json!({"arn": arn.clone()}), None).await;
        assert_eq!(
            read.status, 200,
            "case {label}: GetSubscription must succeed, got {} {:?}",
            read.status, read.body
        );
        let summary = summary_for(&router, &arn).await;

        // Both rules, through the crate's one pair of form assertions: the token is the
        // revision the counted writes put the record at — on the read's `ETag` header and
        // in the summary's `eTag` member alike — and every `date-time` member of either
        // payload is an RFC 3339 string.
        let reported = check_published_subscription(
            &format!("{label}/GetSubscription"),
            &read,
            revision,
        );
        let listed = check_published_summary(
            &format!("{label}/ListSubscriptions"),
            &summary,
            revision,
        );
        assert_eq!(
            listed, reported,
            "case {label}: the summary must name the instants the read reports, got \
             {listed:?} vs {reported:?}"
        );

        // The clock-owed write the row counted really was made: the state it materialises
        // is what the listing reports.
        if let LastWriter::TheClockApplyingADowngrade(target) = row.last_writer {
            assert_eq!(
                summary["planTier"],
                json!(target),
                "case {label}: a due downgrade moves the tier, got {summary:?}"
            );
            assert!(
                summary.get("scheduledChange").is_none_or(Value::is_null),
                "case {label}: a change that has taken effect is no longer pending, got \
                 {summary:?}"
            );
        }
    }
}
