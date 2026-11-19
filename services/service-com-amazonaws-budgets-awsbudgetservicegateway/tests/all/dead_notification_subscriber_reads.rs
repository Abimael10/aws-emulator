// SPDX-License-Identifier: CC0-1.0

//! `DescribeSubscribersForNotification` for a subscriber set the account does NOT hold.
//!
//! The RULE asserted here: this read reports the subscribers stored for the notification
//! the request addresses, and it reports the EMPTY set by leaving the optional
//! `Subscribers` member out — the list is `@length(min: 1, max: 11)`
//! (`smithy_query(query="shape",shape="DescribeSubscribersForNotificationResponse")`), so
//! no empty list can be serialized for it. Nothing has to EXIST for "which subscribers does
//! this notification have" to have an answer — a notification exists only as the set of
//! subscribers attached to it (`Subscribers` is `@length(min: 1)` on the writes too, so
//! deleting the last subscriber takes the notification with it) — so an addressee the
//! account does not hold, a notification the budget does not hold OR a budget name nothing
//! is stored under, answers HTTP 200 with no `Subscribers` key rather than an error. The
//! calls whose subject IS the budget keep refusing it with `NotFoundException`.
//!
//! The rows span what the rule generalises over:
//!
//! * HOW the notification came to be absent — `DeleteNotification` removed it,
//!   `DeleteSubscriber` removed its last subscriber, `UpdateNotification` moved it to
//!   another identity, a refused create never stored it, and it was never created at all;
//! * WHY the budget is not held — never created, a stored name in another case, deleted,
//!   and past its end date;
//! * WHICH notification is addressed — a catalog spanning every member a notification's
//!   identity is made of (the notification type, the comparison operator, the threshold at
//!   each published `@range` bound of `NotificationThreshold` and between them, and the
//!   threshold type, absent on one row);
//! * WHAT the budget holds beside it — nothing, and another notification whose own
//!   subscribers the read must not report.
//!
//! Driven over the wire through the same `MultiServiceRouter` the production router serves
//! from; the harness is the lifecycle suite's.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use std::time::{SystemTime, UNIX_EPOCH};

use super::budget_lifecycle::{caller_account, call, is_error, router, same};
use super::client_error_envelopes::message;
use super::notification_lifecycle::seed_budget;

/// The notifications the tables address. The rows differ along every member a
/// notification's identity is made of, including both published `@range` bounds of
/// `Notification$Threshold` (0 and 15000000000000) and an absent `ThresholdType`
/// (`smithy_query(query="shape",shape="Notification")`).
fn notification_catalog() -> Vec<Value> {
    vec![
        json!({
            "NotificationType": "ACTUAL",
            "ComparisonOperator": "GREATER_THAN",
            "Threshold": 80
        }),
        json!({
            "NotificationType": "FORECASTED",
            "ComparisonOperator": "LESS_THAN",
            "Threshold": 0,
            "ThresholdType": "ABSOLUTE_VALUE"
        }),
        json!({
            "NotificationType": "ACTUAL",
            "ComparisonOperator": "EQUAL_TO",
            "Threshold": 15_000_000_000_000.0_f64,
            "ThresholdType": "PERCENTAGE"
        }),
        json!({
            "NotificationType": "FORECASTED",
            "ComparisonOperator": "GREATER_THAN",
            "Threshold": 42.5,
            "ThresholdType": "PERCENTAGE"
        }),
    ]
}

/// The subscriber set one notification is created with. Sets differ per position so a read
/// that reported another notification's subscribers is visible in the failure.
fn subscriber_set(position: usize) -> Value {
    json!([
        {"SubscriptionType": "EMAIL", "Address": format!("dead-{position}@example.com")},
        {"SubscriptionType": "EMAIL", "Address": format!("dead-{position}-second@example.com")}
    ])
}

/// Attach `notification` to `budget_name` with `subscribers`, failing the test if the
/// setup call did not take.
async fn seed_notification(
    router: &MultiServiceRouter,
    account_id: &str,
    budget_name: &str,
    notification: &Value,
    subscribers: &Value,
) {
    let (status, body) = call(
        router,
        "CreateNotification",
        &json!({
            "AccountId": account_id,
            "BudgetName": budget_name,
            "Notification": notification,
            "Subscribers": subscribers
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: expected CreateNotification on {budget_name:?} to answer 200, got {status} {body}"
    );
}

/// One `DescribeSubscribersForNotification` call.
async fn read_subscribers(
    router: &MultiServiceRouter,
    account_id: &str,
    budget_name: &str,
    notification: &Value,
) -> (u16, Value) {
    call(
        router,
        "DescribeSubscribersForNotification",
        &json!({
            "AccountId": account_id,
            "BudgetName": budget_name,
            "Notification": notification
        }),
    )
    .await
}

/// Assert that the read of `notification` under `budget_name` answers the empty subscriber
/// set: HTTP 200 with no `Subscribers` member and nothing to continue.
async fn assert_empty_read(
    router: &MultiServiceRouter,
    account_id: &str,
    budget_name: &str,
    notification: &Value,
    case: &str,
) {
    let (status, body) = read_subscribers(router, account_id, budget_name, notification).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 for a notification the budget does not hold, \
         got {status} {body}"
    );
    assert!(
        body.get("Subscribers").is_none_or(Value::is_null),
        "case {case:?}: expected the Subscribers member to be left out, got {body}"
    );
    assert!(
        body.get("NextToken").is_none_or(Value::is_null),
        "case {case:?}: expected nothing to continue, got {body}"
    );
}

/// How the notification under test came to be absent from its budget.
#[derive(Clone, Copy, Debug)]
enum Absent {
    /// It was created and then deleted outright.
    NotificationDeleted,
    /// It was created and lost its subscribers one by one, the last taking it with it.
    LastSubscriberDeleted,
    /// It was created and then moved to another identity by an update.
    MovedByUpdate,
    /// A create that was refused never stored it.
    RefusedCreate,
    /// It was never created.
    NeverCreated,
}

/// RULE: however the addressed notification came to be absent, the read answers 200 with
/// no `Subscribers` member — and the notification the budget DOES hold still reports its
/// own subscribers, so the empty answer is about the notification addressed and not about
/// the budget.
#[tokio::test]
async fn a_notification_the_budget_does_not_hold_reports_no_subscribers() {
    let catalog = notification_catalog();
    let account_id = caller_account();
    let ways = [
        Absent::NotificationDeleted,
        Absent::LastSubscriberDeleted,
        Absent::MovedByUpdate,
        Absent::RefusedCreate,
        Absent::NeverCreated,
    ];

    for way in ways {
        for (position, notification) in catalog.iter().enumerate() {
            // A bystander notification of the same budget, whose subscribers the read of
            // the absent one must not report. The last row of the catalog has none, so the
            // rule is asserted for a budget holding nothing as well.
            let bystander = catalog.get(position + 1);
            let case = format!("{way:?} / notification {position} of {}", catalog.len());
            let router = router();
            let budget_name = "dead-notification";
            seed_budget(&router, &account_id, budget_name).await;
            if let Some(bystander) = bystander {
                seed_notification(
                    &router,
                    &account_id,
                    budget_name,
                    bystander,
                    &subscriber_set(position + 1),
                )
                .await;
            }

            match way {
                Absent::NeverCreated => {}
                Absent::RefusedCreate => {
                    // A set past the documented per-type split is refused, so nothing is
                    // stored for the notification the request named.
                    let too_many: Vec<Value> = (0..12)
                        .map(|index| {
                            json!({
                                "SubscriptionType": "EMAIL",
                                "Address": format!("over-{index}@example.com")
                            })
                        })
                        .collect();
                    let (status, body) = call(
                        &router,
                        "CreateNotification",
                        &json!({
                            "AccountId": &account_id,
                            "BudgetName": budget_name,
                            "Notification": notification,
                            "Subscribers": Value::Array(too_many)
                        }),
                    )
                    .await;
                    assert_eq!(
                        status, 400,
                        "case {case:?}: setup expected the oversized create to be refused, \
                         got {status} {body}"
                    );
                }
                Absent::NotificationDeleted => {
                    seed_notification(
                        &router,
                        &account_id,
                        budget_name,
                        notification,
                        &subscriber_set(position),
                    )
                    .await;
                    let (status, body) = call(
                        &router,
                        "DeleteNotification",
                        &json!({
                            "AccountId": &account_id,
                            "BudgetName": budget_name,
                            "Notification": notification
                        }),
                    )
                    .await;
                    assert_eq!(
                        status, 200,
                        "case {case:?}: setup expected DeleteNotification to answer 200, \
                         got {status} {body}"
                    );
                }
                Absent::LastSubscriberDeleted => {
                    let held = subscriber_set(position);
                    seed_notification(&router, &account_id, budget_name, notification, &held).await;
                    for subscriber in held.as_array().expect("a list of subscribers") {
                        let (status, body) = call(
                            &router,
                            "DeleteSubscriber",
                            &json!({
                                "AccountId": &account_id,
                                "BudgetName": budget_name,
                                "Notification": notification,
                                "Subscriber": subscriber
                            }),
                        )
                        .await;
                        assert_eq!(
                            status, 200,
                            "case {case:?}: setup expected DeleteSubscriber to answer 200, \
                             got {status} {body}"
                        );
                    }
                }
                Absent::MovedByUpdate => {
                    seed_notification(
                        &router,
                        &account_id,
                        budget_name,
                        notification,
                        &subscriber_set(position),
                    )
                    .await;
                    // An identity no row of the catalog names, so the moved notification
                    // cannot collide with the bystander.
                    let moved_to = json!({
                        "NotificationType": "ACTUAL",
                        "ComparisonOperator": "GREATER_THAN",
                        "Threshold": 3.5,
                        "ThresholdType": "ABSOLUTE_VALUE"
                    });
                    let (status, body) = call(
                        &router,
                        "UpdateNotification",
                        &json!({
                            "AccountId": &account_id,
                            "BudgetName": budget_name,
                            "OldNotification": notification,
                            "NewNotification": moved_to
                        }),
                    )
                    .await;
                    assert_eq!(
                        status, 200,
                        "case {case:?}: setup expected UpdateNotification to answer 200, \
                         got {status} {body}"
                    );
                }
            }

            assert_empty_read(&router, &account_id, budget_name, notification, &case).await;
            // Repeating the read answers the same thing: the read changed nothing.
            assert_empty_read(&router, &account_id, budget_name, notification, &case).await;

            if let Some(bystander) = bystander {
                let expected = subscriber_set(position + 1);
                let (status, body) =
                    read_subscribers(&router, &account_id, budget_name, bystander).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected the notification the budget holds to be \
                     readable, got {status} {body}"
                );
                assert!(
                    same(&body["Subscribers"], &expected),
                    "case {case:?}: expected the bystander's own subscribers {expected}, \
                     got {body}"
                );
            }
        }
    }
}

/// RULE, the other half: nothing has to EXIST for "which subscribers does this
/// notification have" to have an answer, so a budget the account does not hold answers the
/// same empty 200 — HTTP 200 with the optional `Subscribers` member left out — and this
/// read refuses no request for an absent resource. The calls whose subject IS the budget
/// keep refusing it: the sibling listing of a budget's notifications
/// (`DescribeNotificationsForBudget`) and every write that resolves the budget still answer
/// `NotFoundException`, worded "Unable to get budget: <BudgetName> - the budget doesn't
/// exist." with the name taken from the request.
///
/// The rows differ along why the budget is not held: a name nothing was created under, a
/// stored name in another case (budget names are matched exactly), a budget that was
/// deleted while holding the notification and its subscribers, and a budget whose
/// `TimePeriod$End` has passed ("After the end date, Amazon Web Services deletes the budget
/// and all the associated notifications and subscribers").
#[tokio::test]
async fn a_budget_the_account_does_not_hold_reports_no_subscribers() {
    let router = router();
    let account_id = caller_account();
    let catalog = notification_catalog();
    seed_budget(&router, &account_id, "held").await;
    seed_notification(&router, &account_id, "held", &catalog[0], &subscriber_set(0)).await;

    // A budget that carried the notification under test and its subscribers, then went
    // away: the cascade leaves this read with an addressee nothing is stored for.
    seed_budget(&router, &account_id, "deleted-later").await;
    seed_notification(
        &router,
        &account_id,
        "deleted-later",
        &catalog[0],
        &subscriber_set(0),
    )
    .await;
    let (status, body) = call(
        &router,
        "DeleteBudget",
        &json!({"AccountId": &account_id, "BudgetName": "deleted-later"}),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: expected DeleteBudget to answer 200, got {status} {body}"
    );

    // A budget the clock itself has already deleted: its end date sits a day behind now.
    let now = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("the test host's clock is after the epoch")
            .as_secs(),
    )
    .expect("an epoch that fits in i64");
    let day = 86_400;
    let (status, body) = call(
        &router,
        "CreateBudget",
        &json!({
            "AccountId": &account_id,
            "Budget": {
                "BudgetName": "ended",
                "TimeUnit": "MONTHLY",
                "BudgetType": "COST",
                "BudgetLimit": {"Amount": "5", "Unit": "USD"},
                "TimePeriod": {"Start": now - 2 * day, "End": now - day}
            },
            "NotificationsWithSubscribers": [
                {"Notification": catalog[0].clone(), "Subscribers": subscriber_set(0)}
            ]
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: expected CreateBudget(ended) to answer 200, got {status} {body}"
    );

    for (why, budget_name) in [
        ("a budget name the account holds nothing under", "absent"),
        ("a stored budget name in another case", "HELD"),
        ("a budget that was deleted", "deleted-later"),
        ("a budget past its end date", "ended"),
    ] {
        for (position, notification) in catalog.iter().enumerate() {
            let case = format!("{why} / notification {position}");
            assert_empty_read(&router, &account_id, budget_name, notification, &case).await;
            // Reading it again answers the same thing: the read brought no budget into
            // being and took nothing away.
            assert_empty_read(&router, &account_id, budget_name, notification, &case).await;

            // The calls whose subject IS the budget still refuse it, in the budget's own
            // wording — so a client can still tell "no such budget" from "no subscribers".
            for (operation, request) in [
                (
                    "DescribeNotificationsForBudget",
                    json!({"AccountId": &account_id, "BudgetName": budget_name}),
                ),
                (
                    "CreateSubscriber",
                    json!({
                        "AccountId": &account_id,
                        "BudgetName": budget_name,
                        "Notification": notification,
                        "Subscriber": {
                            "SubscriptionType": "EMAIL",
                            "Address": "late@example.com"
                        }
                    }),
                ),
            ] {
                let (status, body) = call(&router, operation, &request).await;
                assert_eq!(
                    status, 400,
                    "case {case:?}: expected {operation} to be refused, got {status} {body}"
                );
                assert!(
                    is_error(&body, "NotFoundException"),
                    "case {case:?}: expected {operation} to name NotFoundException, got {body}"
                );
                let expected =
                    format!("Unable to get budget: {budget_name} - the budget doesn't exist.");
                assert_eq!(
                    message(&body),
                    expected,
                    "case {case:?}: expected {operation} to answer {expected:?}, got {body}"
                );
            }
        }
    }

    // The budget the account DOES hold still reports the subscribers stored for it, so the
    // empty answers above are about the addressee and not a blanket success.
    let (status, body) = read_subscribers(&router, &account_id, "held", &catalog[0]).await;
    let expected = subscriber_set(0);
    assert_eq!(
        status, 200,
        "expected the held budget to answer 200, got {status} {body}"
    );
    assert!(
        same(&body["Subscribers"], &expected),
        "expected the held budget's subscribers {expected}, got {body}"
    );
}
