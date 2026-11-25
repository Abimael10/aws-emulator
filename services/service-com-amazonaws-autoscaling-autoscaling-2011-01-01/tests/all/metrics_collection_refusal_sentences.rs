// SPDX-License-Identifier: CC0-1.0

//! The sentence metrics collection refuses a metric-collection type outside the service's
//! published catalogue with, and the enumeration that sentence carries, over the awsQuery
//! wire.
//!
//! THE RULE UNDER TEST: a request naming a value outside an enumerated member's own
//! accepted set is refused with ONE fixed sentence for that member — here
//! `Valid metrics collection types are: [` … `].` — whose only variable part is the
//! bracketed enumeration of the values the service accepts, stated in the order the
//! reference-data read publishes them; nothing of the request appears in it, so the sentence
//! is the same however the request manages to be wrong and whichever writer of the group's
//! metrics collection it reached.
//!
//! What this suite owns, beside its siblings: the WHOLE message of the metric-name refusal,
//! and the ORDER of the enumeration it ends with.
//! `metrics_granularity_refusal_sentence.rs` owns the other member of the same operation,
//! and `metric_collection_catalog.rs` owns the catalogue itself, the collection a served
//! request writes and what a refused one leaves behind.
//!
//! The expected set is read OFF THE WIRE from `DescribeMetricCollectionTypes` rather than
//! written out here, so the refusal and the catalogue read cannot drift apart. The table
//! spans what the rule generalises over: how many names the request carries, which of them
//! is outside the catalogue, where it sits in the list, whether it repeats, and the kind of
//! near miss — crossed with both writers of the collection. It carries rows the service must
//! still SERVE, so no row passes by refusing every list.

use crate::support::{
    borrowed, new_engine, p, query, router_with, seed_subnets, tag_value, tag_values,
    xml_unescape,
};

/// Which writer of a group's metrics collection a row drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Toggle {
    Enable,
    Disable,
}

/// The group every row below names, so no row is refused for the group instead.
const GROUP: &str = "metrics-collection-types";

/// One row of the metric-name table: a list carrying at least one name the catalogue does
/// not publish.
struct MetricsCase {
    /// What makes this list outside the published catalogue.
    case: &'static str,
    /// The names the request carries, in the order it carries them.
    named: &'static [&'static str],
}

/// One row per way a list can name something outside the catalogue: a name of no family the
/// service has, a published name in another case or padded, a bound the group has but the
/// warm pool does not, a retained form of a metric that has none, and a bad name first, last,
/// in the middle, doubled up or repeated.
const UNSUPPORTED_METRICS: &[MetricsCase] = &[
    MetricsCase {
        case: "one name of no family the service has",
        named: &["NotARealMetricName"],
    },
    MetricsCase {
        case: "a published name in another case",
        named: &["groupminsize"],
    },
    MetricsCase {
        case: "a published name padded with a leading space",
        named: &[" GroupMinSize"],
    },
    MetricsCase {
        case: "a bound the group has but the warm pool does not",
        named: &["WarmPoolMaxSize"],
    },
    MetricsCase {
        case: "a retained form of a metric that has none",
        named: &["GroupMinSizeRetained"],
    },
    MetricsCase {
        case: "the bad name first, before two published ones",
        named: &["GroupRetainedInstances", "GroupMinSize", "GroupTotalCapacity"],
    },
    MetricsCase {
        case: "the bad name last, after two published ones",
        named: &["GroupMinSize", "GroupTotalCapacity", "GroupRetainedInstances"],
    },
    MetricsCase {
        case: "the bad name between two published ones",
        named: &["GroupMinSize", "GroupRetainedInstances", "GroupTotalCapacity"],
    },
    MetricsCase {
        case: "two bad names together",
        named: &["GroupRetainedInstances", "WarmPoolMaxSize"],
    },
    MetricsCase {
        case: "the same bad name twice",
        named: &["WarmPoolMaxSize", "WarmPoolMaxSize"],
    },
];

/// A request naming a metric-collection type outside the published catalogue is refused with
/// the service's own fixed sentence for that member — the same sentence on either writer of
/// the group's metrics collection, however many names the request carries and wherever the
/// bad one sits.
#[tokio::test]
async fn a_metric_outside_the_catalogue_is_refused_with_the_published_set_in_one_fixed_sentence() {
    let engine = new_engine();
    let router = router_with(engine.clone());
    let subnets = seed_subnets(&engine, 1);
    create_group(&router, GROUP, &subnets[0].id).await;

    let (catalogue, granularities) = published(&router).await;
    let served = granularities[0].clone();
    // The whole sentence: fixed text apart from the bracketed set, which is the catalogue
    // the service publishes rather than a list written out in this suite.
    let expected = format!(
        "Valid metrics collection types are: [{}].",
        catalogue.join(", ")
    );

    for case in UNSUPPORTED_METRICS {
        // The names of this row the catalogue does not publish: at least one, or the row
        // states nothing about the refusal.
        let outside: Vec<&&str> = case
            .named
            .iter()
            .filter(|name| !catalogue.iter().any(|published| published == *name))
            .collect();
        assert!(
            !outside.is_empty(),
            "case {:?}: the row must name something the catalogue does not publish, got \
             {:?} against {catalogue:?}",
            case.case,
            case.named
        );

        for which in [Toggle::Enable, Toggle::Disable] {
            let named: Vec<String> = case.named.iter().map(|name| (*name).to_string()).collect();
            let (status, body) = toggle(&router, which, GROUP, &named, &served).await;

            assert_eq!(
                status, 400,
                "case {:?} ({which:?}): expected 400, got {status}: {body}",
                case.case
            );
            let code = tag_value(&body, "Code");
            assert_eq!(
                code, "ValidationError",
                "case {:?} ({which:?}): expected the ValidationError code, got {code:?}: \
                 {body}",
                case.case
            );
            let message = xml_unescape(&tag_value(&body, "Message"));
            assert_eq!(
                message, expected,
                "case {:?} ({which:?}): expected the message {expected:?}, got {message:?}",
                case.case
            );
            // Why every row states the same sentence: no part of the request reaches it —
            // neither the group it named nor the names the catalogue does not publish.
            assert!(
                !message.contains(GROUP)
                    && !outside.iter().any(|name| message.contains(**name)),
                "case {:?} ({which:?}): the sentence carries nothing from the request, got \
                 {message:?}",
                case.case
            );
        }
    }

    // Every name the sentence enumerates IS accepted, on both writers, so the set it states
    // is truthful and no row above passed by refusing every list.
    for metric in &catalogue {
        let named = [metric.clone()];
        for which in [Toggle::Enable, Toggle::Disable] {
            let (status, body) = toggle(&router, which, GROUP, &named, &served).await;
            assert_eq!(
                status, 200,
                "case {metric:?} ({which:?}): a published metric-collection type must be \
                 served, got {status}: {body}"
            );
        }
    }

    // And the granularity member of the same operation is refused with its OWN sentence: a
    // client matching the metrics sentence must not see it for a bad granularity.
    let named = ["GroupMinSize".to_string()];
    let (status, body) = toggle(&router, Toggle::Enable, GROUP, &named, "5Minute").await;
    assert_eq!(
        status, 400,
        "a granularity outside the catalogue: expected 400, got {status}: {body}"
    );
    let message = xml_unescape(&tag_value(&body, "Message"));
    assert_ne!(
        message, expected,
        "a granularity outside the catalogue is refused with its own sentence, got the \
         metrics sentence {message:?}"
    );
}

/// One row of the order table: a name the service publishes before another.
struct OrderCase {
    /// Why the service's catalogue order puts them this way round.
    case: &'static str,
    earlier: &'static str,
    later: &'static str,
}

/// The order the published catalogue states, as pairs that the model member's PROSE grouping
/// (every instance count before every capacity) would have the other way round.
///
/// Source: the order the service states the catalogue in, which the member
/// `MetricCollectionType$Metric`
/// (`smithy_query(query="shape",shape="com.amazonaws.autoscaling#MetricCollectionType")`)
/// only groups rather than orders — each instance count is followed by the capacity those
/// instances carry, the warm pool's warmed capacity comes after its pending and terminating
/// series, and the two combined series come last.
const ORDER: &[OrderCase] = &[
    OrderCase {
        case: "a group capacity before a later state's instance count",
        earlier: "GroupInServiceCapacity",
        later: "GroupPendingInstances",
    },
    OrderCase {
        case: "the retained terminating pair before the standby pair",
        earlier: "GroupTerminatingRetainedCapacity",
        later: "GroupStandbyInstances",
    },
    OrderCase {
        case: "the standby pair before the group totals",
        earlier: "GroupStandbyCapacity",
        later: "GroupTotalInstances",
    },
    OrderCase {
        case: "the warm pool's retained terminating capacity before its warmed capacity",
        earlier: "WarmPoolTerminatingRetainedCapacity",
        later: "WarmPoolWarmedCapacity",
    },
    OrderCase {
        case: "the warm pool's warmed capacity before its total",
        earlier: "WarmPoolWarmedCapacity",
        later: "WarmPoolTotalCapacity",
    },
    OrderCase {
        case: "the group totals before the warm pool's own minimum",
        earlier: "GroupTotalCapacity",
        later: "WarmPoolMinSize",
    },
];

/// The enumeration the refusal states IS the catalogue the reference-data read publishes,
/// entry for entry and in the same order — so a client parsing the allowed values out of the
/// refusal reads what `DescribeMetricCollectionTypes` would have told it, and one that
/// presents them sees the service's own ordering.
#[tokio::test]
async fn the_refusals_enumeration_is_the_published_catalogue_in_order() {
    let engine = new_engine();
    let router = router_with(engine.clone());
    let subnets = seed_subnets(&engine, 1);
    create_group(&router, GROUP, &subnets[0].id).await;

    let (catalogue, granularities) = published(&router).await;
    let named = ["NoSuchMetricCollectionType".to_string()];
    let (status, body) =
        toggle(&router, Toggle::Enable, GROUP, &named, &granularities[0]).await;
    assert_eq!(status, 400, "the refusal: expected 400, got {status}: {body}");
    let message = xml_unescape(&tag_value(&body, "Message"));
    let enumerated = bracketed(&message);
    assert_eq!(
        enumerated, catalogue,
        "the refusal must enumerate the catalogue the read publishes, entry for entry and \
         in the same order, got {enumerated:?} against {catalogue:?}"
    );

    // Each count of instances is immediately followed by the capacity those instances carry
    // — the pairing the published order holds, including the retained series.
    let mut paired = 0;
    for (index, metric) in enumerated.iter().enumerate() {
        let Some(state) = metric.strip_suffix("Instances") else {
            continue;
        };
        let twin = format!("{state}Capacity");
        assert_eq!(
            enumerated.get(index + 1),
            Some(&twin),
            "case {metric:?}: expected its capacity twin {twin:?} next, got {enumerated:?}"
        );
        paired += 1;
    }
    assert_eq!(
        paired,
        enumerated
            .iter()
            .filter(|metric| metric.ends_with("Instances"))
            .count(),
        "every instance count of the catalogue must be paired, got {enumerated:?}"
    );

    // The pairs and the segments in the order the service states them, each row a pair the
    // model member's prose grouping would have the other way round.
    for case in ORDER {
        let earlier = position(&enumerated, case.earlier, case.case);
        let later = position(&enumerated, case.later, case.case);
        assert!(
            earlier < later,
            "case {:?}: expected {:?} (at {earlier}) before {:?} (at {later}), got \
             {enumerated:?}",
            case.case,
            case.earlier,
            case.later
        );
    }

    // And the two combined series come last, after every group and warm-pool series.
    let combined: Vec<&String> = enumerated
        .iter()
        .filter(|metric| metric.starts_with("GroupAndWarmPool"))
        .collect();
    assert_eq!(
        combined.len(),
        2,
        "expected the two combined series, got {enumerated:?}"
    );
    assert_eq!(
        &enumerated[enumerated.len() - 2..]
            .iter()
            .collect::<Vec<&String>>(),
        &combined,
        "the combined series must come last, got {enumerated:?}"
    );
}

/// Where `metric` sits in the enumeration, for a row that states the order of two entries.
fn position(enumerated: &[String], metric: &str, case: &str) -> usize {
    enumerated
        .iter()
        .position(|entry| entry == metric)
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected {metric:?} in the enumeration {enumerated:?}")
        })
}

/// The entries of the bracketed enumeration a refusal sentence ends with, in the order it
/// states them — what a client parsing the refusal to present the allowed values reads.
fn bracketed(message: &str) -> Vec<String> {
    let open = message
        .find('[')
        .unwrap_or_else(|| panic!("expected a bracketed enumeration in {message:?}"));
    let close = message
        .rfind(']')
        .unwrap_or_else(|| panic!("expected a bracketed enumeration in {message:?}"));
    message[open + 1..close]
        .split(", ")
        .map(str::to_string)
        .collect()
}

/// The whole catalogue read off the wire: the metric-collection types and the granularities
/// `DescribeMetricCollectionTypes` publishes, in the order it publishes them.
async fn published(
    router: &smithy_server_runtime::MultiServiceRouter,
) -> (Vec<String>, Vec<String>) {
    let (status, body) = query(router, "DescribeMetricCollectionTypes", &[]).await;
    assert_eq!(
        status, 200,
        "the published catalogue: expected 200, got {status}: {body}"
    );
    let metrics = tag_values(&body, "Metric");
    let granularities = tag_values(&body, "Granularity");
    assert!(
        metrics.len() > 1,
        "the published catalogue must name more than one metric-collection type, got \
         {metrics:?}"
    );
    assert!(
        !granularities.is_empty(),
        "the published catalogue must name a granularity, got {granularities:?}"
    );
    (metrics, granularities)
}

/// Create one group placed in the zone of `subnet_id`, holding no instance.
async fn create_group(
    router: &smithy_server_runtime::MultiServiceRouter,
    name: &str,
    subnet_id: &str,
) {
    let (status, body) = query(
        router,
        "CreateAutoScalingGroup",
        &[
            ("AutoScalingGroupName", name),
            ("LaunchTemplate.LaunchTemplateName", "lt-1"),
            ("MinSize", "0"),
            ("MaxSize", "2"),
            ("DesiredCapacity", "0"),
            ("VPCZoneIdentifier", subnet_id),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "creating the group {name:?}: expected 200, got {status}: {body}"
    );
}

/// Drive one writer of the group's metrics collection with the metrics `metrics` names,
/// at `granularity` for the enable (the disable has no granularity member).
async fn toggle(
    router: &smithy_server_runtime::MultiServiceRouter,
    which: Toggle,
    group: &str,
    metrics: &[String],
    granularity: &str,
) -> (u16, String) {
    let mut params = vec![p("AutoScalingGroupName", group)];
    if which == Toggle::Enable {
        params.push(p("Granularity", granularity));
    }
    for (index, metric) in metrics.iter().enumerate() {
        params.push(p(&format!("Metrics.member.{}", index + 1), metric.clone()));
    }
    let action = match which {
        Toggle::Enable => "EnableMetricsCollection",
        Toggle::Disable => "DisableMetricsCollection",
    };
    query(router, action, &borrowed(&params)).await
}
