// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the features a log group's CLASS does and does not support.
//!
//! Every case drives the service the way a client does — `POST /` with
//! `X-Amz-Target: Logs_20140328.<Operation>` through the router that mounts it — so a
//! response that is not computed from the shared store cannot pass.
//!
//! The rule under test: "the `Standard` log class supports all CloudWatch Logs features"
//! while the other classes support a subset of them
//! (`CreateLogGroup$logGroupClass`), so a call asking for a feature the group's class does
//! not support is REFUSED as invalid on that resource and stores nothing — while the same
//! call on a Standard class group is served and the resource it created reads back.
//!
//! The table spans what the rule generalises over: which class the group was created in
//! (each value of the model's `LogGroupClass`, including the default a create that names
//! none gets), and which feature the call asks for — one feature per operation that
//! configures it.

use serde_json::{json, Value};

use crate::harness::Logs;

/// The log group classes the model names. Source: the `LogGroupClass` enum
/// (`DELIVERY` | `INFREQUENT_ACCESS` | `STANDARD`).
const LOG_GROUP_CLASSES: [&str; 3] = ["DELIVERY", "INFREQUENT_ACCESS", "STANDARD"];

/// The one class that supports every feature. Source: `CreateLogGroup$logGroupClass` — "The
/// `Standard` log class supports all CloudWatch Logs features."
const FULL_FEATURE_CLASS: &str = "STANDARD";

/// The error code a call asking for a feature the class does not support is refused with:
/// `ValidationException`, the service's refusal of a request it cannot perform, as the
/// recorded refusal of a metric filter put on an Infrequent Access log group carries it.
const CLASS_REFUSAL_CODE: &str = "ValidationException";

/// The whole sentence that refusal carries: fixed text for the class, carrying nothing of the
/// request. Stated here independently of the crate under test.
const CLASS_REFUSAL_MESSAGE: &str = "This operation is only supported on the Standard log class.";

/// One feature a log group carries, and the calls that configure and read it.
struct Feature {
    /// The feature, named in every assertion.
    case: &'static str,
    /// The operation that configures it on a log group.
    configure: &'static str,
    /// The configuring call's members besides `logGroupName`.
    body: fn() -> Value,
    /// The reader that reports it, and the member of its response the configured resource is
    /// listed under.
    read: (&'static str, &'static str),
}

/// The features under test: the two whose configuration this class gate decides.
fn features() -> Vec<Feature> {
    vec![
        Feature {
            case: "a metric filter",
            configure: "PutMetricFilter",
            body: || {
                json!({
                    "filterName": "class-filter",
                    "filterPattern": "ERROR",
                    "metricTransformations": [{
                        "metricName": "Errors",
                        "metricNamespace": "ClassGate",
                        "metricValue": "1",
                    }],
                })
            },
            read: ("DescribeMetricFilters", "metricFilters"),
        },
        Feature {
            case: "a subscription filter",
            configure: "PutSubscriptionFilter",
            body: || {
                let account = engine::emulator_account_id();
                json!({
                    "filterName": "class-subscription",
                    "filterPattern": "",
                    "destinationArn": format!(
                        "arn:aws:kinesis:{}:{account}:stream/class-sink",
                        crate::harness::REGION,
                    ),
                    "roleArn": format!("arn:aws:iam::{account}:role/CWLtoKinesisRole"),
                })
            },
            read: ("DescribeSubscriptionFilters", "subscriptionFilters"),
        },
    ]
}

/// How many of `feature`'s resources the log group `group` holds, read back through the
/// feature's own reader.
async fn configured_count(logs: &Logs, group: &str, feature: &Feature) -> usize {
    let (operation, member) = feature.read;
    let page = logs.ok(operation, &json!({"logGroupName": group})).await;
    page.get(member)
        .and_then(Value::as_array)
        .map_or(0, Vec::len)
}

/// Assert the feature's reader refuses `group` as well: a class that does not support the
/// feature has none of its resources to report, so the read is refused rather than answered
/// with an empty page.
async fn reader_is_refused(logs: &Logs, group: &str, feature: &Feature, case: &str) {
    let (operation, _) = feature.read;
    let (code, message) = logs
        .refusal(operation, &json!({"logGroupName": group}))
        .await;
    assert_eq!(
        code, CLASS_REFUSAL_CODE,
        "case {case:?} ({operation}): expected {CLASS_REFUSAL_CODE}, got {code} ({message})"
    );
    assert_eq!(
        message, CLASS_REFUSAL_MESSAGE,
        "case {case:?} ({operation}): expected {CLASS_REFUSAL_MESSAGE:?}, got {message:?}"
    );
}

/// RULE: Live Tail is outside the subset the classes other than Standard support, so a
/// session over a log group of such a class is refused rather than opened — while the same
/// request over a Standard class group is served.
#[tokio::test]
async fn live_tail_is_opened_only_over_a_class_that_supports_it() {
    // The default (a create naming no class) beside every value of the enum.
    let classes: Vec<Option<&str>> = std::iter::once(None)
        .chain(LOG_GROUP_CLASSES.iter().map(|class| Some(*class)))
        .collect();
    for class in classes {
        let logs = Logs::new();
        let group = format!("/class/live-tail-{}", class.unwrap_or("default").to_lowercase());
        let mut create = json!({"logGroupName": group});
        if let Some(class) = class {
            create["logGroupClass"] = json!(class);
        }
        logs.ok("CreateLogGroup", &create).await;
        let request = json!({"logGroupIdentifiers": [group]});
        let case = format!("Live Tail over a {} class group", class.unwrap_or("default"));

        if class.is_none_or(|class| class == FULL_FEATURE_CLASS) {
            // The answer is an event stream rather than JSON, so the status is what says the
            // session was opened.
            let (status, _headers, _body) = logs.call_raw("StartLiveTail", &request).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the session to be opened, got HTTP {status}"
            );
            continue;
        }

        let (code, message) = logs.refusal("StartLiveTail", &request).await;
        assert_eq!(
            code, "InvalidParameterException",
            "case {case:?}: expected the session to be refused, got {code} ({message})"
        );
        assert_eq!(
            message, CLASS_REFUSAL_MESSAGE,
            "case {case:?}: expected {CLASS_REFUSAL_MESSAGE:?}, got {message:?}"
        );
    }
}
/// RULE: reading a log group's stored events back — by stream or across the group — is
/// outside the subset the classes other than Standard support, so either read over a group of
/// such a class is refused rather than answered, while the ingestion into it, and both reads
/// over a Standard class group, are served.
#[tokio::test]
async fn reading_events_is_served_only_for_a_class_that_supports_it() {
    // The default (a create naming no class) beside every value of the enum.
    let classes: Vec<Option<&str>> = std::iter::once(None)
        .chain(LOG_GROUP_CLASSES.iter().map(|class| Some(*class)))
        .collect();
    for class in classes {
        let logs = Logs::new();
        let group = format!("/class/search-{}", class.unwrap_or("default").to_lowercase());
        let stream = "stream";
        let mut create = json!({"logGroupName": group});
        if let Some(class) = class {
            create["logGroupClass"] = json!(class);
        }
        logs.ok("CreateLogGroup", &create).await;
        logs.ok(
            "CreateLogStream",
            &json!({"logGroupName": group, "logStreamName": stream}),
        )
        .await;
        // Ingestion is served whatever the class: what the class decides is how the events
        // are read back.
        logs.ok(
            "PutLogEvents",
            &json!({
                "logGroupName": group,
                "logStreamName": stream,
                "logEvents": [{
                    "timestamp": crate::harness::now_ms() - 60 * 1_000,
                    "message": "ingested whatever the class",
                }],
            }),
        )
        .await;
        let request = json!({"logGroupName": group});
        let case = format!("a search over a {} class group", class.unwrap_or("default"));

        if class.is_none_or(|class| class == FULL_FEATURE_CLASS) {
            let page = logs.ok("FilterLogEvents", &request).await;
            let found = page["events"].as_array().map_or(0, Vec::len);
            assert_eq!(
                found, 1,
                "case {case:?}: expected the ingested event to be found, got {page}"
            );
            let page = logs
                .ok(
                    "GetLogEvents",
                    &json!({"logGroupName": group, "logStreamName": stream}),
                )
                .await;
            let found = page["events"].as_array().map_or(0, Vec::len);
            assert_eq!(
                found, 1,
                "case {case:?}: expected the stream read to report the event, got {page}"
            );
            continue;
        }

        // Both ways of reading the events back are refused the same way.
        for (operation, body) in [
            ("FilterLogEvents", request.clone()),
            (
                "GetLogEvents",
                json!({"logGroupName": group, "logStreamName": stream}),
            ),
        ] {
            let (code, message) = logs.refusal(operation, &body).await;
            assert_eq!(
                code, CLASS_REFUSAL_CODE,
                "case {case:?} ({operation}): expected the read to be refused, got {code} \
                 ({message})"
            );
            assert_eq!(
                message, CLASS_REFUSAL_MESSAGE,
                "case {case:?} ({operation}): expected {CLASS_REFUSAL_MESSAGE:?}, got {message:?}"
            );
        }
    }
}

/// RULE: a feature outside the subset a class supports is configured only on a group of a
/// class that supports it: the call is refused on any other class, its reader is refused the
/// same way, and nothing of it is stored.
#[tokio::test]
async fn a_feature_is_configured_only_on_a_class_that_supports_it() {
    // The default (a create naming no class) beside every value of the enum.
    let classes: Vec<Option<&str>> = std::iter::once(None)
        .chain(LOG_GROUP_CLASSES.iter().map(|class| Some(*class)))
        .collect();

    for feature in features() {
        for class in &classes {
            let logs = Logs::new();
            let group = format!("/class/{}", class.unwrap_or("default").to_lowercase());
            let mut create = json!({"logGroupName": group});
            if let Some(class) = class {
                create["logGroupClass"] = json!(class);
            }
            logs.ok("CreateLogGroup", &create).await;
            // The class the group actually holds — the default where the create named none.
            let held = logs
                .describe_group(&group)
                .await
                .and_then(|reported| {
                    reported
                        .get("logGroupClass")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .unwrap_or_else(|| panic!("case {}: the group reports its class", feature.case));
            let case = format!("{} on a {held} class group", feature.case);

            let mut body = (feature.body)();
            body["logGroupName"] = json!(group);
            if held == FULL_FEATURE_CLASS {
                logs.ok(feature.configure, &body).await;
                let held_count = configured_count(&logs, &group, &feature).await;
                assert_eq!(
                    held_count, 1,
                    "case {case:?}: expected the configured feature to read back, got \
                     {held_count} of them"
                );
                continue;
            }

            let (code, message) = logs.refusal(feature.configure, &body).await;
            assert_eq!(
                code, CLASS_REFUSAL_CODE,
                "case {case:?}: expected {CLASS_REFUSAL_CODE}, got {code} ({message})"
            );
            assert_eq!(
                message, CLASS_REFUSAL_MESSAGE,
                "case {case:?}: expected {CLASS_REFUSAL_MESSAGE:?}, got {message:?}"
            );
            // The whole feature is unavailable on the class, so its reader is refused the
            // same way rather than reporting an empty page.
            reader_is_refused(&logs, &group, &feature, &case).await;
            // And nothing of it was stored: the group's own projection reports no filter.
            let reported = logs
                .describe_group(&group)
                .await
                .unwrap_or_else(|| panic!("case {case:?}: the group is there"));
            assert_eq!(
                reported.get("metricFilterCount").and_then(Value::as_i64),
                Some(0),
                "case {case:?}: a refused call stores nothing, got {reported}"
            );
        }
    }
}
