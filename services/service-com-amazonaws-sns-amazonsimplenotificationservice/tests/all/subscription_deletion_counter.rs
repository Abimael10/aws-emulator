// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the deletion half of a topic's subscription counters: WHEN an
//! `Unsubscribe` is taken up by `SubscriptionsDeleted`.
//!
//! RULE: the three computed counters a topic reports are a function of its subscription
//! store and of the clock alone. `SubscriptionsConfirmed` and `SubscriptionsPending` follow
//! a subscribe or an unsubscribe at once, while the deletion a call performed is taken up
//! by `SubscriptionsDeleted` only once the clock has made it due, on the span this service
//! lags that count by (`state::SUBSCRIPTION_DELETION_SETTLES_AFTER`): a client that
//! unsubscribes and then polls the three counters until the confirmed and pending ones have
//! settled keeps reading the deleted count from BEFORE the unsubscribe, however many times
//! it looks, and a client that keeps polling sees every deletion counted once the clock has
//! made it due — never a number that is lost.
//!
//! The rows differ along what the rule generalises over: how many subscriptions a topic
//! holds, how many of them are unsubscribed, and whether the ones left behind are confirmed
//! (an `sqs` endpoint, which SNS confirms itself) or awaiting confirmation (an `email`
//! endpoint, which it does not).
//!
//! Every call goes over the wire through the router the emulator serves in production.
//! Nothing here waits a span out: the change is one the CLOCK owes, so each case reads the
//! instants the topic holds its uncounted deletions at and moves them into the past, which
//! is what the clock would have done — so the suite says nothing about how fast this host
//! runs.

use std::collections::HashMap;
use std::time::{Duration, SystemTime};

use service_com_amazonaws_sns_amazonsimplenotificationservice::state::SUBSCRIPTION_DELETION_SETTLES_AFTER;

use super::topics_subscriptions_publish::{attribute_map, Sns};

/// How many times a case reads the counters again while asserting the deleted count stands:
/// a client's convergence loop over the other two counters, which have already settled.
const CONVERGENCE_READS: usize = 5;

/// How much earlier than the span a due instant is allowed to be when it is read back, so
/// the assertion says the deletion is held for a lag of that order without bounding how
/// fast this host ran between the two calls.
const SLACK: Duration = Duration::from_secs(10);

/// The three computed counters of `topic`, as the strings `GetTopicAttributes` reports.
async fn counters(sns: &Sns, case: &str, topic: &str) -> HashMap<String, String> {
    let answer = sns.call("GetTopicAttributes", &[("TopicArn", topic)]).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: GetTopicAttributes expected 200, got {} / {}",
        answer.status, answer.body
    );
    let map = attribute_map(&answer);
    [
        "SubscriptionsConfirmed",
        "SubscriptionsPending",
        "SubscriptionsDeleted",
    ]
    .into_iter()
    .map(|name| {
        let value = map.get(name).cloned().unwrap_or_else(|| {
            panic!("case {case:?}: expected a {name} counter, got {}", answer.body)
        });
        (name.to_string(), value)
    })
    .collect()
}

/// One counter of `topic`, as a number.
async fn counter(sns: &Sns, case: &str, topic: &str, name: &str) -> u64 {
    let counters = counters(sns, case, topic).await;
    let value = &counters[name];
    value
        .parse()
        .unwrap_or_else(|_| panic!("case {case:?}: {name} must be a number, got {value:?}"))
}

/// Subscribe `endpoint` on `topic` and hand back the subscription ARN.
async fn subscribe(sns: &Sns, case: &str, topic: &str, protocol: &str, endpoint: &str) -> String {
    let answer = sns
        .call(
            "Subscribe",
            &[
                ("TopicArn", topic),
                ("Protocol", protocol),
                ("Endpoint", endpoint),
            ],
        )
        .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: Subscribe {protocol} {endpoint} expected 200, got {} / {}",
        answer.status, answer.body
    );
    answer
        .element("SubscriptionArn")
        .unwrap_or_else(|| panic!("case {case:?}: no SubscriptionArn in {}", answer.body))
}

/// The instants `topic` holds its not-yet-counted deletions at, read from the store the
/// counters are computed from.
fn settling_at(sns: &Sns, case: &str, topic: &str) -> Vec<SystemTime> {
    let store = sns.state();
    let store = store.lock();
    store
        .topics
        .get(topic)
        .map(|held| held.subscriptions_deleting.clone())
        .unwrap_or_else(|| panic!("case {case:?}: the topic must be held"))
}

/// One row: how many `sqs` endpoints (confirmed on subscribe) and how many `email`
/// endpoints (left pending) a topic holds, and how many of the confirmed ones are
/// unsubscribed.
struct Row {
    case: &'static str,
    /// The topic name this row uses, which a topic name's charset admits.
    topic: &'static str,
    confirmed: usize,
    pending: usize,
    unsubscribed: usize,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the only subscription of a topic",
            topic: "deleted-counter-only",
            confirmed: 1,
            pending: 0,
            unsubscribed: 1,
        },
        Row {
            case: "one of two confirmed subscriptions, beside one pending",
            topic: "deleted-counter-one-of-two",
            confirmed: 2,
            pending: 1,
            unsubscribed: 1,
        },
        Row {
            case: "every confirmed subscription, beside two pending",
            topic: "deleted-counter-every-confirmed",
            confirmed: 3,
            pending: 2,
            unsubscribed: 3,
        },
        Row {
            case: "none of them",
            topic: "deleted-counter-none",
            confirmed: 2,
            pending: 1,
            unsubscribed: 0,
        },
    ]
}

#[tokio::test]
async fn an_unsubscribe_is_counted_into_subscriptions_deleted_once_the_clock_makes_it_due() {
    for row in rows() {
        let case = row.case;
        let sns = Sns::new();
        let topic = sns.topic(row.topic).await;

        // A fresh topic counts nothing at all.
        let fresh = counters(&sns, case, &topic).await;
        for name in [
            "SubscriptionsConfirmed",
            "SubscriptionsPending",
            "SubscriptionsDeleted",
        ] {
            assert_eq!(
                fresh[name], "0",
                "case {case:?}: a fresh topic reports no {name}, got {:?}",
                fresh[name]
            );
        }

        let mut confirmed = Vec::new();
        for index in 0..row.confirmed {
            confirmed.push(
                subscribe(
                    &sns,
                    case,
                    &topic,
                    "sqs",
                    &format!("arn:aws:sqs:us-east-1:000000000000:deleted-counter-{index}"),
                )
                .await,
            );
        }
        for index in 0..row.pending {
            subscribe(
                &sns,
                case,
                &topic,
                "email",
                &format!("ops{index}@example.test"),
            )
            .await;
        }

        let subscribed = counters(&sns, case, &topic).await;
        assert_eq!(
            (
                subscribed["SubscriptionsConfirmed"].as_str(),
                subscribed["SubscriptionsPending"].as_str(),
                subscribed["SubscriptionsDeleted"].as_str()
            ),
            (
                row.confirmed.to_string().as_str(),
                row.pending.to_string().as_str(),
                "0"
            ),
            "case {case:?}: the subscribe counters follow the store at once, got \
             {subscribed:?}"
        );

        for arn in confirmed.iter().take(row.unsubscribed) {
            let answer = sns.call("Unsubscribe", &[("SubscriptionArn", arn)]).await;
            assert_eq!(
                answer.status, 200,
                "case {case:?}: Unsubscribe expected 200, got {} / {}",
                answer.status, answer.body
            );
        }

        // The confirmed and pending counts follow the unsubscribe at once.
        let after = counters(&sns, case, &topic).await;
        assert_eq!(
            after["SubscriptionsConfirmed"],
            (row.confirmed - row.unsubscribed).to_string(),
            "case {case:?}: a removed subscription stops being counted at once, got {after:?}"
        );
        assert_eq!(
            after["SubscriptionsPending"],
            row.pending.to_string(),
            "case {case:?}: an unsubscribe of a confirmed endpoint leaves the pending count \
             alone, got {after:?}"
        );

        // The deleted count is NOT part of the unsubscribe: a client polling the other two
        // counters to settlement keeps reading the count from before it, however many
        // times it looks.
        for read in 0..CONVERGENCE_READS {
            let seen = counters(&sns, case, &topic).await;
            assert_eq!(
                seen["SubscriptionsDeleted"], "0",
                "case {case:?} (read {read}): the deleted count must stand until the clock \
                 makes the deletion due, got {seen:?}"
            );
        }

        if row.unsubscribed == 0 {
            // Nothing was deleted, so nothing is ever counted — and the topic holds no
            // deletion waiting to be.
            assert!(
                settling_at(&sns, case, &topic).is_empty(),
                "case {case:?}: a topic nothing was unsubscribed from holds no settling \
                 deletion"
            );
            continue;
        }

        // Each deletion is held against the instant the count is due to take it up, which
        // is the span this service lags the count by ahead of the call that performed it.
        let settling = settling_at(&sns, case, &topic);
        assert_eq!(
            settling.len(),
            row.unsubscribed,
            "case {case:?}: expected one settling deletion per unsubscribe, got {settling:?}"
        );
        let earliest_due = SystemTime::now() + SUBSCRIPTION_DELETION_SETTLES_AFTER - SLACK;
        for (at, due) in settling.iter().enumerate() {
            assert!(
                *due >= earliest_due,
                "case {case:?}: deletion {at} must be held for the whole span, due at \
                 {due:?}"
            );
        }

        // Once the clock has made them due, the count reports them — every one, exactly
        // once.
        sns.make_the_deletions_due(case, &topic);
        let deleted = counter(&sns, case, &topic, "SubscriptionsDeleted").await;
        assert_eq!(
            deleted, row.unsubscribed as u64,
            "case {case:?}: every deletion is counted exactly once, got {deleted}"
        );

        // And the count stays where the clock put it: reading it again neither advances it
        // nor loses it, and the other two counters are untouched by the settling.
        let settled = counters(&sns, case, &topic).await;
        assert_eq!(
            settled["SubscriptionsDeleted"],
            row.unsubscribed.to_string(),
            "case {case:?}: reading the counters must not change them, got {settled:?}"
        );
        assert_eq!(
            settled["SubscriptionsConfirmed"],
            (row.confirmed - row.unsubscribed).to_string(),
            "case {case:?}: the confirmed count is unchanged by the deletion settling, got \
             {settled:?}"
        );
        assert_eq!(
            settled["SubscriptionsPending"],
            row.pending.to_string(),
            "case {case:?}: the pending count is unchanged by the deletion settling, got \
             {settled:?}"
        );
    }
}

#[tokio::test]
async fn a_removed_subscription_is_gone_from_the_store_the_counters_are_read_from() {
    // The counters report the store: the subscription the unsubscribe removed is no longer
    // readable, whatever the deleted count has taken up so far.
    let case = "a removed subscription";
    let sns = Sns::new();
    let topic = sns.topic("deleted-counter-store").await;
    let arn = subscribe(
        &sns,
        case,
        &topic,
        "sqs",
        "arn:aws:sqs:us-east-1:000000000000:deleted-counter-store",
    )
    .await;
    let pending = subscribe(&sns, case, &topic, "email", "ops@example.test").await;
    assert_eq!(
        pending, "pending confirmation",
        "case {case:?}: a subscription awaiting confirmation has no ARN yet, got {pending:?}"
    );

    sns.call("Unsubscribe", &[("SubscriptionArn", &arn)]).await;
    let answer = sns
        .call("GetSubscriptionAttributes", &[("SubscriptionArn", &arn)])
        .await;
    assert_eq!(
        answer.status, 404,
        "case {case:?}: a removed subscription must not be readable, got {} / {}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.code(),
        "NotFound",
        "case {case:?}: expected NotFound, got {}",
        answer.body
    );

    // The pending one is still there — the topic still counts it — and the deletion is
    // taken up once the clock has made it due.
    let still_pending = counter(&sns, case, &topic, "SubscriptionsPending").await;
    assert_eq!(
        still_pending, 1,
        "case {case:?}: the subscription that was not removed must still be counted, got \
         {still_pending}"
    );
    assert_eq!(
        counter(&sns, case, &topic, "SubscriptionsDeleted").await,
        0,
        "case {case:?}: the deleted count stands until the clock makes the deletion due"
    );
    sns.make_the_deletions_due(case, &topic);
    let deleted = counter(&sns, case, &topic, "SubscriptionsDeleted").await;
    assert_eq!(
        deleted, 1,
        "case {case:?}: the one deletion is counted exactly once, got {deleted}"
    );
}
