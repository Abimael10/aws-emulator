// SPDX-License-Identifier: CC0-1.0

//! When a pending change on a subscription carries a date, and what that date is.
//!
//! `ScheduledChange$effectiveDate` is documented "The date and time when the change takes
//! effect, in ISO 8601 format. This value is populated after the change is confirmed by the
//! billing system." (`smithy_query(query="shape", shape="ScheduledChange")`). So the member
//! is not the scheduling call's to publish: the answer that ATTACHES a pending change, and
//! every read of the subscription until that change has been confirmed, reports the change
//! without it. Once the confirmation has arrived the date appears, and it is the end of the
//! current billing period — the instant `CancelSubscription` ("the cancellation is
//! scheduled to take effect at the end of the current billing period") and
//! `UpdateSubscription` ("the current tier remains unchanged until the end of the billing
//! cycle (calendar month)") document their changes taking effect.
//!
//! The rule is about every pending change, so the table below spans both modelled
//! `ScheduledChangeType` values and both operations that attach one, across plan families,
//! tiers, resource counts and approval modes. The second test drops the writes and waits:
//! the confirmation is the service's own to make, so it arrives with no further call.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_pricingplanmanager_awspricingplanmanager::state::{
    now as state_now, ComAmazonawsPricingplanmanagerAwspricingplanmanagerState as ServiceState,
    ScheduledChangeRecord,
};

// The HTTP harness and the assertions this suite shares with its siblings: the one request
// builder and paging walk in `subscription_wire_form`, and the end-of-billing-period
// assertion of `subscription_lifecycle`.
use super::subscription_lifecycle::{assert_end_of_billing_period, epoch_seconds};
use super::subscription_wire_form::{
    call, create, create_body, date_time_millis, router_on, summary_for, Answer,
};

/// The pending change a published subscription or summary reports, when it reports one.
fn pending_change(published: &Value) -> Option<&Value> {
    published
        .get("scheduledChange")
        .filter(|change| !change.is_null())
}

/// The date a pending change reports, when it carries one. A member the service leaves
/// absent and one it sends as `null` are the same answer to a client, so both read as "no
/// date".
fn reported_date(change: &Value) -> Option<&Value> {
    change
        .get("effectiveDate")
        .filter(|value| !value.is_null())
}

/// Rewrite the instants of the change pending on `arn` through `place`.
///
/// A row places the confirmation before or after the clock by WRITING its due instant
/// rather than by waiting for it, so no row costs wall-clock time or depends on how fast
/// the host runs. What the service does with that instant is still the service's: the
/// confirmation itself is materialised by the handler, not written here.
fn place_change_instants(
    engine: &Arc<engine::InMemoryEngine>,
    arn: &str,
    case: &str,
    place: impl FnOnce(&mut ScheduledChangeRecord),
) {
    let state = engine.state::<ServiceState>();
    let mut store = state.lock();
    let record = store
        .subscriptions
        .get_mut(arn)
        .unwrap_or_else(|| panic!("case {case}: the subscription is stored"));
    let change = record
        .scheduled_change
        .as_mut()
        .unwrap_or_else(|| panic!("case {case}: the pending change is stored"));
    place(change);
}

/// How far past the clock an instant a row must not reach is placed.
const WELL_PAST_THE_TEST_SECONDS: i64 = 365 * 24 * 60 * 60;

/// Hold the confirmation of the change pending on `arn` off for the rest of the test.
///
/// Both the confirmation and the instant the store applies the change at are pushed well
/// past the end of the test, so what a read reports is the UNCONFIRMED form however long
/// the host takes between two calls.
fn hold_confirmation_off(engine: &Arc<engine::InMemoryEngine>, arn: &str, case: &str) {
    let far_future = aws_smithy_types::DateTime::from_secs(
        state_now().secs() + WELL_PAST_THE_TEST_SECONDS,
    );
    place_change_instants(engine, arn, case, |change| {
        change.confirm_at = far_future;
        change.due_at = far_future;
    });
}

/// Make the confirmation of the change pending on `arn` already due.
///
/// Only the instant the confirmation was owed at is written: the next call is what
/// confirms the change, through the same clock-driven path a caller-free wait goes
/// through. Shared with the suites that need the DATED form of a pending change without
/// spending the span on it.
pub(crate) fn make_confirmation_due(
    engine: &Arc<engine::InMemoryEngine>,
    arn: &str,
    case: &str,
) {
    let already_passed = aws_smithy_types::DateTime::from_secs(state_now().secs() - 1);
    place_change_instants(engine, arn, case, |change| {
        change.confirm_at = already_passed;
    });
}

/// One row's pending change: which operation attaches it, and what it is.
struct Case {
    label: &'static str,
    family: &'static str,
    tier: &'static str,
    resources: usize,
    approval: Option<&'static str>,
    /// The tier to move to, which makes the row a `DOWNGRADE`; `None` cancels instead,
    /// which makes it a `CANCELLATION`.
    downgrade_to: Option<&'static str>,
}

impl Case {
    /// The modelled `ScheduledChangeType` this row's change carries.
    fn change_type(&self) -> &'static str {
        if self.downgrade_to.is_some() {
            "DOWNGRADE"
        } else {
            "CANCELLATION"
        }
    }

    /// The request that attaches this row's pending change to `arn`.
    fn request(&self, arn: &str) -> (&'static str, Value) {
        match self.downgrade_to {
            Some(target) => (
                "UpdateSubscription",
                json!({"arn": arn, "planTier": target}),
            ),
            None => ("CancelSubscription", json!({"arn": arn})),
        }
    }
}

/// The rows: both change types, both operations that attach one, across plan families,
/// tiers, resource counts within the modelled 1..10 bound and approval modes. Every row
/// is created ACTIVE, which is the status both operations schedule a change on.
const CASES: &[Case] = &[
    Case {
        label: "cancel a free CloudFront subscription",
        family: "CloudFront",
        tier: "FREE",
        resources: 2,
        approval: None,
        downgrade_to: None,
    },
    Case {
        label: "cancel a paid CloudFront subscription",
        family: "CloudFront",
        tier: "PRO",
        resources: 1,
        approval: Some("IMMEDIATE"),
        downgrade_to: None,
    },
    Case {
        label: "cancel a top-tier Route53 subscription",
        family: "Route53",
        tier: "PREMIUM",
        resources: 3,
        approval: Some("IMMEDIATE"),
        downgrade_to: None,
    },
    Case {
        label: "downgrade to the bottom of the ladder",
        family: "CloudFront",
        tier: "PREMIUM",
        resources: 1,
        approval: Some("IMMEDIATE"),
        downgrade_to: Some("FREE"),
    },
    Case {
        label: "downgrade one step",
        family: "Route53",
        tier: "BUSINESS",
        resources: 4,
        approval: Some("IMMEDIATE"),
        downgrade_to: Some("PRO"),
    },
];

/// A pending change carries NO date until it has been confirmed, and then carries the end
/// of the current billing period — on the answer that attaches it and on both reporting
/// paths.
#[tokio::test]
async fn a_pending_change_carries_no_date_until_it_is_confirmed() {
    for case in CASES {
        let label = case.label;
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let router = router_on(Arc::clone(&engine));
        let created = create(
            &router,
            create_body(case.family, case.tier, case.resources, None, case.approval),
        )
        .await;
        let arn = created.string("arn");
        let status = created.string("status");
        let etag = created.require_etag(label);

        let (operation, body) = case.request(&arn);
        let scheduled = call(&router, operation, body, Some(&etag)).await;
        assert_eq!(
            scheduled.status, 200,
            "case {label}: {operation} must succeed, got {} {:?}",
            scheduled.status, scheduled.body
        );
        let token = scheduled.require_etag(label);
        let attached = pending_change(&scheduled.body)
            .unwrap_or_else(|| panic!("case {label}: no scheduledChange in {:?}", scheduled.body));
        assert_eq!(
            attached["changeType"],
            json!(case.change_type()),
            "case {label}: unexpected changeType in {attached:?}"
        );
        assert!(
            reported_date(attached).is_none(),
            "case {label}: the call that attaches a pending change publishes no \
             effectiveDate, got {attached:?}"
        );
        let written_at = epoch_seconds(&scheduled.body["updatedAt"], label);

        // Both reporting paths report the same undated change, for as long as the
        // confirmation has not arrived — and reading it is not a write.
        hold_confirmation_off(&engine, &arn, label);
        let read = call(&router, "GetSubscription", json!({"arn": arn.clone()}), None).await;
        let listed = summary_for(&router, &arn).await;
        for (path, published) in [("GetSubscription", &read.body), ("ListSubscriptions", &listed)] {
            let change = pending_change(published).unwrap_or_else(|| {
                panic!("case {label}/{path}: no scheduledChange in {published:?}")
            });
            assert!(
                reported_date(change).is_none(),
                "case {label}/{path}: an unconfirmed change carries no effectiveDate, got \
                 {change:?}"
            );
        }
        assert_eq!(
            read.require_etag(label),
            token,
            "case {label}: reading an unconfirmed change must not move the ETag"
        );

        // Once the confirmation is due, the date appears on both paths: the end of the
        // billing period the change was scheduled in.
        make_confirmation_due(&engine, &arn, label);
        let confirmed = call(&router, "GetSubscription", json!({"arn": arn.clone()}), None).await;
        let listed = summary_for(&router, &arn).await;
        let mut dated: Vec<i64> = Vec::new();
        for (path, published) in
            [("GetSubscription", &confirmed.body), ("ListSubscriptions", &listed)]
        {
            let case_path = format!("{label}/{path}");
            let change = pending_change(published).unwrap_or_else(|| {
                panic!("case {case_path}: no scheduledChange in {published:?}")
            });
            let value = reported_date(change).unwrap_or_else(|| {
                panic!(
                    "case {case_path}: a confirmed change reports the instant it takes \
                     effect, got {change:?}"
                )
            });
            dated.push(date_time_millis(
                &case_path,
                "scheduledChange.effectiveDate",
                value,
            ));
            assert_end_of_billing_period(epoch_seconds(value, &case_path), written_at, &case_path);
        }
        assert_eq!(
            dated[0], dated[1],
            "case {label}: the summary's effectiveDate must name the instant the read reports"
        );

        // Dating the change is not applying it, and not a revision of the subscription:
        // the record still reports what the scheduling write left.
        assert_eq!(
            confirmed.string("planTier"),
            case.tier,
            "case {label}: a confirmed change has not taken effect yet, got {:?}",
            confirmed.body
        );
        assert_eq!(
            confirmed.string("status"),
            status,
            "case {label}: the subscription keeps its status until the change takes effect, \
             got {:?}",
            confirmed.body
        );
        assert_eq!(
            confirmed.require_etag(label),
            token,
            "case {label}: the confirmation is not a write by a caller, so the ETag stands"
        );
        assert_eq!(
            confirmed.body["updatedAt"], scheduled.body["updatedAt"],
            "case {label}: the confirmation does not modify the subscription, got {:?}",
            confirmed.body
        );
    }
}

/// The confirmation is the service's own to make: a client that schedules a change and then
/// waits sees the date appear with no further call of its own making it happen.
///
/// Nothing is written here — this is the one test of the rule that lets the clock do it, so
/// the rows are DOWNGRADES, whose change stays pending until the end of the billing cycle
/// and so stays readable for as long as the wait takes. (A cancellation's own transition
/// ends the subscription, which `subscription_lifecycle` waits for.)
#[tokio::test]
async fn a_pending_change_is_dated_on_the_clock_without_another_call() {
    let rows = [
        ("CloudFront", "PREMIUM", "FREE", 1),
        ("Route53", "BUSINESS", "PRO", 3),
    ];

    let router = router_on(Arc::new(engine::InMemoryEngine::new()));
    for (family, from, to, resources) in rows {
        let label = format!("{family}/{from} -> {to}");
        let created = create(
            &router,
            create_body(family, from, resources, None, Some("IMMEDIATE")),
        )
        .await;
        let arn = created.string("arn");
        let etag = created.require_etag(&label);

        let scheduled = call(
            &router,
            "UpdateSubscription",
            json!({"arn": arn.clone(), "planTier": to}),
            Some(&etag),
        )
        .await;
        assert_eq!(
            scheduled.status, 200,
            "case {label}: UpdateSubscription must succeed, got {} {:?}",
            scheduled.status, scheduled.body
        );
        let attached = pending_change(&scheduled.body)
            .unwrap_or_else(|| panic!("case {label}: no scheduledChange in {:?}", scheduled.body));
        assert!(
            reported_date(attached).is_none(),
            "case {label}: the call that attaches a pending change publishes no \
             effectiveDate, got {attached:?}"
        );
        let written_at = epoch_seconds(&scheduled.body["updatedAt"], &label);

        let dated = poll_until_dated(&router, &arn, &label).await;
        let value = reported_date(&dated)
            .unwrap_or_else(|| panic!("case {label}: the polled change is dated, got {dated:?}"));
        assert_end_of_billing_period(epoch_seconds(value, &label), written_at, &label);
    }
}

/// Poll `GetSubscription` until the change pending on `arn` carries its date, and report
/// the change.
///
/// Waits for the CONDITION, not for a time: when the confirmation arrives is the service's
/// business, so this looks again until the change is dated or a deadline far past it
/// passes.
async fn poll_until_dated(router: &MultiServiceRouter, arn: &str, case: &str) -> Value {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    loop {
        let fetched: Answer = call(router, "GetSubscription", json!({"arn": arn}), None).await;
        if let Some(change) = pending_change(&fetched.body) {
            if reported_date(change).is_some() {
                return change.clone();
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case}: the pending change was never dated, last answer {} {:?}",
            fetched.status,
            fetched.body
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}
