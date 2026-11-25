// SPDX-License-Identifier: CC0-1.0

//! The sentence `EnableMetricsCollection` refuses a granularity this service does not
//! aggregate at with, over the awsQuery wire.
//!
//! THE RULE UNDER TEST: a request naming a granularity outside the set the service
//! publishes is refused with a 400 `ValidationError` whose message is ONE fixed sentence —
//! `Valid metrics granularity type is: [` … `].` — whose only variable part is the
//! bracketed enumeration of the granularities the service accepts; the value the request
//! handed it, the group it named and the metrics it listed appear nowhere in it.
//!
//! The bracketed set is read off the wire here, from the catalog
//! `DescribeMetricCollectionTypes` publishes as `Granularities` (the member
//! `MetricGranularityType$Granularity`, read with
//! `smithy_query(query="shape",shape="com.amazonaws.autoscaling#MetricGranularityType")`),
//! so no row of this suite writes that set out and the sentence is checked to state the
//! very set the request was checked against.
//!
//! The table spans what the rule generalises over: each way a granularity can be outside
//! the published set (another period, another unit, a case difference, the published value
//! padded or split, a plural, the bare number), crossed with how many metrics the same
//! request names (none, one, several) — the sentence is the same for every row, because
//! none of those inputs is part of it. Its counterpart, the metric-name refusal, is
//! asserted here only to be a DIFFERENT sentence; `metric_collection_catalog.rs` owns the
//! catalog itself and what a refused request leaves behind.

use crate::support::{
    borrowed, new_engine, p, query, router_with, seed_subnets, tag_value, tag_values,
    xml_unescape,
};

/// One way of naming a granularity the service does not aggregate at.
struct GranularityCase {
    /// What makes this value outside the published set.
    case: &'static str,
    /// The value the request carries in `Granularity`.
    granularity: &'static str,
}

/// One row per way of being outside the published set — a different period, a different
/// unit, a case difference, the published value padded or split, a plural, and the bare
/// number without its unit. None of these is a member of the catalog the suite reads off
/// the wire, which is asserted before the rows run.
const UNSUPPORTED_GRANULARITIES: &[GranularityCase] = &[
    GranularityCase {
        case: "a period the service does not aggregate at",
        granularity: "5Minute",
    },
    GranularityCase {
        case: "the same period stated in another unit",
        granularity: "60Seconds",
    },
    GranularityCase {
        case: "a published granularity in another case",
        granularity: "1minute",
    },
    GranularityCase {
        case: "a published granularity padded with a leading space",
        granularity: " 1Minute",
    },
    GranularityCase {
        case: "a published granularity split by a space",
        granularity: "1 Minute",
    },
    GranularityCase {
        case: "a published granularity in the plural",
        granularity: "1Minutes",
    },
    GranularityCase {
        case: "the bare number without its unit",
        granularity: "1",
    },
];

/// How many metrics the refused request names beside the bad granularity: naming none means
/// every metric of the catalog, so these three span the member's own range.
const METRIC_SELECTIONS: &[(&str, &[&str])] = &[
    ("naming no metric", &[]),
    ("naming one metric", &["GroupMinSize"]),
    (
        "naming several metrics",
        &["GroupMinSize", "GroupTotalCapacity"],
    ),
];

/// The group every row below names, so no row is refused for the group instead.
const GROUP: &str = "granularity-sentence";

#[tokio::test]
async fn an_unsupported_granularity_is_refused_with_the_published_set_in_one_fixed_sentence() {
    let engine = new_engine();
    let router = router_with(engine.clone());
    let subnets = seed_subnets(&engine, 1);
    create_group(&router, GROUP, &subnets[0].id).await;

    let granularities = published_granularities(&router).await;
    // The whole sentence: fixed text apart from the bracketed set, which is the catalog the
    // service publishes rather than a value written out in this suite.
    let expected = format!(
        "Valid metrics granularity type is: [{}].",
        granularities.join(", ")
    );

    for case in UNSUPPORTED_GRANULARITIES {
        assert!(
            !granularities.iter().any(|published| published == case.granularity),
            "case {:?} ({:?}): the row must name a granularity the catalog does not \
             publish, got the published set {granularities:?}",
            case.case,
            case.granularity
        );

        for (selection, metrics) in METRIC_SELECTIONS {
            let named: Vec<String> = metrics.iter().map(|metric| (*metric).to_string()).collect();
            let (status, body) = enable(&router, GROUP, &named, case.granularity).await;
            let row = format!("{} / {selection}", case.case);

            assert_eq!(
                status, 400,
                "case {row:?} ({:?}): expected 400, got {status}: {body}",
                case.granularity
            );
            let code = tag_value(&body, "Code");
            assert_eq!(
                code, "ValidationError",
                "case {row:?}: expected the ValidationError code, got {code:?}: {body}"
            );

            let message = xml_unescape(&tag_value(&body, "Message"));
            assert_eq!(
                message, expected,
                "case {row:?}: expected the message {expected:?}, got {message:?}"
            );
            // The equality above already pins the whole sentence to the published set; this
            // states WHY the rows differ in nothing: no part of the request reaches it.
            assert!(
                !message.contains(GROUP) && !metrics.iter().any(|metric| message.contains(metric)),
                "case {row:?}: the sentence carries nothing from the request, got {message:?}"
            );
        }
    }

    // The refusals above wrote nothing, so the bracketed set is still what the group would
    // be collecting at had one of them been served.
    let collected = collected_granularities(&router, GROUP).await;
    assert!(
        collected.is_empty(),
        "a refused enable collects no metric, got {collected:?}"
    );

    // Every granularity the sentence names IS accepted, so the set it states is truthful.
    for granularity in &granularities {
        let named = ["GroupMaxSize".to_string()];
        let (status, body) = enable(&router, GROUP, &named, granularity).await;
        assert_eq!(
            status, 200,
            "case {granularity:?}: a published granularity must be served, got {status}: \
             {body}"
        );
        let collected = collected_granularities(&router, GROUP).await;
        assert_eq!(
            collected,
            vec![granularity.clone()],
            "case {granularity:?}: the served enable collects at the granularity it asked \
             for, got {collected:?}"
        );
        disable(&router, GROUP, &named).await;
    }

    // The metric-name refusal of the same operation is its OWN sentence: a client that
    // matches the granularity sentence must not see it for a bad metric name.
    let unknown = ["GroupMinSizes".to_string()];
    let (status, body) = enable(&router, GROUP, &unknown, &granularities[0]).await;
    assert_eq!(
        status, 400,
        "a metric outside the catalog: expected 400, got {status}: {body}"
    );
    let message = xml_unescape(&tag_value(&body, "Message"));
    assert_ne!(
        message, expected,
        "a metric outside the catalog is refused with its own sentence, got the \
         granularity sentence {message:?}"
    );
}

/// The granularities the service publishes, read off the wire.
async fn published_granularities(router: &smithy_server_runtime::MultiServiceRouter) -> Vec<String> {
    let (status, body) = query(router, "DescribeMetricCollectionTypes", &[]).await;
    assert_eq!(
        status, 200,
        "the published catalog: expected 200, got {status}: {body}"
    );
    let granularities = tag_values(&body, "Granularity");
    assert!(
        !granularities.is_empty(),
        "the published catalog must name a granularity, got {granularities:?}"
    );
    granularities
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
            ("MaxSize", "3"),
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

/// `EnableMetricsCollection` for one group at `granularity`, naming the metrics `metrics`
/// lists and none when it is empty.
async fn enable(
    router: &smithy_server_runtime::MultiServiceRouter,
    group: &str,
    metrics: &[String],
    granularity: &str,
) -> (u16, String) {
    let mut params = vec![
        p("AutoScalingGroupName", group),
        p("Granularity", granularity),
    ];
    for (index, metric) in metrics.iter().enumerate() {
        params.push(p(&format!("Metrics.member.{}", index + 1), metric.clone()));
    }
    query(router, "EnableMetricsCollection", &borrowed(&params)).await
}

/// `DisableMetricsCollection` for one group, naming the metrics `metrics` lists.
async fn disable(
    router: &smithy_server_runtime::MultiServiceRouter,
    group: &str,
    metrics: &[String],
) -> (u16, String) {
    let mut params = vec![p("AutoScalingGroupName", group)];
    for (index, metric) in metrics.iter().enumerate() {
        params.push(p(&format!("Metrics.member.{}", index + 1), metric.clone()));
    }
    query(router, "DisableMetricsCollection", &borrowed(&params)).await
}

/// The granularities one group reports its collected metrics at, read back through
/// `DescribeAutoScalingGroups`.
async fn collected_granularities(
    router: &smithy_server_runtime::MultiServiceRouter,
    group: &str,
) -> Vec<String> {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "reading the group {group:?} back: expected 200, got {status}: {body}"
    );
    tag_values(&body, "Granularity")
}
