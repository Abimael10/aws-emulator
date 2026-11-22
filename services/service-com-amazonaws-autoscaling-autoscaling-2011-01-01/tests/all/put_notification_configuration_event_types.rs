// SPDX-License-Identifier: CC0-1.0

//! `PutNotificationConfiguration` over the awsQuery wire: which event types one Auto
//! Scaling group may be configured to notify a topic about.
//!
//! The RULE under test: the configuration a call carries is one topic and the SET of event
//! types it is to be told about, so the service stores, for the (`AutoScalingGroupName`,
//! `TopicARN`) pair, exactly the types the request names — deduplicated, independent of the
//! order they arrive in, and reported back per (topic, event type) by
//! `DescribeNotificationConfigurations` — for every type of the catalog
//! `DescribeAutoScalingNotificationTypes` publishes and for no other value: a request
//! naming a value outside that catalog is refused `ValidationError` (HTTP 400), naming the
//! value it carried and the catalog the caller has to choose from, and writes nothing, so
//! the group's stored configurations are exactly what they were.
//!
//! The rows span what the rule generalises over: which published type a configuration names
//! (every entry of the catalog in turn, one at a time), how many it names (one, two, the
//! whole catalog), the order they arrive in (as published and back to front), whether a
//! type is repeated in one call, and — for the refusal — HOW the value misses the catalog
//! (an event this service does not notify about, a published type without the service
//! prefix, one in the wrong case, one with trailing whitespace, a transition from the
//! SEPARATE lifecycle-hook catalog, the longest value the member admits) and whether it
//! arrives alone, after a published type, on a topic the group already holds a
//! configuration for, or on one it has never been configured with.
//!
//! The published catalog is read from `DescribeAutoScalingNotificationTypes` over the wire
//! rather than imported, so a row cannot pass on a constant this crate and its test agree
//! on but the service never publishes; every accepted row reads the group's configurations
//! back through `DescribeNotificationConfigurations`, so none can pass on an
//! accepted-but-unstored change.

use std::collections::BTreeSet;

use smithy_server_runtime::MultiServiceRouter;

use crate::delete_notification_configuration::{configured, put_configuration, topic_arn};
use crate::lifecycle_hooks::create_group;
use crate::support::{borrowed, new_engine, p, query, router_with, tag_value, xml_unescape};

/// The event type every group of this suite keeps on a second topic, so each row can show
/// the call it makes leaves the group's OTHER topics alone. Spelled out rather than
/// imported, so a wrong constant in the implementation cannot make a row pass; it is an
/// entry of the catalog `DescribeAutoScalingNotificationTypes` publishes, documented on
/// `NotificationConfiguration$NotificationType`
/// (`smithy_query(query="doc", shape="NotificationConfiguration", member="NotificationType")`).
const WITNESS_EVENT: &str = "autoscaling:TEST_NOTIFICATION";

/// An instance-lifecycle TRANSITION, which is an entry of the separate catalog
/// `DescribeLifecycleHookTypes` publishes: what a lifecycle hook is created for, not an
/// event a group notifies about, so a notification configuration naming it is refused.
const HOOK_TRANSITION: &str = "autoscaling:EC2_INSTANCE_LAUNCHING";

/// An event an Auto Scaling group does not notify about at all, in the spelling of a type
/// that does exist — a value outside the published catalog that is otherwise well formed.
const UNPUBLISHED_EVENT: &str = "autoscaling:EC2_INSTANCE_REBOOT";

/// The event types this service publishes as configurable, as
/// `DescribeAutoScalingNotificationTypes` reports them.
async fn published_catalog(router: &MultiServiceRouter) -> Vec<String> {
    let (status, body) = query(router, "DescribeAutoScalingNotificationTypes", &[]).await;
    assert_eq!(
        status, 200,
        "DescribeAutoScalingNotificationTypes expected 200, got {status}: {body}"
    );
    let catalog = crate::support::tag_values(&body, "member");
    assert!(
        catalog.len() > 1,
        "the published catalog must name the types a group can notify about, got {catalog:?}"
    );
    catalog
}

/// Post one `PutNotificationConfiguration` and answer its status, wire error code and
/// message — the form a row that expects a refusal reads.
///
/// Shared with the suite that reads the `TopicARN` form (`notification_topic_arn_form.rs`),
/// which asserts a refusal of the same call: THE ONE COPY of "post this write and tell me
/// how it was answered" for every suite here whose rows expect either answer.
pub(crate) async fn put_configuration_result(
    router: &MultiServiceRouter,
    group: &str,
    topic_arn: &str,
    events: &[String],
) -> (u16, String, String) {
    let mut params = vec![p("AutoScalingGroupName", group), p("TopicARN", topic_arn)];
    for (index, event) in events.iter().enumerate() {
        params.push(p(
            format!("NotificationTypes.member.{}", index + 1).as_str(),
            event.clone(),
        ));
    }
    let (status, body) = query(
        router,
        "PutNotificationConfiguration",
        &borrowed(&params),
    )
    .await;
    (
        status,
        tag_value(&body, "Code"),
        xml_unescape(&tag_value(&body, "Message")),
    )
}

/// The refusal a value outside the published catalog is answered with, spelled out here
/// rather than taken from the service's own generator: the fixed text, the member as the
/// model names it, the value the request carried, and the catalog the caller has to choose
/// from, comma-separated inside brackets.
///
/// That is the one shape this service refuses every value outside one of its catalogs in —
/// the same sentence its scaling-process, metric and adjustment-type refusals carry — so
/// this suite states it for the notification member rather than inventing a second wording
/// for the same class of request.
fn unknown_type_message(offending: &str, catalog: &[String]) -> String {
    format!(
        "The NotificationType value {offending} is not valid: it must be one of [{}]",
        catalog.join(", ")
    )
}

/// The (topic, event type) pairs a configuration of `events` on `topic` amounts to: one per
/// DISTINCT type, which is what makes the request's list a set.
fn expected_pairs(topic: &str, events: &[String]) -> Vec<(String, String)> {
    events
        .iter()
        .collect::<BTreeSet<&String>>()
        .into_iter()
        .map(|event| (topic.to_string(), event.clone()))
        .collect()
}

/// Every published event type is configurable, and what the service stores for a topic is
/// the deduplicated SET the request named — the group's other topics untouched.
#[tokio::test]
async fn a_configuration_stores_the_set_of_published_types_it_names() {
    let catalog = published_catalog(&router_with(new_engine())).await;

    // (case, the types one call names) — the rows differ in which published type is named,
    // how many are named, the order they arrive in, and whether one repeats.
    let mut rows: Vec<(String, Vec<String>)> = catalog
        .iter()
        .map(|event| (format!("the published type {event} on its own"), vec![event.clone()]))
        .collect();
    rows.push((
        "every published type in one call".to_string(),
        catalog.clone(),
    ));
    rows.push((
        "every published type, named back to front".to_string(),
        catalog.iter().rev().cloned().collect(),
    ));
    rows.push((
        format!("{} named three times, which configures one", catalog[0]),
        vec![catalog[0].clone(); 3],
    ));
    rows.push((
        "the first two published types".to_string(),
        catalog[..2].to_vec(),
    ));

    for (row, (case, requested)) in rows.iter().enumerate() {
        let router = router_with(new_engine());
        let group = format!("notified-group-{row}");
        create_group(&router, &group).await;

        // The topic the row configures, and a second one the row never names.
        let subject = topic_arn(&format!("{group}-subject"));
        let witness = topic_arn(&format!("{group}-witness"));
        put_configuration(&router, case, &group, &witness, &[WITNESS_EVENT]).await;

        let (status, code, message) =
            put_configuration_result(&router, &group, &subject, requested).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {code:?} {message:?}"
        );

        let mut expected = expected_pairs(&subject, requested);
        expected.push((witness.clone(), WITNESS_EVENT.to_string()));
        expected.sort();
        let reported = configured(&router, case, &group).await;
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the stored configurations {expected:?}, got {reported:?}"
        );
    }
}

/// A second call naming the same topic REPLACES that topic's event types; one naming
/// another topic leaves the first alone.
#[tokio::test]
async fn a_second_configuration_of_a_topic_replaces_its_types() {
    let catalog = published_catalog(&router_with(new_engine())).await;
    let last = catalog.last().expect("a published catalog").clone();

    // (case, the types first stored, the types the second call names) — the rows differ in
    // whether the replacement grows, shrinks or exchanges the set.
    let rows: Vec<(String, Vec<String>, Vec<String>)> = vec![
        (
            "one type exchanged for another".to_string(),
            vec![catalog[0].clone()],
            vec![last.clone()],
        ),
        (
            "one type grown to the whole published catalog".to_string(),
            vec![catalog[0].clone()],
            catalog.clone(),
        ),
        (
            "the whole published catalog shrunk to one type".to_string(),
            catalog.clone(),
            vec![catalog[1].clone()],
        ),
        (
            "the same type stored twice over".to_string(),
            vec![catalog[0].clone()],
            vec![catalog[0].clone()],
        ),
    ];

    for (row, (case, first, second)) in rows.iter().enumerate() {
        let router = router_with(new_engine());
        let group = format!("reconfigured-group-{row}");
        create_group(&router, &group).await;
        let subject = topic_arn(&format!("{group}-subject"));
        let witness = topic_arn(&format!("{group}-witness"));
        put_configuration(&router, case, &group, &witness, &[WITNESS_EVENT]).await;

        for types in [first, second] {
            let (status, code, message) =
                put_configuration_result(&router, &group, &subject, types).await;
            assert_eq!(
                status, 200,
                "case {case:?}: configuring {types:?} expected HTTP 200, got {status} with \
                 {code:?} {message:?}"
            );
        }

        // Only the second call's types are stored for that topic: a replacement, not a
        // union with what it replaced.
        let mut expected = expected_pairs(&subject, second);
        expected.push((witness.clone(), WITNESS_EVENT.to_string()));
        expected.sort();
        let reported = configured(&router, case, &group).await;
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the stored configurations {expected:?}, got {reported:?}"
        );
    }
}

/// A value outside the published catalog is refused, naming the value and the catalog, and
/// the group's stored configurations do not change.
#[tokio::test]
async fn a_type_outside_the_published_catalog_is_refused_and_stores_nothing() {
    let router = router_with(new_engine());
    let catalog = published_catalog(&router).await;
    // The two values the rows below expect to miss the catalog really do: a row that named a
    // published type would be testing the opposite rule.
    for outside in [UNPUBLISHED_EVENT, HOOK_TRANSITION] {
        assert!(
            !catalog.iter().any(|published| published == outside),
            "case {outside:?}: expected a value outside the published catalog, got it in \
             {catalog:?}"
        );
    }
    let group = "refusing-group";
    create_group(&router, group).await;

    // The topic the group IS configured with, the topic it has never been configured with,
    // and the configuration every refusal below must leave exactly as it is.
    let held = topic_arn("held-topic");
    let fresh = topic_arn("never-configured-topic");
    let held_types = [catalog[0].as_str(), WITNESS_EVENT];
    put_configuration(&router, "the configuration that must survive", group, &held, &held_types)
        .await;
    let before = configured(&router, "the configuration that must survive", group).await;

    // (case, the types one call names, the value the refusal must name) — the rows differ
    // in HOW the value misses the catalog and whether it arrives alone or after a
    // published type.
    let rows: Vec<(String, Vec<String>, String)> = vec![
        (
            "an event this service does not notify about".to_string(),
            vec![UNPUBLISHED_EVENT.to_string()],
            UNPUBLISHED_EVENT.to_string(),
        ),
        (
            "a published type without the service prefix".to_string(),
            vec![catalog[0]
                .rsplit(':')
                .next()
                .expect("a published type")
                .to_string()],
            catalog[0]
                .rsplit(':')
                .next()
                .expect("a published type")
                .to_string(),
        ),
        (
            "a published type in lower case".to_string(),
            vec![catalog[0].to_lowercase()],
            catalog[0].to_lowercase(),
        ),
        (
            "a published type with trailing whitespace".to_string(),
            vec![format!("{} ", catalog[0])],
            format!("{} ", catalog[0]),
        ),
        (
            "a transition of the lifecycle-hook catalog".to_string(),
            vec![HOOK_TRANSITION.to_string()],
            HOOK_TRANSITION.to_string(),
        ),
        (
            "an unknown type named after a published one".to_string(),
            vec![catalog[0].clone(), UNPUBLISHED_EVENT.to_string()],
            UNPUBLISHED_EVENT.to_string(),
        ),
        (
            // `NotificationTypes` carries `XmlStringMaxLen255` members (len 1..255), so
            // this is the longest value the member admits.
            "the longest value the member admits".to_string(),
            vec!["x".repeat(255)],
            "x".repeat(255),
        ),
    ];

    for (case, requested, offending) in rows {
        for (topic_case, topic) in [("a topic the group holds", &held), ("a topic it does not", &fresh)]
        {
            let case = format!("{case} on {topic_case}");
            let (status, code, message) =
                put_configuration_result(&router, group, topic, &requested).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400, got {status} with {code:?} {message:?}"
            );
            assert_eq!(
                code, "ValidationError",
                "case {case:?}: expected the ValidationError code, got {code:?} {message:?}"
            );
            let expected = unknown_type_message(&offending, &catalog);
            assert_eq!(
                message, expected,
                "case {case:?}: expected the message {expected:?}, got {message:?}"
            );
            assert!(
                message.contains(&offending),
                "case {case:?}: the refusal names the value the request carried, got {message:?}"
            );
            for published in &catalog {
                assert!(
                    message.contains(published.as_str()),
                    "case {case:?}: the refusal names the choice {published}, got {message:?}"
                );
            }
            let after = configured(&router, &case, group).await;
            assert_eq!(
                after, before,
                "case {case:?}: a refused configuration changes nothing, expected \
                 {before:?}, got {after:?}"
            );
        }
    }

    // The group is still configurable with a published type after every refusal above.
    let accepted = catalog.last().expect("a published catalog").clone();
    let (status, code, message) =
        put_configuration_result(&router, group, &fresh, std::slice::from_ref(&accepted)).await;
    assert_eq!(
        status, 200,
        "a published type is still accepted, got {status} with {code:?} {message:?}"
    );
    let mut expected = before.clone();
    expected.push((fresh.clone(), accepted.clone()));
    expected.sort();
    let reported = configured(&router, "after the refusals", group).await;
    assert_eq!(
        reported, expected,
        "after the refusals: expected the stored configurations {expected:?}, got {reported:?}"
    );
}
