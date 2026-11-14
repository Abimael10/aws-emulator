// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the deletion half of a topic's subscription counters: WHEN an
//! `Unsubscribe` is taken up by `SubscriptionsDeleted`.
//!
//! RULE: the three computed counters a topic reports are a function of its subscription
//! store and of the clock alone. `SubscriptionsConfirmed` and `SubscriptionsPending` follow
//! a subscribe or an unsubscribe at once, while the deletion a call performed is taken up
//! by `SubscriptionsDeleted` on the span the emulator OWES that transition
//! (`state::SUBSCRIPTION_DELETION_SETTLES_AFTER`): a read taken straight after the
//! `Unsubscribe` still reports the count from before it, and a client looking again sees
//! every deletion counted once the clock has made it due — never a number that stays put.
//!
//! The rows differ along what the rule generalises over: how many subscriptions a topic
//! holds, how many of them are unsubscribed, and whether the ones left behind are confirmed
//! (an `sqs` endpoint, which SNS confirms itself) or awaiting confirmation (an `email`
//! endpoint, which it does not).
//!
//! Every call goes over the wire through the router the emulator serves in production, and
//! nothing here waits out a span: each case LOOKS AGAIN for the count it expects, inside a
//! deadline generous enough to say nothing about how fast this host runs.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::topics_subscriptions_publish::{attribute_map, Sns};

/// How long a case looks again for a count the clock owes before it gives up. Generous: it
/// bounds a span the service owes, not the host's speed.
const DEADLINE: Duration = Duration::from_secs(60);

/// How often a case looks again while it waits.
const LOOK_AGAIN: Duration = Duration::from_millis(50);

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

/// Look again until `name` reports at least `wanted` for `topic`, and hand back what the
/// read that satisfied it saw.
async fn counted_at_least(sns: &Sns, case: &str, topic: &str, name: &str, wanted: u64) -> u64 {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let seen = counter(sns, case, topic, name).await;
        if seen >= wanted {
            return seen;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {name} must reach {wanted} once the clock has made the deletions \
             due, still reporting {seen}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
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

        if row.unsubscribed == 0 {
            // Nothing was deleted, so nothing is ever counted.
            assert_eq!(
                after["SubscriptionsDeleted"], "0",
                "case {case:?}: a topic nothing was unsubscribed from counts no deletion, got \
                 {after:?}"
            );
            continue;
        }

        let deleted = counted_at_least(
            &sns,
            case,
            &topic,
            "SubscriptionsDeleted",
            row.unsubscribed as u64,
        )
        .await;
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
    // taken up on the clock's span.
    let still_pending = counter(&sns, case, &topic, "SubscriptionsPending").await;
    assert_eq!(
        still_pending, 1,
        "case {case:?}: the subscription that was not removed must still be counted, got \
         {still_pending}"
    );
    let deleted = counted_at_least(&sns, case, &topic, "SubscriptionsDeleted", 1).await;
    assert_eq!(
        deleted, 1,
        "case {case:?}: the one deletion is counted exactly once, got {deleted}"
    );
}
